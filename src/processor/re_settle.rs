use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_MARKET, SEED_PROTOCOL_CONFIG, WAD};
use crate::error::LendingError;
use crate::logic::validation::get_unix_timestamp;
use crate::state::{Market, ProtocolConfig};

/// ReSettle (disc 9)
/// Recompute the settlement factor upward after additional post-settlement repayments.
/// Permissionless -- anyone may call.
pub fn process(program_id: &Address, accounts: &[AccountView], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let vault_account = &accounts[1];
    let protocol_config_account = &accounts[2];

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
    let nonce_bytes = market.market_nonce().to_le_bytes();
    let (expected_market_pda, _) =
        Address::find_program_address(&[SEED_MARKET, &market.borrower, &nonce_bytes], program_id);
    if market_account.address() != &expected_market_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // SR-101: vault must match
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // SR-098: settlement_factor_wad must be > 0
    let old_factor = market.settlement_factor_wad();
    if old_factor == 0 {
        return Err(LendingError::NotSettled.into());
    }

    // Verify protocol config PDA
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

    // Step 1: Accrue interest with actual protocol config (ensures fees are properly accrued)
    let current_ts = get_unix_timestamp()?;
    crate::logic::interest::accrue_interest(market, config, current_ts)?;

    // Step 2-4: Compute new settlement factor
    // SR-123: Verify vault account is owned by token program before unsafe deserialization
    if unsafe { vault_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let vault_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(vault_account)?
    };
    let vault_balance = u128::from(vault_token.amount());
    // COAL-C01: No fee reservation (see withdraw.rs for rationale).
    // COAL-H01: Subtract haircut accumulator to prevent recycled inflation.
    let haircut_reserved = u128::from(market.haircut_accumulator());
    let available_for_lenders = vault_balance
        .checked_sub(haircut_reserved)
        .unwrap_or(0); // Defensive: if accumulator exceeds vault, use 0

    // SR-122: Explicit check for zero scale_factor (defense-in-depth)
    let scale_factor = market.scale_factor();
    if scale_factor == 0 {
        return Err(LendingError::InvalidScaleFactor.into());
    }

    let total_normalized = market
        .scaled_total_supply()
        .checked_mul(scale_factor)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(WAD)
        .ok_or(LendingError::MathOverflow)?;

    let new_factor = if total_normalized == 0 {
        WAD
    } else {
        let raw = available_for_lenders
            .checked_mul(WAD)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(total_normalized)
            .ok_or(LendingError::MathOverflow)?;
        let capped = if raw > WAD { WAD } else { raw };
        if capped < 1 {
            1
        } else {
            capped
        }
    };

    // Step 6: Validate improvement
    if new_factor <= old_factor {
        return Err(LendingError::SettlementNotImproved.into());
    }

    // Step 7: Update market
    market.set_settlement_factor_wad(new_factor);

    log!(
        "evt:re_settle market={} new_factor={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        new_factor
    );

    Ok(())
}
