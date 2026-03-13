//! Stress tests at extreme values.
//!
//! Exercises the core math at u64/u128 boundaries, maximum fee/interest
//! combinations, and adversarial input ranges. These tests verify that the
//! protocol handles extreme-but-valid inputs without panicking and produces
//! mathematically correct results.

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

const SECONDS_PER_DAY: i64 = 86_400;

fn mul_wad(a: u128, b: u128) -> Option<u128> {
    math_oracle::mul_wad_checked(a, b)
}

fn pow_wad(base: u128, exp: u32) -> Option<u128> {
    math_oracle::pow_wad_checked(base, exp)
}

fn growth_factor_wad(annual_interest_bps: u16, elapsed_seconds: i64) -> Option<u128> {
    math_oracle::growth_factor_wad_checked(annual_interest_bps, elapsed_seconds)
}

fn expected_scale_factor(
    old_scale_factor: u128,
    annual_interest_bps: u16,
    elapsed_seconds: i64,
) -> Option<u128> {
    mul_wad(
        old_scale_factor,
        growth_factor_wad(annual_interest_bps, elapsed_seconds)?,
    )
}

fn expected_fee_delta(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_interest_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> Option<u64> {
    if fee_rate_bps == 0 || scaled_total_supply == 0 || elapsed_seconds <= 0 {
        return Some(0);
    }

    let interest_delta_wad =
        growth_factor_wad(annual_interest_bps, elapsed_seconds)?.checked_sub(WAD)?;
    if interest_delta_wad == 0 {
        return Some(0);
    }

    let fee_delta_wad = interest_delta_wad
        .checked_mul(u128::from(fee_rate_bps))?
        .checked_div(BPS)?;
    // Use pre-accrual scale_factor_before (matches on-chain Finding 10 fix)
    let fee_normalized = scaled_total_supply
        .checked_mul(scale_factor_before)?
        .checked_div(WAD)?
        .checked_mul(fee_delta_wad)?
        .checked_div(WAD)?;
    u64::try_from(fee_normalized).ok()
}

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

// ===========================================================================
// 3a: Interest accrual near overflow boundary
// ===========================================================================

/// scale_factor = WAD, supply near u128 safe boundary.
/// Verifies either correct computation or clean MathOverflow.
#[test]
fn stress_interest_accrual_large_supply() {
    // u128::MAX / (2 * WAD) is the largest supply where
    // supply * scale_factor doesn't overflow u128.
    let max_safe_supply = u128::MAX / (2 * WAD);

    let mut market = make_market(
        10_000, // 100% annual
        i64::MAX,
        WAD,
        max_safe_supply,
        0,
        0,
    );
    let config = make_config(10_000); // 100% fee rate

    // Full year — the fee computation involves supply * new_sf / WAD * fee_delta / WAD
    // new_sf = 2*WAD at 100% for 1 year
    // supply * 2*WAD might overflow since supply = MAX / (2*WAD)
    // supply * 2*WAD = (MAX / (2*WAD)) * 2*WAD = MAX — exactly at boundary
    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);

    // Should either succeed or return MathOverflow, never panic
    match result {
        Ok(()) => {
            // Pin exact expected values
            let expected_sf = expected_scale_factor(WAD, 10_000, SECONDS_PER_YEAR as i64)
                .expect("expected sf should fit");
            assert_eq!(
                market.scale_factor(),
                expected_sf,
                "scale_factor must match daily-compound oracle = {}",
                expected_sf
            );
            let expected_fee = expected_fee_delta(
                max_safe_supply,
                WAD,
                10_000,
                10_000,
                SECONDS_PER_YEAR as i64,
            )
            .expect("expected fee should fit");
            assert_eq!(
                market.accrued_protocol_fees(),
                expected_fee,
                "fees must match daily-compound oracle"
            );
            assert_eq!(
                market.last_accrual_timestamp(),
                SECONDS_PER_YEAR as i64,
                "last_accrual must be updated to current_timestamp"
            );
            // supply should remain unchanged (accrue_interest does not modify supply)
            assert_eq!(market.scaled_total_supply(), max_safe_supply);
        },
        Err(e) => {
            // MathOverflow (Custom(41)) is acceptable for extreme values
            assert_eq!(
                e,
                pinocchio::error::ProgramError::Custom(41),
                "overflow must be MathOverflow (Custom(41)), got {:?}",
                e
            );
        },
    }
}

/// Supply slightly above the safe boundary — must not panic.
#[test]
fn stress_interest_accrual_supply_over_safe_boundary() {
    let over_safe = u128::MAX / WAD; // larger than safe boundary

    let mut market = make_market(10_000, i64::MAX, WAD, over_safe, 0, 0);
    let config = make_config(10_000);

    // Must not panic
    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    // MathOverflow expected — supply * new_sf will overflow
    assert!(result.is_err(), "should overflow with over-boundary supply");
    assert_eq!(
        result.unwrap_err(),
        pinocchio::error::ProgramError::Custom(41),
        "must be MathOverflow (Custom(41))"
    );
    // State must be unchanged after error
    assert_eq!(
        market.scale_factor(),
        WAD,
        "scale_factor must be unchanged after overflow"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "fees must be unchanged after overflow"
    );
}

/// Scale factor already at 2*WAD (from prior interest), then accrue more.
#[test]
fn stress_interest_compound_on_large_scale_factor() {
    let big_sf = WAD * 10; // 10x interest already accrued
    let supply = 1_000_000_000_000u128; // 1M USDC

    let mut market = make_market(
        10_000, // 100%
        i64::MAX,
        big_sf,
        supply,
        0,
        0,
    );
    let config = make_config(5000); // 50% fee

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);

    match result {
        Ok(()) => {
            let expected_sf = expected_scale_factor(big_sf, 10_000, SECONDS_PER_YEAR as i64)
                .expect("expected sf should fit");
            assert_eq!(
                market.scale_factor(),
                expected_sf,
                "sf must match daily-compound oracle after 100% on 10x"
            );
            let expected_fee =
                expected_fee_delta(supply, big_sf, 10_000, 5000, SECONDS_PER_YEAR as i64)
                    .expect("expected fee should fit");
            assert_eq!(
                market.accrued_protocol_fees(),
                expected_fee,
                "fees must match daily-compound oracle = {}",
                expected_fee
            );
            assert_eq!(
                market.scaled_total_supply(),
                supply,
                "supply must be unchanged"
            );
            assert_eq!(
                market.last_accrual_timestamp(),
                SECONDS_PER_YEAR as i64,
                "last_accrual must be updated"
            );
        },
        Err(e) => {
            // Could overflow on fee computation — must be MathOverflow
            assert_eq!(
                e,
                pinocchio::error::ProgramError::Custom(41),
                "overflow must be MathOverflow (Custom(41))"
            );
        },
    }
}

// ===========================================================================
// 3b: Deposit scaling with u64::MAX amount
// ===========================================================================

#[test]
fn stress_deposit_max_amount_at_wad() {
    let amount = u64::MAX;
    let scale_factor = WAD;

    // Pin exact expected scaled_amount
    let expected_scaled: u128 = u128::from(u64::MAX); // 18446744073709551615

    let amount_u128 = u128::from(amount);
    let scaled = amount_u128.checked_mul(WAD).unwrap() / scale_factor;

    assert_eq!(
        scaled, expected_scaled,
        "scaled must be exactly u64::MAX = {}",
        expected_scaled
    );

    // Normalize back: round-trip must be lossless at WAD
    let recovered = scaled * scale_factor / WAD;
    assert_eq!(
        recovered, expected_scaled,
        "round-trip must be lossless at WAD"
    );
}

#[test]
fn stress_deposit_max_amount_at_double_wad() {
    let amount = u64::MAX;
    let scale_factor = 2 * WAD;

    // Pin exact expected scaled_amount:
    // u64::MAX * WAD / (2*WAD) = u64::MAX / 2 (integer division)
    // u64::MAX = 18446744073709551615, u64::MAX / 2 = 9223372036854775807
    let expected_scaled: u128 = u128::from(u64::MAX) / 2; // 9223372036854775807

    let amount_u128 = u128::from(amount);
    let scaled = amount_u128
        .checked_mul(WAD)
        .expect("u64::MAX * WAD should fit u128")
        / scale_factor;

    assert_eq!(
        scaled, expected_scaled,
        "scaled must be exactly u64::MAX / 2 = {}",
        expected_scaled
    );

    // Normalize: scaled * 2*WAD / WAD = scaled * 2
    let recovered = scaled * scale_factor / WAD;
    // Due to floor division: recovered <= original
    assert!(recovered <= amount_u128);
    let loss = amount_u128 - recovered;
    assert!(
        loss <= 1,
        "round-trip loss should be at most 1, got {}",
        loss
    );
}

#[test]
fn stress_deposit_max_amount_overflow_check() {
    let amount = u64::MAX;
    let scale_factor = WAD + 1; // slightly above WAD

    let amount_u128 = u128::from(amount);
    // u64::MAX * WAD = 18446744073709551615 * 1e18
    // = ~1.8e37, well within u128::MAX (~3.4e38)
    let product = amount_u128.checked_mul(WAD);
    assert!(product.is_some(), "u64::MAX * WAD must fit in u128");

    let scaled = product.unwrap() / scale_factor;
    assert!(scaled > 0, "scaled amount must be positive");
    assert!(
        scaled < u128::from(u64::MAX),
        "scaled must be less than u64::MAX when sf > WAD"
    );

    // Pin: scaled = u64::MAX * WAD / (WAD + 1). Due to sf being only 1 above WAD,
    // the result should be very close to u64::MAX.
    let min_expected = u128::from(u64::MAX) - u128::from(u64::MAX) / WAD - 1;
    assert!(
        scaled >= min_expected,
        "scaled {} should be close to u64::MAX (>= {})",
        scaled,
        min_expected
    );
}

// ===========================================================================
// 3c: Settlement factor extreme overfunding
// ===========================================================================

#[test]
fn stress_settlement_extreme_overfunding() {
    let available = u128::from(u64::MAX); // ~1.8e19
    let total_normalized: u128 = 1; // 1 lamport

    let raw = available.checked_mul(WAD).unwrap() / total_normalized;
    // raw = u64::MAX * 1e18 — very large
    assert!(raw > WAD);

    let capped = if raw > WAD { WAD } else { raw };
    // Pin exact settlement_factor: must be exactly WAD (fully funded)
    assert_eq!(
        capped, WAD,
        "overfunded settlement must be capped at exactly WAD = {}",
        WAD
    );
}

#[test]
fn stress_settlement_extreme_underfunding() {
    let available: u128 = 1; // 1 lamport
    let total_normalized: u128 = u128::from(u64::MAX); // max

    let raw = available.checked_mul(WAD).unwrap() / total_normalized;
    // raw = WAD / u64::MAX = 1e18 / ~1.8e19 = 0 (integer division)
    assert_eq!(raw, 0, "WAD / u64::MAX must floor to 0 in integer math");

    let capped = if raw > WAD { WAD } else { raw };
    let factor = if capped < 1 { 1 } else { capped };

    // Pin exact settlement_factor: must be exactly 1 (minimum floor)
    assert_eq!(
        factor, 1,
        "extreme underfunding settlement must floor to exactly 1"
    );
}

#[test]
fn stress_settlement_equal_available_and_normalized() {
    for val in [1u128, 1_000_000, u128::from(u64::MAX)] {
        let raw = val.checked_mul(WAD).unwrap() / val;
        // Pin exact settlement_factor: when available == total, raw must equal WAD
        assert_eq!(
            raw, WAD,
            "equal available ({}) and normalized must yield raw = WAD = {}",
            val, WAD
        );
        let capped = if raw > WAD { WAD } else { raw };
        assert_eq!(
            capped, WAD,
            "equal available ({}) and normalized must yield settlement_factor = WAD = {}",
            val, WAD
        );
    }
}

// ===========================================================================
// 3d: Fee computation with max rates and large supply
// ===========================================================================

#[test]
fn stress_fee_max_rates() {
    // 100% annual interest, 100% fee rate, max realistic supply
    let supply = 1_000_000_000_000_000u128; // 1B USDC in base units (1e15)

    let mut market = make_market(
        10_000, // 100% annual
        i64::MAX,
        WAD,
        supply,
        0,
        0,
    );
    let config = make_config(10_000); // 100% of interest goes to fees

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);

    // Must succeed for 1B USDC supply with max rates
    assert!(
        result.is_ok(),
        "should succeed for 1B USDC supply with max rates"
    );

    // Pin exact expected values
    let expected_sf = expected_scale_factor(WAD, 10_000, SECONDS_PER_YEAR as i64)
        .expect("expected sf should fit");
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "sf must match daily-compound oracle = {}",
        expected_sf
    );

    let expected_fee =
        expected_fee_delta(supply, WAD, 10_000, 10_000, SECONDS_PER_YEAR as i64)
            .expect("expected fee should fit");
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fee,
        "max-rate fee must match daily-compound oracle = {}",
        expected_fee
    );
    assert_eq!(
        market.scaled_total_supply(),
        supply,
        "supply must be unchanged"
    );
}

#[test]
fn stress_fee_near_u64_overflow() {
    // Find a supply that makes fee_normalized close to u64::MAX
    // fee = supply * new_sf / WAD * fee_delta / WAD
    // At 100% annual + 100% fee: fee ≈ supply * 2 * 1 = 2 * supply
    // u64::MAX / 2 ≈ 9.2e18, so supply ≈ 9.2e18 base units = 9.2T USDC
    let supply = u128::from(u64::MAX / 4); // ~4.6e18

    let mut market = make_market(10_000, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(10_000);

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);

    // Should either succeed or return MathOverflow, not panic
    match result {
        Ok(()) => {
            assert!(
                market.accrued_protocol_fees() > 0,
                "fees must be non-zero on success"
            );
            // State consistency: sf must have been updated
            let expected_sf = expected_scale_factor(WAD, 10_000, SECONDS_PER_YEAR as i64)
                .expect("expected sf should fit");
            assert_eq!(
                market.scale_factor(),
                expected_sf,
                "sf must match daily-compound oracle for 100% annual"
            );
        },
        Err(e) => {
            // Must be exact MathOverflow error code + state unchanged
            assert_eq!(
                e,
                pinocchio::error::ProgramError::Custom(41),
                "overflow must be MathOverflow (Custom(41)), got {:?}",
                e
            );
            assert_eq!(
                market.scale_factor(),
                WAD,
                "sf must be unchanged after overflow"
            );
            assert_eq!(
                market.accrued_protocol_fees(),
                0,
                "fees must be unchanged after overflow"
            );
        },
    }
}

// ===========================================================================
// 3e: Payout computation chain at extreme values
// ===========================================================================

#[test]
fn stress_payout_max_scaled_balance() {
    // Large scaled balance with moderate scale_factor and full settlement
    let scaled_balance = u128::MAX / (2 * WAD); // max that doesn't overflow * sf
    let scale_factor = WAD; // 1x
    let settlement = WAD; // full

    // Pin exact payout: at sf=WAD, settlement=WAD, payout == scaled_balance
    let normalized = scaled_balance * scale_factor / WAD;
    assert_eq!(
        normalized, scaled_balance,
        "normalized must equal scaled_balance at sf=WAD"
    );

    let payout = normalized * settlement / WAD;
    assert_eq!(
        payout, scaled_balance,
        "payout must equal scaled_balance at sf=WAD, settlement=WAD"
    );
}

#[test]
fn stress_payout_with_double_scale_factor() {
    // scaled_balance chosen so that scaled * 2*WAD doesn't overflow
    let scaled_balance = u128::MAX / (4 * WAD);
    let scale_factor = 2 * WAD;
    let settlement = WAD;

    let normalized = scaled_balance
        .checked_mul(scale_factor)
        .expect("should not overflow")
        / WAD;

    let payout = normalized
        .checked_mul(settlement)
        .expect("should not overflow")
        / WAD;

    // Pin exact payout values
    let expected_normalized = scaled_balance * 2;
    assert_eq!(
        normalized, expected_normalized,
        "normalized must be exactly 2x scaled_balance = {}",
        expected_normalized
    );
    assert_eq!(
        payout, expected_normalized,
        "payout must equal normalized at full settlement = {}",
        expected_normalized
    );
}

#[test]
fn stress_payout_with_half_settlement() {
    let scaled_balance: u128 = 1_000_000_000_000; // 1M USDC
    let scale_factor = WAD + WAD / 10; // 1.1x
    let settlement = WAD / 2; // 50% settlement

    // Pin exact payout chain values
    const EXPECTED_NORMALIZED: u128 = 1_100_000_000_000; // 1M * 1.1
    const EXPECTED_PAYOUT: u128 = 550_000_000_000; // 1.1M * 0.5

    let normalized = scaled_balance * scale_factor / WAD;
    assert_eq!(
        normalized, EXPECTED_NORMALIZED,
        "normalized must be exactly {} (1M * 1.1x)",
        EXPECTED_NORMALIZED
    );

    let payout = normalized * settlement / WAD;
    assert_eq!(
        payout, EXPECTED_PAYOUT,
        "payout must be exactly {} (1.1M * 50%)",
        EXPECTED_PAYOUT
    );
}

// ===========================================================================
// Composite stress: Full lifecycle at extremes
// ===========================================================================

#[test]
fn stress_full_lifecycle_large_amounts() {
    let config = make_config(5000); // 50% fee rate
    let supply = 100_000_000_000_000u128; // 100M USDC in base units

    // Start market
    let mut market = make_market(
        5000, // 50% annual
        2 * SECONDS_PER_YEAR as i64,
        WAD,
        supply,
        0,
        0,
    );

    // Accrue for 1 year
    let r = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert!(r.is_ok(), "should handle 100M USDC at 50% annual");

    let expected_sf =
        expected_scale_factor(WAD, 5000, SECONDS_PER_YEAR as i64).expect("expected sf should fit");
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "sf must match daily-compound oracle = {}",
        expected_sf
    );

    let expected_fee = expected_fee_delta(supply, WAD, 5000, 5000, SECONDS_PER_YEAR as i64)
        .expect("expected fee should fit");
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fee,
        "fee must match daily-compound oracle = {}",
        expected_fee
    );

    // Simulate settlement
    let vault_balance = 80_000_000_000_000u128; // 80M left (some borrowed)
    let fees = u128::from(market.accrued_protocol_fees());
    let fees_reserved = fees.min(vault_balance);
    let available = vault_balance - fees_reserved;

    // Pin exact intermediate values
    assert_eq!(
        fees_reserved,
        u128::from(expected_fee),
        "fees_reserved must equal accrued fees"
    );
    let expected_available = vault_balance - u128::from(expected_fee);
    assert_eq!(
        available, expected_available,
        "available must be vault - fees"
    );

    let total_normalized = supply * market.scale_factor() / WAD;
    let expected_total_norm = supply * expected_sf / WAD;
    assert_eq!(
        total_normalized, expected_total_norm,
        "total_normalized must match daily-compound oracle = {}",
        expected_total_norm
    );

    let raw_factor = available * WAD / total_normalized;
    let settlement = raw_factor.min(WAD).max(1);

    // Settlement factor should be < WAD (underfunded)
    assert!(settlement < WAD, "should be underfunded");
    assert!(settlement > 0, "should have non-zero settlement");

    // Conservation check: total payout across all lenders must not exceed available
    let total_payout = total_normalized * settlement / WAD;
    assert!(
        total_payout <= available,
        "total payout {} must not exceed available {}",
        total_payout,
        available
    );

    // Verify individual lender payout
    let lender_scaled = supply / 10; // 10% of pool
    let lender_normalized = lender_scaled * market.scale_factor() / WAD;
    let lender_payout = lender_normalized * settlement / WAD;

    // Payout must be positive and less than original deposit amount
    assert!(lender_payout > 0, "lender payout must be positive");
    assert!(
        lender_payout < supply / 10,
        "underfunded payout {} should be less than deposit {}",
        lender_payout,
        supply / 10
    );
}

/// Multiple sequential interest accruals don't accumulate errors.
#[test]
fn stress_many_sequential_accruals() {
    let config = make_config(1000); // 10% fee

    let mut market = make_market(
        5000, // 50% annual
        i64::MAX,
        WAD,
        10_000_000_000_000u128, // 10M USDC
        0,
        0,
    );

    // Accrue 365 times (once per day for a year)
    let seconds_per_day = 86_400i64;
    for day in 1..=365 {
        let ts = seconds_per_day * day;
        let result = accrue_interest(&mut market, &config, ts);
        assert!(
            result.is_ok(),
            "daily accrual failed on day {}: {:?}",
            day,
            result
        );
        assert!(
            market.scale_factor() >= WAD,
            "scale_factor dropped below WAD on day {}",
            day
        );
    }

    // After 365 daily compounds at 50% annual:
    // Compound factor should be > 1.5 * WAD (compound > simple)
    let simple_sf = WAD + WAD / 2;
    assert!(
        market.scale_factor() > simple_sf,
        "365 daily compounds should exceed simple interest: got {} vs simple {}",
        market.scale_factor(),
        simple_sf
    );

    // Pin: compound factor must be within expected range.
    // Analytical: e^0.5 ~ 1.6487, so sf should be ~1.6487 * WAD.
    // With daily compounding: (1 + 0.5/365)^365 ~ 1.6480 * WAD.
    // Integer truncation reduces it slightly. Bound: [1.64 * WAD, 1.66 * WAD].
    let lower_bound = WAD + WAD * 64 / 100; // 1.64 * WAD
    let upper_bound = WAD + WAD * 66 / 100; // 1.66 * WAD
    assert!(
        market.scale_factor() >= lower_bound,
        "365-day compound sf {} below lower bound {} (1.64*WAD)",
        market.scale_factor(),
        lower_bound
    );
    assert!(
        market.scale_factor() <= upper_bound,
        "365-day compound sf {} above upper bound {} (1.66*WAD)",
        market.scale_factor(),
        upper_bound
    );

    // Fees should have accumulated substantially
    assert!(
        market.accrued_protocol_fees() > 0,
        "fees should accumulate over 365 accruals"
    );

    // Pin: last_accrual must reflect final timestamp
    assert_eq!(
        market.last_accrual_timestamp(),
        seconds_per_day * 365,
        "last_accrual must be day 365"
    );

    // Pin: supply must be unchanged (accrue_interest never modifies supply)
    assert_eq!(
        market.scaled_total_supply(),
        10_000_000_000_000u128,
        "supply must be unchanged after accruals"
    );
}

/// Verify that extreme existing fees don't break further accrual.
#[test]
fn stress_accrual_with_max_existing_fees() {
    let existing_fees = u64::MAX - 1_000_000; // near max

    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000u128, 0, existing_fees);
    let config = make_config(500);

    // If new fees would overflow u64 when added to existing, should return MathOverflow
    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);

    // Must not panic — either succeeds (fees fit) or errors cleanly
    match result {
        Ok(()) => {
            assert!(
                market.accrued_protocol_fees() >= existing_fees,
                "fees must not decrease: got {} < {}",
                market.accrued_protocol_fees(),
                existing_fees
            );
            // Scale factor must have been updated
            assert!(
                market.scale_factor() > WAD,
                "sf must increase after successful accrual"
            );
        },
        Err(e) => {
            // Must be exact MathOverflow error + state unchanged
            assert_eq!(
                e,
                pinocchio::error::ProgramError::Custom(41),
                "overflow must be MathOverflow (Custom(41)), got {:?}",
                e
            );
            assert_eq!(
                market.accrued_protocol_fees(),
                existing_fees,
                "fees must be unchanged after overflow"
            );
        },
    }
}
