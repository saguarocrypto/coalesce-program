use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_BORROWER_WL, DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_BORROWER_WHITELIST,
    SEED_MARKET_AUTHORITY,
};
use crate::error::LendingError;
use crate::logic::interest::accrue_interest;
use crate::logic::validation::{
    check_blacklist, get_unix_timestamp, validate_market_authority, validate_market_pda,
    validate_market_state, validate_protocol_config_pda,
};
use crate::state::{BorrowerWhitelist, Market, ProtocolConfig};

/// Borrow (disc 6)
/// Borrower withdraws USDC from the market vault.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 9 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let borrower = &accounts[1];
    let borrower_token_account = &accounts[2];
    let vault_account = &accounts[3];
    let market_authority = &accounts[4];
    let borrower_whitelist_account = &accounts[5];
    let blacklist_check = &accounts[6];
    let protocol_config_account = &accounts[7];
    let token_program = &accounts[8];

    // Validate token program
    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    // Parse instruction data: amount (8 bytes)
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes([
        data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
    ]);

    // SR-042: amount > 0
    if amount == 0 {
        return Err(LendingError::ZeroAmount.into());
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

    // SR-038: borrower must match market.borrower and be signer
    if market.borrower != *borrower.address().as_ref() {
        return Err(LendingError::Unauthorized.into());
    }
    if !borrower.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // SR-044: vault must match
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // SR-040: must be before maturity
    let current_ts = get_unix_timestamp()?;
    if current_ts >= market.maturity_timestamp() {
        return Err(LendingError::MarketMatured.into());
    }

    // SR-041: blacklist check for borrower
    check_blacklist(blacklist_check, config, borrower.address())?;

    // Step 1: Accrue interest
    accrue_interest(market, config, current_ts)?;

    // Step 2: Compute borrowable (fee reservation)
    // Read vault balance
    // SR-117: Verify vault account is owned by token program before unsafe deserialization
    if unsafe { vault_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let vault_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(vault_account)?
    };
    // SR-118: Validate vault's mint matches market's mint
    if *vault_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }
    let vault_balance = vault_token.amount();
    let fees_reserved = core::cmp::min(vault_balance, market.accrued_protocol_fees());
    let borrowable = vault_balance
        .checked_sub(fees_reserved)
        .ok_or(LendingError::MathOverflow)?;

    // Step 3: Validate amount <= borrowable
    if amount > borrowable {
        return Err(LendingError::BorrowAmountTooHigh.into());
    }

    // SR-106: Check global borrow capacity
    let (expected_wl_pda, _) = Address::find_program_address(
        &[SEED_BORROWER_WHITELIST, borrower.address().as_ref()],
        program_id,
    );
    if borrower_whitelist_account.address() != &expected_wl_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    // Verify ownership before deserializing
    if !borrower_whitelist_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let wl_data = unsafe { borrower_whitelist_account.borrow_unchecked_mut() };
    let wl: &mut BorrowerWhitelist =
        bytemuck::try_from_bytes_mut(wl_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for BorrowerWhitelist
    if wl.discriminator != DISC_BORROWER_WL {
        return Err(ProgramError::InvalidAccountData);
    }

    let new_wl_borrowed = wl
        .current_borrowed()
        .checked_add(amount)
        .ok_or(LendingError::MathOverflow)?;
    if new_wl_borrowed > wl.max_borrow_capacity() {
        return Err(LendingError::GlobalCapacityExceeded.into());
    }

    // C-3: Verify market authority PDA and re-derive bump
    validate_market_authority(market_authority, market_account, market, program_id)?;

    // SR-119: Validate borrower_token_account is owned by token program before unsafe deserialization
    if unsafe { borrower_token_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SR-120: Validate borrower_token_account is owned by borrower (prevents unauthorized fund redirection)
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let borrower_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(borrower_token_account)?
    };
    if borrower_token.owner() != borrower.address() {
        return Err(LendingError::InvalidTokenAccountOwner.into());
    }

    // Step 4: Update market state BEFORE transfer (checks-effects-interactions pattern)
    // This ensures state is consistent even if transfer fails; CPI failure will revert all changes
    let new_total_borrowed = market
        .total_borrowed()
        .checked_add(amount)
        .ok_or(LendingError::MathOverflow)?;
    market.set_total_borrowed(new_total_borrowed);

    // Step 5: Update borrower whitelist current debt BEFORE transfer
    wl.set_current_borrowed(new_wl_borrowed);

    // Step 6: Transfer tokens (vault -> borrower) with PDA signing
    let auth_bump_ref = [market.market_authority_bump];
    let auth_seeds = [
        pinocchio::cpi::Seed::from(SEED_MARKET_AUTHORITY),
        pinocchio::cpi::Seed::from(market_account.address().as_ref()),
        pinocchio::cpi::Seed::from(&auth_bump_ref),
    ];
    pinocchio_token::instructions::Transfer {
        from: vault_account,
        to: borrower_token_account,
        authority: market_authority,
        amount,
    }
    .invoke_signed(&[pinocchio::cpi::Signer::from(&auth_seeds)])?;

    log!(
        "evt:borrow market={} borrower={} amount={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(borrower.address().as_ref()),
        amount
    );

    Ok(())
}
