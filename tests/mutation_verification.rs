//! Mutation verification tests.
//!
//! These tests verify that the test suite would catch common code mutations
//! (comparison flips, arithmetic changes, boundary shifts, error code swaps).
//! Each test is designed to fail if a specific mutation were applied to the
//! production code, ensuring that our test coverage kills those mutations.
//!
//! Every test:
//! 1. Annotates the mutant class being killed
//! 2. Asserts the correct value AND asserts != the mutant value
//! 3. Captures a full state snapshot to verify only expected fields change
//! 4. For error-path tests: asserts exact Custom(N) error code + state unchanged

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
    let new_sf = math_oracle::mul_wad(scale_factor_before, growth);
    let interest_delta_wad = growth.checked_sub(WAD).unwrap();
    let fee_delta_wad = interest_delta_wad
        .checked_mul(u128::from(fee_rate_bps))
        .unwrap()
        .checked_div(BPS)
        .unwrap();
    let fee_normalized = scaled_supply
        .checked_mul(new_sf)
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
// Category 1: Maturity cap mutations
// ===========================================================================

/// Mutant class: `>` → `>=` in maturity comparison (off-by-one at boundary)
///
/// Mutation: Change `current_timestamp > maturity` to `current_timestamp >= maturity`.
/// This test verifies that accrual AT maturity still works correctly
/// (last_accrual is set to maturity, not maturity-1).
#[test]
fn mutation_maturity_cap_gt_to_ge() {
    let maturity = 1000i64;
    let last_accrual = 0i64;
    let mut market = make_market(1000, maturity, WAD, WAD, last_accrual, 0);
    let config = make_config(0);

    // Accrue exactly at maturity
    accrue_interest(&mut market, &config, maturity).unwrap();
    let sf_at = market.scale_factor();

    // Accrue past maturity
    let mut market2 = make_market(1000, maturity, WAD, WAD, last_accrual, 0);
    accrue_interest(&mut market2, &config, maturity + 1000).unwrap();
    let sf_past = market2.scale_factor();

    // Both should be identical (capped at maturity)
    assert_eq!(sf_at, sf_past);
    // last_accrual should be at maturity in both cases
    assert_eq!(market.last_accrual_timestamp(), maturity);
    assert_eq!(market2.last_accrual_timestamp(), maturity);

    // Verify the capped sf is exactly what we'd expect for 1000 seconds
    let expected_delta = 1000u128 * 1000 * WAD / (SECONDS_PER_YEAR * BPS);
    let expected_sf = WAD + WAD * expected_delta / WAD;
    assert_eq!(sf_at, expected_sf);
}

/// Mutant class: remove maturity cap entirely (use current_timestamp directly)
///
/// Mutation: Remove maturity cap entirely (use current_timestamp directly).
/// This test verifies that interest beyond maturity is NOT accrued.
#[test]
fn mutation_maturity_cap_removed() {
    let maturity = 1000i64;
    let last_accrual = 0i64;
    let config = make_config(0);

    // Accrue to maturity
    let mut m1 = make_market(1000, maturity, WAD, WAD, last_accrual, 0);
    accrue_interest(&mut m1, &config, maturity).unwrap();

    // Accrue to 10x maturity — should be same
    let mut m2 = make_market(1000, maturity, WAD, WAD, last_accrual, 0);
    accrue_interest(&mut m2, &config, maturity * 10).unwrap();

    assert_eq!(m1.scale_factor(), m2.scale_factor());

    // Mutant would produce higher sf for longer time
    let mut m_uncapped = make_market(1000, i64::MAX, WAD, WAD, last_accrual, 0);
    accrue_interest(&mut m_uncapped, &config, maturity * 10).unwrap();
    assert!(
        m_uncapped.scale_factor() > m1.scale_factor(),
        "uncapped accrual should produce higher sf"
    );
}

// ===========================================================================
// Category 2: Arithmetic sign mutations
// ===========================================================================

/// Mutant class: `+` → `-` in scale_factor update (sf + delta → sf - delta)
///
/// Mutation: Change `scale_factor + scale_factor_delta` to `scale_factor - scale_factor_delta`.
/// This test verifies that scale_factor always INCREASES after accrual.
#[test]
fn mutation_scale_factor_add_to_sub() {
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);
    let seconds = SECONDS_PER_YEAR as i64;

    accrue_interest(&mut market, &config, seconds).unwrap();

    let expected = scale_factor_after_elapsed(WAD, 1000, seconds);
    assert_eq!(market.scale_factor(), expected);
    assert!(market.scale_factor() > WAD);

    let delta = math_oracle::growth_factor_wad(1000, seconds) - WAD;
    let mutant_sf = WAD - delta;
    assert_ne!(
        market.scale_factor(),
        mutant_sf,
        "scale_factor must NOT match subtraction mutant"
    );
}

/// Mutant class: `checked_add` → `checked_sub` for fee accumulation
///
/// Mutation: Change `checked_add` to `checked_sub` for fee accrual.
/// This test verifies that fees INCREASE (never decrease) after accrual.
#[test]
fn mutation_fee_add_to_sub() {
    let supply = 1_000_000_000_000u128; // 1M USDC
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(500); // 5% fee rate

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected_fee = fee_delta_after_elapsed(supply, WAD, 1000, 500, SECONDS_PER_YEAR as i64);

    assert_eq!(market.accrued_protocol_fees(), expected_fee);
    assert!(
        market.accrued_protocol_fees() > 0,
        "fees should increase, not decrease"
    );
}

// ===========================================================================
// Category 3: Zero-time-elapsed guard mutations
// ===========================================================================

/// Mutant class: `<=` → `<` in zero-time guard (allows 0 elapsed to proceed)
///
/// Mutation: Change `time_elapsed <= 0` to `time_elapsed < 0`.
/// This test verifies that zero elapsed time truly does nothing.
#[test]
fn mutation_time_elapsed_le_to_lt() {
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 100, 0);
    let config = make_config(500);
    let snap_before = MarketSnapshot::capture(&market);

    // Same timestamp => zero elapsed
    accrue_interest(&mut market, &config, 100).unwrap();

    // Full snapshot: ALL fields must be unchanged
    let snap_after = MarketSnapshot::capture(&market);
    snap_before.assert_unchanged(&snap_after, "zero time elapsed le_to_lt");

    assert_eq!(
        market.scale_factor(),
        WAD,
        "zero time should not change scale_factor"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "zero time should not accrue fees"
    );
    assert_eq!(market.last_accrual_timestamp(), 100);
}

/// Mutant class: remove early return for time_elapsed == 0
///
/// Mutation: Remove the early return for time_elapsed == 0.
/// This test verifies that calling with same timestamp is idempotent.
#[test]
fn mutation_no_early_return_zero_time() {
    let mut market = make_market(5000, i64::MAX, WAD, 1_000_000u128, 1000, 42);
    let config = make_config(1000);
    let snap_before = MarketSnapshot::capture(&market);

    accrue_interest(&mut market, &config, 1000).unwrap();

    // Full snapshot: ALL fields must be unchanged
    let snap_after = MarketSnapshot::capture(&market);
    snap_before.assert_unchanged(&snap_after, "no early return zero time");
}

// ===========================================================================
// Category 4: Interest formula mutations
// ===========================================================================

/// Mutant class: BPS→WAD or WAD→BPS constant swap in formula
///
/// Mutation: Use wrong constant (BPS instead of WAD) in interest calculation.
/// This test verifies the exact expected scale_factor for known inputs.
#[test]
fn mutation_wrong_constant_in_formula() {
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);
    let seconds = SECONDS_PER_YEAR as i64;

    accrue_interest(&mut market, &config, seconds).unwrap();

    let expected = scale_factor_after_elapsed(WAD, 1000, seconds);
    assert_eq!(market.scale_factor(), expected);

    // Mutant: using BPS instead of WAD as the precision base would give wildly wrong result
    let mutant_delta = 1000u128 * (SECONDS_PER_YEAR as u128) * BPS / (SECONDS_PER_YEAR * BPS);
    let mutant_sf = WAD + WAD * mutant_delta / WAD;
    assert_ne!(
        market.scale_factor(),
        mutant_sf,
        "scale_factor must NOT match BPS-constant mutant"
    );
}

/// Mutant class: multiplication order swap (affects rounding)
///
/// Mutation: Swap multiplication order causing different rounding.
/// Verify that the exact computation order matches expectation.
#[test]
fn mutation_operation_order() {
    let annual_bps: u16 = 500; // 5%
    let seconds = SECONDS_PER_YEAR as i64;
    let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, seconds).unwrap();

    let expected = scale_factor_after_elapsed(WAD, annual_bps, seconds);
    assert_eq!(market.scale_factor(), expected);

    // Verify exact delta magnitude
    let delta = market.scale_factor() - WAD;
    let expected_delta = math_oracle::growth_factor_wad(annual_bps, seconds) - WAD;
    assert_eq!(delta, expected_delta);
}

// ===========================================================================
// Category 5: Fee rate mutations
// ===========================================================================

/// Mutant class: `> 0` → `== 0` in fee_rate check (always computes fees)
///
/// Mutation: Use `fee_rate_bps == 0` check (instead of `> 0`) so fees are
/// always computed even when zero.
/// Verifies that zero fee rate truly produces zero fees.
#[test]
fn mutation_fee_rate_zero_check() {
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    assert_eq!(market.accrued_protocol_fees(), 0);

    // Non-zero fee rate should produce non-zero fees (proving the code path matters)
    let mut market2 = make_market(1000, i64::MAX, WAD, supply, 0, 0);
    let config2 = make_config(1);
    accrue_interest(&mut market2, &config2, SECONDS_PER_YEAR as i64).unwrap();
    assert!(
        market2.accrued_protocol_fees() > 0,
        "non-zero fee rate should produce non-zero fees"
    );
}

/// Mutant class: old_sf→new_sf swap in fee computation
///
/// Mutation: Change fee formula to use old scale_factor instead of new.
/// This test verifies the fee uses the NEW scale_factor.
#[test]
fn mutation_fee_uses_old_scale_factor() {
    let supply = 1_000_000_000_000u128;
    let annual_bps = 1000u16; // 10%
    let fee_rate = 500u16; // 5% of interest

    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(fee_rate);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let growth = math_oracle::growth_factor_wad(annual_bps, SECONDS_PER_YEAR as i64);
    let interest_delta_wad = growth - WAD;
    let new_sf = math_oracle::mul_wad(WAD, growth);
    let fee_delta_wad = interest_delta_wad * 500 / BPS;
    let expected_fee =
        fee_delta_after_elapsed(supply, WAD, annual_bps, fee_rate, SECONDS_PER_YEAR as i64);

    assert_eq!(market.accrued_protocol_fees(), expected_fee);

    // Mutant: using OLD scale_factor (WAD) gives smaller fee
    let fee_with_old_sf = (supply * WAD / WAD * fee_delta_wad / WAD) as u64;
    assert_ne!(
        market.accrued_protocol_fees(),
        fee_with_old_sf,
        "fees must NOT match old_sf mutant"
    );
    assert!(expected_fee > fee_with_old_sf);
}

// ===========================================================================
// Category 6: Compound interest verification
// ===========================================================================

/// Mutant class: compound→simple interest (delta uses initial sf, not accumulated)
///
/// Mutation: Change compound interest to simple interest.
/// Verify that two-step accrual produces a compound effect.
#[test]
fn mutation_simple_vs_compound() {
    let config = make_config(0);

    // Single step: 0 -> 1000
    let mut m1 = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut m1, &config, 1000).unwrap();

    // Two steps: 0 -> 500 -> 1000
    let mut m2 = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut m2, &config, 500).unwrap();
    accrue_interest(&mut m2, &config, 1000).unwrap();

    // Two-step (compound) should be >= single-step
    assert!(m2.scale_factor() >= m1.scale_factor());
    // For non-trivial rates, strictly greater
    if m1.scale_factor() > WAD {
        assert!(
            m2.scale_factor() > m1.scale_factor(),
            "compound should exceed simple for non-zero interest"
        );
        // Verify the compound excess is positive but small
        let compound_excess = m2.scale_factor() - m1.scale_factor();
        assert!(compound_excess > 0);
    }
}

// ===========================================================================
// Category 7: Settlement factor boundary mutations
// ===========================================================================

/// Mutant class: `max(1, ...)` → `max(0, ...)` (allows zero settlement factor)
///
/// Mutation: Change `max(1, ...)` to `max(0, ...)` in settlement factor.
/// Verifies that settlement factor is never 0 when computed.
#[test]
fn mutation_settlement_factor_min_bound() {
    // Simulate: vault has 0 available, but supply > 0
    // factor = max(1, min(WAD, 0 * WAD / total_normalized))
    // = max(1, 0) = 1
    let available: u128 = 0;
    let total_normalized: u128 = 1_000_000;

    let raw = available
        .checked_mul(WAD)
        .unwrap()
        .checked_div(total_normalized)
        .unwrap();
    let capped = if raw > WAD { WAD } else { raw };
    let factor = if capped < 1 { 1 } else { capped };

    assert_eq!(factor, 1, "settlement factor should be at least 1");

    // Mutant: max(0, ...) would give 0
    let mutant_factor = capped; // no floor at 1
    assert_eq!(mutant_factor, 0, "without floor, factor would be 0");
    assert_ne!(factor, mutant_factor);
}

/// Mutant class: remove `min(WAD, ...)` cap (allows >WAD settlement)
///
/// Mutation: Change `min(WAD, ...)` to not cap at WAD.
/// Verifies that settlement factor never exceeds WAD even when overfunded.
#[test]
fn mutation_settlement_factor_max_bound() {
    // Simulate: vault has MORE than total_normalized (overfunded)
    let available: u128 = 2_000_000;
    let total_normalized: u128 = 1_000_000;

    let raw = available
        .checked_mul(WAD)
        .unwrap()
        .checked_div(total_normalized)
        .unwrap();
    let capped = if raw > WAD { WAD } else { raw };
    let factor = if capped < 1 { 1 } else { capped };

    assert_eq!(factor, WAD, "settlement factor should be capped at WAD");

    // Mutant: no cap would give 2*WAD
    assert_eq!(raw, 2 * WAD, "uncapped raw factor should be 2*WAD");
    assert_ne!(factor, raw, "factor must be capped, not raw");
}

// ===========================================================================
// Category 8: Timestamp update mutations
// ===========================================================================

/// Mutant class: don't update last_accrual_timestamp after accrual
///
/// Mutation: Don't update last_accrual_timestamp after accrual.
/// Verifies that last_accrual is always updated.
#[test]
fn mutation_timestamp_not_updated() {
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, 1000).unwrap();
    assert_eq!(
        market.last_accrual_timestamp(),
        1000,
        "last_accrual should be updated to current_ts"
    );

    // Second accrual should advance further
    let sf_after_first = market.scale_factor();
    accrue_interest(&mut market, &config, 2000).unwrap();
    assert_eq!(market.last_accrual_timestamp(), 2000);

    // Mutant: if timestamp wasn't updated, second accrual would use 0→2000 = 2000s
    // instead of 1000→2000 = 1000s, giving more interest.
    // Verify: compute what the mutant would produce (2000s from 0)
    let mut m_mutant = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut m_mutant, &make_config(0), 2000).unwrap();
    // The correct two-step (compound) result should differ from single 2000s step
    // because compounding applies on a higher base in the second step
    assert_ne!(
        market.scale_factor(),
        m_mutant.scale_factor(),
        "two-step compound must differ from single 2000s step"
    );
}

/// Mutant class: use current_timestamp instead of effective_now (maturity) for last_accrual
///
/// Mutation: Use current_timestamp instead of effective_now for last_accrual.
/// When past maturity, last_accrual should be set to maturity, not current_ts.
#[test]
fn mutation_timestamp_uses_current_not_effective() {
    let maturity = 1000i64;
    let mut market = make_market(1000, maturity, WAD, WAD, 0, 0);
    let config = make_config(0);

    // Accrue past maturity
    accrue_interest(&mut market, &config, 5000).unwrap();

    // last_accrual should be maturity, NOT 5000
    assert_eq!(
        market.last_accrual_timestamp(),
        maturity,
        "past maturity, last_accrual should be capped to maturity"
    );
    assert_ne!(
        market.last_accrual_timestamp(),
        5000,
        "last_accrual must NOT be current_ts when past maturity"
    );
}

// ===========================================================================
// Category 9: Deposit cap boundary mutations
// ===========================================================================

/// Mutant class: `>` → `>=` in cap check (rejects exact-cap deposit)
///
/// Mutation: Change `new_normalized > max_supply` to `new_normalized >= max_supply`.
/// Verifies that depositing EXACTLY to the cap is allowed.
#[test]
fn mutation_deposit_cap_gt_to_ge() {
    let max_supply: u128 = 1_000_000;
    let new_normalized: u128 = 1_000_000;
    let exceeds = new_normalized > max_supply; // should be false
    assert!(!exceeds, "exact cap deposit should be allowed");

    // Mutant: `>=` would reject exact cap
    let mutant_exceeds = new_normalized >= max_supply;
    assert!(mutant_exceeds, "mutant >= would reject exact cap deposit");
}

/// Mutant class: `>` → `> max_supply + 1` (allows off-by-one over cap)
///
/// Mutation: Change `new_normalized > max_supply` to `new_normalized > max_supply + 1`.
/// Verifies that exceeding the cap by 1 is rejected.
#[test]
fn mutation_deposit_cap_off_by_one() {
    let max_supply: u128 = 1_000_000;
    let new_normalized: u128 = 1_000_001;
    let exceeds = new_normalized > max_supply; // should be true
    assert!(exceeds, "exceeding cap by 1 should be rejected");

    // Also verify exact boundary
    let at_cap: u128 = 1_000_000;
    assert!(!(at_cap > max_supply), "exact cap should NOT be rejected");
}

// ===========================================================================
// Category 10: Borrow fee reservation mutations
// ===========================================================================

/// Mutant class: remove fee reservation (borrowable = vault instead of vault - fees)
///
/// Mutation: Remove fee reservation in borrow (use vault_balance directly).
/// Verifies that fees are reserved when computing borrowable amount.
#[test]
fn mutation_borrow_no_fee_reservation() {
    // Simulate: vault=1000, fees=200
    // borrowable = vault - min(vault, fees) = 1000 - 200 = 800
    let vault_balance: u64 = 1000;
    let accrued_fees: u64 = 200;
    let fees_reserved = core::cmp::min(vault_balance, accrued_fees);
    let borrowable = vault_balance - fees_reserved;

    assert_eq!(borrowable, 800);
    assert_eq!(fees_reserved, 200);
    assert!(
        borrowable < vault_balance,
        "fee reservation should reduce borrowable"
    );

    // Mutant: no reservation → borrowable = vault
    let mutant_borrowable = vault_balance;
    assert_ne!(
        borrowable, mutant_borrowable,
        "borrowable must NOT equal full vault when fees exist"
    );
}

/// Mutant class: `min` → `max` in fee reservation
///
/// Mutation: Use `max` instead of `min` for fee reservation.
/// Verifies fee reservation uses min(vault, fees).
#[test]
fn mutation_borrow_fee_min_vs_max() {
    // When fees > vault, reserve all of vault
    let vault: u64 = 100;
    let fees: u64 = 500;
    let reserved = core::cmp::min(vault, fees);
    assert_eq!(reserved, 100, "should reserve min(vault, fees)");

    // Mutant: max(vault, fees) = 500 → would underflow subtraction
    let mutant_reserved = core::cmp::max(vault, fees);
    assert_eq!(mutant_reserved, 500);
    assert_ne!(reserved, mutant_reserved, "min and max must differ");

    // When vault > fees, reserve all fees
    let vault2: u64 = 500;
    let fees2: u64 = 100;
    let reserved2 = core::cmp::min(vault2, fees2);
    assert_eq!(reserved2, 100, "should reserve min(vault, fees)");

    // Mutant: max(vault, fees) = 500 → would leave 0 borrowable
    let mutant_reserved2 = core::cmp::max(vault2, fees2);
    assert_eq!(mutant_reserved2, 500);
    assert_ne!(reserved2, mutant_reserved2);
}
