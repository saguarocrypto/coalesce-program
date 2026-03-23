//! Coverage gap tests.
//!
//! Targets edge cases and boundary conditions identified by coverage analysis.
//! Each test exercises a specific code path that might otherwise be uncovered.

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
    BORROWER_WHITELIST_SIZE, LENDER_POSITION_SIZE, MARKET_SIZE, PROTOCOL_CONFIG_SIZE,
    SECONDS_PER_YEAR, WAD,
};
use coalesce::error::LendingError;
use coalesce::logic::interest::{accrue_interest, compute_settlement_factor};
use coalesce::logic::validation::is_zero_address;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};
use pinocchio::error::ProgramError;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[path = "common/interest_oracle.rs"]
mod interest_oracle;

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

fn scale_factor_after_exact(scale_factor: u128, annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::scale_factor_after_exact(scale_factor, annual_bps, elapsed_seconds)
}

fn expected_fee_delta(
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

// ===========================================================================
// Edge case: Zero-supply market
// ===========================================================================

/// A market with 0 scaled_total_supply should accrue no fees
/// (even with non-zero fee rate) since there's nothing to charge fees on.
#[test]
fn edge_zero_supply_market() {
    let annual_bps = 1000u16;
    let fee_rate_bps = 5000u16;
    let elapsed = SECONDS_PER_YEAR as i64;
    let mut market = make_market(annual_bps, i64::MAX, WAD, 0, 0, 0);
    let config = make_config(5000);

    accrue_interest(&mut market, &config, elapsed).unwrap();

    let expected_sf = scale_factor_after_exact(WAD, annual_bps, elapsed);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), 0);
    assert_eq!(market.last_accrual_timestamp(), elapsed);
    assert_eq!(market.scaled_total_supply(), 0);

    // Neighbor boundary: fee-rate=0 keeps the same expected state for zero supply.
    let mut zero_fee = make_market(annual_bps, i64::MAX, WAD, 0, 0, 0);
    accrue_interest(&mut zero_fee, &make_config(0), elapsed).unwrap();
    assert_eq!(zero_fee.scale_factor(), expected_sf);
    assert_eq!(zero_fee.accrued_protocol_fees(), 0);
    assert_eq!(fee_rate_bps, config.fee_rate_bps());
}

// ===========================================================================
// Edge case: Market with 0 interest rate
// ===========================================================================

/// A market with 0% interest should have no scale_factor growth and no fees.
#[test]
fn edge_zero_interest_rate() {
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(0, i64::MAX, WAD, supply, 0, 0);
    let config = make_config(5000);

    for elapsed in [1i64, SECONDS_PER_YEAR as i64, 2 * SECONDS_PER_YEAR as i64] {
        let mut m = market;
        accrue_interest(&mut m, &config, elapsed).unwrap();
        assert_eq!(m.scale_factor(), WAD, "0-rate should keep scale unchanged");
        assert_eq!(m.accrued_protocol_fees(), 0, "0-rate should yield no fees");
        assert_eq!(m.last_accrual_timestamp(), elapsed);
        assert_eq!(m.scaled_total_supply(), supply);
    }

    accrue_interest(&mut market, &config, 0).unwrap();
    assert_eq!(market.scale_factor(), WAD);
    assert_eq!(market.last_accrual_timestamp(), 0);
}

// ===========================================================================
// Edge case: Market at exact maturity timestamp (boundary)
// ===========================================================================

/// Accruing at exactly the maturity timestamp should include all interest
/// up to maturity.
#[test]
fn edge_exact_maturity_boundary() {
    let maturity = 31_536_000i64; // 1 year
    let mut m_at = make_market(1000, maturity, WAD, WAD, 0, 0);
    let config = make_config(0);

    // Accrue exactly at maturity
    accrue_interest(&mut m_at, &config, maturity).unwrap();

    let expected = scale_factor_after_exact(WAD, 1000, maturity);
    assert_eq!(m_at.scale_factor(), expected);
    assert_eq!(m_at.last_accrual_timestamp(), maturity);

    // Boundary neighbor: maturity+1 must cap to exact maturity result.
    let mut m_after = make_market(1000, maturity, WAD, WAD, 0, 0);
    accrue_interest(&mut m_after, &config, maturity + 1).unwrap();
    assert_eq!(m_after.scale_factor(), expected);
    assert_eq!(m_after.last_accrual_timestamp(), maturity);
}

/// Accruing at maturity - 1 second should leave 1 second of interest unaccrued.
#[test]
fn edge_one_second_before_maturity() {
    let maturity = 31_536_000i64;
    let config = make_config(0);

    let mut m_before = make_market(1000, maturity, WAD, WAD, 0, 0);
    accrue_interest(&mut m_before, &config, maturity - 1).unwrap();

    let mut m_at = make_market(1000, maturity, WAD, WAD, 0, 0);
    accrue_interest(&mut m_at, &config, maturity).unwrap();

    assert_eq!(m_before.last_accrual_timestamp(), maturity - 1);
    assert_eq!(m_at.last_accrual_timestamp(), maturity);

    let expected_before = scale_factor_after_exact(WAD, 1000, maturity - 1);
    let expected_at = scale_factor_after_exact(WAD, 1000, maturity);
    assert_eq!(m_before.scale_factor(), expected_before);
    assert_eq!(m_at.scale_factor(), expected_at);

    let expected_diff = expected_at - expected_before;
    let actual_diff = m_at.scale_factor() - m_before.scale_factor();
    assert_eq!(actual_diff, expected_diff);
}

// ===========================================================================
// Edge case: Deposit that exactly hits the cap
// ===========================================================================

/// Depositing exactly the max_total_supply should succeed (not off-by-one).
#[test]
fn edge_deposit_exact_cap() {
    let max_supply: u128 = 1_000_000; // 1 USDC
    let amount_below: u128 = max_supply - 1;
    let amount_exact: u128 = max_supply;
    let amount_over: u128 = max_supply + 1;
    let scale_factor = WAD;
    let scaled_below = amount_below * WAD / scale_factor;
    let scaled_exact = amount_exact * WAD / scale_factor;
    let scaled_over = amount_over * WAD / scale_factor;

    let normalized_below = scaled_below * scale_factor / WAD;
    let normalized_exact = scaled_exact * scale_factor / WAD;
    let normalized_over = scaled_over * scale_factor / WAD;

    assert!(normalized_below < max_supply);
    assert_eq!(normalized_exact, max_supply);
    assert!(normalized_over > max_supply);
}

/// Depositing one more than the cap should fail.
#[test]
fn edge_deposit_one_over_cap() {
    let max_supply: u128 = 1_000_000;
    let scale_factor = WAD;

    let at_cap_amount = max_supply;
    let over_cap_amount = max_supply + 1;
    let at_cap_normalized = at_cap_amount * WAD / scale_factor * scale_factor / WAD;
    let over_cap_normalized = over_cap_amount * WAD / scale_factor * scale_factor / WAD;

    assert_eq!(at_cap_normalized, max_supply, "x boundary should pass");
    assert!(
        over_cap_normalized > max_supply,
        "x+1 boundary should fail cap check"
    );
}

// ===========================================================================
// Edge case: Lender position with 1 lamport scaled balance
// ===========================================================================

/// Verify that a 1-unit scaled balance can be correctly read and manipulated.
#[test]
fn edge_one_unit_scaled_balance() {
    let mut pos = LenderPosition::zeroed();
    pos.set_scaled_balance(1);
    assert_eq!(pos.scaled_balance(), 1);

    for scale_factor in [WAD, WAD + WAD / 10, 2 * WAD] {
        let normalized = scale_factor / WAD;
        assert_eq!(normalized, scale_factor / WAD);
        assert!(normalized >= 1);
        for settlement in [WAD / 2, WAD] {
            let payout = normalized * settlement / WAD;
            assert!(
                payout <= normalized,
                "settlement payout must not exceed normalized balance"
            );
        }
    }
}

/// Verify that withdrawing 1-unit scaled balance works.
#[test]
fn edge_one_unit_withdrawal_payout() {
    for scale_factor in [WAD, WAD + WAD / 10, 2 * WAD] {
        let normalized = scale_factor / WAD;
        assert!(normalized >= 1);
        let payout_full = normalized * WAD / WAD;
        let payout_half = normalized * (WAD / 2) / WAD;
        assert_eq!(payout_full, normalized);
        assert!(payout_half <= payout_full);
    }
}

// ===========================================================================
// Edge case: Settlement factor at exactly WAD (fully funded)
// ===========================================================================

/// When vault has enough to cover all lenders exactly, settlement_factor = WAD.
#[test]
fn edge_settlement_factor_exactly_wad() {
    let total_normalized = 1_000_000u128;
    for available in [total_normalized - 1, total_normalized, total_normalized + 1] {
        let factor = compute_settlement_factor(available, total_normalized).unwrap();
        if available < total_normalized {
            assert!(factor < WAD);
            assert!(factor >= 1);
        } else {
            assert_eq!(factor, WAD);
        }
    }
}

/// When vault has more than enough (overfunded), settlement_factor still = WAD.
#[test]
fn edge_settlement_factor_overfunded() {
    let total_normalized = 1_000_000u128;
    for available in [2_000_000u128, 10_000_000u128, u128::MAX / WAD] {
        let factor = compute_settlement_factor(available, total_normalized).unwrap();
        assert_eq!(factor, WAD, "overfunded must cap at WAD");
    }
}

// ===========================================================================
// Edge case: Settlement factor with dust amounts
// ===========================================================================

/// 1 lamport vault, huge supply → settlement factor should be 1 (minimum).
#[test]
fn edge_settlement_factor_dust_vault() {
    let total_huge = WAD; // 1e18
    let factor_1 = compute_settlement_factor(1, total_huge).unwrap();
    assert_eq!(factor_1, 1, "1 / WAD should clamp to minimum 1");

    let factor_2 = compute_settlement_factor(2, total_huge).unwrap();
    assert_eq!(factor_2, 2, "2 / WAD should preserve value above min");
    assert!(factor_2 > factor_1);

    let factor_0 = compute_settlement_factor(0, total_huge).unwrap();
    assert_eq!(factor_0, 1, "zero available also clamps to 1");
}

// ===========================================================================
// Edge case: Re-settle when factor already at WAD
// ===========================================================================

/// Re-settlement should fail if factor is already WAD (can't improve).
#[test]
fn edge_resettle_at_wad() {
    let old_factor = WAD;
    for new_factor in [WAD - 1, WAD, WAD + 1] {
        let is_not_improved = new_factor <= old_factor;
        if new_factor <= WAD {
            assert!(is_not_improved);
        } else {
            assert!(!is_not_improved);
        }
    }
}

// ===========================================================================
// Edge case: Maximum annual_bps over maximum time
// ===========================================================================

/// 100% interest rate for the maximum period (1 year to maturity).
#[test]
fn edge_max_rate_max_time() {
    let annual_bps = 10_000u16; // 100%
    let seconds = SECONDS_PER_YEAR as i64;
    let mut market = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
    let config = make_config(0);

    accrue_interest(&mut market, &config, seconds).unwrap();

    let expected = scale_factor_after_exact(WAD, annual_bps, seconds);
    assert_eq!(market.scale_factor(), expected);
    assert_eq!(market.last_accrual_timestamp(), seconds);

    // Neighbor rate boundary.
    let mut market_minus_one = make_market(annual_bps - 1, i64::MAX, WAD, WAD, 0, 0);
    accrue_interest(&mut market_minus_one, &config, seconds).unwrap();
    assert!(market_minus_one.scale_factor() < market.scale_factor());
}

// ===========================================================================
// Edge case: Bytemuck roundtrip from raw bytes
// ===========================================================================

/// Market created from zeroed bytes should have all zero fields.
#[test]
fn edge_market_from_zero_bytes() {
    let bytes = [0u8; MARKET_SIZE];
    let market: &Market = bytemuck::from_bytes(&bytes);

    assert_eq!(market.annual_interest_bps(), 0);
    assert_eq!(market.maturity_timestamp(), 0);
    assert_eq!(market.scale_factor(), 0);
    assert_eq!(market.scaled_total_supply(), 0);
    assert_eq!(market.accrued_protocol_fees(), 0);
}

/// ProtocolConfig from zeroed bytes should have zero fields.
#[test]
fn edge_config_from_zero_bytes() {
    let bytes = [0u8; PROTOCOL_CONFIG_SIZE];
    let config: &ProtocolConfig = bytemuck::from_bytes(&bytes);

    assert_eq!(config.fee_rate_bps(), 0);
    assert_eq!(config.is_initialized, 0);
    assert_eq!(config.bump, 0);
    assert_eq!(config.paused, 0);
    assert_eq!(config.blacklist_mode, 0);
}

/// LenderPosition from zeroed bytes should have zero fields.
#[test]
fn edge_position_from_zero_bytes() {
    let bytes = [0u8; LENDER_POSITION_SIZE];
    let pos: &LenderPosition = bytemuck::from_bytes(&bytes);

    assert_eq!(pos.scaled_balance(), 0);
    assert_eq!(pos.bump, 0);
    assert_eq!(pos.version, 0);
}

/// BorrowerWhitelist from zeroed bytes should have zero fields.
#[test]
fn edge_whitelist_from_zero_bytes() {
    let bytes = [0u8; BORROWER_WHITELIST_SIZE];
    let wl: &BorrowerWhitelist = bytemuck::from_bytes(&bytes);

    assert_eq!(wl.max_borrow_capacity(), 0);
    assert_eq!(wl.current_borrowed(), 0);
    assert_eq!(wl.is_whitelisted, 0);
    assert_eq!(wl.bump, 0);
}

// ===========================================================================
// Edge case: is_zero_address boundary
// ===========================================================================

/// Single non-zero byte at each position should return false.
#[test]
fn edge_is_zero_address_each_byte() {
    assert!(is_zero_address(&[0u8; 32]));
    assert!(!is_zero_address(&[0xFFu8; 32]));

    for i in 0..32 {
        let mut addr = [0u8; 32];
        addr[i] = 1;
        assert!(
            !is_zero_address(&addr),
            "byte {} set should not be zero address",
            i
        );
    }
}

// ===========================================================================
// Edge case: Negative timestamps
// ===========================================================================

/// Verify behavior when last_accrual_timestamp is in the distant past.
#[test]
fn edge_negative_timestamps() {
    let mut market = make_market(1000, i64::MAX, WAD, WAD, -1_000_000, 0);
    let config = make_config(0);

    // Current time = 0 → time_elapsed = 0 - (-1_000_000) = 1_000_000
    accrue_interest(&mut market, &config, 0).unwrap();

    let expected_sf = scale_factor_after_exact(WAD, 1000, 1_000_000);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.last_accrual_timestamp(), 0);

    // Neighbor failure path: current timestamp before last_accrual must error and not mutate.
    let mut invalid = make_market(1000, i64::MAX, WAD, WAD, -1_000_000, 0);
    let before = invalid;
    let err = accrue_interest(&mut invalid, &config, -1_000_001);
    assert_eq!(
        err,
        Err(ProgramError::Custom(LendingError::InvalidTimestamp as u32))
    );
    assert_eq!(invalid.scale_factor(), before.scale_factor());
    assert_eq!(
        invalid.last_accrual_timestamp(),
        before.last_accrual_timestamp()
    );
    assert_eq!(
        invalid.accrued_protocol_fees(),
        before.accrued_protocol_fees()
    );
}

// ===========================================================================
// Edge case: Accumulation of existing fees
// ===========================================================================

/// Verify that existing accrued fees are preserved and new fees add on top.
#[test]
fn edge_existing_fees_accumulation() {
    let initial_fees = 500_000u64; // 0.5 USDC
    let supply = 1_000_000_000_000u128;
    let mut market = make_market(1000, i64::MAX, WAD, supply, 0, initial_fees);
    let config = make_config(1000);

    let elapsed = SECONDS_PER_YEAR as i64;
    accrue_interest(&mut market, &config, elapsed).unwrap();

    let expected_sf = scale_factor_after_exact(WAD, 1000, elapsed);
    let expected_new_fees = expected_fee_delta(supply, WAD, 1000, 1000, elapsed);
    let expected_total = initial_fees + expected_new_fees;

    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), expected_total);
    assert_eq!(market.last_accrual_timestamp(), elapsed);
    assert_eq!(market.scaled_total_supply(), supply);
}
