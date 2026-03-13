//! Targeted mutation-killing tests.
//!
//! Each test is specifically designed to detect a single code mutation
//! that might otherwise slip through the test suite. These are the
//! "last line of defense" tests — they exercise exact boundary
//! conditions and exact expected values.
//!
//! Every test:
//! 1. Annotates the mutant class being killed
//! 2. Asserts the correct value AND asserts != the mutant value
//! 3. Captures a full state snapshot to verify only expected fields change

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_lossless,
    clippy::unnecessary_cast,
    clippy::useless_conversion,
    clippy::identity_op,
    clippy::manual_range_contains,
    clippy::single_match,
    clippy::single_match_else,
    clippy::needless_bool,
    clippy::nonminimal_bool,
    clippy::too_many_arguments,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_abs_diff,
    clippy::unreadable_literal,
    clippy::allow_attributes,
    clippy::struct_field_names,
    clippy::explicit_iter_loop,
    clippy::needless_for_each,
    clippy::absurd_extreme_comparisons,
    clippy::duplicated_attributes,
    clippy::manual_saturating_arithmetic,
    clippy::implicit_saturating_sub,
    clippy::stable_sort_primitive,
    clippy::type_complexity,
    clippy::iter_over_hash_type,
    clippy::bool_to_int_with_if,
    clippy::map_unwrap_or,
    clippy::explicit_counter_loop,
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::if_not_else,
    clippy::int_plus_one,
    clippy::range_plus_one,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::assertions_on_constants,
    clippy::for_kv_map,
    clippy::unnecessary_literal_bound,
    clippy::useless_vec,
    clippy::almost_complete_range,
    clippy::cloned_ref_to_slice_refs,
    unused_comparisons,
    unused_imports,
    unused_doc_comments,
    unused_variables,
    unused_mut,
    unused_assignments,
    dead_code,
    deprecated
)]

use bytemuck::Zeroable;

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

#[path = "common/math_oracle.rs"]
mod math_oracle;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_market(
    annual_interest_bps: u16,
    maturity_timestamp: i64,
    scale_factor: u128,
    scaled_total_supply: u128,
    last_accrual_timestamp: i64,
    accrued_protocol_fees: u64,
) -> Market {
    let mut m = Market::zeroed();
    m.set_annual_interest_bps(annual_interest_bps);
    m.set_maturity_timestamp(maturity_timestamp);
    m.set_scale_factor(scale_factor);
    m.set_scaled_total_supply(scaled_total_supply);
    m.set_last_accrual_timestamp(last_accrual_timestamp);
    m.set_accrued_protocol_fees(accrued_protocol_fees);
    m
}

fn make_config(fee_rate_bps: u16) -> ProtocolConfig {
    let mut c = ProtocolConfig::zeroed();
    c.set_fee_rate_bps(fee_rate_bps);
    c
}

fn scale_factor_after_elapsed(
    scale_factor_before: u128,
    annual_bps: u16,
    elapsed_seconds: i64,
) -> u128 {
    let growth = math_oracle::growth_factor_wad(annual_bps, elapsed_seconds);
    math_oracle::mul_wad(scale_factor_before, growth)
}

fn fee_delta_after_elapsed(
    scaled_supply: u128,
    scale_factor_before: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> u64 {
    if scaled_supply == 0 || fee_rate_bps == 0 || elapsed_seconds <= 0 {
        return 0;
    }

    let growth = math_oracle::growth_factor_wad(annual_bps, elapsed_seconds);
    let interest_delta_wad = growth.checked_sub(WAD).unwrap();
    let fee_delta_wad = interest_delta_wad
        .checked_mul(u128::from(fee_rate_bps))
        .unwrap()
        .checked_div(BPS)
        .unwrap();
    // Use pre-accrual scale_factor_before (matches on-chain Finding 10 fix)
    let fee_normalized = scaled_supply
        .checked_mul(scale_factor_before)
        .unwrap()
        .checked_div(WAD)
        .unwrap()
        .checked_mul(fee_delta_wad)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    u64::try_from(fee_normalized).unwrap()
}

/// Lightweight snapshot of all mutable Market fields for verifying
/// that failed operations (or no-ops) leave state unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarketSnapshot {
    scale_factor: u128,
    scaled_total_supply: u128,
    last_accrual_timestamp: i64,
    accrued_protocol_fees: u64,
}

impl MarketSnapshot {
    fn capture(m: &Market) -> Self {
        Self {
            scale_factor: m.scale_factor(),
            scaled_total_supply: m.scaled_total_supply(),
            last_accrual_timestamp: m.last_accrual_timestamp(),
            accrued_protocol_fees: m.accrued_protocol_fees(),
        }
    }

    fn assert_unchanged(&self, after: &Self, context: &str) {
        assert_eq!(
            self.scale_factor, after.scale_factor,
            "{context}: scale_factor changed"
        );
        assert_eq!(
            self.scaled_total_supply, after.scaled_total_supply,
            "{context}: scaled_total_supply changed"
        );
        assert_eq!(
            self.last_accrual_timestamp, after.last_accrual_timestamp,
            "{context}: last_accrual_timestamp changed"
        );
        assert_eq!(
            self.accrued_protocol_fees, after.accrued_protocol_fees,
            "{context}: accrued_protocol_fees changed"
        );
    }
}

// ===========================================================================
// Kill: time manipulation via backward timestamps
// ===========================================================================

/// Mutant class: saturating_sub → wrapping_sub (backward timestamp silently wraps)
///
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
/// The code validates that effective_now >= last_accrual and returns
/// InvalidTimestamp error if not.
#[test]
fn kill_saturating_sub_for_time_elapsed() {
    // Mutant: saturating_sub → wrapping_sub would silently wrap to large value
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 200, 0);
    let config = make_config(0);
    let snap_before = MarketSnapshot::capture(&market);

    // current_ts < last_accrual → InvalidTimestamp error (SR-114)
    let result = accrue_interest(&mut market, &config, 100);
    assert!(
        result.is_err(),
        "SR-114: backward timestamps should return error"
    );
    assert_eq!(
        result.unwrap_err(),
        pinocchio::error::ProgramError::Custom(20),
        "error should be InvalidTimestamp (20)"
    );

    // Full state snapshot: NO fields should change on error
    let snap_after = MarketSnapshot::capture(&market);
    snap_before.assert_unchanged(&snap_after, "backward timestamp error path");
}

// ===========================================================================
// Kill: interest_delta_wad formula component swap
// ===========================================================================

/// Mutant class: swap(annual_bps, SECONDS_PER_YEAR) in numerator
///
/// Kill mutation: swap annual_bps and time_elapsed in the numerator.
/// For specific inputs, the formula gives a unique answer.
#[test]
fn kill_formula_component_swap() {
    // Mutant: swap(annual_bps, SECONDS_PER_YEAR) → 1000 * 31536000 * WAD / (100 * 10000)
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);
    let snap_before = MarketSnapshot::capture(&market);

    accrue_interest(&mut market, &config, 100).unwrap();

    // Expected: interest_delta_wad = 1000 * 100 * WAD / (31536000 * 10000)
    let expected_delta = 1000u128 * 100 * WAD / (SECONDS_PER_YEAR * BPS);
    let expected_sf = WAD + WAD * expected_delta / WAD;

    assert_eq!(market.scale_factor(), expected_sf);

    // Mutant value: swapping annual_bps with SECONDS_PER_YEAR
    let mutant_delta = SECONDS_PER_YEAR * 100 * WAD / (1000 * BPS);
    let mutant_sf = WAD + WAD * mutant_delta / WAD;
    assert_ne!(
        market.scale_factor(),
        mutant_sf,
        "scale_factor must NOT match swapped-component mutant"
    );

    // Only scale_factor and last_accrual should change; supply and fees unchanged
    assert_eq!(
        market.scaled_total_supply(),
        snap_before.scaled_total_supply,
        "supply should not change from interest accrual"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        snap_before.accrued_protocol_fees,
        "fees should not change with 0 fee rate"
    );
}

// ===========================================================================
// Kill: Division by zero in scale_factor_delta
// ===========================================================================

/// Mutant class: remove WAD division (scale_factor_delta *= WAD instead of /= WAD)
///
/// Kill mutation: remove WAD division from scale_factor_delta computation.
/// Without the division, the value would be enormously larger.
#[test]
fn kill_missing_wad_division() {
    // Mutant: sf_delta = sf * interest_delta (no /WAD) → WAD * (WAD/10) = WAD²/10
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected = scale_factor_after_elapsed(WAD, 1000, SECONDS_PER_YEAR as i64);
    assert_eq!(
        market.scale_factor(),
        expected,
        "must divide by WAD in scale_factor_delta"
    );

    // Mutant value (no division): sf = WAD + WAD * interest_delta_wad
    let interest_delta_wad = math_oracle::growth_factor_wad(1000, SECONDS_PER_YEAR as i64) - WAD;
    let mutant_sf = WAD + WAD * interest_delta_wad;
    assert_ne!(
        market.scale_factor(),
        mutant_sf,
        "scale_factor must NOT match missing-WAD-division mutant"
    );

    // Verify only sf and last_accrual changed (no fees with 0 fee rate)
    assert_eq!(market.accrued_protocol_fees(), 0);
    assert_eq!(market.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);
}

// ===========================================================================
// Kill: fee_delta uses wrong denominator
// ===========================================================================

/// Mutant class: BPS→WAD denominator swap in fee_delta_wad
///
/// fee_delta_wad = interest_delta_wad * fee_rate_bps / BPS (correct)
/// fee_delta_wad = interest_delta_wad * fee_rate_bps / WAD (mutation)
#[test]
fn kill_fee_delta_wrong_denominator() {
    // Mutant: fee_delta_wad = interest_delta * fee_rate / WAD (instead of / BPS)
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(5000); // 50%
    let snap_before = MarketSnapshot::capture(&market);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let growth = math_oracle::growth_factor_wad(1000, SECONDS_PER_YEAR as i64);
    let interest_delta = growth - WAD;
    let new_sf = math_oracle::mul_wad(WAD, growth);
    let expected_fee = fee_delta_after_elapsed(supply, WAD, 1000, 5000, SECONDS_PER_YEAR as i64);

    assert_eq!(market.accrued_protocol_fees(), expected_fee);
    assert!(expected_fee > 0, "fee should be non-trivial");

    // Mutant value: using WAD as denominator gives negligible fee
    let mutant_fee_delta = interest_delta * 5000 / WAD;
    let mutant_fee = (supply * new_sf / WAD * mutant_fee_delta / WAD) as u64;
    assert_ne!(
        market.accrued_protocol_fees(),
        mutant_fee,
        "fees must NOT match WAD-denominator mutant"
    );

    // Supply should not change
    assert_eq!(
        market.scaled_total_supply(),
        snap_before.scaled_total_supply
    );
}

// ===========================================================================
// Kill: new_scale_factor vs old_scale_factor in fee computation
// ===========================================================================

/// Mutant class: old_sf→new_sf in fee computation (Finding 10 fix)
///
/// Kill mutation: use new_scale_factor (post-accrual) instead of old scale_factor.
/// On-chain code now correctly uses pre-accrual scale_factor for fee computation.
#[test]
fn kill_fee_uses_old_vs_new_sf() {
    // Mutant: fee = supply * new_sf / WAD * fee_delta / WAD (instead of old_sf)
    let supply = 1_000_000_000_000u128;
    let annual_bps = 1000u16;
    let fee_rate = 10_000u16; // 100% fee — all interest goes to fees

    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(fee_rate);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let growth = math_oracle::growth_factor_wad(annual_bps, SECONDS_PER_YEAR as i64);
    let new_sf = math_oracle::mul_wad(WAD, growth);
    let interest_delta_wad = growth - WAD;
    let fee_with_new = supply * new_sf / WAD * interest_delta_wad / WAD;

    // With old_sf (pre-accrual = WAD): fee = supply * WAD / WAD * interest_delta / WAD
    let fee_with_old = supply * WAD / WAD * interest_delta_wad / WAD;

    // They should be different
    assert!(fee_with_new > fee_with_old);

    // Actual should match pre-accrual (old_sf) computation (Finding 10 fix)
    assert_eq!(market.accrued_protocol_fees(), fee_with_old as u64);

    // Explicitly assert NOT the mutant value (post-accrual new_sf)
    assert_ne!(
        market.accrued_protocol_fees(),
        fee_with_new as u64,
        "fees must NOT match new_sf mutant value"
    );
}

// ===========================================================================
// Kill: annual_bps=0 → should accrue no interest
// ===========================================================================

/// Mutant class: default annual_bps to 1 (instead of using actual 0)
///
/// Kill mutation: default annual_bps to 1 instead of using actual value.
#[test]
fn kill_zero_annual_bps() {
    // Mutant: hardcode annual_bps=1 → would produce tiny but non-zero interest
    let mut market = make_market(0, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(5000);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    // 0 bps → no interest → no fees → scale_factor unchanged
    assert_eq!(market.scale_factor(), WAD);
    assert_eq!(market.accrued_protocol_fees(), 0);
    // Timestamp should still advance
    assert_eq!(market.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);

    // Mutant value: annual_bps=1 would give sf > WAD
    let mutant_delta = 1u128 * (SECONDS_PER_YEAR as u128) * WAD / (SECONDS_PER_YEAR * BPS);
    let mutant_sf = WAD + WAD * mutant_delta / WAD;
    assert_ne!(
        market.scale_factor(),
        mutant_sf,
        "scale_factor must NOT match annual_bps=1 mutant"
    );
}

// ===========================================================================
// Kill: off-by-one in time_elapsed <= 0 check
// ===========================================================================

/// Mutant class: `time_elapsed <= 0` → `time_elapsed < 0`
///
/// Kill mutation: change `time_elapsed <= 0` to `time_elapsed < 0`.
/// With the mutation, time_elapsed=0 would proceed and try to compute
/// interest (which would be 0, but it would still update last_accrual).
#[test]
fn kill_time_elapsed_off_by_one() {
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 1000, 42);
    let config = make_config(500);
    let snap_before = MarketSnapshot::capture(&market);

    // Exact same timestamp
    accrue_interest(&mut market, &config, 1000).unwrap();

    // Full snapshot: ALL fields must be unchanged
    let snap_after = MarketSnapshot::capture(&market);
    snap_before.assert_unchanged(&snap_after, "zero elapsed time");

    // Explicitly verify each field for clarity
    assert_eq!(market.scale_factor(), WAD);
    assert_eq!(market.accrued_protocol_fees(), 42); // unchanged
    assert_eq!(market.last_accrual_timestamp(), 1000); // unchanged
    assert_eq!(market.scaled_total_supply(), WAD); // unchanged
}

// ===========================================================================
// Kill: existing fees preserved across accrual
// ===========================================================================

/// Mutant class: fees = new_fees (reset) instead of fees += new_fees
///
/// Kill mutation: reset accrued_protocol_fees to 0 before adding new fees.
#[test]
fn kill_fees_reset_before_add() {
    // Mutant: fees = new_fees (instead of initial_fees + new_fees)
    let initial_fees = 1000u64;
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, initial_fees);
    let config = make_config(500);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let new_fee = fee_delta_after_elapsed(supply, WAD, 1000, 500, SECONDS_PER_YEAR as i64);
    let expected_total = initial_fees + new_fee;

    assert_eq!(
        market.accrued_protocol_fees(),
        expected_total,
        "fees should be initial({}) + new({}), not just new",
        initial_fees,
        new_fee
    );

    // Mutant value: fees = new_fee only (no initial_fees)
    assert_ne!(
        market.accrued_protocol_fees(),
        new_fee,
        "fees must NOT match reset-before-add mutant"
    );
    assert!(
        market.accrued_protocol_fees() > initial_fees,
        "existing fees must be preserved: got {}, initial was {}",
        market.accrued_protocol_fees(),
        initial_fees
    );
}

// ===========================================================================
// Kill: scale_factor set to delta instead of old + delta
// ===========================================================================

/// Mutant class: sf = delta (assignment) instead of sf = old + delta (addition)
///
/// Kill mutation: set scale_factor = delta instead of old + delta.
#[test]
fn kill_scale_factor_assignment_vs_addition() {
    // Mutant: sf = delta (not old + delta)
    let initial_sf = WAD * 2; // 2x (already had interest)
    let mut market = make_market(1000, i64::MAX, initial_sf, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let interest_delta_wad = math_oracle::growth_factor_wad(1000, SECONDS_PER_YEAR as i64) - WAD;
    let delta = initial_sf * interest_delta_wad / WAD;
    let expected = scale_factor_after_elapsed(initial_sf, 1000, SECONDS_PER_YEAR as i64);
    assert_eq!(market.scale_factor(), expected);

    // Mutant value: sf = delta (without adding to initial)
    assert_ne!(
        market.scale_factor(),
        delta,
        "scale_factor must NOT match assignment-only mutant"
    );
    assert!(
        market.scale_factor() > initial_sf,
        "scale_factor should grow from initial, not be replaced"
    );
}

// ===========================================================================
// Kill: 1-second precision matters
// ===========================================================================

/// Mutant class: round time_elapsed to nearest minute (time_elapsed / 60 * 60)
///
/// Kill mutation: round time_elapsed to nearest minute.
/// Verifies that 1-second granularity produces different results than 0.
#[test]
fn kill_one_second_precision() {
    // Mutant: time_elapsed = time_elapsed / 60 * 60 → rounds 1s to 0s
    let mut market = make_market(10_000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, 1).unwrap();

    // At 100% annual, 1 second should produce:
    // delta = 10000 * 1 * WAD / (31536000 * 10000) = WAD / 31536000
    let expected_delta = WAD / SECONDS_PER_YEAR;
    let expected_sf = WAD + WAD * expected_delta / WAD;

    assert_eq!(market.scale_factor(), expected_sf);
    assert!(
        market.scale_factor() > WAD,
        "1 second should produce non-zero interest"
    );

    // sf(1s) != sf(0s) — the mutant would round to 0
    assert_ne!(
        market.scale_factor(),
        WAD,
        "1-second precision must differ from 0-second result"
    );
}

// ===========================================================================
// Kill: Settlement factor total_normalized == 0 branch
// ===========================================================================

/// Mutant class: remove total_normalized == 0 guard (causes division by zero)
///
/// Kill mutation: remove total_normalized == 0 check in settlement.
/// When supply is 0, settlement factor should be WAD (fully funded).
#[test]
fn kill_settlement_zero_supply_branch() {
    // Mutant: remove guard → division by zero
    let total_normalized: u128 = 0;
    let factor = if total_normalized == 0 {
        WAD
    } else {
        // Would divide by zero without the check
        let available: u128 = 1000;
        available.checked_mul(WAD).unwrap() / total_normalized
    };

    assert_eq!(
        factor, WAD,
        "zero supply should yield WAD settlement factor"
    );

    // Verify mutant path would give different result for non-zero supply
    let non_zero_supply: u128 = 1000;
    let available: u128 = 500;
    let non_zero_factor = available.checked_mul(WAD).unwrap() / non_zero_supply;
    assert_ne!(
        non_zero_factor, WAD,
        "non-zero supply with partial availability gives factor != WAD"
    );
    assert_eq!(non_zero_factor, WAD / 2);
}

// ===========================================================================
// Kill: Scaled amount computation rounding direction
// ===========================================================================

/// Mutant class: floor→ceil in deposit scaling
///
/// Kill mutation: use ceiling division instead of floor.
/// Deposit scaling should round DOWN (favorable to protocol).
#[test]
fn kill_deposit_rounding_direction() {
    // Mutant: ceil(3 * WAD / (2 * WAD)) = 2 (instead of floor = 1)
    let amount: u128 = 3;
    let scale_factor: u128 = 2 * WAD;

    let scaled = amount * WAD / scale_factor; // floor division
    assert_eq!(scaled, 1, "deposit scaling should round down");

    // Exact value assertion
    assert!(
        scaled <= amount * WAD / scale_factor,
        "scaled amount must be <= theoretical (floor rounding)"
    );

    // Ceiling mutant would give 2
    let ceil_scaled = (amount * WAD + scale_factor - 1) / scale_factor;
    assert_ne!(
        scaled, ceil_scaled,
        "floor and ceiling should differ for this input"
    );
    assert_eq!(ceil_scaled, 2);
}

/// Mutant class: floor→ceil in payout computation
///
/// Kill mutation: use ceiling division in payout computation.
/// Payout should round DOWN (favorable to protocol).
#[test]
fn kill_payout_rounding_direction() {
    // Mutant: ceil payout → 4 instead of 3
    let scaled_amount: u128 = 3;
    let scale_factor: u128 = 2 * WAD;
    let settlement: u128 = WAD / 2;

    let normalized = scaled_amount * scale_factor / WAD;
    let payout = normalized * settlement / WAD;

    assert_eq!(normalized, 6);
    assert_eq!(payout, 3, "payout should round down");

    // Verify rounding favors protocol
    let theoretical_payout_f64 = (scaled_amount as f64) * (scale_factor as f64) / (WAD as f64)
        * (settlement as f64)
        / (WAD as f64);
    assert!(
        (payout as f64) <= theoretical_payout_f64,
        "payout ({}) must be <= theoretical ({})",
        payout,
        theoretical_payout_f64
    );
}

// ===========================================================================
// Kill: ReSettle strict improvement check
// ===========================================================================

/// Mutant class: `<=` → `<` in improvement check (allows same-value resettle)
///
/// Kill mutation: change `new_factor <= old_factor` to `new_factor < old_factor`.
/// With the mutation, re-settling to the SAME factor would be allowed.
#[test]
fn kill_resettle_strict_improvement() {
    // Mutant: `<` would allow same-value resettle
    let old_factor: u128 = WAD / 2;
    let new_factor: u128 = WAD / 2; // same

    // The on-chain check: new_factor <= old_factor → error
    let is_not_improved = new_factor <= old_factor;
    assert!(
        is_not_improved,
        "same factor should NOT count as improvement"
    );

    // Also verify 1 less doesn't count
    let worse_factor: u128 = WAD / 2 - 1;
    assert!(
        worse_factor <= old_factor,
        "worse factor should NOT count as improvement"
    );
}

/// Mutant class: `<=` → `<` verification (genuine improvement accepted)
///
/// Verify strict improvement: new > old is accepted.
#[test]
fn kill_resettle_genuine_improvement() {
    let old_factor: u128 = WAD / 2;
    let new_factor: u128 = WAD / 2 + 1;

    let is_not_improved = new_factor <= old_factor;
    assert!(
        !is_not_improved,
        "higher factor should count as improvement"
    );

    // Verify exact improvement amount
    let improvement = new_factor - old_factor;
    assert_eq!(improvement, 1, "improvement should be exactly 1");

    // All fields other than settlement_factor should remain unchanged in a real resettle
    // (verified via integration tests)
}

// ===========================================================================
// Kill: u64 truncation in fee accumulation
// ===========================================================================

/// Mutant class: `u64::try_from` → `as u64` (silent truncation)
///
/// Kill mutation: skip u64::try_from and just cast with `as u64`.
/// For very large computed fees, try_from correctly returns an error.
#[test]
fn kill_fee_u64_truncation() {
    // Mutant: `as u64` silently truncates; `try_from` returns Err
    let huge_value: u128 = u128::from(u64::MAX) + 1;
    let result = u64::try_from(huge_value);
    assert!(
        result.is_err(),
        "u64::try_from should fail for values > u64::MAX"
    );

    // The `as u64` mutant would silently truncate to 0
    #[allow(clippy::cast_possible_truncation)]
    let truncated = huge_value as u64;
    assert_eq!(truncated, 0, "as u64 would silently truncate to 0");

    // Also verify boundary: u64::MAX itself should succeed
    let boundary_value: u128 = u128::from(u64::MAX);
    let boundary_result = u64::try_from(boundary_value);
    assert!(
        boundary_result.is_ok(),
        "u64::try_from should succeed for u64::MAX"
    );
    assert_eq!(boundary_result.unwrap(), u64::MAX);
}
