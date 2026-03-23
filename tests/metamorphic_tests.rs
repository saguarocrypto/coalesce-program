//! Metamorphic property tests for the CoalesceFi Pinocchio lending protocol.
//!
//! Each test generates pairs of inputs related by known mathematical properties
//! and verifies that the output relation holds. This catches bugs that single-
//! input property tests may miss by exploiting algebraic identities of the
//! protocol's core formulas.

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
    clippy::range_minus_one,
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
use proptest::prelude::*;

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

/// Deposit scaling: scaled_amount = amount * WAD / scale_factor (floor division)
fn deposit_scale(amount: u64, scale_factor: u128) -> Option<u128> {
    let amount_u128 = u128::from(amount);
    amount_u128.checked_mul(WAD)?.checked_div(scale_factor)
}

/// Normalize: normalized = scaled_amount * scale_factor / WAD (floor division)
fn normalize(scaled_amount: u128, scale_factor: u128) -> Option<u128> {
    scaled_amount.checked_mul(scale_factor)?.checked_div(WAD)
}

/// Payout: payout = normalized * settlement_factor / WAD (floor division)
fn compute_payout(
    scaled_amount: u128,
    scale_factor: u128,
    settlement_factor: u128,
) -> Option<u128> {
    let normalized = normalize(scaled_amount, scale_factor)?;
    normalized.checked_mul(settlement_factor)?.checked_div(WAD)
}

/// Compute settlement factor: min(WAD, max(1, available * WAD / total_normalized))
fn compute_settlement_factor(available: u128, total_normalized: u128) -> Option<u128> {
    if total_normalized == 0 {
        return Some(WAD);
    }
    let numerator = available.checked_mul(WAD)?;
    let raw = numerator.checked_div(total_normalized)?;
    let capped = if raw > WAD { WAD } else { raw };
    Some(if capped < 1 { 1 } else { capped })
}

// ---------------------------------------------------------------------------
// Independent oracle helpers (metamorphic cross-check)
// ---------------------------------------------------------------------------

fn mul_wad_oracle(a: u128, b: u128) -> Option<u128> {
    math_oracle::mul_wad_checked(a, b)
}

fn pow_wad_oracle(base: u128, exp: u32) -> Option<u128> {
    math_oracle::pow_wad_checked(base, exp)
}

fn oracle_growth_factor_wad(annual_bps: u16, elapsed: i64) -> Option<u128> {
    if elapsed <= 0 {
        return Some(WAD);
    }
    math_oracle::growth_factor_wad_checked(annual_bps, elapsed)
}

fn oracle_interest_delta_wad(annual_bps: u16, elapsed: i64) -> Option<u128> {
    let growth = oracle_growth_factor_wad(annual_bps, elapsed)?;
    growth.checked_sub(WAD)
}

fn oracle_scale_factor_after_step(initial_sf: u128, annual_bps: u16, elapsed: i64) -> Option<u128> {
    let growth = oracle_growth_factor_wad(annual_bps, elapsed)?;
    initial_sf.checked_mul(growth)?.checked_div(WAD)
}

fn oracle_scaled_amount(amount: u64, sf: u128) -> Option<u128> {
    u128::from(amount).checked_mul(WAD)?.checked_div(sf)
}

fn oracle_normalized_amount(scaled: u128, sf: u128) -> Option<u128> {
    scaled.checked_mul(sf)?.checked_div(WAD)
}

fn oracle_settlement_factor(available: u128, total_normalized: u128) -> Option<u128> {
    if total_normalized == 0 {
        return Some(WAD);
    }
    let raw = available.checked_mul(WAD)?.checked_div(total_normalized)?;
    let capped = if raw > WAD { WAD } else { raw };
    Some(if capped < 1 { 1 } else { capped })
}

fn oracle_payout(scaled: u128, sf: u128, settlement: u128) -> Option<u128> {
    let normalized = oracle_normalized_amount(scaled, sf)?;
    normalized.checked_mul(settlement)?.checked_div(WAD)
}

fn oracle_fee_single_step(
    annual_bps: u16,
    fee_rate_bps: u16,
    supply: u128,
    elapsed: i64,
) -> Option<u64> {
    let growth = oracle_growth_factor_wad(annual_bps, elapsed)?;
    let delta_wad = growth.checked_sub(WAD)?;
    let fee_delta_wad = delta_wad
        .checked_mul(u128::from(fee_rate_bps))?
        .checked_div(BPS)?;
    // Use pre-accrual scale factor WAD (matches on-chain Finding 10 fix)
    let fee = supply
        .checked_mul(WAD)?
        .checked_div(WAD)?
        .checked_mul(fee_delta_wad)?
        .checked_div(WAD)?;
    u64::try_from(fee).ok()
}

// ---------------------------------------------------------------------------
// MR-1: Compound additivity
// accrue(t1) then accrue(t2) produces sf >= accrue(t1+t2) in one step
// (compound effect: splitting accrual into two steps compounds interest)
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr01_compound_additivity(
        annual_bps in 1u16..=10_000,
        t1 in 1i64..=15_768_000i64,
        t2 in 1i64..=15_768_000i64,
    ) {
        let config = make_config(0);
        let total_time = t1.saturating_add(t2);

        // Path A: single accrual for t1 + t2
        let mut m_single = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_single, &config, total_time).is_err() {
            return Ok(());
        }
        let sf_single = m_single.scale_factor();
        let oracle_single = oracle_scale_factor_after_step(WAD, annual_bps, total_time).unwrap();
        prop_assert_eq!(
            sf_single, oracle_single,
            "MR-1 oracle mismatch for single-step accrual"
        );

        // Path B: two-step accrual t1 then t2
        let mut m_compound = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_compound, &config, t1).is_err() {
            return Ok(());
        }
        if accrue_interest(&mut m_compound, &config, t1 + t2).is_err() {
            return Ok(());
        }
        let sf_compound = m_compound.scale_factor();
        let oracle_step1 = oracle_scale_factor_after_step(WAD, annual_bps, t1).unwrap();
        let oracle_compound = oracle_scale_factor_after_step(oracle_step1, annual_bps, t2).unwrap();
        prop_assert_eq!(
            sf_compound, oracle_compound,
            "MR-1 oracle mismatch for two-step compounding"
        );

        // Integer rounding can make split-vs-single differ by 1 WAD unit.
        prop_assert!(
            sf_compound + 1 >= sf_single,
            "MR-1 violated beyond rounding tolerance: compound sf ({}) < single sf ({}), bps={}, t1={}, t2={}",
            sf_compound, sf_single, annual_bps, t1, t2
        );
        let expected_min_delta = oracle_compound.saturating_sub(oracle_single);
        prop_assert!(
            sf_compound.saturating_sub(sf_single.saturating_sub(1)) >= expected_min_delta.saturating_sub(1),
            "MR-1 compound delta unexpectedly below oracle baseline"
        );
    }
}

// ---------------------------------------------------------------------------
// MR-2: Compound commutativity
// accrue(t1) then accrue(t2) == accrue(t2) then accrue(t1)
// (order of accrual segments should not matter for the same total time)
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr02_compound_commutativity(
        annual_bps in 1u16..=10_000,
        t1 in 1i64..=15_768_000i64,
        t2 in 1i64..=15_768_000i64,
    ) {
        let config = make_config(0);

        // Path A: accrue t1 then t2
        let mut m_ab = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_ab, &config, t1).is_err() {
            return Ok(());
        }
        if accrue_interest(&mut m_ab, &config, t1 + t2).is_err() {
            return Ok(());
        }
        let sf_ab = m_ab.scale_factor();
        let oracle_a = oracle_scale_factor_after_step(WAD, annual_bps, t1).unwrap();
        let oracle_ab = oracle_scale_factor_after_step(oracle_a, annual_bps, t2).unwrap();
        prop_assert_eq!(sf_ab, oracle_ab, "MR-2 oracle mismatch for t1->t2");

        // Path B: accrue t2 then t1
        let mut m_ba = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_ba, &config, t2).is_err() {
            return Ok(());
        }
        if accrue_interest(&mut m_ba, &config, t2 + t1).is_err() {
            return Ok(());
        }
        let sf_ba = m_ba.scale_factor();
        let oracle_b = oracle_scale_factor_after_step(WAD, annual_bps, t2).unwrap();
        let oracle_ba = oracle_scale_factor_after_step(oracle_b, annual_bps, t1).unwrap();
        prop_assert_eq!(sf_ba, oracle_ba, "MR-2 oracle mismatch for t2->t1");

        prop_assert_eq!(
            sf_ab, sf_ba,
            "MR-2 violated: accrue(t1,t2)={} != accrue(t2,t1)={}, bps={}, t1={}, t2={}",
            sf_ab, sf_ba, annual_bps, t1, t2
        );
        prop_assert_eq!(oracle_ab, oracle_ba, "oracle commutativity mismatch");
    }
}

// ---------------------------------------------------------------------------
// MR-3: Deposit additivity
// |scaled(a+b) - scaled(a) - scaled(b)| <= 1
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr03_deposit_additivity(
        a in 1u64..=500_000_000u64,
        b in 1u64..=500_000_000u64,
        sf_offset in 0u128..=WAD,
    ) {
        let sf = WAD + sf_offset;

        let scaled_a = match deposit_scale(a, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        let oracle_a = oracle_scaled_amount(a, sf).unwrap();
        prop_assert_eq!(scaled_a, oracle_a, "MR-3 oracle mismatch for scaled(a)");
        let scaled_b = match deposit_scale(b, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        let oracle_b = oracle_scaled_amount(b, sf).unwrap();
        prop_assert_eq!(scaled_b, oracle_b, "MR-3 oracle mismatch for scaled(b)");
        let sum_ab = match a.checked_add(b) {
            Some(s) => s,
            None => return Ok(()),
        };
        let scaled_sum = match deposit_scale(sum_ab, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        let oracle_sum = oracle_scaled_amount(sum_ab, sf).unwrap();
        prop_assert_eq!(scaled_sum, oracle_sum, "MR-3 oracle mismatch for scaled(a+b)");

        let sum_scaled = match scaled_a.checked_add(scaled_b) {
            Some(s) => s,
            None => return Ok(()),
        };

        let diff = if scaled_sum > sum_scaled {
            scaled_sum - sum_scaled
        } else {
            sum_scaled - scaled_sum
        };

        prop_assert!(
            diff <= 1,
            "MR-3 violated: |scaled(a+b) - scaled(a) - scaled(b)| = {} > 1, a={}, b={}, sf={}",
            diff, a, b, sf
        );

        let recovered_a = oracle_normalized_amount(scaled_a, sf).unwrap();
        let recovered_b = oracle_normalized_amount(scaled_b, sf).unwrap();
        prop_assert!(recovered_a <= u128::from(a));
        prop_assert!(recovered_b <= u128::from(b));
    }
}

// ---------------------------------------------------------------------------
// MR-4: Deposit scaling
// scaled(k*a) is within k of k * scaled(a) for integer k
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr04_deposit_scaling(
        a in 1u64..=100_000_000u64,
        k in 2u64..=10u64,
        sf_offset in 0u128..=WAD,
    ) {
        let sf = WAD + sf_offset;

        let scaled_a = match deposit_scale(a, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        prop_assert_eq!(
            scaled_a,
            oracle_scaled_amount(a, sf).unwrap(),
            "MR-4 oracle mismatch for scaled(a)"
        );
        let ka = match a.checked_mul(k) {
            Some(v) => v,
            None => return Ok(()),
        };
        let scaled_ka = match deposit_scale(ka, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        prop_assert_eq!(
            scaled_ka,
            oracle_scaled_amount(ka, sf).unwrap(),
            "MR-4 oracle mismatch for scaled(k*a)"
        );
        let k_scaled_a = match scaled_a.checked_mul(u128::from(k)) {
            Some(v) => v,
            None => return Ok(()),
        };

        let diff = if scaled_ka > k_scaled_a {
            scaled_ka - k_scaled_a
        } else {
            k_scaled_a - scaled_ka
        };

        prop_assert!(
            diff <= u128::from(k),
            "MR-4 violated: |scaled(k*a) - k*scaled(a)| = {} > k={}, a={}, sf={}",
            diff, k, a, sf
        );

        if a > 1 {
            let scaled_prev = deposit_scale(a - 1, sf).unwrap();
            prop_assert!(
                scaled_prev <= scaled_a,
                "MR-4 monotonicity violated for a-1/a"
            );
        }
        let scaled_next = deposit_scale(a.saturating_add(1), sf).unwrap();
        prop_assert!(scaled_next >= scaled_a, "MR-4 monotonicity violated for a/a+1");
    }
}

// ---------------------------------------------------------------------------
// MR-5: Settlement monotonicity
// settle(available=V) >= settle(available=V-1) for same total_normalized
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr05_settlement_monotonicity(
        available in 1u128..=10_000_000_000u128,
        total_normalized in 1u128..=10_000_000_000u128,
    ) {
        let f_high = match compute_settlement_factor(available, total_normalized) {
            Some(f) => f,
            None => return Ok(()),
        };
        let f_low = match compute_settlement_factor(available - 1, total_normalized) {
            Some(f) => f,
            None => return Ok(()),
        };
        let oracle_high = oracle_settlement_factor(available, total_normalized).unwrap();
        let oracle_low = oracle_settlement_factor(available - 1, total_normalized).unwrap();
        prop_assert_eq!(f_high, oracle_high, "MR-5 oracle mismatch high");
        prop_assert_eq!(f_low, oracle_low, "MR-5 oracle mismatch low");

        prop_assert!(
            f_high >= f_low,
            "MR-5 violated: settle(V={})={} < settle(V-1={})={}, total_norm={}",
            available, f_high, available - 1, f_low, total_normalized
        );
        let f_next = compute_settlement_factor(available.saturating_add(1), total_normalized).unwrap();
        prop_assert!(f_next >= f_high, "MR-5 x/x+1 monotonicity violated");
    }
}

// ---------------------------------------------------------------------------
// MR-6: Settlement scaling
// settle(k*available, k*total) == settle(available, total) for proportional scaling
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr06_settlement_scaling(
        available in 1u128..=1_000_000_000u128,
        total_normalized in 1u128..=1_000_000_000u128,
        k in 2u128..=100u128,
    ) {
        let f_original = match compute_settlement_factor(available, total_normalized) {
            Some(f) => f,
            None => return Ok(()),
        };
        prop_assert_eq!(
            f_original,
            oracle_settlement_factor(available, total_normalized).unwrap(),
            "MR-6 oracle mismatch original"
        );

        let k_available = match available.checked_mul(k) {
            Some(v) => v,
            None => return Ok(()),
        };
        let k_total = match total_normalized.checked_mul(k) {
            Some(v) => v,
            None => return Ok(()),
        };
        let f_scaled = match compute_settlement_factor(k_available, k_total) {
            Some(f) => f,
            None => return Ok(()),
        };
        prop_assert_eq!(
            f_scaled,
            oracle_settlement_factor(k_available, k_total).unwrap(),
            "MR-6 oracle mismatch scaled"
        );

        // Due to integer floor division, the scaled version may differ by at most 1
        let diff = if f_original > f_scaled {
            f_original - f_scaled
        } else {
            f_scaled - f_original
        };

        prop_assert!(
            diff <= 1,
            "MR-6 violated: |settle(a,t) - settle(k*a,k*t)| = {} > 1, a={}, t={}, k={}, f_orig={}, f_scaled={}",
            diff, available, total_normalized, k, f_original, f_scaled
        );
        let expected_original = ((available * WAD) / total_normalized).min(WAD).max(1);
        let expected_scaled = ((k_available * WAD) / k_total).min(WAD).max(1);
        prop_assert_eq!(f_original, expected_original);
        prop_assert_eq!(f_scaled, expected_scaled);
    }
}

// ---------------------------------------------------------------------------
// MR-7: Payout monotonicity in settlement
// higher settlement_factor => higher payout for same scaled_balance
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr07_payout_monotonic_in_settlement(
        scaled_balance in 1u128..=1_000_000_000_000u128,
        sf_offset in 0u128..=(WAD / 2),
        settlement_low in 1u128..=(WAD - 1),
        settlement_delta in 1u128..=WAD,
    ) {
        let scale_factor = WAD + sf_offset;
        let settlement_high = settlement_low.saturating_add(settlement_delta).min(WAD);
        if settlement_high <= settlement_low {
            return Ok(());
        }

        let payout_low = match compute_payout(scaled_balance, scale_factor, settlement_low) {
            Some(p) => p,
            None => return Ok(()),
        };
        let payout_high = match compute_payout(scaled_balance, scale_factor, settlement_high) {
            Some(p) => p,
            None => return Ok(()),
        };
        prop_assert_eq!(
            payout_low,
            oracle_payout(scaled_balance, scale_factor, settlement_low).unwrap(),
            "MR-7 oracle mismatch payout_low"
        );
        prop_assert_eq!(
            payout_high,
            oracle_payout(scaled_balance, scale_factor, settlement_high).unwrap(),
            "MR-7 oracle mismatch payout_high"
        );

        prop_assert!(
            payout_high >= payout_low,
            "MR-7 violated: payout(sf_high={})={} < payout(sf_low={})={}, balance={}",
            settlement_high, payout_high, settlement_low, payout_low, scaled_balance
        );
        let normalized = normalize(scaled_balance, scale_factor).unwrap();
        prop_assert!(payout_high <= normalized, "MR-7 payout should be bounded by normalized");
    }
}

// ---------------------------------------------------------------------------
// MR-8: Payout monotonicity in balance
// higher scaled_balance => higher payout for same settlement_factor
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr08_payout_monotonic_in_balance(
        balance_low in 1u128..=500_000_000_000u128,
        balance_delta in 1u128..=500_000_000_000u128,
        sf_offset in 0u128..=(WAD / 2),
        settlement_factor in 1u128..=WAD,
    ) {
        let scale_factor = WAD + sf_offset;
        let balance_high = balance_low.saturating_add(balance_delta);

        let payout_low = match compute_payout(balance_low, scale_factor, settlement_factor) {
            Some(p) => p,
            None => return Ok(()),
        };
        let payout_high = match compute_payout(balance_high, scale_factor, settlement_factor) {
            Some(p) => p,
            None => return Ok(()),
        };
        prop_assert_eq!(
            payout_low,
            oracle_payout(balance_low, scale_factor, settlement_factor).unwrap(),
            "MR-8 oracle mismatch payout_low"
        );
        prop_assert_eq!(
            payout_high,
            oracle_payout(balance_high, scale_factor, settlement_factor).unwrap(),
            "MR-8 oracle mismatch payout_high"
        );

        prop_assert!(
            payout_high >= payout_low,
            "MR-8 violated: payout(bal_high={})={} < payout(bal_low={})={}, sf={}, settle={}",
            balance_high, payout_high, balance_low, payout_low, scale_factor, settlement_factor
        );
        if balance_low > 1 {
            let payout_prev = compute_payout(balance_low - 1, scale_factor, settlement_factor).unwrap();
            prop_assert!(payout_prev <= payout_low, "MR-8 x-1/x monotonicity violated");
        }
    }
}

// ---------------------------------------------------------------------------
// MR-9: Fee monotonicity in rate
// higher fee_rate => higher fees for same interest/supply
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr09_fee_monotonic_in_rate(
        annual_bps in 1u16..=10_000,
        fee_low in 0u16..=5_000,
        fee_delta in 1u16..=5_000,
        time_elapsed in 1i64..=31_536_000i64,
        supply in 1_000_000u128..=1_000_000_000_000u128,
    ) {
        let fee_high = fee_low.saturating_add(fee_delta).min(10_000);
        if fee_high <= fee_low {
            return Ok(());
        }

        let mut m_low = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        let config_low = make_config(fee_low);
        if accrue_interest(&mut m_low, &config_low, time_elapsed).is_err() {
            return Ok(());
        }

        let mut m_high = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        let config_high = make_config(fee_high);
        if accrue_interest(&mut m_high, &config_high, time_elapsed).is_err() {
            return Ok(());
        }

        let oracle_low = match oracle_fee_single_step(annual_bps, fee_low, supply, time_elapsed) {
            Some(v) => v,
            None => return Ok(()),
        };
        let oracle_high = match oracle_fee_single_step(annual_bps, fee_high, supply, time_elapsed) {
            Some(v) => v,
            None => return Ok(()),
        };
        prop_assert_eq!(m_low.accrued_protocol_fees(), oracle_low, "MR-9 oracle mismatch low");
        prop_assert_eq!(m_high.accrued_protocol_fees(), oracle_high, "MR-9 oracle mismatch high");

        prop_assert!(
            m_high.accrued_protocol_fees() >= m_low.accrued_protocol_fees(),
            "MR-9 violated: fees(rate={})={} < fees(rate={})={}, bps={}, t={}, supply={}",
            fee_high, m_high.accrued_protocol_fees(),
            fee_low, m_low.accrued_protocol_fees(),
            annual_bps, time_elapsed, supply
        );
        prop_assert_eq!(
            m_high.scale_factor(),
            m_low.scale_factor(),
            "MR-9 fee-rate changes must not alter scale factor"
        );
    }
}

// ---------------------------------------------------------------------------
// MR-10: Fee monotonicity in supply
// higher supply => higher fees for same rate/period
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr10_fee_monotonic_in_supply(
        annual_bps in 1u16..=10_000,
        fee_rate_bps in 1u16..=10_000,
        supply_low in 1_000_000u128..=500_000_000_000u128,
        supply_delta in 1u128..=500_000_000_000u128,
        time_elapsed in 1i64..=31_536_000i64,
    ) {
        let supply_high = supply_low.saturating_add(supply_delta);
        let config = make_config(fee_rate_bps);

        let mut m_low = make_market(annual_bps, i64::MAX, WAD, supply_low, 0, 0);
        if accrue_interest(&mut m_low, &config, time_elapsed).is_err() {
            return Ok(());
        }

        let mut m_high = make_market(annual_bps, i64::MAX, WAD, supply_high, 0, 0);
        if accrue_interest(&mut m_high, &config, time_elapsed).is_err() {
            return Ok(());
        }

        let oracle_low = match oracle_fee_single_step(annual_bps, fee_rate_bps, supply_low, time_elapsed) {
            Some(v) => v,
            None => return Ok(()),
        };
        let oracle_high = match oracle_fee_single_step(annual_bps, fee_rate_bps, supply_high, time_elapsed) {
            Some(v) => v,
            None => return Ok(()),
        };
        prop_assert_eq!(m_low.accrued_protocol_fees(), oracle_low, "MR-10 oracle mismatch low");
        prop_assert_eq!(m_high.accrued_protocol_fees(), oracle_high, "MR-10 oracle mismatch high");

        prop_assert!(
            m_high.accrued_protocol_fees() >= m_low.accrued_protocol_fees(),
            "MR-10 violated: fees(supply={})={} < fees(supply={})={}, bps={}, fee_bps={}, t={}",
            supply_high, m_high.accrued_protocol_fees(),
            supply_low, m_low.accrued_protocol_fees(),
            annual_bps, fee_rate_bps, time_elapsed
        );
        prop_assert_eq!(
            m_high.scale_factor(),
            m_low.scale_factor(),
            "MR-10 supply changes must not alter scale factor"
        );
    }
}

// ---------------------------------------------------------------------------
// MR-11: Fee monotonicity in time
// longer accrual => more fees
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr11_fee_monotonic_in_time(
        annual_bps in 1u16..=10_000,
        fee_rate_bps in 1u16..=10_000,
        supply in 1_000_000u128..=1_000_000_000_000u128,
        t_short in 1i64..=15_768_000i64,
        t_delta in 1i64..=15_768_000i64,
    ) {
        let t_long = t_short.saturating_add(t_delta);
        let config = make_config(fee_rate_bps);

        let mut m_short = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        if accrue_interest(&mut m_short, &config, t_short).is_err() {
            return Ok(());
        }

        let mut m_long = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        if accrue_interest(&mut m_long, &config, t_long).is_err() {
            return Ok(());
        }

        let oracle_short = match oracle_fee_single_step(annual_bps, fee_rate_bps, supply, t_short) {
            Some(v) => v,
            None => return Ok(()),
        };
        let oracle_long = match oracle_fee_single_step(annual_bps, fee_rate_bps, supply, t_long) {
            Some(v) => v,
            None => return Ok(()),
        };
        prop_assert_eq!(m_short.accrued_protocol_fees(), oracle_short, "MR-11 oracle mismatch short");
        prop_assert_eq!(m_long.accrued_protocol_fees(), oracle_long, "MR-11 oracle mismatch long");

        prop_assert!(
            m_long.accrued_protocol_fees() >= m_short.accrued_protocol_fees(),
            "MR-11 violated: fees(t={})={} < fees(t={})={}, bps={}, fee_bps={}, supply={}",
            t_long, m_long.accrued_protocol_fees(),
            t_short, m_short.accrued_protocol_fees(),
            annual_bps, fee_rate_bps, supply
        );
        prop_assert!(m_long.scale_factor() >= m_short.scale_factor());
    }
}

// ---------------------------------------------------------------------------
// MR-12: Interest rate monotonicity
// higher annual_bps => higher scale_factor for same period
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr12_interest_rate_monotonicity(
        bps_low in 0u16..=5_000,
        bps_delta in 1u16..=5_000,
        time_elapsed in 1i64..=31_536_000i64,
    ) {
        let bps_high = bps_low.saturating_add(bps_delta).min(10_000);
        if bps_high <= bps_low {
            return Ok(());
        }

        let config = make_config(0);

        let mut m_low = make_market(bps_low, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_low, &config, time_elapsed).is_err() {
            return Ok(());
        }

        let mut m_high = make_market(bps_high, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_high, &config, time_elapsed).is_err() {
            return Ok(());
        }

        let oracle_low = oracle_scale_factor_after_step(WAD, bps_low, time_elapsed).unwrap();
        let oracle_high = oracle_scale_factor_after_step(WAD, bps_high, time_elapsed).unwrap();
        prop_assert_eq!(m_low.scale_factor(), oracle_low, "MR-12 oracle mismatch low");
        prop_assert_eq!(m_high.scale_factor(), oracle_high, "MR-12 oracle mismatch high");

        prop_assert!(
            m_high.scale_factor() >= m_low.scale_factor(),
            "MR-12 violated: sf(bps={})={} < sf(bps={})={}, t={}",
            bps_high, m_high.scale_factor(), bps_low, m_low.scale_factor(), time_elapsed
        );
    }
}

// ---------------------------------------------------------------------------
// MR-13: Interest time monotonicity
// longer time_elapsed => higher scale_factor for same rate
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr13_interest_time_monotonicity(
        annual_bps in 1u16..=10_000,
        t_short in 1i64..=15_768_000i64,
        t_delta in 1i64..=15_768_000i64,
    ) {
        let t_long = t_short.saturating_add(t_delta);
        let config = make_config(0);

        let mut m_short = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_short, &config, t_short).is_err() {
            return Ok(());
        }

        let mut m_long = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_long, &config, t_long).is_err() {
            return Ok(());
        }

        let oracle_short = oracle_scale_factor_after_step(WAD, annual_bps, t_short).unwrap();
        let oracle_long = oracle_scale_factor_after_step(WAD, annual_bps, t_long).unwrap();
        prop_assert_eq!(m_short.scale_factor(), oracle_short, "MR-13 oracle mismatch short");
        prop_assert_eq!(m_long.scale_factor(), oracle_long, "MR-13 oracle mismatch long");

        prop_assert!(
            m_long.scale_factor() >= m_short.scale_factor(),
            "MR-13 violated: sf(t={})={} < sf(t={})={}, bps={}",
            t_long, m_long.scale_factor(), t_short, m_short.scale_factor(), annual_bps
        );
    }
}

// ---------------------------------------------------------------------------
// MR-14: Normalize idempotence
// normalize(normalize(x)) == normalize(x)
// Applying normalize twice gives the same result because the second normalize
// just floors the already-normalized value.
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr14_normalize_idempotence(
        scaled_amount in 1u128..=1_000_000_000_000u128,
        sf_offset in 0u128..=WAD,
    ) {
        let sf = WAD + sf_offset;

        // First normalize: normalized1 = scaled_amount * sf / WAD
        let normalized1 = match normalize(scaled_amount, sf) {
            Some(n) => n,
            None => return Ok(()),
        };
        prop_assert_eq!(
            normalized1,
            oracle_normalized_amount(scaled_amount, sf).unwrap(),
            "MR-14 oracle mismatch on first normalize"
        );

        // Scale normalized1 back to "scaled" representation, then normalize again.
        // normalize(normalized1, sf) would compute normalized1 * sf / WAD, which is
        // NOT what we want. "Idempotence" here means: if we treat normalized1 as a
        // real token amount and re-deposit (scale) then re-normalize, we recover the
        // same normalized1 value (to within rounding).
        //
        // However, the stated property is: normalize applied to its own output is
        // stable. Since normalize(x) = floor(x * sf / WAD), applying normalize to
        // normalized1 treats it as a new scaled amount:
        //   normalize(normalize(x)) = floor(floor(x * sf / WAD) * sf / WAD)
        //
        // This is NOT idempotent in general (it keeps growing). Instead, test the
        // meaningful invariant: if we deposit `normalized1` tokens (converting to
        // scaled), then normalize back, we get <= normalized1 (floor rounds down).
        // And the loss is at most 1.
        let re_scaled = match deposit_scale(
            u64::try_from(normalized1.min(u128::from(u64::MAX))).unwrap_or(u64::MAX),
            sf,
        ) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };

        let normalized2 = match normalize(re_scaled, sf) {
            Some(n) => n,
            None => return Ok(()),
        };
        prop_assert_eq!(
            normalized2,
            oracle_normalized_amount(re_scaled, sf).unwrap(),
            "MR-14 oracle mismatch on second normalize"
        );

        // The re-normalized value should be <= original normalized (rounding down)
        // and the difference should be at most 2 (two floor divisions).
        let n1_clamped = normalized1.min(u128::from(u64::MAX));
        let loss = n1_clamped.saturating_sub(normalized2);
        prop_assert!(
            loss <= 2,
            "MR-14 violated: normalize round-trip loss = {}, n1={}, n2={}, sf={}",
            loss, n1_clamped, normalized2, sf
        );
        prop_assert!(
            normalized2 <= n1_clamped,
            "MR-14 violated: second normalize should not exceed first normalize"
        );
    }
}

// ---------------------------------------------------------------------------
// MR-15: Double deposit vs single
// deposit(a) + deposit(b) gives total scaled within 1 of deposit(a+b)
// (Duplicate of MR-3 with different parameter ranges for coverage)
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr15_double_deposit_vs_single(
        a in 1u64..=1_000_000_000u64,
        b in 1u64..=1_000_000_000u64,
        sf_offset in 0u128..=(WAD / 2),
    ) {
        let sf = WAD + sf_offset;

        let scaled_a = match deposit_scale(a, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        let scaled_b = match deposit_scale(b, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        prop_assert_eq!(scaled_a, oracle_scaled_amount(a, sf).unwrap());
        prop_assert_eq!(scaled_b, oracle_scaled_amount(b, sf).unwrap());

        let sum_amount = match a.checked_add(b) {
            Some(s) => s,
            None => return Ok(()),
        };

        let scaled_combined = match deposit_scale(sum_amount, sf) {
            Some(s) => s,
            None => return Ok(()),
        };
        prop_assert_eq!(scaled_combined, oracle_scaled_amount(sum_amount, sf).unwrap());

        let sum_individual = match scaled_a.checked_add(scaled_b) {
            Some(s) => s,
            None => return Ok(()),
        };

        let diff = if scaled_combined > sum_individual {
            scaled_combined - sum_individual
        } else {
            sum_individual - scaled_combined
        };

        prop_assert!(
            diff <= 1,
            "MR-15 violated: |scaled(a+b) - (scaled(a)+scaled(b))| = {} > 1, a={}, b={}, sf={}",
            diff, a, b, sf
        );
        let recovered = normalize(scaled_combined, sf).unwrap();
        prop_assert!(recovered <= u128::from(sum_amount));
    }
}

// ---------------------------------------------------------------------------
// MR-16: Withdrawal symmetry at WAD
// when sf=WAD and settlement=WAD, payout(deposit(x)) == x
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr16_withdrawal_symmetry_at_wad(
        amount in 1u64..=1_000_000_000_000u64,
    ) {
        let sf = WAD;
        let settlement = WAD;

        let scaled = match deposit_scale(amount, sf) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };

        let payout = match compute_payout(scaled, sf, settlement) {
            Some(p) => p,
            None => return Ok(()),
        };
        prop_assert_eq!(scaled, oracle_scaled_amount(amount, sf).unwrap());
        prop_assert_eq!(payout, oracle_payout(scaled, sf, settlement).unwrap());

        // At sf=WAD, settlement=WAD:
        //   scaled = amount * WAD / WAD = amount
        //   normalized = amount * WAD / WAD = amount
        //   payout = amount * WAD / WAD = amount
        prop_assert_eq!(
            payout, u128::from(amount),
            "MR-16 violated: payout ({}) != original ({}) at WAD/WAD",
            payout, amount
        );

        let payout_half = compute_payout(scaled, sf, WAD / 2).unwrap();
        prop_assert_eq!(
            payout_half,
            u128::from(amount) / 2,
            "MR-16 half-settlement boundary mismatch at WAD scale"
        );
    }
}

// ---------------------------------------------------------------------------
// MR-17: Scale factor dominance
// for sf1 > sf2, scaled(amount, sf1) <= scaled(amount, sf2)
// (higher scale factor means each token represents more value, so fewer
//  scaled tokens are issued for the same deposit amount)
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr17_scale_factor_dominance(
        amount in 1u64..=1_000_000_000_000u64,
        sf_base_offset in 0u128..=(WAD / 2),
        sf_delta in 1u128..=(WAD / 2),
    ) {
        let sf_low = WAD + sf_base_offset;
        let sf_high = sf_low.saturating_add(sf_delta);

        let scaled_low = match deposit_scale(amount, sf_low) {
            Some(s) => s,
            None => return Ok(()),
        };
        let scaled_high = match deposit_scale(amount, sf_high) {
            Some(s) => s,
            None => return Ok(()),
        };
        prop_assert_eq!(scaled_low, oracle_scaled_amount(amount, sf_low).unwrap());
        prop_assert_eq!(scaled_high, oracle_scaled_amount(amount, sf_high).unwrap());

        prop_assert!(
            scaled_high <= scaled_low,
            "MR-17 violated: scaled(amount={}, sf_high={})={} > scaled(amount={}, sf_low={})={}",
            amount, sf_high, scaled_high, amount, sf_low, scaled_low
        );
        let recovered_low = normalize(scaled_low, sf_low).unwrap();
        let recovered_high = normalize(scaled_high, sf_high).unwrap();
        prop_assert!(recovered_low <= u128::from(amount));
        prop_assert!(recovered_high <= u128::from(amount));
    }
}

// ===========================================================================
// Additional metamorphic relations (bonus coverage)
// ===========================================================================

// ---------------------------------------------------------------------------
// MR-18: Fee-scale independence
// Fee rate does not affect scale_factor — only the fee accumulator changes.
// Two markets with different fee rates but same interest rate should have
// identical scale_factor after accrual.
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn mr18_fee_scale_independence(
        annual_bps in 1u16..=10_000,
        fee_a in 0u16..=10_000,
        fee_b in 0u16..=10_000,
        time_elapsed in 1i64..=31_536_000i64,
        supply in 1u128..=1_000_000_000_000u128,
    ) {
        let config_a = make_config(fee_a);
        let config_b = make_config(fee_b);

        let mut m_a = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        if accrue_interest(&mut m_a, &config_a, time_elapsed).is_err() {
            return Ok(());
        }

        let mut m_b = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        if accrue_interest(&mut m_b, &config_b, time_elapsed).is_err() {
            return Ok(());
        }

        let oracle_sf = oracle_scale_factor_after_step(WAD, annual_bps, time_elapsed).unwrap();
        let oracle_fee_a = match oracle_fee_single_step(annual_bps, fee_a, supply, time_elapsed) {
            Some(v) => v,
            None => return Ok(()),
        };
        let oracle_fee_b = match oracle_fee_single_step(annual_bps, fee_b, supply, time_elapsed) {
            Some(v) => v,
            None => return Ok(()),
        };
        prop_assert_eq!(m_a.scale_factor(), oracle_sf);
        prop_assert_eq!(m_b.scale_factor(), oracle_sf);
        prop_assert_eq!(m_a.accrued_protocol_fees(), oracle_fee_a);
        prop_assert_eq!(m_b.accrued_protocol_fees(), oracle_fee_b);

        prop_assert_eq!(
            m_a.scale_factor(), m_b.scale_factor(),
            "MR-18 violated: sf differs with fee_a={} vs fee_b={}: {} vs {}",
            fee_a, fee_b, m_a.scale_factor(), m_b.scale_factor()
        );
        if fee_a > fee_b {
            prop_assert!(m_a.accrued_protocol_fees() >= m_b.accrued_protocol_fees());
        } else if fee_b > fee_a {
            prop_assert!(m_b.accrued_protocol_fees() >= m_a.accrued_protocol_fees());
        }
    }
}

// ---------------------------------------------------------------------------
// MR-19: Zero-interest identity
// When annual_bps=0, scale_factor remains exactly WAD regardless of time.
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn mr19_zero_interest_identity(
        time_elapsed in 1i64..=31_536_000i64,
        supply in 0u128..=1_000_000_000_000u128,
        fee_rate_bps in 0u16..=10_000,
    ) {
        let config = make_config(fee_rate_bps);
        let mut market = make_market(0, i64::MAX, WAD, supply, 0, 0);
        if accrue_interest(&mut market, &config, time_elapsed).is_err() {
            return Ok(());
        }
        let first_sf = market.scale_factor();
        let first_fees = market.accrued_protocol_fees();
        let first_ts = market.last_accrual_timestamp();

        prop_assert_eq!(
            market.scale_factor(), WAD,
            "MR-19 violated: sf={} != WAD after accrual with 0% rate, t={}",
            market.scale_factor(), time_elapsed
        );
        prop_assert_eq!(
            market.accrued_protocol_fees(), 0,
            "MR-19 violated: fees={} != 0 after accrual with 0% rate",
            market.accrued_protocol_fees()
        );
        prop_assert_eq!(first_ts, time_elapsed);

        // Re-applying at same timestamp must be a no-op.
        accrue_interest(&mut market, &config, time_elapsed).unwrap();
        prop_assert_eq!(market.scale_factor(), first_sf);
        prop_assert_eq!(market.accrued_protocol_fees(), first_fees);
        prop_assert_eq!(market.last_accrual_timestamp(), first_ts);
    }
}

// ---------------------------------------------------------------------------
// MR-20: Payout bounded by normalized amount
// For any settlement_factor in [1, WAD], payout <= normalized
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr20_payout_bounded_by_normalized(
        scaled_balance in 1u128..=1_000_000_000_000u128,
        sf_offset in 0u128..=(WAD / 2),
        settlement_factor in 1u128..=WAD,
    ) {
        let scale_factor = WAD + sf_offset;

        let normalized = match normalize(scaled_balance, scale_factor) {
            Some(n) => n,
            None => return Ok(()),
        };

        let payout = match compute_payout(scaled_balance, scale_factor, settlement_factor) {
            Some(p) => p,
            None => return Ok(()),
        };
        prop_assert_eq!(
            payout,
            oracle_payout(scaled_balance, scale_factor, settlement_factor).unwrap()
        );

        prop_assert!(
            payout <= normalized,
            "MR-20 violated: payout ({}) > normalized ({}), settlement={}, sf={}",
            payout, normalized, settlement_factor, scale_factor
        );
        let payout_full = compute_payout(scaled_balance, scale_factor, WAD).unwrap();
        prop_assert_eq!(payout_full, normalized, "full settlement should equal normalized");
    }
}

// ---------------------------------------------------------------------------
// MR-21: Interest accrual linearity for sub-day single-step accrual
// Under the daily-compound model, the remaining sub-day portion is linear:
// sf = WAD + WAD * annual_bps * t / (SPY * BPS), for 0 < t < 86400.
// Verify delta proportionality in this linear region only.
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn mr21_single_step_linearity(
        annual_bps in 1u16..=10_000,
        t1 in 1i64..=86_399i64,
        t2 in 1i64..=86_399i64,
    ) {
        let config = make_config(0);

        let mut m1 = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m1, &config, t1).is_err() {
            return Ok(());
        }
        let delta1 = m1.scale_factor() - WAD;
        let oracle1 = oracle_scale_factor_after_step(WAD, annual_bps, t1).unwrap();
        prop_assert_eq!(m1.scale_factor(), oracle1);

        let mut m2 = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m2, &config, t2).is_err() {
            return Ok(());
        }
        let delta2 = m2.scale_factor() - WAD;
        let oracle2 = oracle_scale_factor_after_step(WAD, annual_bps, t2).unwrap();
        prop_assert_eq!(m2.scale_factor(), oracle2);

        // For a single step from WAD:
        // delta = WAD * annual_bps * t / (SPY * BPS)
        // So delta1 / t1 should equal delta2 / t2 (up to floor division rounding).
        // Cross-multiply: delta1 * t2 should be close to delta2 * t1.
        let lhs = delta1.checked_mul(t2 as u128);
        let rhs = delta2.checked_mul(t1 as u128);

        if let (Some(l), Some(r)) = (lhs, rhs) {
            // The maximum rounding error per delta is at most 1 (from floor division).
            // Cross-multiplied: |delta1*t2 - delta2*t1| <= max(t1, t2).
            let diff = if l > r { l - r } else { r - l };
            let max_t = (t1 as u128).max(t2 as u128);
            prop_assert!(
                diff <= max_t,
                "MR-21 violated: |d1*t2 - d2*t1| = {} > max(t1,t2) = {}, bps={}, t1={}, t2={}, d1={}, d2={}",
                diff, max_t, annual_bps, t1, t2, delta1, delta2
            );
        }

        let mut m_total = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        if accrue_interest(&mut m_total, &config, t1.saturating_add(t2)).is_ok() {
            let sf_total = m_total.scale_factor();
            prop_assert!(
                sf_total >= m1.scale_factor().max(m2.scale_factor()),
                "MR-21 combined-time scale factor should dominate each individual horizon"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// MR-22: Settlement at full coverage
// When available >= total_normalized, settlement_factor should be WAD
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn mr22_settlement_full_coverage(
        total_normalized in 1u128..=1_000_000_000u128,
        extra in 0u128..=1_000_000_000u128,
    ) {
        let available = total_normalized.saturating_add(extra);

        let factor = match compute_settlement_factor(available, total_normalized) {
            Some(f) => f,
            None => return Ok(()),
        };
        prop_assert_eq!(
            factor,
            oracle_settlement_factor(available, total_normalized).unwrap()
        );

        prop_assert_eq!(
            factor, WAD,
            "MR-22 violated: settlement_factor={} != WAD when available={} >= total_norm={}",
            factor, available, total_normalized
        );

        if total_normalized > 1 {
            let below = compute_settlement_factor(total_normalized - 1, total_normalized).unwrap();
            prop_assert!(below < WAD, "MR-22 x-1 boundary should be below WAD");
        }
    }
}

// ---------------------------------------------------------------------------
// MR-23: Rounding always favors protocol
// For any deposit, the round-trip (deposit -> normalize) never returns more
// than the original amount.
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn mr23_rounding_favors_protocol(
        amount in 1u64..=1_000_000_000_000u64,
        sf_offset in 0u128..=WAD,
    ) {
        let sf = WAD + sf_offset;
        let amount_u128 = u128::from(amount);

        let scaled = match deposit_scale(amount, sf) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };
        prop_assert_eq!(scaled, oracle_scaled_amount(amount, sf).unwrap());

        let recovered = match normalize(scaled, sf) {
            Some(r) => r,
            None => return Ok(()),
        };
        prop_assert_eq!(recovered, oracle_normalized_amount(scaled, sf).unwrap());

        prop_assert!(
            recovered <= amount_u128,
            "MR-23 violated: recovered ({}) > original ({}), sf={}",
            recovered, amount_u128, sf
        );

        let loss = amount_u128.saturating_sub(recovered);
        let loss_bound = sf.div_ceil(WAD);
        prop_assert!(
            loss <= loss_bound,
            "MR-23 violated: rounding loss {} exceeds bound {} (amount={}, sf={})",
            loss, loss_bound, amount, sf
        );

        let payout_at_wad = compute_payout(scaled, sf, WAD).unwrap();
        prop_assert_eq!(payout_at_wad, recovered, "WAD settlement should recover normalized amount");
    }
}
