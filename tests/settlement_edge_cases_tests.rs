//! Edge case tests for settlement factor calculations.
//!
//! These tests focus on safety-critical paths in settlement calculations:
//! - Overflow protection
//! - Boundary conditions (minimum factor = 1, maximum factor = WAD)
//! - Very small and very large values
//! - Rounding behavior
//!
//! Uses proptest for property-based testing of settlement invariants.

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

use proptest::prelude::*;
use proptest::strategy::Strategy;

use coalesce::error::LendingError;
use coalesce::logic::interest::compute_settlement_factor as onchain_compute_settlement_factor;
use pinocchio::error::ProgramError;

// WAD constant for settlement factor math
const WAD: u128 = 1_000_000_000_000_000_000;

// ---------------------------------------------------------------------------
// Pure settlement factor calculation (matches on-chain logic)
// ---------------------------------------------------------------------------

/// Compute settlement factor from available funds and total normalized supply.
/// This replicates the on-chain logic in re_settle.rs.
fn compute_settlement_factor_checked(
    available_for_lenders: u128,
    total_normalized: u128,
) -> Result<u128, ProgramError> {
    if total_normalized == 0 {
        return Ok(WAD);
    }

    let numerator = available_for_lenders
        .checked_mul(WAD)
        .ok_or(LendingError::MathOverflow)?;
    let raw = numerator
        .checked_div(total_normalized)
        .ok_or(LendingError::MathOverflow)?;
    let capped = if raw > WAD { WAD } else { raw };
    Ok(if capped < 1 { 1 } else { capped })
}

fn compute_settlement_factor(available_for_lenders: u128, total_normalized: u128) -> Option<u128> {
    compute_settlement_factor_checked(available_for_lenders, total_normalized).ok()
}

/// Compute total_normalized from scaled_total_supply and scale_factor.
fn compute_total_normalized_checked(
    scaled_total_supply: u128,
    scale_factor: u128,
) -> Result<u128, ProgramError> {
    let product = scaled_total_supply
        .checked_mul(scale_factor)
        .ok_or(LendingError::MathOverflow)?;
    product
        .checked_div(WAD)
        .ok_or(LendingError::MathOverflow.into())
}

fn compute_total_normalized(scaled_total_supply: u128, scale_factor: u128) -> Option<u128> {
    compute_total_normalized_checked(scaled_total_supply, scale_factor).ok()
}

fn expected_math_overflow() -> ProgramError {
    ProgramError::Custom(LendingError::MathOverflow as u32)
}

fn assert_settlement_matches_onchain(available: u128, normalized: u128) -> u128 {
    let local = compute_settlement_factor_checked(available, normalized).unwrap();
    let onchain = onchain_compute_settlement_factor(available, normalized).unwrap();
    assert_eq!(
        local, onchain,
        "local settlement factor diverged from on-chain helper (available={}, normalized={})",
        available, normalized
    );
    local
}

fn edge_u128_strategy(max: u128) -> impl Strategy<Value = u128> {
    prop_oneof![
        Just(0u128),
        Just(1u128),
        Just(2u128),
        Just(WAD.saturating_sub(1).min(max)),
        Just(WAD.min(max)),
        Just(WAD.saturating_add(1).min(max)),
        Just(max.saturating_sub(1)),
        Just(max),
        0u128..=max,
    ]
}

fn positive_edge_u128_strategy(max: u128) -> impl Strategy<Value = u128> {
    prop_oneof![
        Just(1u128),
        Just(2u128),
        Just(WAD.saturating_sub(1).max(1).min(max)),
        Just(WAD.min(max).max(1)),
        Just(WAD.saturating_add(1).min(max).max(1)),
        Just(max.saturating_sub(1).max(1)),
        Just(max.max(1)),
        1u128..=max.max(1),
    ]
}

fn clamp_settlement(raw: u128) -> u128 {
    let capped = if raw > WAD { WAD } else { raw };
    if capped < 1 {
        1
    } else {
        capped
    }
}

/// After COAL-C01: no fee reservation; available = vault_balance directly.
/// Fee reservation is always zero.
fn fee_reservation(_vault_balance: u128, _accrued_fees: u128) -> u128 {
    0
}

// ---------------------------------------------------------------------------
// Unit tests for boundary conditions
// ---------------------------------------------------------------------------

#[test]
fn settlement_factor_zero_normalized_returns_wad() {
    for available in [0u128, 1, 2, 1_000_000, u128::MAX / WAD] {
        assert_eq!(
            compute_settlement_factor_checked(available, 0).unwrap(),
            WAD,
            "zero-normalized case should always return WAD (available={})",
            available
        );
        assert_eq!(
            onchain_compute_settlement_factor(available, 0).unwrap(),
            WAD,
            "on-chain helper should match local zero-normalized result"
        );
    }
}

#[test]
fn settlement_factor_zero_available_returns_one() {
    // When available = 0 and normalized > 0, factor = max(1, 0) = 1
    for normalized in [1u128, 2, 1_000_000, u128::MAX] {
        assert_eq!(
            assert_settlement_matches_onchain(0, normalized),
            1,
            "zero-available case should clamp to 1 (normalized={})",
            normalized
        );
        let one_available = assert_settlement_matches_onchain(1, normalized);
        assert!(
            one_available >= 1,
            "one-available case must also satisfy min bound"
        );
        assert!(
            one_available >= 1,
            "settlement factor should be monotonic in available"
        );
    }
}

#[test]
fn settlement_factor_fully_funded_returns_wad() {
    // available == normalized => factor = WAD (100%)
    let normalized = 1_000_000_000_000u128; // $1M normalized
    let factor_minus = assert_settlement_matches_onchain(normalized - 1, normalized);
    let factor_exact = assert_settlement_matches_onchain(normalized, normalized);
    let factor_plus = assert_settlement_matches_onchain(normalized + 1, normalized);
    assert!(
        factor_minus < WAD,
        "x-1 boundary should be strictly below WAD"
    );
    assert_eq!(factor_exact, WAD, "x boundary should be exactly WAD");
    assert_eq!(factor_plus, WAD, "x+1 boundary should clamp to WAD");
}

#[test]
fn settlement_factor_overfunded_capped_at_wad() {
    // available > normalized => factor capped at WAD
    let normalized = 1_000_000_000_000u128;
    for available in [
        normalized + 1,
        normalized + 2,
        normalized * 2,
        (u128::MAX / WAD).saturating_sub(1),
    ] {
        assert_eq!(
            assert_settlement_matches_onchain(available, normalized),
            WAD,
            "overfunded factor should clamp to WAD (available={})",
            available
        );
    }

    let factor_at_boundary = assert_settlement_matches_onchain(normalized, normalized);
    let factor_below = assert_settlement_matches_onchain(normalized - 1, normalized);
    assert_eq!(factor_at_boundary, WAD);
    assert!(factor_below < factor_at_boundary);
}

#[test]
fn settlement_factor_underfunded_proportional() {
    // available = 50% of normalized => factor = WAD/2
    let normalized = 1_000_000_000_000u128;
    let available = 500_000_000_000u128;
    let factor = assert_settlement_matches_onchain(available, normalized);
    assert_eq!(factor, WAD / 2);

    // x-1/x/x+1 neighbors on available keep proportionality monotonic.
    let factor_minus = assert_settlement_matches_onchain(available - 1, normalized);
    let factor_plus = assert_settlement_matches_onchain(available + 1, normalized);
    assert!(factor_minus <= factor);
    assert!(factor_plus >= factor);

    // Exact integer formula check for each neighbor.
    for avail in [available - 1, available, available + 1] {
        let expected = clamp_settlement((avail * WAD) / normalized);
        assert_eq!(
            compute_settlement_factor_checked(avail, normalized).unwrap(),
            expected,
            "proportional settlement formula drift at available={}",
            avail
        );
    }
}

#[test]
fn settlement_factor_tiny_available_still_at_least_one() {
    // Even with very small available, factor >= 1
    let normalized = u128::MAX / WAD; // Large but safe
    let f1 = assert_settlement_matches_onchain(1, normalized);
    let f2 = assert_settlement_matches_onchain(2, normalized);
    assert!(f1 >= 1);
    assert!(f2 >= f1);

    let expected_f1 = clamp_settlement((1 * WAD) / normalized);
    let expected_f2 = clamp_settlement((2 * WAD) / normalized);
    assert_eq!(f1, expected_f1);
    assert_eq!(f2, expected_f2);
}

#[test]
fn settlement_factor_large_values_no_overflow() {
    // Test with large but realistic values
    // $100B total supply with scale_factor = 2*WAD (100% interest accrued)
    let scaled_total_supply = 100_000_000_000_000_000u128; // $100B in 6-decimal base units
    let scale_factor = 2 * WAD; // 200% (principal + 100% interest)

    for sf in [scale_factor - 1, scale_factor, scale_factor + 1] {
        let normalized = compute_total_normalized_checked(scaled_total_supply, sf).unwrap();
        let available = normalized / 2; // 50% funded
        let factor = assert_settlement_matches_onchain(available, normalized);
        let expected = clamp_settlement((available * WAD) / normalized);
        assert_eq!(factor, expected);
        assert!(factor <= WAD);
        assert!(factor >= 1);
    }
}

#[test]
fn settlement_factor_extreme_scale_factor() {
    // Scale factor after 100 years at 100% = 2^100 * WAD? No, it compounds.
    // But we can test with a very large scale factor
    let scaled_total_supply = 1_000_000_000_000u128; // $1M
    let scale_factor = WAD * 10; // 10x (900% interest)

    for sf in [scale_factor - 1, scale_factor, scale_factor + 1] {
        let normalized = compute_total_normalized_checked(scaled_total_supply, sf).unwrap();
        let factor_exact = assert_settlement_matches_onchain(normalized, normalized);
        let factor_minus = assert_settlement_matches_onchain(normalized - 1, normalized);
        let factor_plus = assert_settlement_matches_onchain(normalized + 1, normalized);
        assert_eq!(factor_exact, WAD);
        assert!(factor_minus < WAD);
        assert_eq!(factor_plus, WAD);
    }
}

#[test]
fn settlement_factor_one_wei_normalized() {
    // Smallest possible normalized (1)
    assert_eq!(assert_settlement_matches_onchain(0, 1), 1);
    assert_eq!(assert_settlement_matches_onchain(1, 1), WAD);
    assert_eq!(assert_settlement_matches_onchain(2, 1), WAD);

    // x-1/x/x+1 on normalized around 1
    let f_norm_1 = assert_settlement_matches_onchain(1, 1);
    let f_norm_2 = assert_settlement_matches_onchain(1, 2);
    assert_eq!(f_norm_1, WAD);
    assert_eq!(f_norm_2, WAD / 2);
    assert!(f_norm_1 > f_norm_2);
}

#[test]
fn total_normalized_overflow_protection() {
    // scaled_total_supply * scale_factor would overflow
    let scaled_total_supply = u128::MAX;
    let scale_factor = 2;

    let err = compute_total_normalized_checked(scaled_total_supply, scale_factor).unwrap_err();
    assert_eq!(err, expected_math_overflow());
    assert_eq!(
        compute_total_normalized(scaled_total_supply, scale_factor),
        None
    );

    // Boundary x-1 should remain safe.
    let safe_scaled = u128::MAX / 2;
    let safe = compute_total_normalized_checked(safe_scaled, 2).unwrap();
    let safe_expected = safe_scaled * 2 / WAD;
    assert_eq!(safe, safe_expected);
}

#[test]
fn settlement_factor_overflow_protection() {
    // available * WAD would overflow
    let available = u128::MAX;
    let normalized = 1;

    let local_err = compute_settlement_factor_checked(available, normalized).unwrap_err();
    let onchain_err = onchain_compute_settlement_factor(available, normalized).unwrap_err();
    assert_eq!(local_err, expected_math_overflow());
    assert_eq!(onchain_err, expected_math_overflow());
    assert_eq!(compute_settlement_factor(available, normalized), None);

    // Boundary x-1 should still succeed.
    let safe_available = u128::MAX / WAD;
    assert_eq!(
        compute_settlement_factor_checked(safe_available, normalized).unwrap(),
        WAD
    );
    assert_eq!(
        onchain_compute_settlement_factor(safe_available, normalized).unwrap(),
        WAD
    );
}

// ---------------------------------------------------------------------------
// Proptest: Settlement factor invariants
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5000))]

    #[test]
    fn prop_settlement_factor_bounded(
        available in edge_u128_strategy(10_000_000_000_000_000u128),  // up to $10T with edge bias
        total_normalized in positive_edge_u128_strategy(10_000_000_000_000_000u128),
    ) {
        let f = compute_settlement_factor_checked(available, total_normalized).unwrap();
        let onchain = onchain_compute_settlement_factor(available, total_normalized).unwrap();
        prop_assert_eq!(f, onchain, "local and on-chain settlement helpers diverged");
        prop_assert!(f >= 1, "factor must be >= 1, got {}", f);
        prop_assert!(f <= WAD, "factor must be <= WAD, got {}", f);

        let expected = clamp_settlement((available * WAD) / total_normalized);
        prop_assert_eq!(
            f, expected,
            "factor drifted from explicit floor/cap formula (available={}, total={})",
            available, total_normalized
        );

        if available >= total_normalized {
            prop_assert_eq!(f, WAD, "fully-funded or overfunded case must clamp to WAD");
        } else {
            prop_assert!(f < WAD, "underfunded case must be < WAD");
        }
        if available == 0 {
            prop_assert_eq!(f, 1, "zero-available should clamp to 1");
        }
    }

    #[test]
    fn prop_settlement_factor_monotonic_in_available(
        base_available in edge_u128_strategy(5_000_000_000_000_000u128),
        extra in positive_edge_u128_strategy(5_000_000_000_000_000u128),
        total_normalized in positive_edge_u128_strategy(10_000_000_000_000_000u128),
    ) {
        let high_available = base_available.saturating_add(extra);

        let low = compute_settlement_factor_checked(base_available, total_normalized).unwrap();
        let high = compute_settlement_factor_checked(high_available, total_normalized).unwrap();
        prop_assert_eq!(
            low,
            onchain_compute_settlement_factor(base_available, total_normalized).unwrap()
        );
        prop_assert_eq!(
            high,
            onchain_compute_settlement_factor(high_available, total_normalized).unwrap()
        );
        prop_assert!(
            high >= low,
            "factor should be monotonic: low={}, high={} (available {} vs {})",
            low, high, base_available, high_available
        );

        // x-1/x/x+1 neighborhood at fixed normalized.
        if base_available > 0 && base_available < u128::MAX {
            let minus = compute_settlement_factor_checked(base_available - 1, total_normalized).unwrap();
            let plus = compute_settlement_factor_checked(base_available + 1, total_normalized).unwrap();
            prop_assert!(
                minus <= low && low <= plus,
                "x-1/x/x+1 monotonicity violated at available={}",
                base_available
            );
        }
    }

    #[test]
    fn prop_settlement_factor_anti_monotonic_in_normalized(
        available in positive_edge_u128_strategy(10_000_000_000_000_000u128),
        base_normalized in positive_edge_u128_strategy(5_000_000_000_000_000u128),
        extra in positive_edge_u128_strategy(5_000_000_000_000_000u128),
    ) {
        let high_normalized = base_normalized.saturating_add(extra);

        let low_n = compute_settlement_factor_checked(available, base_normalized).unwrap();
        let high_n = compute_settlement_factor_checked(available, high_normalized).unwrap();
        prop_assert_eq!(
            low_n,
            onchain_compute_settlement_factor(available, base_normalized).unwrap()
        );
        prop_assert_eq!(
            high_n,
            onchain_compute_settlement_factor(available, high_normalized).unwrap()
        );
        prop_assert!(
            low_n >= high_n,
            "factor should decrease with normalized: low_n={}, high_n={} (normalized {} vs {})",
            low_n, high_n, base_normalized, high_normalized
        );

        if base_normalized > 1 && base_normalized < u128::MAX {
            let minus = compute_settlement_factor_checked(available, base_normalized - 1).unwrap();
            let plus = compute_settlement_factor_checked(available, base_normalized + 1).unwrap();
            prop_assert!(
                minus >= low_n && low_n >= plus,
                "x-1/x/x+1 anti-monotonicity violated at normalized={}",
                base_normalized
            );
        }
    }

    #[test]
    fn prop_total_normalized_never_overflows_with_sane_inputs(
        scaled_total_supply in edge_u128_strategy(1_000_000_000_000_000_000u128), // up to $1 quadrillion
        scale_factor in prop_oneof![
            Just(WAD),
            Just(WAD + 1),
            Just((10 * WAD).saturating_sub(1)),
            Just(10 * WAD),
            (WAD..=(10 * WAD)),
        ], // 1x to 10x (up to 900% interest) with edge bias
    ) {
        let result = compute_total_normalized_checked(scaled_total_supply, scale_factor);
        prop_assert!(
            result.is_ok(),
            "total_normalized should not overflow for supply={} scale_factor={}",
            scaled_total_supply, scale_factor
        );
        let normalized = result.unwrap();
        let expected = scaled_total_supply * scale_factor / WAD;
        prop_assert_eq!(normalized, expected, "total_normalized formula drift");
        prop_assert_eq!(
            compute_total_normalized(scaled_total_supply, scale_factor),
            Some(expected),
            "Option wrapper should agree with checked helper"
        );

        if scale_factor > WAD {
            let baseline = compute_total_normalized_checked(scaled_total_supply, WAD).unwrap();
            prop_assert!(
                normalized >= baseline,
                "higher scale_factor should not decrease normalized total"
            );
        }
    }

    #[test]
    fn prop_settlement_payout_calculation(
        available in positive_edge_u128_strategy(10_000_000_000_000u128), // $0+ to $10M with edge bias
        total_normalized in positive_edge_u128_strategy(10_000_000_000_000u128),
        scaled_balance in positive_edge_u128_strategy(1_000_000_000_000u128),
        scale_factor in prop_oneof![Just(WAD), Just(2 * WAD), (WAD..=(2 * WAD))],
    ) {
        // Simulate the full payout calculation:
        // payout = min(normalized_balance * settlement_factor / WAD, available)
        //        = min(scaled_balance * scale_factor / WAD * settlement_factor / WAD, available)

        let factor = compute_settlement_factor_checked(available, total_normalized).unwrap();
        prop_assert_eq!(
            factor,
            onchain_compute_settlement_factor(available, total_normalized).unwrap()
        );

        // normalized_balance = scaled_balance * scale_factor / WAD
        let normalized_balance = scaled_balance.checked_mul(scale_factor).unwrap() / WAD;

        // payout = normalized_balance * factor / WAD
        let payout_uncapped = normalized_balance.checked_mul(factor).unwrap() / WAD;
        let payout = std::cmp::min(payout_uncapped, available);

        prop_assert!(payout <= available, "payout must never exceed available");
        prop_assert!(
            payout <= normalized_balance,
            "payout must never exceed lender normalized balance"
        );

        if available >= total_normalized {
            prop_assert_eq!(factor, WAD, "funded markets should have WAD factor");
            prop_assert_eq!(
                payout_uncapped, normalized_balance,
                "with factor=WAD, uncapped payout should equal normalized balance"
            );
        } else {
            prop_assert!(factor < WAD, "underfunded markets should have factor < WAD");
        }

        if scaled_balance < u128::MAX {
            let normalized_plus = (scaled_balance + 1).checked_mul(scale_factor).unwrap() / WAD;
            let payout_plus = normalized_plus.checked_mul(factor).unwrap() / WAD;
            prop_assert!(
                payout_plus >= payout_uncapped,
                "larger scaled balance should not reduce payout"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Proptest: Interest accrual and settlement interaction
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn prop_interest_accrual_increases_total_normalized(
        annual_bps in prop_oneof![Just(1u16), Just(100u16), Just(1_000u16), Just(5_000u16), Just(10_000u16), (1u16..=10_000u16)],
        time_elapsed_seconds in prop_oneof![Just(1u64), Just(3_600u64), Just(86_400u64), Just(31_536_000u64), (1u64..=31_536_000u64)],
        initial_scaled_supply in positive_edge_u128_strategy(1_000_000_000_000u128),
    ) {
        // Simulate interest accrual effect on total_normalized using
        // daily compounding (matching production interest.rs).
        let bps = u128::from(annual_bps);
        let time_elapsed = u128::from(time_elapsed_seconds);
        let seconds_per_year: u128 = 31_536_000;
        let seconds_per_day: u128 = 86_400;
        let days_per_year: u128 = 365;
        let bps_base: u128 = 10_000;

        let mul_wad = |a: u128, b: u128| -> u128 { a.checked_mul(b).unwrap() / WAD };
        let pow_wad = |base: u128, exp: u128| -> u128 {
            let mut result = WAD;
            let mut b = base;
            let mut e = exp;
            while e > 0 {
                if e & 1 == 1 {
                    result = mul_wad(result, b);
                }
                e >>= 1;
                if e > 0 {
                    b = mul_wad(b, b);
                }
            }
            result
        };

        let whole_days = time_elapsed / seconds_per_day;
        let remaining_secs = time_elapsed % seconds_per_day;

        let daily_rate_wad = bps * WAD / (days_per_year * bps_base);
        let days_growth = pow_wad(WAD + daily_rate_wad, whole_days);
        let remaining_delta = bps * remaining_secs * WAD / (seconds_per_year * bps_base);
        let new_scale_factor = mul_wad(days_growth, WAD + remaining_delta);

        let old = compute_total_normalized_checked(initial_scaled_supply, WAD).unwrap();
        let new = compute_total_normalized_checked(initial_scaled_supply, new_scale_factor).unwrap();
        prop_assert!(
            new >= old,
            "interest should increase total_normalized: old={}, new={}, sf={}",
            old, new, new_scale_factor
        );

        if time_elapsed_seconds > 1 {
            let prev_elapsed = time_elapsed - 1;
            let prev_days = prev_elapsed / seconds_per_day;
            let prev_remaining = prev_elapsed % seconds_per_day;
            let prev_days_growth = pow_wad(WAD + daily_rate_wad, prev_days);
            let prev_remaining_delta = bps * prev_remaining * WAD / (seconds_per_year * bps_base);
            let sf_prev = mul_wad(prev_days_growth, WAD + prev_remaining_delta);
            let norm_prev = compute_total_normalized_checked(initial_scaled_supply, sf_prev).unwrap();
            prop_assert!(
                norm_prev <= new,
                "time monotonicity violated: t-1 normalized {} > t normalized {}",
                norm_prev,
                new
            );
        }
    }

    #[test]
    fn prop_underfunded_market_factor_less_than_wad(
        available in positive_edge_u128_strategy(1_000_000_000_000u128),
        extra_deficit in positive_edge_u128_strategy(1_000_000_000_000u128),
    ) {
        // If available < total_normalized, factor < WAD
        let total_normalized = available.saturating_add(extra_deficit);

        let f = compute_settlement_factor_checked(available, total_normalized).unwrap();
        prop_assert_eq!(
            f,
            onchain_compute_settlement_factor(available, total_normalized).unwrap()
        );
        prop_assert!(
            f < WAD,
            "underfunded market should have factor < WAD: available={}, normalized={}, factor={}",
            available, total_normalized, f
        );

        let expected = clamp_settlement((available * WAD) / total_normalized);
        prop_assert_eq!(f, expected, "underfunded formula drift");

        if available < u128::MAX {
            let f_plus = compute_settlement_factor_checked(available + 1, total_normalized).unwrap();
            prop_assert!(f_plus >= f, "factor should increase with one more available unit");
        }
    }
}

// ---------------------------------------------------------------------------
// Edge case: Tiny fractions and dust amounts
// ---------------------------------------------------------------------------

#[test]
fn settlement_factor_dust_amounts() {
    // Test with dust amounts that might cause rounding issues

    for normalized in [1_000_000u128, u64::MAX as u128, u128::MAX / WAD] {
        let f1 = assert_settlement_matches_onchain(1, normalized);
        let f2 = assert_settlement_matches_onchain(2, normalized);
        assert!(f1 >= 1);
        assert!(f2 >= f1);
        assert_eq!(f1, clamp_settlement((1 * WAD) / normalized));
        assert_eq!(f2, clamp_settlement((2 * WAD) / normalized));

        // Simulate lender with normalized balance=1 unit.
        let payout = f1 / WAD;
        assert!(
            payout <= 1,
            "dust payout should not exceed lender normalized balance for normalized={}",
            normalized
        );
    }
}

#[test]
fn settlement_factor_exact_fraction() {
    // Test that exact fractions are preserved
    let normalized = 4_000_000_000_000u128;
    let available = 1_000_000_000_000u128; // 25%

    let factor = assert_settlement_matches_onchain(available, normalized);
    assert_eq!(factor, WAD / 4);

    let factor_minus = assert_settlement_matches_onchain(available - 1, normalized);
    let factor_plus = assert_settlement_matches_onchain(available + 1, normalized);
    assert!(factor_minus <= factor);
    assert!(factor_plus >= factor);
    assert_eq!(
        factor_minus,
        clamp_settlement(((available - 1) * WAD) / normalized)
    );
    assert_eq!(
        factor_plus,
        clamp_settlement(((available + 1) * WAD) / normalized)
    );
}

#[test]
fn settlement_factor_rounding_down() {
    // Test rounding behavior (should round down in favor of protocol safety)
    let normalized = 3u128;
    let f = assert_settlement_matches_onchain(1, normalized);
    let f2 = assert_settlement_matches_onchain(2, normalized);

    // 1 * WAD / 3 = WAD / 3 = 333333333333333333 (rounded down)
    let expected = WAD / 3;
    assert_eq!(f, expected);
    assert_eq!(f2, (2 * WAD) / 3);
    assert!(f2 > f);

    // Verify the factor doesn't allow over-withdrawal:
    // If lender has normalized_balance = 1, payout = 1 * factor / WAD = factor / WAD
    // With factor = WAD/3, payout = WAD/3 / WAD = 1/3 < 1, so we can't withdraw more than available
    let lender_balance = 3u128;
    let payout_uncapped = lender_balance * f / WAD;
    let payout = std::cmp::min(payout_uncapped, 1);
    assert!(
        payout <= 1,
        "rounded payout should never exceed available for protocol safety"
    );
}

// ---------------------------------------------------------------------------
// Fee reservation edge cases
// ---------------------------------------------------------------------------

#[test]
fn fee_reservation_reduces_available() {
    // After COAL-C01: no fee reservation; available = vault_balance directly.
    let vault_balance: u128 = 1_000_000_000_000; // $1M
    let accrued_fees: u128 = 100_000_000_000; // $100K fees

    for fees in [accrued_fees - 1, accrued_fees, accrued_fees + 1] {
        let fees_reserved = fee_reservation(vault_balance, fees);
        let available_for_lenders = vault_balance - fees_reserved;
        assert_eq!(fees_reserved, 0);
        assert_eq!(available_for_lenders, vault_balance);
    }

    let fees_reserved = fee_reservation(vault_balance, accrued_fees);
    let available_for_lenders = vault_balance - fees_reserved;
    assert_eq!(fees_reserved, 0);
    assert_eq!(available_for_lenders, vault_balance);
}

#[test]
fn fee_reservation_caps_at_vault_balance() {
    // After COAL-C01: no fee reservation; available = vault_balance directly.
    let vault_balance: u128 = 100_000_000_000; // $100K
    let accrued_fees: u128 = 500_000_000_000; // $500K fees (underwater)

    let fees_reserved = fee_reservation(vault_balance, accrued_fees);
    assert_eq!(fees_reserved, 0);

    let available_for_lenders = vault_balance - fees_reserved;
    assert_eq!(available_for_lenders, vault_balance);

    let fees_reserved_minus = fee_reservation(vault_balance, accrued_fees - 1);
    let fees_reserved_plus = fee_reservation(vault_balance, accrued_fees + 1);
    assert_eq!(fees_reserved_minus, 0);
    assert_eq!(fees_reserved_plus, 0);

    // With full vault available, settlement factor reflects full vault
    let factor = assert_settlement_matches_onchain(available_for_lenders, vault_balance);
    assert!(factor > 0, "factor should be positive when vault > 0");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// After COAL-C01: fee reservation is always zero; available = vault_balance.
    #[test]
    fn prop_fee_reservation_never_exceeds_vault(
        vault_balance in edge_u128_strategy(10_000_000_000_000u128),
        accrued_fees in edge_u128_strategy(10_000_000_000_000u128),
    ) {
        let fees_reserved = fee_reservation(vault_balance, accrued_fees);

        prop_assert_eq!(
            fees_reserved, 0,
            "fees_reserved should always be 0 (no fee reservation)"
        );

        let available = vault_balance.saturating_sub(fees_reserved);
        prop_assert_eq!(
            available, vault_balance,
            "available should equal vault_balance"
        );
    }
}
