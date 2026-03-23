//! Concolic-style path enumeration tests for CoalesceFi core math.
//!
//! Concolic testing combines concrete and symbolic execution to systematically
//! explore every branch in a function. Since full Haybale/KLEE symbolic execution
//! requires LLVM bitcode tooling that may not be available in all environments,
//! these tests manually enumerate every execution path through each critical
//! function, providing the same branch coverage guarantees as a concolic engine.
//!
//! For each function we:
//!   1. Identify every branch point (if/match/checked_*)
//!   2. Build a path condition table as a comment block
//!   3. Write one test per feasible path with:
//!      - A documented branch condition (file:line)
//!      - The minimal input that reaches that path
//!      - Assertions on the expected output AND intermediate values
//!
//! Run: `cargo test --test concolic_tests --features no-entrypoint`

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
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};
use pinocchio::error::ProgramError;
use proptest::prelude::*;

#[path = "common/math_oracle.rs"]
mod math_oracle;

const SECONDS_PER_DAY: i64 = 86_400;

// =========================================================================
// Helpers (mirroring the in-module test helpers, but for integration tests)
// =========================================================================

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

fn mul_wad_oracle(a: u128, b: u128) -> Result<u128, ProgramError> {
    math_oracle::mul_wad_checked(a, b).ok_or(ProgramError::from(LendingError::MathOverflow))
}

fn pow_wad_oracle(base: u128, exp: u32) -> Result<u128, ProgramError> {
    math_oracle::pow_wad_checked(base, exp).ok_or(ProgramError::from(LendingError::MathOverflow))
}

fn growth_factor_wad_oracle(annual_bps: u16, elapsed: i64) -> Result<u128, ProgramError> {
    if elapsed <= 0 {
        return Ok(WAD);
    }
    math_oracle::growth_factor_wad_checked(annual_bps, elapsed)
        .ok_or(ProgramError::from(LendingError::MathOverflow))
}

/// Stand-alone replica of deposit scaling from deposit.rs:98-104.
/// `scaled_amount = amount * WAD / scale_factor`
fn deposit_scale(amount: u64, scale_factor: u128) -> Result<u128, &'static str> {
    let amount_u128 = u128::from(amount);
    let scaled = amount_u128
        .checked_mul(WAD)
        .ok_or("overflow: amount * WAD")?
        .checked_div(scale_factor)
        .ok_or("div-by-zero: scale_factor")?;
    Ok(scaled)
}

/// Inverse: convert scaled amount back to normalized amount.
/// `normalized = scaled * scale_factor / WAD`
fn normalize(scaled: u128, scale_factor: u128) -> Result<u128, &'static str> {
    scaled
        .checked_mul(scale_factor)
        .ok_or("overflow: scaled * scale_factor")?
        .checked_div(WAD)
        .ok_or("div-by-zero: WAD")
}

/// Stand-alone replica of settlement factor computation from withdraw.rs.
/// COAL-C01: uses full vault balance (no fee reservation).
fn compute_settlement_factor(
    vault_balance: u128,
    _accrued_protocol_fees: u128,
    scaled_total_supply: u128,
    scale_factor: u128,
) -> Result<u128, &'static str> {
    let total_normalized = scaled_total_supply
        .checked_mul(scale_factor)
        .ok_or("overflow in total_normalized mul")?
        .checked_div(WAD)
        .ok_or("div by zero in total_normalized")?;

    if total_normalized == 0 {
        return Ok(WAD);
    }

    let raw = vault_balance
        .checked_mul(WAD)
        .ok_or("overflow in raw mul")?
        .checked_div(total_normalized)
        .ok_or("div by zero in raw")?;

    let capped = if raw > WAD { WAD } else { raw };
    let factor = if capped < 1 { 1 } else { capped };

    Ok(factor)
}

/// Stand-alone replica of fee computation from interest.rs:60-91.
fn compute_fee(
    interest_delta_wad: u128,
    fee_rate_bps: u16,
    scaled_total_supply: u128,
    scale_factor_before: u128,
    existing_fees: u64,
) -> Result<u64, &'static str> {
    let fee_rate = u128::from(fee_rate_bps);
    if fee_rate == 0 {
        return Ok(existing_fees);
    }

    let fee_delta_wad = interest_delta_wad
        .checked_mul(fee_rate)
        .ok_or("overflow: interest_delta * fee_rate")?
        .checked_div(BPS)
        .ok_or("div-by-zero: BPS")?;

    // Use pre-accrual scale_factor_before (Finding 10 fix)
    let fee_normalized = scaled_total_supply
        .checked_mul(scale_factor_before)
        .ok_or("overflow: supply * scale_factor")?
        .checked_div(WAD)
        .ok_or("div-by-zero: WAD (1)")?
        .checked_mul(fee_delta_wad)
        .ok_or("overflow: * fee_delta_wad")?
        .checked_div(WAD)
        .ok_or("div-by-zero: WAD (2)")?;

    let fee_u64 = u64::try_from(fee_normalized).map_err(|_| "fee exceeds u64")?;
    let new_fees = existing_fees
        .checked_add(fee_u64)
        .ok_or("overflow: existing + new fees")?;

    Ok(new_fees)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OracleState {
    scale_factor: u128,
    accrued_fees: u64,
    last_accrual_timestamp: i64,
    annual_bps: u16,
    maturity_timestamp: i64,
    scaled_total_supply: u128,
}

fn oracle_accrue_step(
    mut state: OracleState,
    fee_rate_bps: u16,
    current_timestamp: i64,
) -> Result<OracleState, ProgramError> {
    let effective_now = if current_timestamp > state.maturity_timestamp {
        state.maturity_timestamp
    } else {
        current_timestamp
    };

    if effective_now < state.last_accrual_timestamp {
        return Err(LendingError::InvalidTimestamp.into());
    }

    let elapsed = effective_now - state.last_accrual_timestamp;
    if elapsed <= 0 {
        return Ok(state);
    }

    let growth_wad = growth_factor_wad_oracle(state.annual_bps, elapsed)?;
    let new_scale_factor = mul_wad_oracle(state.scale_factor, growth_wad)?;
    let interest_delta_wad = growth_wad
        .checked_sub(WAD)
        .ok_or(LendingError::MathOverflow)?;

    let fee_rate = u128::from(fee_rate_bps);
    if fee_rate > 0 {
        let fee_delta_wad = interest_delta_wad
            .checked_mul(fee_rate)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(BPS)
            .ok_or(LendingError::MathOverflow)?;

        // Use pre-accrual scale_factor (Finding 10 fix)
        let fee_normalized = state
            .scaled_total_supply
            .checked_mul(state.scale_factor)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?
            .checked_mul(fee_delta_wad)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?;

        let fee_u64 = u64::try_from(fee_normalized).map_err(|_| LendingError::MathOverflow)?;
        state.accrued_fees = state
            .accrued_fees
            .checked_add(fee_u64)
            .ok_or(LendingError::MathOverflow)?;
    }

    state.scale_factor = new_scale_factor;
    state.last_accrual_timestamp = effective_now;
    Ok(state)
}

// =========================================================================
// PATH CONDITION TABLE: accrue_interest
// =========================================================================
//
// Source: src/logic/interest.rs, function accrue_interest()
//
// Branch points and paths:
//
//  Branch 1 (line 20-24): effective_now = min(current_timestamp, maturity)
//    Condition A: current_timestamp > maturity  => effective_now = maturity
//    Condition B: current_timestamp <= maturity  => effective_now = current_timestamp
//
//  Branch 2 (line 27-29): time_elapsed <= 0 => early return Ok(())
//    Condition A: effective_now - last_accrual <= 0  => RETURN Ok (no state change)
//    Condition B: effective_now - last_accrual > 0   => CONTINUE
//
//  Branch 3 (line 36-46): interest_delta_wad computation via checked_mul/checked_div
//    Condition A: annual_bps * time_elapsed * WAD does not overflow  => CONTINUE
//    Condition B: overflow at any step => Err(MathOverflow)
//    Note: interest_delta_wad == 0 when annual_bps==0 OR time_elapsed is tiny
//          (floor division truncates to 0)
//
//  Branch 4 (line 49-53): scale_factor_delta = scale_factor * interest_delta_wad / WAD
//    Condition A: multiplication does not overflow => CONTINUE
//    Condition B: overflow => Err(MathOverflow)
//
//  Branch 5 (line 55-57): new_scale_factor = scale_factor + scale_factor_delta
//    Condition A: addition does not overflow => CONTINUE
//    Condition B: overflow => Err(MathOverflow)
//
//  Branch 6 (line 61): fee_rate_bps > 0?
//    Condition A: fee_rate_bps == 0  => SKIP fee block, go to line 93
//    Condition B: fee_rate_bps > 0   => ENTER fee block
//
//  Branch 7 (line 73-81): fee_normalized computation
//    Sub-path: scaled_total_supply == 0 => fee_normalized == 0
//    Sub-path: normal computation succeeds
//    Sub-path: overflow at any step => Err(MathOverflow)
//
//  Branch 8 (line 83-84): u64::try_from(fee_normalized)
//    Condition A: fee_normalized <= u64::MAX => Ok
//    Condition B: fee_normalized > u64::MAX => Err(MathOverflow)
//
//  Branch 9 (line 86-89): existing_fees + fee_normalized_u64 overflow?
//    Condition A: no overflow => CONTINUE
//    Condition B: overflow => Err(MathOverflow)
//
// Composite paths tested below:
//   Path 1: B2A (time_elapsed == 0, early return)
//   Path 2: B1A + B2B (effective_now capped at maturity, interest computed)
//   Path 3: B2B + interest_delta_wad == 0 (tiny rate * tiny time, floor to 0)
//   Path 4: B2B + B3A + B4A (scale_factor_delta computed successfully)
//   Path 5: B2B + B6B + B7(supply==0) (fee path with zero supply)
//   Path 6: B2B + B6B + B7(normal) + B8A (fee fits in u64)
//   Path 7: B2B + B6B + B7(normal) + B8B (fee overflow u64)
//   Path 8: B2B + B6B + B8A + B9B (fee accumulation overflow)
//   Path 9: B2B + B5B (scale_factor overflow)

// =========================================================================
// accrue_interest: Path 1 -- time_elapsed == 0 (early return)
// =========================================================================
// Branch condition: line 27 `if time_elapsed <= 0` => true
// Minimal input: current_timestamp == last_accrual_timestamp
// Expected: Ok(()), no state changes
#[test]
fn concolic_accrue_path1_time_elapsed_zero() {
    let last_accrual = 1_000_000i64;
    let mut market = make_market(
        1000,          // annual_interest_bps: 10%
        2_000_000_000, // maturity far in future
        WAD,           // scale_factor = 1.0
        WAD,           // some supply
        last_accrual,
        42, // existing fees
    );
    let config = make_config(500); // 5% fee rate

    let sf_before = market.scale_factor();
    let fees_before = market.accrued_protocol_fees();
    let ts_before = market.last_accrual_timestamp();

    // current_timestamp == last_accrual => time_elapsed = 0 => early return
    let result = accrue_interest(&mut market, &config, last_accrual);
    assert!(result.is_ok(), "path 1: should return Ok");

    // Verify NO state mutation occurred
    assert_eq!(
        market.scale_factor(),
        sf_before,
        "path 1: scale_factor unchanged"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        fees_before,
        "path 1: fees unchanged"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        ts_before,
        "path 1: timestamp unchanged"
    );
}

// Also verify time_elapsed < 0 (current_ts before last_accrual)
// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn concolic_accrue_path1b_time_elapsed_negative() {
    let last_accrual = 1_000_000i64;
    let mut market = make_market(1000, 2_000_000_000, WAD, WAD, last_accrual, 0);
    let config = make_config(500);
    let sf_before = market.scale_factor();
    let fees_before = market.accrued_protocol_fees();
    let ts_before = market.last_accrual_timestamp();

    // current_timestamp < last_accrual => InvalidTimestamp error (SR-114)
    let result = accrue_interest(&mut market, &config, last_accrual - 100);
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::InvalidTimestamp as u32)),
        "path 1b: error should be InvalidTimestamp"
    );
    assert_eq!(
        market.scale_factor(),
        sf_before,
        "path 1b: scale_factor unchanged on error"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        fees_before,
        "path 1b: fees unchanged"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        ts_before,
        "path 1b: timestamp unchanged"
    );
}

// =========================================================================
// accrue_interest: Path 2 -- effective_now capped at maturity
// =========================================================================
// Branch condition: line 20 `if current_timestamp > maturity` => true
// Minimal input: current_timestamp = maturity + 1, last_accrual < maturity
// Expected: interest computed only for (maturity - last_accrual) seconds
#[test]
fn concolic_accrue_path2_capped_at_maturity() {
    let maturity = 1_000i64;
    let last_accrual = 0i64;
    let current_ts = 999_999i64; // far past maturity

    let mut market = make_market(
        1000, // 10% annual
        maturity,
        WAD,
        WAD,
        last_accrual,
        0,
    );
    let config = make_config(0);

    let result = accrue_interest(&mut market, &config, current_ts);
    assert!(result.is_ok(), "path 2: should succeed");

    // Verify timestamp is capped at maturity, NOT current_ts
    assert_eq!(
        market.last_accrual_timestamp(),
        maturity,
        "path 2: last_accrual_timestamp capped at maturity"
    );

    // Verify interest reflects exactly (maturity - last_accrual) = 1000 seconds
    let time_elapsed = 1000u128;
    let annual_bps = 1000u128;
    let interest_delta_wad = annual_bps * time_elapsed * WAD / (SECONDS_PER_YEAR * BPS);
    let expected_sf = WAD + (WAD * interest_delta_wad / WAD);
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "path 2: scale_factor reflects maturity-capped time"
    );
}

// =========================================================================
// accrue_interest: Path 3 -- interest_delta_wad == 0 (tiny rate * tiny time)
// =========================================================================
// Branch condition: floor division at line 36-46 truncates to 0
// Minimal input: annual_bps = 1, time_elapsed = 1 second
//   interest_delta_wad = 1 * 1 * WAD / (31536000 * 10000)
//                      = 1e18 / 315360000000
//                      = 3170 (non-zero! need even smaller)
// Actually with u128 floor division: 1 * 1 * WAD / (SECONDS_PER_YEAR * BPS)
//   = 1e18 / 315_360_000_000 = 3170
// This is non-zero. For interest_delta_wad to be truly 0, we need
// annual_bps * time_elapsed * WAD < SECONDS_PER_YEAR * BPS
// i.e., annual_bps * time_elapsed < 315_360_000_000 / WAD = 0.000000000315...
// This is impossible for any nonzero integer inputs since annual_bps >= 1
// and time_elapsed >= 1 gives product >= WAD.
//
// Re-analysis: interest_delta_wad = (annual_bps * time_elapsed * WAD) / (SECONDS_PER_YEAR * BPS)
// The denominator = 315_360_000_000_000 (about 3.15e14)
// Numerator = annual_bps * time_elapsed * 1e18
// For numerator < denominator: annual_bps * time_elapsed < 315_360 (approx)
// Actually: SECONDS_PER_YEAR * BPS = 31_536_000 * 10_000 = 315_360_000_000
// So: annual_bps * time_elapsed * WAD / 315_360_000_000
// For this to be 0: annual_bps * time_elapsed * WAD < 315_360_000_000
//   => annual_bps * time_elapsed < 315_360_000_000 / 1e18 = 0.00000031536
// Impossible for integer inputs >= 1.
//
// So interest_delta_wad is always >= 1 when both annual_bps > 0 and time_elapsed > 0.
// But scale_factor_delta = scale_factor * interest_delta_wad / WAD could be 0 if
// scale_factor * interest_delta_wad < WAD.
// With scale_factor = WAD and interest_delta_wad >= 3170 (minimum), we get
// WAD * 3170 / WAD = 3170, still non-zero.
//
// True zero delta requires annual_bps == 0. That makes interest_delta_wad = 0,
// hence scale_factor_delta = 0, new_scale_factor = scale_factor (unchanged).
#[test]
fn concolic_accrue_path3_interest_delta_zero_via_zero_rate() {
    let mut market = make_market(
        0, // annual_bps = 0 => interest_delta_wad = 0
        i64::MAX,
        WAD,
        WAD,
        0,
        0,
    );
    let config = make_config(0);

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert!(result.is_ok(), "path 3: should succeed");

    // interest_delta_wad = 0 * ... = 0
    // scale_factor_delta = WAD * 0 / WAD = 0
    // new_scale_factor = WAD + 0 = WAD
    assert_eq!(
        market.scale_factor(),
        WAD,
        "path 3: scale_factor unchanged when annual_bps == 0"
    );
    // Timestamp still advances
    assert_eq!(
        market.last_accrual_timestamp(),
        SECONDS_PER_YEAR as i64,
        "path 3: timestamp advances even with zero interest"
    );
}

// =========================================================================
// accrue_interest: Path 4 -- scale_factor_delta computation succeeds
// =========================================================================
// Branch condition: lines 49-57 all checked_mul/checked_div succeed
// Minimal input: moderate values (10% rate, 1 year, WAD scale_factor)
#[test]
fn concolic_accrue_path4_scale_factor_delta_success() {
    let annual_bps: u16 = 1000; // 10%
    let time_elapsed = SECONDS_PER_YEAR as i64; // 1 full year
    let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    let result = accrue_interest(&mut market, &config, time_elapsed);
    assert!(result.is_ok(), "path 4: should succeed");

    let growth = growth_factor_wad_oracle(annual_bps, time_elapsed).unwrap();
    let expected_sf = mul_wad_oracle(WAD, growth).unwrap();

    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "path 4: scale_factor follows daily-compound oracle"
    );
}

// =========================================================================
// accrue_interest: Path 5 -- fee path with zero supply
// =========================================================================
// Branch condition: line 61 fee_rate_bps > 0 => true, line 62 scaled_total_supply == 0
// Expected: fee_normalized = 0 * ... = 0, no fee added
#[test]
fn concolic_accrue_path5_fee_zero_supply() {
    let annual_bps: u16 = 1000;
    let time_elapsed = SECONDS_PER_YEAR as i64;
    let mut market = make_market(
        annual_bps,
        i64::MAX,
        WAD,
        0, // scaled_total_supply = 0
        0,
        0,
    );
    let config = make_config(500); // 5% fee rate, but supply = 0

    let result = accrue_interest(&mut market, &config, time_elapsed);
    assert!(result.is_ok(), "path 5: should succeed");

    // fee_normalized = 0 * new_scale_factor / WAD * fee_delta_wad / WAD = 0
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "path 5: zero supply => zero fees regardless of fee_rate"
    );

    // But scale_factor still increases
    assert!(
        market.scale_factor() > WAD,
        "path 5: scale_factor still increases"
    );
}

// =========================================================================
// accrue_interest: Path 6 -- fee path, non-zero supply, fee fits in u64
// =========================================================================
// Branch condition: line 83 u64::try_from(fee_normalized) => Ok
// Minimal input: moderate supply + rate so fee < u64::MAX
#[test]
fn concolic_accrue_path6_fee_fits_u64() {
    let annual_bps: u16 = 1000; // 10%
    let fee_rate_bps: u16 = 500; // 5%
    let time_elapsed = SECONDS_PER_YEAR as i64;
    let supply: u128 = 1_000_000_000_000; // 1M USDC in 6-decimal base
    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(fee_rate_bps);

    let result = accrue_interest(&mut market, &config, time_elapsed);
    assert!(result.is_ok(), "path 6: should succeed");

    // Verify intermediate values via independent oracle.
    let growth = growth_factor_wad_oracle(annual_bps, time_elapsed).unwrap();
    let interest_delta_wad = growth - WAD;
    let new_sf = mul_wad_oracle(WAD, growth).unwrap();
    let fee_delta_wad = interest_delta_wad * 500 / BPS; // 5% of interest
                                                        // Use pre-accrual scale factor (WAD) for fee computation (Finding 10)
    let fee_normalized = supply * WAD / WAD * fee_delta_wad / WAD;

    assert!(
        fee_normalized <= u128::from(u64::MAX),
        "path 6: fee must fit in u64"
    );

    let expected_fee = fee_normalized as u64;
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fee,
        "path 6: fee matches hand-computed value"
    );
    assert!(expected_fee > 0, "path 6: fee is non-zero");
}

// =========================================================================
// accrue_interest: Path 7 -- fee overflow (fee_normalized > u64::MAX)
// =========================================================================
// Branch condition: line 83-84 u64::try_from(fee_normalized) => Err
// Requires: huge supply * scale_factor * fee_delta produces > u64::MAX
// fee_normalized = scaled_total_supply * new_sf / WAD * fee_delta_wad / WAD
// For overflow: need supply * new_sf / WAD * fee_delta_wad / WAD > u64::MAX (1.8e19)
// With supply = u128::MAX/WAD (1.8e20), new_sf = 2*WAD, fee_delta = WAD:
//   1.8e20 * 2 * WAD / WAD^2 = 3.6e20 > u64::MAX => overflow
#[test]
fn concolic_accrue_path7_fee_overflow_u64() {
    // We need a very large supply and high interest to make fee_normalized > u64::MAX.
    // fee_normalized = supply * new_sf / WAD * fee_delta_wad / WAD
    // Let supply = 10^20 (100B USDC scaled tokens), new_sf = 2*WAD, fee_delta = WAD/10
    // = 10^20 * 2*WAD / WAD * (WAD/10) / WAD = 10^20 * 2 * 0.1 = 2*10^19 > u64::MAX
    let huge_supply: u128 = 100_000_000_000_000_000_000; // 10^20
    let annual_bps: u16 = 10000; // 100% annual => interest_delta_wad = WAD
    let fee_rate_bps: u16 = 10000; // 100% fee rate => fee_delta_wad = WAD
    let time_elapsed = SECONDS_PER_YEAR as i64;

    let mut market = make_market(annual_bps, i64::MAX, WAD, huge_supply, 0, 0);
    let config = make_config(fee_rate_bps);
    let before = (
        market.scale_factor(),
        market.accrued_protocol_fees(),
        market.last_accrual_timestamp(),
    );

    let result = accrue_interest(&mut market, &config, time_elapsed);

    // fee_normalized = 10^20 * 2*WAD / WAD * WAD / WAD = 2 * 10^20 > u64::MAX
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::MathOverflow as u32)),
        "path 7: fee_normalized exceeds u64::MAX => MathOverflow"
    );
    assert_eq!(
        market.scale_factor(),
        before.0,
        "path 7: scale_factor unchanged on error"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        before.1,
        "path 7: fees unchanged on error"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        before.2,
        "path 7: timestamp unchanged on error"
    );
}

// =========================================================================
// accrue_interest: Path 8 -- fee accumulation overflow (existing + new > u64::MAX)
// =========================================================================
// Branch condition: line 86-89 checked_add returns None
// Minimal input: existing_fees near u64::MAX, any new fee > 0
#[test]
fn concolic_accrue_path8_fee_accumulation_overflow() {
    let annual_bps: u16 = 1000; // 10%
    let fee_rate_bps: u16 = 500; // 5%
    let time_elapsed = SECONDS_PER_YEAR as i64;
    let supply: u128 = 1_000_000_000_000;

    let mut market = make_market(
        annual_bps,
        i64::MAX,
        WAD,
        supply,
        0,
        u64::MAX, // existing fees at maximum
    );
    let config = make_config(fee_rate_bps);
    let before = (
        market.scale_factor(),
        market.accrued_protocol_fees(),
        market.last_accrual_timestamp(),
    );

    let result = accrue_interest(&mut market, &config, time_elapsed);

    // The new fee will be > 0 (path 6 proved this), so existing(u64::MAX) + new > u64::MAX
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::MathOverflow as u32)),
        "path 8: fee accumulation overflows u64"
    );
    assert_eq!(
        market.scale_factor(),
        before.0,
        "path 8: scale_factor unchanged on error"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        before.1,
        "path 8: fees unchanged on error"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        before.2,
        "path 8: timestamp unchanged on error"
    );
}

// =========================================================================
// accrue_interest: Path 9 -- scale_factor overflow
// =========================================================================
// Branch condition: line 55-57 scale_factor + scale_factor_delta overflows u128
// OR line 49-51 scale_factor * interest_delta_wad overflows u128
// Minimal input: huge scale_factor
#[test]
fn concolic_accrue_path9_scale_factor_overflow() {
    // scale_factor near u128::MAX/2, 100% interest for a full year
    // interest_delta_wad = WAD (for 100% annual over 1 year)
    // scale_factor_delta = (u128::MAX/2) * WAD / WAD = u128::MAX/2
    // new_sf = u128::MAX/2 + u128::MAX/2 = u128::MAX - 1 (just barely fits)
    // But the intermediate: scale_factor * interest_delta_wad = (u128::MAX/2) * WAD
    // which overflows u128 since u128::MAX/2 > u128::MAX/WAD.
    let huge_sf = u128::MAX / 2;
    let annual_bps: u16 = 10000; // 100%
    let time_elapsed = SECONDS_PER_YEAR as i64;

    let mut market = make_market(annual_bps, i64::MAX, huge_sf, 0, 0, 0);
    let config = make_config(0);
    let before = (
        market.scale_factor(),
        market.accrued_protocol_fees(),
        market.last_accrual_timestamp(),
    );

    let result = accrue_interest(&mut market, &config, time_elapsed);
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::MathOverflow as u32)),
        "path 9: scale_factor * interest_delta_wad overflows u128"
    );
    assert_eq!(
        market.scale_factor(),
        before.0,
        "path 9: scale_factor unchanged on error"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        before.1,
        "path 9: fees unchanged on error"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        before.2,
        "path 9: timestamp unchanged on error"
    );
}

// =========================================================================
// PATH CONDITION TABLE: deposit_scale (deposit.rs:98-104)
// =========================================================================
//
// Source: src/processor/deposit.rs, lines 98-108
//
//  Branch 1 (line 100-102): amount_u128.checked_mul(WAD)
//    Condition A: overflow (amount * WAD > u128::MAX) => Err(MathOverflow)
//    Condition B: succeeds => CONTINUE
//
//  Branch 2 (line 103-104): .checked_div(scale_factor)
//    Condition A: scale_factor == 0 => Err(MathOverflow) (div by zero)
//    Condition B: succeeds => CONTINUE
//
//  Branch 3 (line 107): scaled_amount == 0
//    Condition A: scaled_amount == 0 => Err(ZeroScaledAmount)
//    Condition B: scaled_amount > 0 => CONTINUE
//
// Composite paths:
//   Path 1: amount == 0 (produces scaled_amount == 0)
//   Path 2: small amount, large scale_factor => scaled_amount == 0
//   Path 3: normal scaling (both amount and sf moderate)
//   Path 4: overflow in amount * WAD

#[test]
fn concolic_deposit_path1_amount_zero() {
    // amount = 0 => scaled_amount = 0 * WAD / WAD = 0
    let result = deposit_scale(0, WAD);
    assert!(result.is_ok(), "deposit path 1: computation succeeds");
    assert_eq!(
        result.unwrap_or(0),
        0,
        "deposit path 1: zero amount => zero scaled"
    );
    assert_eq!(
        deposit_scale(1, WAD).unwrap_or(0),
        1,
        "deposit path 1: x+1 boundary"
    );
    // In the actual processor (deposit.rs:44), amount==0 is rejected earlier
    // by the ZeroAmount check, but the math itself produces 0.
}

#[test]
fn concolic_deposit_path2_scaled_amount_zero() {
    // Small amount, huge scale_factor => scaled_amount rounds to 0
    // scaled_amount = amount * WAD / scale_factor
    // With amount = 1 and scale_factor = WAD * (WAD + 1):
    //   = 1 * WAD / (WAD * (WAD+1)) = 1 / (WAD+1) = 0 (floor)
    // Simpler: scale_factor = WAD + 1, amount = 1:
    //   = 1 * WAD / (WAD + 1) = WAD / (WAD+1)
    //   = 999_999_999_999_999_999 (non-zero, too big)
    // Need: amount * WAD < scale_factor
    //   amount = 1, scale_factor = WAD + 1
    //   1 * WAD < WAD + 1 => WAD < WAD + 1 => true! Floor division gives 0.
    //   Wait: 1 * WAD / (WAD + 1) = 10^18 / (10^18 + 1) = 0 (integer division)
    // Yes, this works.
    let large_sf = WAD + 1;
    let result = deposit_scale(1, large_sf);
    assert!(result.is_ok(), "deposit path 2: computation succeeds");
    let scaled = result.unwrap_or(1);
    assert_eq!(
        scaled, 0,
        "deposit path 2: small amount, large sf => scaled == 0"
    );
    assert_eq!(
        deposit_scale(2, large_sf).unwrap_or(0),
        1,
        "deposit path 2: x+1 boundary leaves zero region"
    );
    // In the actual processor this triggers ZeroScaledAmount error (line 107-109)
}

#[test]
fn concolic_deposit_path3_normal_scaling() {
    // Normal case: amount = 1_000_000 (1 USDC), scale_factor = WAD
    let amount = 1_000_000u64;
    let result = deposit_scale(amount, WAD);
    assert!(result.is_ok(), "deposit path 3: should succeed");
    let scaled = result.unwrap_or(0);

    // At WAD, scaling is identity: scaled = amount * WAD / WAD = amount
    assert_eq!(
        scaled,
        u128::from(amount),
        "deposit path 3: at WAD, scaling is identity"
    );

    // Verify round-trip preserves value
    let recovered = normalize(scaled, WAD).unwrap_or(0);
    assert_eq!(
        recovered,
        u128::from(amount),
        "deposit path 3: round-trip is exact at WAD"
    );
    assert_eq!(
        deposit_scale(amount + 1, WAD).unwrap_or(0),
        u128::from(amount + 1)
    );
}

#[test]
fn concolic_deposit_path3b_normal_scaling_above_wad() {
    // scale_factor = 1.1 * WAD (10% interest accrued)
    // Depositor gets fewer shares for the same USDC
    let amount = 1_000_000u64;
    let sf = WAD + WAD / 10; // 1.1 WAD
    let result = deposit_scale(amount, sf);
    assert!(result.is_ok(), "deposit path 3b: should succeed");
    let scaled = result.unwrap_or(0);

    // scaled = 1_000_000 * WAD / (WAD + WAD/10) = 1_000_000 * 10/11
    let expected = u128::from(amount) * WAD / sf;
    assert_eq!(scaled, expected, "deposit path 3b: correct scaling");

    // Verify round-trip: recovered <= amount (protocol-favorable rounding)
    let recovered = normalize(scaled, sf).unwrap_or(0);
    assert!(
        recovered <= u128::from(amount),
        "deposit path 3b: round-trip does not inflate"
    );
    // Rounding loss should be at most 1 base unit
    let loss = u128::from(amount) - recovered;
    assert!(
        loss <= 1,
        "deposit path 3b: rounding loss <= 1, got {}",
        loss
    );
}

#[test]
fn concolic_deposit_path4_overflow_amount_times_wad() {
    // amount * WAD > u128::MAX
    // u64::MAX * WAD = 18446744073709551615 * 10^18 = ~1.84 * 10^37
    // u128::MAX = ~3.4 * 10^38
    // So u64::MAX * WAD does NOT overflow u128.
    // For the standalone helper, u64 input means this path is unreachable.
    // However, if we consider the computation in isolation with u128 inputs:
    // We can verify the checked_mul correctly catches overflow.
    let amount = u64::MAX;
    let result = deposit_scale(amount, WAD);
    // u64::MAX * WAD = 18446744073709551615 * 10^18 fits in u128 (< 3.4e38)
    assert!(
        result.is_ok(),
        "deposit path 4: u64::MAX * WAD fits in u128"
    );

    // For scale_factor = 1 (degenerate), scaled = amount * WAD / 1 = amount * WAD
    let result2 = deposit_scale(amount, 1);
    assert!(result2.is_ok(), "deposit path 4b: sf=1 succeeds");
    assert_eq!(
        result2.unwrap_or(0),
        u128::from(amount) * WAD,
        "deposit path 4b: scaled = amount * WAD"
    );

    let overflow = (u128::MAX).checked_mul(WAD);
    assert!(
        overflow.is_none(),
        "deposit path 4c: u128 multiplication overflow is detectable"
    );
}

// =========================================================================
// PATH CONDITION TABLE: settlement factor (withdraw.rs)
// =========================================================================
//
// COAL-C01: settlement factor uses full vault balance (no fee reservation).
//
//  Branch 1: total_normalized = scaled_supply * sf / WAD
//    Condition: checked_mul/checked_div may overflow
//
//  Branch 2: total_normalized == 0
//    Condition A: total_normalized == 0 => return WAD
//    Condition B: total_normalized > 0 => CONTINUE
//
//  Branch 3: raw = vault_balance * WAD / total_normalized
//    Condition: checked_mul/checked_div may overflow
//
//  Branch 4: raw > WAD
//    Condition A: raw > WAD => capped = WAD (overfunded)
//    Condition B: raw <= WAD => capped = raw (underfunded or exact)
//
//  Branch 5: capped < 1
//    Condition A: capped < 1 => factor = 1 (minimum clamp)
//    Condition B: capped >= 1 => factor = capped
//
// Composite paths:
//   Path 1: total_normalized == 0 => WAD
//   Path 2: vault_balance == 0 => raw = 0 => capped = 0 => factor = 1 (clamped)
//   Path 3: vault_balance > total_normalized (overfunded) => raw > WAD => factor = WAD
//   Path 4: vault_balance < total_normalized (underfunded) => proportional factor

#[test]
fn concolic_settlement_path1_total_normalized_zero() {
    // scaled_total_supply = 0 => total_normalized = 0 => return WAD
    let result = compute_settlement_factor(
        1_000_000, // vault has funds
        0,         // no fees
        0,         // zero supply => total_normalized = 0
        WAD,
    );
    assert!(result.is_ok(), "settlement path 1: should succeed");
    assert_eq!(
        result.unwrap_or(0),
        WAD,
        "settlement path 1: zero supply => factor = WAD"
    );
}

#[test]
fn concolic_settlement_path2_vault_half() {
    // COAL-C01: vault_balance used directly (fees ignored)
    // vault = 500K, supply = 1M => factor = WAD/2
    let supply: u128 = 1_000_000;
    let result = compute_settlement_factor(
        500_000, // vault balance
        500_000, // fees ignored
        supply, WAD,
    );
    assert!(result.is_ok(), "settlement path 2: should succeed");
    let expected = 500_000u128 * WAD / 1_000_000;
    assert_eq!(
        result.unwrap_or(0),
        expected,
        "settlement path 2: vault = 50% of supply => factor = WAD/2"
    );
}

#[test]
fn concolic_settlement_path2b_vault_empty() {
    // vault_balance = 0 => raw = 0 => capped = 0 => factor = 1 (clamped)
    let result = compute_settlement_factor(
        0,         // empty vault
        0,         // fees ignored
        1_000_000, // some supply
        WAD,
    );
    assert!(result.is_ok(), "settlement path 2b: should succeed");
    assert_eq!(
        result.unwrap_or(0),
        1,
        "settlement path 2b: empty vault => factor clamped to 1"
    );
}

#[test]
fn concolic_settlement_path3_overfunded() {
    // vault_balance > total_normalized => raw > WAD => factor = WAD
    let supply: u128 = 1_000_000;
    let result = compute_settlement_factor(
        2_000_000, // vault = 2x supply
        0,         // no fees
        supply, WAD,
    );
    assert!(result.is_ok(), "settlement path 3: should succeed");

    let factor = result.unwrap_or(0);
    // vault = 2M, total_normalized = 1M => raw = 2*WAD => capped at WAD
    assert_eq!(factor, WAD, "settlement path 3: overfunded => factor = WAD");
}

#[test]
fn concolic_settlement_path4_underfunded() {
    // vault_balance < total_normalized => proportional
    // 50% funded: vault = 500K, total_normalized = 1M
    let supply: u128 = 1_000_000;
    let result = compute_settlement_factor(
        500_000, // vault = 50% of supply
        0, supply, WAD,
    );
    assert!(result.is_ok(), "settlement path 4: should succeed");

    let factor = result.unwrap_or(0);
    // raw = 500_000 * WAD / 1_000_000 = WAD / 2
    let expected = WAD / 2;
    assert_eq!(
        factor, expected,
        "settlement path 4: 50% funded => factor = WAD/2"
    );
}

#[test]
fn concolic_settlement_path4b_vault_equals_supply() {
    // COAL-C01: vault = 1M used directly (fees ignored), supply = 1M
    // factor = 1M * WAD / 1M = WAD (fully funded)
    let supply: u128 = 1_000_000;
    let result = compute_settlement_factor(
        1_000_000, // vault
        200_000,   // fees ignored
        supply, WAD,
    );
    assert!(result.is_ok(), "settlement path 4b: should succeed");

    let factor = result.unwrap_or(0);
    assert_eq!(
        factor, WAD,
        "settlement path 4b: vault == supply => factor = WAD"
    );
}

#[test]
fn concolic_settlement_fees_ignored() {
    // COAL-C01: fees are ignored; vault_balance used directly
    // vault = 100, supply = 1M => factor = 100 * WAD / 1M
    let result = compute_settlement_factor(
        100,     // vault
        999_999, // fees ignored
        1_000_000, WAD,
    );
    assert!(result.is_ok(), "settlement fees ignored: should succeed");
    let expected = 100u128 * WAD / 1_000_000;
    assert_eq!(
        result.unwrap_or(0),
        expected,
        "settlement fees ignored: factor = vault * WAD / total_normalized"
    );
}

// =========================================================================
// PATH CONDITION TABLE: fee computation (interest.rs:60-91)
// =========================================================================
//
// Source: src/logic/interest.rs, lines 60-91
//
//  Branch 1 (line 61): fee_rate_bps > 0?
//    Condition A: fee_rate_bps == 0 => SKIP entire fee block
//    Condition B: fee_rate_bps > 0  => ENTER fee block
//
//  Branch 2 (line 65-69): fee_delta_wad = interest_delta * fee_rate / BPS
//    Condition: checked_mul may overflow
//
//  Branch 3 (line 73-81): fee_normalized = supply * new_sf / WAD * fee_delta / WAD
//    Sub-condition A: scaled_total_supply == 0 => fee_normalized = 0
//    Sub-condition B: normal computation succeeds
//    Sub-condition C: overflow at any step
//
//  Branch 4 (line 83-84): u64::try_from(fee_normalized)
//    Condition A: fits => Ok
//    Condition B: exceeds u64::MAX => Err
//
//  Branch 5 (line 86-89): existing + new checked_add
//    Condition A: fits => Ok
//    Condition B: overflow => Err
//
// Composite paths:
//   Path 1: fee_rate == 0 => existing_fees returned unchanged
//   Path 2: scaled_total_supply == 0 => fee_normalized = 0 => existing_fees + 0
//   Path 3: normal computation => new_fees
//   Path 4: fee_normalized > u64::MAX => Err

#[test]
fn concolic_fee_path1_zero_rate() {
    let result = compute_fee(
        WAD / 10,  // 10% interest delta
        0,         // fee_rate_bps = 0
        1_000_000, // some supply
        WAD,
        42, // existing fees
    );
    assert!(result.is_ok(), "fee path 1: should succeed");
    assert_eq!(
        result.unwrap_or(0),
        42,
        "fee path 1: zero rate => existing fees returned unchanged"
    );
}

#[test]
fn concolic_fee_path2_zero_supply() {
    let result = compute_fee(
        WAD / 10,
        500, // 5% fee rate
        0,   // zero supply
        WAD,
        0,
    );
    assert!(result.is_ok(), "fee path 2: should succeed");
    assert_eq!(
        result.unwrap_or(1),
        0,
        "fee path 2: zero supply => zero fee_normalized => 0 + 0 = 0"
    );
}

#[test]
fn concolic_fee_path3_normal() {
    let interest_delta_wad = WAD / 10; // 10% annual
    let fee_rate_bps: u16 = 500; // 5%
    let supply: u128 = 1_000_000_000_000; // 1M USDC
    let new_sf = WAD + interest_delta_wad;

    let result = compute_fee(interest_delta_wad, fee_rate_bps, supply, new_sf, 0);
    assert!(result.is_ok(), "fee path 3: should succeed");

    // Verify intermediate values
    let fee_delta_wad = interest_delta_wad * 500 / BPS;
    let fee_normalized = supply * new_sf / WAD * fee_delta_wad / WAD;
    let expected = fee_normalized as u64;

    assert_eq!(
        result.unwrap_or(0),
        expected,
        "fee path 3: matches hand-computed fee"
    );
    assert!(expected > 0, "fee path 3: non-zero fee");
}

#[test]
fn concolic_fee_path3b_proportionality() {
    // Doubling fee_rate doubles the fee (linearity property)
    let interest_delta_wad = WAD / 10;
    let supply: u128 = 1_000_000_000_000;
    let new_sf = WAD + interest_delta_wad;

    let fee_at_500 = compute_fee(interest_delta_wad, 500, supply, new_sf, 0).unwrap_or(0);
    let fee_at_1000 = compute_fee(interest_delta_wad, 1000, supply, new_sf, 0).unwrap_or(0);

    // Due to integer division, exact 2x may not hold, but should be within 1
    assert!(
        fee_at_1000 >= fee_at_500 * 2 - 1 && fee_at_1000 <= fee_at_500 * 2 + 1,
        "fee path 3b: doubling rate approximately doubles fee: {} vs 2*{}",
        fee_at_1000,
        fee_at_500
    );
    let fee_at_501 = compute_fee(interest_delta_wad, 501, supply, new_sf, 0).unwrap_or(0);
    assert!(
        fee_at_501 >= fee_at_500,
        "fee path 3b: increasing fee rate by 1 bps must not decrease fees"
    );
}

#[test]
fn concolic_fee_path4_overflow() {
    // fee_normalized > u64::MAX
    // Using very large supply and maximum rates
    let interest_delta_wad = WAD; // 100% annual
    let supply: u128 = 100_000_000_000_000_000_000; // 10^20
    let new_sf = 2 * WAD; // high scale factor

    let result = compute_fee(interest_delta_wad, 10000, supply, new_sf, 0);
    assert_eq!(
        result,
        Err("fee exceeds u64"),
        "fee path 4: fee_normalized > u64::MAX => exact overflow error"
    );
}

#[test]
fn concolic_fee_path5_accumulation_overflow() {
    // existing_fees = u64::MAX, new fee > 0 => checked_add overflows
    let interest_delta_wad = WAD / 10;
    let supply: u128 = 1_000_000_000_000;
    let new_sf = WAD + interest_delta_wad;

    let result = compute_fee(interest_delta_wad, 500, supply, new_sf, u64::MAX);
    assert_eq!(
        result,
        Err("overflow: existing + new fees"),
        "fee path 5: u64::MAX + any > 0 overflows"
    );
}

// =========================================================================
// Cross-function path: accrue_interest fee independence from scale_factor
// =========================================================================
// Property P-FEE-4: fees do not alter scale_factor

#[test]
fn concolic_fee_independence_from_scale_factor() {
    let annual_bps: u16 = 1000;
    let time_elapsed = SECONDS_PER_YEAR as i64;
    let supply: u128 = 1_000_000_000_000;

    // Run with fees
    let mut market_with_fees = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config_fees = make_config(5000); // 50% fee rate
    accrue_interest(&mut market_with_fees, &config_fees, time_elapsed).unwrap_or(());

    // Run without fees
    let mut market_no_fees = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config_no_fees = make_config(0);
    accrue_interest(&mut market_no_fees, &config_no_fees, time_elapsed).unwrap_or(());

    // scale_factor must be identical regardless of fee config
    assert_eq!(
        market_with_fees.scale_factor(),
        market_no_fees.scale_factor(),
        "fee computation does not alter scale_factor"
    );

    // But fees should differ
    assert!(
        market_with_fees.accrued_protocol_fees() > 0,
        "with fee config, fees should be non-zero"
    );
    assert_eq!(
        market_no_fees.accrued_protocol_fees(),
        0,
        "without fee config, fees should be zero"
    );

    // Independent oracle: sf depends only on rate and time, never on fee config.
    let growth = growth_factor_wad_oracle(annual_bps, time_elapsed).unwrap();
    let expected_sf = mul_wad_oracle(WAD, growth).unwrap();
    assert_eq!(market_with_fees.scale_factor(), expected_sf);
    assert_eq!(market_no_fees.scale_factor(), expected_sf);
}

// =========================================================================
// Cross-function path: full deposit-accrue-withdraw round-trip
// =========================================================================
// This exercises deposit scaling + interest accrual + settlement factor

#[test]
fn concolic_full_roundtrip_deposit_accrue_settle() {
    // Simulate: deposit 1M USDC, accrue 10% interest for 1 year, settle at 100%
    let amount = 1_000_000u64;
    let sf_initial = WAD;

    // Step 1: Deposit scaling
    let scaled = deposit_scale(amount, sf_initial).unwrap_or(0);
    assert_eq!(scaled, u128::from(amount), "at WAD, scaled == amount");

    // Step 2: Interest accrual (10% over 1 year)
    let mut market = make_market(
        1000, // 10% annual
        i64::MAX,
        sf_initial,
        scaled,
        0,
        0,
    );
    let config = make_config(0);
    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap_or(());
    let sf_after = market.scale_factor();
    let expected_sf = mul_wad_oracle(
        sf_initial,
        growth_factor_wad_oracle(1000, SECONDS_PER_YEAR as i64).unwrap(),
    )
    .unwrap();
    assert_eq!(
        sf_after, expected_sf,
        "scale_factor follows daily-compound oracle"
    );

    // Step 3: Settlement factor (fully funded)
    let total_normalized = scaled * sf_after / WAD;
    let vault_balance = total_normalized + 1; // slightly overfunded
    let settlement = compute_settlement_factor(vault_balance, 0, scaled, sf_after);
    assert_eq!(settlement.unwrap_or(0), WAD, "fully funded => WAD");

    // Step 4: Payout computation
    let normalized = normalize(scaled, sf_after).unwrap_or(0);
    let payout = normalized * WAD / WAD; // settlement_factor = WAD => payout = normalized

    let expected_normalized = scaled * expected_sf / WAD;
    assert_eq!(
        normalized, expected_normalized,
        "normalized includes accrued interest"
    );
    assert_eq!(payout, expected_normalized, "full payout at WAD settlement");
}

// =========================================================================
// Edge case: maturity exactly at last_accrual
// =========================================================================
#[test]
fn concolic_accrue_maturity_equals_last_accrual() {
    // effective_now = min(current_ts=999, maturity=100) = 100
    // time_elapsed = 100 - 100 = 0 => early return
    let mut market = make_market(1000, 100, WAD, WAD, 100, 0);
    let config = make_config(0);

    let result = accrue_interest(&mut market, &config, 999);
    assert!(result.is_ok(), "maturity == last_accrual: should return Ok");
    assert_eq!(
        market.scale_factor(),
        WAD,
        "maturity == last_accrual: no interest accrued"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        100,
        "maturity == last_accrual: timestamp unchanged"
    );
}

// =========================================================================
// Edge case: minimum meaningful interest (1 bps, 1 second)
// =========================================================================
#[test]
fn concolic_accrue_minimum_meaningful_interest() {
    let mut market = make_market(1, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, 1).unwrap_or(());

    // interest_delta_wad = 1 * 1 * WAD / (SECONDS_PER_YEAR * BPS)
    //                    = 1e18 / 315_360_000_000
    let expected_delta_wad = 1u128 * 1 * WAD / (SECONDS_PER_YEAR * BPS);
    // = 3_170_979 (floor division)
    assert!(
        expected_delta_wad > 0,
        "minimum interest_delta_wad is non-zero"
    );

    let expected_sf_delta = WAD * expected_delta_wad / WAD;
    assert_eq!(
        expected_sf_delta, expected_delta_wad,
        "at WAD scale_factor, sf_delta == interest_delta_wad"
    );

    assert_eq!(
        market.scale_factor(),
        WAD + expected_delta_wad,
        "1 bps * 1 second => scale_factor increases by minimum delta"
    );
    assert_eq!(
        market.scale_factor(),
        WAD + (1u128 * 1 * WAD / (SECONDS_PER_YEAR * BPS)),
        "minimum meaningful interest follows exact formula"
    );
}

// =========================================================================
// Edge case: settlement factor with scale_factor > WAD
// =========================================================================
#[test]
fn concolic_settlement_with_accrued_interest() {
    // After interest accrual, scale_factor = 1.1 * WAD
    // supply = 1M scaled tokens, total_normalized = 1M * 1.1 = 1.1M
    // vault has exactly 1.1M => settlement = WAD
    let sf = WAD + WAD / 10;
    let supply: u128 = 1_000_000;
    let total_normalized = supply * sf / WAD; // 1_100_000
    assert_eq!(total_normalized, 1_100_000);

    let result = compute_settlement_factor(total_normalized, 0, supply, sf);
    assert_eq!(
        result.unwrap_or(0),
        WAD,
        "exactly funded after interest => WAD"
    );

    // Slightly underfunded (vault = 1M, owes 1.1M)
    let result2 = compute_settlement_factor(1_000_000, 0, supply, sf);
    let expected = 1_000_000u128 * WAD / total_normalized;
    assert_eq!(
        result2.unwrap_or(0),
        expected,
        "underfunded after interest => proportional"
    );
    // 1M / 1.1M = 0.909... * WAD
    assert!(result2.unwrap_or(0) < WAD, "underfunded => factor < WAD");
}

// =========================================================================
// Verify all accrue_interest paths compose correctly with sequential calls
// =========================================================================
#[test]
fn concolic_accrue_sequential_monotonicity() {
    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000, 0, 0);
    let config = make_config(500);

    let mut prev_sf = market.scale_factor();
    let mut prev_fees = market.accrued_protocol_fees();

    // Accrue in 10 steps of 1000 seconds each
    for step in 1..=10 {
        let ts = step * 1000i64;
        accrue_interest(&mut market, &config, ts).unwrap_or(());

        assert!(
            market.scale_factor() >= prev_sf,
            "step {}: scale_factor must be monotonically non-decreasing",
            step
        );
        assert!(
            market.accrued_protocol_fees() >= prev_fees,
            "step {}: fees must be monotonically non-decreasing",
            step
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            ts,
            "step {}: timestamp advances",
            step
        );

        prev_sf = market.scale_factor();
        prev_fees = market.accrued_protocol_fees();
    }

    assert!(
        market.scale_factor() > WAD,
        "sequential accruals with positive rate should strictly increase scale factor"
    );
}

// =========================================================================
// Differential checks against an independent oracle model
// =========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn concolic_differential_accrue_step_equivalence(
        annual_bps in prop_oneof![Just(0u16), Just(1u16), Just(10_000u16), 0u16..=10_000u16],
        fee_rate in prop_oneof![Just(0u16), Just(1u16), Just(10_000u16), 0u16..=10_000u16],
        maturity_base in prop_oneof![Just(0i64), Just(1_000i64), Just(2_000_000_000i64), 0i64..2_000_000_000i64],
        scale_offset in prop_oneof![Just(0u64), Just(1u64), Just(999_999_999_999_999_999u64), 0u64..1_000_000_000_000_000_000u64],
        supply in prop_oneof![Just(0u128), Just(1u128), Just(1_000_000u128), 0u128..1_000_000_000_000u128],
        existing_fees in prop_oneof![Just(0u64), Just(1u64), Just(u64::MAX - 1), Just(u64::MAX)],
        last_accrual in 0i64..1_000_000i64,
        dt1 in 0i64..200_000i64,
        dt2 in 0i64..200_000i64,
        force_backward_second_step in any::<bool>(),
    ) {
        let maturity = core::cmp::max(maturity_base, last_accrual);
        let initial_sf = WAD + u128::from(scale_offset);

        let mut market = make_market(
            annual_bps,
            maturity,
            initial_sf,
            supply,
            last_accrual,
            existing_fees,
        );
        let config = make_config(fee_rate);

        let mut oracle = OracleState {
            scale_factor: initial_sf,
            accrued_fees: existing_fees,
            last_accrual_timestamp: last_accrual,
            annual_bps,
            maturity_timestamp: maturity,
            scaled_total_supply: supply,
        };

        let ts1 = last_accrual.saturating_add(dt1);
        let ts2_forward = ts1.saturating_add(dt2);
        let ts2 = if force_backward_second_step {
            last_accrual.saturating_sub(1)
        } else {
            ts2_forward
        };

        for ts in [ts1, ts2] {
            let before = (
                market.scale_factor(),
                market.accrued_protocol_fees(),
                market.last_accrual_timestamp(),
            );
            let program_result = accrue_interest(&mut market, &config, ts);
            let oracle_result = oracle_accrue_step(oracle, fee_rate, ts);

            prop_assert_eq!(
                program_result.is_ok(),
                oracle_result.is_ok(),
                "program/oracle success mismatch at ts={}",
                ts
            );

            match (program_result, oracle_result) {
                (Ok(()), Ok(next_oracle)) => {
                    oracle = next_oracle;
                    prop_assert_eq!(market.scale_factor(), oracle.scale_factor);
                    prop_assert_eq!(market.accrued_protocol_fees(), oracle.accrued_fees);
                    prop_assert_eq!(
                        market.last_accrual_timestamp(),
                        oracle.last_accrual_timestamp
                    );
                },
                (Err(program_err), Err(oracle_err)) => {
                    prop_assert_eq!(program_err, oracle_err);
                    prop_assert_eq!(market.scale_factor(), before.0);
                    prop_assert_eq!(market.accrued_protocol_fees(), before.1);
                    prop_assert_eq!(market.last_accrual_timestamp(), before.2);
                },
                _ => prop_assert!(false, "program/oracle branch mismatch"),
            }
        }
    }
}
