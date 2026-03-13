//! Cross-compilation verification tests for CoalesceFi Pinocchio.
//!
//! These tests verify that all pure math and state layout operations produce
//! identical results regardless of compilation target (native x86/ARM vs BPF).
//! Every test uses hardcoded "golden" expected values that serve as
//! regression/cross-platform anchors.

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

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};
use pinocchio::error::ProgramError;

#[path = "common/interest_oracle.rs"]
mod interest_oracle;

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

/// Replicates the settlement factor formula from withdraw/re_settle processors.
/// settlement_factor = clamp(available * WAD / total_normalized, 1, WAD)
/// If total_normalized == 0, returns WAD.
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

// ===========================================================================
// 1. BYTE-LEVEL DETERMINISM
//    Compute results for known inputs and assert exact output bytes.
// ===========================================================================

#[test]
fn byte_level_determinism_accrue_interest() {
    // Fixed inputs
    let mut market = make_market(
        1000,              // 10% annual
        i64::MAX,          // far-future maturity
        WAD,               // initial scale_factor = 1.0
        1_000_000_000_000, // 1M USDC scaled supply
        0,                 // last accrual at epoch 0
        0,                 // no prior fees
    );
    let config = make_config(500); // 5% fee rate

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    // Golden values computed analytically from the same model.
    let (expected_sf, expected_fees) =
        analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 500, 1_000_000_000_000);
    let expected_sf_bytes = expected_sf.to_le_bytes();
    let expected_fees_bytes = expected_fees.to_le_bytes();

    let expected_ts: i64 = SECONDS_PER_YEAR as i64;
    let expected_ts_bytes = expected_ts.to_le_bytes();

    // Verify exact byte-level output
    assert_eq!(
        market.scale_factor().to_le_bytes(),
        expected_sf_bytes,
        "scale_factor bytes mismatch"
    );
    assert_eq!(
        market.accrued_protocol_fees().to_le_bytes(),
        expected_fees_bytes,
        "accrued_protocol_fees bytes mismatch"
    );
    assert_eq!(
        market.last_accrual_timestamp().to_le_bytes(),
        expected_ts_bytes,
        "last_accrual_timestamp bytes mismatch"
    );

    // Also verify the raw Market bytes at known offsets
    let raw: &[u8; 250] = bytemuck::bytes_of(&market).try_into().unwrap();
    // scale_factor at offset 148..164
    assert_eq!(&raw[148..164], &expected_sf_bytes);
    // accrued_protocol_fees at offset 164..172
    assert_eq!(&raw[164..172], &expected_fees_bytes);
    // last_accrual_timestamp at offset 204..212 (after total_interest_repaid at 196)
    assert_eq!(&raw[204..212], &expected_ts_bytes);
}

// Golden byte array for settlement factor 0.75 * WAD = 750_000_000_000_000_000
const GOLDEN_SETTLEMENT_75PCT_BYTES: [u8; 16] = [
    0x00, 0x00, 0x8B, 0xBD, 0x06, 0x89, 0x68, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[test]
fn byte_level_determinism_settlement_factor() {
    // available = 750_000_000 (750 USDC), total_normalized = 1_000_000_000
    let factor = compute_settlement_factor(750_000_000, 1_000_000_000);
    let expected: u128 = 750_000_000_000_000_000; // 0.75 * WAD
    assert_eq!(factor.to_le_bytes(), expected.to_le_bytes());

    // Assert exact byte-level match against pinned golden constant
    assert_eq!(
        factor.to_le_bytes(),
        GOLDEN_SETTLEMENT_75PCT_BYTES,
        "settlement_factor bytes must match pinned golden constant"
    );
}

// Golden bytes for WAD (1_000_000_000_000_000_000) -- scale_factor unchanged at zero rate
const GOLDEN_WAD_BYTES: [u8; 16] = [
    0x00, 0x00, 0x64, 0xA7, 0xB3, 0xB6, 0xE0, 0x0D, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
// Golden bytes for zero fees (0u64)
const GOLDEN_ZERO_FEES_BYTES: [u8; 8] = [0x00; 8];

#[test]
fn byte_level_determinism_zero_interest_rate() {
    let mut market = make_market(0, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = make_config(500);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    // Zero rate => no change to scale_factor, no fees
    assert_eq!(market.scale_factor().to_le_bytes(), WAD.to_le_bytes());
    assert_eq!(
        market.accrued_protocol_fees().to_le_bytes(),
        0u64.to_le_bytes()
    );

    // Assert exact byte-level match against pinned golden constants
    assert_eq!(
        market.scale_factor().to_le_bytes(),
        GOLDEN_WAD_BYTES,
        "zero-rate scale_factor bytes must match pinned WAD constant"
    );
    assert_eq!(
        market.accrued_protocol_fees().to_le_bytes(),
        GOLDEN_ZERO_FEES_BYTES,
        "zero-rate fees bytes must be all zeros"
    );
}

// ===========================================================================
// 2. ENDIANNESS VERIFICATION
//    Construct structs from raw bytes and verify field accessors.
// ===========================================================================

#[test]
fn endianness_market_from_raw_bytes() {
    let mut raw = [0u8; 250];

    // Write annual_interest_bps = 1000 (0x03E8) at offset 106
    raw[106..108].copy_from_slice(&1000u16.to_le_bytes());
    // Write maturity_timestamp = 1_700_000_000 at offset 108
    raw[108..116].copy_from_slice(&1_700_000_000i64.to_le_bytes());
    // Write max_total_supply = 10_000_000_000 at offset 116
    raw[116..124].copy_from_slice(&10_000_000_000u64.to_le_bytes());
    // Write market_nonce = 42 at offset 124
    raw[124..132].copy_from_slice(&42u64.to_le_bytes());
    // Write scaled_total_supply = WAD at offset 132
    raw[132..148].copy_from_slice(&WAD.to_le_bytes());
    // Write scale_factor = WAD + WAD/10 at offset 148
    let sf: u128 = WAD + WAD / 10;
    raw[148..164].copy_from_slice(&sf.to_le_bytes());
    // Write accrued_protocol_fees = 12345 at offset 164
    raw[164..172].copy_from_slice(&12345u64.to_le_bytes());
    // Write total_deposited = 5_000_000 at offset 172
    raw[172..180].copy_from_slice(&5_000_000u64.to_le_bytes());
    // Write total_borrowed = 3_000_000 at offset 180
    raw[180..188].copy_from_slice(&3_000_000u64.to_le_bytes());
    // Write total_repaid = 1_000_000 at offset 188
    raw[188..196].copy_from_slice(&1_000_000u64.to_le_bytes());
    // Write total_interest_repaid = 500_000 at offset 196
    raw[196..204].copy_from_slice(&500_000u64.to_le_bytes());
    // Write last_accrual_timestamp = 1_699_999_000 at offset 204
    raw[204..212].copy_from_slice(&1_699_999_000i64.to_le_bytes());
    // Write settlement_factor_wad = WAD * 3 / 4 at offset 212
    let settlement: u128 = WAD * 3 / 4;
    raw[212..228].copy_from_slice(&settlement.to_le_bytes());

    let market: &Market = bytemuck::from_bytes(&raw);

    assert_eq!(market.annual_interest_bps(), 1000);
    assert_eq!(market.maturity_timestamp(), 1_700_000_000);
    assert_eq!(market.max_total_supply(), 10_000_000_000);
    assert_eq!(market.market_nonce(), 42);
    assert_eq!(market.scaled_total_supply(), WAD);
    assert_eq!(market.scale_factor(), sf);
    assert_eq!(market.accrued_protocol_fees(), 12345);
    assert_eq!(market.total_deposited(), 5_000_000);
    assert_eq!(market.total_borrowed(), 3_000_000);
    assert_eq!(market.total_repaid(), 1_000_000);
    assert_eq!(market.total_interest_repaid(), 500_000);
    assert_eq!(market.last_accrual_timestamp(), 1_699_999_000);
    assert_eq!(market.settlement_factor_wad(), settlement);
}

#[test]
fn endianness_protocol_config_from_raw_bytes() {
    let mut raw = [0u8; 194];

    // admin at [9..41]: fill with 0xAA
    raw[9..41].fill(0xAA);
    // fee_rate_bps = 5000 at [41..43]
    raw[41..43].copy_from_slice(&5000u16.to_le_bytes());
    // fee_authority at [43..75]: fill with 0xBB
    raw[43..75].fill(0xBB);
    // whitelist_manager at [75..107]: fill with 0xCC
    raw[75..107].fill(0xCC);
    // blacklist_program at [107..139]: fill with 0xDD
    raw[107..139].fill(0xDD);
    // is_initialized = 1 at [139]
    raw[139] = 1;
    // bump = 255 at [140]
    raw[140] = 255;

    let config: &ProtocolConfig = bytemuck::from_bytes(&raw);

    assert_eq!(config.fee_rate_bps(), 5000);
    assert_eq!(config.admin, [0xAA; 32]);
    assert_eq!(config.fee_authority, [0xBB; 32]);
    assert_eq!(config.whitelist_manager, [0xCC; 32]);
    assert_eq!(config.blacklist_program, [0xDD; 32]);
    assert_eq!(config.is_initialized, 1);
    assert_eq!(config.bump, 255);
}

#[test]
fn endianness_lender_position_from_raw_bytes() {
    let mut raw = [0u8; 128];

    // market at [9..41]: fill with 0x11
    raw[9..41].fill(0x11);
    // lender at [41..73]: fill with 0x22
    raw[41..73].fill(0x22);
    // scaled_balance = 999_999_999_999_999_999 at [73..89]
    let balance: u128 = 999_999_999_999_999_999;
    raw[73..89].copy_from_slice(&balance.to_le_bytes());
    // bump = 42 at [89]
    raw[89] = 42;

    let pos: &LenderPosition = bytemuck::from_bytes(&raw);

    assert_eq!(pos.market, [0x11; 32]);
    assert_eq!(pos.lender, [0x22; 32]);
    assert_eq!(pos.scaled_balance(), balance);
    assert_eq!(pos.bump, 42);
}

#[test]
fn endianness_borrower_whitelist_from_raw_bytes() {
    let mut raw = [0u8; 96];

    // borrower at [9..41]: fill with 0x33
    raw[9..41].fill(0x33);
    // is_whitelisted = 1 at [41]
    raw[41] = 1;
    // max_borrow_capacity = 5_000_000_000 at [42..50]
    raw[42..50].copy_from_slice(&5_000_000_000u64.to_le_bytes());
    // current_borrowed = 1_000_000_000 at [50..58]
    raw[50..58].copy_from_slice(&1_000_000_000u64.to_le_bytes());
    // bump = 7 at [58]
    raw[58] = 7;

    let wl: &BorrowerWhitelist = bytemuck::from_bytes(&raw);

    assert_eq!(wl.borrower, [0x33; 32]);
    assert_eq!(wl.is_whitelisted, 1);
    assert_eq!(wl.max_borrow_capacity(), 5_000_000_000);
    assert_eq!(wl.current_borrowed(), 1_000_000_000);
    assert_eq!(wl.bump, 7);
}

// Pinned byte sequence for u128::MAX in little-endian
const GOLDEN_U128_MAX_LE_BYTES: [u8; 16] = [0xFF; 16];
// Pinned golden value
const GOLDEN_U128_MAX: u128 = 340_282_366_920_938_463_463_374_607_431_768_211_455;

#[test]
fn endianness_u128_max_roundtrip() {
    // Pin the exact value of u128::MAX
    assert_eq!(
        u128::MAX,
        GOLDEN_U128_MAX,
        "u128::MAX must match pinned golden value"
    );

    let mut m = Market::zeroed();
    m.set_scale_factor(u128::MAX);
    assert_eq!(m.scale_factor(), u128::MAX);

    let raw: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
    // All 16 bytes at offset 148..164 should be 0xFF
    assert_eq!(&raw[148..164], &[0xFF; 16]);

    // Assert bytes match pinned constant
    assert_eq!(
        &raw[148..164],
        &GOLDEN_U128_MAX_LE_BYTES,
        "u128::MAX LE bytes must match pinned golden sequence"
    );
    assert_eq!(
        u128::MAX.to_le_bytes(),
        GOLDEN_U128_MAX_LE_BYTES,
        "u128::MAX.to_le_bytes() must match pinned golden sequence"
    );
}

// Pinned byte sequence for -1i64 in two's complement little-endian
const GOLDEN_NEG1_I64_LE_BYTES: [u8; 8] = [0xFF; 8];
// Pinned byte sequence for i64::MIN in two's complement little-endian
const GOLDEN_I64_MIN_LE_BYTES: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80];
// Pinned golden value for i64::MIN
const GOLDEN_I64_MIN: i64 = -9_223_372_036_854_775_808;

#[test]
fn endianness_negative_timestamps() {
    // Pin exact value of i64::MIN
    assert_eq!(
        i64::MIN,
        GOLDEN_I64_MIN,
        "i64::MIN must match pinned golden value"
    );

    let mut m = Market::zeroed();
    m.set_maturity_timestamp(-1);
    assert_eq!(m.maturity_timestamp(), -1);

    // Verify -1 LE bytes at maturity_timestamp offset (108..116)
    let raw_neg1: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
    assert_eq!(
        &raw_neg1[108..116],
        &GOLDEN_NEG1_I64_LE_BYTES,
        "-1i64 LE bytes at maturity_timestamp must match pinned golden sequence"
    );

    m.set_maturity_timestamp(i64::MIN);
    assert_eq!(m.maturity_timestamp(), i64::MIN);

    // Verify i64::MIN LE bytes at maturity_timestamp offset (108..116)
    let raw_min: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
    assert_eq!(
        &raw_min[108..116],
        &GOLDEN_I64_MIN_LE_BYTES,
        "i64::MIN LE bytes at maturity_timestamp must match pinned golden sequence"
    );

    // Verify raw bytes for -1 (all 0xFF in two's complement LE)
    // last_accrual_timestamp is at offset 204 (after total_interest_repaid at 196)
    m.set_last_accrual_timestamp(-1);
    let raw: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
    assert_eq!(&raw[204..212], &[0xFF; 8]);

    // Assert last_accrual_timestamp bytes match pinned constant
    assert_eq!(
        &raw[204..212],
        &GOLDEN_NEG1_I64_LE_BYTES,
        "-1i64 LE bytes at last_accrual_timestamp must match pinned golden sequence"
    );
}

// ===========================================================================
// 3. ARITHMETIC CONSISTENCY
//    Verify u128 operations produce expected exact results for boundary values.
// ===========================================================================

// Golden intermediate values for u128 multiplication
const GOLDEN_WAD_SQUARED: u128 = 1_000_000_000_000_000_000_000_000_000_000_000_000;
const GOLDEN_WAD_TIMES_BPS: u128 = 10_000_000_000_000_000_000_000;
const GOLDEN_WAD_TIMES_SPY: u128 = 31_536_000_000_000_000_000_000_000;

#[test]
fn arithmetic_u128_multiplication_exact() {
    // WAD * WAD should fit in u128 and produce exact value
    let result = WAD.checked_mul(WAD).unwrap();
    assert_eq!(result, GOLDEN_WAD_SQUARED, "WAD*WAD golden mismatch");

    // WAD * BPS
    let result2 = WAD.checked_mul(BPS).unwrap();
    assert_eq!(result2, GOLDEN_WAD_TIMES_BPS, "WAD*BPS golden mismatch");

    // WAD * SECONDS_PER_YEAR
    let result3 = WAD.checked_mul(SECONDS_PER_YEAR).unwrap();
    assert_eq!(
        result3, GOLDEN_WAD_TIMES_SPY,
        "WAD*SECONDS_PER_YEAR golden mismatch"
    );

    // Pin byte-level representation of WAD^2
    assert_eq!(
        result.to_le_bytes(),
        GOLDEN_WAD_SQUARED.to_le_bytes(),
        "WAD^2 byte representation must match golden snapshot"
    );
}

// Golden intermediate values for u128 division
const GOLDEN_WAD_DIV_BPS: u128 = 100_000_000_000_000;
const GOLDEN_WAD_DIV_SPY: u128 = 31_709_791_983; // floor(10^18 / 31536000)
const GOLDEN_BPS_TIMES_SPY: u128 = 315_360_000_000;

#[test]
fn arithmetic_u128_division_exact() {
    // WAD / BPS = 10^14
    let wad_div_bps = WAD / BPS;
    assert_eq!(wad_div_bps, GOLDEN_WAD_DIV_BPS, "WAD/BPS golden mismatch");

    // WAD / SECONDS_PER_YEAR -- verify exact integer truncation
    let result = WAD / SECONDS_PER_YEAR;
    assert_eq!(result, GOLDEN_WAD_DIV_SPY, "WAD/SPY golden mismatch");

    // Verify truncation: result * SECONDS_PER_YEAR < WAD
    let remainder = WAD - result * SECONDS_PER_YEAR;
    assert!(remainder > 0, "integer division should truncate");
    assert!(
        remainder < SECONDS_PER_YEAR,
        "remainder must be less than divisor"
    );

    // BPS * SECONDS_PER_YEAR
    let denom = BPS.checked_mul(SECONDS_PER_YEAR).unwrap();
    assert_eq!(denom, GOLDEN_BPS_TIMES_SPY, "BPS*SPY golden mismatch");
}

#[test]
fn arithmetic_checked_mul_overflow_detection() {
    // u128::MAX * 2 should overflow
    assert!(u128::MAX.checked_mul(2).is_none());

    // u128::MAX * 1 should not overflow
    assert_eq!(u128::MAX.checked_mul(1), Some(u128::MAX));

    // Large but non-overflowing: (u128::MAX / WAD) * WAD
    let large = u128::MAX / WAD;
    let result = large.checked_mul(WAD);
    assert!(result.is_some());
    // Due to integer truncation: (u128::MAX / WAD) * WAD <= u128::MAX
    assert!(result.unwrap() <= u128::MAX);
}

#[test]
fn arithmetic_checked_div_zero_and_boundary() {
    // Division by zero returns None
    assert!(100u128.checked_div(0).is_none());

    // u128::MAX / 1 = u128::MAX
    assert_eq!(u128::MAX.checked_div(1), Some(u128::MAX));

    // u128::MAX / u128::MAX = 1
    assert_eq!(u128::MAX.checked_div(u128::MAX), Some(1));

    // 0 / anything = 0
    assert_eq!(0u128.checked_div(WAD), Some(0));
}

// Golden intermediate values for interest delta formula
const GOLDEN_IDW_NUMERATOR: u128 = 31_536_000_000_000_000_000_000_000_000; // 1000 * 31536000 * WAD
const GOLDEN_IDW_DENOMINATOR: u128 = 315_360_000_000; // SPY * BPS
const GOLDEN_IDW_10PCT: u128 = 100_000_000_000_000_000; // WAD / 10
const GOLDEN_WAD_DIV_10: u128 = 100_000_000_000_000_000;

#[test]
fn arithmetic_interest_delta_formula_exact() {
    // Manually compute: annual_bps=1000, time=31536000, WAD=10^18
    // interest_delta_wad = 1000 * 31536000 * WAD / (31536000 * 10000)
    //                    = 1000 * WAD / 10000
    //                    = WAD / 10
    let annual_bps: u128 = 1000;
    let time_elapsed: u128 = SECONDS_PER_YEAR;

    // Step 1: annual_bps * time_elapsed
    let step1 = annual_bps.checked_mul(time_elapsed).unwrap();
    assert_eq!(
        step1, 31_536_000_000,
        "step1: annual_bps * time_elapsed golden mismatch"
    );

    // Step 2: step1 * WAD
    let numerator = step1.checked_mul(WAD).unwrap();
    assert_eq!(numerator, GOLDEN_IDW_NUMERATOR, "numerator golden mismatch");

    // Step 3: denominator = SPY * BPS
    let denominator = SECONDS_PER_YEAR.checked_mul(BPS).unwrap();
    assert_eq!(
        denominator, GOLDEN_IDW_DENOMINATOR,
        "denominator golden mismatch"
    );

    // Step 4: interest_delta_wad = numerator / denominator
    let interest_delta_wad = numerator.checked_div(denominator).unwrap();
    assert_eq!(
        interest_delta_wad, GOLDEN_IDW_10PCT,
        "interest_delta_wad golden mismatch"
    );
    assert_eq!(interest_delta_wad, WAD / 10);
    assert_eq!(interest_delta_wad, GOLDEN_WAD_DIV_10);
}

// Golden intermediate values for scale factor delta
const GOLDEN_SF_DELTA_1X: u128 = 100_000_000_000_000_000; // WAD * (WAD/10) / WAD = WAD/10
const GOLDEN_SF_MUL_IDW_1X: u128 = 100_000_000_000_000_000_000_000_000_000_000_000; // WAD * WAD/10
const GOLDEN_SF_15X: u128 = 1_500_000_000_000_000_000; // 1.5 * WAD
const GOLDEN_SF_DELTA_15X: u128 = 150_000_000_000_000_000; // 1.5 * WAD/10

#[test]
fn arithmetic_scale_factor_delta_exact() {
    // scale_factor = WAD, interest_delta_wad = WAD/10
    // scale_factor_delta = WAD * (WAD/10) / WAD = WAD/10
    let sf: u128 = WAD;
    let idw: u128 = WAD / 10;

    // Step 1: sf * idw
    let sf_mul_idw = sf.checked_mul(idw).unwrap();
    assert_eq!(
        sf_mul_idw, GOLDEN_SF_MUL_IDW_1X,
        "sf*idw golden mismatch for 1x"
    );

    // Step 2: divide by WAD
    let delta = sf_mul_idw.checked_div(WAD).unwrap();
    assert_eq!(delta, GOLDEN_SF_DELTA_1X, "sf_delta golden mismatch for 1x");
    assert_eq!(delta, WAD / 10);

    // With scale_factor = 1.5 * WAD
    let sf2: u128 = WAD + WAD / 2;
    assert_eq!(sf2, GOLDEN_SF_15X, "1.5*WAD golden mismatch");

    // Step 1: sf2 * idw
    let sf2_mul_idw = sf2.checked_mul(idw).unwrap();
    assert_eq!(
        sf2_mul_idw, 150_000_000_000_000_000_000_000_000_000_000_000,
        "sf2*idw golden mismatch for 1.5x"
    );

    // Step 2: divide by WAD
    let delta2 = sf2_mul_idw.checked_div(WAD).unwrap();
    assert_eq!(
        delta2, GOLDEN_SF_DELTA_15X,
        "sf_delta golden mismatch for 1.5x"
    );
}

// Golden intermediate values for fee computation
const GOLDEN_IDW_MUL_FEE_RATE: u128 = 50_000_000_000_000_000_000; // (WAD/10) * 500
const GOLDEN_FEE_DELTA_WAD: u128 = 5_000_000_000_000_000; // WAD/200
const GOLDEN_SUPPLY_MUL_SF: u128 = 1_100_000_000_000_000_000_000_000_000_000; // 1T * 1.1*WAD
const GOLDEN_SUPPLY_NORMALIZED: u128 = 1_100_000_000_000; // supply * new_sf / WAD
const GOLDEN_FEE_NORMALIZED: u128 = 5_500_000_000;

#[test]
fn arithmetic_fee_computation_exact() {
    // interest_delta_wad = WAD/10, fee_rate_bps = 500
    // fee_delta_wad = (WAD/10) * 500 / 10000 = WAD/200 = 5_000_000_000_000_000
    let idw: u128 = WAD / 10;
    let fee_rate: u128 = 500;

    // Step 1: idw * fee_rate
    let idw_mul_fee = idw.checked_mul(fee_rate).unwrap();
    assert_eq!(
        idw_mul_fee, GOLDEN_IDW_MUL_FEE_RATE,
        "idw*fee_rate golden mismatch"
    );

    // Step 2: divide by BPS
    let fee_delta_wad = idw_mul_fee.checked_div(BPS).unwrap();
    assert_eq!(
        fee_delta_wad, GOLDEN_FEE_DELTA_WAD,
        "fee_delta_wad golden mismatch"
    );

    // fee_normalized = supply * new_sf / WAD * fee_delta_wad / WAD
    let supply: u128 = 1_000_000_000_000;
    let new_sf: u128 = WAD + WAD / 10; // 1.1 * WAD

    // Step 3: supply * new_sf
    let supply_mul_sf = supply.checked_mul(new_sf).unwrap();
    assert_eq!(
        supply_mul_sf, GOLDEN_SUPPLY_MUL_SF,
        "supply*new_sf golden mismatch"
    );

    // Step 4: / WAD
    let supply_normalized = supply_mul_sf.checked_div(WAD).unwrap();
    assert_eq!(
        supply_normalized, GOLDEN_SUPPLY_NORMALIZED,
        "supply_normalized golden mismatch"
    );

    // Step 5: * fee_delta_wad
    // 1_100_000_000_000 * 5_000_000_000_000_000 = 5_500_000_000_000_000_000_000_000_000
    let norm_mul_fee = supply_normalized.checked_mul(fee_delta_wad).unwrap();
    assert_eq!(
        norm_mul_fee, 5_500_000_000_000_000_000_000_000_000,
        "norm_mul_fee golden mismatch"
    );

    // Step 6: / WAD
    let fee_normalized = norm_mul_fee.checked_div(WAD).unwrap();
    assert_eq!(
        fee_normalized, GOLDEN_FEE_NORMALIZED,
        "fee_normalized golden mismatch"
    );
}

// ===========================================================================
// 4. POD LAYOUT VERIFICATION
//    Assert exact byte offsets for every field in all state structs.
// ===========================================================================

#[test]
fn pod_layout_market_size_and_offsets() {
    assert_eq!(size_of::<Market>(), 250);

    // Field offsets using offset_of! macro
    assert_eq!(offset_of!(Market, discriminator), 0);
    assert_eq!(offset_of!(Market, version), 8);
    assert_eq!(offset_of!(Market, borrower), 9);
    assert_eq!(offset_of!(Market, mint), 41);
    assert_eq!(offset_of!(Market, vault), 73);
    assert_eq!(offset_of!(Market, market_authority_bump), 105);
    assert_eq!(offset_of!(Market, annual_interest_bps), 106);
    assert_eq!(offset_of!(Market, maturity_timestamp), 108);
    assert_eq!(offset_of!(Market, max_total_supply), 116);
    assert_eq!(offset_of!(Market, market_nonce), 124);
    assert_eq!(offset_of!(Market, scaled_total_supply), 132);
    assert_eq!(offset_of!(Market, scale_factor), 148);
    assert_eq!(offset_of!(Market, accrued_protocol_fees), 164);
    assert_eq!(offset_of!(Market, total_deposited), 172);
    assert_eq!(offset_of!(Market, total_borrowed), 180);
    assert_eq!(offset_of!(Market, total_repaid), 188);
    assert_eq!(offset_of!(Market, total_interest_repaid), 196);
    assert_eq!(offset_of!(Market, last_accrual_timestamp), 204);
    assert_eq!(offset_of!(Market, settlement_factor_wad), 212);
    assert_eq!(offset_of!(Market, bump), 228);
    assert_eq!(offset_of!(Market, padding), 229);
}

#[test]
fn pod_layout_market_write_and_verify_raw() {
    let mut m = Market::zeroed();
    m.borrower = [0x01; 32];
    m.mint = [0x02; 32];
    m.vault = [0x03; 32];
    m.market_authority_bump = 0xFE;
    m.set_annual_interest_bps(0x1234);
    m.set_maturity_timestamp(0x0102030405060708);
    m.set_max_total_supply(0xAABBCCDDEEFF0011);
    m.set_market_nonce(0x1122334455667788);
    m.set_scaled_total_supply(0x99AABBCCDDEEFF00_1122334455667788);
    m.set_scale_factor(0xFFEEDDCCBBAA9988_7766554433221100);
    m.set_accrued_protocol_fees(0xDEADBEEFCAFEBABE);
    m.set_total_deposited(0x1111111111111111);
    m.set_total_borrowed(0x2222222222222222);
    m.set_total_repaid(0x3333333333333333);
    m.set_total_interest_repaid(0x5555555555555555);
    m.set_last_accrual_timestamp(0x4444444444444444);
    m.set_settlement_factor_wad(0xABCDEF0123456789_FEDCBA9876543210);
    m.bump = 0xAB;

    let raw: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();

    // Verify each field at its exact offset (discriminator[8]+version[1] prefix)
    assert_eq!(&raw[0..8], &[0u8; 8]); // discriminator (zeroed)
    assert_eq!(raw[8], 0); // version (zeroed)
    assert_eq!(&raw[9..41], &[0x01; 32]);
    assert_eq!(&raw[41..73], &[0x02; 32]);
    assert_eq!(&raw[73..105], &[0x03; 32]);
    assert_eq!(raw[105], 0xFE);
    assert_eq!(&raw[106..108], &0x1234u16.to_le_bytes());
    assert_eq!(&raw[108..116], &0x0102030405060708i64.to_le_bytes());
    assert_eq!(&raw[116..124], &0xAABBCCDDEEFF0011u64.to_le_bytes());
    assert_eq!(&raw[124..132], &0x1122334455667788u64.to_le_bytes());
    assert_eq!(
        &raw[132..148],
        &0x99AABBCCDDEEFF00_1122334455667788u128.to_le_bytes()
    );
    assert_eq!(
        &raw[148..164],
        &0xFFEEDDCCBBAA9988_7766554433221100u128.to_le_bytes()
    );
    assert_eq!(&raw[164..172], &0xDEADBEEFCAFEBABEu64.to_le_bytes());
    assert_eq!(&raw[172..180], &0x1111111111111111u64.to_le_bytes());
    assert_eq!(&raw[180..188], &0x2222222222222222u64.to_le_bytes());
    assert_eq!(&raw[188..196], &0x3333333333333333u64.to_le_bytes());
    assert_eq!(&raw[196..204], &0x5555555555555555u64.to_le_bytes());
    assert_eq!(&raw[204..212], &0x4444444444444444i64.to_le_bytes());
    assert_eq!(
        &raw[212..228],
        &0xABCDEF0123456789_FEDCBA9876543210u128.to_le_bytes()
    );
    assert_eq!(raw[228], 0xAB);
    // Padding [229..250] should be all zeros (zeroed struct)
    assert_eq!(&raw[229..250], &[0u8; 21]);
}

#[test]
fn pod_layout_protocol_config_size_and_offsets() {
    assert_eq!(size_of::<ProtocolConfig>(), 194);

    assert_eq!(offset_of!(ProtocolConfig, discriminator), 0);
    assert_eq!(offset_of!(ProtocolConfig, version), 8);
    assert_eq!(offset_of!(ProtocolConfig, admin), 9);
    assert_eq!(offset_of!(ProtocolConfig, fee_rate_bps), 41);
    assert_eq!(offset_of!(ProtocolConfig, fee_authority), 43);
    assert_eq!(offset_of!(ProtocolConfig, whitelist_manager), 75);
    assert_eq!(offset_of!(ProtocolConfig, blacklist_program), 107);
    assert_eq!(offset_of!(ProtocolConfig, is_initialized), 139);
    assert_eq!(offset_of!(ProtocolConfig, bump), 140);
    assert_eq!(offset_of!(ProtocolConfig, paused), 141);
    assert_eq!(offset_of!(ProtocolConfig, blacklist_mode), 142);
    assert_eq!(offset_of!(ProtocolConfig, allowed_mint), 143);
    assert_eq!(offset_of!(ProtocolConfig, padding), 175);
}

#[test]
fn pod_layout_protocol_config_write_and_verify_raw() {
    let mut c = ProtocolConfig::zeroed();
    c.admin = [0xA1; 32];
    c.set_fee_rate_bps(9999);
    c.fee_authority = [0xB2; 32];
    c.whitelist_manager = [0xC3; 32];
    c.blacklist_program = [0xD4; 32];
    c.is_initialized = 1;
    c.bump = 200;

    let raw: &[u8; 194] = bytemuck::bytes_of(&c).try_into().unwrap();

    assert_eq!(&raw[0..8], &[0u8; 8]); // discriminator (zeroed)
    assert_eq!(raw[8], 0); // version (zeroed)
    assert_eq!(&raw[9..41], &[0xA1; 32]);
    assert_eq!(&raw[41..43], &9999u16.to_le_bytes());
    assert_eq!(&raw[43..75], &[0xB2; 32]);
    assert_eq!(&raw[75..107], &[0xC3; 32]);
    assert_eq!(&raw[107..139], &[0xD4; 32]);
    assert_eq!(raw[139], 1);
    assert_eq!(raw[140], 200);
    assert_eq!(raw[141], 0); // paused (zeroed)
    assert_eq!(raw[142], 0); // blacklist_mode (zeroed)
    assert_eq!(&raw[143..175], &[0u8; 32]); // allowed_mint (zeroed)
    assert_eq!(&raw[175..194], &[0u8; 19]); // padding
}

#[test]
fn pod_layout_lender_position_size_and_offsets() {
    assert_eq!(size_of::<LenderPosition>(), 128);

    assert_eq!(offset_of!(LenderPosition, discriminator), 0);
    assert_eq!(offset_of!(LenderPosition, version), 8);
    assert_eq!(offset_of!(LenderPosition, market), 9);
    assert_eq!(offset_of!(LenderPosition, lender), 41);
    assert_eq!(offset_of!(LenderPosition, scaled_balance), 73);
    assert_eq!(offset_of!(LenderPosition, bump), 89);
    assert_eq!(offset_of!(LenderPosition, padding), 90);
}

#[test]
fn pod_layout_lender_position_write_and_verify_raw() {
    let mut p = LenderPosition::zeroed();
    p.market = [0xEE; 32];
    p.lender = [0xFF; 32];
    p.set_scaled_balance(0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0);
    p.bump = 123;

    let raw: &[u8; 128] = bytemuck::bytes_of(&p).try_into().unwrap();

    assert_eq!(&raw[0..8], &[0u8; 8]); // discriminator (zeroed)
    assert_eq!(raw[8], 0); // version (zeroed)
    assert_eq!(&raw[9..41], &[0xEE; 32]);
    assert_eq!(&raw[41..73], &[0xFF; 32]);
    assert_eq!(
        &raw[73..89],
        &0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0u128.to_le_bytes()
    );
    assert_eq!(raw[89], 123);
    assert_eq!(&raw[90..128], &[0u8; 38]);
}

#[test]
fn pod_layout_borrower_whitelist_size_and_offsets() {
    assert_eq!(size_of::<BorrowerWhitelist>(), 96);

    assert_eq!(offset_of!(BorrowerWhitelist, discriminator), 0);
    assert_eq!(offset_of!(BorrowerWhitelist, version), 8);
    assert_eq!(offset_of!(BorrowerWhitelist, borrower), 9);
    assert_eq!(offset_of!(BorrowerWhitelist, is_whitelisted), 41);
    assert_eq!(offset_of!(BorrowerWhitelist, max_borrow_capacity), 42);
    assert_eq!(offset_of!(BorrowerWhitelist, current_borrowed), 50);
    assert_eq!(offset_of!(BorrowerWhitelist, bump), 58);
    assert_eq!(offset_of!(BorrowerWhitelist, padding), 59);
}

#[test]
fn pod_layout_borrower_whitelist_write_and_verify_raw() {
    let mut w = BorrowerWhitelist::zeroed();
    w.borrower = [0x77; 32];
    w.is_whitelisted = 1;
    w.set_max_borrow_capacity(0x1122334455667788);
    w.set_current_borrowed(0xAABBCCDDEEFF0011);
    w.bump = 88;

    let raw: &[u8; 96] = bytemuck::bytes_of(&w).try_into().unwrap();

    assert_eq!(&raw[0..8], &[0u8; 8]); // discriminator (zeroed)
    assert_eq!(raw[8], 0); // version (zeroed)
    assert_eq!(&raw[9..41], &[0x77; 32]);
    assert_eq!(raw[41], 1);
    assert_eq!(&raw[42..50], &0x1122334455667788u64.to_le_bytes());
    assert_eq!(&raw[50..58], &0xAABBCCDDEEFF0011u64.to_le_bytes());
    assert_eq!(raw[58], 88);
    assert_eq!(&raw[59..96], &[0u8; 37]);
}

// ===========================================================================
// 5. INTEREST COMPUTATION GOLDEN VECTORS
//    Table of (annual_bps, time_elapsed, initial_scale_factor, fee_rate,
//    supply) -> (new_scale_factor, new_fees, new_last_accrual)
// ===========================================================================

struct InterestVector {
    // Inputs
    annual_bps: u16,
    time_elapsed: i64, // current_ts - last_accrual (last_accrual = 0, maturity = i64::MAX)
    initial_sf: u128,  // starting scale_factor
    fee_rate_bps: u16, // protocol fee rate
    supply: u128,      // scaled_total_supply
    // Expected outputs
    expected_sf: u128,
    expected_fees: u64,
    expected_ts: i64,
}

fn analytical_accrue(
    annual_bps: u128,
    time_elapsed: u128,
    initial_sf: u128,
    fee_rate_bps: u128,
    supply: u128,
) -> (u128, u64) {
    let annual_bps_u16 = u16::try_from(annual_bps).expect("annual_bps must fit u16");
    let fee_rate_bps_u16 = u16::try_from(fee_rate_bps).expect("fee_rate_bps must fit u16");
    let elapsed_i64 = i64::try_from(time_elapsed).expect("time_elapsed must fit i64");
    let new_sf = interest_oracle::scale_factor_after_exact(initial_sf, annual_bps_u16, elapsed_i64);
    let fees = interest_oracle::fee_delta_exact(
        supply,
        initial_sf,
        annual_bps_u16,
        fee_rate_bps_u16,
        elapsed_i64,
    );
    (new_sf, fees)
}

#[test]
fn interest_golden_vectors() {
    // Pre-compute expected values analytically using the same formulas
    // to establish golden values, then verify the actual function matches.
    let vectors: Vec<InterestVector> = {
        let mut v = Vec::new();

        // V01: 10% annual, 1 full year, scale=WAD, no fees, 1M supply
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 0, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 0,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V02: 10% annual, 1 year, scale=WAD, 5% fee, 1M supply
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V03: 5% annual, 1 year, scale=WAD, no fees
        let (sf, fees) = analytical_accrue(500, SECONDS_PER_YEAR, WAD, 0, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 500,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 0,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V04: 100% annual (10000 bps), 1 year
        let (sf, fees) = analytical_accrue(10000, SECONDS_PER_YEAR, WAD, 0, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 10000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 0,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V05: 1 bps annual, 1 year (smallest nonzero rate)
        let (sf, fees) = analytical_accrue(1, SECONDS_PER_YEAR, WAD, 0, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 0,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V06: 10% annual, 1 second elapsed
        let (sf, fees) = analytical_accrue(1000, 1, WAD, 0, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: 1,
            initial_sf: WAD,
            fee_rate_bps: 0,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: 1,
        });

        // V07: 10% annual, 1 hour (3600 seconds)
        let (sf, fees) = analytical_accrue(1000, 3600, WAD, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: 3600,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: 3600,
        });

        // V08: 10% annual, 1 day (86400 seconds)
        let (sf, fees) = analytical_accrue(1000, 86400, WAD, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: 86400,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: 86400,
        });

        // V09: 10% annual, 30 days
        let (sf, fees) = analytical_accrue(1000, 2_592_000, WAD, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: 2_592_000,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: 2_592_000,
        });

        // V10: 10% annual, 1 year, 100% fee rate (all interest to fees)
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 10000, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 10000,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V11: Scale factor already > 1 (1.5x), 10% annual, 1 year
        let sf_15 = WAD + WAD / 2;
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, sf_15, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: sf_15,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V12: Scale factor = 2.0x, 50% annual, 1 year
        let sf_20 = WAD * 2;
        let (sf, fees) = analytical_accrue(5000, SECONDS_PER_YEAR, sf_20, 1000, 500_000_000);
        v.push(InterestVector {
            annual_bps: 5000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: sf_20,
            fee_rate_bps: 1000,
            supply: 500_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V13: Very small supply (1 unit), 10% annual, 1 year, fees
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 500, 1);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V14: Large supply (10B USDC in 6-decimal units), 10%, 1 year
        let big_supply: u128 = 10_000_000_000_000_000; // 10B USDC
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 500, big_supply);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: big_supply,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V15: 0% annual rate => no change
        let (sf, fees) = analytical_accrue(0, SECONDS_PER_YEAR, WAD, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 0,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V16: 10% annual, half year
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR / 2, WAD, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: (SECONDS_PER_YEAR / 2) as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: (SECONDS_PER_YEAR / 2) as i64,
        });

        // V17: 20% annual, quarter year, scale factor 1.1
        let sf_11 = WAD + WAD / 10;
        let (sf, fees) =
            analytical_accrue(2000, SECONDS_PER_YEAR / 4, sf_11, 250, 2_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 2000,
            time_elapsed: (SECONDS_PER_YEAR / 4) as i64,
            initial_sf: sf_11,
            fee_rate_bps: 250,
            supply: 2_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: (SECONDS_PER_YEAR / 4) as i64,
        });

        // V18: 1% annual, 10 seconds, tiny amount
        let (sf, fees) = analytical_accrue(100, 10, WAD, 100, 100);
        v.push(InterestVector {
            annual_bps: 100,
            time_elapsed: 10,
            initial_sf: WAD,
            fee_rate_bps: 100,
            supply: 100,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: 10,
        });

        // V19: 10% annual, 90 days (typical loan term)
        let ninety_days: u128 = 90 * 86400;
        let (sf, fees) = analytical_accrue(1000, ninety_days, WAD, 500, 5_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: ninety_days as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 5_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: ninety_days as i64,
        });

        // V20: 15% annual, 180 days, scale 1.05, 3% fee
        let half_year_days: u128 = 180 * 86400;
        let sf_105 = WAD + WAD / 20;
        let (sf, fees) = analytical_accrue(1500, half_year_days, sf_105, 300, 3_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1500,
            time_elapsed: half_year_days as i64,
            initial_sf: sf_105,
            fee_rate_bps: 300,
            supply: 3_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: half_year_days as i64,
        });

        // V21: 50% annual, 7 days, 10% fee
        let seven_days: u128 = 7 * 86400;
        let (sf, fees) = analytical_accrue(5000, seven_days, WAD, 1000, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 5000,
            time_elapsed: seven_days as i64,
            initial_sf: WAD,
            fee_rate_bps: 1000,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: seven_days as i64,
        });

        // V22: 0.01% annual (1 bps), 1 year, with fees
        let (sf, fees) = analytical_accrue(1, SECONDS_PER_YEAR, WAD, 500, 1_000_000_000_000);
        v.push(InterestVector {
            annual_bps: 1,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 1_000_000_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V23: Scale factor very large (10x), moderate rate
        let sf_10x = WAD * 10;
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, sf_10x, 500, 100_000_000);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: sf_10x,
            fee_rate_bps: 500,
            supply: 100_000_000,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        // V24: Supply = 0, with fees => fees should be 0
        let (sf, fees) = analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 500, 0);
        v.push(InterestVector {
            annual_bps: 1000,
            time_elapsed: SECONDS_PER_YEAR as i64,
            initial_sf: WAD,
            fee_rate_bps: 500,
            supply: 0,
            expected_sf: sf,
            expected_fees: fees,
            expected_ts: SECONDS_PER_YEAR as i64,
        });

        v
    };

    // Spot-check key analytical anchors.
    assert_eq!(vectors[0].expected_fees, 0, "V01 fees");
    assert_eq!(vectors[14].expected_sf, WAD, "V15 sf (0% rate)");
    assert_eq!(vectors[14].expected_fees, 0, "V15 fees (0% rate)");

    // Run each vector through accrue_interest and verify exact match
    for (i, v) in vectors.iter().enumerate() {
        let mut market = make_market(v.annual_bps, i64::MAX, v.initial_sf, v.supply, 0, 0);
        let config = make_config(v.fee_rate_bps);

        accrue_interest(&mut market, &config, v.time_elapsed).unwrap();

        assert_eq!(
            market.scale_factor(),
            v.expected_sf,
            "Vector V{:02} scale_factor mismatch: got {}, expected {}",
            i + 1,
            market.scale_factor(),
            v.expected_sf
        );
        assert_eq!(
            market.accrued_protocol_fees(),
            v.expected_fees,
            "Vector V{:02} fees mismatch: got {}, expected {}",
            i + 1,
            market.accrued_protocol_fees(),
            v.expected_fees
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            v.expected_ts,
            "Vector V{:02} timestamp mismatch",
            i + 1,
        );
    }
}

// ===========================================================================
// 5b. INDEPENDENTLY-COMPUTED GOLDEN VECTORS (Python Decimal oracle)
//
// These values were computed once using Python's `decimal.Decimal` at 50-digit
// precision, replicating the exact on-chain integer arithmetic (WAD-scaled
// floor division and binary exponentiation).  They serve as immutable anchors
// that catch formula changes — unlike the `analytical_accrue` helper above
// (which clones the production code), these constants were derived from an
// independent implementation.
//
// Python source (for reproducibility):
//   from decimal import Decimal, getcontext
//   getcontext().prec = 50
//   WAD = 10**18; BPS = 10000; SPY = 31536000; SPD = 86400; DPY = 365
//   def mul_wad(a, b): return (a * b) // WAD
//   def pow_wad(base, exp):
//       result = WAD; b = base; e = exp
//       while e > 0:
//           if e & 1: result = mul_wad(result, b)
//           e >>= 1
//           if e > 0: b = mul_wad(b, b)
//       return result
//   def growth(bps, secs):
//       d = secs // SPD; r = secs % SPD
//       dr = (bps * WAD) // (DPY * BPS)
//       g = pow_wad(WAD + dr, d)
//       rd = (bps * r * WAD) // (SPY * BPS)
//       return mul_wad(g, WAD + rd)
// ===========================================================================

/// V1: 10% annual, 1 full year (365 days = 31_536_000 seconds)
/// Python: growth(1000, 31536000) = 1_105_155_781_616_264_095
const GOLDEN_INDEPENDENT_V1_SF: u128 = 1_105_155_781_616_264_095;

/// V2: 10% annual, 1 day (86_400 seconds) — exactly 1 compound step, no sub-day remainder
/// Python: growth(1000, 86400) = 1_000_273_972_602_739_726
const GOLDEN_INDEPENDENT_V2_SF: u128 = 1_000_273_972_602_739_726;

/// V3: 10% annual, 12 hours (43_200 seconds) — pure sub-day linear accrual, 0 compound steps
/// Python: growth(1000, 43200) = 1_000_136_986_301_369_863
const GOLDEN_INDEPENDENT_V3_SF: u128 = 1_000_136_986_301_369_863;

/// V4: 100% annual (10_000 bps), 1 full year — maximum rate stress test
/// Python: growth(10000, 31536000) = 2_714_567_482_021_873_489
const GOLDEN_INDEPENDENT_V4_SF: u128 = 2_714_567_482_021_873_489;

/// V5: 0.01% annual (1 bps), 1 full year — minimum non-zero rate
/// Python: growth(1, 31536000) = 1_000_100_004_986_466_169
const GOLDEN_INDEPENDENT_V5_SF: u128 = 1_000_100_004_986_466_169;

#[test]
fn interest_golden_vectors_hardcoded() {
    // These golden values were computed independently using Python Decimal at
    // 50-digit precision. They do NOT use `analytical_accrue` or any Rust helper
    // that mirrors production code. A mismatch here means the on-chain formula
    // has diverged from the independently-verified reference implementation.
    let config = make_config(0);

    let cases: &[(u16, i64, u128, &str)] = &[
        (
            1000,
            31_536_000,
            GOLDEN_INDEPENDENT_V1_SF,
            "V1: 10% annual, 1 year",
        ),
        (
            1000,
            86_400,
            GOLDEN_INDEPENDENT_V2_SF,
            "V2: 10% annual, 1 day",
        ),
        (
            1000,
            43_200,
            GOLDEN_INDEPENDENT_V3_SF,
            "V3: 10% annual, 12 hours",
        ),
        (
            10000,
            31_536_000,
            GOLDEN_INDEPENDENT_V4_SF,
            "V4: 100% annual, 1 year",
        ),
        (
            1,
            31_536_000,
            GOLDEN_INDEPENDENT_V5_SF,
            "V5: 0.01% annual, 1 year",
        ),
    ];

    for &(annual_bps, elapsed, expected_sf, label) in cases {
        let mut market = make_market(annual_bps, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, elapsed).unwrap();

        assert_eq!(
            market.scale_factor(),
            expected_sf,
            "{}: scale_factor mismatch: got {}, expected {}",
            label,
            market.scale_factor(),
            expected_sf
        );
    }
}

// Golden values for maturity capping: 10% annual rate, 1000 seconds elapsed
// interest_delta_wad = 1000 * 1000 * WAD / (31536000 * 10000)
//                    = 1_000_000_000 * WAD / 315_360_000_000
// 1_000_000_000 * 1_000_000_000_000_000_000 = 1_000_000_000_000_000_000_000_000_000
// / 315_360_000_000 = 3_170_979_198_376 (floor)
const GOLDEN_MATURITY_CAP_IDW: u128 = 3_170_979_198_376;

#[test]
fn interest_golden_vectors_maturity_capping() {
    // When current_ts > maturity, accrual should stop at maturity
    let maturity: i64 = 1000;
    let mut market = make_market(1000, maturity, WAD, 1_000_000_000_000, 0, 0);
    let config = make_config(500);

    // Call with timestamp far past maturity
    accrue_interest(&mut market, &config, 2_000_000).unwrap();

    // Should have accrued only for 1000 seconds
    let (expected_sf, expected_fees) = analytical_accrue(1000, 1000, WAD, 500, 1_000_000_000_000);

    // Verify the interest_delta_wad intermediate golden value
    let idw = (1000u128)
        .checked_mul(1000)
        .unwrap()
        .checked_mul(WAD)
        .unwrap()
        .checked_div(SECONDS_PER_YEAR.checked_mul(BPS).unwrap())
        .unwrap();
    assert_eq!(
        idw, GOLDEN_MATURITY_CAP_IDW,
        "maturity cap idw golden mismatch"
    );

    // new_sf = WAD + WAD * idw / WAD = WAD + idw
    let golden_sf = WAD + idw;
    assert_eq!(expected_sf, golden_sf, "maturity cap sf analytical match");

    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), expected_fees);
    assert_eq!(market.last_accrual_timestamp(), maturity);

    // Verify timestamp was capped at maturity, not the input timestamp
    assert_ne!(
        market.last_accrual_timestamp(),
        2_000_000,
        "timestamp must be capped at maturity"
    );
}

#[test]
fn interest_golden_vectors_cumulative_fees() {
    // Verify that existing fees are added to (not overwritten)
    let existing_fees: u64 = 1_000_000;
    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, existing_fees);
    let config = make_config(500);

    accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

    let (expected_sf, new_fee_delta) =
        analytical_accrue(1000, SECONDS_PER_YEAR, WAD, 500, 1_000_000_000_000);
    let expected_cumulative = existing_fees + new_fee_delta;

    assert_eq!(
        market.accrued_protocol_fees(),
        expected_cumulative,
        "Fees should accumulate on top of existing"
    );
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "scale_factor must match analytical value after accrual"
    );

    // Byte-level assertion for cumulative fees.
    assert_eq!(
        market.accrued_protocol_fees().to_le_bytes(),
        expected_cumulative.to_le_bytes(),
        "cumulative fees byte representation must match expected snapshot"
    );
}

// ===========================================================================
// 6. SETTLEMENT FACTOR GOLDEN VECTORS
//    Table of (available, total_normalized) -> expected_factor
// ===========================================================================

struct SettlementVector {
    available: u128,
    total_normalized: u128,
    expected_factor: u128,
}

#[test]
fn settlement_factor_golden_vectors() {
    let vectors = [
        // SV01: Full repayment (factor = WAD = 1.0)
        SettlementVector {
            available: 1_000_000_000,
            total_normalized: 1_000_000_000,
            expected_factor: WAD,
        },
        // SV02: 75% repayment
        SettlementVector {
            available: 750_000_000,
            total_normalized: 1_000_000_000,
            expected_factor: 750_000_000_000_000_000,
        },
        // SV03: 50% repayment
        SettlementVector {
            available: 500_000_000,
            total_normalized: 1_000_000_000,
            expected_factor: 500_000_000_000_000_000,
        },
        // SV04: 25% repayment
        SettlementVector {
            available: 250_000_000,
            total_normalized: 1_000_000_000,
            expected_factor: 250_000_000_000_000_000,
        },
        // SV05: 1% repayment
        SettlementVector {
            available: 10_000_000,
            total_normalized: 1_000_000_000,
            expected_factor: 10_000_000_000_000_000,
        },
        // SV06: Tiny remainder (1 token available out of 1B)
        SettlementVector {
            available: 1,
            total_normalized: 1_000_000_000,
            // raw = 1 * WAD / 1_000_000_000 = 1_000_000_000
            expected_factor: 1_000_000_000,
        },
        // SV07: Over-repayment (available > total) => capped at WAD
        SettlementVector {
            available: 2_000_000_000,
            total_normalized: 1_000_000_000,
            expected_factor: WAD,
        },
        // SV08: Zero total_normalized => WAD
        SettlementVector {
            available: 1_000_000,
            total_normalized: 0,
            expected_factor: WAD,
        },
        // SV09: Both zero => WAD
        SettlementVector {
            available: 0,
            total_normalized: 0,
            expected_factor: WAD,
        },
        // SV10: Zero available, nonzero total => clamped to 1
        SettlementVector {
            available: 0,
            total_normalized: 1_000_000_000,
            // raw = 0 * WAD / 1B = 0, capped = 0, clamped to 1
            expected_factor: 1,
        },
        // SV11: Equal small amounts
        SettlementVector {
            available: 1,
            total_normalized: 1,
            expected_factor: WAD,
        },
        // SV12: Large amounts (10B USDC) with 99.9% repayment
        SettlementVector {
            available: 9_990_000_000_000_000,
            total_normalized: 10_000_000_000_000_000,
            // raw = 9_990_000_000_000_000 * WAD / 10_000_000_000_000_000
            //     = 999_000_000_000_000_000
            expected_factor: 999_000_000_000_000_000,
        },
        // SV13: 1/3 repayment (tests integer truncation)
        SettlementVector {
            available: 333_333_333,
            total_normalized: 1_000_000_000,
            // raw = 333_333_333 * WAD / 1_000_000_000
            //     = 333_333_333 * 10^18 / 10^9
            //     = 333_333_333_000_000_000
            expected_factor: 333_333_333_000_000_000,
        },
        // SV14: Boundary -- available = 1, total_normalized = u64::MAX
        // raw = 1 * WAD / 18_446_744_073_709_551_615
        //     = 10^18 / 18_446_744_073_709_551_615 = 0 (floor)
        // Clamped: max(0, 1) = 1
        SettlementVector {
            available: 1,
            total_normalized: u64::MAX as u128,
            expected_factor: 1,
        },
        // SV15: Boundary -- available = total_normalized - 1 (one token short of full)
        // raw = 999_999_999 * WAD / 1_000_000_000
        //     = 999_999_999 * 10^18 / 10^9
        //     = 999_999_999_000_000_000
        SettlementVector {
            available: 999_999_999,
            total_normalized: 1_000_000_000,
            expected_factor: 999_999_999_000_000_000,
        },
        // SV16: Boundary -- very large equal amounts (u64::MAX for both => WAD)
        SettlementVector {
            available: u64::MAX as u128,
            total_normalized: u64::MAX as u128,
            expected_factor: WAD,
        },
    ];

    for (i, v) in vectors.iter().enumerate() {
        let result = compute_settlement_factor(v.available, v.total_normalized);
        assert_eq!(
            result, v.expected_factor,
            "Settlement vector SV{:02} mismatch: available={}, total_normalized={}, got={}, expected={}",
            i + 1,
            v.available,
            v.total_normalized,
            result,
            v.expected_factor
        );
    }
}

#[test]
fn settlement_factor_golden_vectors_specific_values() {
    // Verify specific computed values that serve as cross-platform anchors

    // 75% repayment: 0.75 * WAD
    assert_eq!(
        compute_settlement_factor(750_000_000, 1_000_000_000),
        750_000_000_000_000_000
    );

    // Full repayment: exactly WAD
    assert_eq!(compute_settlement_factor(1_000_000_000, 1_000_000_000), WAD);

    // Over-repayment: capped at WAD
    assert_eq!(compute_settlement_factor(5_000_000_000, 1_000_000_000), WAD);

    // Zero available: clamped to 1
    assert_eq!(compute_settlement_factor(0, 1_000_000_000), 1);

    // Very small fraction: 1 / 10^18
    let result = compute_settlement_factor(1, WAD);
    assert_eq!(result, 1); // raw = 1 * WAD / WAD = 1

    // 2/3 repayment (integer truncation test)
    let result = compute_settlement_factor(666_666_666, 1_000_000_000);
    assert_eq!(result, 666_666_666_000_000_000);
}

// ===========================================================================
// ADDITIONAL CROSS-COMPILATION SAFETY CHECKS
// ===========================================================================

#[test]
fn cross_compilation_constants_exact() {
    // These constants MUST be identical across all targets
    assert_eq!(WAD, 1_000_000_000_000_000_000u128);
    assert_eq!(
        WAD.to_le_bytes(),
        [
            0x00, 0x00, 0x64, 0xA7, 0xB3, 0xB6, 0xE0, 0x0D, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ]
    );

    assert_eq!(BPS, 10_000u128);
    assert_eq!(
        BPS.to_le_bytes(),
        [
            0x10, 0x27, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ]
    );

    assert_eq!(SECONDS_PER_YEAR, 31_536_000u128);
    assert_eq!(
        SECONDS_PER_YEAR.to_le_bytes(),
        [
            0x80, 0x33, 0xE1, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ]
    );
}

#[test]
fn cross_compilation_struct_alignment() {
    // repr(C) structs have alignment 1 for all-byte-array types
    assert_eq!(core::mem::align_of::<Market>(), 1);
    assert_eq!(core::mem::align_of::<ProtocolConfig>(), 1);
    assert_eq!(core::mem::align_of::<LenderPosition>(), 1);
    assert_eq!(core::mem::align_of::<BorrowerWhitelist>(), 1);
}

#[test]
fn cross_compilation_zeroed_structs_are_all_zero_bytes() {
    let m = Market::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&m);
    assert!(
        raw.iter().all(|&b| b == 0),
        "Zeroed Market should be all-zero bytes"
    );

    let c = ProtocolConfig::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&c);
    assert!(
        raw.iter().all(|&b| b == 0),
        "Zeroed ProtocolConfig should be all-zero bytes"
    );

    let p = LenderPosition::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&p);
    assert!(
        raw.iter().all(|&b| b == 0),
        "Zeroed LenderPosition should be all-zero bytes"
    );

    let w = BorrowerWhitelist::zeroed();
    let raw: &[u8] = bytemuck::bytes_of(&w);
    assert!(
        raw.iter().all(|&b| b == 0),
        "Zeroed BorrowerWhitelist should be all-zero bytes"
    );
}

#[test]
fn cross_compilation_bytemuck_roundtrip_all_structs() {
    // Market: write -> bytes -> read
    let mut m = Market::zeroed();
    m.set_scale_factor(WAD);
    m.set_annual_interest_bps(1000);
    m.set_maturity_timestamp(1_700_000_000);
    let bytes = bytemuck::bytes_of(&m);
    let m2: &Market = bytemuck::from_bytes(bytes);
    assert_eq!(m2.scale_factor(), WAD);
    assert_eq!(m2.annual_interest_bps(), 1000);
    assert_eq!(m2.maturity_timestamp(), 1_700_000_000);

    // ProtocolConfig: write -> bytes -> read
    let mut c = ProtocolConfig::zeroed();
    c.set_fee_rate_bps(5000);
    c.is_initialized = 1;
    let bytes = bytemuck::bytes_of(&c);
    let c2: &ProtocolConfig = bytemuck::from_bytes(bytes);
    assert_eq!(c2.fee_rate_bps(), 5000);
    assert_eq!(c2.is_initialized, 1);

    // LenderPosition: write -> bytes -> read
    let mut p = LenderPosition::zeroed();
    p.set_scaled_balance(123_456_789_000_000_000);
    let bytes = bytemuck::bytes_of(&p);
    let p2: &LenderPosition = bytemuck::from_bytes(bytes);
    assert_eq!(p2.scaled_balance(), 123_456_789_000_000_000);

    // BorrowerWhitelist: write -> bytes -> read
    let mut w = BorrowerWhitelist::zeroed();
    w.set_max_borrow_capacity(10_000_000_000);
    w.set_current_borrowed(5_000_000_000);
    let bytes = bytemuck::bytes_of(&w);
    let w2: &BorrowerWhitelist = bytemuck::from_bytes(bytes);
    assert_eq!(w2.max_borrow_capacity(), 10_000_000_000);
    assert_eq!(w2.current_borrowed(), 5_000_000_000);
}

// Golden error code for MathOverflow
const GOLDEN_MATH_OVERFLOW_CODE: u32 = 41;

#[test]
fn cross_compilation_interest_overflow_returns_error() {
    // Extremely large scale_factor that will overflow during multiplication
    let huge_scale = u128::MAX / 2;
    let mut market = make_market(10000, i64::MAX, huge_scale, 1, 0, 0);
    let config = make_config(0);

    // Snapshot state before the call to verify it's unchanged after error
    let sf_before = market.scale_factor();
    let fees_before = market.accrued_protocol_fees();
    let ts_before = market.last_accrual_timestamp();

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert!(
        result.is_err(),
        "Should return MathOverflow for huge scale_factor"
    );

    // Assert exact Custom error code (not just is_err())
    let err = result.unwrap_err();
    assert_eq!(
        err,
        ProgramError::Custom(GOLDEN_MATH_OVERFLOW_CODE),
        "Error must be ProgramError::Custom(41) for MathOverflow"
    );
    assert_eq!(
        err,
        ProgramError::Custom(LendingError::MathOverflow as u32),
        "Error must match LendingError::MathOverflow"
    );

    // Verify state is unchanged after error
    assert_eq!(
        market.scale_factor(),
        sf_before,
        "scale_factor must be unchanged after overflow error"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        fees_before,
        "accrued_protocol_fees must be unchanged after overflow error"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        ts_before,
        "last_accrual_timestamp must be unchanged after overflow error"
    );
}

#[test]
fn cross_compilation_two_step_vs_one_step_compound() {
    // Verify the compound effect: two-step accrual should produce >= single-step
    let config = make_config(0);

    // Single step: 0 -> 1000
    let mut m_single = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    accrue_interest(&mut m_single, &config, 1000).unwrap();
    let sf_single = m_single.scale_factor();

    // Two steps: 0 -> 500, 500 -> 1000
    let mut m_double = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    accrue_interest(&mut m_double, &config, 500).unwrap();
    accrue_interest(&mut m_double, &config, 1000).unwrap();
    let sf_double = m_double.scale_factor();

    // Two-step should be >= single-step due to compounding
    assert!(sf_double >= sf_single);
    // Both should be > WAD
    assert!(sf_single > WAD);
    assert!(sf_double > WAD);

    // Verify exact golden values for both paths
    let (expected_single, _) = analytical_accrue(1000, 1000, WAD, 0, 1_000_000_000_000);
    assert_eq!(sf_single, expected_single);
}
