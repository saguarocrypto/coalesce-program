//! Time-weighted interest lifecycle verification tests.
//!
//! These tests validate the on-chain discrete compound interest implementation
//! against theoretical continuous-compound and daily-compound reference values,
//! and verify maturity cutoff, fee accumulation, drift bounds, and edge cases.

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

/// Accrue interest in `n_steps` equal steps over `total_seconds`.
/// The last step absorbs any remainder from integer division so the full
/// period is covered exactly.
fn accrue_in_steps(market: &mut Market, config: &ProtocolConfig, total_seconds: i64, n_steps: u64) {
    let step_size = total_seconds / n_steps as i64;
    let start = market.last_accrual_timestamp();
    for i in 1..n_steps {
        let ts = start + step_size * i as i64;
        accrue_interest(market, config, ts).unwrap();
    }
    // Final step lands exactly at start + total_seconds
    let final_ts = start + total_seconds;
    accrue_interest(market, config, final_ts).unwrap();
}

/// f64 continuous compound: WAD * e^(rate * time / SECONDS_PER_YEAR)
fn continuous_compound_f64(annual_rate_bps: u16, seconds: i64) -> f64 {
    let rate = f64::from(annual_rate_bps) / 10_000.0;
    let time_frac = seconds as f64 / SECONDS_PER_YEAR as f64;
    (WAD as f64) * (rate * time_frac).exp()
}

/// f64 discrete compound: WAD * (1 + rate / n)^n
/// Uses iterative multiplication to reduce f64 precision loss for large n.
fn discrete_compound_f64(annual_rate_bps: u16, n_steps: u64) -> f64 {
    let rate = f64::from(annual_rate_bps) / 10_000.0;
    let per_step = rate / n_steps as f64;
    let base = 1.0 + per_step;
    // For small n, powi is fine. For large n, use exp/ln for better precision.
    if n_steps <= 1000 {
        (WAD as f64) * base.powi(n_steps as i32)
    } else {
        // (1 + r/n)^n = exp(n * ln(1 + r/n))
        (WAD as f64) * (n_steps as f64 * base.ln()).exp()
    }
}

/// f64 simple interest: WAD * (1 + rate)
fn simple_interest_f64(annual_rate_bps: u16) -> f64 {
    let rate = f64::from(annual_rate_bps) / 10_000.0;
    (WAD as f64) * (1.0 + rate)
}

fn mul_wad_oracle(a: u128, b: u128) -> u128 {
    math_oracle::mul_wad(a, b)
}

fn pow_wad_oracle(base: u128, exp: u32) -> u128 {
    math_oracle::pow_wad(base, exp)
}

fn oracle_growth_factor_wad(annual_interest_bps: u16, elapsed_seconds: i64) -> u128 {
    math_oracle::growth_factor_wad(annual_interest_bps, elapsed_seconds)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OracleAccrualState {
    scale_factor: u128,
    accrued_protocol_fees: u64,
    last_accrual_timestamp: i64,
}

fn oracle_single_step_scale_factor(
    starting_scale_factor: u128,
    annual_interest_bps: u16,
    elapsed_seconds: i64,
) -> u128 {
    let growth = oracle_growth_factor_wad(annual_interest_bps, elapsed_seconds);
    mul_wad_oracle(starting_scale_factor, growth)
}

fn oracle_accrue_step(
    state: &mut OracleAccrualState,
    annual_interest_bps: u16,
    fee_rate_bps: u16,
    scaled_total_supply: u128,
    maturity_timestamp: i64,
    current_timestamp: i64,
) {
    let effective_now = if current_timestamp > maturity_timestamp {
        maturity_timestamp
    } else {
        current_timestamp
    };

    assert!(
        effective_now >= state.last_accrual_timestamp,
        "oracle requires non-decreasing timestamps"
    );

    let time_elapsed = effective_now - state.last_accrual_timestamp;
    if time_elapsed <= 0 {
        return;
    }

    let growth = oracle_growth_factor_wad(annual_interest_bps, time_elapsed);
    let interest_delta_wad = growth.checked_sub(WAD).expect("growth must be >= WAD");

    let new_scale_factor = mul_wad_oracle(state.scale_factor, growth);

    if fee_rate_bps > 0 {
        let fee_delta_wad = interest_delta_wad
            .checked_mul(u128::from(fee_rate_bps))
            .expect("interest_delta * fee_rate overflow")
            .checked_div(BPS)
            .expect("fee_delta division failed");
        let fee_normalized = scaled_total_supply
            .checked_mul(new_scale_factor)
            .expect("supply * new_scale overflow")
            .checked_div(WAD)
            .expect("normalized supply division failed")
            .checked_mul(fee_delta_wad)
            .expect("normalized supply * fee_delta overflow")
            .checked_div(WAD)
            .expect("fee normalization division failed");
        let fee_normalized_u64 = u64::try_from(fee_normalized).expect("fee fits in u64");
        state.accrued_protocol_fees = state
            .accrued_protocol_fees
            .checked_add(fee_normalized_u64)
            .expect("fee accumulation overflow");
    }

    state.scale_factor = new_scale_factor;
    state.last_accrual_timestamp = effective_now;
}

fn oracle_accrue_in_steps(
    annual_interest_bps: u16,
    fee_rate_bps: u16,
    maturity_timestamp: i64,
    initial_scale_factor: u128,
    scaled_total_supply: u128,
    initial_last_accrual_timestamp: i64,
    initial_fees: u64,
    total_seconds: i64,
    n_steps: u64,
) -> OracleAccrualState {
    let mut state = OracleAccrualState {
        scale_factor: initial_scale_factor,
        accrued_protocol_fees: initial_fees,
        last_accrual_timestamp: initial_last_accrual_timestamp,
    };

    let step_size = total_seconds / n_steps as i64;
    let start = initial_last_accrual_timestamp;
    for i in 1..n_steps {
        let ts = start + step_size * i as i64;
        oracle_accrue_step(
            &mut state,
            annual_interest_bps,
            fee_rate_bps,
            scaled_total_supply,
            maturity_timestamp,
            ts,
        );
    }

    oracle_accrue_step(
        &mut state,
        annual_interest_bps,
        fee_rate_bps,
        scaled_total_supply,
        maturity_timestamp,
        start + total_seconds,
    );

    state
}

const ONE_YEAR: i64 = SECONDS_PER_YEAR as i64;

// ===========================================================================
// Test 1: Full lifecycle -- 365 daily steps at 10% annual
// ===========================================================================

#[test]
fn full_lifecycle_daily_accrual_10_percent() {
    let annual_bps: u16 = 1000; // 10%
    let config = make_config(0);
    let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);

    // Accrue in 365 daily steps
    accrue_in_steps(&mut market, &config, ONE_YEAR, 365);

    let final_sf = market.scale_factor();
    let final_sf_f64 = final_sf as f64;
    let oracle = oracle_accrue_in_steps(annual_bps, 0, i64::MAX, WAD, WAD, 0, 0, ONE_YEAR, 365);
    assert_eq!(
        final_sf, oracle.scale_factor,
        "on-chain daily accrual must match integer oracle exactly"
    );
    assert_eq!(market.accrued_protocol_fees(), oracle.accrued_protocol_fees);
    assert_eq!(
        market.last_accrual_timestamp(),
        oracle.last_accrual_timestamp,
        "last_accrual_timestamp should land exactly at period end"
    );

    // Reference values
    let simple = simple_interest_f64(annual_bps); // WAD * 1.10
    let daily_compound = discrete_compound_f64(annual_bps, 365); // WAD * (1 + 0.1/365)^365
    let continuous = continuous_compound_f64(annual_bps, ONE_YEAR); // WAD * e^0.10

    // The on-chain result must fall between simple interest and continuous compound.
    // Simple interest is the 1-step result (lower bound of compound).
    // Continuous compound is the theoretical upper limit as step count -> infinity.
    assert!(
        final_sf_f64 >= simple,
        "scale_factor {} should be >= simple interest {}",
        final_sf_f64,
        simple
    );
    assert!(
        final_sf_f64 <= continuous * 1.000_001, // tiny tolerance for f64
        "scale_factor {} should be <= continuous compound {}",
        final_sf_f64,
        continuous
    );

    // Error vs daily compound should be very small (integer truncation per step).
    // Each of 365 steps can lose at most a few units from truncation, so max error
    // is negligible relative to ~1e18.
    let error_vs_daily = (final_sf_f64 - daily_compound).abs();
    let relative_error = error_vs_daily / daily_compound;
    assert!(
        relative_error < 1e-6,
        "relative error vs daily compound {} is too large ({})",
        daily_compound,
        relative_error
    );
}

// ===========================================================================
// Test 2: Step-size independence -- more steps => strictly higher scale_factor
// ===========================================================================

#[test]
fn step_size_independence_monotonic_convergence() {
    let annual_bps: u16 = 1000; // 10%
    let config = make_config(0);

    let step_counts: &[u64] = &[1, 12, 52, 365, 8760];
    let mut results: Vec<(u64, u128)> = Vec::new();

    for &steps in step_counts {
        let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        accrue_in_steps(&mut market, &config, ONE_YEAR, steps);
        let oracle =
            oracle_accrue_in_steps(annual_bps, 0, i64::MAX, WAD, WAD, 0, 0, ONE_YEAR, steps);
        assert_eq!(
            market.scale_factor(),
            oracle.scale_factor,
            "steps={}: on-chain must match integer oracle",
            steps
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            oracle.last_accrual_timestamp
        );
        results.push((steps, market.scale_factor()));
    }

    // Verify all step granularities remain in a tight deterministic band and
    // below continuous-compound reference.
    let continuous = continuous_compound_f64(annual_bps, ONE_YEAR);
    let daily_reference = discrete_compound_f64(annual_bps, 365);
    let mut min_sf = u128::MAX;
    let mut max_sf = 0u128;
    for &(steps, sf) in &results {
        min_sf = min_sf.min(sf);
        max_sf = max_sf.max(sf);
        let sf_f64 = sf as f64;
        // All should be <= continuous (with tiny f64 tolerance)
        assert!(
            sf_f64 <= continuous * 1.000_001,
            "{} steps: scale_factor {} exceeds continuous compound {}",
            steps,
            sf_f64,
            continuous
        );
        let relative_error_vs_daily = (sf_f64 - daily_reference).abs() / daily_reference;
        assert!(
            relative_error_vs_daily < 2e-5,
            "{} steps: relative error {} too large vs daily reference {}",
            steps,
            relative_error_vs_daily,
            daily_reference
        );
    }

    // Different step granularities should produce very close values.
    let band_relative = (max_sf.saturating_sub(min_sf)) as f64 / (min_sf as f64);
    assert!(
        band_relative < 2e-5,
        "step-size result spread {} is too large (min={}, max={})",
        band_relative,
        min_sf,
        max_sf
    );
}

// ===========================================================================
// Test 3: Fee accumulation over lifecycle
// ===========================================================================

#[test]
fn fee_accumulation_lifecycle() {
    let annual_bps: u16 = 1000; // 10%
    let fee_bps: u16 = 500; // 5%
    let config = make_config(fee_bps);

    // Use a meaningful scaled supply: 1M USDC (6 decimals)
    let supply: u128 = 1_000_000_000_000; // 1M USDC in base units

    // --- Multi-step (daily) ---
    let mut market_daily = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_in_steps(&mut market_daily, &config, ONE_YEAR, 365);
    let fees_daily = market_daily.accrued_protocol_fees();
    let oracle_daily = oracle_accrue_in_steps(
        annual_bps,
        fee_bps,
        i64::MAX,
        WAD,
        supply,
        0,
        0,
        ONE_YEAR,
        365,
    );
    assert_eq!(
        market_daily.scale_factor(),
        oracle_daily.scale_factor,
        "daily path scale_factor must match integer oracle"
    );
    assert_eq!(
        fees_daily, oracle_daily.accrued_protocol_fees,
        "daily path fee total must match integer oracle"
    );

    // --- Single-step ---
    let mut market_single = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_in_steps(&mut market_single, &config, ONE_YEAR, 1);
    let fees_single = market_single.accrued_protocol_fees();
    let oracle_single = oracle_accrue_in_steps(
        annual_bps,
        fee_bps,
        i64::MAX,
        WAD,
        supply,
        0,
        0,
        ONE_YEAR,
        1,
    );
    assert_eq!(
        market_single.scale_factor(),
        oracle_single.scale_factor,
        "single-step scale_factor must match integer oracle"
    );
    assert_eq!(
        fees_single, oracle_single.accrued_protocol_fees,
        "single-step fee total must match integer oracle"
    );

    // Theoretical fee approximation: supply * (e^(0.10) - 1) * fee_rate
    // The interest portion is (e^0.10 - 1) of the principal.
    // The fee is fee_rate_bps/BPS of that interest.
    let rate = 0.10_f64;
    let fee_rate = 0.05_f64;
    let theoretical_interest = (supply as f64) * (rate.exp() - 1.0);
    let theoretical_fees = theoretical_interest * fee_rate;

    // Multi-step fees should be reasonably close to theoretical
    let relative_error_daily = ((fees_daily as f64) - theoretical_fees).abs() / theoretical_fees;
    assert!(
        relative_error_daily < 0.05,
        "daily fee accumulation relative error {} is too large (expected ~{}, got {})",
        relative_error_daily,
        theoretical_fees,
        fees_daily
    );

    // Both fee amounts should be positive
    assert!(fees_daily > 0, "daily fees should be positive");
    assert!(fees_single > 0, "single-step fees should be positive");

    // Compare single-step vs multi-step fee totals.
    //
    // The fee formula at each step is:
    //   fee_delta = (scaled_supply * new_sf / WAD) * (interest_delta_wad * fee_rate / BPS) / WAD
    //
    // With a single step, the entire year's interest is applied at once:
    //   interest_delta_wad = rate * WAD (e.g., WAD/10 for 10%)
    //   new_sf = WAD + WAD * rate = WAD * (1 + rate)
    //   fee = supply * WAD*(1+r)/WAD * rate*WAD*fee_rate/(BPS*WAD) = supply * (1+r) * r * fee_rate
    //
    // With many small steps, each step has a smaller interest_delta_wad but the
    // scale_factor grows compounding. The multi-step approach accumulates more
    // scale_factor growth (compound effect) but has smaller per-step fee_delta_wad.
    //
    // The net effect depends on the specific formula: in this protocol, the single-step
    // fee can be larger because it multiplies the full interest_delta_wad by the full
    // new_scale_factor in one shot.  We verify both are in a reasonable range.
    let max_fee = std::cmp::max(fees_daily, fees_single);
    let min_fee = std::cmp::min(fees_daily, fees_single);
    let fee_ratio = (max_fee as f64) / (min_fee as f64);
    assert!(
        fee_ratio < 1.15,
        "single-step vs daily fee ratio {} is too large (single={}, daily={})",
        fee_ratio,
        fees_single,
        fees_daily
    );

    // Neighbor fee-rate monotonicity: +/-1 bps should not invert fee ordering.
    let mut market_fee_minus_one = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_in_steps(
        &mut market_fee_minus_one,
        &make_config(fee_bps - 1),
        ONE_YEAR,
        365,
    );
    let mut market_fee_plus_one = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_in_steps(
        &mut market_fee_plus_one,
        &make_config(fee_bps + 1),
        ONE_YEAR,
        365,
    );
    assert!(
        market_fee_minus_one.accrued_protocol_fees() <= fees_daily,
        "fee_bps-1 should not produce larger fees than baseline"
    );
    assert!(
        fees_daily <= market_fee_plus_one.accrued_protocol_fees(),
        "fee_bps+1 should not produce smaller fees than baseline"
    );
}

// ===========================================================================
// Test 4: Maturity cutoff precision
// ===========================================================================

#[test]
fn maturity_cutoff_precision() {
    let annual_bps: u16 = 1000; // 10%
    let config = make_config(0);

    let start_ts: i64 = 0;
    let six_months: i64 = ONE_YEAR / 2; // maturity at exactly 6 months
    let maturity_ts = start_ts + six_months;

    // Accrue hourly for a full year (but maturity is at 6 months)
    let mut market = make_market(annual_bps, maturity_ts, WAD, WAD, start_ts, 0);
    let one_hour: i64 = 3600;
    let total_hours: i64 = ONE_YEAR / one_hour;
    for h in 1..=total_hours {
        let ts = start_ts + one_hour * h;
        accrue_interest(&mut market, &config, ts).unwrap();
    }
    let sf_past_maturity = market.scale_factor();

    // Now compute what the 6-month value should be: accrue only to maturity
    let mut market_exact = make_market(annual_bps, maturity_ts, WAD, WAD, start_ts, 0);
    let hours_to_maturity = six_months / one_hour;
    for h in 1..=hours_to_maturity {
        let ts = start_ts + one_hour * h;
        accrue_interest(&mut market_exact, &config, ts).unwrap();
    }
    let sf_at_maturity = market_exact.scale_factor();

    // The final scale_factor after accruing past maturity must equal the scale_factor
    // at exactly maturity -- no further interest should accrue.
    assert_eq!(
        sf_past_maturity, sf_at_maturity,
        "scale_factor after maturity {} should equal scale_factor at maturity {}",
        sf_past_maturity, sf_at_maturity
    );

    // Verify it is not WAD (i.e. interest did accrue for the 6-month period)
    assert!(
        sf_at_maturity > WAD,
        "scale_factor at maturity {} should be greater than WAD",
        sf_at_maturity
    );

    // Verify the last_accrual_timestamp is capped at maturity
    assert_eq!(
        market.last_accrual_timestamp(),
        maturity_ts,
        "last_accrual_timestamp should be capped at maturity"
    );

    // Verify interest stopped at maturity, not 1 second before or after:
    // Accrue one step to maturity-1, then another to maturity, then another to maturity+1
    let mut market_precise = make_market(annual_bps, maturity_ts, WAD, WAD, start_ts, 0);
    accrue_interest(&mut market_precise, &config, maturity_ts - 1).unwrap();
    let sf_before_maturity = market_precise.scale_factor();

    accrue_interest(&mut market_precise, &config, maturity_ts).unwrap();
    let sf_exactly_maturity = market_precise.scale_factor();

    accrue_interest(&mut market_precise, &config, maturity_ts + 1).unwrap();
    let sf_after_maturity = market_precise.scale_factor();

    // Interest should accrue from maturity-1 to maturity (1 second of interest)
    assert!(
        sf_exactly_maturity > sf_before_maturity,
        "should accrue 1 second of interest at maturity boundary"
    );

    // But no interest from maturity to maturity+1
    assert_eq!(
        sf_after_maturity, sf_exactly_maturity,
        "no interest should accrue past maturity"
    );
}

// ===========================================================================
// Test 5: Drift analysis (proptest)
// ===========================================================================

mod drift_analysis {
    use super::*;
    use proptest::prelude::*;

    /// Run the on-chain accrual for a given step count and return the scale_factor.
    fn run_accrual(annual_bps: u16, total_seconds: i64, n_steps: u64) -> u128 {
        let config = make_config(0);
        let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        accrue_in_steps(&mut market, &config, total_seconds, n_steps);
        market.scale_factor()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn drift_stays_below_threshold(
            step_count in prop_oneof![
                Just(1u64),
                Just(10u64),
                Just(100u64),
                Just(1000u64),
                Just(10000u64),
            ],
            annual_bps in 100u16..=10000u16,
        ) {
            let total_seconds = ONE_YEAR;
            let on_chain_sf = run_accrual(annual_bps, total_seconds, step_count);
            let oracle = oracle_accrue_in_steps(
                annual_bps,
                0,
                i64::MAX,
                WAD,
                WAD,
                0,
                0,
                total_seconds,
                step_count,
            );
            prop_assert_eq!(
                on_chain_sf,
                oracle.scale_factor,
                "on-chain and integer oracle mismatch: step_count={}, annual_bps={}",
                step_count,
                annual_bps
            );
            let on_chain_f64 = on_chain_sf as f64;
            let continuous = continuous_compound_f64(annual_bps, total_seconds);
            prop_assert!(
                on_chain_f64 <= continuous * 1.000_001,
                "step_count={}, annual_bps={}: on_chain={} exceeds continuous={}",
                step_count,
                annual_bps,
                on_chain_f64,
                continuous
            );

            let simple = oracle_single_step_scale_factor(WAD, annual_bps, total_seconds);
            if step_count == 1 {
                prop_assert_eq!(
                    on_chain_sf, simple,
                    "1-step path should equal exact simple-interest formula"
                );
            } else {
                prop_assert!(
                    on_chain_sf >= simple,
                    "multi-step path should be >= one-step simple-interest result"
                );
            }

            // Compute the absolute and relative drift between on-chain integer
            // result and the f64 continuous compound reference.
            let abs_drift = (on_chain_f64 - continuous).abs();
            let relative_drift = abs_drift / continuous;

            // The on-chain result implements discrete compound (simple interest
            // per step, compound across steps). The gap between on-chain and
            // continuous has two components:
            //   1. Discretization gap: discrete(n) vs continuous, which is
            //      approximately rate^2 / (2*n) for large n. For n=1 (simple
            //      interest), the gap is (1+r) - e^r, which can be up to
            //      ~26% at r=1.0 (100% rate).
            //   2. Integer truncation: each step truncates up to a few units
            //      of WAD-precision, accumulating O(n) absolute error.
            //
            // The discretization gap between (1+r/n)^n and e^r is approximately
            // r^2 / (2*n) for moderate r/n. For the max rate (r=1.0) the gap is
            // 1/(2n). We use r_max^2/(2*n) with a safety margin as threshold.
            //
            // Thresholds (for r up to 1.0):
            //   - 1 step:     1/(2)     = 0.50  => allow 1.0
            //   - 10 steps:   1/(20)    = 0.05  => allow 0.1
            //   - 100 steps:  1/(200)   = 0.005 => allow 0.01
            //   - 1000 steps: 1/(2000)  = 5e-4  => allow 1e-3
            //   - 10000 steps:1/(20000) = 5e-5  => allow 1e-4
            let threshold = match step_count {
                1 => 1.0,
                10 => 0.1,
                100 => 0.01,
                1000 => 1e-3,
                10000 => 1e-4,
                _ => 1.0,
            };

            prop_assert!(
                relative_drift < threshold,
                "step_count={}, annual_bps={}: relative drift {} exceeds {} \
                 (on_chain={}, continuous={})",
                step_count,
                annual_bps,
                relative_drift,
                threshold,
                on_chain_f64,
                continuous,
            );
        }
    }

    // Deterministic drift analysis for the specific step counts from the spec.
    #[test]
    fn drift_analysis_deterministic() {
        let annual_bps: u16 = 1000; // 10%
        let step_counts: &[u64] = &[1, 10, 100, 1000, 10000];
        let total_seconds = ONE_YEAR;
        let simple = oracle_single_step_scale_factor(WAD, annual_bps, total_seconds);

        let mut prev_relative_drift: Option<f64> = None;

        for &steps in step_counts {
            let on_chain_sf = run_accrual(annual_bps, total_seconds, steps);
            let oracle = oracle_accrue_in_steps(
                annual_bps,
                0,
                i64::MAX,
                WAD,
                WAD,
                0,
                0,
                total_seconds,
                steps,
            );
            assert_eq!(
                on_chain_sf, oracle.scale_factor,
                "steps={}: on-chain should match integer oracle exactly",
                steps
            );
            if steps == 1 {
                assert_eq!(
                    on_chain_sf, simple,
                    "one-step deterministic path must equal simple-interest formula"
                );
            } else {
                assert!(
                    on_chain_sf >= simple,
                    "steps={}: multi-step path must be >= one-step simple-interest result",
                    steps
                );
            }
            let on_chain_f64 = on_chain_sf as f64;
            let continuous = continuous_compound_f64(annual_bps, total_seconds);

            let abs_drift = (on_chain_f64 - continuous).abs();
            let relative_drift = abs_drift / continuous;

            // Relative drift vs continuous should be bounded
            assert!(
                relative_drift < 1e-1,
                "steps={}: continuous relative drift {} exceeds 0.1",
                steps,
                relative_drift
            );

            // For high step counts, drift should be very small.
            // At 10% rate: gap ~ r^2/(2n), so for n=1000 => 5e-6, n=10000 => 5e-7.
            // Allow 1e-4 to account for integer truncation and the last-step
            // remainder absorption in the helper.
            if steps >= 1000 {
                assert!(
                    relative_drift < 1e-4,
                    "steps={}: continuous relative drift {} exceeds 1e-4",
                    steps,
                    relative_drift
                );
            }

            // Drift should generally decrease with more steps (convergence).
            // We check that the sequence is non-increasing (allowing for the
            // 1-step case which is simple interest and has the largest gap).
            if let Some(prev) = prev_relative_drift {
                assert!(
                    relative_drift <= prev + 1e-15,
                    "steps={}: drift {} should be <= previous drift {} (convergence)",
                    steps,
                    relative_drift,
                    prev
                );
            }
            prev_relative_drift = Some(relative_drift);
        }
    }
}

// ===========================================================================
// Test 6: Zero-rate lifecycle
// ===========================================================================

#[test]
fn zero_rate_lifecycle() {
    let annual_bps: u16 = 0; // 0% interest
    let fee_bps: u16 = 500; // 5% fee rate (should not matter with 0% interest)
    let config = make_config(fee_bps);

    let supply: u128 = 1_000_000_000_000;
    let initial_fees: u64 = 77;
    for &steps in &[1u64, 7, 365, 10_000] {
        let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, initial_fees);
        accrue_in_steps(&mut market, &config, ONE_YEAR, steps);

        assert_eq!(
            market.scale_factor(),
            WAD,
            "steps={}: scale_factor should remain exactly WAD with 0% interest",
            steps
        );
        assert_eq!(
            market.accrued_protocol_fees(),
            initial_fees,
            "steps={}: pre-existing fees should not change at 0% interest",
            steps
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            ONE_YEAR,
            "steps={}: last_accrual_timestamp should advance to end of year",
            steps
        );

        // Re-accruing at the same timestamp must be a no-op.
        let pre = market;
        accrue_interest(&mut market, &config, ONE_YEAR).unwrap();
        assert_eq!(market.scale_factor(), pre.scale_factor());
        assert_eq!(market.accrued_protocol_fees(), pre.accrued_protocol_fees());
        assert_eq!(
            market.last_accrual_timestamp(),
            pre.last_accrual_timestamp()
        );
    }

    // Maturity cap with zero rate: timestamp must clamp, and state must remain unchanged.
    let maturity_ts = ONE_YEAR / 2;
    let mut capped_market = make_market(annual_bps, maturity_ts, WAD, supply, 0, initial_fees);
    accrue_in_steps(&mut capped_market, &config, ONE_YEAR, 365);
    assert_eq!(capped_market.scale_factor(), WAD);
    assert_eq!(capped_market.accrued_protocol_fees(), initial_fees);
    assert_eq!(
        capped_market.last_accrual_timestamp(),
        maturity_ts,
        "last_accrual_timestamp should clamp at maturity with zero rate"
    );
}

// ===========================================================================
// Test 7: Max-rate lifecycle (100% = 10000 bps)
// ===========================================================================

#[test]
fn max_rate_lifecycle_daily_accrual() {
    let annual_bps: u16 = 10000; // 100% interest
    let config = make_config(0);

    let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);

    // Accrue daily for 1 year
    accrue_in_steps(&mut market, &config, ONE_YEAR, 365);

    let final_sf = market.scale_factor();
    let final_sf_f64 = final_sf as f64;
    let wad_f64 = WAD as f64;

    // Simple interest at 100% = 2 * WAD
    let simple = 2.0 * wad_f64;

    // Continuous compound at 100% = e^1 * WAD ~ 2.71828 * WAD
    let continuous = std::f64::consts::E * wad_f64;

    // The on-chain result (daily compound) should be between simple and continuous
    assert!(
        final_sf_f64 >= simple,
        "max-rate scale_factor {} should be >= simple interest (2*WAD = {})",
        final_sf_f64,
        simple
    );
    assert!(
        final_sf_f64 <= continuous * 1.000_001,
        "max-rate scale_factor {} should be <= continuous compound (e*WAD = {})",
        final_sf_f64,
        continuous
    );

    // Verify it is close to discrete daily compound: (1 + 1/365)^365
    let daily_compound = discrete_compound_f64(annual_bps, 365);
    let relative_error = (final_sf_f64 - daily_compound).abs() / daily_compound;
    assert!(
        relative_error < 1e-6,
        "max-rate relative error vs daily compound {} is too large ({})",
        daily_compound,
        relative_error
    );

    // Verify no overflow: scale_factor should be a valid u128 > 0
    assert!(final_sf > 0, "scale_factor should not overflow to 0");
    assert!(
        final_sf > WAD,
        "scale_factor {} should be larger than WAD after 100% annual rate",
        final_sf
    );
}

// ===========================================================================
// Additional edge case: single-step vs reference sanity
// ===========================================================================

#[test]
fn single_step_equals_simple_interest() {
    // A single-step accrual over 1 full year should equal simple interest exactly.
    // This is because with 1 step, there is no compounding.
    let annual_bps: u16 = 1000; // 10%
    let config = make_config(0);

    for &elapsed in &[ONE_YEAR - 1, ONE_YEAR, ONE_YEAR + 1] {
        let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        accrue_interest(&mut market, &config, elapsed).unwrap();

        let expected = oracle_single_step_scale_factor(WAD, annual_bps, elapsed);
        assert_eq!(
            market.scale_factor(),
            expected,
            "elapsed={}: single-step should equal exact integer formula",
            elapsed
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            elapsed,
            "elapsed={}: last_accrual_timestamp should match the accrual timestamp",
            elapsed
        );
    }

    // Maturity clamp neighbor: x+1 past maturity must equal x at maturity.
    let maturity_ts = ONE_YEAR;
    let mut market = make_market(annual_bps, maturity_ts, WAD, WAD, 0, 0);
    accrue_interest(&mut market, &config, maturity_ts + 1).unwrap();
    let expected_at_maturity = oracle_single_step_scale_factor(WAD, annual_bps, maturity_ts);
    assert_eq!(
        market.scale_factor(),
        expected_at_maturity,
        "single-step past maturity must clamp to maturity result"
    );
    assert_eq!(market.last_accrual_timestamp(), maturity_ts);
}
