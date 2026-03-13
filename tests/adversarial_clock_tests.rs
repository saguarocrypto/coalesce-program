//! Adversarial clock manipulation tests for the CoalesceFi lending protocol.
//!
//! These tests verify that `accrue_interest` is resilient against:
//!   - Clock jumps backwards (saturating_sub produces 0, no damage)
//!   - Clock jumps forward dramatically (interest capped at maturity, no overflow)
//!   - Zero time_elapsed edge cases (idempotent, no double-charging)
//!   - 1-second granularity precision
//!   - Maturity boundary precision
//!   - Adversarial timestamp sequences (oscillating, staircase, random)
//!   - Last accrual timestamp postconditions
//!   - Negative / unusual timestamps

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
use pinocchio::error::ProgramError;

use coalesce::constants::{SECONDS_PER_YEAR, WAD};
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

#[path = "common/interest_oracle.rs"]
mod interest_oracle;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const ONE_YEAR: i64 = SECONDS_PER_YEAR as i64;

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

/// Snapshot of mutable Market fields for comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarketSnapshot {
    scale_factor: u128,
    accrued_protocol_fees: u64,
    last_accrual_timestamp: i64,
}

fn snapshot(m: &Market) -> MarketSnapshot {
    MarketSnapshot {
        scale_factor: m.scale_factor(),
        accrued_protocol_fees: m.accrued_protocol_fees(),
        last_accrual_timestamp: m.last_accrual_timestamp(),
    }
}

fn growth_factor_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::growth_factor_wad_exact(annual_bps, elapsed_seconds)
}

fn interest_delta_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::interest_delta_wad_exact(annual_bps, elapsed_seconds)
}

fn scale_factor_after_exact(scale_factor: u128, annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::scale_factor_after_exact(scale_factor, annual_bps, elapsed_seconds)
}

fn fee_delta_exact(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> u64 {
    interest_oracle::fee_delta_exact(
        scaled_total_supply,
        scale_factor_before,
        annual_bps,
        fee_rate_bps,
        elapsed_seconds,
    )
}

fn assert_invalid_timestamp(result: Result<(), ProgramError>) {
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::InvalidTimestamp as u32))
    );
}

// ===========================================================================
// 1. Clock jumps backwards (4 tests)
// ===========================================================================

/// 1a. current_ts < last_accrual_ts -- returns InvalidTimestamp error.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn clock_backwards_no_interest_accrued() {
    let last_accrual = 1_000_000i64;
    let current_ts = 500_000i64; // well before last_accrual
    let mut market = make_market(1000, i64::MAX, WAD, WAD, last_accrual, 0);
    let config = make_config(500);

    let snap_before = snapshot(&market);
    let result = accrue_interest(&mut market, &config, current_ts);

    // SR-114: Backward timestamps are now rejected with InvalidTimestamp error
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), ProgramError::Custom(20)); // InvalidTimestamp

    // State must be unchanged on error
    assert_eq!(market.scale_factor(), snap_before.scale_factor);
    assert_eq!(
        market.accrued_protocol_fees(),
        snap_before.accrued_protocol_fees
    );
    assert_eq!(market.last_accrual_timestamp(), last_accrual);
}

/// 1b. Clock goes back by 1 second -- returns InvalidTimestamp error.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn clock_backwards_by_one_second() {
    let last_accrual = 1_000_000i64;
    let current_ts = last_accrual - 1;
    let mut market = make_market(1000, i64::MAX, WAD, WAD, last_accrual, 42);
    let config = make_config(500);

    let snap_before = snapshot(&market);
    let result = accrue_interest(&mut market, &config, current_ts);

    assert_invalid_timestamp(result);

    // State must be unchanged on error
    assert_eq!(snapshot(&market), snap_before);
    assert_eq!(market.scaled_total_supply(), WAD);
}

/// 1c. Clock goes back by 1 year -- returns InvalidTimestamp error.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn clock_backwards_by_one_year() {
    let last_accrual = 2_000_000_000i64;
    let current_ts = last_accrual - ONE_YEAR;
    let mut market = make_market(5000, i64::MAX, WAD, WAD, last_accrual, 100);
    let config = make_config(1000);

    let snap_before = snapshot(&market);
    let result = accrue_interest(&mut market, &config, current_ts);

    assert_invalid_timestamp(result);

    // State must be unchanged on error
    assert_eq!(snapshot(&market), snap_before);
    assert_eq!(market.scaled_total_supply(), WAD);
}

/// 1d. Repeated backwards jumps -- returns InvalidTimestamp error each time.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn clock_backwards_repeated_jumps() {
    let last_accrual = 1_000_000i64;
    let mut market = make_market(1000, i64::MAX, WAD, WAD, last_accrual, 0);
    let config = make_config(500);

    let snap_before = snapshot(&market);

    // Apply 50 backwards jumps, each going further into the past
    // SR-114: Each should return InvalidTimestamp error
    for i in 1..=50 {
        let ts = last_accrual - i * 10_000;
        let result = accrue_interest(&mut market, &config, ts);
        assert_invalid_timestamp(result);
    }

    // State must be unchanged after all failed attempts
    assert_eq!(snapshot(&market), snap_before);
    assert_eq!(market.scaled_total_supply(), WAD);
}

// ===========================================================================
// 2. Clock jumps forward dramatically (4 tests)
// ===========================================================================

/// 2a. Clock jumps 10 years past maturity -- interest capped at maturity,
///     scale_factor does not overflow.
#[test]
fn clock_forward_10_years_past_maturity() {
    let start = 0i64;
    let maturity = ONE_YEAR; // 1-year maturity
    let far_future = maturity + 10 * ONE_YEAR; // 10 years past maturity
    let mut market = make_market(1000, maturity, WAD, WAD, start, 0);
    let config = make_config(0);
    let supply = market.scaled_total_supply();

    accrue_interest(&mut market, &config, far_future).unwrap();

    // Interest should only cover 0..maturity (1 year at 10%).
    let expected_sf = scale_factor_after_exact(WAD, 1000, maturity);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.last_accrual_timestamp(), maturity);
    assert_eq!(market.accrued_protocol_fees(), 0);
    assert_eq!(market.scaled_total_supply(), supply);
}

/// 2b. Clock jumps to i64::MAX -- no panic, interest capped at maturity.
#[test]
fn clock_forward_to_i64_max() {
    let start = 0i64;
    let maturity = ONE_YEAR;
    let mut market = make_market(1000, maturity, WAD, WAD, start, 0);
    let config = make_config(0);
    let supply = market.scaled_total_supply();

    // Must not panic
    let result = accrue_interest(&mut market, &config, i64::MAX);
    assert!(result.is_ok());

    // Interest capped at maturity
    assert_eq!(market.last_accrual_timestamp(), maturity);

    // Verify scale_factor matches exactly 1-year accrual at 10%.
    let expected_sf = scale_factor_after_exact(WAD, 1000, ONE_YEAR);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), 0);
    assert_eq!(market.scaled_total_supply(), supply);
}

/// 2c. Clock jumps from 0 to maturity in one step -- same result as reaching
///     maturity gradually.
#[test]
fn clock_jump_to_maturity_equals_gradual() {
    let maturity = ONE_YEAR;
    let config = make_config(500);
    let supply = 1_000_000_000_000u128;

    // Single jump to maturity
    let mut market_single = make_market(1000, maturity, WAD, supply, 0, 0);
    accrue_interest(&mut market_single, &config, maturity).unwrap();
    let expected_single_sf = scale_factor_after_exact(WAD, 1000, maturity);
    let expected_single_fees = fee_delta_exact(supply, WAD, 1000, 500, maturity);
    assert_eq!(market_single.scale_factor(), expected_single_sf);
    assert_eq!(market_single.accrued_protocol_fees(), expected_single_fees);

    // Gradual: accrue in daily steps exactly to maturity
    let mut market_gradual = make_market(1000, maturity, WAD, supply, 0, 0);
    let mut expected_gradual_sf = WAD;
    let mut expected_gradual_fees = 0u64;
    let mut expected_last = 0i64;
    let one_day = 86400i64;
    let full_days = maturity / one_day;
    for d in 1..=full_days {
        let ts = d * one_day;
        accrue_interest(&mut market_gradual, &config, ts).unwrap();
        let elapsed = ts - expected_last;
        let sf_before = expected_gradual_sf;
        expected_gradual_sf = scale_factor_after_exact(expected_gradual_sf, 1000, elapsed);
        expected_gradual_fees = expected_gradual_fees
            .checked_add(fee_delta_exact(
                supply,
                sf_before,
                1000,
                500,
                elapsed,
            ))
            .expect("expected fees overflow");
        expected_last = ts;
    }
    // Final step to exact maturity (handles remainder)
    accrue_interest(&mut market_gradual, &config, maturity).unwrap();
    let elapsed_tail = maturity - expected_last;
    let sf_before_tail = expected_gradual_sf;
    expected_gradual_sf = scale_factor_after_exact(expected_gradual_sf, 1000, elapsed_tail);
    expected_gradual_fees = expected_gradual_fees
        .checked_add(fee_delta_exact(
            supply,
            sf_before_tail,
            1000,
            500,
            elapsed_tail,
        ))
        .expect("expected fees overflow");

    assert_eq!(market_gradual.scale_factor(), expected_gradual_sf);
    assert_eq!(
        market_gradual.accrued_protocol_fees(),
        expected_gradual_fees
    );
    // Both should have last_accrual at maturity
    assert_eq!(market_single.last_accrual_timestamp(), maturity);
    assert_eq!(market_gradual.last_accrual_timestamp(), maturity);
    assert!(market_gradual.scale_factor() >= market_single.scale_factor());
}

/// 2d. Clock at exactly u32::MAX (year 2106 timestamp) -- no overflow.
#[test]
fn clock_at_u32_max_timestamp() {
    let ts_u32_max = i64::from(u32::MAX); // 4294967295
    let start = ts_u32_max - ONE_YEAR;
    let maturity = ts_u32_max + ONE_YEAR; // maturity well beyond u32::MAX
    let mut market = make_market(1000, maturity, WAD, WAD, start, 0);
    let config = make_config(0);
    let supply = market.scaled_total_supply();

    let result = accrue_interest(&mut market, &config, ts_u32_max);
    assert!(result.is_ok());

    // One year of 10% interest.
    let expected_sf = scale_factor_after_exact(WAD, 1000, ONE_YEAR);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.last_accrual_timestamp(), ts_u32_max);
    assert_eq!(market.accrued_protocol_fees(), 0);
    assert_eq!(market.scaled_total_supply(), supply);
}

// ===========================================================================
// 3. Zero time_elapsed edge cases (3 tests)
// ===========================================================================

/// 3a. Deposit and withdraw in same timestamp -- scale_factor unchanged,
///     meaning payout equals deposit when scale_factor == WAD.
#[test]
fn same_timestamp_deposit_withdraw_scale_unchanged() {
    let ts = 1_000_000i64;
    let mut market = make_market(1000, i64::MAX, WAD, 0, ts, 0);
    let config = make_config(500);
    let snap = snapshot(&market);

    // Simulate deposit: accrue at same ts, then set supply
    accrue_interest(&mut market, &config, ts).unwrap();
    assert_eq!(
        snapshot(&market),
        snap,
        "same-timestamp accrue must be no-op"
    );

    // If a deposit of X USDC is made when scale_factor == WAD,
    // scaled_amount = X * WAD / WAD = X. Withdrawing at the same timestamp
    // yields payout = X * WAD / WAD = X. No loss, no gain.
    for deposit_amount in [1u128, 1_000_000u128, 1_000_001u128] {
        let scaled_amount = deposit_amount * WAD / market.scale_factor();
        let payout = scaled_amount * market.scale_factor() / WAD;
        assert_eq!(payout, deposit_amount);
    }
}

/// 3b. Multiple accruals at same timestamp -- idempotent (no double-charging).
#[test]
fn multiple_accruals_same_timestamp_idempotent() {
    let start = 1_000_000i64;
    let ts = start + 3600; // 1 hour later
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, start, 0);
    let config = make_config(500);

    // First accrual
    accrue_interest(&mut market, &config, ts).unwrap();
    let snap_after_first = snapshot(&market);
    let expected_sf = scale_factor_after_exact(WAD, 1000, ts - start);
    let expected_fees = fee_delta_exact(supply, WAD, 1000, 500, ts - start);
    assert_eq!(snap_after_first.scale_factor, expected_sf);
    assert_eq!(snap_after_first.accrued_protocol_fees, expected_fees);

    // Second and third accruals at the same timestamp
    accrue_interest(&mut market, &config, ts).unwrap();
    assert_eq!(
        snapshot(&market),
        snap_after_first,
        "second accrual at same ts must be no-op"
    );

    accrue_interest(&mut market, &config, ts).unwrap();
    assert_eq!(
        snapshot(&market),
        snap_after_first,
        "third accrual at same ts must be no-op"
    );
}

/// 3c. Borrow and repay in same timestamp -- no interest charged.
#[test]
fn borrow_and_repay_same_timestamp_no_interest() {
    let ts = 1_000_000i64;
    let mut market = make_market(1000, i64::MAX, WAD, WAD, ts, 0);
    let config = make_config(500);

    // Accrue at the same timestamp (simulates borrow then immediate repay)
    accrue_interest(&mut market, &config, ts).unwrap();

    let snap = snapshot(&market);
    assert_eq!(snap.scale_factor, WAD);
    assert_eq!(snap.accrued_protocol_fees, 0);
    assert_eq!(snap.last_accrual_timestamp, ts);
}

// ===========================================================================
// 4. Clock advances by exactly 1 second (3 tests)
// ===========================================================================

/// 4a. 86400 sequential 1-second accruals (one day) vs single 86400-second accrual.
///     Compound tolerance: multi-step >= single-step, within reasonable bound.
#[test]
fn one_second_accruals_one_day_vs_single_step() {
    let config = make_config(0);
    let annual_bps: u16 = 1000; // 10%
    let one_day: i64 = 86400;

    // Sequential: 86400 one-second steps
    let mut market_sequential = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    let mut expected_seq = WAD;
    for s in 1..=one_day {
        accrue_interest(&mut market_sequential, &config, s).unwrap();
        expected_seq = scale_factor_after_exact(expected_seq, annual_bps, 1);
    }

    // Single: one 86400-second step
    let mut market_single = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_single, &config, one_day).unwrap();
    let expected_single = scale_factor_after_exact(WAD, annual_bps, one_day);
    assert_eq!(market_sequential.scale_factor(), expected_seq);
    assert_eq!(market_single.scale_factor(), expected_single);

    let sf_sequential = market_sequential.scale_factor();
    let sf_single = market_single.scale_factor();

    // Multi-step compounds, so it should be >= single-step
    assert!(
        sf_sequential >= sf_single,
        "sequential ({}) should be >= single ({})",
        sf_sequential,
        sf_single
    );

    // The compound effect over 1 day at 10% is tiny; they should be very close.
    let diff = sf_sequential - sf_single;
    let relative_diff = diff as f64 / sf_single as f64;
    assert!(
        relative_diff < 1e-6,
        "relative compound difference {} is too large",
        relative_diff
    );
}

/// 4b. 1-second accruals around maturity boundary -- last second before maturity
///     charges interest, first second after does not.
#[test]
fn one_second_accruals_around_maturity_boundary() {
    let maturity = 1_000_000i64;
    let config = make_config(0);
    let annual_bps: u16 = 1000;

    let mut market = make_market(annual_bps, maturity, WAD, WAD, maturity - 2, 0);

    // Accrual at maturity - 1: should charge 1 second of interest
    accrue_interest(&mut market, &config, maturity - 1).unwrap();
    let sf_before = market.scale_factor();
    let expected_before = scale_factor_after_exact(WAD, annual_bps, 1);
    assert_eq!(sf_before, expected_before);

    // Accrual at maturity: should charge 1 more second of interest
    accrue_interest(&mut market, &config, maturity).unwrap();
    let sf_at = market.scale_factor();
    let expected_at = scale_factor_after_exact(expected_before, annual_bps, 1);
    assert_eq!(sf_at, expected_at);
    assert_eq!(market.last_accrual_timestamp(), maturity);

    // Accrual at maturity + 1: should NOT charge any more interest
    accrue_interest(&mut market, &config, maturity + 1).unwrap();
    let sf_after = market.scale_factor();
    assert_eq!(sf_after, sf_at, "no interest past maturity");
}

/// 4c. 1-second accrual with very high rate (100% annual) -- no precision loss.
#[test]
fn one_second_accrual_high_rate_no_precision_loss() {
    let config = make_config(0);
    let annual_bps: u16 = 10000; // 100% annual

    let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market, &config, 1).unwrap();

    let expected_delta_wad = interest_delta_wad_exact(annual_bps, 1);
    let expected_sf = scale_factor_after_exact(WAD, annual_bps, 1);
    assert_eq!(market.scale_factor(), expected_sf);

    // Verify the delta is non-zero (precision retained)
    assert!(
        expected_delta_wad > 0,
        "1-second delta at 100% annual must be positive"
    );
    assert!(market.scale_factor() > WAD, "scale_factor must increase");

    // Neighboring boundary: 9999 bps must accrue <= 10000 bps for same elapsed.
    let mut lower_rate = make_market(9999, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut lower_rate, &config, 1).unwrap();
    assert!(lower_rate.scale_factor() <= market.scale_factor());
}

// ===========================================================================
// 5. Maturity boundary precision (3 tests)
// ===========================================================================

/// 5a. current_ts == maturity - 1 -- interest accrued for the last second.
#[test]
fn maturity_boundary_one_second_before() {
    let maturity = 1_000_000i64;
    let start = maturity - 100; // 100 seconds before maturity
    let config = make_config(0);

    let mut market = make_market(1000, maturity, WAD, WAD, start, 0);
    accrue_interest(&mut market, &config, maturity - 1).unwrap();

    let expected_sf = scale_factor_after_exact(WAD, 1000, 99);
    assert_eq!(market.scale_factor(), expected_sf);
    assert!(market.scale_factor() > WAD, "interest should have accrued");
    assert_eq!(market.last_accrual_timestamp(), maturity - 1);
    assert_eq!(market.accrued_protocol_fees(), 0);
}

/// 5b. current_ts == maturity -- interest accrued up to exactly maturity.
#[test]
fn maturity_boundary_exactly_at_maturity() {
    let maturity = 1_000_000i64;
    let start = maturity - 100;
    let config = make_config(0);

    let mut market = make_market(1000, maturity, WAD, WAD, start, 0);
    accrue_interest(&mut market, &config, maturity).unwrap();

    let expected_sf = scale_factor_after_exact(WAD, 1000, 100);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.last_accrual_timestamp(), maturity);
    assert_eq!(market.accrued_protocol_fees(), 0);
}

/// 5c. current_ts == maturity + 1 -- NO additional interest beyond maturity.
#[test]
fn maturity_boundary_one_second_after() {
    let maturity = 1_000_000i64;
    let start = maturity - 100;
    let config = make_config(0);

    // Accrue to exactly maturity
    let mut market_at = make_market(1000, maturity, WAD, WAD, start, 0);
    accrue_interest(&mut market_at, &config, maturity).unwrap();
    let sf_at_maturity = market_at.scale_factor();

    // Accrue to maturity + 1
    let mut market_past = make_market(1000, maturity, WAD, WAD, start, 0);
    accrue_interest(&mut market_past, &config, maturity + 1).unwrap();
    let sf_past_maturity = market_past.scale_factor();
    let expected_sf = scale_factor_after_exact(WAD, 1000, 100);

    // Must be identical: no interest accrues beyond maturity
    assert_eq!(
        sf_past_maturity, sf_at_maturity,
        "no interest should accrue past maturity"
    );
    assert_eq!(sf_past_maturity, expected_sf);
    assert_eq!(market_past.last_accrual_timestamp(), maturity);
}

// ===========================================================================
// 6. Adversarial timestamp sequences (3 tests)
// ===========================================================================

/// 6a. Oscillating clock: forward, back, forward, back -- backward jumps
///     now return InvalidTimestamp error, forward jumps succeed.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn adversarial_oscillating_clock() {
    let config = make_config(500);
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, 0);

    // Oscillating sequence: each "forward" goes higher than the last
    // (ts, is_backward) where is_backward indicates if ts < last_accrual
    let timestamps: &[(i64, bool)] = &[
        (100, false),  // forward from 0
        (50, true),    // backward
        (200, false),  // forward from 100
        (100, true),   // backward
        (300, false),  // forward from 200
        (150, true),   // backward
        (400, false),  // forward from 300
        (200, true),   // backward
        (500, false),  // forward from 400
        (250, true),   // backward
        (600, false),  // forward from 500
        (300, true),   // backward
        (700, false),  // forward from 600
        (350, true),   // backward
        (800, false),  // forward from 700
        (400, true),   // backward
        (900, false),  // forward from 800
        (450, true),   // backward
        (1000, false), // forward from 900
    ];

    let mut prev_sf = market.scale_factor();
    let mut prev_fees = market.accrued_protocol_fees();

    for &(ts, is_backward) in timestamps {
        let result = accrue_interest(&mut market, &config, ts);

        if is_backward {
            // SR-114: Backward timestamps return InvalidTimestamp error
            assert!(result.is_err());
            assert_eq!(result.unwrap_err(), ProgramError::Custom(20));
        } else {
            result.unwrap();
        }

        let current_sf = market.scale_factor();
        let current_fees = market.accrued_protocol_fees();

        // Monotonically non-decreasing (unchanged on error, increased on success)
        assert!(
            current_sf >= prev_sf,
            "scale_factor went down: {} -> {} at ts={}",
            prev_sf,
            current_sf,
            ts
        );
        assert!(
            current_fees >= prev_fees,
            "fees went down: {} -> {} at ts={}",
            prev_fees,
            current_fees,
            ts
        );

        prev_sf = current_sf;
        prev_fees = current_fees;
    }

    // After the sequence, interest should have accrued (final ts = 1000 > start = 0)
    assert!(market.scale_factor() > WAD);
}

/// 6b. Staircase: 100, 50, 200, 150, 300 -- backward jumps return InvalidTimestamp error.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn adversarial_staircase_sequence() {
    let config = make_config(0);
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 0, 0);

    // Staircase sequence: [100, 50, 200, 150, 300]

    // After ts=100: accrual from 0..100 (100s)
    accrue_interest(&mut market, &config, 100).unwrap();
    let sf_after_100 = market.scale_factor();
    assert!(sf_after_100 > WAD);
    assert_eq!(market.last_accrual_timestamp(), 100);

    // After ts=50: backwards, returns InvalidTimestamp error
    let result = accrue_interest(&mut market, &config, 50);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), ProgramError::Custom(20)); // InvalidTimestamp
    assert_eq!(market.scale_factor(), sf_after_100);
    assert_eq!(market.last_accrual_timestamp(), 100);

    // After ts=200: accrual from 100..200 (100s more)
    accrue_interest(&mut market, &config, 200).unwrap();
    let sf_after_200 = market.scale_factor();
    assert!(sf_after_200 > sf_after_100);
    assert_eq!(market.last_accrual_timestamp(), 200);

    // After ts=150: backwards, returns InvalidTimestamp error
    let result = accrue_interest(&mut market, &config, 150);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), ProgramError::Custom(20)); // InvalidTimestamp
    assert_eq!(market.scale_factor(), sf_after_200);
    assert_eq!(market.last_accrual_timestamp(), 200);

    // After ts=300: accrual from 200..300 (100s more)
    accrue_interest(&mut market, &config, 300).unwrap();
    let sf_after_300 = market.scale_factor();
    assert!(sf_after_300 > sf_after_200);
    assert_eq!(market.last_accrual_timestamp(), 300);
}

/// 6c. proptest: random sequence of 100 timestamps -- scale_factor and fees are
///     monotonically non-decreasing.
///
///     Note: We use a maturity cap to bound total accrual time, preventing
///     MathOverflow that legitimately occurs when compounding at high rates
///     over very long periods. The key property under test is monotonicity,
///     not overflow-freedom at extreme parameters.
mod adversarial_proptest {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn random_timestamp_sequence_monotonic(
            timestamps in proptest::collection::vec(0i64..=100_000_000i64, 100),
            annual_bps in 100u16..=10000u16,
            fee_bps in 0u16..=5000u16,
        ) {
            // Maturity is set to cap accrual at ~3.17 years (100M seconds).
            // At 100% annual, this keeps the scale_factor well within u128 range
            // while still exercising adversarial ordering.
            let maturity = 100_000_000i64;
            let config = make_config(fee_bps);
            let supply = 1_000_000_000_000u128;
            let mut market = make_market(annual_bps, maturity, WAD, supply, 0, 0);

            let mut prev_sf = market.scale_factor();
            let mut prev_fees = market.accrued_protocol_fees();

            for ts in &timestamps {
                let result = accrue_interest(&mut market, &config, *ts);
                // If MathOverflow occurs (e.g. fee truncation to u64), the state
                // must not have been modified, so monotonicity still holds.
                if result.is_err() {
                    // State unchanged on error -- verify monotonicity preserved
                    let current_sf = market.scale_factor();
                    let current_fees = market.accrued_protocol_fees();
                    prop_assert!(current_sf >= prev_sf);
                    prop_assert!(current_fees >= prev_fees);
                    continue;
                }
                let current_sf = market.scale_factor();
                let current_fees = market.accrued_protocol_fees();

                prop_assert!(
                    current_sf >= prev_sf,
                    "scale_factor not monotonic: {} -> {} at ts={}",
                    prev_sf, current_sf, ts
                );
                prop_assert!(
                    current_fees >= prev_fees,
                    "fees not monotonic: {} -> {} at ts={}",
                    prev_fees, current_fees, ts
                );

                prev_sf = current_sf;
                prev_fees = current_fees;
            }
        }
    }
}

// ===========================================================================
// 7. Last accrual timestamp postcondition (3 tests)
// ===========================================================================

/// 7a. After accrual with current_ts < maturity, last_accrual == current_ts.
#[test]
fn last_accrual_postcondition_before_maturity() {
    let maturity = 2_000_000i64;
    let config = make_config(0);
    let mut market = make_market(1000, maturity, WAD, WAD, 0, 0);

    let test_timestamps: &[i64] = &[1, 100, 1_000, 100_000, 1_999_999];
    for &ts in test_timestamps {
        market.set_last_accrual_timestamp(0);
        market.set_scale_factor(WAD);
        market.set_accrued_protocol_fees(0);
        accrue_interest(&mut market, &config, ts).unwrap();
        let expected_sf = scale_factor_after_exact(WAD, 1000, ts);
        assert_eq!(market.scale_factor(), expected_sf);
        assert_eq!(
            market.last_accrual_timestamp(),
            ts,
            "last_accrual should be current_ts={} when before maturity={}",
            ts,
            maturity
        );

        // Idempotence boundary at same timestamp.
        let snap = snapshot(&market);
        accrue_interest(&mut market, &config, ts).unwrap();
        assert_eq!(snapshot(&market), snap);
    }
}

/// 7b. After accrual with current_ts > maturity, last_accrual == maturity.
#[test]
fn last_accrual_postcondition_after_maturity() {
    let maturity = 1_000_000i64;
    let config = make_config(0);
    let mut market = make_market(1000, maturity, WAD, WAD, 0, 0);

    let test_timestamps: &[i64] = &[
        maturity + 1,
        maturity + 1000,
        maturity + ONE_YEAR,
        maturity + 10 * ONE_YEAR,
    ];

    for &ts in test_timestamps {
        market.set_last_accrual_timestamp(0);
        market.set_scale_factor(WAD);
        market.set_accrued_protocol_fees(0);
        accrue_interest(&mut market, &config, ts).unwrap();
        let expected_sf = scale_factor_after_exact(WAD, 1000, maturity);
        assert_eq!(market.scale_factor(), expected_sf);
        assert_eq!(
            market.last_accrual_timestamp(),
            maturity,
            "last_accrual should be maturity={} when current_ts={} is past maturity",
            maturity,
            ts
        );

        // Further future calls should be no-op once maturity is reached.
        let snap = snapshot(&market);
        accrue_interest(&mut market, &config, ts + ONE_YEAR).unwrap();
        assert_eq!(snapshot(&market), snap);
    }
}

/// 7c. After accrual with current_ts == maturity, last_accrual == maturity.
#[test]
fn last_accrual_postcondition_at_maturity() {
    let maturity = 1_000_000i64;
    let config = make_config(0);
    let mut market = make_market(1000, maturity, WAD, WAD, 0, 0);

    accrue_interest(&mut market, &config, maturity).unwrap();
    let expected_sf = scale_factor_after_exact(WAD, 1000, maturity);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(
        market.last_accrual_timestamp(),
        maturity,
        "last_accrual should equal maturity when current_ts == maturity"
    );

    // Boundary neighbor: once at maturity, maturity+1 should be a no-op.
    let snap = snapshot(&market);
    accrue_interest(&mut market, &config, maturity + 1).unwrap();
    assert_eq!(snapshot(&market), snap);
}

// ===========================================================================
// 8. Negative / unusual timestamps (2 tests)
// ===========================================================================

/// 8a. Timestamp of 0 (Unix epoch) -- handles gracefully.
#[test]
fn timestamp_zero_unix_epoch() {
    // Scenario: last_accrual = 0, current_ts = 0 => time_elapsed = 0 => no-op
    let mut market_same = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(500);
    accrue_interest(&mut market_same, &config, 0).unwrap();
    assert_eq!(market_same.scale_factor(), WAD);
    assert_eq!(market_same.accrued_protocol_fees(), 0);

    // Scenario: last_accrual = 0, current_ts = 1 => 1 second of interest
    let mut market_one = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_one, &config, 1).unwrap();
    assert!(
        market_one.scale_factor() > WAD,
        "1 second from epoch should accrue interest"
    );

    // Scenario: maturity = 0, current_ts = 0 => effective_now = 0, time_elapsed = 0
    let mut market_maturity_zero = make_market(1000, 0, WAD, WAD, 0, 0);
    accrue_interest(&mut market_maturity_zero, &config, 0).unwrap();
    assert_eq!(market_maturity_zero.scale_factor(), WAD);

    // Scenario: maturity = 0, current_ts = 1000 => effective_now = 0, time_elapsed = 0
    let mut market_expired = make_market(1000, 0, WAD, WAD, 0, 0);
    accrue_interest(&mut market_expired, &config, 1000).unwrap();
    assert_eq!(
        market_expired.scale_factor(),
        WAD,
        "maturity=0 should prevent all accrual"
    );
}

/// 8b. Very large negative timestamp difference returns InvalidTimestamp error.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn timestamp_large_negative_difference() {
    // last_accrual near i64::MAX, current_ts near i64::MIN
    // SR-114: This now returns InvalidTimestamp error instead of silently handling it.
    let last_accrual = i64::MAX / 2;
    let current_ts = 0i64;
    let mut market = make_market(1000, i64::MAX, WAD, WAD, last_accrual, 0);
    let config = make_config(500);

    let snap_before = snapshot(&market);
    let result = accrue_interest(&mut market, &config, current_ts);

    // SR-114: Backward timestamps are now rejected with InvalidTimestamp error
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), ProgramError::Custom(20)); // InvalidTimestamp

    // State must be unchanged on error
    assert_eq!(snapshot(&market), snap_before);

    // Even with negative timestamps (if the system ever allowed them)
    // last_accrual = large positive, current_ts = negative
    // effective_now = current_ts (since maturity is i64::MAX) = -1_000_000_000
    // -1_000_000_000 < 1_000_000_000 => InvalidTimestamp error
    let mut market2 = make_market(1000, i64::MAX, WAD, WAD, 1_000_000_000, 0);
    let result2 = accrue_interest(&mut market2, &config, -1_000_000_000);
    assert!(result2.is_err());
    assert_eq!(result2.unwrap_err(), ProgramError::Custom(20)); // InvalidTimestamp
    assert_eq!(market2.scale_factor(), WAD);
    assert_eq!(market2.accrued_protocol_fees(), 0);
}
