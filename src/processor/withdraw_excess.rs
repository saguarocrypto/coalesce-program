use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_MARKET_AUTHORITY, SEED_PROTOCOL_CONFIG, WAD,
};
use crate::error::LendingError;
use crate::logic::validation::{
    check_blacklist, get_unix_timestamp, validate_market_authority, validate_market_pda,
};
use crate::state::{Market, ProtocolConfig};

/// WithdrawExcess (disc 11)
/// Allows the borrower to withdraw excess funds from the vault after:
/// - Market has matured
/// - All lenders have fully withdrawn (scaled_total_supply == 0)
/// - Settlement factor is WAD (full settlement achieved)
/// - Protocol fees have been collected (accrued_protocol_fees == 0)
///
/// This prevents loss of funds when borrower overpays interest.
pub fn process(program_id: &Address, accounts: &[AccountView], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 8 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let borrower = &accounts[1];
    let borrower_token_account = &accounts[2];
    let vault_account = &accounts[3];
    let market_authority = &accounts[4];
    let token_program = &accounts[5];
    let protocol_config_account = &accounts[6];
    let blacklist_check = &accounts[7];

    // Validate token program
    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    // Borrower must be signer
    if !borrower.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // Verify protocol_config PDA and read it
    let (expected_config_pda, _) =
        Address::find_program_address(&[SEED_PROTOCOL_CONFIG], program_id);
    if protocol_config_account.address() != &expected_config_pda {
        return Err(LendingError::InvalidPDA.into());
    }
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

    // Read market (read-only for this instruction)
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let market_data = unsafe { market_account.borrow_unchecked() };
    let market: &Market =
        bytemuck::try_from_bytes(market_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for Market
    if market.discriminator != DISC_MARKET {
        return Err(ProgramError::InvalidAccountData);
    }

    // SR-121: Verify market PDA derivation
    validate_market_pda(market_account, market, program_id)?;

    // Verify caller is the market's borrower
    if market.borrower != *borrower.address().as_ref() {
        return Err(LendingError::Unauthorized.into());
    }

    // Blacklist check for borrower (Finding 8: was missing from withdraw_excess)
    check_blacklist(blacklist_check, config, borrower.address())?;

    // Verify vault matches
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // Check 1: Market must be past maturity
    let current_ts = get_unix_timestamp()?;
    if current_ts < market.maturity_timestamp() {
        return Err(LendingError::NotMatured.into());
    }

    // Check 2: All lenders must have withdrawn (scaled_total_supply == 0)
    if market.scaled_total_supply() > 0 {
        return Err(LendingError::LendersPendingWithdrawals.into());
    }

    // Check 3: Settlement must be complete (settlement_factor == WAD)
    // This ensures lenders received full value
    let settlement_factor = market.settlement_factor_wad();
    if settlement_factor == 0 {
        // Settlement never happened (no withdrawals occurred)
        return Err(LendingError::SettlementNotComplete.into());
    }
    if settlement_factor < WAD {
        // Market was in distress - shouldn't allow excess withdrawal
        return Err(LendingError::FeeCollectionDuringDistress.into());
    }

    // Check 4: Protocol fees must have been collected
    if market.accrued_protocol_fees() > 0 {
        return Err(LendingError::FeesNotCollected.into());
    }

    // Verify vault is owned by token program
    if unsafe { vault_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Get vault balance
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let vault_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(vault_account)?
    };

    // Verify vault mint matches market mint
    if *vault_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }

    let vault_balance = vault_token.amount();
    // COAL-H01: Subtract haircut accumulator to prevent borrower from sweeping
    // unpaid lender haircut value that remains in the vault after force-close.
    let haircut_reserved = market.haircut_accumulator();
    let excess_amount = vault_balance.saturating_sub(haircut_reserved);
    if excess_amount == 0 {
        return Err(LendingError::NoExcessToWithdraw.into());
    }

    // Verify borrower token account mint matches
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let borrower_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(borrower_token_account)?
    };
    if *borrower_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }

    // C-3: Verify market authority PDA and re-derive bump
    validate_market_authority(market_authority, market_account, market, program_id)?;

    // Transfer excess tokens (vault -> borrower) with PDA signing
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
        amount: excess_amount,
    }
    .invoke_signed(&[pinocchio::cpi::Signer::from(&auth_seeds)])?;

    log!(
        "evt:withdraw_excess market={} borrower={} amount={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(borrower.address().as_ref()),
        excess_amount
    );

    Ok(())
}
