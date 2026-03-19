use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_HAIRCUT_STATE, DISC_LENDER_POSITION, DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_HAIRCUT_STATE,
    SEED_LENDER, SEED_MARKET_AUTHORITY, SETTLEMENT_GRACE_PERIOD, WAD,
};
use crate::error::LendingError;
use crate::logic::interest::accrue_interest;
use crate::logic::validation::{
    check_blacklist, get_unix_timestamp, validate_market_authority, validate_market_pda,
    validate_protocol_config_pda,
};
use crate::state::{HaircutState, LenderPosition, Market, ProtocolConfig};

/// Withdraw (disc 7)
/// Lender withdraws their proportional share of USDC after maturity.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 10 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let lender = &accounts[1];
    let lender_token_account = &accounts[2];
    let vault_account = &accounts[3];
    let lender_position_account = &accounts[4];
    let market_authority = &accounts[5];
    let blacklist_check = &accounts[6];
    let protocol_config_account = &accounts[7];
    let token_program = &accounts[8];
    let haircut_state_account = &accounts[9];

    // Validate token program
    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    // Parse instruction data: scaled_amount (16 bytes, u128) + min_payout (8 bytes, u64)
    if data.len() < 24 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let mut scaled_amount_bytes = [0u8; 16];
    scaled_amount_bytes.copy_from_slice(&data[0..16]);
    let mut scaled_amount = u128::from_le_bytes(scaled_amount_bytes);

    // SR-111: min_payout for slippage protection (0 = no minimum)
    let min_payout = u64::from_le_bytes(
        data[16..24]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

    // Lender must be signer
    if !lender.is_signer() {
        return Err(LendingError::Unauthorized.into());
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

    // SR-053: vault must match
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // SR-049: must be at or past maturity
    let current_ts = get_unix_timestamp()?;
    if current_ts < market.maturity_timestamp() {
        return Err(LendingError::NotMatured.into());
    }

    // SR-050: blacklist check for lender
    check_blacklist(blacklist_check, config, lender.address())?;

    // Step 1: Accrue interest (capped at maturity)
    accrue_interest(market, config, current_ts)?;

    // Read lender position
    let (expected_pos_pda, _) = Address::find_program_address(
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
    // Verify ownership before deserializing
    if !lender_position_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let pos_data = unsafe { lender_position_account.borrow_unchecked_mut() };
    let position: &mut LenderPosition =
        bytemuck::try_from_bytes_mut(pos_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for LenderPosition
    if position.discriminator != DISC_LENDER_POSITION {
        return Err(ProgramError::InvalidAccountData);
    }

    // SR-051: scaled_balance must be > 0
    if position.scaled_balance() == 0 {
        return Err(LendingError::NoBalance.into());
    }

    // Step 2: Compute or retrieve settlement factor (one-time lock)
    //
    // H-2: Known limitation — settlement race condition
    // The settlement factor is locked on the first withdrawal after maturity + grace period.
    // If a borrower repays between the first and second withdrawal, the factor cannot improve
    // (unless ReSettle is called). We cannot add a slot-based lock because the Market struct
    // has a fixed size (250 bytes) and adding fields would break deserialization.
    // Mitigations: (1) ReSettle instruction allows factor improvement, (2) SETTLEMENT_GRACE_PERIOD
    // gives borrowers time to repay before first settlement.
    //
    // M-1/L-4: Settlement factor is clamped to [1, WAD].
    // - WAD (1e18) means lenders receive 100% of their entitled payout.
    // - Values < WAD indicate underfunded vault (lenders receive proportional haircut).
    // - Clamped to minimum of 1 to prevent division-by-zero in payout computation.
    // - Capped at WAD because excess funds belong to the borrower (not lenders).
    if market.settlement_factor_wad() == 0 {
        // SR-112: Settlement grace period to prevent front-running
        // First settlement must wait SETTLEMENT_GRACE_PERIOD after maturity
        let grace_end = market
            .maturity_timestamp()
            .checked_add(SETTLEMENT_GRACE_PERIOD)
            .ok_or(LendingError::MathOverflow)?;
        if current_ts < grace_end {
            return Err(LendingError::SettlementGracePeriod.into());
        }

        // First post-maturity withdrawal -- settle the market
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
        let vault_balance = u128::from(vault_token.amount());
        // COAL-C01: Compute settlement factor from full vault balance.
        // Fee reservation is removed — the collect_fees distress guard (SR-057)
        // already prevents fee extraction when settlement_factor < WAD, so
        // reserving fees here only harms lenders in distressed markets.
        let available_for_lenders = vault_balance;

        let total_normalized = market
            .scaled_total_supply()
            .checked_mul(market.scale_factor())
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?;

        let settlement_factor = if total_normalized == 0 {
            WAD
        } else {
            let raw = available_for_lenders
                .checked_mul(WAD)
                .ok_or(LendingError::MathOverflow)?
                .checked_div(total_normalized)
                .ok_or(LendingError::MathOverflow)?;
            // max(1, min(WAD, raw))
            let capped = if raw > WAD { WAD } else { raw };
            if capped < 1 {
                1
            } else {
                capped
            }
        };

        market.set_settlement_factor_wad(settlement_factor);
    }

    // Step 3: Resolve scaled amount (0 = full withdrawal)
    if scaled_amount == 0 {
        scaled_amount = position.scaled_balance();
    }

    // SR-052: scaled_amount <= position.scaled_balance
    if scaled_amount > position.scaled_balance() {
        return Err(LendingError::InsufficientScaledBalance.into());
    }

    // Step 4: Compute payout
    let scale_factor = market.scale_factor();
    let settlement_factor = market.settlement_factor_wad();

    // SR-122: Explicit check for zero scale_factor (defense-in-depth)
    if scale_factor == 0 {
        return Err(LendingError::InvalidScaleFactor.into());
    }

    let normalized_amount = scaled_amount
        .checked_mul(scale_factor)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(WAD)
        .ok_or(LendingError::MathOverflow)?;

    let payout_u128 = normalized_amount
        .checked_mul(settlement_factor)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(WAD)
        .ok_or(LendingError::MathOverflow)?;

    let payout = u64::try_from(payout_u128).map_err(|_| LendingError::MathOverflow)?;

    if payout == 0 {
        return Err(LendingError::ZeroPayout.into());
    }

    // SR-111: Slippage protection - verify payout meets minimum
    if min_payout > 0 && payout < min_payout {
        return Err(LendingError::PayoutBelowMinimum.into());
    }

    // C-3: Verify market authority PDA and re-derive bump
    validate_market_authority(market_authority, market_account, market, program_id)?;

    // H-03: Verify lender token account ownership before transfer
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let lender_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(lender_token_account)?
    };
    if lender_token.owner() != lender.address() {
        return Err(LendingError::InvalidTokenAccountOwner.into());
    }

    // COAL-H01: Track the unpaid portion of a distressed withdrawal.
    //
    // This instruction deliberately updates an exact tally and a conservative
    // upper bound on the same obligation:
    // - exact view: `position.haircut_owed`, `position.withdrawal_sf`, and the
    //   market-level `haircut_accumulator`;
    // - conservative settlement view: `HaircutState { weight_sum, offset_sum }`.
    //
    // The exact view answers "how much is this lender still owed right now?"
    // and is what claim/sweep paths enforce. The conservative view answers
    // "what is the maximum settlement factor that still leaves enough value for
    // both remaining lenders and already-withdrawn lenders if SF improves?"
    //
    // Keeping those roles separate avoids hiding borrower repayments from
    // `re_settle`, while still preventing borrower/fee sweep instructions from
    // draining haircut-reserved funds.
    if settlement_factor < WAD {
        let entitled = u64::try_from(normalized_amount).map_err(|_| LendingError::MathOverflow)?;
        let gap = entitled.saturating_sub(payout);
        if gap > 0 {
            // Validate and borrow HaircutState PDA
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
            let haircut_state: &mut HaircutState = bytemuck::try_from_bytes_mut(hs_data)
                .map_err(|_| ProgramError::InvalidAccountData)?;
            if haircut_state.discriminator != DISC_HAIRCUT_STATE {
                return Err(ProgramError::InvalidAccountData);
            }
            if haircut_state.market != *market_account.address().as_ref() {
                return Err(LendingError::InvalidPDA.into());
            }

            // Step 1: Remove any prior conservative contribution for this
            // position before recomputing it at the new anchor.
            let existing_owed = position.haircut_owed();
            let existing_sf = position.withdrawal_sf();
            if existing_owed > 0 {
                let (old_w, old_o) =
                    crate::logic::haircuts::position_contribution(existing_owed, existing_sf)?;
                let w_sum = haircut_state.claim_weight_sum().saturating_sub(old_w);
                let o_sum = haircut_state.claim_offset_sum().saturating_sub(old_o);
                haircut_state.set_claim_weight_sum(w_sum);
                haircut_state.set_claim_offset_sum(o_sum);
            }

            // Step 2: If this lender already had an outstanding haircut and the
            // market SF improved since their last anchor, shrink that old debt
            // to the still-unrecovered remainder at the new anchor. The
            // recovered portion leaves `haircut_accumulator` immediately,
            // because those funds are no longer reserved for this position.
            let rebased_owed = if existing_owed > 0 && existing_sf != settlement_factor {
                let remaining =
                    crate::logic::haircuts::rebase_remaining_owed(existing_owed, existing_sf, settlement_factor)?;
                let recovered = existing_owed.saturating_sub(remaining);
                if recovered > 0 {
                    let new_acc = market.haircut_accumulator().saturating_sub(recovered);
                    market.set_haircut_accumulator(new_acc);
                }
                remaining
            } else {
                existing_owed
            };

            // Step 3: Add the new shortfall created by this withdrawal at the
            // current settlement factor.
            let new_owed = rebased_owed
                .checked_add(gap)
                .ok_or(LendingError::MathOverflow)?;
            position.set_haircut_owed(new_owed);
            position.set_withdrawal_sf(settlement_factor);

            // Step 4: Update the exact market-wide reserve. This is what later
            // prevents `withdraw_excess` / `collect_fees` from sweeping value
            // that belongs to prior withdrawers.
            let new_acc = market
                .haircut_accumulator()
                .checked_add(gap)
                .ok_or(LendingError::MathOverflow)?;
            market.set_haircut_accumulator(new_acc);

            // Step 5: Reinsert this position into the conservative settlement
            // aggregate at its new anchor.
            let (new_w, new_o) =
                crate::logic::haircuts::position_contribution(new_owed, settlement_factor)?;
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
    }

    // CEI: Update state BEFORE transfer CPI (Finding 3).
    let new_balance = position
        .scaled_balance()
        .checked_sub(scaled_amount)
        .ok_or(LendingError::MathOverflow)?;
    position.set_scaled_balance(new_balance);

    let new_scaled_total = market
        .scaled_total_supply()
        .checked_sub(scaled_amount)
        .ok_or(LendingError::MathOverflow)?;
    market.set_scaled_total_supply(new_scaled_total);

    // COAL-M01: Decrement total_deposited to keep the counter consistent with
    // the actual principal held. Deposits are only allowed before maturity, so
    // this does not free cap space for new deposits.
    let new_total_deposited = market.total_deposited().saturating_sub(payout);
    market.set_total_deposited(new_total_deposited);

    // Step 5: Transfer tokens (vault -> lender) with PDA signing
    let auth_bump_ref = [market.market_authority_bump];
    let auth_seeds = [
        pinocchio::cpi::Seed::from(SEED_MARKET_AUTHORITY),
        pinocchio::cpi::Seed::from(market_account.address().as_ref()),
        pinocchio::cpi::Seed::from(&auth_bump_ref),
    ];
    pinocchio_token::instructions::Transfer {
        from: vault_account,
        to: lender_token_account,
        authority: market_authority,
        amount: payout,
    }
    .invoke_signed(&[pinocchio::cpi::Signer::from(&auth_seeds)])?;

    log!(
        "evt:withdraw market={} lender={} payout={} scaled={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(lender.address().as_ref()),
        payout,
        scaled_amount
    );

    Ok(())
}
