use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_MARKET_AUTHORITY, WAD};
use crate::error::LendingError;
use crate::logic::interest::accrue_interest;
use crate::logic::validation::{
    get_unix_timestamp, validate_market_authority, validate_market_pda, validate_market_state,
    validate_protocol_config_pda,
};
use crate::state::{Market, ProtocolConfig};

/// CollectFees (disc 8)
/// Fee authority withdraws accrued protocol fees from a market vault.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    let _ = data; // No instruction data beyond discriminator
    if accounts.len() < 7 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let protocol_config_account = &accounts[1];
    let fee_authority = &accounts[2];
    let fee_destination = &accounts[3];
    let vault_account = &accounts[4];
    let market_authority = &accounts[5];
    let token_program = &accounts[6];

    // Validate token program
    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    // Verify protocol config PDA
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

    // SR-054: fee_authority must match and be signer
    if config.fee_authority != *fee_authority.address().as_ref() {
        return Err(LendingError::Unauthorized.into());
    }
    if !fee_authority.is_signer() {
        return Err(LendingError::Unauthorized.into());
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

    // SR-121: Verify market PDA derivation
    validate_market_pda(market_account, market, program_id)?;

    // H-7: Validate scale_factor is non-zero before any math
    validate_market_state(market)?;

    // SR-057: Block fee collection during market distress
    // If settlement_factor is set (> 0) but less than WAD, the market is in distress
    // and lender recovery should take priority over fee collection
    let settlement_factor = market.settlement_factor_wad();
    if settlement_factor > 0 && settlement_factor < WAD {
        return Err(LendingError::FeeCollectionDuringDistress.into());
    }

    // SR-113: Block fee collection while lenders have pending withdrawals,
    // UNLESS settlement_factor == WAD (fully solvent). When sf == WAD the
    // market is solvent, but the vault may not hold surplus above lender
    // claims (COAL-C01 removed fee reservation from settlement). The
    // lender-claims cap below (applied after computing withdrawable)
    // prevents draining below obligations.
    if market.scaled_total_supply() > 0 && settlement_factor != WAD {
        return Err(LendingError::LendersPendingWithdrawals.into());
    }

    // SR-056: vault must match
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // Step 1: Accrue interest
    let current_ts = get_unix_timestamp()?;
    accrue_interest(market, config, current_ts)?;

    // SR-055: accrued_protocol_fees must be > 0
    let accrued_fees = market.accrued_protocol_fees();
    if accrued_fees == 0 {
        return Err(LendingError::NoFeesToCollect.into());
    }

    // Step 2: Compute withdrawable
    // SR-123: Verify vault account is owned by token program before unsafe deserialization
    if unsafe { vault_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let vault_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(vault_account)?
    };

    // SR-124: Validate vault's mint matches market's mint
    if *vault_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }

    let vault_balance = vault_token.amount();
    let withdrawable = core::cmp::min(accrued_fees, vault_balance);

    // COAL-C01 compensation: when lenders still have claims, cap fee
    // withdrawal to vault surplus above total lender obligations. Before
    // COAL-C01, fee reservation in settlement kept sf < WAD, so SR-057
    // blocked this path entirely. After COAL-C01 sf can reach WAD with
    // no surplus for fees — this guard prevents vault drain.
    let withdrawable = if market.scaled_total_supply() > 0 {
        let sf = market.scale_factor();
        let total_normalized = market
            .scaled_total_supply()
            .checked_mul(sf)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?;
        let lender_claims =
            u64::try_from(total_normalized).map_err(|_| LendingError::MathOverflow)?;
        let safe_max = vault_balance.saturating_sub(lender_claims);
        core::cmp::min(withdrawable, safe_max)
    } else {
        withdrawable
    };

    if withdrawable == 0 {
        return Err(LendingError::NoFeesToCollect.into());
    }

    // C-3: Verify market authority PDA and re-derive bump
    validate_market_authority(market_authority, market_account, market, program_id)?;

    // SR-127: Verify fee destination is owned by token program
    if unsafe { fee_destination.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let fee_dest_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(fee_destination)?
    };
    // SR-127: Verify fee destination mint matches market mint
    if *fee_dest_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }
    // H-1: Verify fee destination token account is owned by the fee authority
    if fee_dest_token.owner() != fee_authority.address() {
        return Err(LendingError::InvalidTokenAccountOwner.into());
    }

    // Step 3: Transfer tokens (vault -> fee_destination) with PDA signing
    let auth_bump_ref = [market.market_authority_bump];
    let auth_seeds = [
        pinocchio::cpi::Seed::from(SEED_MARKET_AUTHORITY),
        pinocchio::cpi::Seed::from(market_account.address().as_ref()),
        pinocchio::cpi::Seed::from(&auth_bump_ref),
    ];
    pinocchio_token::instructions::Transfer {
        from: vault_account,
        to: fee_destination,
        authority: market_authority,
        amount: withdrawable,
    }
    .invoke_signed(&[pinocchio::cpi::Signer::from(&auth_seeds)])?;

    // Step 4: Update accrued_protocol_fees
    let remaining_fees = accrued_fees
        .checked_sub(withdrawable)
        .ok_or(LendingError::MathOverflow)?;
    market.set_accrued_protocol_fees(remaining_fees);

    log!(
        "evt:collect_fees market={} amount={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        withdrawable
    );

    Ok(())
}
