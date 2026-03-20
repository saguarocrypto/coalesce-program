use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_HAIRCUT_STATE, DISC_LENDER_POSITION, DISC_MARKET, DISC_PROTOCOL_CONFIG,
    SEED_HAIRCUT_STATE, SEED_LENDER, SEED_MARKET_AUTHORITY, SETTLEMENT_GRACE_PERIOD, WAD,
};
use crate::error::LendingError;
use crate::logic::interest::accrue_interest;
use crate::logic::validation::{
    validate_market_authority, validate_market_pda, validate_protocol_config_pda,
};
use crate::state::{HaircutState, LenderPosition, Market, ProtocolConfig};

/// ForceClosePosition (disc 18)
/// Borrower force-closes a lender position after maturity + grace period.
/// Computes payout (same formula as withdraw), transfers to escrow ATA,
/// zeros the position, and decrements scaled_total_supply.
/// Enables withdraw_excess to succeed when dust, lost wallets, or blacklisted
/// lenders prevent voluntary withdrawal.
pub fn process(program_id: &Address, accounts: &[AccountView], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 9 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let borrower = &accounts[1];
    let lender_position_account = &accounts[2];
    let vault_account = &accounts[3];
    let escrow_token_account = &accounts[4];
    let market_authority = &accounts[5];
    let protocol_config_account = &accounts[6];
    let token_program = &accounts[7];
    let haircut_state_account = &accounts[8];

    // Validate token program
    if token_program.address() != &pinocchio_token::ID {
        return Err(LendingError::InvalidTokenProgram.into());
    }

    // Borrower must be signer
    if !borrower.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // Verify market ownership and deserialize
    if !market_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let market_data = unsafe { market_account.borrow_unchecked_mut() };
    let market: &mut Market =
        bytemuck::try_from_bytes_mut(market_data).map_err(|_| ProgramError::InvalidAccountData)?;

    if market.discriminator != DISC_MARKET {
        return Err(ProgramError::InvalidAccountData);
    }

    // Validate market PDA
    validate_market_pda(market_account, market, program_id)?;

    // Borrower must match market
    if *borrower.address().as_ref() != market.borrower {
        return Err(LendingError::Unauthorized.into());
    }

    // Vault must match market
    if market.vault != *vault_account.address().as_ref() {
        return Err(LendingError::InvalidVault.into());
    }

    // Validate protocol config
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

    // Emergency pause check
    if config.is_paused() {
        return Err(LendingError::ProtocolPaused.into());
    }

    // Must be past maturity + grace period
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

    // Accrue interest (capped at maturity — ensures scale_factor is up-to-date)
    accrue_interest(market, config, current_ts)?;

    // Compute or use existing settlement factor.
    // Unlike withdraw (which requires a lender to be present), force_close must
    // handle the case where NO lender has withdrawn yet (settlement_factor == 0).
    // Without this, the borrower cannot clear the first/only abandoned position.
    let settlement_factor = if market.settlement_factor_wad() == 0 {
        // First settlement — compute from vault balance (same logic as withdraw.rs)
        if unsafe { vault_account.owner() } != &pinocchio_token::ID {
            return Err(LendingError::InvalidAccountOwner.into());
        }
        let vault_token = unsafe {
            pinocchio_token::state::TokenAccount::from_account_view_unchecked(vault_account)?
        };
        if *vault_token.mint().as_ref() != market.mint {
            return Err(LendingError::InvalidMint.into());
        }
        let vault_balance = u128::from(vault_token.amount());

        let sf = market.scale_factor();
        if sf == 0 {
            return Err(LendingError::InvalidScaleFactor.into());
        }

        let total_normalized = market
            .scaled_total_supply()
            .checked_mul(sf)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?;

        let factor = if total_normalized == 0 {
            WAD
        } else {
            let raw = vault_balance
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

        market.set_settlement_factor_wad(factor);
        factor
    } else {
        market.settlement_factor_wad()
    };

    // Read lender position
    // Derive lender address from the position account (we read it below)
    if !lender_position_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let pos_data = unsafe { lender_position_account.borrow_unchecked_mut() };
    let position: &mut LenderPosition =
        bytemuck::try_from_bytes_mut(pos_data).map_err(|_| ProgramError::InvalidAccountData)?;

    if position.discriminator != DISC_LENDER_POSITION {
        return Err(ProgramError::InvalidAccountData);
    }

    // Verify position belongs to this market
    if position.market != *market_account.address().as_ref() {
        return Err(LendingError::InvalidPDA.into());
    }

    // Verify lender position PDA derivation using the lender stored in the position
    // SAFETY: lender field is [u8; 32] same layout as Address
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

    // Validate escrow token account is owned by token program
    if unsafe { escrow_token_account.owner() } != &pinocchio_token::ID {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // Validate escrow token account belongs to the lender (prevents fund redirection)
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let escrow_token = unsafe {
        pinocchio_token::state::TokenAccount::from_account_view_unchecked(escrow_token_account)?
    };
    if escrow_token.owner() != lender_address {
        return Err(LendingError::InvalidTokenAccountOwner.into());
    }
    // Validate escrow token mint matches market mint
    if *escrow_token.mint().as_ref() != market.mint {
        return Err(LendingError::InvalidMint.into());
    }

    // Position must have balance
    let scaled_amount = position.scaled_balance();
    if scaled_amount == 0 {
        return Err(LendingError::NoBalance.into());
    }

    // Compute payout (same formula as withdraw.rs)
    let scale_factor = market.scale_factor();
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

    // Validate market authority PDA for CPI signing
    validate_market_authority(market_authority, market_account, market, program_id)?;

    // COAL-H01: Track haircut gap during distress.
    // Same state transition as withdraw.rs — see there for full rationale.
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

            // Step 1: Remove old contribution from HaircutState
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

            // Step 2: Rebase existing haircut if SF changed
            let rebased_owed = if existing_owed > 0 && existing_sf != settlement_factor {
                let remaining = crate::logic::haircuts::rebase_remaining_owed(
                    existing_owed,
                    existing_sf,
                    settlement_factor,
                )?;
                let recovered = existing_owed.saturating_sub(remaining);
                if recovered > 0 {
                    let new_acc = market.haircut_accumulator().saturating_sub(recovered);
                    market.set_haircut_accumulator(new_acc);
                }
                remaining
            } else {
                existing_owed
            };

            // Step 3: Add new gap
            let new_owed = rebased_owed
                .checked_add(gap)
                .ok_or(LendingError::MathOverflow)?;
            position.set_haircut_owed(new_owed);
            position.set_withdrawal_sf(settlement_factor);

            // Step 4: Update market accumulator
            let new_acc = market
                .haircut_accumulator()
                .checked_add(gap)
                .ok_or(LendingError::MathOverflow)?;
            market.set_haircut_accumulator(new_acc);

            // Step 5: Add new contribution to HaircutState
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

    // CEI: Update state BEFORE transfer CPI
    position.set_scaled_balance(0);

    let new_scaled_total = market
        .scaled_total_supply()
        .checked_sub(scaled_amount)
        .ok_or(LendingError::MathOverflow)?;
    market.set_scaled_total_supply(new_scaled_total);

    // Transfer payout to escrow (skip if payout == 0 — dust/deadlock case)
    if payout > 0 {
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
            amount: payout,
        }
        .invoke_signed(&[pinocchio::cpi::Signer::from(&auth_seeds)])?;
    }

    log!(
        "evt:force_close market={} lender={} payout={} scaled={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(lender_address.as_ref()),
        payout,
        scaled_amount
    );

    Ok(())
}
