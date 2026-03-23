use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_HAIRCUT_STATE, DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_HAIRCUT_STATE, SEED_MARKET,
    SEED_PROTOCOL_CONFIG, WAD,
};
use crate::error::LendingError;
use crate::logic::validation::get_unix_timestamp;
use crate::state::{HaircutState, Market, ProtocolConfig};

/// ReSettle (disc 9)
/// Recompute the settlement factor upward after additional post-settlement repayments.
/// Permissionless -- anyone may call.
pub fn process(program_id: &Address, accounts: &[AccountView], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let vault_account = &accounts[1];
    let protocol_config_account = &accounts[2];
    let haircut_state_account = &accounts[3];

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

    // COAL-H01: Read the conservative aggregate for already-withdrawn lenders.
    //
    // `re_settle` does NOT subtract `market.haircut_accumulator` from the vault.
    // Doing so would make later borrower repayments invisible to the solver.
    // Instead, `HaircutState` captures how much of the vault may still belong to
    // prior withdrawers as SF improves, while still letting new repayments raise
    // the current settlement factor for everyone.
    //
    // Validate HaircutState PDA derivation and ownership.
    let (expected_haircut_pda, _) = Address::find_program_address(
        &[SEED_HAIRCUT_STATE, market_account.address().as_ref()],
        program_id,
    );
    if haircut_state_account.address() != &expected_haircut_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    if !haircut_state_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let haircut_data = unsafe { haircut_state_account.borrow_unchecked() };
    let haircut_state: &HaircutState =
        bytemuck::try_from_bytes(haircut_data).map_err(|_| ProgramError::InvalidAccountData)?;
    if haircut_state.discriminator != DISC_HAIRCUT_STATE {
        return Err(ProgramError::InvalidAccountData);
    }
    if haircut_state.market != *market_account.address().as_ref() {
        return Err(LendingError::InvalidPDA.into());
    }

    // COAL-H01: Conservative re_settle formula.
    //   new_sf = WAD * (V + O) / (R + W)
    // where:
    //   V = current vault balance,
    //   R = normalized claim of lenders still in the market,
    //   W/O = conservative linearised claim terms for prior withdrawers.
    //
    // This makes settlement "self-preserving" after withdrawals:
    // - remaining lenders are priced off the current vault,
    // - prior withdrawers still influence the next SF through HaircutState,
    // - borrower repayments can improve SF immediately,
    // - repeating `re_settle` with identical state is idempotent and must
    //   revert with `SettlementNotImproved`.
    let weight_sum = haircut_state.claim_weight_sum();
    let offset_sum = haircut_state.claim_offset_sum();
    let new_factor = crate::logic::haircuts::compute_resettle_factor(
        vault_balance,
        total_normalized,
        weight_sum,
        offset_sum,
    )?;

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
