use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_BORROWER_WL, DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_BORROWER_WHITELIST,
};
use crate::error::LendingError;
use crate::logic::interest::accrue_interest;
use crate::logic::validation::{
    get_unix_timestamp, validate_market_pda, validate_market_state, validate_protocol_config_pda,
};
use crate::state::{BorrowerWhitelist, Market, ProtocolConfig};

/// Repay (disc 5)
/// Repay USDC to the market vault. Anyone may call.
/// Updates the borrower's current_borrowed to allow re-borrowing after repayment.
///
/// Compliance note: No blacklist check is performed on the payer because repayment
/// reduces protocol risk. Blocking a sanctioned entity from repaying would leave
/// the market under-collateralized, harming innocent lenders.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 8 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let payer = &accounts[1];
    let payer_token_account = &accounts[2];
    let vault_account = &accounts[3];
    let protocol_config_account = &accounts[4];
    let mint_account = &accounts[5];
    let borrower_whitelist_account = &accounts[6];
    let token_program = &accounts[7];

    // Validate token program
    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    // Parse instruction data: amount (8 bytes)
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes(
        data[0..8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

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

    // SR-048: mint must match
    if market.mint != *mint_account.address().as_ref() {
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

    // Finding 9: Cap repayment at per-market principal outstanding.
    // principal_repaid = total_repaid - total_interest_repaid (interest goes to a separate bucket)
    // market_outstanding = total_borrowed - principal_repaid
    let principal_repaid = market
        .total_repaid()
        .checked_sub(market.total_interest_repaid())
        .ok_or(LendingError::MathOverflow)?;
    let market_outstanding = market
        .total_borrowed()
        .checked_sub(principal_repaid)
        .ok_or(LendingError::MathOverflow)?;
    if amount > market_outstanding {
        return Err(LendingError::RepaymentExceedsDebt.into());
    }

    // SR-123: Verify vault account is owned by token program before transfer
    if unsafe { vault_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Step 2: Validate and load borrower whitelist (needed for checks before transfer)
    let borrower_key: &[u8; 32] = &market.borrower;
    let (expected_wl_pda, _) =
        Address::find_program_address(&[SEED_BORROWER_WHITELIST, borrower_key], program_id);
    if borrower_whitelist_account.address() != &expected_wl_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    if !borrower_whitelist_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let wl_data = unsafe { borrower_whitelist_account.borrow_unchecked_mut() };
    let wl: &mut BorrowerWhitelist =
        bytemuck::try_from_bytes_mut(wl_data).map_err(|_| ProgramError::InvalidAccountData)?;

    if wl.discriminator != DISC_BORROWER_WL {
        return Err(ProgramError::InvalidAccountData);
    }

    // SR-116: Validate repayment does not exceed current debt
    let current = wl.current_borrowed();
    if amount > current {
        return Err(LendingError::RepaymentExceedsDebt.into());
    }

    // CEI: Update all state BEFORE transfer CPI (Finding 3).
    let new_total_repaid = market
        .total_repaid()
        .checked_add(amount)
        .ok_or(LendingError::MathOverflow)?;
    market.set_total_repaid(new_total_repaid);

    let new_borrowed = current
        .checked_sub(amount)
        .ok_or(LendingError::MathOverflow)?;
    wl.set_current_borrowed(new_borrowed);

    // Step 3: Transfer tokens (payer -> vault)
    pinocchio_token::instructions::Transfer {
        from: payer_token_account,
        to: vault_account,
        authority: payer,
        amount,
    }
    .invoke()?;

    log!(
        "evt:repay market={} payer={} amount={} borrower_debt={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(payer.address().as_ref()),
        amount,
        new_borrowed
    );

    Ok(())
}
