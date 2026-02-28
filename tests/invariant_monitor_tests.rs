//! Integration tests for the CoalesceFi off-chain invariant monitor.
//!
//! These tests exercise the invariant checking logic from `monitoring/src/`
//! by directly constructing on-chain state structs and verifying that every
//! invariant is correctly enforced.
//!
//! Run with:
//! ```sh
//! cargo test --test invariant_monitor_tests
//! ```

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

// We re-declare the types and invariant logic inline because the monitoring
// crate is a separate binary. This file mirrors the monitoring crate's
// public API and validates the exact same logic.

// ---------------------------------------------------------------------------
// Inline type + invariant module (mirrors monitoring/src/)
// ---------------------------------------------------------------------------

/// Constants matching on-chain `src/constants.rs`.
const WAD: u128 = 1_000_000_000_000_000_000;

// We use bytemuck for zero-copy struct construction.
use bytemuck::Zeroable;

// Re-define the state structs to avoid cross-crate binary dependency issues.
// These must be byte-identical to both the on-chain structs and the monitoring
// crate's copies.

#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Market {
    borrower: [u8; 32],
    mint: [u8; 32],
    vault: [u8; 32],
    market_authority_bump: u8,
    annual_interest_bps: [u8; 2],
    maturity_timestamp: [u8; 8],
    max_total_supply: [u8; 8],
    market_nonce: [u8; 8],
    scaled_total_supply: [u8; 16],
    scale_factor: [u8; 16],
    accrued_protocol_fees: [u8; 8],
    total_deposited: [u8; 8],
    total_borrowed: [u8; 8],
    total_repaid: [u8; 8],
    last_accrual_timestamp: [u8; 8],
    settlement_factor_wad: [u8; 16],
    bump: u8,
    _padding: [u8; 38],
}

impl Market {
    fn annual_interest_bps(&self) -> u16 {
        u16::from_le_bytes(self.annual_interest_bps)
    }
    fn set_annual_interest_bps(&mut self, v: u16) {
        self.annual_interest_bps = v.to_le_bytes();
    }
    fn maturity_timestamp(&self) -> i64 {
        i64::from_le_bytes(self.maturity_timestamp)
    }
    fn set_maturity_timestamp(&mut self, v: i64) {
        self.maturity_timestamp = v.to_le_bytes();
    }
    fn max_total_supply(&self) -> u64 {
        u64::from_le_bytes(self.max_total_supply)
    }
    fn set_max_total_supply(&mut self, v: u64) {
        self.max_total_supply = v.to_le_bytes();
    }
    fn scaled_total_supply(&self) -> u128 {
        u128::from_le_bytes(self.scaled_total_supply)
    }
    fn set_scaled_total_supply(&mut self, v: u128) {
        self.scaled_total_supply = v.to_le_bytes();
    }
    fn scale_factor(&self) -> u128 {
        u128::from_le_bytes(self.scale_factor)
    }
    fn set_scale_factor(&mut self, v: u128) {
        self.scale_factor = v.to_le_bytes();
    }
    fn accrued_protocol_fees(&self) -> u64 {
        u64::from_le_bytes(self.accrued_protocol_fees)
    }
    fn set_accrued_protocol_fees(&mut self, v: u64) {
        self.accrued_protocol_fees = v.to_le_bytes();
    }
    fn total_deposited(&self) -> u64 {
        u64::from_le_bytes(self.total_deposited)
    }
    fn set_total_deposited(&mut self, v: u64) {
        self.total_deposited = v.to_le_bytes();
    }
    fn total_borrowed(&self) -> u64 {
        u64::from_le_bytes(self.total_borrowed)
    }
    fn set_total_borrowed(&mut self, v: u64) {
        self.total_borrowed = v.to_le_bytes();
    }
    fn total_repaid(&self) -> u64 {
        u64::from_le_bytes(self.total_repaid)
    }
    fn set_total_repaid(&mut self, v: u64) {
        self.total_repaid = v.to_le_bytes();
    }
    fn last_accrual_timestamp(&self) -> i64 {
        i64::from_le_bytes(self.last_accrual_timestamp)
    }
    fn set_last_accrual_timestamp(&mut self, v: i64) {
        self.last_accrual_timestamp = v.to_le_bytes();
    }
    fn settlement_factor_wad(&self) -> u128 {
        u128::from_le_bytes(self.settlement_factor_wad)
    }
    fn set_settlement_factor_wad(&mut self, v: u128) {
        self.settlement_factor_wad = v.to_le_bytes();
    }

    fn is_initialized(&self) -> bool {
        self.scale_factor() != 0
    }
    fn is_settled(&self) -> bool {
        self.settlement_factor_wad() != 0
    }
    fn has_active_borrows(&self) -> bool {
        self.total_borrowed() > self.total_repaid()
    }
}

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct LenderPosition {
    market: [u8; 32],
    lender: [u8; 32],
    scaled_balance: [u8; 16],
    bump: u8,
    _padding: [u8; 47],
}

impl LenderPosition {
    fn scaled_balance(&self) -> u128 {
        u128::from_le_bytes(self.scaled_balance)
    }
    fn set_scaled_balance(&mut self, v: u128) {
        self.scaled_balance = v.to_le_bytes();
    }
}

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct BorrowerWhitelist {
    borrower: [u8; 32],
    is_whitelisted: u8,
    max_borrow_capacity: [u8; 8],
    current_borrowed: [u8; 8],
    bump: u8,
    _padding: [u8; 46],
}

impl BorrowerWhitelist {
    fn max_borrow_capacity(&self) -> u64 {
        u64::from_le_bytes(self.max_borrow_capacity)
    }
    fn set_max_borrow_capacity(&mut self, v: u64) {
        self.max_borrow_capacity = v.to_le_bytes();
    }
    fn current_borrowed(&self) -> u64 {
        u64::from_le_bytes(self.current_borrowed)
    }
    fn set_current_borrowed(&mut self, v: u64) {
        self.current_borrowed = v.to_le_bytes();
    }
}

// ---------------------------------------------------------------------------
// Inline invariant logic (mirrors monitoring/src/invariants.rs)
// ---------------------------------------------------------------------------

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Critical,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ViolationType {
    VaultInsolvency,
    ScaleFactorBelowWad,
    ScaleFactorDecreased,
    SettlementFactorOutOfBounds,
    SettlementFactorDecreased,
    SuspiciousAccruedFees,
    SupplyCapExceeded,
    LenderBalanceInconsistency,
    WhitelistCapacityExceeded,
    StaleAccrual,
}

#[derive(Debug, Clone)]
struct InvariantViolation {
    market_pubkey: String,
    violation_type: ViolationType,
    expected: String,
    actual: String,
    severity: Severity,
}

#[derive(Default)]
struct MonitorState {
    prev_scale_factors: HashMap<[u8; 32], u128>,
    prev_settlement_factors: HashMap<[u8; 32], u128>,
}

fn check_vault_solvency(pk: &str, m: &Market, vault_bal: u64) -> Result<(), InvariantViolation> {
    if !m.has_active_borrows() && vault_bal < m.accrued_protocol_fees() {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::VaultInsolvency,
            expected: format!("vault_balance >= fees ({})", m.accrued_protocol_fees()),
            actual: format!("vault_balance = {}", vault_bal),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

fn check_scale_factor_validity(pk: &str, m: &Market) -> Result<(), InvariantViolation> {
    if m.is_initialized() && m.scale_factor() < WAD {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::ScaleFactorBelowWad,
            expected: format!("scale_factor >= WAD ({})", WAD),
            actual: format!("scale_factor = {}", m.scale_factor()),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

fn check_scale_factor_monotonicity(
    pk: &str,
    key: &[u8; 32],
    m: &Market,
    state: &mut MonitorState,
) -> Result<(), InvariantViolation> {
    let current = m.scale_factor();
    if let Some(&prev) = state.prev_scale_factors.get(key) {
        if current < prev {
            state.prev_scale_factors.insert(*key, current);
            return Err(InvariantViolation {
                market_pubkey: pk.to_string(),
                violation_type: ViolationType::ScaleFactorDecreased,
                expected: format!("scale_factor >= prev ({})", prev),
                actual: format!("scale_factor = {}", current),
                severity: Severity::Critical,
            });
        }
    }
    state.prev_scale_factors.insert(*key, current);
    Ok(())
}

fn check_settlement_factor_bounds(pk: &str, m: &Market) -> Result<(), InvariantViolation> {
    let sf = m.settlement_factor_wad();
    if sf != 0 && (sf < 1 || sf > WAD) {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::SettlementFactorOutOfBounds,
            expected: format!("1 <= sf <= WAD ({})", WAD),
            actual: format!("sf = {}", sf),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

fn check_settlement_factor_monotonicity(
    pk: &str,
    key: &[u8; 32],
    m: &Market,
    state: &mut MonitorState,
) -> Result<(), InvariantViolation> {
    let current = m.settlement_factor_wad();
    if let Some(&prev) = state.prev_settlement_factors.get(key) {
        if current < prev {
            state.prev_settlement_factors.insert(*key, current);
            return Err(InvariantViolation {
                market_pubkey: pk.to_string(),
                violation_type: ViolationType::SettlementFactorDecreased,
                expected: format!("sf >= prev ({})", prev),
                actual: format!("sf = {}", current),
                severity: Severity::Critical,
            });
        }
    }
    state.prev_settlement_factors.insert(*key, current);
    Ok(())
}

fn check_fee_non_negativity(pk: &str, m: &Market) -> Result<(), InvariantViolation> {
    const SUSPICIOUS_THRESHOLD: u64 = u64::MAX - 1_000_000_000_000;
    if m.accrued_protocol_fees() > SUSPICIOUS_THRESHOLD {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::SuspiciousAccruedFees,
            expected: format!("fees < {}", SUSPICIOUS_THRESHOLD),
            actual: format!("fees = {}", m.accrued_protocol_fees()),
            severity: Severity::Warning,
        });
    }
    Ok(())
}

fn check_supply_cap(pk: &str, m: &Market) -> Result<(), InvariantViolation> {
    if !m.is_initialized() {
        return Ok(());
    }
    let real_supply = m.scaled_total_supply().saturating_mul(m.scale_factor()) / WAD;
    let cap = u128::from(m.max_total_supply());
    if real_supply > cap {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::SupplyCapExceeded,
            expected: format!("real_supply <= cap ({})", cap),
            actual: format!("real_supply = {}", real_supply),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

fn check_lender_balance_consistency(
    pk: &str,
    m: &Market,
    positions: &[LenderPosition],
) -> Result<(), InvariantViolation> {
    let sum: u128 = positions.iter().map(|p| p.scaled_balance()).sum();
    if sum != m.scaled_total_supply() {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::LenderBalanceInconsistency,
            expected: format!("sum == scaled_total_supply ({})", m.scaled_total_supply()),
            actual: format!("sum = {}", sum),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

fn check_whitelist_capacity(pk: &str, wl: &BorrowerWhitelist) -> Result<(), InvariantViolation> {
    if wl.current_borrowed() > wl.max_borrow_capacity() {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::WhitelistCapacityExceeded,
            expected: format!("borrowed <= cap ({})", wl.max_borrow_capacity()),
            actual: format!("borrowed = {}", wl.current_borrowed()),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

fn check_stale_accrual(
    pk: &str,
    m: &Market,
    current_unix: i64,
    threshold: i64,
) -> Result<(), InvariantViolation> {
    if !m.is_initialized() || m.is_settled() {
        return Ok(());
    }
    let last = m.last_accrual_timestamp();
    if last == 0 {
        return Ok(());
    }
    let elapsed = current_unix.saturating_sub(last);
    if elapsed > threshold {
        return Err(InvariantViolation {
            market_pubkey: pk.to_string(),
            violation_type: ViolationType::StaleAccrual,
            expected: format!("elapsed <= {} secs", threshold),
            actual: format!("elapsed = {} secs", elapsed),
            severity: Severity::Warning,
        });
    }
    Ok(())
}

fn check_all_market_invariants(
    pk: &str,
    key: &[u8; 32],
    m: &Market,
    vault_bal: u64,
    positions: &[LenderPosition],
    state: &mut MonitorState,
    current_unix: i64,
    stale_threshold: i64,
) -> Vec<InvariantViolation> {
    let checks: Vec<Result<(), InvariantViolation>> = vec![
        check_vault_solvency(pk, m, vault_bal),
        check_scale_factor_validity(pk, m),
        check_scale_factor_monotonicity(pk, key, m, state),
        check_settlement_factor_bounds(pk, m),
        check_settlement_factor_monotonicity(pk, key, m, state),
        check_fee_non_negativity(pk, m),
        check_supply_cap(pk, m),
        check_lender_balance_consistency(pk, m, positions),
        check_stale_accrual(pk, m, current_unix, stale_threshold),
    ];
    checks.into_iter().filter_map(|r| r.err()).collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a valid, initialized market that passes all invariants.
fn make_valid_market() -> Market {
    let mut m = Market::zeroed();
    m.set_scale_factor(WAD);
    m.set_max_total_supply(10_000_000); // 10M USDC
    m.set_scaled_total_supply(5_000_000); // 5M scaled (= 5M real at WAD)
    m.set_accrued_protocol_fees(1_000);
    m.set_total_deposited(5_000_000);
    m.set_total_borrowed(0);
    m.set_total_repaid(0);
    m.set_maturity_timestamp(2_000_000_000); // far future
    m.set_last_accrual_timestamp(1_700_000_000);
    m.set_settlement_factor_wad(0);
    m.set_annual_interest_bps(500); // 5%
    m
}

/// Create lender positions that sum to the given total.
fn make_positions(market_key: &[u8; 32], balances: &[u128]) -> Vec<LenderPosition> {
    balances
        .iter()
        .enumerate()
        .map(|(i, &bal)| {
            let mut p = LenderPosition::zeroed();
            p.market = *market_key;
            p.lender = [i as u8; 32];
            p.set_scaled_balance(bal);
            p
        })
        .collect()
}

fn assert_violation_details(
    violation: InvariantViolation,
    violation_type: ViolationType,
    severity: Severity,
    expected_contains: &str,
    actual_contains: &str,
) {
    assert_eq!(violation.violation_type, violation_type);
    assert_eq!(violation.severity, severity);
    assert!(
        violation.expected.contains(expected_contains),
        "expected field '{}' does not contain '{}'",
        violation.expected,
        expected_contains
    );
    assert!(
        violation.actual.contains(actual_contains),
        "actual field '{}' does not contain '{}'",
        violation.actual,
        actual_contains
    );
}

// ===========================================================================
// 1. Valid states pass all invariants
// ===========================================================================

#[test]
fn valid_market_passes_all_invariants() {
    let m = make_valid_market();
    let key = [1u8; 32];
    let mut state = MonitorState::default();
    let positions = make_positions(&key, &[5_000_000]);

    let violations = check_all_market_invariants(
        "test",
        &key,
        &m,
        10_000_000,
        &positions,
        &mut state,
        1_700_000_100,
        3600,
    );
    assert!(
        violations.is_empty(),
        "expected no violations, got: {:?}",
        violations
    );
    assert_eq!(state.prev_scale_factors.get(&key), Some(&WAD));
    assert_eq!(state.prev_settlement_factors.get(&key), Some(&0));

    let second_pass = check_all_market_invariants(
        "test",
        &key,
        &m,
        10_000_000,
        &positions,
        &mut state,
        1_700_000_100,
        3600,
    );
    assert!(
        second_pass.is_empty(),
        "second pass should be stable, got: {:?}",
        second_pass
    );
}

#[test]
fn valid_market_with_multiple_lenders_passes() {
    let mut m = make_valid_market();
    m.set_scaled_total_supply(1_000_000);
    let key = [2u8; 32];
    let mut state = MonitorState::default();
    let positions = make_positions(&key, &[300_000, 200_000, 500_000]);

    let violations = check_all_market_invariants(
        "test",
        &key,
        &m,
        10_000_000,
        &positions,
        &mut state,
        1_700_000_100,
        3600,
    );
    assert!(
        violations.is_empty(),
        "expected no violations, got: {:?}",
        violations
    );
    let sum_positions: u128 = positions.iter().map(|p| p.scaled_balance()).sum();
    assert_eq!(sum_positions, m.scaled_total_supply());
    assert_eq!(state.prev_scale_factors.get(&key), Some(&WAD));
}

#[test]
fn valid_whitelist_passes() {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.set_max_borrow_capacity(5_000_000);
    wl.set_current_borrowed(2_000_000);
    assert!(check_whitelist_capacity("test", &wl).is_ok());
    wl.set_current_borrowed(5_000_000);
    assert!(check_whitelist_capacity("test", &wl).is_ok());
}

// ===========================================================================
// 2. Each violation type is detected
// ===========================================================================

#[test]
fn detect_vault_insolvency() {
    let mut m = make_valid_market();
    m.set_accrued_protocol_fees(500);
    m.set_total_borrowed(0);
    m.set_total_repaid(0);
    let err = check_vault_solvency("test", &m, 100).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::VaultInsolvency,
        Severity::Critical,
        "vault_balance >= fees",
        "vault_balance = 100",
    );

    assert!(check_vault_solvency("test", &m, 500).is_ok());
}

#[test]
fn detect_scale_factor_below_wad() {
    let mut m = Market::zeroed();
    m.set_scale_factor(WAD / 2); // Below WAD but non-zero => initialized
    let err = check_scale_factor_validity("test", &m).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::ScaleFactorBelowWad,
        Severity::Critical,
        "scale_factor >= WAD",
        "scale_factor = 500000000000000000",
    );

    m.set_scale_factor(WAD);
    assert!(check_scale_factor_validity("test", &m).is_ok());
}

#[test]
fn detect_scale_factor_decreased() {
    let key = [3u8; 32];
    let mut state = MonitorState::default();
    let mut m = make_valid_market();
    m.set_scale_factor(WAD + 1000);
    assert!(check_scale_factor_monotonicity("test", &key, &m, &mut state).is_ok());

    m.set_scale_factor(WAD + 500);
    let err = check_scale_factor_monotonicity("test", &key, &m, &mut state).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::ScaleFactorDecreased,
        Severity::Critical,
        "scale_factor >= prev",
        "scale_factor = 1000000000000000500",
    );
    assert_eq!(state.prev_scale_factors.get(&key), Some(&(WAD + 500)));
}

#[test]
fn detect_settlement_factor_out_of_bounds_above_wad() {
    let mut m = make_valid_market();
    m.set_settlement_factor_wad(WAD + 1);
    let err = check_settlement_factor_bounds("test", &m).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::SettlementFactorOutOfBounds,
        Severity::Critical,
        "1 <= sf <= WAD",
        "sf = 1000000000000000001",
    );

    m.set_settlement_factor_wad(WAD);
    assert!(check_settlement_factor_bounds("test", &m).is_ok());
}

#[test]
fn detect_settlement_factor_decreased() {
    let key = [4u8; 32];
    let mut state = MonitorState::default();
    let mut m = make_valid_market();
    m.set_settlement_factor_wad(WAD);
    assert!(check_settlement_factor_monotonicity("test", &key, &m, &mut state).is_ok());

    m.set_settlement_factor_wad(WAD / 2);
    let err = check_settlement_factor_monotonicity("test", &key, &m, &mut state).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::SettlementFactorDecreased,
        Severity::Critical,
        "sf >= prev",
        "sf = 500000000000000000",
    );
    assert_eq!(state.prev_settlement_factors.get(&key), Some(&(WAD / 2)));
}

#[test]
fn detect_suspicious_fees() {
    let mut m = make_valid_market();
    m.set_accrued_protocol_fees(u64::MAX);
    let err = check_fee_non_negativity("test", &m).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::SuspiciousAccruedFees,
        Severity::Warning,
        "fees <",
        "fees = 18446744073709551615",
    );

    const SUSPICIOUS_THRESHOLD: u64 = u64::MAX - 1_000_000_000_000;
    m.set_accrued_protocol_fees(SUSPICIOUS_THRESHOLD);
    assert!(check_fee_non_negativity("test", &m).is_ok());
}

#[test]
fn detect_supply_cap_exceeded() {
    let mut m = make_valid_market();
    m.set_max_total_supply(10_000_000);
    m.set_scaled_total_supply(20_000_000); // 20M > 10M cap
    let err = check_supply_cap("test", &m).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::SupplyCapExceeded,
        Severity::Critical,
        "real_supply <= cap",
        "real_supply = 20000000",
    );

    m.set_scaled_total_supply(10_000_000);
    assert!(check_supply_cap("test", &m).is_ok());
}

#[test]
fn detect_lender_balance_inconsistency() {
    let mut m = make_valid_market();
    m.set_scaled_total_supply(1_000);
    // Positions sum to 500, not 1000.
    let key = [5u8; 32];
    let positions = make_positions(&key, &[300, 200]);
    let err = check_lender_balance_consistency("test", &m, &positions).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::LenderBalanceInconsistency,
        Severity::Critical,
        "sum == scaled_total_supply",
        "sum = 500",
    );
}

#[test]
fn detect_whitelist_capacity_exceeded() {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.set_max_borrow_capacity(1_000_000);
    wl.set_current_borrowed(1_000_001);
    let err = check_whitelist_capacity("test", &wl).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::WhitelistCapacityExceeded,
        Severity::Critical,
        "borrowed <= cap",
        "borrowed = 1000001",
    );
}

#[test]
fn detect_stale_accrual() {
    let mut m = make_valid_market();
    m.set_last_accrual_timestamp(1_000);
    let err = check_stale_accrual("test", &m, 5_000, 3600).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::StaleAccrual,
        Severity::Warning,
        "elapsed <= 3600 secs",
        "elapsed = 4000 secs",
    );
    assert!(check_stale_accrual("test", &m, 4_600, 3600).is_ok());
}

// ===========================================================================
// 3. Edge cases
// ===========================================================================

#[test]
fn zeroed_market_not_initialized_passes_all() {
    let m = Market::zeroed();
    let key = [10u8; 32];
    let mut state = MonitorState::default();

    let violations = check_all_market_invariants("test", &key, &m, 0, &[], &mut state, 0, 3600);
    assert!(
        violations.is_empty(),
        "zeroed market should pass: {:?}",
        violations
    );
    assert_eq!(state.prev_scale_factors.get(&key), Some(&0));
    assert_eq!(state.prev_settlement_factors.get(&key), Some(&0));
}

#[test]
fn market_at_maturity_still_checked() {
    let mut m = make_valid_market();
    m.set_maturity_timestamp(1_700_000_000); // maturity in the "past" relative to current_unix
    m.set_last_accrual_timestamp(1_800_000_000); // keep accrual fresh relative to current_unix
    let key = [11u8; 32];
    let mut state = MonitorState::default();
    let positions = make_positions(&key, &[5_000_000]);

    let violations = check_all_market_invariants(
        "test",
        &key,
        &m,
        10_000_000,
        &positions,
        &mut state,
        1_800_000_100,
        3600,
    );
    // Should pass since all fields are still valid.
    assert!(
        violations.is_empty(),
        "matured market should pass: {:?}",
        violations
    );

    m.set_scaled_total_supply(10_000_001);
    let violations_bad = check_all_market_invariants(
        "test",
        &key,
        &m,
        10_000_000,
        &positions,
        &mut state,
        1_800_000_100,
        3600,
    );
    assert!(
        violations_bad
            .iter()
            .any(|v| v.violation_type == ViolationType::SupplyCapExceeded),
        "matured market should still enforce supply-cap invariant"
    );
}

#[test]
fn market_post_settlement_stale_accrual_skipped() {
    let mut m = make_valid_market();
    m.set_settlement_factor_wad(WAD);
    m.set_last_accrual_timestamp(1_000); // very old
                                         // Stale accrual should be skipped for settled markets.
    assert!(check_stale_accrual("test", &m, 1_000_000, 3600).is_ok());

    m.set_settlement_factor_wad(0);
    let err = check_stale_accrual("test", &m, 1_000_000, 3600).unwrap_err();
    assert_eq!(err.violation_type, ViolationType::StaleAccrual);
}

#[test]
fn market_post_settlement_passes_all() {
    let mut m = make_valid_market();
    m.set_settlement_factor_wad(WAD / 2); // 50% settlement
    let key = [12u8; 32];
    let mut state = MonitorState::default();
    let positions = make_positions(&key, &[5_000_000]);

    let violations = check_all_market_invariants(
        "test",
        &key,
        &m,
        10_000_000,
        &positions,
        &mut state,
        1_700_000_100,
        3600,
    );
    assert!(
        violations.is_empty(),
        "settled market should pass: {:?}",
        violations
    );
    assert_eq!(state.prev_settlement_factors.get(&key), Some(&(WAD / 2)));
}

#[test]
fn vault_solvency_not_checked_with_active_borrows() {
    let mut m = make_valid_market();
    m.set_total_borrowed(1_000_000);
    m.set_total_repaid(500_000);
    m.set_accrued_protocol_fees(999_999);
    // vault_balance = 0, but borrows are active so no solvency violation.
    assert!(check_vault_solvency("test", &m, 0).is_ok());

    m.set_total_repaid(1_000_000);
    let err = check_vault_solvency("test", &m, 0).unwrap_err();
    assert_eq!(err.violation_type, ViolationType::VaultInsolvency);
}

#[test]
fn settlement_factor_zero_is_unsettled_and_passes() {
    let m = make_valid_market(); // settlement_factor_wad = 0
    assert!(check_settlement_factor_bounds("test", &m).is_ok());
    assert!(!m.is_settled());
}

#[test]
fn settlement_factor_exactly_wad_passes() {
    let mut m = make_valid_market();
    m.set_settlement_factor_wad(WAD);
    assert!(check_settlement_factor_bounds("test", &m).is_ok());
    m.set_settlement_factor_wad(WAD + 1);
    assert!(check_settlement_factor_bounds("test", &m).is_err());
}

#[test]
fn settlement_factor_exactly_one_passes() {
    let mut m = make_valid_market();
    m.set_settlement_factor_wad(1);
    assert!(check_settlement_factor_bounds("test", &m).is_ok());
    m.set_settlement_factor_wad(2);
    assert!(check_settlement_factor_bounds("test", &m).is_ok());
}

#[test]
fn scale_factor_exactly_wad_passes() {
    let m = make_valid_market(); // scale_factor = WAD
    assert!(check_scale_factor_validity("test", &m).is_ok());

    let mut below = m;
    below.set_scale_factor(WAD - 1);
    assert!(check_scale_factor_validity("test", &below).is_err());
}

#[test]
fn scale_factor_well_above_wad_passes() {
    let mut m = make_valid_market();
    m.set_scale_factor(WAD * 2);
    assert!(check_scale_factor_validity("test", &m).is_ok());
    m.set_scale_factor(u128::MAX);
    assert!(check_scale_factor_validity("test", &m).is_ok());
}

#[test]
fn supply_cap_exactly_at_limit_passes() {
    for (supply, should_fail) in [
        (4_999_999u128, false),
        (5_000_000u128, false),
        (5_000_001u128, true),
    ] {
        let mut m = make_valid_market();
        m.set_max_total_supply(5_000_000);
        m.set_scaled_total_supply(supply);
        assert_eq!(check_supply_cap("test", &m).is_err(), should_fail);
    }
}

#[test]
fn supply_cap_one_over_fails() {
    let mut m = make_valid_market();
    m.set_max_total_supply(5_000_000);
    m.set_scaled_total_supply(5_000_001);
    let err = check_supply_cap("test", &m).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::SupplyCapExceeded,
        Severity::Critical,
        "real_supply <= cap",
        "real_supply = 5000001",
    );
}

#[test]
fn lender_balance_empty_market_zero_positions_passes() {
    let mut m = make_valid_market();
    m.set_scaled_total_supply(0);
    assert!(check_lender_balance_consistency("test", &m, &[]).is_ok());
    let key = [13u8; 32];
    let positions = make_positions(&key, &[0]);
    assert!(check_lender_balance_consistency("test", &m, &positions).is_ok());
}

#[test]
fn whitelist_at_exactly_capacity_passes() {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.set_max_borrow_capacity(1_000_000);
    wl.set_current_borrowed(1_000_000);
    assert!(check_whitelist_capacity("test", &wl).is_ok());
    wl.set_current_borrowed(1_000_001);
    assert!(check_whitelist_capacity("test", &wl).is_err());
}

#[test]
fn whitelist_zero_capacity_zero_borrowed_passes() {
    let mut wl = BorrowerWhitelist::zeroed();
    assert!(check_whitelist_capacity("test", &wl).is_ok());
    wl.set_current_borrowed(1);
    let err = check_whitelist_capacity("test", &wl).unwrap_err();
    assert_eq!(err.violation_type, ViolationType::WhitelistCapacityExceeded);
}

// ===========================================================================
// 4. Lender balance consistency with multiple lenders
// ===========================================================================

#[test]
fn lender_balance_single_lender_exact_match() {
    let mut m = make_valid_market();
    m.set_scaled_total_supply(42_000);
    let key = [20u8; 32];
    let positions = make_positions(&key, &[42_000]);
    assert!(check_lender_balance_consistency("test", &m, &positions).is_ok());
    let sum: u128 = positions.iter().map(|p| p.scaled_balance()).sum();
    assert_eq!(sum, m.scaled_total_supply());
}

#[test]
fn lender_balance_three_lenders_exact_match() {
    let mut m = make_valid_market();
    m.set_scaled_total_supply(600);
    let key = [21u8; 32];
    let positions = make_positions(&key, &[100, 200, 300]);
    assert!(check_lender_balance_consistency("test", &m, &positions).is_ok());
    let sum: u128 = positions.iter().map(|p| p.scaled_balance()).sum();
    assert_eq!(sum, 600);
}

#[test]
fn lender_balance_three_lenders_sum_too_low() {
    let mut m = make_valid_market();
    m.set_scaled_total_supply(700);
    let key = [22u8; 32];
    let positions = make_positions(&key, &[100, 200, 300]); // sum=600 != 700
    let err = check_lender_balance_consistency("test", &m, &positions).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::LenderBalanceInconsistency,
        Severity::Critical,
        "sum == scaled_total_supply",
        "sum = 600",
    );
}

#[test]
fn lender_balance_three_lenders_sum_too_high() {
    let mut m = make_valid_market();
    m.set_scaled_total_supply(500);
    let key = [23u8; 32];
    let positions = make_positions(&key, &[100, 200, 300]); // sum=600 != 500
    let err = check_lender_balance_consistency("test", &m, &positions).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::LenderBalanceInconsistency,
        Severity::Critical,
        "sum == scaled_total_supply",
        "sum = 600",
    );
}

#[test]
fn lender_balance_many_small_positions() {
    let mut m = make_valid_market();
    let balances: Vec<u128> = (1..=100).map(|i| i as u128).collect();
    let sum: u128 = balances.iter().sum(); // 5050
    m.set_scaled_total_supply(sum);
    let key = [24u8; 32];
    let positions = make_positions(&key, &balances);
    assert!(check_lender_balance_consistency("test", &m, &positions).is_ok());
    assert_eq!(positions.len(), 100);
}

// ===========================================================================
// 5. Stale accrual detection at various thresholds
// ===========================================================================

#[test]
fn stale_accrual_just_below_threshold_passes() {
    let mut m = make_valid_market();
    m.set_last_accrual_timestamp(1_000);
    assert!(check_stale_accrual("test", &m, 4_599, 3600).is_ok());
    assert!(check_stale_accrual("test", &m, 4_600, 3600).is_ok());
}

#[test]
fn stale_accrual_exactly_at_threshold_passes() {
    let mut m = make_valid_market();
    m.set_last_accrual_timestamp(1_000);
    // elapsed = 3600, threshold = 3600 => not stale (> not >=).
    assert!(check_stale_accrual("test", &m, 4_600, 3600).is_ok());
    let err = check_stale_accrual("test", &m, 4_601, 3600).unwrap_err();
    assert_eq!(err.violation_type, ViolationType::StaleAccrual);
}

#[test]
fn stale_accrual_one_second_over_threshold_fails() {
    let mut m = make_valid_market();
    m.set_last_accrual_timestamp(1_000);
    let err = check_stale_accrual("test", &m, 4_601, 3600).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::StaleAccrual,
        Severity::Warning,
        "elapsed <= 3600 secs",
        "elapsed = 3601 secs",
    );
}

#[test]
fn stale_accrual_custom_short_threshold() {
    let mut m = make_valid_market();
    m.set_last_accrual_timestamp(100);
    // threshold = 60 seconds, elapsed = 61.
    let err = check_stale_accrual("test", &m, 161, 60).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::StaleAccrual,
        Severity::Warning,
        "elapsed <= 60 secs",
        "elapsed = 61 secs",
    );
    assert!(check_stale_accrual("test", &m, 160, 60).is_ok());
}

#[test]
fn stale_accrual_custom_long_threshold() {
    let mut m = make_valid_market();
    m.set_last_accrual_timestamp(100);
    // threshold = 86400 (1 day), elapsed = 3600 => passes.
    assert!(check_stale_accrual("test", &m, 3700, 86_400).is_ok());
    assert!(check_stale_accrual("test", &m, 86_501, 86_400).is_err());
}

#[test]
fn stale_accrual_zero_last_timestamp_skipped() {
    let mut m = make_valid_market();
    m.set_last_accrual_timestamp(0);
    // Even with large current_unix, timestamp 0 is skipped.
    assert!(check_stale_accrual("test", &m, 1_000_000, 60).is_ok());
    m.set_last_accrual_timestamp(1);
    assert!(check_stale_accrual("test", &m, 1_000_000, 60).is_err());
}

// ===========================================================================
// 6. Monotonicity tracking across multiple cycles
// ===========================================================================

#[test]
fn scale_factor_monotonicity_multiple_increases_pass() {
    let key = [30u8; 32];
    let mut state = MonitorState::default();
    let mut m = make_valid_market();

    for factor in [WAD, WAD + 100, WAD + 200, WAD + 1000, WAD * 2] {
        m.set_scale_factor(factor);
        assert!(
            check_scale_factor_monotonicity("test", &key, &m, &mut state).is_ok(),
            "should pass at factor {}",
            factor,
        );
    }
    assert_eq!(state.prev_scale_factors.get(&key), Some(&(WAD * 2)));
}

#[test]
fn scale_factor_monotonicity_same_value_passes() {
    let key = [31u8; 32];
    let mut state = MonitorState::default();
    let mut m = make_valid_market();
    m.set_scale_factor(WAD + 500);

    for _ in 0..5 {
        assert!(check_scale_factor_monotonicity("test", &key, &m, &mut state).is_ok());
    }
    assert_eq!(state.prev_scale_factors.get(&key), Some(&(WAD + 500)));
}

#[test]
fn settlement_factor_monotonicity_same_value_passes() {
    let key = [32u8; 32];
    let mut state = MonitorState::default();
    let mut m = make_valid_market();
    m.set_settlement_factor_wad(WAD / 2);

    for _ in 0..5 {
        assert!(check_settlement_factor_monotonicity("test", &key, &m, &mut state).is_ok());
    }
    assert_eq!(state.prev_settlement_factors.get(&key), Some(&(WAD / 2)));
}

#[test]
fn different_markets_have_independent_monotonicity_tracking() {
    let key_a = [40u8; 32];
    let key_b = [41u8; 32];
    let mut state = MonitorState::default();

    let mut m_a = make_valid_market();
    m_a.set_scale_factor(WAD + 1000);
    assert!(check_scale_factor_monotonicity("mkt_a", &key_a, &m_a, &mut state).is_ok());

    let mut m_b = make_valid_market();
    m_b.set_scale_factor(WAD + 500);
    // Market B has a lower scale factor than A, but that's fine -- different market.
    assert!(check_scale_factor_monotonicity("mkt_b", &key_b, &m_b, &mut state).is_ok());

    // Now decrease A's factor -- should fail for A but not affect B.
    m_a.set_scale_factor(WAD + 999);
    let err = check_scale_factor_monotonicity("mkt_a", &key_a, &m_a, &mut state).unwrap_err();
    assert_violation_details(
        err,
        ViolationType::ScaleFactorDecreased,
        Severity::Critical,
        "scale_factor >= prev",
        "scale_factor = 1000000000000000999",
    );

    // B should still be fine at its same value.
    assert!(check_scale_factor_monotonicity("mkt_b", &key_b, &m_b, &mut state).is_ok());
    assert_eq!(state.prev_scale_factors.get(&key_b), Some(&(WAD + 500)));
}

// ===========================================================================
// 7. Aggregate check returns correct number of violations
// ===========================================================================

#[test]
fn aggregate_no_violations_for_valid_state() {
    let m = make_valid_market();
    let key = [50u8; 32];
    let mut state = MonitorState::default();
    let positions = make_positions(&key, &[5_000_000]);
    let vs = check_all_market_invariants(
        "test",
        &key,
        &m,
        10_000_000,
        &positions,
        &mut state,
        1_700_000_100,
        3600,
    );
    assert!(vs.is_empty());
    assert_eq!(state.prev_scale_factors.get(&key), Some(&WAD));
    assert_eq!(state.prev_settlement_factors.get(&key), Some(&0));
}

#[test]
fn aggregate_multiple_violations() {
    let key = [51u8; 32];
    let mut state = MonitorState::default();
    let mut m = Market::zeroed();
    // Initialize with scale factor below WAD.
    m.set_scale_factor(WAD - 1);
    // Supply cap exceeded.
    m.set_scaled_total_supply(2_000_000);
    m.set_max_total_supply(1_000_000);
    // Suspicious fees.
    m.set_accrued_protocol_fees(u64::MAX);

    let vs = check_all_market_invariants("test", &key, &m, 0, &[], &mut state, 0, 3600);
    // Should detect at minimum: ScaleFactorBelowWad, SupplyCapExceeded,
    // SuspiciousAccruedFees, LenderBalanceInconsistency (sum 0 != 2_000_000).
    assert!(vs.len() >= 4, "expected >= 4, got {}: {:?}", vs.len(), vs);

    let types: Vec<_> = vs.iter().map(|v| v.violation_type.clone()).collect();
    assert!(types.contains(&ViolationType::ScaleFactorBelowWad));
    assert!(types.contains(&ViolationType::SupplyCapExceeded));
    assert!(types.contains(&ViolationType::SuspiciousAccruedFees));
    assert!(types.contains(&ViolationType::LenderBalanceInconsistency));
    assert!(
        !types.contains(&ViolationType::StaleAccrual),
        "last_accrual_timestamp=0 should skip stale accrual check"
    );
}

// ===========================================================================
// 8. Struct size assertions (must match on-chain)
// ===========================================================================

#[test]
fn struct_sizes_match_on_chain() {
    let market_size = std::mem::size_of::<Market>();
    let lender_size = std::mem::size_of::<LenderPosition>();
    let whitelist_size = std::mem::size_of::<BorrowerWhitelist>();
    assert_eq!(market_size, 250);
    assert_eq!(lender_size, 128);
    assert_eq!(whitelist_size, 96);
    assert_ne!(market_size, 249);
    assert_ne!(lender_size, 127);
    assert_ne!(whitelist_size, 95);
}

// ===========================================================================
// 9. Property-based tests
// ===========================================================================

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy: generate a valid market state paired with a "current unix"
    /// timestamp that is within the stale-accrual threshold of the market's
    /// `last_accrual_timestamp`.
    fn valid_market_strategy() -> impl Strategy<Value = (Market, i64)> {
        (
            // scaled_total_supply: 0..10M
            0u128..10_000_000,
            // max_total_supply: the cap must be >= real supply
            // We'll set it to scaled_total_supply + some margin.
            0u64..20_000_000,
            // accrued_protocol_fees: small, not suspicious
            0u64..1_000_000_000,
            // total_borrowed, total_repaid (repaid <= borrowed)
            0u64..10_000_000,
            // last_accrual_timestamp: recent
            1_699_000_000i64..1_700_100_000,
            // annual_interest_bps: 0..10000
            0u16..10_001,
            // elapsed since last accrual: 0..3599 (within 1-hour threshold)
            0i64..3600,
        )
            .prop_map(
                |(scaled, raw_cap, fees, borrowed, last_accrual, bps, elapsed)| {
                    let mut m = Market::zeroed();
                    m.set_scale_factor(WAD);
                    m.set_scaled_total_supply(scaled);
                    // Ensure cap >= real supply (which == scaled when scale_factor == WAD).
                    let cap = std::cmp::max(raw_cap, scaled as u64);
                    m.set_max_total_supply(cap);
                    m.set_accrued_protocol_fees(fees);
                    m.set_total_borrowed(borrowed);
                    m.set_total_repaid(borrowed); // no active borrows
                    m.set_maturity_timestamp(2_000_000_000);
                    m.set_last_accrual_timestamp(last_accrual);
                    m.set_annual_interest_bps(bps);
                    let current_unix = last_accrual + elapsed;
                    (m, current_unix)
                },
            )
    }

    proptest! {
        #[test]
        fn valid_random_market_passes_invariants((m, current_unix) in valid_market_strategy()) {
            let key = [99u8; 32];
            let mut state = MonitorState::default();
            let positions = make_positions(&key, &[m.scaled_total_supply()]);

            // Vault balance is generous.
            let vault_bal = m.accrued_protocol_fees().saturating_add(1_000_000);

            let vs = check_all_market_invariants(
                "prop", &key, &m, vault_bal, &positions, &mut state, current_unix, 3600,
            );
            prop_assert!(vs.is_empty(), "violations: {:?}", vs);
            prop_assert_eq!(state.prev_scale_factors.get(&key), Some(&WAD));
            prop_assert_eq!(state.prev_settlement_factors.get(&key), Some(&0));
        }

        #[test]
        fn random_scale_factor_below_wad_detected(
            sf in prop_oneof![Just(1u128), Just(WAD - 1), (1u128..WAD)],
        ) {
            let mut m = Market::zeroed();
            m.set_scale_factor(sf);
            let err = check_scale_factor_validity("prop", &m).unwrap_err();
            prop_assert_eq!(err.violation_type, ViolationType::ScaleFactorBelowWad);
            prop_assert_eq!(err.severity, Severity::Critical);
        }

        #[test]
        fn random_supply_cap_exceeded_detected(
            scaled in prop_oneof![Just(1u128), Just(WAD), Just(u64::MAX as u128), (1u128..u64::MAX as u128)],
        ) {
            let mut m = Market::zeroed();
            m.set_scale_factor(WAD);
            m.set_scaled_total_supply(scaled);
            // Cap is always 0 => any non-zero supply exceeds cap.
            m.set_max_total_supply(0);
            let err = check_supply_cap("prop", &m).unwrap_err();
            prop_assert_eq!(err.violation_type, ViolationType::SupplyCapExceeded);
            prop_assert_eq!(err.severity, Severity::Critical);
        }

        #[test]
        fn random_whitelist_over_capacity_detected(
            cap in prop_oneof![Just(0u64), Just(1u64), Just(u64::MAX / 2 - 1), (0u64..u64::MAX / 2)],
            extra in prop_oneof![Just(1u64), Just(2u64), Just(999_999u64), (1u64..1_000_000)],
        ) {
            let mut wl = BorrowerWhitelist::zeroed();
            wl.set_max_borrow_capacity(cap);
            wl.set_current_borrowed(cap.saturating_add(extra));
            // Only assert violation if total_borrowed actually > cap.
            if wl.current_borrowed() > wl.max_borrow_capacity() {
                let err = check_whitelist_capacity("prop", &wl).unwrap_err();
                prop_assert_eq!(err.violation_type, ViolationType::WhitelistCapacityExceeded);
                prop_assert_eq!(err.severity, Severity::Critical);
            }
        }

        #[test]
        fn random_lender_imbalance_detected(
            total in prop_oneof![Just(1u128), Just(WAD), Just(9_999_999u128), (1u128..10_000_000)],
            delta in prop_oneof![Just(1u128), Just(2u128), Just(999_999u128), (1u128..1_000_000)],
        ) {
            let mut m = Market::zeroed();
            m.set_scale_factor(WAD);
            m.set_scaled_total_supply(total);
            // Sum of positions is total - delta (always less than total since delta >= 1).
            let pos_bal = total.saturating_sub(delta);
            let key = [88u8; 32];
            let positions = make_positions(&key, &[pos_bal]);
            if pos_bal != total {
                let err = check_lender_balance_consistency("prop", &m, &positions).unwrap_err();
                prop_assert_eq!(err.violation_type, ViolationType::LenderBalanceInconsistency);
                prop_assert_eq!(err.severity, Severity::Critical);
            }
        }

        #[test]
        fn random_mutation_scale_factor_decrease_detected(
            initial in prop_oneof![Just(WAD), Just(WAD + 1), Just(WAD * 10 - 1), (WAD..(WAD * 10))],
            decrease in prop_oneof![Just(1u128), Just(WAD - 1), (1u128..WAD)],
        ) {
            let key = [77u8; 32];
            let mut state = MonitorState::default();
            let mut m = Market::zeroed();
            m.set_scale_factor(initial);
            let _ = check_scale_factor_monotonicity("prop", &key, &m, &mut state);

            let decreased = initial.saturating_sub(decrease);
            if decreased < initial {
                m.set_scale_factor(decreased);
                let err = check_scale_factor_monotonicity("prop", &key, &m, &mut state).unwrap_err();
                prop_assert_eq!(err.violation_type, ViolationType::ScaleFactorDecreased);
                prop_assert_eq!(state.prev_scale_factors.get(&key), Some(&decreased));
            }
        }
    }
}
