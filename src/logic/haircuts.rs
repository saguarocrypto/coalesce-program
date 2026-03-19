//! Shared haircut math for distressed-withdrawal recovery.
//!
//! The protocol now tracks haircut state in two complementary layers:
//! 1. `position.haircut_owed` + `position.withdrawal_sf` store the exact unpaid
//!    portion for each lender who withdrew while `settlement_factor_wad < WAD`.
//! 2. `market.haircut_accumulator` stores the sum of all still-unpaid haircut
//!    amounts so borrower/fee sweep paths cannot drain tokens reserved for those
//!    lenders.
//! 3. `HaircutState { claim_weight_sum, claim_offset_sum }` stores a
//!    conservative linearised view of all outstanding claims so `re_settle` can
//!    solve for the next settlement factor without subtracting the accumulator
//!    from the vault balance.
//!
//! That split is important:
//! - `re_settle` must see borrower repayments as immediately improving
//!   settlement quality for both remaining lenders and prior withdrawers.
//! - `withdraw_excess` / `collect_fees` must still treat unpaid haircuts as
//!   reserved value that cannot be swept out of the vault.
//! - `claim_haircut` / `force_claim_haircut` pay the exact per-position claim
//!   implied by the improvement from `withdrawal_sf` to the current market SF,
//!   then decrement both the exact accumulator and the conservative aggregate.
//!
//! All processors that touch haircut state (withdraw, force_close_position,
//! claim_haircut, force_claim_haircut, re_settle) use these helpers so the
//! exact and conservative views stay in sync.

use crate::constants::WAD;
use crate::error::LendingError;
use pinocchio::error::ProgramError;

/// Rebase an existing haircut owed from `old_sf` to `new_sf`.
///
/// Returns the portion of `old_owed` that is still outstanding at `new_sf`:
///
/// ```text
/// remaining = old_owed * (WAD - new_sf) / (WAD - old_sf)
/// ```
///
/// This is used when a lender who already has an outstanding haircut withdraws
/// again at a better settlement factor. The earlier unpaid amount is first
/// shrunk to the still-unrecovered remainder at the new anchor, then any new
/// gap from the fresh withdrawal is added on top.
///
/// Rounds **down** (conservative for the claimant — the protocol keeps the dust).
pub fn rebase_remaining_owed(old_owed: u64, old_sf: u128, new_sf: u128) -> Result<u64, ProgramError> {
    if old_owed == 0 || old_sf == new_sf {
        return Ok(old_owed);
    }
    let wad_minus_new = WAD
        .checked_sub(new_sf)
        .ok_or(LendingError::MathOverflow)?;
    let wad_minus_old = WAD
        .checked_sub(old_sf)
        .ok_or(LendingError::MathOverflow)?;
    if wad_minus_old == 0 {
        // old_sf == WAD means no distress at that anchor — owed should be 0.
        return Ok(0);
    }
    let remaining = u128::from(old_owed)
        .checked_mul(wad_minus_new)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(wad_minus_old)
        .ok_or(LendingError::MathOverflow)?;
    u64::try_from(remaining).map_err(|_| LendingError::MathOverflow.into())
}

/// Compute the exact claimable amount for a position.
///
/// ```text
/// claimable = owed * (current_sf - anchor_sf) / (WAD - anchor_sf)
/// ```
///
/// This is the lender-facing claim rule:
/// - at the original withdrawal SF, claimable is zero;
/// - as SF improves, a proportional slice of the haircut becomes withdrawable;
/// - at `current_sf == WAD`, the full remaining haircut becomes claimable.
///
/// Returns 0 if `current_sf <= anchor_sf`.
/// Rounds **down** (conservative for the claimant).
pub fn claimable_exact(owed: u64, anchor_sf: u128, current_sf: u128) -> Result<u64, ProgramError> {
    if owed == 0 || current_sf <= anchor_sf {
        return Ok(0);
    }
    let sf_delta = current_sf
        .checked_sub(anchor_sf)
        .ok_or(LendingError::MathOverflow)?;
    let sf_remaining = WAD
        .checked_sub(anchor_sf)
        .ok_or(LendingError::MathOverflow)?;
    if sf_remaining == 0 {
        // anchor_sf == WAD means no distress — nothing to claim.
        return Ok(0);
    }
    let result = u128::from(owed)
        .checked_mul(sf_delta)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(sf_remaining)
        .ok_or(LendingError::MathOverflow)?;
    u64::try_from(result).map_err(|_| LendingError::MathOverflow.into())
}

/// Compute the conservative `(weight, offset)` contribution for a position.
///
/// ```text
/// weight = ceil(owed * WAD / (WAD - anchor))
/// offset = floor(weight * anchor / WAD)
/// ```
///
/// The upper-bound claim at any `sf` is `weight * sf / WAD - offset`.
///
/// `re_settle` sums these conservative lines across all haircutted positions.
/// That lets it account for prior withdrawers without subtracting
/// `haircut_accumulator` from the vault balance. Repayments therefore remain
/// visible to settlement improvement, while the resulting factor is still
/// conservative and idempotent.
///
/// The upper bound is always >= the exact claim, so `re_settle` never
/// overstates solvency and the same `(vault, remaining_supply, HaircutState)`
/// tuple always produces the same settlement factor.
pub fn position_contribution(owed: u64, anchor_sf: u128) -> Result<(u128, u128), ProgramError> {
    if owed == 0 {
        return Ok((0, 0));
    }
    let wad_minus_anchor = WAD
        .checked_sub(anchor_sf)
        .ok_or(LendingError::MathOverflow)?;
    if wad_minus_anchor == 0 {
        return Ok((0, 0));
    }
    // weight = ceil(owed * WAD / (WAD - anchor))
    //        = (owed * WAD + (WAD - anchor) - 1) / (WAD - anchor)
    let numerator = u128::from(owed)
        .checked_mul(WAD)
        .ok_or(LendingError::MathOverflow)?;
    let weight = numerator
        .checked_add(wad_minus_anchor)
        .ok_or(LendingError::MathOverflow)?
        .checked_sub(1)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(wad_minus_anchor)
        .ok_or(LendingError::MathOverflow)?;

    // offset = floor(weight * anchor / WAD)
    let offset = weight
        .checked_mul(anchor_sf)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(WAD)
        .ok_or(LendingError::MathOverflow)?;

    Ok((weight, offset))
}

/// Compute the conservative settlement factor from the `re_settle` formula.
///
/// ```text
/// new_sf = WAD * (V + O) / (R + W)
/// ```
///
/// Where:
/// - `V` is the current vault balance,
/// - `R` is the normalized claim of lenders who still have balance in the market,
/// - `W` / `O` are the aggregate conservative haircut terms from `HaircutState`.
///
/// Intuition:
/// - remaining lenders consume `R * sf / WAD`,
/// - prior withdrawers can still claim up to `W * sf / WAD - O`,
/// - solving `vault >= remaining_lenders + prior_withdrawers` gives the
///   formula above.
///
/// Clamped to `[1, WAD]`.  Returns `WAD` if `R + W == 0`.
pub fn compute_resettle_factor(
    vault_balance: u128,
    total_normalized: u128,
    weight_sum: u128,
    offset_sum: u128,
) -> Result<u128, ProgramError> {
    let denominator = total_normalized
        .checked_add(weight_sum)
        .ok_or(LendingError::MathOverflow)?;
    if denominator == 0 {
        return Ok(WAD);
    }
    let numerator_inner = vault_balance
        .checked_add(offset_sum)
        .ok_or(LendingError::MathOverflow)?;
    let raw = numerator_inner
        .checked_mul(WAD)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(denominator)
        .ok_or(LendingError::MathOverflow)?;
    let capped = if raw > WAD { WAD } else { raw };
    Ok(if capped < 1 { 1 } else { capped })
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    const HALF_WAD: u128 = WAD / 2; // 0.5

    // ── rebase_remaining_owed ─────────────────────────────────────────

    #[test]
    fn rebase_no_change_when_same_sf() {
        assert_eq!(rebase_remaining_owed(250_000, HALF_WAD, HALF_WAD).unwrap(), 250_000);
    }

    #[test]
    fn rebase_zero_owed() {
        assert_eq!(rebase_remaining_owed(0, HALF_WAD, WAD * 3 / 4).unwrap(), 0);
    }

    #[test]
    fn rebase_half_to_three_quarters() {
        // owed=250k anchored at 0.5, rebase to 0.75
        // remaining = 250k * (1.0 - 0.75) / (1.0 - 0.5) = 250k * 0.5 = 125k
        let r = rebase_remaining_owed(250_000, HALF_WAD, WAD * 3 / 4).unwrap();
        assert_eq!(r, 125_000);
    }

    #[test]
    fn rebase_to_wad_gives_zero() {
        assert_eq!(rebase_remaining_owed(250_000, HALF_WAD, WAD).unwrap(), 0);
    }

    // ── claimable_exact ───────────────────────────────────────────────

    #[test]
    fn claimable_no_improvement() {
        assert_eq!(claimable_exact(250_000, HALF_WAD, HALF_WAD).unwrap(), 0);
    }

    #[test]
    fn claimable_full_recovery() {
        // sf reaches WAD → full haircut recovered
        assert_eq!(claimable_exact(250_000, HALF_WAD, WAD).unwrap(), 250_000);
    }

    #[test]
    fn claimable_partial_recovery() {
        // anchor=0.5, current=0.75 → 250k * 0.25 / 0.5 = 125k
        assert_eq!(claimable_exact(250_000, HALF_WAD, WAD * 3 / 4).unwrap(), 125_000);
    }

    #[test]
    fn claimable_zero_owed() {
        assert_eq!(claimable_exact(0, HALF_WAD, WAD).unwrap(), 0);
    }

    // ── position_contribution ─────────────────────────────────────────

    #[test]
    fn contribution_zero_owed() {
        let (w, o) = position_contribution(0, HALF_WAD).unwrap();
        assert_eq!(w, 0);
        assert_eq!(o, 0);
    }

    #[test]
    fn contribution_half_wad_anchor() {
        // owed=250k, anchor=0.5
        // weight = ceil(250k * WAD / (0.5 * WAD)) = 500k
        // offset = floor(500k * 0.5*WAD / WAD) = 250k
        let (w, o) = position_contribution(250_000, HALF_WAD).unwrap();
        assert_eq!(w, 500_000);
        assert_eq!(o, 250_000);
    }

    #[test]
    fn contribution_upper_bound_ge_exact() {
        // For arbitrary anchor/sf, claim_upper >= claim_exact
        let owed = 100_000u64;
        let anchor = WAD * 4 / 10; // 0.4
        let (w, o) = position_contribution(owed, anchor).unwrap();

        for sf_bps in [50u128, 60, 70, 80, 90, 100] {
            let sf = WAD * sf_bps / 100;
            if sf <= anchor {
                continue;
            }
            let exact = claimable_exact(owed, anchor, sf).unwrap();
            let upper = w
                .checked_mul(sf)
                .unwrap()
                .checked_div(WAD)
                .unwrap()
                .checked_sub(o)
                .unwrap();
            assert!(
                upper >= u128::from(exact),
                "upper {upper} < exact {exact} at sf_bps={sf_bps}"
            );
        }
    }

    // ── compute_resettle_factor ───────────────────────────────────────

    #[test]
    fn resettle_no_haircuts_is_standard() {
        // V=500k, R=1M, W=0, O=0 → sf = WAD * 500k / 1M = 0.5*WAD
        let sf = compute_resettle_factor(500_000, 1_000_000, 0, 0).unwrap();
        assert_eq!(sf, HALF_WAD);
    }

    #[test]
    fn resettle_with_haircuts_gives_0p75() {
        // The auditor's example: V=500k, R=500k, H=250k at anchor=0.5
        // weight=500k, offset=250k
        // sf = WAD * (500k + 250k) / (500k + 500k) = 0.75 * WAD
        let sf = compute_resettle_factor(500_000, 500_000, 500_000, 250_000).unwrap();
        assert_eq!(sf, WAD * 3 / 4);
    }

    #[test]
    fn resettle_is_idempotent() {
        // Same inputs → same output. A second re_settle must not improve.
        let sf1 = compute_resettle_factor(500_000, 500_000, 500_000, 250_000).unwrap();
        let sf2 = compute_resettle_factor(500_000, 500_000, 500_000, 250_000).unwrap();
        assert_eq!(sf1, sf2);
    }

    #[test]
    fn resettle_only_haircut_claims() {
        // R=0 (all remaining lenders withdrew), W=500k, O=250k, V=250k
        // sf = WAD * (250k + 250k) / (0 + 500k) = WAD
        let sf = compute_resettle_factor(250_000, 0, 500_000, 250_000).unwrap();
        assert_eq!(sf, WAD);
    }

    #[test]
    fn resettle_empty_market() {
        // No claims at all
        let sf = compute_resettle_factor(0, 0, 0, 0).unwrap();
        assert_eq!(sf, WAD);
    }

    #[test]
    fn resettle_capped_at_wad() {
        // Vault larger than total claims → cap at WAD
        let sf = compute_resettle_factor(10_000_000, 500_000, 0, 0).unwrap();
        assert_eq!(sf, WAD);
    }

    #[test]
    fn resettle_minimum_clamp() {
        // Vault = 0, claims exist → sf = 1 (minimum)
        let sf = compute_resettle_factor(0, 1_000_000, 0, 0).unwrap();
        assert_eq!(sf, 1);
    }

    // ── mixed-anchor idempotence ──────────────────────────────────────

    #[test]
    fn resettle_mixed_anchors_stable() {
        // Two positions with different anchors:
        // A: owed=200k, anchor=0.4   → weight=333334, offset=133333
        // B: owed=50k,  anchor=0.7   → weight=166667, offset=116666
        let (wa, oa) = position_contribution(200_000, WAD * 4 / 10).unwrap();
        let (wb, ob) = position_contribution(50_000, WAD * 7 / 10).unwrap();
        let w = wa + wb;
        let o = oa + ob;

        let sf1 = compute_resettle_factor(500_000, 500_000, w, o).unwrap();
        let sf2 = compute_resettle_factor(500_000, 500_000, w, o).unwrap();
        assert_eq!(sf1, sf2, "mixed-anchor re_settle must be idempotent");
    }

    // ── rebase + contribution consistency ─────────────────────────────

    #[test]
    fn rebase_then_new_contribution_is_consistent() {
        // After rebase from 0.5 to 0.75, the remaining owed should produce
        // a contribution that when added back is consistent.
        let remaining = rebase_remaining_owed(250_000, HALF_WAD, WAD * 3 / 4).unwrap();
        assert_eq!(remaining, 125_000);
        let (w, o) = position_contribution(remaining, WAD * 3 / 4).unwrap();
        // weight = ceil(125k * WAD / (0.25 * WAD)) = 500k
        // offset = floor(500k * 0.75*WAD / WAD) = 375k
        assert_eq!(w, 500_000);
        assert_eq!(o, 375_000);

        // Claim at WAD: upper = 500k * 1.0 - 375k = 125k ≥ exact(125k) ✓
        let exact = claimable_exact(remaining, WAD * 3 / 4, WAD).unwrap();
        assert_eq!(exact, 125_000);
        let upper = w * WAD / WAD - o;
        assert!(upper >= u128::from(exact));
    }
}
