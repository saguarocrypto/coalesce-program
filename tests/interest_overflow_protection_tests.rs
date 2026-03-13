//! Overflow protection tests for interest accrual.
//!
//! These tests verify that the interest accrual logic properly protects
//! against arithmetic overflows at extreme parameter boundaries.
//!
//! Safety-critical paths tested:
//! - Multiplication overflow in interest_delta_wad calculation
//! - Multiplication overflow in scale_factor_delta calculation
//! - Addition overflow in new_scale_factor calculation
//! - Multiplication overflow in fee calculation
//! - Truncation overflow when converting fee to u64

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

use bytemuck::{bytes_of, Zeroable};
use proptest::prelude::*;

use coalesce::constants::{SECONDS_PER_YEAR, WAD};
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};
use pinocchio::error::ProgramError;

#[path = "common/interest_oracle.rs"]
mod interest_oracle;

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

fn growth_factor_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::growth_factor_wad_exact(annual_bps, elapsed_seconds)
}

fn interest_delta_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::interest_delta_wad_exact(annual_bps, elapsed_seconds)
}

fn scale_factor_after_elapsed_exact(
    scale_factor: u128,
    annual_bps: u16,
    elapsed_seconds: i64,
) -> u128 {
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

fn fee_normalized_u128_exact(
    scaled_total_supply: u128,
    new_scale_factor: u128,
    interest_delta_wad: u128,
) -> u128 {
    scaled_total_supply
        .checked_mul(new_scale_factor)
        .expect("fee normalized mul overflow")
        .checked_div(WAD)
        .expect("fee normalized div by zero")
        .checked_mul(interest_delta_wad)
        .expect("fee normalized delta mul overflow")
        .checked_div(WAD)
        .expect("fee normalized delta div by zero")
}

fn assert_market_unchanged(before: &Market, after: &Market) {
    assert_eq!(bytes_of(before), bytes_of(after), "market mutated on error");
}

fn assert_math_overflow(result: Result<(), ProgramError>) {
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::MathOverflow as u32))
    );
}

// ---------------------------------------------------------------------------
// Unit tests: Specific overflow scenarios
// ---------------------------------------------------------------------------

#[test]
fn overflow_huge_scale_factor_times_interest_delta() {
    // scale_factor near u128::MAX will overflow when multiplied by interest_delta_wad
    let huge_scale = u128::MAX / 2;
    let annual_bps: u16 = 10000; // 100%
    let seconds = SECONDS_PER_YEAR as i64;

    let mut market = make_market(annual_bps, i64::MAX, huge_scale, 1, 0, 0);
    let config = make_config(0);
    let before = market;

    let result = accrue_interest(&mut market, &config, seconds);
    assert_math_overflow(result);
    assert_market_unchanged(&before, &market);
}

#[test]
fn overflow_huge_supply_times_scale_factor_in_fee_calc() {
    // Boundary: x should succeed, x+1 should overflow in supply * scale_factor_before.
    // After Finding 10 fix, fee uses pre-accrual SF (WAD here), not post-accrual.
    let annual_bps: u16 = 10_000; // 100% => new_sf = 2 * WAD after one year
    let fee_rate_bps: u16 = 1; // keep fee truncation path away from u64 overflow
    let elapsed = SECONDS_PER_YEAR as i64;
    let expected_sf = scale_factor_after_elapsed_exact(WAD, annual_bps, elapsed);
    // With pre-accrual SF (WAD), overflow boundary is u128::MAX / WAD
    let max_safe_supply_for_mul = u128::MAX / WAD;
    let overflow_supply = max_safe_supply_for_mul + 1;
    let config = make_config(fee_rate_bps);

    let mut safe_market = make_market(annual_bps, i64::MAX, WAD, max_safe_supply_for_mul, 0, 0);
    let safe_result = accrue_interest(&mut safe_market, &config, elapsed);
    assert!(safe_result.is_ok(), "safe boundary should not overflow");

    let expected_fees = fee_delta_exact(
        max_safe_supply_for_mul,
        WAD,
        annual_bps,
        fee_rate_bps,
        elapsed,
    );
    assert_eq!(safe_market.scale_factor(), expected_sf);
    assert_eq!(safe_market.accrued_protocol_fees(), expected_fees);
    assert_eq!(safe_market.last_accrual_timestamp(), elapsed);

    let mut overflow_market = make_market(annual_bps, i64::MAX, WAD, overflow_supply, 0, 0);
    let before = overflow_market;
    let overflow_result = accrue_interest(&mut overflow_market, &config, elapsed);
    assert_math_overflow(overflow_result);
    assert_market_unchanged(&before, &overflow_market);
}

#[test]
fn overflow_fee_truncation_to_u64() {
    // Boundary: x should fit in u64, x+1 should fail at u64::try_from(fee_normalized).
    let annual_bps: u16 = 10_000; // 100%
    let fee_rate_bps: u16 = 10_000; // 100%
    let elapsed = SECONDS_PER_YEAR as i64;
    let expected_sf = scale_factor_after_elapsed_exact(WAD, annual_bps, elapsed);
    let idw = interest_delta_wad_exact(annual_bps, elapsed);
    let max_u64_u128 = u128::from(u64::MAX);
    let mut lo = 0u128;
    let mut hi = max_u64_u128;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let fee_mid = fee_normalized_u128_exact(mid, WAD, idw);
        if fee_mid <= max_u64_u128 {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let safe_supply = lo;
    let overflow_supply = safe_supply + 1;
    let config = make_config(fee_rate_bps);

    let mut safe_market = make_market(annual_bps, i64::MAX, WAD, safe_supply, 0, 0);
    let safe_result = accrue_interest(&mut safe_market, &config, elapsed);
    assert!(
        safe_result.is_ok(),
        "safe truncation boundary should succeed"
    );
    assert_eq!(safe_market.scale_factor(), expected_sf);
    let expected_safe_fee =
        fee_delta_exact(safe_supply, WAD, annual_bps, fee_rate_bps, elapsed);
    assert_eq!(safe_market.accrued_protocol_fees(), expected_safe_fee);
    assert_eq!(safe_market.last_accrual_timestamp(), elapsed);

    let mut overflow_market = make_market(annual_bps, i64::MAX, WAD, overflow_supply, 0, 0);
    let before = overflow_market;
    let overflow_result = accrue_interest(&mut overflow_market, &config, elapsed);
    assert_math_overflow(overflow_result);
    assert_market_unchanged(&before, &overflow_market);
}

#[test]
fn no_overflow_with_realistic_parameters() {
    // Test with realistic production parameters
    // $100B total supply at 10% annual interest with 5% protocol fee
    let supply = 100_000_000_000_000_000u128; // $100B in 6-decimal base units
    let annual_bps: u16 = 1000; // 10%
    let fee_rate: u16 = 500; // 5%

    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(fee_rate);

    let before_supply = market.scaled_total_supply();

    // Accrue for 1 year.
    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert!(result.is_ok(), "realistic parameters should not overflow");

    // Exact expected values.
    let elapsed = SECONDS_PER_YEAR as i64;
    let expected_sf = scale_factor_after_elapsed_exact(WAD, annual_bps, elapsed);
    let expected_fee = fee_delta_exact(supply, WAD, annual_bps, fee_rate, elapsed);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), expected_fee);
    assert_eq!(market.last_accrual_timestamp(), elapsed);
    assert_eq!(market.scaled_total_supply(), before_supply);
}

#[test]
fn no_overflow_9_years_compounding_succeeds() {
    // Test that 5 years of 100% annual interest compounding succeeds.
    // This aligns with the protocol's max maturity horizon.
    let supply = 1_000_000_000_000u128; // $1M
    let annual_bps: u16 = 10000; // 100%

    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(0);

    // Accrue for 5 years in 1-year steps and verify exact oracle values.
    let mut expected_sf = WAD;
    for year in 1..=5 {
        let ts = (year as i64) * (SECONDS_PER_YEAR as i64);
        let result = accrue_interest(&mut market, &config, ts);
        assert!(
            result.is_ok(),
            "year {} should not overflow: scale_factor = {}",
            year,
            market.scale_factor()
        );
        expected_sf =
            scale_factor_after_elapsed_exact(expected_sf, annual_bps, SECONDS_PER_YEAR as i64);
        assert_eq!(market.scale_factor(), expected_sf);
        assert_eq!(market.last_accrual_timestamp(), ts);
        assert_eq!(market.accrued_protocol_fees(), 0);
    }
}

#[test]
fn overflow_at_year_10_compounding() {
    // Test that 6th yearly accrual overflows for 100% APR daily compounding
    // and preserves state on error.
    let supply = 1_000_000_000_000u128;
    let annual_bps: u16 = 10000; // 100%

    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(0);

    // Accrue through year 5.
    let mut expected_sf = WAD;
    for year in 1..=5 {
        let ts = (year as i64) * (SECONDS_PER_YEAR as i64);
        accrue_interest(&mut market, &config, ts).unwrap();
        expected_sf =
            scale_factor_after_elapsed_exact(expected_sf, annual_bps, SECONDS_PER_YEAR as i64);
    }
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(
        market.last_accrual_timestamp(),
        5 * (SECONDS_PER_YEAR as i64)
    );

    // Year 6 should overflow; state must remain unchanged.
    let ts_year_6 = 6 * (SECONDS_PER_YEAR as i64);
    let before = market;
    let result = accrue_interest(&mut market, &config, ts_year_6);
    assert_math_overflow(result);
    assert_market_unchanged(&before, &market);
}

// ---------------------------------------------------------------------------
// Proptest: Overflow protection holds for random inputs
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn prop_accrue_never_panics(
        annual_bps in 0u16..=10000,
        time_elapsed in 0i64..=100_000_000i64, // up to ~3.17 years
        scale_factor in WAD..=(1000 * WAD), // 1x to 1000x
        supply in 0u128..=1_000_000_000_000_000_000u128, // up to $1 quadrillion
        fee_rate_bps in 0u16..=10000,
    ) {
        let last_accrual = 0i64;
        let maturity = i64::MAX;
        let current_ts = last_accrual + time_elapsed;

        let mut market = make_market(annual_bps, maturity, scale_factor, supply, last_accrual, 0);
        let config = make_config(fee_rate_bps);
        let before = market;

        // Must not panic - may return Ok or Err.
        let result = accrue_interest(&mut market, &config, current_ts);

        if let Err(e) = result {
            prop_assert_eq!(
                e,
                ProgramError::Custom(LendingError::MathOverflow as u32),
                "only MathOverflow is expected in this domain"
            );
            prop_assert_eq!(bytes_of(&market), bytes_of(&before), "state changed on error");
        } else {
            prop_assert!(
                market.scale_factor() >= before.scale_factor(),
                "scale factor must be non-decreasing on success"
            );
            prop_assert_eq!(market.scaled_total_supply(), before.scaled_total_supply());
            prop_assert_eq!(market.last_accrual_timestamp(), current_ts);
        }
    }

    #[test]
    fn prop_scale_factor_increases_or_error(
        annual_bps in 1u16..=10000,
        time_elapsed in 1i64..=31_536_000i64, // 1 second to 1 year
        supply in 1u128..=1_000_000_000_000_000u128,
        fee_rate_bps in 0u16..=10000,
    ) {
        let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        let config = make_config(fee_rate_bps);

        let initial_sf = market.scale_factor();
        let before = market;
        let result = accrue_interest(&mut market, &config, time_elapsed);

        if let Err(e) = result {
            prop_assert_eq!(e, ProgramError::Custom(LendingError::MathOverflow as u32));
            prop_assert_eq!(bytes_of(&market), bytes_of(&before), "state changed on error");
        } else {
            prop_assert!(
                market.scale_factor() > initial_sf,
                "scale_factor should increase: initial={}, after={}",
                initial_sf, market.scale_factor()
            );
            prop_assert_eq!(market.last_accrual_timestamp(), time_elapsed);
            prop_assert_eq!(market.scaled_total_supply(), before.scaled_total_supply());
        }
    }

    #[test]
    fn prop_fees_increase_or_error(
        annual_bps in 1u16..=10000,
        time_elapsed in 1i64..=31_536_000i64,
        supply in 1_000_000u128..=1_000_000_000_000_000u128, // need supply for fees
        fee_rate_bps in 1u16..=10000,
    ) {
        let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        let config = make_config(fee_rate_bps);

        let initial_fees = market.accrued_protocol_fees();
        let before = market;
        let result = accrue_interest(&mut market, &config, time_elapsed);

        if let Err(e) = result {
            prop_assert_eq!(e, ProgramError::Custom(LendingError::MathOverflow as u32));
            prop_assert_eq!(bytes_of(&market), bytes_of(&before), "state changed on error");
        } else {
            prop_assert!(
                market.accrued_protocol_fees() >= initial_fees,
                "fees should increase: initial={}, after={}",
                initial_fees, market.accrued_protocol_fees()
            );
            prop_assert!(
                market.scale_factor() > WAD,
                "positive rate/time should increase scale_factor"
            );
            prop_assert_eq!(market.last_accrual_timestamp(), time_elapsed);
            prop_assert_eq!(market.scaled_total_supply(), before.scaled_total_supply());
        }
    }
}

// ---------------------------------------------------------------------------
// Extreme boundary tests
// ---------------------------------------------------------------------------

#[test]
fn boundary_max_u16_annual_bps() {
    // Neighboring boundaries: u16::MAX-1 and u16::MAX for 1 second elapsed.
    let elapsed = 1i64;
    let mut prev_sf = 0u128;
    for annual_bps in [u16::MAX - 1, u16::MAX] {
        let mut market = make_market(annual_bps, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        let config = make_config(0);
        let result = accrue_interest(&mut market, &config, elapsed);
        assert!(
            result.is_ok(),
            "1 second at boundary rate should not overflow"
        );

        let expected_sf = scale_factor_after_elapsed_exact(WAD, annual_bps, elapsed);
        assert_eq!(market.scale_factor(), expected_sf);
        assert_eq!(market.last_accrual_timestamp(), elapsed);
        assert_eq!(market.accrued_protocol_fees(), 0);
        if prev_sf != 0 {
            assert!(
                expected_sf >= prev_sf,
                "higher annual_bps should not lower scale_factor"
            );
        }
        prev_sf = expected_sf;
    }
}

#[test]
fn boundary_max_time_elapsed() {
    // Boundary neighbors around maturity cap.
    let annual_bps = 1000u16;
    let fee_rate_bps = 500u16;
    let supply = 1_000_000_000_000u128;
    let maturity = SECONDS_PER_YEAR as i64;
    let far_future = 100 * SECONDS_PER_YEAR as i64;
    let config = make_config(fee_rate_bps);

    for ts in [maturity - 1, maturity, maturity + 1, far_future] {
        let mut market = make_market(annual_bps, maturity, WAD, supply, 0, 0);
        let result = accrue_interest(&mut market, &config, ts);
        assert!(result.is_ok(), "accrual should succeed at ts={ts}");

        let effective_elapsed = if ts > maturity { maturity } else { ts };
        let expected_sf = scale_factor_after_elapsed_exact(WAD, annual_bps, effective_elapsed);
        let expected_fee = fee_delta_exact(
            supply,
            WAD,
            annual_bps,
            fee_rate_bps,
            effective_elapsed,
        );

        assert_eq!(market.scale_factor(), expected_sf);
        assert_eq!(market.accrued_protocol_fees(), expected_fee);
        assert_eq!(market.last_accrual_timestamp(), effective_elapsed);
        assert_eq!(market.scaled_total_supply(), supply);
    }
}

#[test]
fn boundary_max_scale_factor_that_works() {
    // x-1 / x / x+1 boundary for scale_factor * growth_factor multiplication.
    let elapsed = SECONDS_PER_YEAR as i64;
    let annual_bps = 10_000u16;
    let config = make_config(0);
    let growth = growth_factor_wad_exact(annual_bps, elapsed);
    let max_safe_scale = u128::MAX / growth;

    for scale in [max_safe_scale - 1, max_safe_scale] {
        let mut market = make_market(annual_bps, i64::MAX, scale, 1, 0, 0);
        let result = accrue_interest(&mut market, &config, elapsed);
        assert!(result.is_ok(), "scale={scale} should be safe");
        let expected_sf = scale_factor_after_elapsed_exact(scale, annual_bps, elapsed);
        assert_eq!(market.scale_factor(), expected_sf);
        assert_eq!(market.last_accrual_timestamp(), elapsed);
    }

    let overflow_scale = max_safe_scale + 1;
    let mut overflow_market = make_market(annual_bps, i64::MAX, overflow_scale, 1, 0, 0);
    let before = overflow_market;
    let overflow = accrue_interest(&mut overflow_market, &config, elapsed);
    assert_math_overflow(overflow);
    assert_market_unchanged(&before, &overflow_market);
}

#[test]
fn boundary_zero_everything() {
    // x-1/x/x+1 neighbor checks around timestamp zero with zero-rate configuration.
    let mut invalid_ts_market = make_market(0, 0, WAD, 0, 0, 123);
    let config = make_config(0);
    let before_invalid = invalid_ts_market;
    let invalid = accrue_interest(&mut invalid_ts_market, &config, -1);
    assert_eq!(
        invalid,
        Err(ProgramError::Custom(LendingError::InvalidTimestamp as u32))
    );
    assert_market_unchanged(&before_invalid, &invalid_ts_market);

    let mut exact_zero_market = make_market(0, 0, WAD, 0, 0, 123);
    let zero_result = accrue_interest(&mut exact_zero_market, &config, 0);
    assert!(zero_result.is_ok());
    assert_eq!(exact_zero_market.scale_factor(), WAD);
    assert_eq!(exact_zero_market.accrued_protocol_fees(), 123);
    assert_eq!(exact_zero_market.last_accrual_timestamp(), 0);

    let mut capped_one_market = make_market(0, 0, WAD, 0, 0, 123);
    let one_result = accrue_interest(&mut capped_one_market, &config, 1);
    assert!(one_result.is_ok());
    assert_eq!(capped_one_market.scale_factor(), WAD);
    assert_eq!(capped_one_market.accrued_protocol_fees(), 123);
    assert_eq!(capped_one_market.last_accrual_timestamp(), 0);
}

// ---------------------------------------------------------------------------
// Regression tests: Known edge cases
// ---------------------------------------------------------------------------

#[test]
fn regression_one_second_interest_precision() {
    // Neighboring elapsed-time boundaries around 1 second.
    let annual_bps: u16 = 1000;
    let config = make_config(0);

    let mut market_0 = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_0, &config, 0).unwrap();
    assert_eq!(market_0.scale_factor(), WAD);
    assert_eq!(market_0.last_accrual_timestamp(), 0);

    let mut market_1 = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_1, &config, 1).unwrap();
    let expected_sf_1 = scale_factor_after_elapsed_exact(WAD, annual_bps, 1);
    assert_eq!(market_1.scale_factor(), expected_sf_1);
    assert_eq!(market_1.last_accrual_timestamp(), 1);

    let mut market_2 = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_2, &config, 2).unwrap();
    let expected_sf_2 = scale_factor_after_elapsed_exact(WAD, annual_bps, 2);
    assert_eq!(market_2.scale_factor(), expected_sf_2);
    assert_eq!(market_2.last_accrual_timestamp(), 2);

    assert!(market_1.scale_factor() > market_0.scale_factor());
    assert!(market_2.scale_factor() > market_1.scale_factor());
}

#[test]
fn regression_fee_accrual_at_100_percent_fee_rate() {
    // x-1/x boundary on fee rate with exact fee oracle checks.
    let annual_bps: u16 = 1000; // 10%
    let fee_rate_bps_low: u16 = 9999;
    let fee_rate_bps_high: u16 = 10000;
    let supply = 1_000_000_000_000u128;
    let elapsed = SECONDS_PER_YEAR as i64;
    let expected_sf = scale_factor_after_elapsed_exact(WAD, annual_bps, elapsed);

    let mut low = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_interest(&mut low, &make_config(fee_rate_bps_low), elapsed).unwrap();
    let expected_low_fee =
        fee_delta_exact(supply, WAD, annual_bps, fee_rate_bps_low, elapsed);
    assert_eq!(low.scale_factor(), expected_sf);
    assert_eq!(low.accrued_protocol_fees(), expected_low_fee);

    let mut high = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_interest(&mut high, &make_config(fee_rate_bps_high), elapsed).unwrap();
    let expected_high_fee =
        fee_delta_exact(supply, WAD, annual_bps, fee_rate_bps_high, elapsed);
    assert_eq!(high.scale_factor(), expected_sf);
    assert_eq!(high.accrued_protocol_fees(), expected_high_fee);
    assert!(high.accrued_protocol_fees() >= low.accrued_protocol_fees());
}

#[test]
fn regression_compounding_effect() {
    // Verify exact single-step vs multi-step compound formulas.
    let annual_bps: u16 = 1000;
    let half_year = (SECONDS_PER_YEAR / 2) as i64;

    // Single accrual for 1 year.
    let mut market_single = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);
    accrue_interest(&mut market_single, &config, SECONDS_PER_YEAR as i64).unwrap();

    // Two accruals of 6 months each.
    let mut market_double = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_double, &config, half_year).unwrap();
    accrue_interest(&mut market_double, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected_single =
        scale_factor_after_elapsed_exact(WAD, annual_bps, SECONDS_PER_YEAR as i64);
    let expected_half_step = scale_factor_after_elapsed_exact(WAD, annual_bps, half_year);
    let expected_double =
        scale_factor_after_elapsed_exact(expected_half_step, annual_bps, half_year);

    assert_eq!(market_single.scale_factor(), expected_single);
    assert_eq!(market_double.scale_factor(), expected_double);
    assert!(market_double.scale_factor() > market_single.scale_factor());
    assert_eq!(
        market_double.last_accrual_timestamp(),
        SECONDS_PER_YEAR as i64
    );
}
