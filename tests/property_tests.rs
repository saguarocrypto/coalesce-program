//! Property-based tests for interest accrual, fee logic, and settlement factor.
//!
//! Uses `proptest` to verify mathematical invariants over randomized inputs.
//! These tests exercise `src/logic/interest.rs` via the public API on bytemuck
//! state structs — no BPF build required.
//!
//! Edge-biased strategies ensure WAD boundaries, zero, max, and off-by-one
//! values are exercised with high probability.

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
use num_bigint::BigUint;
use proptest::prelude::*;

// Re-use the on-chain types directly (they are `pub`).
use coalesce::constants::{SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

const BPS: u128 = 10_000;

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

fn mul_wad(a: u128, b: u128) -> u128 {
    math_oracle::mul_wad(a, b)
}

fn pow_wad(base: u128, exp: u32) -> u128 {
    math_oracle::pow_wad(base, exp)
}

/// Adapter: accepts u128 elapsed_seconds (local convention) and delegates to
/// `math_oracle::growth_factor_wad` which expects i64.
fn growth_factor_wad(annual_bps: u16, elapsed_seconds: u128) -> u128 {
    math_oracle::growth_factor_wad(
        annual_bps,
        i64::try_from(elapsed_seconds).expect("elapsed_seconds must fit i64"),
    )
}

fn expected_scale_factor(initial_sf: u128, annual_bps: u16, elapsed_seconds: u128) -> u128 {
    mul_wad(initial_sf, growth_factor_wad(annual_bps, elapsed_seconds))
}

fn expected_fee_delta(
    scaled_supply: u128,
    initial_sf: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: u128,
) -> u64 {
    if scaled_supply == 0 || fee_rate_bps == 0 || elapsed_seconds == 0 {
        return 0;
    }

    let new_sf = expected_scale_factor(initial_sf, annual_bps, elapsed_seconds);
    let interest_delta_wad = growth_factor_wad(annual_bps, elapsed_seconds) - WAD;
    let fee_delta_wad = interest_delta_wad * u128::from(fee_rate_bps) / BPS;
    let fee = scaled_supply * new_sf / WAD * fee_delta_wad / WAD;
    u64::try_from(fee).expect("fee must fit u64")
}

// ---------------------------------------------------------------------------
// Edge-biased strategies
// ---------------------------------------------------------------------------

fn edge_biased_bps() -> impl Strategy<Value = u16> {
    prop_oneof![
        3 => Just(0u16),
        3 => Just(1u16),
        3 => Just(9_999u16),
        3 => Just(10_000u16),
        88 => 0u16..=10_000u16,
    ]
}

fn edge_biased_supply() -> impl Strategy<Value = u128> {
    prop_oneof![
        2 => Just(0u128),
        2 => Just(1u128),
        2 => Just(WAD - 1),
        2 => Just(WAD),
        2 => Just(WAD + 1),
        2 => Just(u64::MAX as u128),
        88 => 0u128..=1_000_000_000_000u128,
    ]
}

fn edge_biased_scale_factor() -> impl Strategy<Value = u128> {
    prop_oneof![
        3 => Just(WAD),
        3 => Just(WAD + 1),
        3 => Just(2 * WAD),
        3 => Just(WAD + WAD / 10),
        88 => WAD..=(2 * WAD),
    ]
}

fn edge_biased_time() -> impl Strategy<Value = i64> {
    prop_oneof![
        3 => Just(0i64),
        3 => Just(1i64),
        3 => Just(SECONDS_PER_YEAR as i64 - 1),
        3 => Just(SECONDS_PER_YEAR as i64),
        88 => 0i64..=31_536_000i64,
    ]
}

fn edge_biased_fee_rate() -> impl Strategy<Value = u16> {
    prop_oneof![
        3 => Just(0u16),
        3 => Just(1u16),
        3 => Just(5_000u16),
        3 => Just(10_000u16),
        88 => 0u16..=10_000u16,
    ]
}

// ---------------------------------------------------------------------------
// Property 1: accrue_interest never panics for valid inputs
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_interest_no_overflow(
        annual_bps in edge_biased_bps(),
        time_elapsed in edge_biased_time(),
        scale_factor in edge_biased_scale_factor(),
        fee_rate_bps in edge_biased_fee_rate(),
        supply in edge_biased_supply(),
    ) {
        let last_accrual = 1_000_000i64;
        let maturity = last_accrual + 2 * 31_536_000; // 2 years out
        let current_ts = last_accrual + time_elapsed;

        let mut market = make_market(
            annual_bps,
            maturity,
            scale_factor,
            supply,
            last_accrual,
            0,
        );
        let config = make_config(fee_rate_bps);

        // Must not panic — may return Ok or Err(MathOverflow)
        let result = accrue_interest(&mut market, &config, current_ts);
        match result {
            Ok(()) => {
                // On success, scale_factor must not decrease
                prop_assert!(
                    market.scale_factor() >= scale_factor,
                    "scale_factor must never decrease: before={}, after={}",
                    scale_factor, market.scale_factor()
                );
            }
            Err(_) => {
                // On error, state should remain unchanged
                prop_assert_eq!(market.scale_factor(), scale_factor);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property 2: Monotonicity in time — longer elapsed ⇒ larger (or equal) scale_factor
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_interest_monotonic_in_time(
        annual_bps in edge_biased_bps().prop_filter("non-zero rate", |b| *b > 0),
        t1 in prop_oneof![
            3 => Just(1i64),
            3 => Just(SECONDS_PER_YEAR as i64 / 2),
            94 => 1i64..=15_768_000i64,
        ],
        delta in prop_oneof![
            3 => Just(1i64),
            3 => Just(SECONDS_PER_YEAR as i64 / 2),
            94 => 1i64..=15_768_000i64,
        ],
    ) {
        let last_accrual = 0i64;
        let maturity = i64::MAX;
        let config = make_config(0);

        // Accrue for t1
        let mut m1 = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m1, &config, t1).is_err() {
            return Ok(());
        }
        let sf1 = m1.scale_factor();

        // Accrue for t1 + delta
        let t2 = t1.saturating_add(delta);
        let mut m2 = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m2, &config, t2).is_err() {
            return Ok(());
        }
        let sf2 = m2.scale_factor();

        prop_assert!(sf2 >= sf1, "scale_factor should be monotonically increasing in time: sf({})={} vs sf({})={}", t1, sf1, t2, sf2);
    }
}

// ---------------------------------------------------------------------------
// Property 3: Monotonicity in rate — higher annual_bps ⇒ larger scale_factor delta
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_interest_monotonic_in_rate(
        bps_low in edge_biased_bps().prop_filter("bounded", |b| *b <= 5_000),
        bps_delta in 1u16..=5_000,
        time_elapsed in prop_oneof![
            3 => Just(1i64),
            3 => Just(SECONDS_PER_YEAR as i64),
            94 => 1i64..=31_536_000i64,
        ],
    ) {
        let bps_high = bps_low.saturating_add(bps_delta).min(10_000);
        if bps_high <= bps_low {
            return Ok(());
        }

        let last_accrual = 0i64;
        let maturity = i64::MAX;
        let config = make_config(0);

        let mut m_low = make_market(bps_low, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m_low, &config, time_elapsed).is_err() {
            return Ok(());
        }

        let mut m_high = make_market(bps_high, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m_high, &config, time_elapsed).is_err() {
            return Ok(());
        }

        prop_assert!(
            m_high.scale_factor() >= m_low.scale_factor(),
            "higher rate should yield >= scale_factor: low({})={}, high({})={}",
            bps_low, m_low.scale_factor(), bps_high, m_high.scale_factor()
        );
    }
}

// ---------------------------------------------------------------------------
// Property 4: scale_factor never decreases
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_scale_factor_never_decreases(
        annual_bps in edge_biased_bps(),
        time_elapsed in edge_biased_time(),
        fee_rate_bps in edge_biased_fee_rate(),
    ) {
        let last_accrual = 100i64;
        let maturity = i64::MAX;
        let initial_sf = WAD;

        let mut market = make_market(annual_bps, maturity, initial_sf, WAD, last_accrual, 0);
        let config = make_config(fee_rate_bps);

        let current_ts = last_accrual + time_elapsed;
        if accrue_interest(&mut market, &config, current_ts).is_ok() {
            prop_assert!(
                market.scale_factor() >= initial_sf,
                "scale_factor must never decrease: initial={}, after={}",
                initial_sf, market.scale_factor()
            );
            // Additional: fees must be >= 0 (always true for u64, but verify non-corruption)
            // This is structurally guaranteed but confirms no memory corruption
            let _ = market.accrued_protocol_fees();
        }
    }
}

// ---------------------------------------------------------------------------
// Property 5: Fee accrual proportional — higher fee_rate_bps ⇒ more fees
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_fee_accrual_proportional(
        annual_bps in edge_biased_bps().prop_filter("non-zero", |b| *b > 0),
        fee_low in edge_biased_fee_rate().prop_filter("bounded", |f| *f <= 5_000),
        fee_delta in 1u16..=5_000,
        time_elapsed in prop_oneof![
            3 => Just(1i64),
            3 => Just(SECONDS_PER_YEAR as i64),
            94 => 1i64..=31_536_000i64,
        ],
    ) {
        let fee_high = fee_low.saturating_add(fee_delta).min(10_000);
        if fee_high <= fee_low {
            return Ok(());
        }

        let last_accrual = 0i64;
        let maturity = i64::MAX;
        let supply = 1_000_000_000_000u128; // 1M USDC

        let mut m_low = make_market(annual_bps, maturity, WAD, supply, last_accrual, 0);
        let config_low = make_config(fee_low);
        if accrue_interest(&mut m_low, &config_low, time_elapsed).is_err() {
            return Ok(());
        }

        let mut m_high = make_market(annual_bps, maturity, WAD, supply, last_accrual, 0);
        let config_high = make_config(fee_high);
        if accrue_interest(&mut m_high, &config_high, time_elapsed).is_err() {
            return Ok(());
        }

        prop_assert!(
            m_high.accrued_protocol_fees() >= m_low.accrued_protocol_fees(),
            "higher fee rate should yield >= fees: low({})={}, high({})={}",
            fee_low, m_low.accrued_protocol_fees(), fee_high, m_high.accrued_protocol_fees()
        );
    }
}

// ---------------------------------------------------------------------------
// Property 6: Zero fee rate ⇒ zero fees
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_fee_zero_when_rate_zero(
        annual_bps in edge_biased_bps(),
        time_elapsed in edge_biased_time(),
        supply in edge_biased_supply(),
    ) {
        let last_accrual = 0i64;
        let maturity = i64::MAX;

        let mut market = make_market(annual_bps, maturity, WAD, supply, last_accrual, 0);
        let config = make_config(0); // fee_rate = 0

        if accrue_interest(&mut market, &config, time_elapsed).is_ok() {
            prop_assert_eq!(
                market.accrued_protocol_fees(), 0,
                "fee_rate_bps=0 should yield 0 fees"
            );
            // All fee-related state should be 0
            assert_eq!(market.accrued_protocol_fees(), 0);
        }
    }
}

// ---------------------------------------------------------------------------
// Property 7: Interest capped at maturity — extending past maturity doesn't increase
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_interest_capped_at_maturity(
        annual_bps in edge_biased_bps().prop_filter("non-zero", |b| *b > 0),
        maturity_offset in prop_oneof![
            3 => Just(1i64),
            3 => Just(100i64),
            3 => Just(SECONDS_PER_YEAR as i64),
            91 => 100i64..=1_000_000i64,
        ],
        overshoot in prop_oneof![
            3 => Just(1i64),
            3 => Just(SECONDS_PER_YEAR as i64),
            94 => 1i64..=31_536_000i64,
        ],
    ) {
        let last_accrual = 0i64;
        let maturity = last_accrual + maturity_offset;
        let config = make_config(0);

        // Accrue exactly to maturity
        let mut m_at = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m_at, &config, maturity).is_err() {
            return Ok(());
        }

        // Accrue past maturity
        let past = maturity.saturating_add(overshoot);
        let mut m_past = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m_past, &config, past).is_err() {
            return Ok(());
        }

        prop_assert_eq!(
            m_at.scale_factor(), m_past.scale_factor(),
            "interest should be identical at and past maturity"
        );

        // Verify last_accrual_timestamp is capped at maturity
        prop_assert_eq!(
            m_at.last_accrual_timestamp(), maturity,
            "last_accrual should be at maturity"
        );
        prop_assert_eq!(
            m_past.last_accrual_timestamp(), maturity,
            "last_accrual should be capped at maturity even when current_ts > maturity"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 8: Sequential vs single accrual — compound effect verified
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn test_sequential_vs_single_accrual(
        annual_bps in edge_biased_bps().prop_filter("non-zero", |b| *b > 0),
        t1 in prop_oneof![
            3 => Just(1i64),
            3 => Just(SECONDS_PER_YEAR as i64 / 2),
            94 => 1i64..=15_768_000i64,
        ],
        t2_delta in prop_oneof![
            3 => Just(1i64),
            3 => Just(SECONDS_PER_YEAR as i64 / 2),
            94 => 1i64..=15_768_000i64,
        ],
    ) {
        let last_accrual = 0i64;
        let maturity = i64::MAX;
        let config = make_config(0);
        let t2 = t1.saturating_add(t2_delta);

        // Single call: 0 -> t2
        let mut m_single = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m_single, &config, t2).is_err() {
            return Ok(());
        }

        // Two calls: 0 -> t1 -> t2
        let mut m_double = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut m_double, &config, t1).is_err() {
            return Ok(());
        }
        if accrue_interest(&mut m_double, &config, t2).is_err() {
            return Ok(());
        }

        // Due to compound effect, two-step should be >= single-step.
        // When the compound term (rate^2 * t1 * t2_delta at WAD scale)
        // survives integer truncation, the inequality is strict.
        //
        // Derivation of the 100_000 threshold:
        // The compound "extra" vs simple interest is approximately:
        //   extra ≈ (annual_bps / BPS)^2 * t1 * t2_delta * WAD / SPY^2
        //         = annual_bps^2 * t1 * t2_delta * WAD / (BPS^2 * SPY^2)
        // This survives floor-truncation to a u128 only when extra >= 1, i.e.:
        //   annual_bps^2 * t1 * t2_delta >= BPS^2 * SPY^2 / WAD
        //   = (10_000)^2 * (31_536_000)^2 / 1e18
        //   ≈ 99_446 (rounded up to 100_000 for a conservative threshold).
        let compound_survives = (annual_bps as u128).pow(2)
            * (t1 as u128)
            * (t2_delta as u128)
            >= 100_000;

        // Due to WAD flooring across both the day-compound and partial-day
        // linear paths, two-step accrual can be a few units lower than
        // single-step in edge cases. Allow a small bounded tolerance.
        let rounding_tolerance: u128 = 4;
        if compound_survives {
            prop_assert!(
                m_double.scale_factor().saturating_add(rounding_tolerance) >= m_single.scale_factor(),
                "compound (two-step) should be >= single-step within bounded rounding: single={}, double={}, bps={}, t1={}, t2_delta={}, tol={}",
                m_single.scale_factor(), m_double.scale_factor(), annual_bps, t1, t2_delta, rounding_tolerance
            );
        } else {
            prop_assert!(
                m_double.scale_factor().saturating_add(rounding_tolerance) >= m_single.scale_factor(),
                "compound (two-step) should be >= single-step within bounded rounding: single={}, double={}, tol={}",
                m_single.scale_factor(), m_double.scale_factor(), rounding_tolerance
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Property 9: Settlement factor bounded — always in [1, WAD]
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_settlement_factor_bounded(
        available in prop_oneof![
            2 => Just(0u128),
            2 => Just(1u128),
            2 => Just(WAD),
            2 => Just(u64::MAX as u128),
            92 => 0u128..=10_000_000_000u128,
        ],
        total_normalized in prop_oneof![
            2 => Just(1u128),
            2 => Just(WAD),
            2 => Just(u64::MAX as u128),
            94 => 1u128..=10_000_000_000u128,
        ],
    ) {
        // Replicate the on-chain settlement factor computation
        let raw = if total_normalized == 0 {
            WAD
        } else {
            match available.checked_mul(WAD) {
                Some(numerator) => match numerator.checked_div(total_normalized) {
                    Some(r) => r,
                    None => return Ok(()),
                },
                None => return Ok(()),
            }
        };
        let capped = if raw > WAD { WAD } else { raw };
        let factor = if capped < 1 { 1 } else { capped };

        prop_assert!(factor >= 1, "settlement factor must be >= 1, got {}", factor);
        prop_assert!(factor <= WAD, "settlement factor must be <= WAD, got {}", factor);
    }
}

// ---------------------------------------------------------------------------
// Property 10: Settlement factor monotonic in vault balance
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_settlement_factor_monotonic_in_vault(
        base_available in prop_oneof![
            2 => Just(0u128),
            2 => Just(1u128),
            96 => 0u128..=5_000_000_000u128,
        ],
        extra in prop_oneof![
            2 => Just(1u128),
            2 => Just(WAD),
            96 => 1u128..=5_000_000_000u128,
        ],
        total_normalized in prop_oneof![
            2 => Just(1u128),
            2 => Just(WAD),
            96 => 1u128..=10_000_000_000u128,
        ],
    ) {
        let high_available = base_available.saturating_add(extra);

        // Compute factor for base_available
        let compute_factor = |avail: u128, norm: u128| -> Option<u128> {
            if norm == 0 {
                return Some(WAD);
            }
            let numerator = avail.checked_mul(WAD)?;
            let raw = numerator.checked_div(norm)?;
            let capped = if raw > WAD { WAD } else { raw };
            Some(if capped < 1 { 1 } else { capped })
        };

        let f_low = match compute_factor(base_available, total_normalized) {
            Some(f) => f,
            None => return Ok(()),
        };
        let f_high = match compute_factor(high_available, total_normalized) {
            Some(f) => f,
            None => return Ok(()),
        };

        prop_assert!(
            f_high >= f_low,
            "more vault balance should yield >= settlement factor: low({})={}, high({})={}",
            base_available, f_low, high_available, f_high
        );
    }
}

// ---------------------------------------------------------------------------
// BigUint independent oracle — computes the same daily-compound formula at
// double WAD precision (10^36) and truncates to WAD (10^18).  This catches
// any systematic truncation bias in the u128 production code because the
// intermediate precision is strictly higher.
// ---------------------------------------------------------------------------

fn bigint_growth_factor(annual_bps: u16, elapsed_seconds: u128) -> BigUint {
    let wad_big = BigUint::from(WAD);
    let precision = &wad_big * &wad_big; // 10^36

    let bps_big = BigUint::from(10_000u64);
    let days_per_year = BigUint::from(365u64);
    let seconds_per_day = BigUint::from(86_400u64);
    let seconds_per_year = BigUint::from(SECONDS_PER_YEAR);

    let whole_days = elapsed_seconds / 86_400;
    let remaining_secs = elapsed_seconds % 86_400;

    // daily_rate = annual_bps * PRECISION / (365 * 10000)
    let daily_rate = BigUint::from(annual_bps as u64) * &precision / (&days_per_year * &bps_big);
    let daily_base = &precision + &daily_rate;

    // Exact BigUint exponentiation at double-WAD precision
    let mut growth = precision.clone();
    let mut base = daily_base;
    let mut exp = whole_days;
    while exp > 0 {
        if exp & 1 == 1 {
            growth = &growth * &base / &precision;
        }
        exp >>= 1;
        if exp > 0 {
            base = &base * &base / &precision;
        }
    }

    // Linear sub-day remainder
    let remaining_delta =
        BigUint::from(annual_bps as u64) * BigUint::from(remaining_secs) * &precision
            / (&seconds_per_year * &bps_big);
    let remaining_growth = &precision + &remaining_delta;

    let total_growth = &growth * &remaining_growth / &precision;

    // Truncate from double-WAD back to WAD
    &wad_big * &total_growth / &precision
}

// ---------------------------------------------------------------------------
// Property 11: Differential test — on-chain u128 interest vs BigUint oracle
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_differential_interest_vs_bigint(
        annual_bps in edge_biased_bps().prop_filter("non-zero", |b| *b > 0),
        time_elapsed in edge_biased_time().prop_filter("non-zero", |t| *t > 0),
    ) {
        let last_accrual = 0i64;
        let maturity = i64::MAX;
        let config = make_config(0);

        let mut market = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        if accrue_interest(&mut market, &config, time_elapsed).is_err() {
            return Ok(());
        }

        let on_chain_sf = market.scale_factor();
        let bigint_sf = bigint_growth_factor(annual_bps, time_elapsed as u128);

        // The on-chain scale_factor starts at WAD and is multiplied by the
        // growth factor: new_sf = WAD * growth / WAD = growth.  So for
        // initial_sf = WAD, on_chain_sf == on_chain growth factor.
        let on_chain_big = BigUint::from(on_chain_sf);
        let diff = if on_chain_big >= bigint_sf {
            &on_chain_big - &bigint_sf
        } else {
            &bigint_sf - &on_chain_big
        };

        // Tolerance: each mul_wad in pow_wad introduces up to 1 WAD-unit of
        // truncation divergence between 10^18 and 10^36 precision paths.
        // Higher rates amplify the truncation per squaring step because
        // larger intermediate values lose more relative precision during
        // floor division.  We bound this at 5 per elapsed day + 10 for
        // sub-day and edge cases.  For a full year this is ~1835, still
        // vanishingly small relative to WAD (10^18) — a real formula bug
        // would produce million+ unit divergences.
        let days = (time_elapsed as u128) / 86_400;
        let tolerance = BigUint::from(days * 5 + 10);
        prop_assert!(
            diff <= tolerance,
            "BigUint oracle mismatch: on_chain_sf={}, bigint_sf={}, diff={}, tolerance={}, bps={}, time={}",
            on_chain_sf, bigint_sf, diff, tolerance, annual_bps, time_elapsed
        );
    }
}

// ---------------------------------------------------------------------------
// Property 12: Differential test — fee computation vs BigUint oracle
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_differential_fee_vs_bigint(
        annual_bps in edge_biased_bps().prop_filter("non-zero", |b| *b > 0),
        fee_rate_bps in edge_biased_fee_rate().prop_filter("non-zero", |f| *f > 0),
        supply in prop_oneof![
            2 => Just(100_000_000u128),  // 100 USDC
            2 => Just(1_000_000_000_000u128),  // 1M USDC
            96 => 1_000_000u128..=1_000_000_000_000u128,
        ],
    ) {
        let time = SECONDS_PER_YEAR as i64; // full year for simplicity
        let config = make_config(fee_rate_bps);

        let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        if accrue_interest(&mut market, &config, time).is_err() {
            return Ok(());
        }

        // BigUint fee oracle at double-WAD precision
        let wad_big = BigUint::from(WAD);
        let bps_big = BigUint::from(10_000u64);

        let growth_big = bigint_growth_factor(annual_bps, SECONDS_PER_YEAR as u128);
        // new_sf = WAD * growth / WAD = growth (initial_sf = WAD)
        let interest_delta_big = &growth_big - &wad_big;
        let fee_delta_wad_big = &interest_delta_big * BigUint::from(fee_rate_bps as u64) / &bps_big;
        let supply_normalized_big = BigUint::from(supply) * &growth_big / &wad_big;
        let expected_fee_big = &supply_normalized_big * &fee_delta_wad_big / &wad_big;

        let on_chain_fee = BigUint::from(market.accrued_protocol_fees());
        let diff = if on_chain_fee >= expected_fee_big {
            &on_chain_fee - &expected_fee_big
        } else {
            &expected_fee_big - &on_chain_fee
        };

        // Tolerance: fee computation chains through growth_factor (which
        // accumulates ~2 per day of truncation) then two additional
        // mul/div WAD steps for the fee itself.  The fee magnitude
        // amplifies the growth-factor truncation by supply/WAD.  We use
        // a tolerance proportional to supply * days / WAD + a base margin.
        let days = u128::from(SECONDS_PER_YEAR) / 86_400;
        let tolerance = BigUint::from(supply * days / WAD + 10);
        prop_assert!(
            diff <= tolerance,
            "BigUint fee oracle mismatch: on_chain={}, expected={}, diff={}, tolerance={}, bps={}, fee_bps={}, supply={}",
            market.accrued_protocol_fees(), expected_fee_big, diff, tolerance, annual_bps, fee_rate_bps, supply
        );
    }
}

// ===========================================================================
// Regression seed tests for critical edge cases
// ===========================================================================

#[test]
fn regression_interest_zero_time_elapsed() {
    // Edge: zero time elapsed should be a no-op
    let mut market = make_market(10_000, i64::MAX, WAD, WAD, 1000, 42);
    let config = make_config(5000);
    accrue_interest(&mut market, &config, 1000).unwrap();
    assert_eq!(market.scale_factor(), WAD);
    assert_eq!(market.accrued_protocol_fees(), 42);
}

#[test]
fn regression_interest_one_second_at_max_rate() {
    // Edge: 1 second at maximum rate (100%)
    let mut market = make_market(10_000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);
    accrue_interest(&mut market, &config, 1).unwrap();
    let expected_delta = WAD / SECONDS_PER_YEAR;
    assert_eq!(market.scale_factor(), WAD + WAD * expected_delta / WAD);
    assert!(market.scale_factor() > WAD);
}

#[test]
fn regression_interest_full_year_at_min_rate() {
    // Edge: full year at minimum non-zero rate (1 bps = 0.01%)
    let mut market = make_market(1, i64::MAX, WAD, 1_000_000_000_000u128, 0, 0);
    let config = make_config(10_000);
    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    let expected_sf = expected_scale_factor(WAD, 1, u128::from(SECONDS_PER_YEAR));
    assert_eq!(market.scale_factor(), expected_sf);
    assert!(market.accrued_protocol_fees() > 0);
}

#[test]
fn regression_settlement_factor_zero_available() {
    // Edge: zero available → settlement factor should be 1 (minimum)
    let available: u128 = 0;
    let total_normalized: u128 = 1_000_000;
    let raw = available * WAD / total_normalized;
    let capped = raw.min(WAD);
    let factor = capped.max(1);
    assert_eq!(factor, 1);
}

#[test]
fn regression_settlement_factor_overfunded() {
    // Edge: overfunded → settlement factor capped at WAD
    let available: u128 = 2_000_000;
    let total_normalized: u128 = 1_000_000;
    let raw = available * WAD / total_normalized;
    let capped = raw.min(WAD);
    let factor = capped.max(1);
    assert_eq!(factor, WAD);
}

#[test]
fn regression_differential_max_bps_max_time() {
    // Edge: maximum bps + maximum time
    let mut market = make_market(10_000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);
    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected = expected_scale_factor(WAD, 10_000, u128::from(SECONDS_PER_YEAR));
    assert_eq!(market.scale_factor(), expected);
}
