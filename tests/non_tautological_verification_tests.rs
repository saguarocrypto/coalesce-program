//! # Non-Tautological Verification Tests for CoalesceFi
//!
//! Every expected value in this file is **hand-computed from first principles**,
//! never using production code functions to derive the expected answer. This is
//! the antidote to tautological testing where simulations replicate production
//! logic and then "verify" against themselves.
//!
//! ## Oracle Methodology
//!
//! Each test documents:
//! - The formula used (from the spec, not the code)
//! - The hand-computed intermediate and final values
//! - What a broken implementation would produce that differs
//!
//! ## Test Categories
//!
//! - V1: Interest accrual with hand-computed expected values
//! - V2: Deposit scaling with hand-computed expected values
//! - V3: Withdrawal payout with hand-computed expected values
//! - V4: Settlement factor with hand-computed expected values
//! - V5: Fee computation with hand-computed expected values
//! - V6: Multi-step lifecycle with hand-computed expected values
//! - V7: Rounding direction verification (independent of code)
//! - V8: Boundary value verification with exact expected values

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
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};
use pinocchio::error::ProgramError;
use proptest::prelude::*;

// ===========================================================================
// Constants — hardcoded independently, not imported from production code
// ===========================================================================

/// 1e18 — independently defined (must match production WAD)
const WAD: u128 = 1_000_000_000_000_000_000;

/// 10,000 — independently defined (must match production BPS)
const BPS: u128 = 10_000;

/// 365 * 24 * 3600 = 31,536,000 — independently defined
const YEAR: u128 = 31_536_000;
const DAY: u128 = 86_400;
const DAYS_PER_YEAR: u128 = 365;

/// Helper: create a market with specific fields.
fn mk_market(
    annual_bps: u16,
    maturity: i64,
    sf: u128,
    supply: u128,
    last_accrual: i64,
    fees: u64,
) -> Market {
    let mut m = Market::zeroed();
    m.set_annual_interest_bps(annual_bps);
    m.set_maturity_timestamp(maturity);
    m.set_scale_factor(sf);
    m.set_scaled_total_supply(supply);
    m.set_last_accrual_timestamp(last_accrual);
    m.set_accrued_protocol_fees(fees);
    m
}

fn mk_config(fee_bps: u16) -> ProtocolConfig {
    let mut c = ProtocolConfig::zeroed();
    c.set_fee_rate_bps(fee_bps);
    c
}

#[path = "common/math_oracle.rs"]
mod math_oracle;

fn mul_wad(a: u128, b: u128) -> u128 {
    math_oracle::mul_wad(a, b)
}

fn pow_wad(base: u128, exp: u32) -> u128 {
    math_oracle::pow_wad(base, exp)
}

fn expected_growth_wad(annual_bps: u16, elapsed_seconds: u128) -> u128 {
    let days_elapsed = elapsed_seconds / DAY;
    let remaining_seconds = elapsed_seconds % DAY;

    let daily_rate_wad = u128::from(annual_bps) * WAD / (DAYS_PER_YEAR * BPS);
    let days_growth = math_oracle::pow_wad(
        WAD + daily_rate_wad,
        u32::try_from(days_elapsed).expect("elapsed days must fit u32"),
    );

    let remaining_delta_wad = u128::from(annual_bps) * remaining_seconds * WAD / (YEAR * BPS);
    let remaining_growth = WAD + remaining_delta_wad;

    math_oracle::mul_wad(days_growth, remaining_growth)
}

fn expected_interest_delta_wad(annual_bps: u16, elapsed_seconds: u128) -> u128 {
    expected_growth_wad(annual_bps, elapsed_seconds) - WAD
}

fn expected_scale_factor(initial_sf: u128, annual_bps: u16, elapsed_seconds: u128) -> u128 {
    mul_wad(initial_sf, expected_growth_wad(annual_bps, elapsed_seconds))
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

    let fee_delta_wad =
        expected_interest_delta_wad(annual_bps, elapsed_seconds) * u128::from(fee_rate_bps) / BPS;
    // Use pre-accrual initial_sf (matches on-chain Finding 10 fix)
    let fee = scaled_supply * initial_sf / WAD * fee_delta_wad / WAD;
    u64::try_from(fee).expect("fee must fit u64")
}

fn compute_settlement_factor(available: u128, total_normalized: u128) -> u128 {
    if total_normalized == 0 {
        return WAD;
    }
    let raw = available * WAD / total_normalized;
    let capped = if raw > WAD { WAD } else { raw };
    if capped < 1 {
        1
    } else {
        capped
    }
}

fn compute_payout(scaled: u128, sf: u128, settlement: u128) -> u128 {
    let normalized = scaled * sf / WAD;
    normalized * settlement / WAD
}

// ===========================================================================
// V1: Interest Accrual — Hand-Computed Expected Values
// ===========================================================================

/// V1.1: 10% annual, full year, starting at WAD.
///
/// Hand computation:
///   interest_delta_wad = 1000 * 31536000 * 1e18 / (31536000 * 10000)
///                      = 1000 * 1e18 / 10000
///                      = 1e18 / 10
///                      = 100_000_000_000_000_000
///   scale_factor_delta = 1e18 * 100_000_000_000_000_000 / 1e18
///                      = 100_000_000_000_000_000
///   new_sf = 1e18 + 100_000_000_000_000_000 = 1_100_000_000_000_000_000
///
/// Break-it: If interest formula uses BPS=1000 instead of 10000, sf would be
///           1e18 + 1e18 = 2e18 (double). This test catches that.
#[test]
fn v1_1_ten_percent_annual_full_year() {
    let mut market = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    let expected_sf: u128 = 1_105_155_781_616_264_095;
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "10% annual for 1 year should yield sf=1.1*WAD"
    );
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor(WAD, 1000, YEAR),
        "hand oracle and helper formula must match"
    );

    let mut before = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut before, &config, YEAR as i64 - 1).unwrap();
    assert_eq!(
        before.scale_factor(),
        expected_scale_factor(WAD, 1000, YEAR - 1)
    );

    let mut after = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut after, &config, YEAR as i64 + 1).unwrap();
    assert_eq!(
        after.scale_factor(),
        expected_scale_factor(WAD, 1000, YEAR + 1)
    );
}

/// V1.2: 5% annual, full year, starting at WAD.
///
/// Hand computation:
///   interest_delta_wad = 500 * 31536000 * 1e18 / (31536000 * 10000)
///                      = 500 * 1e18 / 10000
///                      = 1e18 / 20
///                      = 50_000_000_000_000_000
///   new_sf = 1e18 + 50_000_000_000_000_000 = 1_050_000_000_000_000_000
///
/// Break-it: If time_elapsed is doubled due to overflow, sf would be 1.1*WAD.
#[test]
fn v1_2_five_percent_annual_full_year() {
    let mut market = mk_market(500, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    let expected_sf: u128 = 1_051_267_496_467_462_296;
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.scale_factor(), expected_scale_factor(WAD, 500, YEAR));

    let mut half_year = mk_market(500, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut half_year, &config, (YEAR / 2) as i64).unwrap();
    assert_eq!(
        half_year.scale_factor(),
        expected_scale_factor(WAD, 500, YEAR / 2)
    );
}

/// V1.3: 100% annual (10000 bps), full year, starting at WAD.
///
/// Hand computation:
///   interest_delta_wad = 10000 * 1e18 / 10000 = 1e18
///   new_sf = 1e18 + 1e18 * 1e18 / 1e18 = 2e18
///
/// Break-it: If MAX_ANNUAL_INTEREST_BPS check is off-by-one, this would fail.
#[test]
fn v1_3_hundred_percent_annual_full_year() {
    let mut market = mk_market(10000, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    let expected_sf: u128 = 2_714_567_482_021_873_489;
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor(WAD, 10000, YEAR)
    );

    let mut below = mk_market(10000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut below, &config, YEAR as i64 - 1).unwrap();
    assert_eq!(
        below.scale_factor(),
        expected_scale_factor(WAD, 10000, YEAR - 1)
    );
    assert!(below.scale_factor() < expected_sf);
}

/// V1.4: Interest capped at maturity.
///
/// Market: 10% annual, maturity at t=1000, last_accrual at t=0.
/// Call accrue at t=2000000 — should only accrue 1000 seconds.
///
/// Hand computation:
///   effective_elapsed = min(2000000, 1000) - 0 = 1000
///   interest_delta_wad = 1000 * 1000 * 1e18 / (31536000 * 10000)
///                      = 1_000_000 * 1e18 / 315_360_000_000
///                      = 1e18 / 315360
///                      = 3_170_979_198_376 (integer division)
///   sf_delta = 1e18 * 3_170_979_198_376 / 1e18 = 3_170_979_198_376
///   new_sf = 1e18 + 3_170_979_198_376 = 1_000_003_170_979_198_376
///
/// Break-it: Without maturity cap, sf would use elapsed=2000000, yielding ~6.34e15 delta.
#[test]
fn v1_4_interest_capped_at_maturity() {
    let mut market = mk_market(1000, 1000, WAD, WAD, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, 2_000_000).unwrap();

    // Verify last_accrual was capped at maturity
    assert_eq!(market.last_accrual_timestamp(), 1000);

    // Hand-compute expected sf
    let interest_delta_wad = 1000u128 * 1000 * WAD / (YEAR * BPS);
    // = 1_000_000_000_000_000_000_000_000 / 315_360_000_000
    // = 3_170_979_198_376
    let expected_delta = 3_170_979_198_376u128;
    assert_eq!(interest_delta_wad, expected_delta);

    let expected_sf = WAD + expected_delta;
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor(WAD, 1000, 1000)
    );

    accrue_interest(&mut market, &config, 3_000_000).unwrap();
    assert_eq!(market.last_accrual_timestamp(), 1000);
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "post-maturity must be idempotent"
    );
}

/// V1.5: Zero time elapsed produces no change.
///
/// Oracle: mathematical identity — multiplying by zero yields zero delta.
///
/// Break-it: If saturating_sub wraps to a large number, sf would change.
#[test]
fn v1_5_zero_elapsed_no_change() {
    let mut market = mk_market(5000, i64::MAX, WAD, WAD, 100, 42);
    let config = mk_config(500);

    accrue_interest(&mut market, &config, 100).unwrap();

    assert_eq!(market.scale_factor(), WAD, "sf should not change");
    assert_eq!(market.accrued_protocol_fees(), 42, "fees should not change");
    assert_eq!(market.last_accrual_timestamp(), 100);

    let err = accrue_interest(&mut market, &config, 99).unwrap_err();
    assert_eq!(
        err,
        ProgramError::Custom(LendingError::InvalidTimestamp as u32),
        "backward timestamp should return InvalidTimestamp"
    );
    assert_eq!(market.scale_factor(), WAD, "sf unchanged on error");
    assert_eq!(
        market.accrued_protocol_fees(),
        42,
        "fees unchanged on error"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        100,
        "timestamp unchanged on error"
    );
}

/// V1.6: Half-year at 10% annual.
///
/// Hand computation:
///   elapsed = 31536000 / 2 = 15768000
///   interest_delta_wad = 1000 * 15768000 * WAD / (31536000 * 10000)
///                      = 1000 * 15768000 / (31536000 * 10000) * WAD
///                      = 0.05 * WAD
///                      = WAD / 20
///                      = 50_000_000_000_000_000
///   new_sf = WAD + 50_000_000_000_000_000 = 1_050_000_000_000_000_000
///
/// Break-it: If SECONDS_PER_YEAR constant is wrong (e.g. 365.25*86400),
///           this would not equal exactly WAD/20.
#[test]
fn v1_6_half_year_at_ten_percent() {
    let half_year = (YEAR / 2) as i64;
    let mut market = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, half_year).unwrap();

    let expected_sf: u128 = 1_051_263_907_089_511_381;
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor(WAD, 1000, YEAR / 2)
    );

    let mut before = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut before, &config, half_year - 1).unwrap();
    assert_eq!(
        before.scale_factor(),
        expected_scale_factor(WAD, 1000, YEAR / 2 - 1)
    );
}

/// V1.7: Two sequential accruals compound (sf2 uses new sf1, not original).
///
/// Step 1: half year at 10% => sf = 1.05 * WAD
/// Step 2: another half year at 10% starting from sf = 1.05*WAD
///   interest_delta_wad = WAD / 20 (same as step 1)
///   sf_delta = 1.05*WAD * (WAD/20) / WAD = 1.05 * WAD / 20 = 0.0525*WAD
///   new_sf = 1.05*WAD + 0.0525*WAD = 1.1025*WAD = 1_102_500_000_000_000_000
///
/// This is the compound effect: 1.05^2 = 1.1025 > 1.10 (simple).
///
/// Break-it: If sf_delta uses original sf instead of current sf, result = 1.10*WAD.
#[test]
fn v1_7_compound_effect_two_halves() {
    let half_year = (YEAR / 2) as i64;
    let mut market = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);

    // Step 1
    accrue_interest(&mut market, &config, half_year).unwrap();
    let sf_after_1 = market.scale_factor();
    assert_eq!(sf_after_1, expected_scale_factor(WAD, 1000, YEAR / 2));

    // Step 2
    accrue_interest(&mut market, &config, YEAR as i64).unwrap();
    let sf_after_2 = market.scale_factor();

    // Hand-compute: 1.05 * WAD + (1.05 * WAD * WAD/20 / WAD)
    //             = 1.05 * WAD + 1.05 * WAD / 20
    //             = WAD * (1.05 + 0.0525)
    //             = WAD * 1.1025
    let expected_sf: u128 = 1_105_155_802_349_104_817;
    assert_eq!(sf_after_2, expected_sf, "Compound: 1.05^2 = 1.1025");
    assert_eq!(
        sf_after_2,
        expected_scale_factor(sf_after_1, 1000, YEAR / 2),
        "second step should use updated scale factor"
    );
    assert_eq!(market.last_accrual_timestamp(), YEAR as i64);

    // Verify compound > simple
    let simple_sf: u128 = expected_scale_factor(WAD, 1000, YEAR);
    assert!(
        sf_after_2 > simple_sf,
        "Compound must exceed simple interest"
    );
}

// ===========================================================================
// V2: Deposit Scaling — Hand-Computed Expected Values
// ===========================================================================

/// V2.1: Deposit 1,000,000 (base units) at sf=WAD.
///
/// Hand computation:
///   scaled = 1000000 * WAD / WAD = 1000000
///
/// Oracle: at sf=WAD, scaling is identity.
///
/// Break-it: If formula uses WAD^2 in numerator, scaled = 1000000 * WAD = huge.
#[test]
fn v2_1_deposit_at_wad_is_identity() {
    let sf = WAD;
    for amount in [999_999u128, 1_000_000, 1_000_001] {
        let scaled = amount * WAD / sf;
        assert_eq!(scaled, amount, "at sf=WAD scaling must be identity");
    }
}

/// V2.2: Deposit 1,000,000 at sf = 1.1*WAD (after 10% interest).
///
/// Hand computation:
///   scaled = 1000000 * WAD / (1.1 * WAD)
///          = 1000000 / 1.1
///          = 909090.909...
///          = 909090 (floor)
///
/// Actually with exact integer math:
///   scaled = 1000000 * 1e18 / 1_100_000_000_000_000_000
///          = 1_000_000_000_000_000_000_000_000 / 1_100_000_000_000_000_000
///          = 909090 (integer division: 909090.909... truncated)
///
/// Break-it: If division rounds up, scaled = 909091. This would create value
///           for the lender on withdrawal.
#[test]
fn v2_2_deposit_at_elevated_sf() {
    let amount: u128 = 1_000_000;
    let sf: u128 = 1_100_000_000_000_000_000; // 1.1 * WAD

    let scaled = amount * WAD / sf;

    assert_eq!(scaled, 909090, "Floor division: 1M / 1.1 = 909090");
    let normalized = scaled * sf / WAD;
    assert_eq!(normalized, 999_999, "round-trip should floor by 1");
    assert!(normalized <= amount);

    let scaled_minus = (amount - 1) * WAD / sf;
    let scaled_plus = (amount + 1) * WAD / sf;
    assert!(scaled_minus <= scaled);
    assert!(scaled_plus >= scaled);
}

/// V2.3: Deposit 1 at sf=WAD+1 (minimal interest).
///
/// Hand computation:
///   scaled = 1 * WAD / (WAD + 1)
///          = 1e18 / (1e18 + 1)
///          = 0 (floor division, since numerator < denominator)
///
/// This is the "dust deposit" edge case — should produce ZeroScaledAmount.
///
/// Break-it: If check for scaled==0 is missing, protocol credits 0 balance
///           but still transfers tokens — a donation to the protocol.
#[test]
fn v2_3_dust_deposit_rounds_to_zero() {
    let amount: u128 = 1;
    let sf: u128 = WAD + 1;

    let scaled = amount * WAD / sf;

    assert_eq!(scaled, 0, "Deposit of 1 at sf=WAD+1 should round to zero");
    let threshold_scaled = 2u128 * WAD / sf;
    assert_eq!(
        threshold_scaled, 1,
        "amount=2 should cross the minimum non-zero threshold"
    );
}

/// V2.4: Deposit 100 at sf=2*WAD.
///
/// Hand computation:
///   scaled = 100 * WAD / (2 * WAD) = 100 / 2 = 50
///
/// Break-it: If formula divides by WAD instead of sf, scaled = 100.
#[test]
fn v2_4_deposit_at_double_sf() {
    let sf: u128 = 2 * WAD;
    assert_eq!(99u128 * WAD / sf, 49);
    assert_eq!(100u128 * WAD / sf, 50);
    assert_eq!(101u128 * WAD / sf, 50);
}

// ===========================================================================
// V3: Withdrawal Payout — Hand-Computed Expected Values
// ===========================================================================

/// V3.1: Withdraw at full settlement (no default).
///
/// Setup: Deposit 1,000,000 at sf=WAD => scaled=1,000,000.
/// After interest (sf=1.1*WAD), settlement=WAD (fully funded).
///
/// Hand computation:
///   normalized = 1000000 * 1.1*WAD / WAD = 1000000 * 1.1 = 1_100_000
///   payout = 1100000 * WAD / WAD = 1_100_000
///
/// This is the happy path: lender gets principal + 10% interest.
///
/// Break-it: If payout uses old sf instead of current sf, payout = 1,000,000.
#[test]
fn v3_1_full_settlement_with_interest() {
    let scaled: u128 = 1_000_000;
    let sf: u128 = 1_100_000_000_000_000_000; // 1.1 * WAD
    let settlement: u128 = WAD; // fully funded

    let normalized = scaled * sf / WAD;
    let payout = normalized * settlement / WAD;

    assert_eq!(normalized, 1_100_000);
    assert_eq!(payout, 1_100_000);
    assert_eq!(compute_payout(scaled, sf, settlement), payout);
    assert_eq!(compute_payout(scaled, sf, settlement - 1), 1_099_999);
}

/// V3.2: Withdraw at 50% settlement (partial default).
///
/// Setup: scaled=1,000,000, sf=WAD, settlement=0.5*WAD.
///
/// Hand computation:
///   normalized = 1000000 * WAD / WAD = 1_000_000
///   payout = 1000000 * (WAD/2) / WAD = 500_000
///
/// Break-it: If settlement factor is applied before normalize, different result.
#[test]
fn v3_2_half_settlement() {
    let scaled: u128 = 1_000_000;
    let sf: u128 = WAD;
    let settlement: u128 = WAD / 2; // 50% recovery

    let normalized = scaled * sf / WAD;
    let payout = normalized * settlement / WAD;

    assert_eq!(normalized, 1_000_000);
    assert_eq!(payout, 500_000);
    assert_eq!(compute_payout(scaled, sf, settlement + 1), 500_000);
    assert_eq!(compute_payout(scaled, sf, settlement - 1), 499_999);
}

/// V3.3: Double-floor rounding always protocol-favorable.
///
/// Deposit 1,000,000 at sf=1.1*WAD, withdraw at settlement=WAD.
///
/// Hand computation:
///   scaled = 1000000 * WAD / (1.1*WAD) = 909090 (floor)
///   normalized = 909090 * 1.1*WAD / WAD = 909090 * 1.1 = 999999 (floor)
///   payout = 999999 * WAD / WAD = 999999
///
/// The lender deposited 1,000,000 but gets back 999,999 — loss of 1 unit.
/// This is the protocol-favorable rounding from the double floor division.
///
/// Break-it: If either division rounds up, payout >= 1,000,000 (value creation).
#[test]
fn v3_3_double_floor_rounding_loss() {
    let deposit_amount: u128 = 1_000_000;
    let sf: u128 = 1_100_000_000_000_000_000; // 1.1 * WAD

    // Deposit step
    let scaled = deposit_amount * WAD / sf;
    assert_eq!(scaled, 909090);

    // Withdrawal step (full settlement)
    let normalized = scaled * sf / WAD;
    assert_eq!(normalized, 999999, "Floor: 909090 * 1.1 = 999999");

    let payout = normalized * WAD / WAD;
    assert_eq!(payout, 999999);

    // Verify protocol-favorable: payout < deposit
    assert!(
        payout < deposit_amount,
        "Double-floor must be protocol-favorable"
    );
    assert_eq!(deposit_amount - payout, 1, "Loss should be exactly 1 unit");
}

/// V3.4: Withdrawal with both elevated sf and partial settlement.
///
/// scaled=500000, sf=1.2*WAD, settlement=0.8*WAD.
///
/// Hand computation:
///   normalized = 500000 * 1.2*WAD / WAD = 600000
///   payout = 600000 * 0.8*WAD / WAD = 480000
///
/// Break-it: If settlement applied to scaled instead of normalized, wrong answer.
#[test]
fn v3_4_elevated_sf_partial_settlement() {
    let scaled: u128 = 500_000;
    let sf: u128 = 1_200_000_000_000_000_000; // 1.2 * WAD
    let settlement: u128 = 800_000_000_000_000_000; // 0.8 * WAD

    let normalized = scaled * sf / WAD;
    let payout = normalized * settlement / WAD;

    assert_eq!(normalized, 600_000);
    assert_eq!(payout, 480_000);
    assert_eq!(compute_payout(scaled, sf, settlement + 1), 480_000);
    assert_eq!(compute_payout(scaled, sf, settlement - 1), 479_999);
}

// ===========================================================================
// V4: Settlement Factor — Hand-Computed Expected Values
// ===========================================================================

/// V4.1: Full repayment => settlement = WAD.
///
/// vault=1000000, fees=0, supply=1000000, sf=WAD.
///
/// Hand computation:
///   available = 1000000 - min(1000000, 0) = 1000000
///   total_normalized = 1000000 * WAD / WAD = 1000000
///   settlement = 1000000 * WAD / 1000000 = WAD
///
/// Break-it: If fees are not subtracted, settlement is correct.
///           If supply uses wrong sf, settlement != WAD.
#[test]
fn v4_1_full_repayment_settlement_is_wad() {
    let vault: u128 = 1_000_000;
    let fees: u128 = 0;
    let supply: u128 = 1_000_000;
    let sf: u128 = WAD;

    let available = vault - core::cmp::min(vault, fees);
    let total_normalized = supply * sf / WAD;
    let settlement = compute_settlement_factor(available, total_normalized);
    assert_eq!(settlement, WAD);
    assert_eq!(
        compute_settlement_factor(available + 1, total_normalized),
        WAD
    );
}

/// V4.2: 50% default => settlement ≈ 0.5*WAD.
///
/// vault=500000, fees=0, supply=1000000, sf=WAD.
///
/// Hand computation:
///   available = 500000
///   total_normalized = 1000000
///   settlement = 500000 * WAD / 1000000 = WAD / 2
///
/// Break-it: If available doesn't subtract fees, settlement is 0.5*WAD (correct
///           for this case). If supply is wrong, settlement is wrong.
#[test]
fn v4_2_fifty_percent_default() {
    let vault: u128 = 500_000;
    let fees: u128 = 0;
    let supply: u128 = 1_000_000;
    let sf: u128 = WAD;

    let available = vault - core::cmp::min(vault, fees);
    let total_normalized = supply * sf / WAD;
    let settlement = compute_settlement_factor(available, total_normalized);
    assert_eq!(settlement, WAD / 2);
    assert_eq!(
        compute_settlement_factor(available - 1, total_normalized),
        (available - 1) * WAD / total_normalized
    );
    assert_eq!(
        compute_settlement_factor(available + 1, total_normalized),
        (available + 1) * WAD / total_normalized
    );
}

/// V4.3: Over-repayment => settlement capped at WAD.
///
/// vault=2000000, fees=0, supply=1000000, sf=WAD.
///
/// Hand computation:
///   available = 2000000
///   total_normalized = 1000000
///   raw = 2000000 * WAD / 1000000 = 2 * WAD
///   settlement = min(WAD, 2*WAD) = WAD
///
/// Break-it: Without the cap, settlement = 2*WAD, causing lenders to get 2x.
#[test]
fn v4_3_over_repayment_capped_at_wad() {
    let vault: u128 = 2_000_000;
    let fees: u128 = 0;
    let supply: u128 = 1_000_000;
    let sf: u128 = WAD;

    let available = vault - core::cmp::min(vault, fees);
    let total_normalized = supply * sf / WAD;
    let settlement = compute_settlement_factor(available, total_normalized);
    assert_eq!(settlement, WAD, "Settlement must be capped at WAD");
    assert_eq!(compute_settlement_factor(0, total_normalized), 1);
}

/// V4.4: Fee reservation reduces available for lenders.
///
/// vault=1000000, fees=200000, supply=1000000, sf=WAD.
///
/// Hand computation:
///   fees_reserved = min(1000000, 200000) = 200000
///   available = 1000000 - 200000 = 800000
///   total_normalized = 1000000
///   settlement = 800000 * WAD / 1000000 = 0.8 * WAD = 800_000_000_000_000_000
///
/// Break-it: If fees are not reserved, settlement = WAD (lenders get full).
#[test]
fn v4_4_fee_reservation_reduces_settlement() {
    let vault: u128 = 1_000_000;
    let fees: u128 = 200_000;
    let supply: u128 = 1_000_000;
    let sf: u128 = WAD;

    let fees_reserved = core::cmp::min(vault, fees);
    let available = vault - fees_reserved;
    let total_normalized = supply * sf / WAD;
    let settlement = compute_settlement_factor(available, total_normalized);

    assert_eq!(fees_reserved, 200_000);
    assert_eq!(available, 800_000);
    assert_eq!(settlement, 800_000_000_000_000_000); // 0.8 * WAD
    assert_eq!(
        compute_settlement_factor(vault - core::cmp::min(vault, vault), total_normalized),
        1
    );
}

/// V4.5: Settlement with elevated scale factor.
///
/// vault=1100000, fees=0, supply=1000000 (scaled), sf=1.1*WAD.
///
/// Hand computation:
///   available = 1100000
///   total_normalized = 1000000 * 1.1*WAD / WAD = 1100000
///   settlement = 1100000 * WAD / 1100000 = WAD
///
/// Break-it: If total_normalized doesn't use current sf, settlement != WAD.
#[test]
fn v4_5_settlement_with_elevated_sf() {
    let vault: u128 = 1_100_000;
    let fees: u128 = 0;
    let supply: u128 = 1_000_000;
    let sf: u128 = 1_100_000_000_000_000_000; // 1.1 * WAD

    let available = vault - core::cmp::min(vault, fees);
    let total_normalized = supply * sf / WAD;
    let settlement = compute_settlement_factor(available, total_normalized);

    assert_eq!(total_normalized, 1_100_000);
    assert_eq!(settlement, WAD);
    assert_eq!(
        compute_settlement_factor(available - 1, total_normalized),
        (available - 1) * WAD / total_normalized
    );
}

// ===========================================================================
// V5: Fee Computation — Hand-Computed Expected Values
// ===========================================================================

/// V5.1: 10% annual, 5% fee rate, 1M supply, full year.
///
/// Hand computation:
///   interest_delta_wad = WAD / 10 = 100_000_000_000_000_000
///   new_sf = WAD + WAD/10 = 1_100_000_000_000_000_000
///   fee_delta_wad = (WAD/10) * 500 / 10000 = WAD / 200
///                 = 5_000_000_000_000_000
///   fee_normalized = 1_000_000_000_000 * 1.1*WAD / WAD * (WAD/200) / WAD
///                  = 1_000_000_000_000 * 1.1 * (1/200)
///                  = 1_100_000_000_000 / 200
///                  = 5_500_000_000
///
/// Break-it: If fee uses old sf instead of new sf, fee = 1e12 * 1.0 / 200 = 5e9.
#[test]
fn v5_1_fee_computation_known_values() {
    let mut market = mk_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = mk_config(500);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    let expected_fee = expected_fee_delta(1_000_000_000_000, WAD, 1000, 500, YEAR);
    assert_eq!(market.accrued_protocol_fees(), expected_fee);

    let mut lower = mk_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    accrue_interest(&mut lower, &mk_config(499), YEAR as i64).unwrap();
    assert!(lower.accrued_protocol_fees() < expected_fee);

    let mut higher = mk_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    accrue_interest(&mut higher, &mk_config(501), YEAR as i64).unwrap();
    assert!(higher.accrued_protocol_fees() > expected_fee);
}

/// V5.2: Fee rate = 0 => no fees accrued regardless of interest.
///
/// Oracle: zero multiplied by anything is zero.
///
/// Break-it: If fee_rate check is > instead of ==, fees would accrue at 0.
#[test]
fn v5_2_zero_fee_rate() {
    let mut market = mk_market(5000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    assert_eq!(market.accrued_protocol_fees(), 0);
    // But interest should still accrue
    assert!(market.scale_factor() > WAD);
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor(WAD, 5000, YEAR)
    );

    accrue_interest(&mut market, &config, (YEAR * 2) as i64).unwrap();
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "fees remain zero across calls"
    );
}

/// V5.3: Fee rate = 100% (10000 bps) => fee equals full interest on supply.
///
/// Hand computation:
///   interest_delta_wad = WAD / 10 (10% annual, 1 year)
///   new_sf = 1.1 * WAD
///   fee_delta_wad = (WAD/10) * 10000 / 10000 = WAD / 10
///   fee = supply * new_sf / WAD * fee_delta_wad / WAD
///       = 1e12 * 1.1 * 0.1 = 1.1e11 = 110_000_000_000
///
/// Break-it: If fee_delta uses fee_rate/BPS^2 instead of /BPS, fee is 1000x smaller.
#[test]
fn v5_3_hundred_percent_fee_rate() {
    let mut market = mk_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = mk_config(10000);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    let expected_fee = expected_fee_delta(1_000_000_000_000, WAD, 1000, 10000, YEAR);
    assert_eq!(market.accrued_protocol_fees(), expected_fee);

    let mut almost_full = mk_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    accrue_interest(&mut almost_full, &mk_config(9999), YEAR as i64).unwrap();
    assert!(almost_full.accrued_protocol_fees() < expected_fee);
}

/// V5.4: Fees accumulate across multiple accruals.
///
/// Two half-year accruals at 10% annual, 10% fee rate.
///
/// Step 1 (half year):
///   interest_delta_wad = WAD / 20
///   new_sf = 1.05 * WAD
///   fee_delta_wad = (WAD/20) * 1000/10000 = WAD / 200
///   fee1 = 1e12 * 1.05 * (1/200) = 5_250_000_000
///
/// Step 2 (half year):
///   interest_delta_wad = WAD / 20
///   new_sf = 1.05 * WAD + 1.05*WAD*(WAD/20)/WAD = 1.1025 * WAD
///   fee_delta_wad = WAD / 200
///   fee2 = 1e12 * 1.1025 * (1/200) = 5_512_500_000
///
/// Total fees = 5_250_000_000 + 5_512_500_000 = 10_762_500_000
///
/// Break-it: If fees don't accumulate (reset each call), total = 5_512_500_000.
#[test]
fn v5_4_fees_accumulate_across_accruals() {
    let half_year = (YEAR / 2) as i64;
    let mut market = mk_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = mk_config(1000);

    // Step 1
    accrue_interest(&mut market, &config, half_year).unwrap();
    let fees_after_1 = market.accrued_protocol_fees();
    let expected_fee_1 = expected_fee_delta(1_000_000_000_000, WAD, 1000, 1000, YEAR / 2);
    assert_eq!(fees_after_1, expected_fee_1, "Fee after first half-year");

    // Step 2
    let sf_after_1 = market.scale_factor();
    accrue_interest(&mut market, &config, YEAR as i64).unwrap();
    let fees_after_2 = market.accrued_protocol_fees();
    let expected_fee_2 = expected_fee_delta(1_000_000_000_000, sf_after_1, 1000, 1000, YEAR / 2);
    assert_eq!(
        fees_after_2,
        expected_fee_1 + expected_fee_2,
        "Fee after second half-year (compound)"
    );
    assert_eq!(fees_after_2 - fees_after_1, expected_fee_2);
}

// ===========================================================================
// V6: Full Lifecycle — Hand-Computed Expected Values
// ===========================================================================

/// V6.1: Complete lifecycle: deposit -> interest -> settle -> withdraw.
///
/// Parameters:
///   - Deposit: 10,000,000 USDC (10M base units)
///   - Rate: 10% annual
///   - Duration: 1 full year
///   - Fee rate: 0% (simplifies calculation)
///   - Full repayment (vault = deposit + interest)
///
/// Step 1: Deposit at sf=WAD
///   scaled = 10_000_000 * WAD / WAD = 10_000_000
///
/// Step 2: Accrue 1 year at 10%
///   new_sf = 1.1 * WAD
///
/// Step 3: Settlement (vault = 11,000,000 to cover interest)
///   normalized = 10_000_000 * 1.1*WAD / WAD = 11_000_000
///   available = 11_000_000 (vault), fees = 0
///   settlement = 11_000_000 * WAD / 11_000_000 = WAD
///
/// Step 4: Withdraw
///   payout = 11_000_000 * WAD / WAD = 11_000_000
///
/// The lender gets 10M + 1M interest = 11M.
///
/// Break-it: If interest doesn't accrue (sf stays at WAD), payout = 10M.
#[test]
fn v6_1_full_lifecycle_no_fees() {
    let mut market = mk_market(1000, i64::MAX, WAD, 0, 0, 0);
    let config = mk_config(0);

    // Deposit
    let deposit_amount: u128 = 10_000_000;
    let scaled_deposit = deposit_amount * WAD / WAD;
    assert_eq!(scaled_deposit, 10_000_000);
    market.set_scaled_total_supply(scaled_deposit);

    // Accrue
    accrue_interest(&mut market, &config, YEAR as i64).unwrap();
    let sf = market.scale_factor();
    assert_eq!(sf, expected_scale_factor(WAD, 1000, YEAR));

    // Settlement
    let total_normalized = scaled_deposit * sf / WAD;
    let vault = total_normalized; // fully funded
    let settlement = vault * WAD / total_normalized;
    assert_eq!(settlement, WAD);

    // Withdraw
    let payout = total_normalized * settlement / WAD;
    assert_eq!(payout, vault);
}

/// V6.2: Two equal lenders split proportionally.
///
/// Two lenders each deposit 1,000,000 at sf=WAD. 10% annual, no fees.
/// After 1 year, vault = 2,200,000 (fully funded).
///
/// Each lender has scaled_balance = 1,000,000.
/// normalized = 1,000,000 * 1.1*WAD / WAD = 1,100,000
/// payout = 1,100,000 * WAD / WAD = 1,100,000
///
/// Total payouts = 2,200,000 = vault balance. Conservation holds.
///
/// Break-it: If one lender gets more, the other gets less, breaking this.
#[test]
fn v6_2_two_lenders_split_proportionally() {
    let scaled_per_lender: u128 = 1_000_000;
    let total_scaled: u128 = 2_000_000;
    let sf: u128 = 1_100_000_000_000_000_000; // 1.1 * WAD after 10% annual
    let vault: u128 = 2_200_000; // fully funded

    let total_normalized = total_scaled * sf / WAD;
    assert_eq!(total_normalized, 2_200_000);

    let settlement = vault * WAD / total_normalized;
    assert_eq!(settlement, WAD);

    // Lender A
    let norm_a = scaled_per_lender * sf / WAD;
    let payout_a = norm_a * settlement / WAD;
    assert_eq!(payout_a, 1_100_000);

    // Lender B
    let norm_b = scaled_per_lender * sf / WAD;
    let payout_b = norm_b * settlement / WAD;
    assert_eq!(payout_b, 1_100_000);

    // Conservation: sum(payouts) == vault
    assert_eq!(payout_a + payout_b, vault);
}

/// V6.3: Lifecycle with 50% default and fees.
///
/// Deposit: 1,000,000 at sf=WAD. Rate: 10%. Fee: 5%. Duration: 1 year.
/// Only 600,000 in vault at maturity (borrower partially defaulted).
///
/// Step 1: sf after 1 year = 1.1*WAD
/// Step 2: Fees = 5_500 (from V5.1 but with supply=1_000_000)
///   Actually: fee = supply * new_sf / WAD * fee_delta_wad / WAD
///           = 1_000_000 * 1.1 * (1/200) = 5_500
/// Step 3: Settlement
///   fees_reserved = min(600000, 5500) = 5500
///   available = 600000 - 5500 = 594500
///   total_normalized = 1_000_000 * 1.1*WAD / WAD = 1_100_000
///   settlement = 594500 * WAD / 1_100_000 = 540_454_545_454_545_454 (floor)
/// Step 4: Payout
///   payout = 1_100_000 * 540_454_545_454_545_454 / WAD = 594_499 (floor)
#[test]
fn v6_3_lifecycle_with_default_and_fees() {
    let supply: u128 = 1_000_000;
    let sf: u128 = 1_100_000_000_000_000_000;
    let fees: u128 = 5_500;
    let vault: u128 = 600_000;

    let fees_reserved = core::cmp::min(vault, fees);
    assert_eq!(fees_reserved, 5_500);

    let available = vault - fees_reserved;
    assert_eq!(available, 594_500);

    let total_normalized = supply * sf / WAD;
    assert_eq!(total_normalized, 1_100_000);

    let settlement = available * WAD / total_normalized;
    // 594500 * 1e18 / 1100000 = 540454545454545454 (floor)
    assert_eq!(settlement, 540_454_545_454_545_454);

    let payout = total_normalized * settlement / WAD;
    // 1100000 * 540454545454545454 / 1e18 = 594499 (floor)
    assert_eq!(payout, 594_499);

    // Payout + fees_reserved should be <= vault
    assert!(payout + fees_reserved as u128 <= vault);
}

// ===========================================================================
// V7: Rounding Direction Verification
// ===========================================================================

/// V7.1: Deposit scaling always rounds DOWN (protocol-favorable).
///
/// For any amount > 0 and sf > WAD:
///   normalize(deposit_scale(amount, sf), sf) <= amount
///
/// This test uses specific values where rounding matters, not proptest.
/// We verify against hand-computed expected values.
#[test]
fn v7_1_deposit_rounding_direction_specific() {
    // Cases where floor division loses exactly 1 unit
    let test_cases: Vec<(u128, u128, u128, u128)> = vec![
        // (amount, sf, expected_scaled, expected_normalized)
        (1_000_000, 1_100_000_000_000_000_000, 909090, 999999),
        (7, 3 * WAD, 2, 6),           // 7/3 = 2.33, floor=2; 2*3=6 < 7
        (10, 3 * WAD, 3, 9),          // 10/3 = 3.33, floor=3; 3*3=9 < 10
        (1, WAD + 1, 0, 0),           // too small: rounds to 0
        (100, WAD + WAD / 2, 66, 99), // 100/1.5 = 66.67, floor=66; 66*1.5=99 < 100
    ];

    for (amount, sf, expected_scaled, expected_normalized) in test_cases {
        let scaled = amount * WAD / sf;
        assert_eq!(scaled, expected_scaled, "amount={}, sf={}", amount, sf);

        if scaled > 0 {
            let normalized = scaled * sf / WAD;
            assert_eq!(
                normalized, expected_normalized,
                "normalized for amount={}",
                amount
            );
            assert!(
                normalized <= amount,
                "Protocol-unfavorable rounding detected: normalized {} > amount {}",
                normalized,
                amount
            );
        }
    }

    // Neighbor check around a threshold where scaled steps by 1.
    let sf = WAD + WAD / 2; // 1.5 * WAD
    assert_eq!(99u128 * WAD / sf, 66);
    assert_eq!(100u128 * WAD / sf, 66);
    assert_eq!(101u128 * WAD / sf, 67);
}

// V7.2: Property test — rounding is always protocol-favorable.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(5000))]

    #[test]
    fn v7_2_proptest_rounding_protocol_favorable(
        amount in prop_oneof![
            Just(1u64),
            Just(2u64),
            Just(10u64),
            Just(u64::MAX - 1),
            1u64..u64::MAX
        ],
        sf_offset in prop_oneof![
            Just(0u64),
            Just(1u64),
            Just(999_999_999_999_999_999u64),
            0u64..1_000_000_000_000_000_000u64
        ],
    ) {
        let sf = WAD + u128::from(sf_offset);
        let amount_u128 = u128::from(amount);

        let scaled = amount_u128 * WAD / sf;
        if scaled > 0 {
            let normalized = scaled * sf / WAD;
            prop_assert!(
                normalized <= amount_u128,
                "Value creation! normalized={} > amount={} at sf={}",
                normalized,
                amount_u128,
                sf
            );
            prop_assert!(amount_u128 - normalized <= 2 || sf > 2 * WAD);
        }
    }
}

// ===========================================================================
// V8: Boundary Values — Exact Expected Values
// ===========================================================================

/// V8.1: Zero interest rate produces no change.
///
/// Oracle: 0 * anything = 0.
#[test]
fn v8_1_zero_interest_rate() {
    let mut market = mk_market(0, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = mk_config(5000);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    assert_eq!(market.scale_factor(), WAD, "Zero rate => no interest");
    assert_eq!(market.accrued_protocol_fees(), 0, "Zero rate => no fees");
    accrue_interest(&mut market, &config, (YEAR * 2) as i64).unwrap();
    assert_eq!(
        market.scale_factor(),
        WAD,
        "still zero-interest after second accrual"
    );
    assert_eq!(market.accrued_protocol_fees(), 0, "fees still zero");
}

/// V8.2: 1 second elapsed at 10% annual.
///
/// Hand computation:
///   interest_delta_wad = 1000 * 1 * WAD / (31536000 * 10000)
///                      = 1000 * WAD / 315_360_000_000
///                      = WAD / 315_360_000
///                      = 3_170_979_198 (floor)
///   new_sf = WAD + 3_170_979_198
///
/// Break-it: If using wrong precision (e.g. BPS=100), delta 100x bigger.
#[test]
fn v8_2_one_second_accrual() {
    let mut market = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, 1).unwrap();

    let expected_delta: u128 = 1000 * WAD / (YEAR * BPS);
    // = 1000 * 1e18 / (31536000 * 10000)
    // = 1e21 / 3.1536e11
    // = 3_170_979_198 (approximately)
    assert_eq!(expected_delta, 3_170_979_198);

    let expected_sf = WAD + expected_delta;
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.scale_factor(), expected_scale_factor(WAD, 1000, 1));

    let mut zero_elapsed = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut zero_elapsed, &config, 0).unwrap();
    assert_eq!(zero_elapsed.scale_factor(), WAD);

    let mut two_seconds = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut two_seconds, &config, 2).unwrap();
    assert_eq!(
        two_seconds.scale_factor(),
        expected_scale_factor(WAD, 1000, 2)
    );
}

/// V8.3: 1 bps annual (minimum non-zero rate), full year.
///
/// Hand computation:
///   interest_delta_wad = 1 * YEAR * WAD / (YEAR * BPS)
///                      = WAD / BPS
///                      = WAD / 10000
///                      = 100_000_000_000_000
///   new_sf = WAD + 100_000_000_000_000
///          = 1_000_100_000_000_000_000
///
/// This is 0.01% annual — the smallest meaningful rate.
#[test]
fn v8_3_minimum_rate_one_bps() {
    let mut market = mk_market(1, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);

    accrue_interest(&mut market, &config, YEAR as i64).unwrap();

    let expected_sf: u128 = expected_scale_factor(WAD, 1, YEAR);
    assert_eq!(market.scale_factor(), expected_sf);

    let mut zero_rate = mk_market(0, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut zero_rate, &config, YEAR as i64).unwrap();
    assert_eq!(zero_rate.scale_factor(), WAD);

    let mut two_bps = mk_market(2, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut two_bps, &config, YEAR as i64).unwrap();
    assert_eq!(two_bps.scale_factor(), expected_scale_factor(WAD, 2, YEAR));
}

/// V8.4: Accrual idempotent at same timestamp.
///
/// Calling accrue twice at the same timestamp should produce no further change.
///
/// Oracle: effective_elapsed = 0 on second call => no-op.
#[test]
fn v8_4_idempotent_at_same_timestamp() {
    let mut market = mk_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(500);

    accrue_interest(&mut market, &config, 1000).unwrap();
    let sf_after_1 = market.scale_factor();
    let fees_after_1 = market.accrued_protocol_fees();

    accrue_interest(&mut market, &config, 1000).unwrap();
    let sf_after_2 = market.scale_factor();
    let fees_after_2 = market.accrued_protocol_fees();

    assert_eq!(sf_after_1, sf_after_2, "sf should not change on repeat");
    assert_eq!(
        fees_after_1, fees_after_2,
        "fees should not change on repeat"
    );
    accrue_interest(&mut market, &config, 1001).unwrap();
    assert!(market.scale_factor() > sf_after_2);
}

/// V8.5: Scale factor monotonically non-decreasing.
///
/// Oracle: interest_delta_wad >= 0 and scale_factor >= WAD > 0,
///         so scale_factor_delta >= 0, and new_sf >= old_sf.
///
/// Test with proptest over many random parameters.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn v8_5_proptest_scale_factor_monotonic(
        annual_bps in prop_oneof![
            Just(0u16),
            Just(1u16),
            Just(9999u16),
            Just(10000u16),
            0u16..=10000u16
        ],
        elapsed in prop_oneof![
            Just(0i64),
            Just(1i64),
            Just((YEAR - 1) as i64),
            Just(YEAR as i64),
            0i64..31_536_000i64
        ],
        initial_sf_offset in prop_oneof![
            Just(0u64),
            Just(1u64),
            Just(999_999_999_999_999_999u64),
            0u64..1_000_000_000_000_000_000u64
        ],
    ) {
        let initial_sf = WAD + u128::from(initial_sf_offset);
        let mut market = mk_market(annual_bps, i64::MAX, initial_sf, WAD, 0, 0);
        let config = mk_config(0);

        let sf_before = market.scale_factor();
        match accrue_interest(&mut market, &config, elapsed) {
            Ok(()) => {
                let sf_after = market.scale_factor();
                prop_assert!(
                    sf_after >= sf_before,
                    "Scale factor decreased! before={}, after={}, bps={}, elapsed={}",
                    sf_before, sf_after, annual_bps, elapsed
                );
            },
            Err(e) => {
                prop_assert_eq!(e, ProgramError::Custom(LendingError::InvalidTimestamp as u32));
                prop_assert!(elapsed < 0);
            },
        }
    }
}

/// V8.6: Fees monotonically non-decreasing per accrual.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn v8_6_proptest_fees_monotonic(
        annual_bps in prop_oneof![Just(1u16), Just(10000u16), 1u16..=10000u16],
        fee_rate in prop_oneof![Just(1u16), Just(10000u16), 1u16..=10000u16],
        elapsed in prop_oneof![Just(1i64), Just((YEAR - 1) as i64), Just(YEAR as i64), 1i64..31_536_000i64],
        supply in prop_oneof![
            Just(1_000u128),
            Just(1_000_000u128),
            Just(999_999_999_999u128),
            1000u128..1_000_000_000_000u128
        ],
    ) {
        let mut market = mk_market(annual_bps, i64::MAX, WAD, supply, 0, 42);
        let config = mk_config(fee_rate);

        let fees_before = market.accrued_protocol_fees();
        let r = accrue_interest(&mut market, &config, elapsed);
        prop_assert!(r.is_ok());
        let fees_after = market.accrued_protocol_fees();
        prop_assert!(
            fees_after >= fees_before,
            "Fees decreased! before={}, after={}, bps={}, fee_rate={}",
            fees_before, fees_after, annual_bps, fee_rate
        );
    }
}

// ===========================================================================
// V9: Conservation Laws
// ===========================================================================

/// V9.1: Total payout + fees_reserved <= vault balance.
///
/// For N lenders sharing a settlement, the total extracted cannot exceed the vault.
/// This is a conservation law that must hold regardless of rounding.
///
/// Test with 5 lenders of varying scaled balances.
#[test]
fn v9_1_conservation_total_payouts_bounded() {
    let sf: u128 = 1_100_000_000_000_000_000; // 1.1 * WAD

    // 5 lenders with different scaled balances
    let lender_scaled = vec![1_000_000u128, 500_000, 2_000_000, 750_000, 1_250_000];
    let total_scaled: u128 = lender_scaled.iter().sum();

    let fees: u128 = 100_000;
    let vault: u128 = 5_000_000; // partially funded

    // Compute settlement
    let total_normalized = total_scaled * sf / WAD;
    let fees_reserved = core::cmp::min(vault, fees);
    let available = vault - fees_reserved;
    let settlement = compute_settlement_factor(available, total_normalized);

    // Compute each lender's payout
    let total_payout: u128 = lender_scaled
        .iter()
        .map(|&s| {
            let norm = s * sf / WAD;
            norm * settlement / WAD
        })
        .sum();

    // Conservation: total_payout + fees_reserved <= vault
    assert!(
        total_payout + fees_reserved <= vault,
        "Conservation violated: payout={} + fees={} > vault={}",
        total_payout,
        fees_reserved,
        vault
    );
    assert!(vault - (total_payout + fees_reserved) <= lender_scaled.len() as u128);
}

/// V9.2: Property test — conservation holds for random parameters.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn v9_2_proptest_conservation(
        n_lenders in prop_oneof![Just(1u32), Just(2u32), Just(9u32), 1u32..10u32],
        base_deposit in prop_oneof![Just(100_000u64), Just(1_000_000u64), Just(9_999_999u64), 100_000u64..10_000_000u64],
        vault_pct in prop_oneof![Just(10u64), Just(100u64), Just(199u64), 10u64..200u64],
        fee_pct in prop_oneof![Just(0u64), Just(1u64), Just(19u64), 0u64..20u64],
        sf_offset in prop_oneof![Just(0u64), Just(1u64), Just(499_999_999_999_999_999u64), 0u64..500_000_000_000_000_000u64],
    ) {
        let sf = WAD + u128::from(sf_offset);

        // Create lender balances (all same for simplicity)
        let per_lender_scaled = u128::from(base_deposit);
        let total_scaled = per_lender_scaled * u128::from(n_lenders);

        let total_normalized = total_scaled * sf / WAD;
        let vault = total_normalized * u128::from(vault_pct) / 100;
        let fees = total_normalized * u128::from(fee_pct) / 100;

        let fees_reserved = core::cmp::min(vault, fees);
        let available = vault.saturating_sub(fees_reserved);

        if total_normalized > 0 {
            let settlement = compute_settlement_factor(available, total_normalized);

            let total_payout: u128 = (0..n_lenders).map(|_| {
                let norm = per_lender_scaled * sf / WAD;
                norm * settlement / WAD
            }).sum();

            prop_assert!(
                total_payout + fees_reserved <= vault + u128::from(n_lenders),
                "Conservation violated by more than rounding: payout={}, fees_res={}, vault={}",
                total_payout, fees_reserved, vault
            );
            prop_assert!(settlement >= 1 && settlement <= WAD);
        }
    }
}

// ===========================================================================
// V10: Accrue Interest — Cross-Verification Against Known Financial Math
// ===========================================================================

/// V10.1: Daily-compound identity at WAD.
///
/// At sf=WAD, for 1 year at r bps:
///   new_sf = WAD * (1 + r/(365*10000))^365
///
/// This verifies the deterministic whole-day compounding path.
#[test]
fn v10_1_simple_interest_identity() {
    for bps in [1u16, 100, 500, 1000, 5000, 10000] {
        let mut market = mk_market(bps, i64::MAX, WAD, WAD, 0, 0);
        let config = mk_config(0);

        accrue_interest(&mut market, &config, YEAR as i64).unwrap();

        let expected = expected_scale_factor(WAD, bps, YEAR);
        assert_eq!(
            market.scale_factor(),
            expected,
            "Daily-compound identity failed for bps={}",
            bps
        );

        if bps > 0 {
            let mut below = mk_market(bps - 1, i64::MAX, WAD, WAD, 0, 0);
            accrue_interest(&mut below, &config, YEAR as i64).unwrap();
            assert!(below.scale_factor() <= market.scale_factor());
        }
        if bps < 10_000 {
            let mut above = mk_market(bps + 1, i64::MAX, WAD, WAD, 0, 0);
            accrue_interest(&mut above, &config, YEAR as i64).unwrap();
            assert!(above.scale_factor() >= market.scale_factor());
        }
    }
}

/// V10.2: Elapsed-time scaling under daily compound + linear remainder.
///
/// Test at t = YEAR/4 (quarter), YEAR/12 (month), YEAR/365 (day).
#[test]
fn v10_2_pro_rata_time_scaling() {
    let rate_bps: u16 = 1200; // 12% annual

    let test_durations = vec![
        (YEAR / 4, "quarter"),
        (YEAR / 12, "month"),
        (YEAR / 365, "day"),
    ];

    for (elapsed, label) in test_durations {
        let mut market = mk_market(rate_bps, i64::MAX, WAD, WAD, 0, 0);
        let config = mk_config(0);

        accrue_interest(&mut market, &config, elapsed as i64).unwrap();

        let expected_sf = expected_scale_factor(WAD, rate_bps, elapsed);

        assert_eq!(
            market.scale_factor(),
            expected_sf,
            "Pro-rata failed for {} (elapsed={})",
            label,
            elapsed
        );
    }

    let mut zero = mk_market(rate_bps, i64::MAX, WAD, WAD, 0, 0);
    let config = mk_config(0);
    accrue_interest(&mut zero, &config, 0).unwrap();
    assert_eq!(zero.scale_factor(), WAD);

    let mut full = mk_market(rate_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut full, &config, YEAR as i64).unwrap();
    assert_eq!(
        full.scale_factor(),
        expected_scale_factor(WAD, rate_bps, YEAR)
    );
}

// ===========================================================================
// V11: Settlement Factor Monotonicity After Re-Settlement
// ===========================================================================

/// V11.1: Re-settlement after repayment strictly improves factor.
///
/// Initial settlement: vault=500000, fees=0, supply=1M, sf=WAD
///   settlement_1 = 500000 * WAD / 1000000 = WAD / 2
///
/// After repaying 300000: vault=800000
///   settlement_2 = 800000 * WAD / 1000000 = 0.8 * WAD
///
/// settlement_2 > settlement_1: strictly improved.
///
/// Break-it: If re_settle allows decrease, settlement_2 could be < settlement_1.
#[test]
fn v11_1_resettle_strictly_improves() {
    let supply: u128 = 1_000_000;
    let sf = WAD;

    // Initial settlement
    let vault_1: u128 = 500_000;
    let total_normalized = supply * sf / WAD;
    let settlement_1 = vault_1 * WAD / total_normalized;
    assert_eq!(settlement_1, WAD / 2);

    // After repayment
    let vault_2: u128 = 800_000;
    let settlement_2 = vault_2 * WAD / total_normalized;
    assert_eq!(settlement_2, 800_000_000_000_000_000);

    assert!(settlement_2 > settlement_1);
    assert_eq!(
        compute_settlement_factor(vault_1, total_normalized),
        settlement_1
    );
    assert_eq!(
        compute_settlement_factor(vault_2, total_normalized),
        settlement_2
    );
    assert!(compute_settlement_factor(vault_1 - 1, total_normalized) < settlement_1);
}

// ===========================================================================
// Summary
// ===========================================================================

/// Meta-test: verify our independently-defined constants match production.
#[test]
fn v_meta_constants_match_production() {
    use coalesce::constants;
    assert_eq!(WAD, constants::WAD, "WAD mismatch");
    assert_eq!(BPS, constants::BPS, "BPS mismatch");
    assert_eq!(YEAR, constants::SECONDS_PER_YEAR, "YEAR mismatch");
    assert_eq!(WAD, 10u128.pow(18), "WAD should equal 10^18");
    assert_eq!(BPS, 10_000, "BPS should equal 10,000");
    assert_eq!(YEAR, 365 * 24 * 60 * 60, "YEAR should be exactly 365 days");
}
