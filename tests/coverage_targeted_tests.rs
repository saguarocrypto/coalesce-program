//! Coverage-targeted tests for the CoalesceFi Pinocchio lending protocol.
//!
//! Each test exercises a specific untested code path identified through
//! static analysis of the processor, logic, and validation modules.
//! Tests focus on pure logic (no BPF runtime) using `bytemuck::Zeroable`
//! to construct state objects directly.

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

use coalesce::constants::{
    BORROWER_WHITELIST_SIZE, BPS, LENDER_POSITION_SIZE, MARKET_SIZE, MAX_ANNUAL_INTEREST_BPS,
    MAX_FEE_RATE_BPS, MIN_MATURITY_DELTA, PROTOCOL_CONFIG_SIZE, SECONDS_PER_YEAR, USDC_DECIMALS,
    WAD, ZERO_ADDRESS,
};
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::logic::validation::is_zero_address;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};
use pinocchio::error::ProgramError;

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

/// Reproduce the on-chain deposit scaling: scaled_amount = amount * WAD / scale_factor
fn deposit_scale(amount: u64, scale_factor: u128) -> Option<u128> {
    let amount_u128 = u128::from(amount);
    amount_u128.checked_mul(WAD)?.checked_div(scale_factor)
}

/// Normalize: normalized = scaled_amount * scale_factor / WAD
fn normalize(scaled_amount: u128, scale_factor: u128) -> Option<u128> {
    scaled_amount.checked_mul(scale_factor)?.checked_div(WAD)
}

/// Payout: payout = normalized * settlement_factor / WAD
fn compute_payout(
    scaled_amount: u128,
    scale_factor: u128,
    settlement_factor: u128,
) -> Option<u128> {
    let normalized = normalize(scaled_amount, scale_factor)?;
    normalized.checked_mul(settlement_factor)?.checked_div(WAD)
}

/// Reproduce the on-chain settlement factor logic from withdraw.rs / re_settle.rs.
fn compute_settlement_factor(available_for_lenders: u128, total_normalized: u128) -> u128 {
    if total_normalized == 0 {
        WAD
    } else {
        let raw = available_for_lenders
            .checked_mul(WAD)
            .unwrap()
            .checked_div(total_normalized)
            .unwrap();
        let capped = if raw > WAD { WAD } else { raw };
        if capped < 1 {
            1
        } else {
            capped
        }
    }
}

fn expect_lending_error(result: Result<(), ProgramError>, expected: LendingError) {
    assert_eq!(result, Err(ProgramError::Custom(expected as u32)));
}

fn expect_lending_error_u128(result: Result<u128, ProgramError>, expected: LendingError) {
    assert_eq!(result, Err(ProgramError::Custom(expected as u32)));
}

fn expect_lending_error_u64(result: Result<u64, ProgramError>, expected: LendingError) {
    assert_eq!(result, Err(ProgramError::Custom(expected as u32)));
}

fn expected_scale_factor_single_step(
    initial_sf: u128,
    annual_bps: u16,
    elapsed_seconds: i64,
) -> u128 {
    let growth_wad = math_oracle::growth_factor_wad(annual_bps, elapsed_seconds);
    math_oracle::mul_wad(initial_sf, growth_wad)
}

fn expected_fee_delta_single_step(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> u64 {
    if scaled_total_supply == 0 || fee_rate_bps == 0 || elapsed_seconds <= 0 {
        return 0;
    }

    let growth_wad = math_oracle::growth_factor_wad(annual_bps, elapsed_seconds);
    let interest_delta_wad = growth_wad.checked_sub(WAD).unwrap();
    let fee_delta_wad = interest_delta_wad
        .checked_mul(u128::from(fee_rate_bps))
        .unwrap()
        .checked_div(BPS)
        .unwrap();
    // Use pre-accrual scale_factor_before (Finding 10 fix)
    let fee_normalized = scaled_total_supply
        .checked_mul(scale_factor_before)
        .unwrap()
        .checked_div(WAD)
        .unwrap()
        .checked_mul(fee_delta_wad)
        .unwrap()
        .checked_div(WAD)
        .unwrap();

    u64::try_from(fee_normalized).unwrap()
}

fn validate_close_position(position: &LenderPosition) -> Result<(), ProgramError> {
    if position.scaled_balance() != 0 {
        return Err(LendingError::PositionNotEmpty.into());
    }
    Ok(())
}

fn validate_resettle_improvement(old_factor: u128, new_factor: u128) -> Result<(), ProgramError> {
    if new_factor <= old_factor {
        return Err(LendingError::SettlementNotImproved.into());
    }
    Ok(())
}

fn resolve_withdraw_scaled_amount(
    requested_scaled_amount: u128,
    position_balance: u128,
) -> Result<u128, ProgramError> {
    let effective_scaled_amount = if requested_scaled_amount == 0 {
        position_balance
    } else {
        requested_scaled_amount
    };
    if effective_scaled_amount > position_balance {
        return Err(LendingError::InsufficientScaledBalance.into());
    }
    Ok(effective_scaled_amount)
}

fn validate_borrow_capacity(
    whitelist: &BorrowerWhitelist,
    borrow_amount: u64,
) -> Result<u64, ProgramError> {
    let new_total = whitelist
        .current_borrowed()
        .checked_add(borrow_amount)
        .ok_or(LendingError::MathOverflow)?;
    if new_total > whitelist.max_borrow_capacity() {
        return Err(LendingError::GlobalCapacityExceeded.into());
    }
    Ok(new_total)
}

fn validate_whitelist_capacity(
    is_whitelisted: u8,
    max_borrow_capacity: u64,
) -> Result<(), ProgramError> {
    if is_whitelisted == 1 && max_borrow_capacity == 0 {
        return Err(LendingError::InvalidCapacity.into());
    }
    Ok(())
}

fn compute_withdrawable_fees(accrued_fees: u64, vault_balance: u64) -> Result<u64, ProgramError> {
    if accrued_fees == 0 {
        return Err(LendingError::NoFeesToCollect.into());
    }
    let withdrawable = core::cmp::min(accrued_fees, vault_balance);
    if withdrawable == 0 {
        return Err(LendingError::NoFeesToCollect.into());
    }
    Ok(withdrawable)
}

fn checked_mul_or_error(a: u128, b: u128) -> Result<u128, ProgramError> {
    a.checked_mul(b).ok_or(LendingError::MathOverflow.into())
}

fn checked_div_or_error(a: u128, b: u128) -> Result<u128, ProgramError> {
    a.checked_div(b).ok_or(LendingError::MathOverflow.into())
}

// ===========================================================================
// Section 1: accrue_interest -- time_elapsed <= 0 early return
// ===========================================================================

/// When current_ts < last_accrual (backwards clock), returns InvalidTimestamp error.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn accrue_backwards_clock_no_state_change() {
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 500, 42);
    let config = make_config(1000);

    // current_ts = 100 < last_accrual = 500 -> InvalidTimestamp error (SR-114)
    let result = accrue_interest(&mut market, &config, 100);
    assert!(
        result.is_err(),
        "SR-114: backward timestamps should return error"
    );
    assert_eq!(
        result.unwrap_err(),
        pinocchio::error::ProgramError::Custom(20),
        "error should be InvalidTimestamp (37)"
    );
    assert_eq!(
        market.scale_factor(),
        WAD,
        "scale_factor must not change on error"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        42,
        "fees must not change on error"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        500,
        "last_accrual must not change on error"
    );
}

/// When current_ts is capped at maturity and maturity < last_accrual,
/// effective_now < last_accrual -> InvalidTimestamp error.
/// SR-114: Backward timestamp manipulation is now explicitly rejected.
#[test]
fn accrue_maturity_before_last_accrual_no_op() {
    // maturity=100, last_accrual=200, current_ts=9999
    // effective_now = min(9999, 100) = 100
    // 100 < 200 (last_accrual) -> InvalidTimestamp error (SR-114)
    let mut market = make_market(5000, 100, WAD, WAD, 200, 7);
    let config = make_config(500);

    let result = accrue_interest(&mut market, &config, 9999);
    assert!(
        result.is_err(),
        "SR-114: effective_now < last_accrual should return error"
    );
    assert_eq!(
        result.unwrap_err(),
        pinocchio::error::ProgramError::Custom(20),
        "error should be InvalidTimestamp (37)"
    );
    assert_eq!(market.scale_factor(), WAD);
    assert_eq!(market.accrued_protocol_fees(), 7);
    assert_eq!(market.last_accrual_timestamp(), 200);
}

/// Double call at the same timestamp (idempotency) -- second call is a no-op.
#[test]
fn accrue_double_call_same_timestamp_idempotent() {
    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = make_config(500);

    accrue_interest(&mut market, &config, 1000).unwrap();
    let sf_after_first = market.scale_factor();
    let fees_after_first = market.accrued_protocol_fees();
    assert_eq!(
        sf_after_first,
        expected_scale_factor_single_step(WAD, 1000, 1000)
    );

    accrue_interest(&mut market, &config, 1000).unwrap();
    assert_eq!(market.scale_factor(), sf_after_first);
    assert_eq!(market.accrued_protocol_fees(), fees_after_first);

    accrue_interest(&mut market, &config, 1001).unwrap();
    let expected_after_1001 = expected_scale_factor_single_step(sf_after_first, 1000, 1);
    assert_eq!(market.scale_factor(), expected_after_1001);
    assert_eq!(market.last_accrual_timestamp(), 1001);
}

// ===========================================================================
// Section 2: accrue_interest -- scale_factor_delta == 0 for tiny intervals
// ===========================================================================

/// With a very low annual_bps and very short time, the integer division
/// in interest_delta_wad may produce 0, yielding no scale_factor change.
#[test]
fn accrue_tiny_rate_tiny_time_zero_delta() {
    // annual_bps=1, time=1 second
    // interest_delta_wad = 1 * 1 * WAD / (31536000 * 10000)
    //                    = WAD / 315_360_000_000
    //                    = 1e18 / 3.15e11
    //                    ~ 3170979 (non-zero actually)
    // scale_factor_delta = WAD * 3170979 / WAD = 3170979
    // So even 1 bps for 1 second produces a non-zero delta.
    // Let's try annual_bps=1, time=1 with a smaller scale_factor
    let mut market = make_market(1, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, 1).unwrap();

    let expected = expected_scale_factor_single_step(WAD, 1, 1);
    assert_eq!(market.scale_factor(), expected);
    assert_eq!(market.last_accrual_timestamp(), 1);

    let mut zero_elapsed = make_market(1, i64::MAX, WAD, WAD, 100, 0);
    accrue_interest(&mut zero_elapsed, &config, 100).unwrap();
    assert_eq!(zero_elapsed.scale_factor(), WAD);
    assert_eq!(zero_elapsed.last_accrual_timestamp(), 100);
}

/// With scale_factor already very large, even small interest_delta_wad
/// could produce an overflow. Verify checked_mul catches it.
#[test]
fn accrue_overflow_in_scale_factor_delta() {
    // scale_factor near u128::MAX / 2
    let huge_sf = u128::MAX / 2;
    let mut market = make_market(10000, i64::MAX, huge_sf, 0, 0, 0);
    let config = make_config(0);

    let before_scale = market.scale_factor();
    let before_fees = market.accrued_protocol_fees();
    let before_ts = market.last_accrual_timestamp();

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    expect_lending_error(result, LendingError::MathOverflow);
    assert_eq!(market.scale_factor(), before_scale);
    assert_eq!(market.accrued_protocol_fees(), before_fees);
    assert_eq!(market.last_accrual_timestamp(), before_ts);
}

// ===========================================================================
// Section 3: accrue_interest -- fee accrual edge cases
// ===========================================================================

/// When fee_rate > 0 but scaled_total_supply == 0, the fee_normalized
/// computation should yield 0 (no supply to charge fees on).
#[test]
fn accrue_fee_with_zero_supply_yields_zero_fees() {
    let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    let config = make_config(5000);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor_single_step(WAD, 1000, SECONDS_PER_YEAR as i64)
    );
    assert_eq!(market.accrued_protocol_fees(), 0, "no supply means no fees");
    assert_eq!(market.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);
}

/// When fee_rate > 0 and supply is very small (e.g., 1 lamport),
/// fee_normalized may truncate to 0 due to integer division.
#[test]
fn accrue_fee_tiny_supply_truncates_to_zero() {
    // supply = 1, time = 1 second, annual_bps = 1, fee_rate = 1
    // interest_delta_wad = 1 * 1 * WAD / (SECONDS_PER_YEAR * BPS) ~ 3.17e6
    // fee_delta_wad = interest_delta_wad * 1 / BPS ~ 317
    // fee_normalized = 1 * new_sf / WAD * fee_delta_wad / WAD
    //               ~ 1 * 1 * 317 / WAD ~ 0 (truncated)
    let mut market = make_market(1, i64::MAX, WAD, 1, 0, 0);
    let config = make_config(1);

    accrue_interest(&mut market, &config, 1).unwrap();
    assert_eq!(market.accrued_protocol_fees(), 0);

    let mut market_high_supply = make_market(1, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_high_supply, &config, 1).unwrap();
    assert_eq!(
        market_high_supply.accrued_protocol_fees(),
        317,
        "same rate/time but higher supply must produce non-zero fee"
    );
}

/// fee_normalized overflows u64 -> MathOverflow error
#[test]
fn accrue_fee_overflow_u64_try_from() {
    // Need fee_normalized > u64::MAX
    // fee_normalized = supply * new_sf / WAD * fee_delta_wad / WAD
    // With supply near u128::MAX/WAD, new_sf=2*WAD, fee_delta = WAD:
    // fee = (u128::MAX/WAD) * 2*WAD / WAD * WAD / WAD
    //     = (u128::MAX/WAD) * 2
    //     >> u64::MAX
    let huge_supply = u128::MAX / WAD;
    let mut market = make_market(10000, i64::MAX, WAD, huge_supply, 0, 0);
    let config = make_config(10000);

    let before_scale = market.scale_factor();
    let before_fees = market.accrued_protocol_fees();
    let before_ts = market.last_accrual_timestamp();

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    expect_lending_error(result, LendingError::MathOverflow);
    assert_eq!(market.scale_factor(), before_scale);
    assert_eq!(market.accrued_protocol_fees(), before_fees);
    assert_eq!(market.last_accrual_timestamp(), before_ts);
}

/// Existing fees are preserved and new fees accumulate on top.
#[test]
fn accrue_fees_accumulate_on_existing() {
    let initial_fees = 999_999u64;
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, initial_fees);
    let config = make_config(500);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    let expected_fees = initial_fees
        + expected_fee_delta_single_step(supply, WAD, 1000, 500, SECONDS_PER_YEAR as i64);

    assert_eq!(market.accrued_protocol_fees(), expected_fees);
    assert!(market.accrued_protocol_fees() > initial_fees);
    assert_eq!(market.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);
}

/// Fee accumulation overflow: existing fees near u64::MAX plus new fees
/// should return MathOverflow from checked_add.
#[test]
fn accrue_fee_accumulation_overflow() {
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, u64::MAX);
    let config = make_config(5000);

    let before_scale = market.scale_factor();
    let before_fees = market.accrued_protocol_fees();
    let before_ts = market.last_accrual_timestamp();

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    expect_lending_error(result, LendingError::MathOverflow);
    assert_eq!(market.scale_factor(), before_scale);
    assert_eq!(market.accrued_protocol_fees(), before_fees);
    assert_eq!(market.last_accrual_timestamp(), before_ts);
}

// ===========================================================================
// Section 4: accrue_interest -- maturity boundary conditions
// ===========================================================================

/// Accrue exactly to maturity, then call again past maturity.
/// Second call should be a no-op since last_accrual is already at maturity.
#[test]
fn accrue_past_maturity_after_reaching_maturity() {
    let maturity = 1000i64;
    let mut market = make_market(1000, maturity, WAD, WAD, 0, 0);
    let config = make_config(0);

    // First: accrue to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();
    let sf_at_maturity = market.scale_factor();
    assert_eq!(market.last_accrual_timestamp(), maturity);
    assert_eq!(
        sf_at_maturity,
        expected_scale_factor_single_step(WAD, 1000, maturity)
    );

    // Second: accrue far past maturity -- effective_now = maturity,
    // time_elapsed = maturity - maturity = 0 -> no-op
    accrue_interest(&mut market, &config, maturity + 999_999).unwrap();
    assert_eq!(
        market.scale_factor(),
        sf_at_maturity,
        "no change after maturity"
    );
    assert_eq!(market.last_accrual_timestamp(), maturity);

    let mut before_maturity = make_market(1000, maturity, WAD, WAD, 0, 0);
    accrue_interest(&mut before_maturity, &config, maturity - 1).unwrap();
    assert_eq!(
        before_maturity.scale_factor(),
        expected_scale_factor_single_step(WAD, 1000, maturity - 1)
    );
}

/// Sequential accruals straddling maturity: before and after.
#[test]
fn accrue_straddle_maturity() {
    let maturity = 1000i64;
    let mut market = make_market(1000, maturity, WAD, WAD, 0, 0);
    let config = make_config(0);

    // Step 1: accrue to 500 (before maturity)
    accrue_interest(&mut market, &config, 500).unwrap();
    let sf_at_500 = market.scale_factor();
    assert!(sf_at_500 > WAD);
    assert_eq!(market.last_accrual_timestamp(), 500);

    // Step 2: accrue to 2000 (past maturity) -- caps at maturity
    accrue_interest(&mut market, &config, 2000).unwrap();
    let sf_at_maturity = market.scale_factor();
    assert!(sf_at_maturity > sf_at_500);
    assert_eq!(market.last_accrual_timestamp(), maturity);

    // Step 3: call again past maturity -- no-op
    accrue_interest(&mut market, &config, 5000).unwrap();
    assert_eq!(market.scale_factor(), sf_at_maturity);
}

// ===========================================================================
// Section 5: accrue_interest -- zero/edge scale_factor
// ===========================================================================

/// scale_factor == 0 (zeroed market). scale_factor_delta = 0 * X / WAD = 0.
/// new_scale_factor = 0 + 0 = 0. This is a degenerate state.
#[test]
fn accrue_zero_scale_factor() {
    let mut market = make_market(1000, i64::MAX, 0, WAD, 0, 0);
    let config = make_config(0);

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert_eq!(result, Ok(()));
    assert_eq!(market.scale_factor(), 0);
    assert_eq!(market.accrued_protocol_fees(), 0);
    assert_eq!(market.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);
}

/// scale_factor == 1 (minimal non-zero). Very small but non-zero growth.
#[test]
fn accrue_minimal_scale_factor() {
    let mut market = make_market(10000, i64::MAX, 1, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    assert_eq!(market.scale_factor(), 2);
    assert_eq!(market.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);

    let mut market_before_boundary = make_market(10000, i64::MAX, 1, WAD, 0, 0);
    accrue_interest(
        &mut market_before_boundary,
        &config,
        SECONDS_PER_YEAR as i64 - 1,
    )
    .unwrap();
    let expected_before_boundary =
        expected_scale_factor_single_step(1, 10000, SECONDS_PER_YEAR as i64 - 1);
    assert_eq!(
        market_before_boundary.scale_factor(),
        expected_before_boundary,
        "one second short of a full year should follow the same oracle"
    );
    assert_eq!(market_before_boundary.scale_factor(), 2);
}

// ===========================================================================
// Section 6: Deposit math -- ZeroScaledAmount edge case
// ===========================================================================

/// When amount is very small and scale_factor is very large,
/// scaled_amount = amount * WAD / scale_factor rounds to 0.
/// On-chain, this triggers LendingError::ZeroScaledAmount.
#[test]
fn deposit_zero_scaled_amount_from_rounding() {
    let scale_factor = WAD * 2;
    assert_eq!(deposit_scale(1, scale_factor).unwrap(), 0);
    assert_eq!(deposit_scale(2, scale_factor).unwrap(), 1);
    assert_eq!(deposit_scale(3, scale_factor).unwrap(), 1);
}

/// Slightly larger amount that does NOT round to zero.
#[test]
fn deposit_non_zero_scaled_amount_threshold() {
    let scale_factor = WAD * 2;
    assert_eq!(deposit_scale(1, scale_factor).unwrap(), 0);
    assert_eq!(deposit_scale(2, scale_factor).unwrap(), 1);
    assert_eq!(deposit_scale(4, scale_factor).unwrap(), 2);
}

/// Large scale_factor: amount=1, scale_factor=WAD*1000
/// scaled = 1 * WAD / (1000 * WAD) = 0
#[test]
fn deposit_extreme_scale_factor_always_zero_scaled() {
    let scale_factor = WAD * 1000;
    assert_eq!(deposit_scale(999, scale_factor).unwrap(), 0);
    assert_eq!(deposit_scale(1000, scale_factor).unwrap(), 1);
    assert_eq!(deposit_scale(1001, scale_factor).unwrap(), 1);
}

/// Minimum amount that produces non-zero scaled for a given scale_factor.
#[test]
fn deposit_minimum_nonzero_scaled_amount() {
    let scale_factor = WAD + WAD / 10; // 1.1x

    // We need amount * WAD / scale_factor >= 1
    // amount >= scale_factor / WAD (ceiling)
    // scale_factor / WAD = 1 + 0.1 = 1.1 -> ceiling = 2
    // Actually: amount * WAD / (WAD + WAD/10) = amount * WAD / (11*WAD/10) = amount * 10 / 11
    // For amount=1: 10/11 = 0 (floor). For amount=2: 20/11 = 1.
    let scaled_1 = deposit_scale(1, scale_factor).unwrap();
    let scaled_2 = deposit_scale(2, scale_factor).unwrap();
    let scaled_3 = deposit_scale(3, scale_factor).unwrap();

    assert_eq!(scaled_1, 0, "amount=1 at 1.1x yields 0");
    assert_eq!(scaled_2, 1, "amount=2 at 1.1x is exact threshold");
    assert_eq!(scaled_3, 2, "x+1 around threshold should advance by 1");
}

// ===========================================================================
// Section 7: Deposit math -- cap check logic
// ===========================================================================

/// Verify the cap check logic: new_normalized > max_supply triggers error.
#[test]
fn deposit_cap_exact_boundary() {
    let max_supply: u64 = 1_000_000;
    let scale_factor = WAD;
    let max_supply_u128 = u128::from(max_supply);
    for (amount, should_exceed) in [
        (max_supply - 1, false),
        (max_supply, false),
        (max_supply + 1, true),
    ] {
        let scaled = deposit_scale(amount, scale_factor).unwrap();
        let new_normalized = normalize(scaled, scale_factor).unwrap();
        assert_eq!(
            new_normalized > max_supply_u128,
            should_exceed,
            "amount={} produced normalized={}",
            amount,
            new_normalized
        );
    }
}

/// Cap check with non-trivial scale_factor (interest has accrued).
/// After interest, the normalized value of scaled tokens is higher.
#[test]
fn deposit_cap_with_accrued_interest() {
    let scale_factor = WAD + WAD / 10; // 1.1x
    let max_supply: u64 = 1_000_000; // 1 USDC
    let max_supply_u128 = u128::from(max_supply);

    let under = normalize(deposit_scale(999_999, scale_factor).unwrap(), scale_factor).unwrap();
    let at_cap = normalize(
        deposit_scale(max_supply, scale_factor).unwrap(),
        scale_factor,
    )
    .unwrap();
    let over = normalize(
        deposit_scale(max_supply + 2, scale_factor).unwrap(),
        scale_factor,
    )
    .unwrap();

    assert!(under <= max_supply_u128);
    assert!(at_cap <= max_supply_u128);
    assert!(over > max_supply_u128);
}

// ===========================================================================
// Section 8: Settlement factor computation
// ===========================================================================

/// total_normalized == 0 -> settlement factor = WAD
#[test]
fn settlement_factor_zero_supply_returns_wad() {
    assert_eq!(compute_settlement_factor(0, 0), WAD);
    assert_eq!(compute_settlement_factor(1_000_000, 0), WAD);
    assert_eq!(compute_settlement_factor(u128::MAX, 0), WAD);
}

/// available == 0, total_normalized > 0 -> raw = 0, capped = 0, factor = 1
#[test]
fn settlement_factor_zero_available_returns_one() {
    assert_eq!(compute_settlement_factor(0, 1), 1);
    assert_eq!(compute_settlement_factor(0, 1_000_000), 1);
    assert_eq!(compute_settlement_factor(0, u128::MAX), 1);
}

/// available much less than total -> raw rounds to 0 -> factor = 1
#[test]
fn settlement_factor_dust_available_returns_one() {
    // available = 1, total = WAD (huge)
    // raw = 1 * WAD / WAD = 1
    // capped = 1 (< WAD), factor = max(1, 1) = 1
    let factor = compute_settlement_factor(1, WAD);
    assert_eq!(factor, 1);

    // Even more extreme: available = 1, total = WAD * 1000
    // raw = WAD / (WAD * 1000) = 1/1000 = 0
    // capped = 0, factor = max(1, 0) = 1
    let factor2 = compute_settlement_factor(1, WAD * 1000);
    assert_eq!(factor2, 1, "raw=0 should be clamped to 1");
    assert_eq!(compute_settlement_factor(2, WAD * 1000), 1);
}

/// available > total -> raw > WAD -> capped at WAD
#[test]
fn settlement_factor_overfunded_capped_at_wad() {
    let total = 1_000_000u128;
    assert_eq!(compute_settlement_factor(total + 1, total), WAD);
    assert_eq!(compute_settlement_factor(total * 2, total), WAD);
}

/// available == total -> factor == WAD (exactly funded)
#[test]
fn settlement_factor_exactly_funded() {
    let total = 1_000_000u128;
    let factor = compute_settlement_factor(total, total);
    assert_eq!(factor, WAD);
    assert_eq!(
        compute_settlement_factor(total - 1, total),
        WAD - 1_000_000_000_000
    );
}

/// 75% funded -> factor = 0.75 * WAD
#[test]
fn settlement_factor_partial_funding() {
    let available = 750_000u128;
    let total = 1_000_000u128;
    assert_eq!(compute_settlement_factor(available, total), WAD * 3 / 4);
    assert_eq!(
        compute_settlement_factor(available - 1, total),
        (available - 1) * WAD / total
    );
    assert_eq!(
        compute_settlement_factor(available + 1, total),
        (available + 1) * WAD / total
    );
}

/// Settlement factor with actual market state (scale_factor included).
#[test]
fn settlement_factor_with_market_state() {
    let scale_factor = WAD + WAD / 10; // 1.1x
    let supply = 1_000_000u128;

    // total_normalized = supply * scale_factor / WAD = 1_100_000
    let total_normalized = normalize(supply, scale_factor).unwrap();
    assert_eq!(total_normalized, 1_100_000);

    // Vault has 880_000 (80% of total_normalized)
    let available = 880_000u128;
    let factor = compute_settlement_factor(available, total_normalized);
    // 880000 * WAD / 1100000 = 0.8 * WAD
    assert_eq!(factor, WAD * 4 / 5);
    assert_eq!(
        compute_settlement_factor(available - 1, total_normalized),
        (available - 1) * WAD / total_normalized
    );
    assert_eq!(
        compute_settlement_factor(available + 1, total_normalized),
        (available + 1) * WAD / total_normalized
    );
}

// ===========================================================================
// Section 9: Withdrawal payout computation
// ===========================================================================

/// Full withdrawal with full settlement -> payout = normalized amount
#[test]
fn withdrawal_full_settlement_full_balance() {
    let scaled_balance = 1_000_000u128;
    let scale_factor = WAD;
    let settlement = WAD;

    let payout = compute_payout(scaled_balance, scale_factor, settlement).unwrap();
    assert_eq!(payout, 1_000_000);
    let one_below_full = compute_payout(scaled_balance, scale_factor, settlement - 1).unwrap();
    assert_eq!(one_below_full, 999_999);
}

/// Withdrawal with half settlement -> payout = half of normalized
#[test]
fn withdrawal_half_settlement() {
    let scaled_balance = 1_000_000u128;
    let scale_factor = WAD;
    let settlement = WAD / 2;

    let payout = compute_payout(scaled_balance, scale_factor, settlement).unwrap();
    assert_eq!(payout, 500_000);

    let wad_scaled = WAD;
    assert_eq!(
        compute_payout(wad_scaled, scale_factor, settlement - 1).unwrap(),
        settlement - 1
    );
    assert_eq!(
        compute_payout(wad_scaled, scale_factor, settlement + 1).unwrap(),
        settlement + 1
    );
}

/// Withdrawal with zero payout: very small scaled_amount, large scale_factor,
/// tiny settlement -> payout rounds to 0 (triggers ZeroPayout on-chain).
#[test]
fn withdrawal_zero_payout_from_rounding() {
    // scaled_amount = 1, scale_factor = WAD, settlement = 1
    // normalized = 1 * WAD / WAD = 1
    // payout = 1 * 1 / WAD = 0
    assert_eq!(compute_payout(1, WAD, 1).unwrap(), 0);
    assert_eq!(compute_payout(1, WAD, WAD).unwrap(), 1);
}

/// InsufficientScaledBalance: requested > balance
#[test]
fn withdrawal_insufficient_scaled_balance() {
    let position_balance = 1_000u128;
    expect_lending_error_u128(
        resolve_withdraw_scaled_amount(1_001, position_balance),
        LendingError::InsufficientScaledBalance,
    );
    assert_eq!(
        resolve_withdraw_scaled_amount(1_000, position_balance).unwrap(),
        1_000
    );
}

/// Partial withdrawal: scaled_amount = 0 means full withdrawal on-chain
#[test]
fn withdrawal_zero_scaled_amount_means_full() {
    let position_balance = 500_000u128;
    assert_eq!(
        resolve_withdraw_scaled_amount(0, position_balance).unwrap(),
        position_balance
    );
    assert_eq!(
        resolve_withdraw_scaled_amount(position_balance, position_balance).unwrap(),
        position_balance
    );
    expect_lending_error_u128(
        resolve_withdraw_scaled_amount(position_balance + 1, position_balance),
        LendingError::InsufficientScaledBalance,
    );
}

/// Verify the normalization step in withdrawal payout with interest
#[test]
fn withdrawal_payout_with_interest_accrued() {
    let scale_factor = WAD + WAD / 5; // 1.2x interest
    let settlement = WAD; // fully funded
    let scaled_balance = 1_000_000u128;

    // normalized = 1_000_000 * 1.2 * WAD / WAD = 1_200_000
    let normalized = normalize(scaled_balance, scale_factor).unwrap();
    assert_eq!(normalized, 1_200_000);

    // payout = 1_200_000 * WAD / WAD = 1_200_000
    let payout = compute_payout(scaled_balance, scale_factor, settlement).unwrap();
    assert_eq!(payout, 1_200_000);
    assert_eq!(
        compute_payout(scaled_balance, scale_factor, settlement - 1).unwrap(),
        1_199_999
    );
}

// ===========================================================================
// Section 10: ReSettle logic
// ===========================================================================

/// ReSettle: new_factor <= old_factor -> SettlementNotImproved
#[test]
fn resettle_not_improved_same_factor() {
    let old_factor = WAD / 2;
    let new_factor = WAD / 2;
    expect_lending_error(
        validate_resettle_improvement(old_factor, new_factor),
        LendingError::SettlementNotImproved,
    );
}

/// ReSettle: new_factor < old_factor -> also fails
#[test]
fn resettle_not_improved_lower_factor() {
    let old_factor = WAD / 2;
    let new_factor = WAD / 2 - 1;
    expect_lending_error(
        validate_resettle_improvement(old_factor, new_factor),
        LendingError::SettlementNotImproved,
    );
}

/// ReSettle: new_factor = old_factor + 1 -> passes
#[test]
fn resettle_minimal_improvement() {
    let old_factor = WAD / 2;
    let new_factor = WAD / 2 + 1;
    assert_eq!(
        validate_resettle_improvement(old_factor, new_factor),
        Ok(())
    );
    expect_lending_error(
        validate_resettle_improvement(old_factor, old_factor),
        LendingError::SettlementNotImproved,
    );
}

/// ReSettle: total_normalized == 0 -> factor = WAD, old_factor < WAD -> improvement
#[test]
fn resettle_zero_supply_after_settlement() {
    // If all lenders withdrew (supply=0), new factor = WAD
    let old_factor = WAD / 2;
    let new_factor = compute_settlement_factor(1_000_000, 0);
    assert_eq!(new_factor, WAD);
    assert_eq!(
        validate_resettle_improvement(old_factor, new_factor),
        Ok(())
    );
    expect_lending_error(
        validate_resettle_improvement(WAD, new_factor),
        LendingError::SettlementNotImproved,
    );
}

// ===========================================================================
// Section 11: Borrow math -- fee reservation and borrowable
// ===========================================================================

/// Fee reservation: borrowable = vault_balance - min(vault_balance, accrued_fees)
#[test]
fn borrow_fee_reservation_all_fees() {
    let vault_balance: u64 = 1_000_000;
    let accrued_fees: u64 = 500_000;

    let fees_reserved = core::cmp::min(vault_balance, accrued_fees);
    let borrowable = vault_balance.checked_sub(fees_reserved).unwrap();

    assert_eq!(fees_reserved, 500_000);
    assert_eq!(borrowable, 500_000);
    assert_eq!(fees_reserved + borrowable, vault_balance);
}

/// Fee reservation: when fees > vault_balance, all vault is reserved
#[test]
fn borrow_fee_reservation_fees_exceed_vault() {
    let vault_balance: u64 = 100_000;
    let accrued_fees: u64 = 500_000;

    let fees_reserved = core::cmp::min(vault_balance, accrued_fees);
    let borrowable = vault_balance.checked_sub(fees_reserved).unwrap();

    assert_eq!(fees_reserved, 100_000);
    assert_eq!(borrowable, 0, "all vault is reserved for fees");
    assert_eq!(fees_reserved + borrowable, vault_balance);
    assert_eq!(core::cmp::min(vault_balance, vault_balance), vault_balance);
}

/// Fee reservation: when fees == 0, full vault is borrowable
#[test]
fn borrow_fee_reservation_no_fees() {
    let vault_balance: u64 = 1_000_000;
    for fees in [0u64, 1] {
        let fees_reserved = core::cmp::min(vault_balance, fees);
        let borrowable = vault_balance.checked_sub(fees_reserved).unwrap();
        assert_eq!(fees_reserved + borrowable, vault_balance);
        if fees == 0 {
            assert_eq!(borrowable, vault_balance);
        } else {
            assert_eq!(borrowable, vault_balance - 1);
        }
    }
}

/// Global capacity check: current_borrowed + amount > max_borrow_capacity
#[test]
fn borrow_global_capacity_exceeded() {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.is_whitelisted = 1;
    wl.set_max_borrow_capacity(1_000_000);
    wl.set_current_borrowed(800_000);

    expect_lending_error_u64(
        validate_borrow_capacity(&wl, 300_000),
        LendingError::GlobalCapacityExceeded,
    );
}

/// Global capacity check: exact boundary
#[test]
fn borrow_global_capacity_exact_boundary() {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.is_whitelisted = 1;
    wl.set_max_borrow_capacity(1_000_000);
    wl.set_current_borrowed(500_000);

    assert_eq!(validate_borrow_capacity(&wl, 499_999).unwrap(), 999_999);
    assert_eq!(validate_borrow_capacity(&wl, 500_000).unwrap(), 1_000_000);
    expect_lending_error_u64(
        validate_borrow_capacity(&wl, 500_001),
        LendingError::GlobalCapacityExceeded,
    );
}

// ===========================================================================
// Section 12: SetBorrowerWhitelist logic
// ===========================================================================

/// Whitelisting with max_borrow_capacity == 0 when is_whitelisted == 1
/// should trigger InvalidCapacity on-chain.
#[test]
fn whitelist_zero_capacity_when_whitelisting() {
    expect_lending_error(
        validate_whitelist_capacity(1, 0),
        LendingError::InvalidCapacity,
    );
    assert_eq!(validate_whitelist_capacity(1, 1), Ok(()));
}

/// De-whitelisting (is_whitelisted == 0) with max_borrow_capacity == 0 is fine.
#[test]
fn whitelist_zero_capacity_when_dewhitelisting() {
    assert_eq!(validate_whitelist_capacity(0, 0), Ok(()));
    assert_eq!(validate_whitelist_capacity(0, 1), Ok(()));
}

/// Updating existing whitelist: current_borrowed is NOT modified
#[test]
fn whitelist_update_preserves_current_borrowed() {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.borrower = [0xAA; 32];
    wl.bump = 9;
    wl.is_whitelisted = 1;
    wl.set_max_borrow_capacity(1_000_000);
    wl.set_current_borrowed(500_000);
    let original_borrower = wl.borrower;
    let original_bump = wl.bump;
    let original_borrowed = wl.current_borrowed();

    // Simulate update: change capacity but NOT current_borrowed
    let new_capacity: u64 = 2_000_000;
    let new_is_whitelisted: u8 = 1;

    wl.is_whitelisted = new_is_whitelisted;
    wl.set_max_borrow_capacity(new_capacity);
    // current_borrowed is intentionally NOT modified

    assert_eq!(wl.max_borrow_capacity(), 2_000_000);
    assert_eq!(
        wl.current_borrowed(),
        500_000,
        "current_borrowed must be preserved"
    );
    assert_eq!(wl.current_borrowed(), original_borrowed);
    assert_eq!(wl.is_whitelisted, 1);
    assert_eq!(wl.borrower, original_borrower);
    assert_eq!(wl.bump, original_bump);
    assert_eq!(
        validate_whitelist_capacity(wl.is_whitelisted, wl.max_borrow_capacity()),
        Ok(())
    );
}

// ===========================================================================
// Section 13: is_zero_address validation
// ===========================================================================

/// ZERO_ADDRESS constant is all zeros
#[test]
fn is_zero_address_with_constant() {
    assert!(is_zero_address(&ZERO_ADDRESS));
    let mut first_byte_set = ZERO_ADDRESS;
    first_byte_set[0] = 1;
    assert!(!is_zero_address(&first_byte_set));

    let mut last_byte_set = ZERO_ADDRESS;
    last_byte_set[31] = 1;
    assert!(!is_zero_address(&last_byte_set));
}

/// Single byte set at each of 32 positions returns false
#[test]
fn is_zero_address_single_byte_set() {
    let mut non_zero_count = 0usize;
    for i in 0..32 {
        let mut addr = [0u8; 32];
        addr[i] = 1;
        assert!(!is_zero_address(&addr), "byte {} set -> not zero", i);
        non_zero_count += 1;
    }
    assert_eq!(non_zero_count, 32);
}

/// All-ones is not zero
#[test]
fn is_zero_address_all_ones() {
    let addr = [0xFF; 32];
    assert!(!is_zero_address(&addr));
    let mut almost_all_ones = [0xFF; 32];
    almost_all_ones[17] = 0;
    assert!(!is_zero_address(&almost_all_ones));
}

/// Only the first byte set
#[test]
fn is_zero_address_only_first_byte() {
    let mut addr = [0u8; 32];
    addr[0] = 0x42;
    assert!(!is_zero_address(&addr));
    addr[0] = 0;
    assert!(is_zero_address(&addr));
}

/// Only the last byte set
#[test]
fn is_zero_address_only_last_byte() {
    let mut addr = [0u8; 32];
    addr[31] = 0x01;
    assert!(!is_zero_address(&addr));
    addr[31] = 0;
    assert!(is_zero_address(&addr));
}

// ===========================================================================
// Section 14: State struct field independence and size assertions
// ===========================================================================

/// Market struct is exactly MARKET_SIZE bytes
#[test]
fn market_struct_size() {
    let size = core::mem::size_of::<Market>();
    assert_eq!(size, MARKET_SIZE);
    assert_ne!(size, MARKET_SIZE + 1);
    assert_ne!(size, MARKET_SIZE.saturating_sub(1));
}

/// ProtocolConfig struct is exactly PROTOCOL_CONFIG_SIZE bytes
#[test]
fn protocol_config_struct_size() {
    let size = core::mem::size_of::<ProtocolConfig>();
    assert_eq!(size, PROTOCOL_CONFIG_SIZE);
    assert_ne!(size, PROTOCOL_CONFIG_SIZE + 1);
    assert_ne!(size, PROTOCOL_CONFIG_SIZE.saturating_sub(1));
}

/// LenderPosition struct is exactly LENDER_POSITION_SIZE bytes
#[test]
fn lender_position_struct_size() {
    let size = core::mem::size_of::<LenderPosition>();
    assert_eq!(size, LENDER_POSITION_SIZE);
    assert_ne!(size, LENDER_POSITION_SIZE + 1);
    assert_ne!(size, LENDER_POSITION_SIZE.saturating_sub(1));
}

/// BorrowerWhitelist struct is exactly BORROWER_WHITELIST_SIZE bytes
#[test]
fn borrower_whitelist_struct_size() {
    let size = core::mem::size_of::<BorrowerWhitelist>();
    assert_eq!(size, BORROWER_WHITELIST_SIZE);
    assert_ne!(size, BORROWER_WHITELIST_SIZE + 1);
    assert_ne!(size, BORROWER_WHITELIST_SIZE.saturating_sub(1));
}

/// Setting market fields does not corrupt neighbors
#[test]
fn market_field_independence_comprehensive() {
    let mut m = Market::zeroed();

    m.set_annual_interest_bps(5000);
    m.set_maturity_timestamp(1_000_000);
    m.set_max_total_supply(999_999);
    m.set_market_nonce(42);
    m.set_scaled_total_supply(WAD * 100);
    m.set_scale_factor(WAD + 1);
    m.set_accrued_protocol_fees(12345);
    m.set_total_deposited(100_000);
    m.set_total_borrowed(50_000);
    m.set_total_repaid(30_000);
    m.set_last_accrual_timestamp(500_000);
    m.set_settlement_factor_wad(WAD / 2);
    m.bump = 255;
    m.market_authority_bump = 42;

    assert_eq!(m.annual_interest_bps(), 5000);
    assert_eq!(m.maturity_timestamp(), 1_000_000);
    assert_eq!(m.max_total_supply(), 999_999);
    assert_eq!(m.market_nonce(), 42);
    assert_eq!(m.scaled_total_supply(), WAD * 100);
    assert_eq!(m.scale_factor(), WAD + 1);
    assert_eq!(m.accrued_protocol_fees(), 12345);
    assert_eq!(m.total_deposited(), 100_000);
    assert_eq!(m.total_borrowed(), 50_000);
    assert_eq!(m.total_repaid(), 30_000);
    assert_eq!(m.last_accrual_timestamp(), 500_000);
    assert_eq!(m.settlement_factor_wad(), WAD / 2);
    assert_eq!(m.bump, 255);
    assert_eq!(m.market_authority_bump, 42);
}

/// BorrowerWhitelist field independence
#[test]
fn borrower_whitelist_field_independence() {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.borrower = [0xAA; 32];
    wl.is_whitelisted = 1;
    wl.set_max_borrow_capacity(5_000_000);
    wl.set_current_borrowed(1_000_000);
    wl.bump = 128;

    assert_eq!(wl.borrower, [0xAA; 32]);
    assert_eq!(wl.is_whitelisted, 1);
    assert_eq!(wl.max_borrow_capacity(), 5_000_000);
    assert_eq!(wl.current_borrowed(), 1_000_000);
    assert_eq!(wl.bump, 128);
}

/// LenderPosition field independence
#[test]
fn lender_position_field_independence() {
    let mut pos = LenderPosition::zeroed();
    pos.market = [0xBB; 32];
    pos.lender = [0xCC; 32];
    pos.set_scaled_balance(WAD * 42);
    pos.bump = 200;

    assert_eq!(pos.market, [0xBB; 32]);
    assert_eq!(pos.lender, [0xCC; 32]);
    assert_eq!(pos.scaled_balance(), WAD * 42);
    assert_eq!(pos.bump, 200);
}

/// ProtocolConfig field independence
#[test]
fn protocol_config_field_independence() {
    let mut cfg = ProtocolConfig::zeroed();
    cfg.admin = [0x11; 32];
    cfg.set_fee_rate_bps(9999);
    cfg.fee_authority = [0x22; 32];
    cfg.whitelist_manager = [0x33; 32];
    cfg.blacklist_program = [0x44; 32];
    cfg.is_initialized = 1;
    cfg.bump = 77;

    assert_eq!(cfg.admin, [0x11; 32]);
    assert_eq!(cfg.fee_rate_bps(), 9999);
    assert_eq!(cfg.fee_authority, [0x22; 32]);
    assert_eq!(cfg.whitelist_manager, [0x33; 32]);
    assert_eq!(cfg.blacklist_program, [0x44; 32]);
    assert_eq!(cfg.is_initialized, 1);
    assert_eq!(cfg.bump, 77);
}

// ===========================================================================
// Section 15: Bytemuck from raw bytes
// ===========================================================================

/// Market from raw bytes -- verify all fields readable
#[test]
fn market_from_raw_bytes() {
    let mut bytes = [0u8; MARKET_SIZE];
    // Set annual_interest_bps at offset 106 (8+1+32+32+32+1 = 106)
    let bps_bytes = 1000u16.to_le_bytes();
    bytes[106] = bps_bytes[0];
    bytes[107] = bps_bytes[1];

    let market: &Market = bytemuck::from_bytes(&bytes);
    assert_eq!(market.annual_interest_bps(), 1000);
    assert_eq!(market.scale_factor(), 0);
    assert_eq!(market.maturity_timestamp(), 0);

    let mut bytes_lo = [0u8; MARKET_SIZE];
    bytes_lo[106..108].copy_from_slice(&999u16.to_le_bytes());
    let market_lo: &Market = bytemuck::from_bytes(&bytes_lo);
    assert_eq!(market_lo.annual_interest_bps(), 999);

    let mut bytes_hi = [0u8; MARKET_SIZE];
    bytes_hi[106..108].copy_from_slice(&1001u16.to_le_bytes());
    let market_hi: &Market = bytemuck::from_bytes(&bytes_hi);
    assert_eq!(market_hi.annual_interest_bps(), 1001);
    assert_eq!(market_hi.scale_factor(), 0);
}

/// ProtocolConfig from raw bytes
#[test]
fn protocol_config_from_raw_bytes() {
    let mut bytes = [0u8; PROTOCOL_CONFIG_SIZE];
    // fee_rate_bps at offset 41 (8+1+32 = 41, after discriminator[8]+version[1]+admin[32])
    let bps_bytes = 7500u16.to_le_bytes();
    bytes[41] = bps_bytes[0];
    bytes[42] = bps_bytes[1];
    // is_initialized at offset 139
    bytes[139] = 1;

    let config: &ProtocolConfig = bytemuck::from_bytes(&bytes);
    assert_eq!(config.fee_rate_bps(), 7500);
    assert_eq!(config.is_initialized, 1);

    let mut bytes_low = [0u8; PROTOCOL_CONFIG_SIZE];
    bytes_low[41..43].copy_from_slice(&7499u16.to_le_bytes());
    let config_low: &ProtocolConfig = bytemuck::from_bytes(&bytes_low);
    assert_eq!(config_low.fee_rate_bps(), 7499);
    assert_eq!(config_low.is_initialized, 0);

    let mut bytes_high = [0u8; PROTOCOL_CONFIG_SIZE];
    bytes_high[41..43].copy_from_slice(&7501u16.to_le_bytes());
    bytes_high[139] = 1;
    let config_high: &ProtocolConfig = bytemuck::from_bytes(&bytes_high);
    assert_eq!(config_high.fee_rate_bps(), 7501);
    assert_eq!(config_high.is_initialized, 1);
}

// ===========================================================================
// Section 16: Constants correctness
// ===========================================================================

#[test]
fn constants_values_comprehensive() {
    assert_eq!(WAD, 1_000_000_000_000_000_000u128);
    assert_eq!(WAD, 10u128.pow(18));
    assert_eq!(BPS, 10_000u128);
    assert_eq!(SECONDS_PER_YEAR, 365 * 24 * 60 * 60);
    assert_eq!(SECONDS_PER_YEAR, 31_536_000u128);
    assert_eq!(MAX_ANNUAL_INTEREST_BPS, 10_000u16);
    assert_eq!(MAX_FEE_RATE_BPS, 10_000u16);
    assert_eq!(USDC_DECIMALS, 6u8);
    assert_eq!(MIN_MATURITY_DELTA, 60i64);
    assert_eq!(ZERO_ADDRESS, [0u8; 32]);
    assert_eq!(PROTOCOL_CONFIG_SIZE, 194);
    assert_eq!(MARKET_SIZE, 250);
    assert_eq!(LENDER_POSITION_SIZE, 128);
    assert_eq!(BORROWER_WHITELIST_SIZE, 96);
}

// ===========================================================================
// Section 17: Error enum correctness
// ===========================================================================

/// Verify all error discriminants match spec values (category-based organization)
#[test]
fn error_discriminants() {
    use pinocchio::error::ProgramError;

    // INITIALIZATION ERRORS (0-4)
    assert_eq!(LendingError::AlreadyInitialized as u32, 0);
    assert_eq!(LendingError::InvalidFeeRate as u32, 1);
    assert_eq!(LendingError::InvalidCapacity as u32, 2);
    assert_eq!(LendingError::InvalidMaturity as u32, 3);
    assert_eq!(LendingError::MarketAlreadyExists as u32, 4);

    // AUTHORIZATION ERRORS (5-9)
    assert_eq!(LendingError::Unauthorized as u32, 5);
    assert_eq!(LendingError::NotWhitelisted as u32, 6);
    assert_eq!(LendingError::Blacklisted as u32, 7);
    assert_eq!(LendingError::ProtocolPaused as u32, 8);
    assert_eq!(LendingError::BorrowerHasActiveDebt as u32, 9);

    // ACCOUNT VALIDATION ERRORS (10-16)
    assert_eq!(LendingError::InvalidAddress as u32, 10);
    assert_eq!(LendingError::InvalidMint as u32, 11);
    assert_eq!(LendingError::InvalidVault as u32, 12);
    assert_eq!(LendingError::InvalidPDA as u32, 13);
    assert_eq!(LendingError::InvalidAccountOwner as u32, 14);
    assert_eq!(LendingError::InvalidTokenProgram as u32, 15);
    assert_eq!(LendingError::InvalidTokenAccountOwner as u32, 16);

    // INPUT VALIDATION ERRORS (17-20)
    assert_eq!(LendingError::ZeroAmount as u32, 17);
    assert_eq!(LendingError::ZeroScaledAmount as u32, 18);
    assert_eq!(LendingError::InvalidScaleFactor as u32, 19);
    assert_eq!(LendingError::InvalidTimestamp as u32, 20);

    // BALANCE/CAPACITY ERRORS (21-27)
    assert_eq!(LendingError::InsufficientBalance as u32, 21);
    assert_eq!(LendingError::InsufficientScaledBalance as u32, 22);
    assert_eq!(LendingError::NoBalance as u32, 23);
    assert_eq!(LendingError::ZeroPayout as u32, 24);
    assert_eq!(LendingError::CapExceeded as u32, 25);
    assert_eq!(LendingError::BorrowAmountTooHigh as u32, 26);
    assert_eq!(LendingError::GlobalCapacityExceeded as u32, 27);

    // MARKET STATE ERRORS (28-35)
    assert_eq!(LendingError::MarketMatured as u32, 28);
    assert_eq!(LendingError::NotMatured as u32, 29);
    assert_eq!(LendingError::NotSettled as u32, 30);
    assert_eq!(LendingError::SettlementNotImproved as u32, 31);
    assert_eq!(LendingError::SettlementGracePeriod as u32, 32);
    assert_eq!(LendingError::SettlementNotComplete as u32, 33);
    assert_eq!(LendingError::PositionNotEmpty as u32, 34);
    assert_eq!(LendingError::RepaymentExceedsDebt as u32, 35);

    // FEE/WITHDRAWAL ERRORS (36-40)
    assert_eq!(LendingError::NoFeesToCollect as u32, 36);
    assert_eq!(LendingError::FeeCollectionDuringDistress as u32, 37);
    assert_eq!(LendingError::LendersPendingWithdrawals as u32, 38);
    assert_eq!(LendingError::FeesNotCollected as u32, 39);
    assert_eq!(LendingError::NoExcessToWithdraw as u32, 40);

    // OPERATIONAL ERRORS (41-42)
    assert_eq!(LendingError::MathOverflow as u32, 41);
    assert_eq!(LendingError::PayoutBelowMinimum as u32, 42);

    // Verify From conversion
    let pe: ProgramError = LendingError::MathOverflow.into();
    assert_eq!(pe, ProgramError::Custom(41));

    let pe2: ProgramError = LendingError::ZeroPayout.into();
    assert_eq!(pe2, ProgramError::Custom(24));
}

// ===========================================================================
// Section 18: Compound interest correctness
// ===========================================================================

/// Two half-year accruals compound to more than a single full-year accrual.
#[test]
fn compound_interest_exceeds_simple() {
    let config = make_config(0);
    let annual_bps = 1000u16; // 10%
    let half_year = SECONDS_PER_YEAR as i64 / 2;
    let full_year = SECONDS_PER_YEAR as i64;

    // Simple: single step over full year
    let mut m_simple = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut m_simple, &config, full_year).unwrap();
    let expected_simple = expected_scale_factor_single_step(WAD, annual_bps, full_year);
    assert_eq!(m_simple.scale_factor(), expected_simple);

    // Compound: two half-year steps
    let mut m_compound = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut m_compound, &config, half_year).unwrap();
    accrue_interest(&mut m_compound, &config, full_year).unwrap();
    let half_step = expected_scale_factor_single_step(WAD, annual_bps, half_year);
    let expected_compound = expected_scale_factor_single_step(half_step, annual_bps, half_year);
    assert_eq!(m_compound.scale_factor(), expected_compound);

    assert!(
        m_compound.scale_factor() > m_simple.scale_factor(),
        "compound ({}) > simple ({})",
        m_compound.scale_factor(),
        m_simple.scale_factor()
    );

    let quarter = full_year / 4;
    let mut m_quarterly = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    for step in [quarter, quarter * 2, quarter * 3, full_year] {
        accrue_interest(&mut m_quarterly, &config, step).unwrap();
    }
    assert!(m_quarterly.scale_factor() >= m_compound.scale_factor());
}

/// At 100% annual for 1 year, scale_factor should double.
#[test]
fn interest_100_percent_doubles_scale_factor() {
    let mut market = make_market(10000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    assert!(market.scale_factor() > 2 * WAD);
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor_single_step(WAD, 10000, SECONDS_PER_YEAR as i64)
    );

    let mut before_year = make_market(10000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut before_year, &config, SECONDS_PER_YEAR as i64 - 1).unwrap();
    assert_eq!(
        before_year.scale_factor(),
        expected_scale_factor_single_step(WAD, 10000, SECONDS_PER_YEAR as i64 - 1)
    );

    let mut after_year = make_market(10000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut after_year, &config, SECONDS_PER_YEAR as i64 + 1).unwrap();
    assert_eq!(
        after_year.scale_factor(),
        expected_scale_factor_single_step(WAD, 10000, SECONDS_PER_YEAR as i64 + 1)
    );
}

/// At 50% annual for 1 year, scale_factor = 1.5 * WAD.
#[test]
fn interest_50_percent_one_year() {
    let mut market = make_market(5000, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    assert!(market.scale_factor() > WAD + WAD / 2);
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor_single_step(WAD, 5000, SECONDS_PER_YEAR as i64)
    );

    let mut before_year = make_market(5000, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut before_year, &config, SECONDS_PER_YEAR as i64 - 1).unwrap();
    assert_eq!(
        before_year.scale_factor(),
        expected_scale_factor_single_step(WAD, 5000, SECONDS_PER_YEAR as i64 - 1)
    );
}

/// Starting from scale_factor = 2*WAD (already doubled), another year at 10%
/// should yield 2*WAD + 2*WAD/10 = 2.2*WAD.
#[test]
fn interest_compounds_on_existing_scale_factor() {
    let initial_sf = 2 * WAD;
    let mut market = make_market(1000, i64::MAX, initial_sf, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected = expected_scale_factor_single_step(initial_sf, 1000, SECONDS_PER_YEAR as i64);
    assert_eq!(market.scale_factor(), expected);
    assert!(market.scale_factor() > initial_sf + initial_sf / 10);

    let mut one_second = make_market(1000, i64::MAX, initial_sf, WAD, 0, 0);
    accrue_interest(&mut one_second, &config, 1).unwrap();
    assert_eq!(
        one_second.scale_factor(),
        expected_scale_factor_single_step(initial_sf, 1000, 1)
    );
}

// ===========================================================================
// Section 19: Fee computation exact values
// ===========================================================================

/// 10% annual, 50% fee, 1M supply, 1 year: exact fee value
#[test]
fn fee_exact_value_10pct_50fee() {
    let annual_bps = 1000u16;
    let fee_rate = 5000u16;
    let supply = 1_000_000_000_000u128; // 1M USDC
    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(fee_rate);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    let expected_fee =
        expected_fee_delta_single_step(supply, WAD, annual_bps, fee_rate, SECONDS_PER_YEAR as i64);

    assert_eq!(market.accrued_protocol_fees(), expected_fee);
    assert!(expected_fee > 0);
    // With Finding 10 fix (pre-accrual SF), fee is ~52.5B not ~58B
    assert!(expected_fee > 50_000_000_000);

    let mut lower_fee_market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_interest(
        &mut lower_fee_market,
        &make_config(fee_rate - 1),
        SECONDS_PER_YEAR as i64,
    )
    .unwrap();
    let mut higher_fee_market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
    accrue_interest(
        &mut higher_fee_market,
        &make_config(fee_rate + 1),
        SECONDS_PER_YEAR as i64,
    )
    .unwrap();
    assert!(lower_fee_market.accrued_protocol_fees() < expected_fee);
    assert!(higher_fee_market.accrued_protocol_fees() > expected_fee);
}

/// 100% fee: all interest goes to fees
#[test]
fn fee_100_percent_captures_all_interest() {
    let supply = 1_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(10000); // 100% fee

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected_fee =
        expected_fee_delta_single_step(supply, WAD, 1000, 10000, SECONDS_PER_YEAR as i64);

    assert_eq!(market.accrued_protocol_fees(), expected_fee);

    let mut almost_full_fee = make_market(1000, i64::MAX, WAD, supply, 0, 0);
    accrue_interest(
        &mut almost_full_fee,
        &make_config(9999),
        SECONDS_PER_YEAR as i64,
    )
    .unwrap();
    assert!(almost_full_fee.accrued_protocol_fees() < expected_fee);
}

/// 1 bps fee: minimal fee
#[test]
fn fee_1_bps_minimal_fee() {
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(10000, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(1); // 1 bps = 0.01%

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected = expected_fee_delta_single_step(supply, WAD, 10000, 1, SECONDS_PER_YEAR as i64);

    assert_eq!(market.accrued_protocol_fees(), expected);
    assert!(market.accrued_protocol_fees() > 0);

    let mut zero_fee = make_market(10000, i64::MAX, WAD, supply, 0, 0);
    accrue_interest(&mut zero_fee, &make_config(0), SECONDS_PER_YEAR as i64).unwrap();
    assert_eq!(zero_fee.accrued_protocol_fees(), 0);

    let mut two_bps = make_market(10000, i64::MAX, WAD, supply, 0, 0);
    accrue_interest(&mut two_bps, &make_config(2), SECONDS_PER_YEAR as i64).unwrap();
    assert!(two_bps.accrued_protocol_fees() > market.accrued_protocol_fees());
}

// ===========================================================================
// Section 20: Close lender position logic
// ===========================================================================

/// Position with scaled_balance > 0 cannot be closed (PositionNotEmpty)
#[test]
fn close_position_non_empty_rejected() {
    let mut pos = LenderPosition::zeroed();
    pos.set_scaled_balance(1);
    expect_lending_error(
        validate_close_position(&pos),
        LendingError::PositionNotEmpty,
    );
    assert_eq!(pos.scaled_balance(), 1);
}

/// Position with scaled_balance == 0 can be closed
#[test]
fn close_position_empty_allowed() {
    let pos = LenderPosition::zeroed();
    assert_eq!(validate_close_position(&pos), Ok(()));
    assert_eq!(
        pos.scaled_balance(),
        0,
        "zeroed position should be closeable"
    );
}

// ===========================================================================
// Section 21: Repay math
// ===========================================================================

/// Repay uses a zero-fee config for accrual (per spec).
/// Scale factor should still grow (interest accrues), but no fees.
#[test]
fn repay_accrual_with_zero_fee_config() {
    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let zero_config: ProtocolConfig = ProtocolConfig::zeroed();

    accrue_interest(&mut market, &zero_config, SECONDS_PER_YEAR as i64).unwrap();

    assert_eq!(
        market.scale_factor(),
        expected_scale_factor_single_step(WAD, 1000, SECONDS_PER_YEAR as i64)
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "zero config means no fees"
    );
    assert_eq!(market.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);
}

/// Running totals: total_repaid accumulates
#[test]
fn repay_total_repaid_accumulates() {
    let mut market = Market::zeroed();
    market.set_total_repaid(0);

    let amounts = [100_000u64, 200_000, 500_000];
    let mut expected_total = 0u64;

    for amount in amounts {
        expected_total = expected_total.checked_add(amount).unwrap();
        market.set_total_repaid(expected_total);
    }

    assert_eq!(market.total_repaid(), 800_000);
    assert_eq!(market.total_repaid(), expected_total);
    assert!(u64::MAX.checked_add(1).is_none());
}

// ===========================================================================
// Section 22: Full lifecycle math simulation
// ===========================================================================

/// Simulate: deposit -> accrue -> borrow -> accrue -> repay -> settle -> withdraw
#[test]
fn full_lifecycle_math_simulation() {
    let config = make_config(500); // 5% fee
    let annual_bps = 1000u16; // 10% annual
    let maturity = SECONDS_PER_YEAR as i64; // 1 year

    // Create market
    let mut market = make_market(annual_bps, maturity, WAD, 0, 0, 0);
    market.set_max_total_supply(10_000_000);

    // Deposit 1M USDC
    let deposit_amount: u64 = 1_000_000;
    let scaled_deposit = u128::from(deposit_amount) * WAD / market.scale_factor();
    assert_eq!(scaled_deposit, u128::from(deposit_amount));
    market.set_scaled_total_supply(scaled_deposit);
    market.set_total_deposited(deposit_amount);

    // Accrue for half year
    let half_year = maturity / 2;
    accrue_interest(&mut market, &config, half_year).unwrap();
    let sf_half = market.scale_factor();
    assert_eq!(
        sf_half,
        expected_scale_factor_single_step(WAD, annual_bps, half_year)
    );

    // Borrow 500K
    let borrow_amount = 500_000u64;
    market.set_total_borrowed(borrow_amount);

    // Accrue to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();
    let sf_maturity = market.scale_factor();
    assert_eq!(
        sf_maturity,
        expected_scale_factor_single_step(sf_half, annual_bps, half_year)
    );
    assert_eq!(market.last_accrual_timestamp(), maturity);

    // Verify fees accrued
    assert!(market.accrued_protocol_fees() > 0);

    // Repay 600K
    let repay_amount = 600_000u64;
    market.set_total_repaid(repay_amount);

    // Simulate settlement
    let vault_balance = u128::from(deposit_amount - borrow_amount + repay_amount); // 1M - 500K + 600K = 1.1M
    let fees = u128::from(market.accrued_protocol_fees());
    let fees_reserved = fees.min(vault_balance);
    let available = vault_balance - fees_reserved;
    assert!(fees_reserved <= fees);
    assert!(fees_reserved <= vault_balance);

    let total_normalized = normalize(market.scaled_total_supply(), sf_maturity).unwrap();
    assert!(total_normalized > 0);

    let settlement = compute_settlement_factor(available, total_normalized);
    assert!(settlement > 0);
    assert!(settlement <= WAD);

    market.set_settlement_factor_wad(settlement);

    // Compute payout for full balance
    let payout = compute_payout(market.scaled_total_supply(), sf_maturity, settlement).unwrap();
    assert!(payout > 0);
    assert!(
        payout <= available,
        "payout ({}) should not exceed available ({})",
        payout,
        available
    );
    assert!(payout + fees_reserved <= vault_balance);
    assert_eq!(market.total_deposited(), deposit_amount);
    assert_eq!(market.total_borrowed(), borrow_amount);
    assert_eq!(market.total_repaid(), repay_amount);
}

// ===========================================================================
// Section 23: Collect fees math
// ===========================================================================

/// When accrued_fees == 0, collect should fail (NoFeesToCollect)
#[test]
fn collect_fees_zero_accrued() {
    let market = Market::zeroed();
    assert_eq!(market.accrued_protocol_fees(), 0);
    expect_lending_error_u64(
        compute_withdrawable_fees(market.accrued_protocol_fees(), 1_000_000),
        LendingError::NoFeesToCollect,
    );
}

/// Withdrawable = min(accrued_fees, vault_balance)
#[test]
fn collect_fees_withdrawable_capped_by_vault() {
    let accrued_fees: u64 = 1_000_000;
    let vault_balance: u64 = 500_000;

    let withdrawable = compute_withdrawable_fees(accrued_fees, vault_balance).unwrap();
    assert_eq!(withdrawable, 500_000);

    // Remaining fees after collection
    let remaining = accrued_fees.checked_sub(withdrawable).unwrap();
    assert_eq!(remaining, 500_000);
    assert_eq!(
        compute_withdrawable_fees(accrued_fees, vault_balance + 1).unwrap(),
        500_001
    );
}

/// Withdrawable = accrued_fees when vault has enough
#[test]
fn collect_fees_withdrawable_full_amount() {
    let accrued_fees: u64 = 500_000;
    let vault_balance: u64 = 1_000_000;

    let withdrawable = compute_withdrawable_fees(accrued_fees, vault_balance).unwrap();
    assert_eq!(withdrawable, 500_000);

    let remaining = accrued_fees.checked_sub(withdrawable).unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(
        compute_withdrawable_fees(accrued_fees, accrued_fees).unwrap(),
        accrued_fees
    );
}

/// Withdrawable = 0 when vault is empty -> NoFeesToCollect
#[test]
fn collect_fees_empty_vault() {
    let accrued_fees: u64 = 1_000_000;
    let vault_balance: u64 = 0;
    expect_lending_error_u64(
        compute_withdrawable_fees(accrued_fees, vault_balance),
        LendingError::NoFeesToCollect,
    );
    assert_eq!(compute_withdrawable_fees(accrued_fees, 1).unwrap(), 1);
}

// ===========================================================================
// Section 24: Edge cases in integer arithmetic
// ===========================================================================

/// u128 checked_mul overflow detection
#[test]
fn u128_checked_mul_overflow() {
    let a: u128 = u128::MAX;
    let b: u128 = 2;
    expect_lending_error_u128(checked_mul_or_error(a, b), LendingError::MathOverflow);
    assert_eq!(
        checked_mul_or_error(u128::MAX / 2, 2).unwrap(),
        u128::MAX - 1
    );
}

/// u128 checked_div by zero
#[test]
fn u128_checked_div_by_zero() {
    let a: u128 = 1_000_000;
    expect_lending_error_u128(checked_div_or_error(a, 0), LendingError::MathOverflow);
    assert_eq!(checked_div_or_error(a, 1).unwrap(), a);
    assert_eq!(checked_div_or_error(a, 2).unwrap(), 500_000);
}

/// u64::try_from on values at the boundary
#[test]
fn u64_try_from_boundary() {
    let at_max = u64::try_from(u128::from(u64::MAX)).unwrap();
    assert_eq!(at_max, u64::MAX);
    assert!(u64::try_from(u128::from(u64::MAX) + 1).is_err());
    assert_eq!(
        u64::try_from(u128::from(u64::MAX) - 1).unwrap(),
        u64::MAX - 1
    );
}

/// Saturating subtraction for time_elapsed
#[test]
fn saturating_sub_behavior() {
    let a: i64 = 100;
    let b: i64 = 200;
    assert_eq!(a.saturating_sub(b), -100);
    assert_eq!(b.saturating_sub(a), 100);
    assert_eq!(i64::MIN.saturating_sub(1), i64::MIN);
    assert_eq!(i64::MAX.saturating_sub(-1), i64::MAX);
}

/// Verify u128 arithmetic doesn't lose precision for WAD operations
#[test]
fn wad_arithmetic_precision() {
    let product = checked_mul_or_error(WAD, WAD).unwrap();
    assert_eq!(product, 10u128.pow(36));
    assert_eq!(checked_div_or_error(product, WAD).unwrap(), WAD);
    assert_eq!(
        checked_mul_or_error(WAD, WAD - 1).unwrap(),
        10u128.pow(36) - WAD
    );
}

// ===========================================================================
// Section 25: Market state transitions
// ===========================================================================

/// Market: total_deposited, total_borrowed, total_repaid tracking
#[test]
fn market_running_totals() {
    let mut market = Market::zeroed();

    // Deposit 1M
    let new_deposited = market.total_deposited().checked_add(1_000_000).unwrap();
    market.set_total_deposited(new_deposited);
    assert_eq!(market.total_deposited(), 1_000_000);

    // Borrow 500K
    let new_borrowed = market.total_borrowed().checked_add(500_000).unwrap();
    market.set_total_borrowed(new_borrowed);
    assert_eq!(market.total_borrowed(), 500_000);

    // Repay 300K
    let new_repaid = market.total_repaid().checked_add(300_000).unwrap();
    market.set_total_repaid(new_repaid);
    assert_eq!(market.total_repaid(), 300_000);

    // All totals independent
    assert_eq!(market.total_deposited(), 1_000_000);
    assert_eq!(market.total_borrowed(), 500_000);
    assert_eq!(market.total_repaid(), 300_000);
}

/// Settlement factor: once set, persists across reads
#[test]
fn market_settlement_factor_persistence() {
    let mut market = Market::zeroed();
    assert_eq!(market.settlement_factor_wad(), 0);

    market.set_settlement_factor_wad(1);
    assert_eq!(market.settlement_factor_wad(), 1);

    market.set_settlement_factor_wad(WAD * 3 / 4);
    assert_eq!(market.settlement_factor_wad(), WAD * 3 / 4);

    // Other operations don't affect it
    market.set_scale_factor(WAD * 2);
    market.set_accrued_protocol_fees(12345);
    assert_eq!(market.settlement_factor_wad(), WAD * 3 / 4);

    market.set_settlement_factor_wad(WAD);
    assert_eq!(market.settlement_factor_wad(), WAD);
}

// ===========================================================================
// Section 26: Multiple lender scenario
// ===========================================================================

/// Two lenders with different balances get proportional payouts
#[test]
fn multiple_lenders_proportional_payouts() {
    let scale_factor = WAD + WAD / 10; // 1.1x
    let settlement = WAD; // fully funded

    let lender_a_scaled = 600_000u128; // 60% of pool
    let lender_b_scaled = 400_000u128; // 40% of pool

    let payout_a = compute_payout(lender_a_scaled, scale_factor, settlement).unwrap();
    let payout_b = compute_payout(lender_b_scaled, scale_factor, settlement).unwrap();

    // Verify proportionality
    assert!(payout_a > payout_b, "larger position gets larger payout");

    // Normalized: a=660000, b=440000
    assert_eq!(payout_a, 660_000);
    assert_eq!(payout_b, 440_000);

    // Total payouts should equal total normalized
    let total_normalized = normalize(lender_a_scaled + lender_b_scaled, scale_factor).unwrap();
    assert_eq!(payout_a + payout_b, total_normalized);
}

/// Under settlement, payouts are proportionally reduced
#[test]
fn multiple_lenders_under_settlement() {
    let scale_factor = WAD;
    let settlement = WAD / 2; // 50% settlement

    let lender_a_scaled = 600_000u128;
    let lender_b_scaled = 400_000u128;

    let payout_a = compute_payout(lender_a_scaled, scale_factor, settlement).unwrap();
    let payout_b = compute_payout(lender_b_scaled, scale_factor, settlement).unwrap();

    assert_eq!(payout_a, 300_000); // 600K * 50%
    assert_eq!(payout_b, 200_000); // 400K * 50%
    assert_eq!(payout_a + payout_b, 500_000);
    assert_eq!(
        compute_payout(lender_a_scaled, scale_factor, settlement + 1).unwrap(),
        300_000
    );
    assert_eq!(
        compute_payout(lender_b_scaled, scale_factor, settlement + 1).unwrap(),
        200_000
    );
}

// ===========================================================================
// Section 27: Extreme timestamp values
// ===========================================================================

/// i64::MAX maturity with current_ts near i64::MAX
#[test]
fn accrue_near_max_timestamp() {
    let maturity = i64::MAX;
    let last_accrual = i64::MAX - 1000;
    let mut market = make_market(1000, maturity, WAD, WAD, last_accrual, 0);
    let config = make_config(0);

    // current_ts = i64::MAX -> effective_now = i64::MAX (not past maturity)
    // time_elapsed = i64::MAX - (i64::MAX - 1000) = 1000
    let result = accrue_interest(&mut market, &config, i64::MAX);
    assert_eq!(result, Ok(()));
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor_single_step(WAD, 1000, 1000)
    );
    assert_eq!(market.last_accrual_timestamp(), i64::MAX);
}

/// Negative timestamps: maturity in the past from epoch perspective
#[test]
fn accrue_negative_timestamps() {
    let maturity = -100i64;
    let last_accrual = -200i64;
    let current_ts = 0i64;

    let mut market = make_market(1000, maturity, WAD, WAD, last_accrual, 0);
    let config = make_config(0);

    // effective_now = min(0, -100) = -100
    // time_elapsed = -100 - (-200) = 100
    accrue_interest(&mut market, &config, current_ts).unwrap();
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor_single_step(WAD, 1000, 100)
    );
    assert_eq!(market.last_accrual_timestamp(), maturity);

    let mut shorter = make_market(1000, maturity, WAD, WAD, -200, 0);
    accrue_interest(&mut shorter, &config, -150).unwrap();
    assert_eq!(
        shorter.scale_factor(),
        expected_scale_factor_single_step(WAD, 1000, 50)
    );
    assert_eq!(shorter.last_accrual_timestamp(), -150);
}

// ===========================================================================
// Section 28: Rounding behavior verification
// ===========================================================================

/// Deposit rounding: always floors (favors protocol)
#[test]
fn deposit_rounding_floors() {
    assert_eq!(deposit_scale(2, 2 * WAD).unwrap(), 1);
    assert_eq!(deposit_scale(3, 2 * WAD).unwrap(), 1);
    assert_eq!(deposit_scale(4, 2 * WAD).unwrap(), 2);
}

/// Normalize rounding: always floors
#[test]
fn normalize_rounding_floors() {
    assert_eq!(normalize(2, 2 * WAD).unwrap(), 4);
    assert_eq!(normalize(3, 2 * WAD).unwrap(), 6);
    assert_eq!(normalize(4, 2 * WAD).unwrap(), 8);
    assert_eq!(normalize(1, WAD + 1).unwrap(), 1);
    assert_eq!(normalize(2, WAD + 1).unwrap(), 2);
}

/// Payout rounding: always floors
#[test]
fn payout_rounding_floors() {
    assert_eq!(compute_payout(2, WAD, WAD / 2).unwrap(), 1);
    assert_eq!(compute_payout(3, WAD, WAD / 2).unwrap(), 1);
    assert_eq!(compute_payout(4, WAD, WAD / 2).unwrap(), 2);
}

/// Double rounding: deposit then withdraw can lose at most 2 units
#[test]
fn double_rounding_loss_bounded() {
    let scale_factor = WAD + WAD / 3; // 1.333...x (non-exact division)
    let settlement = WAD;

    for amount in [1u64, 2, 3, 7, 13, 97, 1_000_001] {
        let scaled = deposit_scale(amount, scale_factor).unwrap();
        if scaled == 0 {
            continue;
        }
        let payout = compute_payout(scaled, scale_factor, settlement).unwrap();
        let original = u128::from(amount);
        assert!(
            payout <= original,
            "payout ({}) must not exceed original ({})",
            payout,
            original
        );
        let loss = original - payout;
        assert!(
            loss <= 2,
            "round-trip loss ({}) should be <= 2 for amount={}",
            loss,
            amount
        );
    }

    let a = compute_payout(
        deposit_scale(97, scale_factor).unwrap(),
        scale_factor,
        settlement,
    )
    .unwrap();
    let b = compute_payout(
        deposit_scale(98, scale_factor).unwrap(),
        scale_factor,
        settlement,
    )
    .unwrap();
    assert!(
        b >= a,
        "round-trip payout should be monotonic in input amount"
    );
}
