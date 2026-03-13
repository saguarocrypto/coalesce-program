//! Upgrade safety tests for the CoalesceFi Pinocchio lending protocol.
//!
//! These tests verify that program upgrades do not break:
//! - Account layout backwards compatibility (bytemuck casts from raw bytes)
//! - State migration safety (padding, zeroing, roundtrips)
//! - Deterministic replay (identical inputs always produce identical outputs)
//! - Field size boundary safety (overflow and max-value handling)
//! - Version sentinel / bump byte locations
//! - Core invariant preservation across simulated upgrade scenarios

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
use core::mem::{offset_of, size_of};

use coalesce::constants::{
    BORROWER_WHITELIST_SIZE, BPS, LENDER_POSITION_SIZE, MARKET_SIZE, PROTOCOL_CONFIG_SIZE,
    SECONDS_PER_YEAR, WAD,
};
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};
use pinocchio::error::ProgramError;

#[path = "common/math_oracle.rs"]
mod math_oracle;

// ===========================================================================
// Helpers
// ===========================================================================

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

/// Build a fully-populated Market with non-trivial values in every field.
fn make_nontrivial_market() -> Market {
    let mut m = Market::zeroed();
    m.borrower = [0xAA; 32];
    m.mint = [0xBB; 32];
    m.vault = [0xCC; 32];
    m.market_authority_bump = 253;
    m.set_annual_interest_bps(1500);
    m.set_maturity_timestamp(1_700_000_000);
    m.set_max_total_supply(5_000_000_000);
    m.set_market_nonce(12345);
    m.set_scaled_total_supply(2_000_000_000_000_000_000);
    m.set_scale_factor(WAD + WAD / 10); // 1.1 * WAD
    m.set_accrued_protocol_fees(42_000_000);
    m.set_total_deposited(3_000_000_000);
    m.set_total_borrowed(1_500_000_000);
    m.set_total_repaid(500_000_000);
    m.set_total_interest_repaid(100_000_000);
    m.set_last_accrual_timestamp(1_699_000_000);
    m.set_settlement_factor_wad(WAD * 3 / 4);
    m.bump = 254;
    m
}

/// Build a fully-populated ProtocolConfig with non-trivial values.
fn make_nontrivial_config() -> ProtocolConfig {
    let mut c = ProtocolConfig::zeroed();
    c.admin = [0x11; 32];
    c.set_fee_rate_bps(500);
    c.fee_authority = [0x22; 32];
    c.whitelist_manager = [0x33; 32];
    c.blacklist_program = [0x44; 32];
    c.is_initialized = 1;
    c.bump = 200;
    c
}

/// Build a fully-populated LenderPosition with non-trivial values.
fn make_nontrivial_lender_position() -> LenderPosition {
    let mut p = LenderPosition::zeroed();
    p.market = [0x55; 32];
    p.lender = [0x66; 32];
    p.set_scaled_balance(999_888_777_666_555_444);
    p.bump = 100;
    p
}

/// Build a fully-populated BorrowerWhitelist with non-trivial values.
fn make_nontrivial_borrower_whitelist() -> BorrowerWhitelist {
    let mut w = BorrowerWhitelist::zeroed();
    w.borrower = [0x77; 32];
    w.is_whitelisted = 1;
    w.set_max_borrow_capacity(10_000_000_000);
    w.set_current_borrowed(3_000_000_000);
    w.bump = 55;
    w
}

/// Replicates the settlement factor formula from withdraw/re_settle processors.
fn compute_settlement_factor(available: u128, total_normalized: u128) -> u128 {
    if total_normalized == 0 {
        return WAD;
    }
    let raw = available
        .checked_mul(WAD)
        .expect("overflow in settlement_factor raw")
        .checked_div(total_normalized)
        .expect("division by zero in settlement_factor");
    let capped = if raw > WAD { WAD } else { raw };
    if capped < 1 {
        1
    } else {
        capped
    }
}

fn expected_settlement_factor(available: u128, total_normalized: u128) -> u128 {
    if total_normalized == 0 {
        return WAD;
    }
    let raw = available
        .checked_mul(WAD)
        .expect("settlement numerator overflow")
        .checked_div(total_normalized)
        .expect("settlement division by zero");
    let capped = if raw > WAD { WAD } else { raw };
    if capped < 1 {
        1
    } else {
        capped
    }
}

fn effective_elapsed(last_accrual: i64, maturity: i64, current_timestamp: i64) -> i64 {
    let effective_now = if current_timestamp > maturity {
        maturity
    } else {
        current_timestamp
    };
    if effective_now <= last_accrual {
        0
    } else {
        effective_now - last_accrual
    }
}

fn expected_mul_wad(a: u128, b: u128) -> u128 {
    math_oracle::mul_wad(a, b)
}

fn expected_pow_wad(base: u128, exp: u32) -> u128 {
    math_oracle::pow_wad(base, exp)
}

fn expected_growth_factor_wad(annual_interest_bps: u16, elapsed_seconds: i64) -> u128 {
    if elapsed_seconds <= 0 {
        return WAD;
    }
    math_oracle::growth_factor_wad(annual_interest_bps, elapsed_seconds)
}

fn expected_interest_delta_wad(annual_interest_bps: u16, elapsed_seconds: i64) -> u128 {
    expected_growth_factor_wad(annual_interest_bps, elapsed_seconds)
        .checked_sub(WAD)
        .expect("growth factor must be >= WAD")
}

fn expected_scale_factor_after_elapsed(
    starting_scale_factor: u128,
    annual_interest_bps: u16,
    elapsed_seconds: i64,
) -> u128 {
    let growth_wad = expected_growth_factor_wad(annual_interest_bps, elapsed_seconds);
    expected_mul_wad(starting_scale_factor, growth_wad)
}

fn expected_fee_delta(
    scaled_total_supply: u128,
    pre_accrual_scale_factor: u128,
    annual_interest_bps: u16,
    elapsed_seconds: i64,
    fee_rate_bps: u16,
) -> u64 {
    if fee_rate_bps == 0 || elapsed_seconds <= 0 {
        return 0;
    }
    let interest_delta_wad = expected_interest_delta_wad(annual_interest_bps, elapsed_seconds);
    let fee_delta_wad = interest_delta_wad
        .checked_mul(u128::from(fee_rate_bps))
        .expect("fee delta overflow")
        .checked_div(BPS)
        .expect("fee delta division by zero");
    let fee_normalized = scaled_total_supply
        .checked_mul(pre_accrual_scale_factor)
        .expect("supply * scale overflow")
        .checked_div(WAD)
        .expect("fee path division by zero")
        .checked_mul(fee_delta_wad)
        .expect("fee multiply overflow")
        .checked_div(WAD)
        .expect("fee normalization division by zero");
    u64::try_from(fee_normalized).expect("fee must fit in u64 for this scenario")
}

fn market_bytes(market: &Market) -> Vec<u8> {
    bytemuck::bytes_of(market).to_vec()
}

// ===========================================================================
// 1. ACCOUNT LAYOUT BACKWARDS COMPATIBILITY (4 tests)
// ===========================================================================

/// Construct Market from raw bytes (simulating pre-upgrade account data),
/// verify all accessors return correct values.
#[test]
fn upgrade_layout_market_from_raw_bytes() {
    let mut raw = [0u8; 250];

    // Write every field at its documented offset (discriminator[8]+version[1] prefix)
    raw[9..41].copy_from_slice(&[0xAA; 32]); // borrower
    raw[41..73].copy_from_slice(&[0xBB; 32]); // mint
    raw[73..105].copy_from_slice(&[0xCC; 32]); // vault
    raw[105] = 253; // market_authority_bump
    raw[106..108].copy_from_slice(&1500u16.to_le_bytes()); // annual_interest_bps
    raw[108..116].copy_from_slice(&1_700_000_000i64.to_le_bytes()); // maturity_timestamp
    raw[116..124].copy_from_slice(&5_000_000_000u64.to_le_bytes()); // max_total_supply
    raw[124..132].copy_from_slice(&12345u64.to_le_bytes()); // market_nonce
    let scaled_supply: u128 = 2_000_000_000_000_000_000;
    raw[132..148].copy_from_slice(&scaled_supply.to_le_bytes()); // scaled_total_supply
    let sf: u128 = WAD + WAD / 10;
    raw[148..164].copy_from_slice(&sf.to_le_bytes()); // scale_factor
    raw[164..172].copy_from_slice(&42_000_000u64.to_le_bytes()); // accrued_protocol_fees
    raw[172..180].copy_from_slice(&3_000_000_000u64.to_le_bytes()); // total_deposited
    raw[180..188].copy_from_slice(&1_500_000_000u64.to_le_bytes()); // total_borrowed
    raw[188..196].copy_from_slice(&500_000_000u64.to_le_bytes()); // total_repaid
    raw[196..204].copy_from_slice(&100_000_000u64.to_le_bytes()); // total_interest_repaid
    raw[204..212].copy_from_slice(&1_699_000_000i64.to_le_bytes()); // last_accrual_timestamp
    let settlement: u128 = WAD * 3 / 4;
    raw[212..228].copy_from_slice(&settlement.to_le_bytes()); // settlement_factor_wad
    raw[228] = 254; // bump
                    // padding [229..250] left as zeros

    let market: &Market = bytemuck::from_bytes(&raw);

    assert_eq!(market.borrower, [0xAA; 32]);
    assert_eq!(market.mint, [0xBB; 32]);
    assert_eq!(market.vault, [0xCC; 32]);
    assert_eq!(market.market_authority_bump, 253);
    assert_eq!(market.annual_interest_bps(), 1500);
    assert_eq!(market.maturity_timestamp(), 1_700_000_000);
    assert_eq!(market.max_total_supply(), 5_000_000_000);
    assert_eq!(market.market_nonce(), 12345);
    assert_eq!(market.scaled_total_supply(), scaled_supply);
    assert_eq!(market.scale_factor(), sf);
    assert_eq!(market.accrued_protocol_fees(), 42_000_000);
    assert_eq!(market.total_deposited(), 3_000_000_000);
    assert_eq!(market.total_borrowed(), 1_500_000_000);
    assert_eq!(market.total_repaid(), 500_000_000);
    assert_eq!(market.total_interest_repaid(), 100_000_000);
    assert_eq!(market.last_accrual_timestamp(), 1_699_000_000);
    assert_eq!(market.settlement_factor_wad(), settlement);
    assert_eq!(market.bump, 254);
}

/// Construct ProtocolConfig from raw bytes, verify all accessors.
#[test]
fn upgrade_layout_protocol_config_from_raw_bytes() {
    let mut raw = [0u8; 194];

    raw[9..41].copy_from_slice(&[0x11; 32]); // admin
    raw[41..43].copy_from_slice(&500u16.to_le_bytes()); // fee_rate_bps
    raw[43..75].copy_from_slice(&[0x22; 32]); // fee_authority
    raw[75..107].copy_from_slice(&[0x33; 32]); // whitelist_manager
    raw[107..139].copy_from_slice(&[0x44; 32]); // blacklist_program
    raw[139] = 1; // is_initialized
    raw[140] = 200; // bump

    let config: &ProtocolConfig = bytemuck::from_bytes(&raw);

    assert_eq!(config.admin, [0x11; 32]);
    assert_eq!(config.fee_rate_bps(), 500);
    assert_eq!(config.fee_authority, [0x22; 32]);
    assert_eq!(config.whitelist_manager, [0x33; 32]);
    assert_eq!(config.blacklist_program, [0x44; 32]);
    assert_eq!(config.is_initialized, 1);
    assert_eq!(config.bump, 200);
}

/// Construct LenderPosition from raw bytes, verify all accessors.
#[test]
fn upgrade_layout_lender_position_from_raw_bytes() {
    let mut raw = [0u8; 128];

    raw[9..41].copy_from_slice(&[0x55; 32]); // market
    raw[41..73].copy_from_slice(&[0x66; 32]); // lender
    let balance: u128 = 999_888_777_666_555_444;
    raw[73..89].copy_from_slice(&balance.to_le_bytes()); // scaled_balance
    raw[89] = 100; // bump

    let pos: &LenderPosition = bytemuck::from_bytes(&raw);

    assert_eq!(pos.market, [0x55; 32]);
    assert_eq!(pos.lender, [0x66; 32]);
    assert_eq!(pos.scaled_balance(), balance);
    assert_eq!(pos.bump, 100);
}

/// Construct BorrowerWhitelist from raw bytes, verify all accessors.
/// Also verify raw-bytes construction produces same result as field-by-field.
#[test]
fn upgrade_layout_borrower_whitelist_from_raw_bytes() {
    let mut raw = [0u8; 96];

    raw[9..41].copy_from_slice(&[0x77; 32]); // borrower
    raw[41] = 1; // is_whitelisted
    raw[42..50].copy_from_slice(&10_000_000_000u64.to_le_bytes()); // max_borrow_capacity
    raw[50..58].copy_from_slice(&3_000_000_000u64.to_le_bytes()); // current_borrowed
    raw[58] = 55; // bump

    let wl: &BorrowerWhitelist = bytemuck::from_bytes(&raw);

    assert_eq!(wl.borrower, [0x77; 32]);
    assert_eq!(wl.is_whitelisted, 1);
    assert_eq!(wl.max_borrow_capacity(), 10_000_000_000);
    assert_eq!(wl.current_borrowed(), 3_000_000_000);
    assert_eq!(wl.bump, 55);

    // Verify that raw-bytes construction matches field-by-field construction
    let field_built = make_nontrivial_borrower_whitelist();
    let field_bytes: &[u8] = bytemuck::bytes_of(&field_built);
    assert_eq!(
        &raw[..],
        field_bytes,
        "raw bytes must match field-by-field construction"
    );
}

// ===========================================================================
// 2. STATE MIGRATION SAFETY (4 tests)
// ===========================================================================

/// Verify that zeroed padding bytes do not affect any computation.
#[test]
fn upgrade_migration_zeroed_padding_no_effect() {
    // Create two identical markets, one with padding zeroed, one with padding filled
    let mut m1 = make_nontrivial_market();
    let mut m2 = make_nontrivial_market();

    // Fill padding of m2 with non-zero values
    m2.padding = [0xFF; 21];

    // All accessors should return identical values
    assert_eq!(m1.borrower, m2.borrower);
    assert_eq!(m1.mint, m2.mint);
    assert_eq!(m1.vault, m2.vault);
    assert_eq!(m1.market_authority_bump, m2.market_authority_bump);
    assert_eq!(m1.annual_interest_bps(), m2.annual_interest_bps());
    assert_eq!(m1.maturity_timestamp(), m2.maturity_timestamp());
    assert_eq!(m1.max_total_supply(), m2.max_total_supply());
    assert_eq!(m1.market_nonce(), m2.market_nonce());
    assert_eq!(m1.scaled_total_supply(), m2.scaled_total_supply());
    assert_eq!(m1.scale_factor(), m2.scale_factor());
    assert_eq!(m1.accrued_protocol_fees(), m2.accrued_protocol_fees());
    assert_eq!(m1.total_deposited(), m2.total_deposited());
    assert_eq!(m1.total_borrowed(), m2.total_borrowed());
    assert_eq!(m1.total_repaid(), m2.total_repaid());
    assert_eq!(m1.total_interest_repaid(), m2.total_interest_repaid());
    assert_eq!(m1.last_accrual_timestamp(), m2.last_accrual_timestamp());
    assert_eq!(m1.settlement_factor_wad(), m2.settlement_factor_wad());
    assert_eq!(m1.bump, m2.bump);

    // Run accrue_interest on both and verify identical results
    let config = make_config(500);
    m1.set_last_accrual_timestamp(0);
    m1.set_maturity_timestamp(i64::MAX);
    m1.set_scale_factor(WAD);
    m1.set_accrued_protocol_fees(0);

    m2.set_last_accrual_timestamp(0);
    m2.set_maturity_timestamp(i64::MAX);
    m2.set_scale_factor(WAD);
    m2.set_accrued_protocol_fees(0);

    accrue_interest(&mut m1, &config, SECONDS_PER_YEAR as i64).unwrap();
    accrue_interest(&mut m2, &config, SECONDS_PER_YEAR as i64).unwrap();

    assert_eq!(m1.scale_factor(), m2.scale_factor());
    assert_eq!(m1.accrued_protocol_fees(), m2.accrued_protocol_fees());
    assert_eq!(m1.last_accrual_timestamp(), m2.last_accrual_timestamp());
}

/// Verify that writing a hypothetical new field into the padding area
/// does not corrupt existing fields.
#[test]
fn upgrade_migration_new_field_in_padding_no_corruption() {
    let m = make_nontrivial_market();
    let original_bytes: Vec<u8> = bytemuck::bytes_of(&m).to_vec();

    // Simulate a future upgrade that writes data into padding bytes [229..237]
    let mut modified_bytes = original_bytes.clone();
    modified_bytes[229..237].copy_from_slice(&0xDEAD_BEEF_CAFE_BABEu64.to_le_bytes());

    // Re-read as Market and verify all original fields unchanged
    let m_v2: &Market = bytemuck::from_bytes(&modified_bytes);

    assert_eq!(m_v2.borrower, m.borrower);
    assert_eq!(m_v2.mint, m.mint);
    assert_eq!(m_v2.vault, m.vault);
    assert_eq!(m_v2.market_authority_bump, m.market_authority_bump);
    assert_eq!(m_v2.annual_interest_bps(), m.annual_interest_bps());
    assert_eq!(m_v2.maturity_timestamp(), m.maturity_timestamp());
    assert_eq!(m_v2.max_total_supply(), m.max_total_supply());
    assert_eq!(m_v2.market_nonce(), m.market_nonce());
    assert_eq!(m_v2.scaled_total_supply(), m.scaled_total_supply());
    assert_eq!(m_v2.scale_factor(), m.scale_factor());
    assert_eq!(m_v2.accrued_protocol_fees(), m.accrued_protocol_fees());
    assert_eq!(m_v2.total_deposited(), m.total_deposited());
    assert_eq!(m_v2.total_borrowed(), m.total_borrowed());
    assert_eq!(m_v2.total_repaid(), m.total_repaid());
    assert_eq!(m_v2.total_interest_repaid(), m.total_interest_repaid());
    assert_eq!(m_v2.last_accrual_timestamp(), m.last_accrual_timestamp());
    assert_eq!(m_v2.settlement_factor_wad(), m.settlement_factor_wad());
    assert_eq!(m_v2.bump, m.bump);

    // Only the padding area should differ
    assert_eq!(&original_bytes[..229], &modified_bytes[..229]);
    assert_ne!(&original_bytes[229..237], &modified_bytes[229..237]);

    // Also test ProtocolConfig padding
    let c = make_nontrivial_config();
    let c_bytes: Vec<u8> = bytemuck::bytes_of(&c).to_vec();
    let mut c_modified = c_bytes.clone();
    c_modified[141..149].copy_from_slice(&0x1234567890ABCDEFu64.to_le_bytes());

    let c_v2: &ProtocolConfig = bytemuck::from_bytes(&c_modified);
    assert_eq!(c_v2.admin, c.admin);
    assert_eq!(c_v2.fee_rate_bps(), c.fee_rate_bps());
    assert_eq!(c_v2.fee_authority, c.fee_authority);
    assert_eq!(c_v2.whitelist_manager, c.whitelist_manager);
    assert_eq!(c_v2.blacklist_program, c.blacklist_program);
    assert_eq!(c_v2.is_initialized, c.is_initialized);
    assert_eq!(c_v2.bump, c.bump);
}

/// Verify that existing account data (all fields non-trivial) survives
/// a bytemuck cast roundtrip (struct -> bytes -> struct).
#[test]
fn upgrade_migration_bytemuck_cast_roundtrip() {
    // Market roundtrip
    let m_orig = make_nontrivial_market();
    let m_bytes: &[u8] = bytemuck::bytes_of(&m_orig);
    let m_back: &Market = bytemuck::from_bytes(m_bytes);
    assert_eq!(m_back.borrower, m_orig.borrower);
    assert_eq!(m_back.scale_factor(), m_orig.scale_factor());
    assert_eq!(
        m_back.settlement_factor_wad(),
        m_orig.settlement_factor_wad()
    );
    assert_eq!(m_back.total_deposited(), m_orig.total_deposited());
    assert_eq!(m_back.bump, m_orig.bump);

    // ProtocolConfig roundtrip
    let c_orig = make_nontrivial_config();
    let c_bytes: &[u8] = bytemuck::bytes_of(&c_orig);
    let c_back: &ProtocolConfig = bytemuck::from_bytes(c_bytes);
    assert_eq!(c_back.admin, c_orig.admin);
    assert_eq!(c_back.fee_rate_bps(), c_orig.fee_rate_bps());
    assert_eq!(c_back.is_initialized, c_orig.is_initialized);
    assert_eq!(c_back.bump, c_orig.bump);

    // LenderPosition roundtrip
    let p_orig = make_nontrivial_lender_position();
    let p_bytes: &[u8] = bytemuck::bytes_of(&p_orig);
    let p_back: &LenderPosition = bytemuck::from_bytes(p_bytes);
    assert_eq!(p_back.market, p_orig.market);
    assert_eq!(p_back.lender, p_orig.lender);
    assert_eq!(p_back.scaled_balance(), p_orig.scaled_balance());
    assert_eq!(p_back.bump, p_orig.bump);

    // BorrowerWhitelist roundtrip
    let w_orig = make_nontrivial_borrower_whitelist();
    let w_bytes: &[u8] = bytemuck::bytes_of(&w_orig);
    let w_back: &BorrowerWhitelist = bytemuck::from_bytes(w_bytes);
    assert_eq!(w_back.borrower, w_orig.borrower);
    assert_eq!(w_back.is_whitelisted, w_orig.is_whitelisted);
    assert_eq!(w_back.max_borrow_capacity(), w_orig.max_borrow_capacity());
    assert_eq!(w_back.current_borrowed(), w_orig.current_borrowed());
    assert_eq!(w_back.bump, w_orig.bump);

    // Full byte equality
    assert_eq!(
        bytemuck::bytes_of(m_back),
        bytemuck::bytes_of(&m_orig),
        "Market roundtrip byte mismatch"
    );
    assert_eq!(
        bytemuck::bytes_of(c_back),
        bytemuck::bytes_of(&c_orig),
        "ProtocolConfig roundtrip byte mismatch"
    );
    assert_eq!(
        bytemuck::bytes_of(p_back),
        bytemuck::bytes_of(&p_orig),
        "LenderPosition roundtrip byte mismatch"
    );
    assert_eq!(
        bytemuck::bytes_of(w_back),
        bytemuck::bytes_of(&w_orig),
        "BorrowerWhitelist roundtrip byte mismatch"
    );
}

/// Verify that `Zeroable::zeroed()` is a valid initial state for all structs.
#[test]
fn upgrade_migration_zeroed_valid_initial_state() {
    // Market: all zero is valid (pre-initialization state)
    let m = Market::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&m);
    assert!(raw.iter().all(|&b| b == 0));
    assert_eq!(m.scale_factor(), 0);
    assert_eq!(m.annual_interest_bps(), 0);
    assert_eq!(m.maturity_timestamp(), 0);
    assert_eq!(m.settlement_factor_wad(), 0);
    assert_eq!(size_of::<Market>(), MARKET_SIZE);

    // ProtocolConfig: zeroed is valid (uninitialized)
    let c = ProtocolConfig::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&c);
    assert!(raw.iter().all(|&b| b == 0));
    assert_eq!(c.fee_rate_bps(), 0);
    assert_eq!(c.is_initialized, 0);
    assert_eq!(size_of::<ProtocolConfig>(), PROTOCOL_CONFIG_SIZE);

    // LenderPosition: zeroed is valid (no position)
    let p = LenderPosition::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&p);
    assert!(raw.iter().all(|&b| b == 0));
    assert_eq!(p.scaled_balance(), 0);
    assert_eq!(size_of::<LenderPosition>(), LENDER_POSITION_SIZE);

    // BorrowerWhitelist: zeroed is valid (not whitelisted)
    let w = BorrowerWhitelist::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&w);
    assert!(raw.iter().all(|&b| b == 0));
    assert_eq!(w.max_borrow_capacity(), 0);
    assert_eq!(w.current_borrowed(), 0);
    assert_eq!(w.is_whitelisted, 0);
    assert_eq!(size_of::<BorrowerWhitelist>(), BORROWER_WHITELIST_SIZE);
}

// ===========================================================================
// 3. DETERMINISTIC REPLAY (4 tests)
// ===========================================================================

/// Capture exact output bytes from the basic lending cycle regression pattern
/// and verify they are deterministic.
#[test]
fn upgrade_deterministic_basic_lending_cycle() {
    let creation_ts: i64 = 1_000;
    let year = SECONDS_PER_YEAR as i64;
    let maturity_ts: i64 = creation_ts + year;
    let deposit_amount: u64 = 1_000_000_000; // 1000 USDC

    // Run the scenario
    let mut market = Market::zeroed();
    market.set_annual_interest_bps(1000);
    market.set_maturity_timestamp(maturity_ts);
    market.set_max_total_supply(10_000_000_000);
    market.set_scale_factor(WAD);
    market.set_last_accrual_timestamp(creation_ts);

    let config = make_config(0);

    // Deposit at creation time: scaled_amount = deposit * WAD / WAD = deposit
    let scaled_deposit = u128::from(deposit_amount);
    market.set_scaled_total_supply(scaled_deposit);
    market.set_total_deposited(deposit_amount);

    // Accrue interest to maturity
    accrue_interest(&mut market, &config, maturity_ts).unwrap();

    // Capture output bytes
    let sf_bytes = market.scale_factor().to_le_bytes();
    let ts_bytes = market.last_accrual_timestamp().to_le_bytes();

    let expected_sf = expected_scale_factor_after_elapsed(WAD, 1000, year);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(sf_bytes, expected_sf.to_le_bytes());
    assert_eq!(market.last_accrual_timestamp(), maturity_ts);
    assert_eq!(ts_bytes, maturity_ts.to_le_bytes());
}

/// Re-run identical inputs and verify byte-identical output (deterministic execution).
#[test]
fn upgrade_deterministic_identical_reruns() {
    #[derive(Clone, Copy)]
    struct Scenario {
        annual_bps: u16,
        fee_rate_bps: u16,
        maturity: i64,
        starting_scale_factor: u128,
        scaled_supply: u128,
        last_accrual: i64,
        starting_fees: u64,
        now: i64,
    }

    let year = SECONDS_PER_YEAR as i64;
    let scenarios = [
        Scenario {
            annual_bps: 0,
            fee_rate_bps: 0,
            maturity: i64::MAX,
            starting_scale_factor: WAD,
            scaled_supply: 1,
            last_accrual: 0,
            starting_fees: 0,
            now: year,
        },
        Scenario {
            annual_bps: 1000,
            fee_rate_bps: 500,
            maturity: i64::MAX,
            starting_scale_factor: WAD,
            scaled_supply: 1_000_000_000_000,
            last_accrual: 0,
            starting_fees: 7,
            now: year,
        },
        Scenario {
            annual_bps: 10000,
            fee_rate_bps: 2500,
            maturity: year / 2,
            starting_scale_factor: WAD,
            scaled_supply: 7_654_321_000_000,
            last_accrual: 0,
            starting_fees: 0,
            now: year, // capped at maturity
        },
        Scenario {
            annual_bps: 1,
            fee_rate_bps: 1,
            maturity: i64::MAX,
            starting_scale_factor: WAD + 1,
            scaled_supply: 99_999_999,
            last_accrual: 0,
            starting_fees: 0,
            now: 1,
        },
        Scenario {
            annual_bps: 2500,
            fee_rate_bps: 100,
            maturity: i64::MAX,
            starting_scale_factor: 2 * WAD,
            scaled_supply: 123_456_789_012_345,
            last_accrual: 10,
            starting_fees: 50,
            now: 10 + year / 3,
        },
    ];

    for (scenario_index, scenario) in scenarios.iter().enumerate() {
        let elapsed = effective_elapsed(scenario.last_accrual, scenario.maturity, scenario.now);
        let expected_sf = expected_scale_factor_after_elapsed(
            scenario.starting_scale_factor,
            scenario.annual_bps,
            elapsed,
        );
        let expected_fee_delta = expected_fee_delta(
            scenario.scaled_supply,
            scenario.starting_scale_factor,
            scenario.annual_bps,
            elapsed,
            scenario.fee_rate_bps,
        );
        let expected_total_fees = scenario
            .starting_fees
            .checked_add(expected_fee_delta)
            .expect("test scenario fee sum overflow");
        let expected_last = if scenario.now > scenario.maturity {
            scenario.maturity
        } else {
            scenario.now
        };

        let mut baseline: Option<Vec<u8>> = None;
        for run in 0..32 {
            let config = make_config(scenario.fee_rate_bps);
            let mut market = make_market(
                scenario.annual_bps,
                scenario.maturity,
                scenario.starting_scale_factor,
                scenario.scaled_supply,
                scenario.last_accrual,
                scenario.starting_fees,
            );
            accrue_interest(&mut market, &config, scenario.now).unwrap();

            assert_eq!(
                market.scale_factor(),
                expected_sf,
                "scale factor drifted in scenario {} run {}",
                scenario_index,
                run
            );
            assert_eq!(
                market.accrued_protocol_fees(),
                expected_total_fees,
                "fees drifted in scenario {} run {}",
                scenario_index,
                run
            );
            assert_eq!(
                market.last_accrual_timestamp(),
                expected_last,
                "last_accrual drifted in scenario {} run {}",
                scenario_index,
                run
            );

            let bytes = market_bytes(&market);
            if let Some(first) = &baseline {
                assert_eq!(
                    &bytes, first,
                    "byte replay diverged in scenario {} run {}",
                    scenario_index, run
                );
            } else {
                baseline = Some(bytes);
            }
        }
    }
}

/// Verify that the same inputs always produce the same scale_factor,
/// accrued_protocol_fees, and settlement_factor_wad.
#[test]
fn upgrade_deterministic_key_fields_stable() {
    struct KeyFieldCase {
        annual_bps: u16,
        fee_rate_bps: u16,
        supply: u128,
        elapsed: i64,
    }

    let year = SECONDS_PER_YEAR as i64;
    let cases = [
        KeyFieldCase {
            annual_bps: 1000,
            fee_rate_bps: 500,
            supply: 5_000_000_000_000,
            elapsed: year,
        },
        KeyFieldCase {
            annual_bps: 1,
            fee_rate_bps: 1,
            supply: 1_000_000,
            elapsed: 1,
        },
        KeyFieldCase {
            annual_bps: 10000,
            fee_rate_bps: 0,
            supply: 777_777_777_777,
            elapsed: year / 2,
        },
    ];

    for (case_idx, case) in cases.iter().enumerate() {
        let expected_sf = expected_scale_factor_after_elapsed(WAD, case.annual_bps, case.elapsed);
        let expected_fees = expected_fee_delta(
            case.supply,
            WAD,
            case.annual_bps,
            case.elapsed,
            case.fee_rate_bps,
        );
        let expected_total_normalized = case
            .supply
            .checked_mul(expected_sf)
            .expect("total_normalized mul overflow")
            .checked_div(WAD)
            .expect("total_normalized div by zero");

        let mut baseline_bytes: Option<Vec<u8>> = None;
        for run in 0..16 {
            let config = make_config(case.fee_rate_bps);
            let mut m = make_market(case.annual_bps, i64::MAX, WAD, case.supply, 0, 0);
            accrue_interest(&mut m, &config, case.elapsed).unwrap();

            assert_eq!(
                m.scale_factor(),
                expected_sf,
                "scale_factor mismatch in case {} run {}",
                case_idx,
                run
            );
            assert_eq!(
                m.accrued_protocol_fees(),
                expected_fees,
                "fees mismatch in case {} run {}",
                case_idx,
                run
            );

            let bytes = market_bytes(&m);
            if let Some(first) = &baseline_bytes {
                assert_eq!(
                    &bytes, first,
                    "market bytes diverged in case {} run {}",
                    case_idx, run
                );
            } else {
                baseline_bytes = Some(bytes);
            }

            let settlement_inputs = [
                (
                    0u128,
                    expected_settlement_factor(0, expected_total_normalized),
                ),
                (
                    1u128,
                    expected_settlement_factor(1, expected_total_normalized),
                ),
                (
                    expected_total_normalized.saturating_sub(1),
                    expected_settlement_factor(
                        expected_total_normalized.saturating_sub(1),
                        expected_total_normalized,
                    ),
                ),
                (expected_total_normalized, WAD),
                (expected_total_normalized.saturating_add(1), WAD),
            ];

            for (available, expected_settlement) in settlement_inputs {
                let computed = compute_settlement_factor(available, expected_total_normalized);
                let repeated = compute_settlement_factor(available, expected_total_normalized);
                assert_eq!(
                    computed, repeated,
                    "settlement non-deterministic in case {} run {} available {}",
                    case_idx, run, available
                );
                assert_eq!(
                    computed, expected_settlement,
                    "settlement mismatch in case {} run {} available {}",
                    case_idx, run, available
                );
                assert!(
                    (1..=WAD).contains(&computed),
                    "settlement out of bounds in case {} run {}",
                    case_idx,
                    run
                );
            }
        }
    }
}

/// Run all operations twice with identical inputs and verify identical state.
#[test]
fn upgrade_deterministic_full_scenario_replay() {
    // Simulate a complete lifecycle and return stage snapshots.
    fn run_scenario() -> Vec<Vec<u8>> {
        let config = make_config(500);
        let year = SECONDS_PER_YEAR as i64;
        let mut m = Market::zeroed();
        m.set_annual_interest_bps(1000);
        m.set_maturity_timestamp(year);
        m.set_max_total_supply(10_000_000_000);
        m.set_scale_factor(WAD);
        m.set_last_accrual_timestamp(0);
        m.set_scaled_total_supply(1_000_000_000);
        m.set_total_deposited(1_000_000_000);

        let mut snapshots = Vec::new();
        snapshots.push(market_bytes(&m)); // Stage 0: pre-accrual

        // Accrue at half year.
        accrue_interest(&mut m, &config, year / 2).unwrap();
        let expected_sf_half = expected_scale_factor_after_elapsed(WAD, 1000, year / 2);
        let expected_fee_half =
            expected_fee_delta(1_000_000_000, WAD, 1000, year / 2, 500);
        assert_eq!(m.scale_factor(), expected_sf_half);
        assert_eq!(m.accrued_protocol_fees(), expected_fee_half);
        assert_eq!(m.last_accrual_timestamp(), year / 2);
        snapshots.push(market_bytes(&m)); // Stage 1: half-year

        // Accrue at maturity.
        accrue_interest(&mut m, &config, year).unwrap();
        let expected_sf_full =
            expected_scale_factor_after_elapsed(expected_sf_half, 1000, year / 2);
        let expected_fee_second =
            expected_fee_delta(1_000_000_000, expected_sf_half, 1000, year / 2, 500);
        assert_eq!(m.scale_factor(), expected_sf_full);
        assert_eq!(
            m.accrued_protocol_fees(),
            expected_fee_half + expected_fee_second
        );
        assert_eq!(m.last_accrual_timestamp(), year);
        snapshots.push(market_bytes(&m)); // Stage 2: maturity

        // Accrue past maturity (must be a strict no-op).
        let before_noop = market_bytes(&m);
        accrue_interest(&mut m, &config, year * 2).unwrap();
        let after_noop = market_bytes(&m);
        assert_eq!(
            after_noop, before_noop,
            "post-maturity accrue must be no-op"
        );
        snapshots.push(after_noop); // Stage 3: post-maturity no-op

        snapshots
    }

    let baseline = run_scenario();
    for run in 1..=16 {
        let replay = run_scenario();
        assert_eq!(
            replay, baseline,
            "full lifecycle replay diverged on run {}",
            run
        );
    }
}

// ===========================================================================
// 4. FIELD SIZE BOUNDARY SAFETY (3 tests)
// ===========================================================================

/// Verify u64 fields do not silently overflow on maximum values
/// (accrue_interest should return an error).
#[test]
fn upgrade_boundary_u64_overflow_detection() {
    let year = SECONDS_PER_YEAR as i64;
    let config = make_config(10000); // 100% fee rate

    // Build boundary values from the oracle so the test tracks the production formula.
    let expected_new_sf = expected_scale_factor_after_elapsed(WAD, 10_000, year);
    let exact_fee_delta = expected_fee_delta(1, WAD, 10_000, year, 10_000);
    assert!(
        exact_fee_delta > 0,
        "boundary scenario fee delta should be positive"
    );

    // x-1 neighbor: succeeds and lands exactly at u64::MAX.
    let mut just_fits = make_market(10_000, i64::MAX, WAD, 1, 0, u64::MAX - exact_fee_delta);
    accrue_interest(&mut just_fits, &config, year).unwrap();
    assert_eq!(just_fits.accrued_protocol_fees(), u64::MAX);
    assert_eq!(just_fits.scale_factor(), expected_new_sf);
    assert_eq!(just_fits.last_accrual_timestamp(), year);

    // x neighbor: must overflow and remain fully unchanged.
    let mut overflows = make_market(10_000, i64::MAX, WAD, 1, 0, u64::MAX - exact_fee_delta + 1);
    let before_overflow = market_bytes(&overflows);
    let err = accrue_interest(&mut overflows, &config, year).unwrap_err();
    assert_eq!(err, ProgramError::Custom(LendingError::MathOverflow as u32));
    assert_eq!(
        market_bytes(&overflows),
        before_overflow,
        "overflow path must not mutate market state"
    );

    // x+1 neighbor: also overflows with identical error and no mutation.
    let mut definitely_overflows = make_market(10_000, i64::MAX, WAD, 1, 0, u64::MAX);
    let before = market_bytes(&definitely_overflows);
    let err = accrue_interest(&mut definitely_overflows, &config, year).unwrap_err();
    assert_eq!(err, ProgramError::Custom(LendingError::MathOverflow as u32));
    assert_eq!(
        market_bytes(&definitely_overflows),
        before,
        "u64::MAX fee base must reject atomically"
    );
}

/// Verify u128 scale_factor at maximum does not corrupt adjacent fields.
#[test]
fn upgrade_boundary_u128_scale_factor_max_no_corruption() {
    let mut m = Market::zeroed();
    m.set_annual_interest_bps(1000);
    m.set_maturity_timestamp(1_700_000_000);
    m.set_max_total_supply(5_000_000_000);
    m.set_scaled_total_supply(1_000_000_000);
    m.set_accrued_protocol_fees(42_000);

    // Set scale_factor to u128::MAX
    m.set_scale_factor(u128::MAX);

    // Verify scale_factor is correct
    assert_eq!(m.scale_factor(), u128::MAX);

    // Verify adjacent fields are NOT corrupted
    // scaled_total_supply is at offset 132..148, scale_factor is at 148..164
    assert_eq!(
        m.scaled_total_supply(),
        1_000_000_000,
        "scaled_total_supply corrupted by scale_factor max"
    );
    // accrued_protocol_fees is at offset 164..172
    assert_eq!(
        m.accrued_protocol_fees(),
        42_000,
        "accrued_protocol_fees corrupted by scale_factor max"
    );
    // annual_interest_bps is at offset 106..108
    assert_eq!(
        m.annual_interest_bps(),
        1000,
        "annual_interest_bps corrupted by scale_factor max"
    );

    // Also verify via raw bytes
    let raw: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
    assert_eq!(
        &raw[148..164],
        &[0xFF; 16],
        "scale_factor bytes should be all 0xFF"
    );
    assert_eq!(
        &raw[132..148],
        &1_000_000_000u128.to_le_bytes(),
        "scaled_total_supply bytes should be unchanged"
    );
    assert_eq!(
        &raw[164..172],
        &42_000u64.to_le_bytes(),
        "accrued_protocol_fees bytes should be unchanged"
    );
}

/// Verify timestamp fields handle maximum i64 values correctly.
#[test]
fn upgrade_boundary_timestamp_max_values() {
    let mut m = Market::zeroed();

    // Set maturity to i64::MAX
    m.set_maturity_timestamp(i64::MAX);
    assert_eq!(m.maturity_timestamp(), i64::MAX);

    // Set last_accrual to i64::MAX
    m.set_last_accrual_timestamp(i64::MAX);
    assert_eq!(m.last_accrual_timestamp(), i64::MAX);

    // Set to i64::MIN (negative)
    m.set_maturity_timestamp(i64::MIN);
    assert_eq!(m.maturity_timestamp(), i64::MIN);

    m.set_last_accrual_timestamp(i64::MIN);
    assert_eq!(m.last_accrual_timestamp(), i64::MIN);

    // Accrue with maturity = i64::MAX, last_accrual = i64::MAX - 1
    // time_elapsed should be 1 second
    m.set_annual_interest_bps(1000);
    m.set_maturity_timestamp(i64::MAX);
    m.set_scale_factor(WAD);
    m.set_scaled_total_supply(1_000_000);
    m.set_last_accrual_timestamp(i64::MAX - 1);
    m.set_accrued_protocol_fees(0);

    let config = make_config(0);
    // current_timestamp = i64::MAX, maturity = i64::MAX, effective_now = i64::MAX
    // time_elapsed = i64::MAX - (i64::MAX - 1) = 1
    accrue_interest(&mut m, &config, i64::MAX).unwrap();
    assert!(
        m.scale_factor() > WAD,
        "scale_factor should increase by 1 second of interest"
    );
    assert_eq!(m.last_accrual_timestamp(), i64::MAX);

    // Verify the accrue_interest result is sane for 1 second at 10%
    // interest_delta_wad = 1000 * 1 * WAD / (SECONDS_PER_YEAR * BPS)
    let expected_delta = 1000u128 * 1 * WAD / (SECONDS_PER_YEAR * BPS);
    let expected_sf = WAD + WAD * expected_delta / WAD;
    assert_eq!(m.scale_factor(), expected_sf);
}

// ===========================================================================
// 5. VERSION SENTINEL TESTS (3 tests)
// ===========================================================================

/// Verify is_initialized byte location and semantics for ProtocolConfig.
#[test]
fn upgrade_sentinel_is_initialized_location() {
    // Documented offset: is_initialized at byte 139
    assert_eq!(offset_of!(ProtocolConfig, is_initialized), 139);

    // Zeroed config: is_initialized = 0 (uninitialized)
    let c = ProtocolConfig::zeroed();
    assert_eq!(c.is_initialized, 0);

    // After initialization: is_initialized = 1
    let mut c = ProtocolConfig::zeroed();
    c.is_initialized = 1;
    assert_eq!(c.is_initialized, 1);

    // Verify via raw bytes
    let raw: &[u8; 194] = bytemuck::bytes_of(&c).try_into().unwrap();
    assert_eq!(raw[139], 1, "is_initialized should be at byte offset 139");

    // Verify that setting is_initialized does not corrupt adjacent fields
    c.set_fee_rate_bps(9999);
    c.blacklist_program = [0xDD; 32];
    c.is_initialized = 1;
    assert_eq!(c.fee_rate_bps(), 9999);
    assert_eq!(c.blacklist_program, [0xDD; 32]);
}

/// Verify that bump bytes are at documented offsets for all structs.
#[test]
fn upgrade_sentinel_bump_offsets() {
    // Market: bump at offset 228
    assert_eq!(offset_of!(Market, bump), 228);
    let mut m = Market::zeroed();
    m.bump = 255;
    let raw: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
    assert_eq!(raw[228], 255);

    // Market: market_authority_bump at offset 105
    assert_eq!(offset_of!(Market, market_authority_bump), 105);
    m.market_authority_bump = 128;
    let raw: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
    assert_eq!(raw[105], 128);

    // ProtocolConfig: bump at offset 140
    assert_eq!(offset_of!(ProtocolConfig, bump), 140);
    let mut c = ProtocolConfig::zeroed();
    c.bump = 200;
    let raw: &[u8; 194] = bytemuck::bytes_of(&c).try_into().unwrap();
    assert_eq!(raw[140], 200);

    // LenderPosition: bump at offset 89
    assert_eq!(offset_of!(LenderPosition, bump), 89);
    let mut p = LenderPosition::zeroed();
    p.bump = 42;
    let raw: &[u8; 128] = bytemuck::bytes_of(&p).try_into().unwrap();
    assert_eq!(raw[89], 42);

    // BorrowerWhitelist: bump at offset 58
    assert_eq!(offset_of!(BorrowerWhitelist, bump), 58);
    let mut w = BorrowerWhitelist::zeroed();
    w.bump = 77;
    let raw: &[u8; 96] = bytemuck::bytes_of(&w).try_into().unwrap();
    assert_eq!(raw[58], 77);
}

/// Test that a "v2" struct with an appended field in the padding area is
/// backwards-compatible with bytemuck reads of the original size.
#[test]
fn upgrade_sentinel_v2_appended_field_backwards_compatible() {
    // Simulate v2 Market: original 250 bytes with a new u64 field in padding [229..237]
    let m_v1 = make_nontrivial_market();
    let v1_bytes: Vec<u8> = bytemuck::bytes_of(&m_v1).to_vec();

    // "v2" writes a new field into the padding region
    let mut v2_bytes = v1_bytes.clone();
    let new_field_value: u64 = 0xCAFE_BABE_1234_5678;
    v2_bytes[229..237].copy_from_slice(&new_field_value.to_le_bytes());

    // Original v1 reader can still read the struct correctly
    let m_v1_read: &Market = bytemuck::from_bytes(&v2_bytes);
    assert_eq!(m_v1_read.borrower, m_v1.borrower);
    assert_eq!(m_v1_read.mint, m_v1.mint);
    assert_eq!(m_v1_read.vault, m_v1.vault);
    assert_eq!(m_v1_read.scale_factor(), m_v1.scale_factor());
    assert_eq!(
        m_v1_read.settlement_factor_wad(),
        m_v1.settlement_factor_wad()
    );
    assert_eq!(m_v1_read.bump, m_v1.bump);

    // The new field can be read from the raw bytes
    let new_field_read = u64::from_le_bytes(v2_bytes[229..237].try_into().unwrap());
    assert_eq!(new_field_read, new_field_value);

    // Verify the same for ProtocolConfig v2 (new field in padding [141..149])
    let c_v1 = make_nontrivial_config();
    let mut c_v2_bytes: Vec<u8> = bytemuck::bytes_of(&c_v1).to_vec();
    let config_new_field: u64 = 0xDEAD_BEEF_FACE_FEED;
    c_v2_bytes[141..149].copy_from_slice(&config_new_field.to_le_bytes());

    let c_v1_read: &ProtocolConfig = bytemuck::from_bytes(&c_v2_bytes);
    assert_eq!(c_v1_read.admin, c_v1.admin);
    assert_eq!(c_v1_read.fee_rate_bps(), c_v1.fee_rate_bps());
    assert_eq!(c_v1_read.is_initialized, c_v1.is_initialized);
    assert_eq!(c_v1_read.bump, c_v1.bump);

    // LenderPosition v2 (new field in padding [90..98])
    let p_v1 = make_nontrivial_lender_position();
    let mut p_v2_bytes: Vec<u8> = bytemuck::bytes_of(&p_v1).to_vec();
    let pos_new_field: u64 = 0xAAAA_BBBB_CCCC_DDDD;
    p_v2_bytes[90..98].copy_from_slice(&pos_new_field.to_le_bytes());

    let p_v1_read: &LenderPosition = bytemuck::from_bytes(&p_v2_bytes);
    assert_eq!(p_v1_read.market, p_v1.market);
    assert_eq!(p_v1_read.lender, p_v1.lender);
    assert_eq!(p_v1_read.scaled_balance(), p_v1.scaled_balance());
    assert_eq!(p_v1_read.bump, p_v1.bump);

    // BorrowerWhitelist v2 (new field in padding [59..67])
    let w_v1 = make_nontrivial_borrower_whitelist();
    let mut w_v2_bytes: Vec<u8> = bytemuck::bytes_of(&w_v1).to_vec();
    let wl_new_field: u64 = 0x1111_2222_3333_4444;
    w_v2_bytes[59..67].copy_from_slice(&wl_new_field.to_le_bytes());

    let w_v1_read: &BorrowerWhitelist = bytemuck::from_bytes(&w_v2_bytes);
    assert_eq!(w_v1_read.borrower, w_v1.borrower);
    assert_eq!(w_v1_read.is_whitelisted, w_v1.is_whitelisted);
    assert_eq!(w_v1_read.max_borrow_capacity(), w_v1.max_borrow_capacity());
    assert_eq!(w_v1_read.current_borrowed(), w_v1.current_borrowed());
    assert_eq!(w_v1_read.bump, w_v1.bump);
}

// ===========================================================================
// 6. INVARIANT PRESERVATION ACROSS UPGRADE (2 tests)
// ===========================================================================

/// Verify 6 core invariants at various market lifecycle stages:
///   1. scale_factor >= WAD (when initialized to WAD, it only grows)
///   2. scaled_total_supply * scale_factor / WAD == approximate total normalized supply
///   3. settlement_factor_wad <= WAD (capped)
///   4. settlement_factor_wad >= 1 (floor) when nonzero
///   5. total_borrowed <= total_deposited (can't borrow more than deposited)
///   6. last_accrual_timestamp <= maturity_timestamp (accrual is capped)
fn check_invariants(m: &Market, label: &str) {
    let sf = m.scale_factor();
    let settlement = m.settlement_factor_wad();

    // Invariant 1: scale_factor >= WAD when initialized to WAD
    if sf > 0 {
        assert!(
            sf >= WAD,
            "{}: scale_factor ({}) < WAD ({})",
            label,
            sf,
            WAD
        );
    }

    // Invariant 2: total_normalized is computable without overflow (when sf > 0)
    if sf > 0 {
        let total_normalized = m.scaled_total_supply().checked_mul(sf);
        assert!(
            total_normalized.is_some(),
            "{}: total_normalized overflow",
            label
        );
    }

    // Invariant 3: settlement_factor_wad <= WAD
    assert!(
        settlement <= WAD,
        "{}: settlement ({}) > WAD",
        label,
        settlement
    );

    // Invariant 4: settlement_factor_wad >= 1 when nonzero
    if settlement > 0 {
        assert!(
            settlement >= 1,
            "{}: settlement ({}) < 1",
            label,
            settlement
        );
    }

    // Invariant 5: total_borrowed <= total_deposited
    assert!(
        m.total_borrowed() <= m.total_deposited(),
        "{}: total_borrowed ({}) > total_deposited ({})",
        label,
        m.total_borrowed(),
        m.total_deposited()
    );

    // Invariant 6: last_accrual_timestamp <= maturity_timestamp
    assert!(
        m.last_accrual_timestamp() <= m.maturity_timestamp(),
        "{}: last_accrual ({}) > maturity ({})",
        label,
        m.last_accrual_timestamp(),
        m.maturity_timestamp()
    );
}

/// Simulate a market at various lifecycle stages and verify all 6 core
/// invariants hold when re-reading state from serialized bytes.
#[test]
fn upgrade_invariant_lifecycle_stages() {
    let creation_ts: i64 = 1_000_000;
    let year = SECONDS_PER_YEAR as i64;
    let maturity_ts: i64 = creation_ts + year;
    let config = make_config(500);

    // Stage 1: Pre-maturity with active borrows
    let mut m = Market::zeroed();
    m.set_annual_interest_bps(1000);
    m.set_maturity_timestamp(maturity_ts);
    m.set_max_total_supply(10_000_000_000);
    m.set_scale_factor(WAD);
    m.set_last_accrual_timestamp(creation_ts);
    m.set_total_deposited(5_000_000_000);
    m.set_total_borrowed(2_000_000_000);
    m.set_scaled_total_supply(5_000_000_000);

    // Zero-elapsed boundary: pre-accrual call must be no-op.
    let before_noop = market_bytes(&m);
    accrue_interest(&mut m, &config, creation_ts).unwrap();
    assert_eq!(market_bytes(&m), before_noop, "zero elapsed must be no-op");

    // Accrue half a year of interest
    let half_year_ts = creation_ts + year / 2;
    accrue_interest(&mut m, &config, half_year_ts).unwrap();
    let expected_half_sf = expected_scale_factor_after_elapsed(WAD, 1000, year / 2);
    let expected_half_fee =
        expected_fee_delta(5_000_000_000, WAD, 1000, year / 2, 500);
    assert_eq!(m.scale_factor(), expected_half_sf);
    assert_eq!(m.accrued_protocol_fees(), expected_half_fee);
    assert_eq!(m.last_accrual_timestamp(), half_year_ts);

    // Serialize and deserialize
    let bytes = bytemuck::bytes_of(&m).to_vec();
    let m_read: &Market = bytemuck::from_bytes(&bytes);
    assert_eq!(
        bytes,
        market_bytes(m_read),
        "stage 1 roundtrip bytes mismatch"
    );
    check_invariants(m_read, "Stage 1: pre-maturity with active borrows");

    // Stage 2: Post-maturity pre-settlement
    accrue_interest(&mut m, &config, maturity_ts + 1_000_000).unwrap();
    let expected_full_sf = expected_scale_factor_after_elapsed(expected_half_sf, 1000, year / 2);
    let expected_second_fee =
        expected_fee_delta(5_000_000_000, expected_half_sf, 1000, year / 2, 500);
    assert_eq!(m.scale_factor(), expected_full_sf);
    assert_eq!(
        m.accrued_protocol_fees(),
        expected_half_fee + expected_second_fee
    );

    let bytes = bytemuck::bytes_of(&m).to_vec();
    let m_read: &Market = bytemuck::from_bytes(&bytes);
    assert_eq!(
        bytes,
        market_bytes(m_read),
        "stage 2 roundtrip bytes mismatch"
    );
    check_invariants(m_read, "Stage 2: post-maturity pre-settlement");
    assert_eq!(
        m_read.last_accrual_timestamp(),
        maturity_ts,
        "should cap at maturity"
    );
    let before_post_maturity_noop = market_bytes(&m);
    accrue_interest(&mut m, &config, maturity_ts + 2_000_000).unwrap();
    assert_eq!(
        market_bytes(&m),
        before_post_maturity_noop,
        "repeated post-maturity accrual must be no-op"
    );

    // Stage 3: Post-settlement
    m.set_total_repaid(1_000_000_000);
    // Compute settlement factor: vault has (deposited - borrowed + repaid) = 4B
    let vault_balance: u128 = 4_000_000_000;
    let fees = u128::from(m.accrued_protocol_fees());
    let fees_reserved = core::cmp::min(vault_balance, fees);
    let available = vault_balance - fees_reserved;
    let total_normalized = m
        .scaled_total_supply()
        .checked_mul(m.scale_factor())
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    let settlement = compute_settlement_factor(available, total_normalized);
    let expected_settlement = expected_settlement_factor(available, total_normalized);
    assert_eq!(
        settlement, expected_settlement,
        "settlement formula mismatch"
    );
    m.set_settlement_factor_wad(settlement);

    let bytes = bytemuck::bytes_of(&m).to_vec();
    let m_read: &Market = bytemuck::from_bytes(&bytes);
    assert_eq!(
        bytes,
        market_bytes(m_read),
        "stage 3 roundtrip bytes mismatch"
    );
    check_invariants(m_read, "Stage 3: post-settlement");
    assert!(
        m_read.settlement_factor_wad() > 0,
        "settlement should be set"
    );
    assert!(
        m_read.settlement_factor_wad() <= WAD,
        "settlement should be <= WAD"
    );
}

/// proptest: Generate random valid market states, serialize to bytes, deserialize
/// back, and verify all invariants.
mod proptest_invariants {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn upgrade_invariant_random_valid_market(
            annual_bps in 0u16..=10_000,
            maturity_offset in 1_000i64..=315_360_000,
            total_deposited in 1_000u64..=10_000_000_000,
            borrow_ratio in 0u32..=100,
            repay_ratio in 0u32..=100,
            accrual_fraction in 0u32..=100,
            fee_rate_bps in 0u16..=10_000,
        ) {
            let creation_ts: i64 = 1_000_000;
            let maturity_ts = creation_ts + maturity_offset;
            let total_borrowed = (total_deposited as u128 * borrow_ratio as u128 / 100) as u64;
            let total_repaid = (total_borrowed as u128 * repay_ratio as u128 / 100) as u64;

            let mut m = Market::zeroed();
            m.set_annual_interest_bps(annual_bps);
            m.set_maturity_timestamp(maturity_ts);
            m.set_max_total_supply(total_deposited * 2);
            m.set_scale_factor(WAD);
            m.set_last_accrual_timestamp(creation_ts);
            m.set_total_deposited(total_deposited);
            m.set_total_borrowed(total_borrowed);
            m.set_total_repaid(total_repaid);
            m.set_scaled_total_supply(u128::from(total_deposited));

            // Accrue partial interest
            let accrual_ts = creation_ts + (maturity_offset as i64 * accrual_fraction as i64 / 100);
            let config = make_config(fee_rate_bps);
            let _ = accrue_interest(&mut m, &config, accrual_ts);

            // Serialize and deserialize
            let bytes = bytemuck::bytes_of(&m).to_vec();
            let m_read: &Market = bytemuck::from_bytes(&bytes);

            // Check invariants
            let sf = m_read.scale_factor();
            if sf > 0 {
                prop_assert!(sf >= WAD, "scale_factor ({}) < WAD", sf);
            }

            let settlement = m_read.settlement_factor_wad();
            prop_assert!(settlement <= WAD, "settlement ({}) > WAD", settlement);

            prop_assert!(
                m_read.total_borrowed() <= m_read.total_deposited(),
                "total_borrowed ({}) > total_deposited ({})",
                m_read.total_borrowed(), m_read.total_deposited()
            );

            prop_assert!(
                m_read.last_accrual_timestamp() <= m_read.maturity_timestamp(),
                "last_accrual ({}) > maturity ({})",
                m_read.last_accrual_timestamp(), m_read.maturity_timestamp()
            );

            // Verify byte equality after roundtrip
            let bytes2 = bytemuck::bytes_of(m_read);
            prop_assert_eq!(&bytes[..], bytes2, "roundtrip byte mismatch");
        }
    }
}
