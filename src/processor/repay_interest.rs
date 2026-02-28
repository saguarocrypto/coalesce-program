use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{DISC_MARKET, DISC_PROTOCOL_CONFIG};
use crate::error::LendingError;
use crate::logic::interest::accrue_interest;
use crate::logic::validation::{
    get_unix_timestamp, validate_market_pda, validate_market_state, validate_protocol_config_pda,
};
use crate::state::{Market, ProtocolConfig};

/// RepayInterest (disc 6)
/// Repay accrued interest to the market vault WITHOUT affecting borrower capacity.
/// This instruction allows borrowers to pay interest without reducing `current_borrowed`,
/// ensuring that interest payments don't artificially free up borrowing capacity.
/// Anyone may call (e.g., the borrower or a third party on their behalf).
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let payer = &accounts[1];
    let payer_token_account = &accounts[2];
    let vault_account = &accounts[3];
    let protocol_config_account = &accounts[4];
    let token_program = &accounts[5];

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

    // SR-045: amount > 0
    if amount == 0 {
        return Err(LendingError::ZeroAmount.into());
    }

    // Payer must be signer
    if !payer.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // SR-110: Verify payer_token_account is owned by payer (prevents griefing attacks)
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let payer_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(payer_token_account)?
    };
    if payer_token.owner() != payer.address() {
        return Err(LendingError::InvalidTokenAccountOwner.into());
    }

    // SR-126: Verify payer token account mint (validated after market is read below)

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

    // SR-047: vault must match
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // SR-126: Verify payer token account mint matches market mint
    if *payer_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }

    // Verify protocol_config PDA and read it
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

    // Step 1: Accrue interest
    let current_ts = get_unix_timestamp()?;
    accrue_interest(market, config, current_ts)?;

    // SR-123: Verify vault account is owned by token program before transfer
    if unsafe { vault_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Step 2: Transfer tokens (payer -> vault)
    pinocchio_token::instructions::Transfer {
        from: payer_token_account,
        to: vault_account,
        authority: payer,
        amount,
    }
    .invoke()?;

    // Step 3: Update market - track interest repaid separately
    // This adds to total_repaid for overall tracking, but does NOT affect
    // borrower's current_borrowed (preserving their capacity)
    let new_total_repaid = market
        .total_repaid()
        .checked_add(amount)
        .ok_or(LendingError::MathOverflow)?;
    market.set_total_repaid(new_total_repaid);

    // Also track interest repaid specifically for analytics
    let new_interest_repaid = market
        .total_interest_repaid()
        .checked_add(amount)
        .ok_or(LendingError::MathOverflow)?;
    market.set_total_interest_repaid(new_interest_repaid);

    // NOTE: We intentionally do NOT update the borrower's current_borrowed.
    // This is the key difference from the regular repay instruction.
    // Interest payments should not free up borrowing capacity.

    log!(
        "evt:repay_interest market={} payer={} amount={} total_interest_repaid={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(payer.address().as_ref()),
        amount,
        new_interest_repaid
    );

    Ok(())
}
