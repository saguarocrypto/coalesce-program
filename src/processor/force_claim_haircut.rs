use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_HAIRCUT_STATE, DISC_LENDER_POSITION, DISC_MARKET, DISC_PROTOCOL_CONFIG,
    SEED_HAIRCUT_STATE, SEED_LENDER, SEED_MARKET_AUTHORITY, SETTLEMENT_GRACE_PERIOD, WAD,
};
use crate::error::LendingError;
use crate::logic::validation::{
    validate_market_authority, validate_market_pda, validate_protocol_config_pda,
};
use crate::state::{HaircutState, LenderPosition, Market, ProtocolConfig};

/// ForceClaimHaircut (disc 20)
/// Borrower force-claims a haircut on behalf of an abandoned or blacklisted lender.
/// Mirrors force_close_position: borrower signs, funds go to the lender's token
/// account. Prevents permanently stuck accumulators from blocking withdraw_excess.
pub fn process(program_id: &Address, accounts: &[AccountView], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 9 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let borrower = &accounts[1];
    let lender_position_account = &accounts[2];
    let escrow_token_account = &accounts[3];
    let vault_account = &accounts[4];
    let market_authority = &accounts[5];
    let haircut_state_account = &accounts[6];
    let protocol_config_account = &accounts[7];
    let token_program = &accounts[8];

    // --- Standard validations ---

    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    if !borrower.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    validate_protocol_config_pda(protocol_config_account, program_id)?;
    if !protocol_config_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let config_data = unsafe { protocol_config_account.borrow_unchecked() };
    let config: &ProtocolConfig =
        bytemuck::try_from_bytes(config_data).map_err(|_| ProgramError::InvalidAccountData)?;
    if config.discriminator != DISC_PROTOCOL_CONFIG {
        return Err(ProgramError::InvalidAccountData);
    }
    if config.is_paused() {
        return Err(LendingError::ProtocolPaused.into());
    }

    // --- Market validation ---

    if !market_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    let market_data = unsafe { market_account.borrow_unchecked_mut() };
    let market: &mut Market =
        bytemuck::try_from_bytes_mut(market_data).map_err(|_| ProgramError::InvalidAccountData)?;
    if market.discriminator != DISC_MARKET {
        return Err(ProgramError::InvalidAccountData);
    }
    validate_market_pda(market_account, market, program_id)?;

    if *borrower.address().as_ref() != market.borrower {
        return Err(LendingError::Unauthorized.into());
    }
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // Must be past maturity + grace period (same gate as force_close_position)
    let current_ts = crate::logic::validation::get_unix_timestamp()?;
    if current_ts < market.maturity_timestamp() {
        return Err(LendingError::NotMatured.into());
    }
    let grace_end = market
        .maturity_timestamp()
        .checked_add(SETTLEMENT_GRACE_PERIOD)
        .ok_or(LendingError::MathOverflow)?;
    if current_ts < grace_end {
        return Err(LendingError::SettlementGracePeriod.into());
    }

    let current_sf = market.settlement_factor_wad();
    if current_sf == 0 {
        return Err(LendingError::NotSettled.into());
    }

    // --- Position validation ---

    if !lender_position_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    let pos_data = unsafe { lender_position_account.borrow_unchecked_mut() };
    let position: &mut LenderPosition =
        bytemuck::try_from_bytes_mut(pos_data).map_err(|_| ProgramError::InvalidAccountData)?;
    if position.discriminator != DISC_LENDER_POSITION {
        return Err(ProgramError::InvalidAccountData);
    }
    if position.market != *market_account.address().as_ref() {
        return Err(LendingError::InvalidPDA.into());
    }

    // Derive lender from position (borrower doesn't know the lender wallet)
    let lender_address: &Address = unsafe { &*(position.lender.as_ptr().cast::<Address>()) };
    let (expected_pos_pda, _) = Address::find_program_address(
        &[
            SEED_LENDER,
            market_account.address().as_ref(),
            lender_address.as_ref(),
        ],
        program_id,
    );
    if lender_position_account.address() != &expected_pos_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Validate escrow token account belongs to the lender
    if unsafe { escrow_token_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    let escrow_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(escrow_token_account)?
    };
    if escrow_token.owner() != lender_address {
        return Err(LendingError::InvalidTokenAccountOwner.into());
    }
    if *escrow_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }

    // --- HaircutState validation ---

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
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    let hs_data = unsafe { haircut_state_account.borrow_unchecked_mut() };
    let haircut_state: &mut HaircutState =
        bytemuck::try_from_bytes_mut(hs_data).map_err(|_| ProgramError::InvalidAccountData)?;
    if haircut_state.discriminator != DISC_HAIRCUT_STATE {
        return Err(ProgramError::InvalidAccountData);
    }
    if haircut_state.market != *market_account.address().as_ref() {
        return Err(LendingError::InvalidPDA.into());
    }

    // --- Claim computation (same as claim_haircut) ---

    let haircut_owed = position.haircut_owed();
    if haircut_owed == 0 {
        return Err(LendingError::NoHaircutToClaim.into());
    }

    let withdrawal_sf = position.withdrawal_sf();
    if current_sf <= withdrawal_sf {
        return Err(LendingError::SettlementNotImproved.into());
    }

    let mut claimable =
        crate::logic::haircuts::claimable_exact(haircut_owed, withdrawal_sf, current_sf)?;

    // Defense-in-depth: cap at vault surplus
    if unsafe { vault_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    let vault_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(vault_account)?
    };
    if *vault_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }
    let vault_balance = vault_token.amount();

    let remaining_obligations = if market.scaled_total_supply() > 0 {
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
        let obligations_u128 = total_normalized
            .checked_mul(current_sf)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?;
        u64::try_from(obligations_u128).map_err(|_| LendingError::MathOverflow)?
    } else {
        0u64
    };

    let available = vault_balance.saturating_sub(remaining_obligations);
    if claimable > available {
        claimable = available;
    }
    if claimable == 0 {
        return Err(LendingError::ZeroPayout.into());
    }

    // --- CEI: Update state BEFORE transfer CPI ---

    // Step 1: Remove old contribution from HaircutState
    let (old_w, old_o) =
        crate::logic::haircuts::position_contribution(haircut_owed, withdrawal_sf)?;
    let w_sum = haircut_state.claim_weight_sum().saturating_sub(old_w);
    let o_sum = haircut_state.claim_offset_sum().saturating_sub(old_o);
    haircut_state.set_claim_weight_sum(w_sum);
    haircut_state.set_claim_offset_sum(o_sum);

    // Step 2: Update position
    let new_owed = haircut_owed
        .checked_sub(claimable)
        .ok_or(LendingError::MathOverflow)?;
    position.set_haircut_owed(new_owed);
    if new_owed > 0 {
        position.set_withdrawal_sf(current_sf);
    } else {
        position.set_withdrawal_sf(0);
    }

    // Step 3: Update market accumulator
    let new_acc = market
        .haircut_accumulator()
        .checked_sub(claimable)
        .ok_or(LendingError::MathOverflow)?;
    market.set_haircut_accumulator(new_acc);

    // Step 4: Add new contribution to HaircutState
    if new_owed > 0 {
        let (new_w, new_o) = crate::logic::haircuts::position_contribution(new_owed, current_sf)?;
        let w_sum = haircut_state
            .claim_weight_sum()
            .checked_add(new_w)
            .ok_or(LendingError::MathOverflow)?;
        let o_sum = haircut_state
            .claim_offset_sum()
            .checked_add(new_o)
            .ok_or(LendingError::MathOverflow)?;
        haircut_state.set_claim_weight_sum(w_sum);
        haircut_state.set_claim_offset_sum(o_sum);
    }

    // --- Transfer ---

    validate_market_authority(market_authority, market_account, market, program_id)?;

    let auth_bump_ref = [market.market_authority_bump];
    let auth_seeds = [
        pinocchio::cpi::Seed::from(SEED_MARKET_AUTHORITY),
        pinocchio::cpi::Seed::from(market_account.address().as_ref()),
        pinocchio::cpi::Seed::from(&auth_bump_ref),
    ];
    pinocchio_token::instructions::Transfer {
        from: vault_account,
        to: escrow_token_account,
        authority: market_authority,
        amount: claimable,
    }
    .invoke_signed(&[pinocchio::cpi::Signer::from(&auth_seeds)])?;

    log!(
        "evt:force_claim_haircut market={} lender={} claimed={} remaining={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(lender_address.as_ref()),
        claimable,
        new_owed
    );

    Ok(())
}
