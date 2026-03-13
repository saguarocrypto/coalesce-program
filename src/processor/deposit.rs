use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_LENDER_POSITION, DISC_MARKET, DISC_PROTOCOL_CONFIG, LENDER_POSITION_SIZE, SEED_LENDER, WAD,
};
use crate::error::LendingError;
use crate::logic::interest::accrue_interest;
use crate::logic::validation::{
    check_blacklist, get_unix_timestamp, validate_market_pda, validate_protocol_config_pda,
};
use crate::state::{LenderPosition, Market, ProtocolConfig};

/// Deposit (disc 3)
/// Lender deposits USDC into a market vault, receiving scaled balance.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 10 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let lender = &accounts[1];
    let lender_token_account = &accounts[2];
    let vault_account = &accounts[3];
    let lender_position_account = &accounts[4];
    let blacklist_check = &accounts[5];
    let protocol_config_account = &accounts[6];
    let mint_account = &accounts[7];
    let token_program = &accounts[8];
    // accounts[9] = system_program

    // Validate token program
    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    // Parse instruction data: amount (8 bytes)
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());

    // SR-029: amount > 0
    if amount == 0 {
        return Err(LendingError::ZeroAmount.into());
    }

    // Lender must be signer
    if !lender.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // Verify protocol config PDA and read it
    validate_protocol_config_pda(protocol_config_account, program_id)?;
    // Verify ownership before deserializing
    if !protocol_config_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let config_data = unsafe { protocol_config_account.borrow_unchecked() };
    let config: &ProtocolConfig =
        bytemuck::try_from_bytes(config_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for ProtocolConfig
    if config.discriminator != DISC_PROTOCOL_CONFIG {
        return Err(ProgramError::InvalidAccountData);
    }

    // Emergency pause check
    if config.is_paused() {
        return Err(LendingError::ProtocolPaused.into());
    }

    // Verify market is owned by our program
    if !market_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Read market mutably
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let market_data = unsafe { market_account.borrow_unchecked_mut() };
    let market: &mut Market =
        bytemuck::try_from_bytes_mut(market_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for Market
    if market.discriminator != DISC_MARKET {
        return Err(ProgramError::InvalidAccountData);
    }

    // Verify market PDA derivation
    validate_market_pda(market_account, market, program_id)?;

    // SR-035: vault must match
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // SR-037: mint must match
    if market.mint != *mint_account.address().as_ref() {
        return Err(LendingError::InvalidMint.into());
    }

    // SR-031: must be before maturity
    let current_ts = get_unix_timestamp()?;
    if current_ts >= market.maturity_timestamp() {
        return Err(LendingError::MarketMatured.into());
    }

    // SR-033: blacklist check for lender
    check_blacklist(blacklist_check, config, lender.address())?;

    // Step 1: Accrue interest
    accrue_interest(market, config, current_ts)?;

    // Step 2: Compute scaled amount = amount * WAD / scale_factor
    let amount_u128 = u128::from(amount);
    let scale_factor = market.scale_factor();
    // Defense-in-depth: reject zero scale_factor before division
    if scale_factor == 0 {
        return Err(LendingError::InvalidScaleFactor.into());
    }
    let scaled_amount = amount_u128
        .checked_mul(WAD)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(scale_factor)
        .ok_or(LendingError::MathOverflow)?;

    // Step 3: Revert if scaled_amount == 0
    if scaled_amount == 0 {
        return Err(LendingError::ZeroScaledAmount.into());
    }

    // Step 3: Validate cap on raw principal (not interest-inflated value).
    // Finding 4: Using scaled_total_supply * scale_factor included accrued
    // interest, which meant the effective deposit cap shrank over time.
    let new_total_deposited = market
        .total_deposited()
        .checked_add(amount)
        .ok_or(LendingError::MathOverflow)?;
    let max_supply = market.max_total_supply();
    if new_total_deposited > max_supply {
        return Err(LendingError::CapExceeded.into());
    }

    let new_scaled_total = market
        .scaled_total_supply()
        .checked_add(scaled_amount)
        .ok_or(LendingError::MathOverflow)?;

    // H-03: Verify token account ownership before transfer
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let lender_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(lender_token_account)?
    };
    if lender_token.owner() != lender.address() {
        return Err(LendingError::InvalidTokenAccountOwner.into());
    }

    // Step 5: Transfer tokens (lender -> vault)
    pinocchio_token::instructions::Transfer {
        from: lender_token_account,
        to: vault_account,
        authority: lender,
        amount,
    }
    .invoke()?;

    // Create or update lender position
    let (expected_pos_pda, pos_bump) = Address::find_program_address(
        &[
            SEED_LENDER,
            market_account.address().as_ref(),
            lender.address().as_ref(),
        ],
        program_id,
    );
    if lender_position_account.address() != &expected_pos_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Use ownership check instead of lamports to determine if position exists.
    // Lamports can be donated to any PDA, so lamports > 0 is not proof of initialization.
    let position_exists = lender_position_account.owned_by(program_id);
    if position_exists {
        // Defense-in-depth: verify ownership (invariant from branch condition)
        if !lender_position_account.owned_by(program_id) {
            return Err(LendingError::InvalidAccountOwner.into());
        }
        // Update existing position
        // SAFETY: This is the only mutable borrow of this account in this instruction.
        // Account data length is verified by bytemuck::try_from_bytes_mut.
        let pos_data = unsafe { lender_position_account.borrow_unchecked_mut() };
        let position: &mut LenderPosition =
            bytemuck::try_from_bytes_mut(pos_data).map_err(|_| ProgramError::InvalidAccountData)?;

        // Discriminator check for LenderPosition
        if position.discriminator != DISC_LENDER_POSITION {
            return Err(ProgramError::InvalidAccountData);
        }

        // Verify position belongs to this market and lender
        if position.market != *market_account.address().as_ref() {
            return Err(LendingError::InvalidPDA.into());
        }
        if position.lender != *lender.address().as_ref() {
            return Err(LendingError::Unauthorized.into());
        }

        let new_balance = position
            .scaled_balance()
            .checked_add(scaled_amount)
            .ok_or(LendingError::MathOverflow)?;
        position.set_scaled_balance(new_balance);
    } else {
        // Create the lender position account
        let pos_bump_ref = [pos_bump];
        let pos_signer_seeds = [
            pinocchio::cpi::Seed::from(SEED_LENDER),
            pinocchio::cpi::Seed::from(market_account.address().as_ref()),
            pinocchio::cpi::Seed::from(lender.address().as_ref()),
            pinocchio::cpi::Seed::from(&pos_bump_ref),
        ];
        pinocchio_system::create_account_with_minimum_balance_signed(
            lender_position_account,
            LENDER_POSITION_SIZE,
            program_id,
            lender,
            None,
            &[pinocchio::cpi::Signer::from(&pos_signer_seeds)],
        )?;

        // SAFETY: This is the only mutable borrow of this account in this instruction.
        // Account data length is verified by bytemuck::try_from_bytes_mut.
        let pos_data = unsafe { lender_position_account.borrow_unchecked_mut() };
        let position: &mut LenderPosition =
            bytemuck::try_from_bytes_mut(pos_data).map_err(|_| ProgramError::InvalidAccountData)?;

        position
            .discriminator
            .copy_from_slice(&DISC_LENDER_POSITION);
        position.version = 1;
        position
            .market
            .copy_from_slice(market_account.address().as_ref());
        position.lender.copy_from_slice(lender.address().as_ref());
        position.set_scaled_balance(scaled_amount);
        position.bump = pos_bump;
    }

    // Step 7: Update market
    market.set_scaled_total_supply(new_scaled_total);
    market.set_total_deposited(new_total_deposited);

    log!(
        "evt:deposit market={} lender={} amount={} scaled={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(lender.address().as_ref()),
        amount,
        scaled_amount
    );

    Ok(())
}
