//! Chaos engineering tests for adversarial validator behavior.
//!
//! These tests simulate conditions that arise from malicious or faulty
//! validators: transaction reordering within a slot, duplicate delivery,
//! partial/corrupted account state, concurrent conflicting operations,
//! extreme state transitions, and recovery after failures.
//!
//! All tests operate at the math/state layer (no BPF runtime) to verify
//! that the protocol's invariants hold under adversarial conditions.

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
use pinocchio::error::ProgramError;
use proptest::prelude::*;

use coalesce::constants::{
    BORROWER_WHITELIST_SIZE, BPS, LENDER_POSITION_SIZE, MARKET_SIZE, MAX_ANNUAL_INTEREST_BPS,
    MAX_FEE_RATE_BPS, PROTOCOL_CONFIG_SIZE, SECONDS_PER_YEAR, WAD,
};
use coalesce::error::LendingError;
use coalesce::logic::interest::{accrue_interest, compute_settlement_factor};
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

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

/// Snapshot of mutable Market fields for comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarketSnapshot {
    scale_factor: u128,
    scaled_total_supply: u128,
    accrued_protocol_fees: u64,
    last_accrual_timestamp: i64,
    total_deposited: u64,
    total_borrowed: u64,
    total_repaid: u64,
    settlement_factor_wad: u128,
    annual_interest_bps: u16,
}

fn snapshot(m: &Market) -> MarketSnapshot {
    MarketSnapshot {
        scale_factor: m.scale_factor(),
        scaled_total_supply: m.scaled_total_supply(),
        accrued_protocol_fees: m.accrued_protocol_fees(),
        last_accrual_timestamp: m.last_accrual_timestamp(),
        total_deposited: m.total_deposited(),
        total_borrowed: m.total_borrowed(),
        total_repaid: m.total_repaid(),
        settlement_factor_wad: m.settlement_factor_wad(),
        annual_interest_bps: m.annual_interest_bps(),
    }
}

fn mul_wad_oracle(a: u128, b: u128) -> Option<u128> {
    math_oracle::mul_wad_checked(a, b)
}

fn pow_wad_oracle(base: u128, exp: u32) -> Option<u128> {
    math_oracle::pow_wad_checked(base, exp)
}

fn oracle_growth_factor_wad(
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
) -> u128 {
    let effective_now = current_ts.min(maturity_timestamp);
    if effective_now <= last_accrual_timestamp {
        return WAD;
    }
    let elapsed = effective_now - last_accrual_timestamp;
    math_oracle::growth_factor_wad_checked(annual_interest_bps, elapsed).unwrap()
}

fn oracle_interest_delta_wad(
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
) -> u128 {
    oracle_growth_factor_wad(
        annual_interest_bps,
        last_accrual_timestamp,
        maturity_timestamp,
        current_ts,
    )
    .saturating_sub(WAD)
}

fn oracle_scale_factor_after_step(
    scale_factor: u128,
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
) -> u128 {
    let growth = oracle_growth_factor_wad(
        annual_interest_bps,
        last_accrual_timestamp,
        maturity_timestamp,
        current_ts,
    );
    mul_wad_oracle(scale_factor, growth).unwrap()
}

fn oracle_fee_delta_normalized(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
    fee_rate_bps: u16,
) -> u64 {
    if fee_rate_bps == 0 {
        return 0;
    }
    let interest_delta_wad = oracle_interest_delta_wad(
        annual_interest_bps,
        last_accrual_timestamp,
        maturity_timestamp,
        current_ts,
    );
    if interest_delta_wad == 0 {
        return 0;
    }
    let fee_delta_wad = interest_delta_wad * u128::from(fee_rate_bps) / BPS;
    // Use pre-accrual scale_factor_before (matches on-chain logic after Finding 10 fix)
    let fee_normalized = scaled_total_supply * scale_factor_before / WAD * fee_delta_wad / WAD;
    u64::try_from(fee_normalized).unwrap()
}

fn oracle_settlement_factor(total_normalized: u128, vault_balance: u64, accrued_fees: u64) -> u128 {
    if total_normalized == 0 {
        return WAD;
    }
    let vault_u128 = u128::from(vault_balance);
    let fees_reserved = vault_u128.min(u128::from(accrued_fees));
    let available = vault_u128.saturating_sub(fees_reserved);
    let raw = available * WAD / total_normalized;
    raw.min(WAD).max(1)
}

fn assert_supply_matches_lenders<const N: usize>(market: &Market, lenders: &[LenderPosition; N]) {
    let sum_scaled: u128 = lenders.iter().map(|l| l.scaled_balance()).sum();
    assert_eq!(
        market.scaled_total_supply(),
        sum_scaled,
        "scaled_total_supply must equal sum of lender positions"
    );
}

fn assert_solvency_invariant(market: &Market, vault: u64) {
    let expected_vault = (market.total_deposited() as u128)
        .checked_sub(market.total_borrowed() as u128)
        .and_then(|v| v.checked_add(market.total_repaid() as u128))
        .unwrap();
    assert_eq!(
        vault as u128, expected_vault,
        "solvency invariant violated: vault {} != deposited-borrowed+repaid {}",
        vault, expected_vault
    );
}

/// Simulate a deposit at the state layer: accrue interest, then compute
/// scaled_amount and update supply/balances. Returns (scaled_amount, success).
fn simulate_deposit(
    market: &mut Market,
    config: &ProtocolConfig,
    lender: &mut LenderPosition,
    amount: u64,
    current_ts: i64,
    vault_balance: &mut u64,
) -> (u128, bool) {
    if amount == 0 {
        return (0, false);
    }
    if accrue_interest(market, config, current_ts).is_err() {
        return (0, false);
    }
    let sf = market.scale_factor();
    if sf == 0 {
        return (0, false);
    }
    let amount_u128 = u128::from(amount);
    let scaled = match amount_u128.checked_mul(WAD).and_then(|v| v.checked_div(sf)) {
        Some(s) if s > 0 => s,
        _ => return (0, false),
    };
    let new_total = match market.scaled_total_supply().checked_add(scaled) {
        Some(t) => t,
        None => return (0, false),
    };
    market.set_scaled_total_supply(new_total);
    market.set_total_deposited(market.total_deposited().saturating_add(amount));
    *vault_balance = vault_balance.saturating_add(amount);
    lender.set_scaled_balance(lender.scaled_balance().saturating_add(scaled));
    (scaled, true)
}

/// Simulate a borrow at the state layer. Returns success.
fn simulate_borrow(
    market: &mut Market,
    config: &ProtocolConfig,
    amount: u64,
    current_ts: i64,
    vault_balance: &mut u64,
) -> bool {
    if amount == 0 {
        return false;
    }
    if accrue_interest(market, config, current_ts).is_err() {
        return false;
    }
    let fees_reserved = (*vault_balance).min(market.accrued_protocol_fees());
    let borrowable = vault_balance.saturating_sub(fees_reserved);
    if amount > borrowable {
        return false;
    }
    market.set_total_borrowed(market.total_borrowed().saturating_add(amount));
    *vault_balance = vault_balance.saturating_sub(amount);
    true
}

/// Simulate a withdraw at the state layer. Returns (payout, success).
fn simulate_withdraw(
    market: &mut Market,
    config: &ProtocolConfig,
    lender: &mut LenderPosition,
    current_ts: i64,
    vault_balance: &mut u64,
) -> (u64, bool) {
    if accrue_interest(market, config, current_ts).is_err() {
        return (0, false);
    }
    let scaled_balance = lender.scaled_balance();
    if scaled_balance == 0 {
        return (0, false);
    }

    // Save original settlement factor so we can restore on failure
    let original_sf = market.settlement_factor_wad();

    // Compute settlement factor if not set
    if market.settlement_factor_wad() == 0 {
        let vault_u128 = u128::from(*vault_balance);
        let fees_u128 = u128::from(market.accrued_protocol_fees());
        let fees_reserved = vault_u128.min(fees_u128);
        let available = vault_u128.saturating_sub(fees_reserved);

        let total_normalized = market
            .scaled_total_supply()
            .checked_mul(market.scale_factor())
            .and_then(|v| v.checked_div(WAD))
            .unwrap_or(0);

        let sf = if total_normalized == 0 {
            WAD
        } else {
            let raw = available
                .checked_mul(WAD)
                .and_then(|v| v.checked_div(total_normalized))
                .unwrap_or(0);
            raw.min(WAD).max(1)
        };
        market.set_settlement_factor_wad(sf);
    }

    let normalized = scaled_balance
        .checked_mul(market.scale_factor())
        .and_then(|v| v.checked_div(WAD))
        .unwrap_or(0);
    let payout_u128 = normalized
        .checked_mul(market.settlement_factor_wad())
        .and_then(|v| v.checked_div(WAD))
        .unwrap_or(0);
    let payout = match u64::try_from(payout_u128) {
        Ok(p) => p,
        Err(_) => {
            market.set_settlement_factor_wad(original_sf);
            return (0, false);
        },
    };
    if payout == 0 || payout > *vault_balance {
        market.set_settlement_factor_wad(original_sf);
        return (0, false);
    }

    lender.set_scaled_balance(0);
    let new_supply = market.scaled_total_supply().saturating_sub(scaled_balance);
    market.set_scaled_total_supply(new_supply);
    *vault_balance = vault_balance.saturating_sub(payout);
    (payout, true)
}

/// Simulate fee collection at the state layer. Returns (collected, success).
fn simulate_collect_fees(
    market: &mut Market,
    config: &ProtocolConfig,
    current_ts: i64,
    vault_balance: &mut u64,
) -> (u64, bool) {
    if accrue_interest(market, config, current_ts).is_err() {
        return (0, false);
    }
    let fees = market.accrued_protocol_fees();
    if fees == 0 {
        return (0, false);
    }
    let mut collectible = fees.min(*vault_balance);
    // COAL-C01: cap fee withdrawal above lender claims when supply > 0
    if market.scaled_total_supply() > 0 {
        let sf = market.scale_factor();
        let total_norm = market.scaled_total_supply()
            .checked_mul(sf).unwrap()
            .checked_div(WAD).unwrap();
        let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
        let safe_max = vault_balance.saturating_sub(lender_claims);
        collectible = collectible.min(safe_max);
    }
    if collectible == 0 {
        return (0, false);
    }
    market.set_accrued_protocol_fees(fees.saturating_sub(collectible));
    *vault_balance = vault_balance.saturating_sub(collectible);
    (collectible, true)
}

// ===========================================================================
// 1. Transaction reordering within a slot (4 tests)
// ===========================================================================

/// 1a. Two deposits at the same timestamp in different orders produce
/// identical final state.
#[test]
fn chaos_reorder_two_deposits_same_timestamp_commutative() {
    let ts = 1_000i64;
    let config = make_config(500);
    let deposit_a = 500_000u64;
    let deposit_b = 300_000u64;

    // Order A then B
    let mut market_ab = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    let mut lender_a_ab = LenderPosition::zeroed();
    let mut lender_b_ab = LenderPosition::zeroed();
    let mut vault_ab = 0u64;
    simulate_deposit(
        &mut market_ab,
        &config,
        &mut lender_a_ab,
        deposit_a,
        ts,
        &mut vault_ab,
    );
    simulate_deposit(
        &mut market_ab,
        &config,
        &mut lender_b_ab,
        deposit_b,
        ts,
        &mut vault_ab,
    );

    // Order B then A
    let mut market_ba = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    let mut lender_a_ba = LenderPosition::zeroed();
    let mut lender_b_ba = LenderPosition::zeroed();
    let mut vault_ba = 0u64;
    simulate_deposit(
        &mut market_ba,
        &config,
        &mut lender_b_ba,
        deposit_b,
        ts,
        &mut vault_ba,
    );
    simulate_deposit(
        &mut market_ba,
        &config,
        &mut lender_a_ba,
        deposit_a,
        ts,
        &mut vault_ba,
    );

    // Market state must be identical regardless of order
    assert_eq!(snapshot(&market_ab), snapshot(&market_ba));
    assert_eq!(vault_ab, vault_ba);

    // Each lender's individual position is the same regardless of order
    assert_eq!(lender_a_ab.scaled_balance(), lender_a_ba.scaled_balance());
    assert_eq!(lender_b_ab.scaled_balance(), lender_b_ba.scaled_balance());
}

/// 1b. Deposit then borrow vs borrow then deposit at same timestamp.
/// These produce different outcomes (deposit-first gives more borrowable),
/// but both must leave state in a valid configuration.
#[test]
fn chaos_reorder_deposit_then_borrow_vs_borrow_then_deposit() {
    let ts = 1_000i64;
    let config = make_config(500);
    let initial_deposit = 1_000_000u64;
    let annual_bps = 1000u16;
    let borrow_amount = 800_000u64;
    let expected_sf = oracle_scale_factor_after_step(WAD, annual_bps, 0, i64::MAX, ts);
    let expected_scaled_deposit = u128::from(initial_deposit) * WAD / expected_sf;

    // Scenario 1: deposit first, then borrow
    let mut market_db = make_market(annual_bps, i64::MAX, WAD, 0, 0, 0);
    let mut lender_db = LenderPosition::zeroed();
    let mut vault_db = 0u64;
    let (scaled_db, deposit_ok_db) = simulate_deposit(
        &mut market_db,
        &config,
        &mut lender_db,
        initial_deposit,
        ts,
        &mut vault_db,
    );
    assert!(deposit_ok_db);
    assert_eq!(scaled_db, expected_scaled_deposit);
    let borrow_ok_db = simulate_borrow(&mut market_db, &config, borrow_amount, ts, &mut vault_db);

    // Scenario 2: try borrow first (from empty vault), then deposit
    let mut market_bd = make_market(annual_bps, i64::MAX, WAD, 0, 0, 0);
    let mut lender_bd = LenderPosition::zeroed();
    let mut vault_bd = 0u64;

    // Separate accrual effects from borrow failure effects.
    accrue_interest(&mut market_bd, &config, ts).unwrap();
    let snap_before_failed_borrow = snapshot(&market_bd);
    let vault_before_failed_borrow = vault_bd;

    let borrow_ok_bd = simulate_borrow(&mut market_bd, &config, 800_000, ts, &mut vault_bd);
    assert!(
        !borrow_ok_bd,
        "borrow from an empty vault should fail even after accrual"
    );
    assert_eq!(
        snapshot(&market_bd),
        snap_before_failed_borrow,
        "failed borrow must not mutate market state when accrual is idempotent"
    );
    assert_eq!(
        vault_bd, vault_before_failed_borrow,
        "failed borrow must not mutate vault"
    );

    let (scaled_bd, deposit_ok_bd) = simulate_deposit(
        &mut market_bd,
        &config,
        &mut lender_bd,
        initial_deposit,
        ts,
        &mut vault_bd,
    );
    assert!(deposit_ok_bd);
    assert_eq!(scaled_bd, expected_scaled_deposit);

    // Deposit-then-borrow should succeed; borrow-from-empty should fail
    assert!(borrow_ok_db, "deposit-then-borrow should succeed");
    assert_eq!(market_db.scale_factor(), expected_sf);
    assert_eq!(market_bd.scale_factor(), expected_sf);
    assert_eq!(market_db.last_accrual_timestamp(), ts);
    assert_eq!(market_bd.last_accrual_timestamp(), ts);
    assert_eq!(market_db.accrued_protocol_fees(), 0);
    assert_eq!(market_bd.accrued_protocol_fees(), 0);
    assert_eq!(lender_db.scaled_balance(), expected_scaled_deposit);
    assert_eq!(lender_bd.scaled_balance(), expected_scaled_deposit);
    assert_eq!(market_db.scaled_total_supply(), expected_scaled_deposit);
    assert_eq!(market_bd.scaled_total_supply(), expected_scaled_deposit);
    assert_eq!(market_db.total_deposited(), initial_deposit);
    assert_eq!(market_bd.total_deposited(), initial_deposit);
    assert_eq!(market_db.total_borrowed(), borrow_amount);
    assert_eq!(market_bd.total_borrowed(), 0);
    assert_eq!(vault_db, initial_deposit - borrow_amount);
    assert_eq!(vault_bd, initial_deposit);
    assert_solvency_invariant(&market_db, vault_db);
    assert_solvency_invariant(&market_bd, vault_bd);
}

/// 1c. Multiple accruals at the same timestamp are idempotent regardless of
/// how many times or in what context they are called.
#[test]
fn chaos_reorder_multiple_accruals_same_timestamp_idempotent() {
    let start = 500i64;
    let target = 1_000i64;
    let config = make_config(500);
    let supply = 1_000_000_000_000u128;
    let annual_bps = 1000u16;
    let fee_bps = 500u16;

    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, start, 0);
    let expected_sf = oracle_scale_factor_after_step(WAD, annual_bps, start, i64::MAX, target);
    let expected_fee = oracle_fee_delta_normalized(
        supply,
        WAD, // pre-accrual scale factor (Finding 10)
        annual_bps,
        start,
        i64::MAX,
        target,
        fee_bps,
    );
    accrue_interest(&mut market, &config, target).unwrap();
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), expected_fee);
    assert_eq!(market.last_accrual_timestamp(), target);
    let snap_first = snapshot(&market);

    // Call accrue_interest 20 more times at the same timestamp
    for _ in 0..20 {
        accrue_interest(&mut market, &config, target).unwrap();
        assert_eq!(
            snapshot(&market),
            snap_first,
            "repeated accrual at same timestamp must be idempotent"
        );
    }

    // Boundary check: one second later must advance exactly once.
    let next = target + 1;
    let expected_sf_next =
        oracle_scale_factor_after_step(expected_sf, annual_bps, target, i64::MAX, next);
    let expected_fee_next = oracle_fee_delta_normalized(
        supply,
        expected_sf, // pre-accrual scale factor for this step (Finding 10)
        annual_bps,
        target,
        i64::MAX,
        next,
        fee_bps,
    );
    accrue_interest(&mut market, &config, next).unwrap();
    assert_eq!(market.scale_factor(), expected_sf_next);
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fee + expected_fee_next
    );
    assert_eq!(market.last_accrual_timestamp(), next);
}

/// 1d. Interleaved operations from different lenders at the same timestamp.
/// All lenders deposit at time T; order should not affect per-lender balances
/// since scale_factor is constant within a timestamp.
#[test]
fn chaos_reorder_interleaved_lenders_same_timestamp() {
    let ts = 5_000i64;
    let config = make_config(500);
    let amounts = [100_000u64, 200_000, 300_000, 400_000];
    let annual_bps = 1000u16;
    let expected_sf = oracle_scale_factor_after_step(WAD, annual_bps, 0, i64::MAX, ts);
    let expected_scaled = amounts.map(|amt| u128::from(amt) * WAD / expected_sf);
    let expected_total_deposited: u64 = amounts.iter().sum();
    let expected_total_supply: u128 = expected_scaled.iter().sum();

    // Forward order: lender 0, 1, 2, 3
    let mut market_fwd = make_market(annual_bps, i64::MAX, WAD, 0, 0, 0);
    let mut lenders_fwd = [LenderPosition::zeroed(); 4];
    let mut vault_fwd = 0u64;
    for (i, &amt) in amounts.iter().enumerate() {
        let (scaled, ok) = simulate_deposit(
            &mut market_fwd,
            &config,
            &mut lenders_fwd[i],
            amt,
            ts,
            &mut vault_fwd,
        );
        assert!(ok);
        assert_eq!(
            scaled, expected_scaled[i],
            "forward order scaled amount mismatch"
        );
    }

    // Reverse order: lender 3, 2, 1, 0
    let mut market_rev = make_market(annual_bps, i64::MAX, WAD, 0, 0, 0);
    let mut lenders_rev = [LenderPosition::zeroed(); 4];
    let mut vault_rev = 0u64;
    for (i, &amt) in amounts.iter().enumerate().rev() {
        let (scaled, ok) = simulate_deposit(
            &mut market_rev,
            &config,
            &mut lenders_rev[i],
            amt,
            ts,
            &mut vault_rev,
        );
        assert!(ok);
        assert_eq!(
            scaled, expected_scaled[i],
            "reverse order scaled amount mismatch"
        );
    }

    // Market-level state must be identical
    assert_eq!(snapshot(&market_fwd), snapshot(&market_rev));
    assert_eq!(vault_fwd, vault_rev);
    assert_eq!(market_fwd.scale_factor(), expected_sf);
    assert_eq!(market_rev.scale_factor(), expected_sf);
    assert_eq!(market_fwd.scaled_total_supply(), expected_total_supply);
    assert_eq!(market_rev.scaled_total_supply(), expected_total_supply);
    assert_eq!(market_fwd.total_deposited(), expected_total_deposited);
    assert_eq!(market_rev.total_deposited(), expected_total_deposited);
    assert_eq!(vault_fwd, expected_total_deposited);
    assert_solvency_invariant(&market_fwd, vault_fwd);
    assert_solvency_invariant(&market_rev, vault_rev);
    assert_supply_matches_lenders(&market_fwd, &lenders_fwd);
    assert_supply_matches_lenders(&market_rev, &lenders_rev);

    // Each lender position must be identical
    for i in 0..4 {
        assert_eq!(
            lenders_fwd[i].scaled_balance(),
            lenders_rev[i].scaled_balance(),
            "lender {} scaled_balance differs between orderings",
            i
        );
        assert_eq!(
            lenders_fwd[i].scaled_balance(),
            expected_scaled[i],
            "lender {} scaled balance mismatch against oracle",
            i
        );
    }
}

// ===========================================================================
// 2. Duplicate transaction delivery (3 tests)
// ===========================================================================

/// 2a. Same deposit executed twice adds to supply (not idempotent).
#[test]
fn chaos_duplicate_deposit_adds_supply() {
    let ts = 1_000i64;
    let config = make_config(0);
    let deposit_amount = 1_000_000u64;

    let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;

    // First deposit
    let (scaled_1, ok_1) = simulate_deposit(
        &mut market,
        &config,
        &mut lender,
        deposit_amount,
        ts,
        &mut vault,
    );
    assert!(ok_1);
    let supply_after_1 = market.scaled_total_supply();
    let balance_after_1 = lender.scaled_balance();

    // Second identical deposit
    let (scaled_2, ok_2) = simulate_deposit(
        &mut market,
        &config,
        &mut lender,
        deposit_amount,
        ts,
        &mut vault,
    );
    assert!(ok_2);

    // Supply and balance should have doubled (since scale_factor is unchanged at same ts)
    assert_eq!(
        scaled_1, scaled_2,
        "same amount at same sf should produce same scaled amount"
    );
    assert_eq!(
        market.scaled_total_supply(),
        supply_after_1 * 2,
        "duplicate deposit should double total supply"
    );
    assert_eq!(
        lender.scaled_balance(),
        balance_after_1 * 2,
        "duplicate deposit should double lender balance"
    );
    assert_eq!(vault, deposit_amount * 2);
}

/// 2b. Same accrual executed twice at the same timestamp is idempotent.
#[test]
fn chaos_duplicate_accrual_idempotent() {
    let start = 0i64;
    let ts = 10_000i64;
    let config = make_config(500);
    let supply = 1_000_000_000_000u128;
    let annual_bps = 1000u16;
    let fee_bps = 500u16;

    let mut market = make_market(annual_bps, i64::MAX, WAD, supply, start, 0);
    let expected_sf = oracle_scale_factor_after_step(WAD, annual_bps, start, i64::MAX, ts);
    let expected_fee = oracle_fee_delta_normalized(
        supply,
        WAD, // pre-accrual scale factor (Finding 10)
        annual_bps,
        start,
        i64::MAX,
        ts,
        fee_bps,
    );
    accrue_interest(&mut market, &config, ts).unwrap();
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), expected_fee);
    assert_eq!(market.last_accrual_timestamp(), ts);
    let snap_after_first = snapshot(&market);

    // Duplicate accrual
    accrue_interest(&mut market, &config, ts).unwrap();
    let snap_after_second = snapshot(&market);

    assert_eq!(
        snap_after_first, snap_after_second,
        "duplicate accrual at same timestamp must produce identical state"
    );

    // Boundary check: a one-second increment should advance once.
    let next = ts + 1;
    let expected_sf_next =
        oracle_scale_factor_after_step(expected_sf, annual_bps, ts, i64::MAX, next);
    let expected_fee_next = oracle_fee_delta_normalized(
        supply,
        expected_sf, // pre-accrual scale factor for this step (Finding 10)
        annual_bps,
        ts,
        i64::MAX,
        next,
        fee_bps,
    );
    accrue_interest(&mut market, &config, next).unwrap();
    assert_eq!(market.scale_factor(), expected_sf_next);
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fee + expected_fee_next
    );
    assert_eq!(market.last_accrual_timestamp(), next);
}

/// 2c. Same borrow executed twice -- second should fail if insufficient funds.
#[test]
fn chaos_duplicate_borrow_second_fails_if_insufficient() {
    let ts = 1_000i64;
    let config = make_config(0);

    let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;

    // Deposit 1M
    simulate_deposit(&mut market, &config, &mut lender, 1_000_000, ts, &mut vault);

    // Borrow 600K -- should succeed
    let ok_1 = simulate_borrow(&mut market, &config, 600_000, ts, &mut vault);
    assert!(ok_1, "first borrow of 600K should succeed");
    assert_eq!(vault, 400_000);

    // Duplicate borrow of 600K -- should fail (only 400K left)
    let ok_2 = simulate_borrow(&mut market, &config, 600_000, ts, &mut vault);
    assert!(!ok_2, "duplicate borrow should fail: insufficient funds");
    assert_eq!(
        vault, 400_000,
        "vault should be unchanged after failed borrow"
    );
}

// ===========================================================================
// 3. Partial / corrupted account state (5 tests)
// ===========================================================================

/// 3a. Market with truncated data (< 250 bytes) -- bytemuck cast fails safely.
#[test]
fn chaos_corrupted_market_truncated_data() {
    macro_rules! assert_truncated_rejected {
        ($ty:ty, $size:expr, $label:expr) => {{
            for size in [0usize, 1usize, $size / 2, $size - 1] {
                let buf = vec![0u8; size];
                assert!(
                    bytemuck::try_from_bytes::<$ty>(&buf).is_err(),
                    "{} cast should reject truncated size {} (expected {})",
                    $label,
                    size,
                    $size
                );
            }
            let exact = vec![0u8; $size];
            assert!(
                bytemuck::try_from_bytes::<$ty>(&exact).is_ok(),
                "{} cast should accept exact size {}",
                $label,
                $size
            );
        }};
    }

    assert_truncated_rejected!(Market, MARKET_SIZE, "Market");
    assert_truncated_rejected!(
        BorrowerWhitelist,
        BORROWER_WHITELIST_SIZE,
        "BorrowerWhitelist"
    );
    assert_truncated_rejected!(LenderPosition, LENDER_POSITION_SIZE, "LenderPosition");
    assert_truncated_rejected!(ProtocolConfig, PROTOCOL_CONFIG_SIZE, "ProtocolConfig");
}

/// 3b. ProtocolConfig with all zeros is handled as uninitialized.
#[test]
fn chaos_corrupted_config_all_zeros_uninitialized() {
    let config = ProtocolConfig::zeroed();

    // All fields should be zero
    assert_eq!(config.fee_rate_bps(), 0);
    assert_eq!(
        config.is_initialized, 0,
        "all-zeros config should be uninitialized"
    );
    assert_eq!(config.admin, [0u8; 32]);
    assert_eq!(config.fee_authority, [0u8; 32]);
    assert_eq!(config.whitelist_manager, [0u8; 32]);
    assert_eq!(config.blacklist_program, [0u8; 32]);

    // Using a zero config with accrue_interest should work (fee_rate_bps=0, no fee accrual)
    let mut market = make_market(1000, i64::MAX, WAD, WAD, 0, 0);
    let result = accrue_interest(&mut market, &config, 1000);
    assert!(
        result.is_ok(),
        "accrue with zeroed config should succeed (0 fee rate)"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "no fees with zero fee_rate_bps"
    );
    assert!(market.scale_factor() > WAD, "interest should still accrue");
}

/// 3c. Market with invalid scale_factor (0) -- arithmetic handles gracefully.
#[test]
fn chaos_corrupted_market_zero_scale_factor() {
    let config = make_config(500);

    // scale_factor = 0 should cause accrue_interest to compute a scale_factor_delta of 0,
    // resulting in new_scale_factor = 0 + 0 = 0 (no division by zero).
    let mut market = make_market(1000, i64::MAX, 0, WAD, 0, 0);
    let result = accrue_interest(&mut market, &config, 1000);
    assert!(
        result.is_ok(),
        "accrue with zero scale_factor should not panic"
    );
    assert_eq!(
        market.scale_factor(),
        0,
        "scale_factor remains 0 when starting from 0"
    );
    assert_eq!(market.last_accrual_timestamp(), 1000);
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "fee accrual should remain zero when scale_factor is zero"
    );

    // Deposits with scale_factor=0 should fail (division by zero in scaling)
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;
    let snap_before_failed_deposit = snapshot(&market);
    let (_, ok) = simulate_deposit(
        &mut market,
        &config,
        &mut lender,
        1_000_000,
        1000,
        &mut vault,
    );
    assert!(!ok, "deposit with zero scale_factor should fail");
    assert_eq!(
        snapshot(&market),
        snap_before_failed_deposit,
        "failed deposit must not mutate market state"
    );
    assert_eq!(
        lender.scaled_balance(),
        0,
        "failed deposit must not mutate lender"
    );
    assert_eq!(vault, 0, "failed deposit must not mutate vault");
}

/// 3d. Market with scale_factor < WAD -- detect sub-unity scaling.
#[test]
fn chaos_corrupted_market_scale_factor_below_wad() {
    let config = make_config(0);
    let sub_wad = WAD / 2; // 0.5 WAD

    // A scale_factor below WAD is abnormal (should only go up from WAD).
    // But the math should still work without panics.
    let mut market = make_market(1000, i64::MAX, sub_wad, 0, 0, 0);
    let expected_sf = oracle_scale_factor_after_step(sub_wad, 1000, 0, i64::MAX, 1000);
    let result = accrue_interest(&mut market, &config, 1000);
    assert!(
        result.is_ok(),
        "accrue with sub-WAD scale_factor should not panic"
    );
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "sub-WAD accrual should follow exact oracle math"
    );

    // Detect: scale_factor remains sub-WAD but increases.
    assert!(
        market.scale_factor() > sub_wad,
        "scale_factor should still increase under positive interest"
    );
    assert!(
        market.scale_factor() < WAD,
        "scale_factor starting below WAD remains below WAD after tiny accrual: {}",
        market.scale_factor()
    );

    // Boundary behavior: deposits still compute deterministically and produce
    // scaled amounts above nominal when scale_factor < WAD.
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;
    let deposit = 1_000u64;
    let (scaled, ok) =
        simulate_deposit(&mut market, &config, &mut lender, deposit, 1000, &mut vault);
    assert!(ok, "deposit should still execute deterministically");
    let expected_scaled = u128::from(deposit) * WAD / expected_sf;
    assert_eq!(scaled, expected_scaled);
    assert!(
        scaled > u128::from(deposit),
        "sub-WAD scaling must inflate scaled units"
    );
    assert_eq!(lender.scaled_balance(), expected_scaled);
    assert_eq!(market.scaled_total_supply(), expected_scaled);
    assert_eq!(vault, deposit);
}

/// 3e. LenderPosition with scaled_balance > market's scaled_total_supply --
/// detect inconsistency.
#[test]
fn chaos_corrupted_lender_exceeds_total_supply() {
    let config = make_config(0);
    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000, 0, 0);
    let mut lender = LenderPosition::zeroed();
    lender.set_scaled_balance(2_000_000); // 2x the total supply
    let mut vault = 1_000_000u64;

    let inconsistent = lender.scaled_balance() > market.scaled_total_supply();
    assert!(
        inconsistent,
        "lender balance ({}) should be detected as exceeding total supply ({})",
        lender.scaled_balance(),
        market.scaled_total_supply()
    );

    // If we tried to withdraw this lender, the payout would be based on their
    // full balance, but the market doesn't have enough supply to cover it.
    // The scaled_total_supply would underflow on subtraction.
    let would_underflow = market
        .scaled_total_supply()
        .checked_sub(lender.scaled_balance())
        .is_none();
    assert!(
        would_underflow,
        "subtracting lender balance from total supply should underflow"
    );

    // Failed withdrawal from an inconsistent state must not mutate data.
    accrue_interest(&mut market, &config, 1_000).unwrap();
    let snap_before = snapshot(&market);
    let lender_before = lender.scaled_balance();
    let vault_before = vault;
    let (payout, ok) = simulate_withdraw(&mut market, &config, &mut lender, 1_000, &mut vault);
    assert!(
        !ok,
        "inconsistent oversized lender balance should not be withdrawable"
    );
    assert_eq!(payout, 0);
    assert_eq!(
        snapshot(&market),
        snap_before,
        "failed withdraw must not mutate market"
    );
    assert_eq!(
        lender.scaled_balance(),
        lender_before,
        "failed withdraw must not mutate lender balance"
    );
    assert_eq!(vault, vault_before, "failed withdraw must not mutate vault");
}

// ===========================================================================
// 4. Concurrent conflicting operations (4 tests)
// ===========================================================================

/// 4a. Two borrows that together exceed vault capacity -- at least one must fail.
#[test]
fn chaos_concurrent_borrows_exceed_capacity() {
    let ts = 1_000i64;
    let config = make_config(0);
    let deposit = 1_000_000u64;
    let borrow_each = 700_000u64; // 700K + 700K = 1.4M > 1M

    // Shared starting state
    let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;
    simulate_deposit(&mut market, &config, &mut lender, deposit, ts, &mut vault);

    // First borrow succeeds
    let ok_1 = simulate_borrow(&mut market, &config, borrow_each, ts, &mut vault);
    assert!(ok_1, "first borrow should succeed");
    assert_eq!(vault, deposit - borrow_each);

    // Second borrow should fail
    let snap_before_failed_borrow = snapshot(&market);
    let vault_before_failed_borrow = vault;
    let ok_2 = simulate_borrow(&mut market, &config, borrow_each, ts, &mut vault);
    assert!(!ok_2, "second borrow should fail: exceeds capacity");
    assert_eq!(
        snapshot(&market),
        snap_before_failed_borrow,
        "failed second borrow must not mutate market"
    );
    assert_eq!(
        vault, vault_before_failed_borrow,
        "failed second borrow must not mutate vault"
    );

    assert_eq!(market.total_deposited(), deposit);
    assert_eq!(market.total_borrowed(), borrow_each);
    assert_eq!(market.total_repaid(), 0);
    assert_supply_matches_lenders(&market, &[lender]);
    assert_solvency_invariant(&market, vault);
}

/// 4b. Deposit and withdraw on same market at same timestamp (post-maturity).
/// Both should succeed independently when there are sufficient funds.
#[test]
fn chaos_concurrent_deposit_and_withdraw_same_timestamp() {
    let maturity = 1_000i64;
    let config = make_config(0);

    let mut market = make_market(0, maturity, WAD, 0, 0, 0);
    let mut lender_a = LenderPosition::zeroed();
    let mut lender_b = LenderPosition::zeroed();
    let mut vault = 0u64;

    // Pre-maturity: lender A deposits 1M at t=100
    simulate_deposit(
        &mut market,
        &config,
        &mut lender_a,
        1_000_000,
        100,
        &mut vault,
    );

    // Lender B deposits at maturity
    let ts_at_maturity = maturity;
    simulate_deposit(
        &mut market,
        &config,
        &mut lender_b,
        500_000,
        ts_at_maturity,
        &mut vault,
    );

    // Post-maturity: lender A withdraws
    let post_maturity = maturity + 1;
    let (payout_a, ok_a) = simulate_withdraw(
        &mut market,
        &config,
        &mut lender_a,
        post_maturity,
        &mut vault,
    );
    assert!(ok_a, "lender A withdrawal should succeed");
    assert!(payout_a > 0, "lender A should receive a payout");

    // Verify remaining state is consistent
    assert_eq!(lender_a.scaled_balance(), 0);
    assert!(
        lender_b.scaled_balance() > 0,
        "lender B still has a position"
    );
    assert_eq!(
        market.scaled_total_supply(),
        lender_b.scaled_balance(),
        "total supply should equal remaining lender B balance"
    );
}

/// 4c. CollectFees and Borrow competing for vault balance -- fee reservation works.
#[test]
fn chaos_concurrent_collect_fees_and_borrow() {
    let annual_bps = 1000u16;
    let fee_bps = 1000u16;
    let config = make_config(fee_bps); // 10% fee rate
    let maturity = 100_000i64;
    let accrual_ts = 50_000i64;
    let deposit_amount = 10_000_000u64;

    // Setup: deposit, accrue interest to generate fees
    let mut market = make_market(annual_bps, maturity, WAD, 0, 0, 0);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;
    let (scaled, deposit_ok) = simulate_deposit(
        &mut market,
        &config,
        &mut lender,
        deposit_amount,
        0,
        &mut vault,
    );
    assert!(deposit_ok);
    assert_eq!(scaled, u128::from(deposit_amount));

    let expected_sf = oracle_scale_factor_after_step(WAD, annual_bps, 0, maturity, accrual_ts);
    let expected_fee_delta = oracle_fee_delta_normalized(
        market.scaled_total_supply(),
        WAD, // pre-accrual scale factor (Finding 10)
        annual_bps,
        0,
        maturity,
        accrual_ts,
        fee_bps,
    );
    accrue_interest(&mut market, &config, accrual_ts).unwrap();
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(market.accrued_protocol_fees(), expected_fee_delta);
    assert_eq!(market.last_accrual_timestamp(), accrual_ts);

    let fees_reserved = vault.min(market.accrued_protocol_fees());
    let borrowable = vault.saturating_sub(fees_reserved);
    assert!(
        fees_reserved > 0,
        "fees should reserve part of vault liquidity"
    );
    assert!(borrowable < vault, "borrowable must exclude reserved fees");

    // Full-vault borrow must fail and leave state unchanged.
    let snap_before_overborrow = snapshot(&market);
    let vault_before_overborrow = vault;
    let full_borrow_ok = simulate_borrow(
        &mut market,
        &config,
        vault_before_overborrow,
        accrual_ts,
        &mut vault,
    );
    assert!(
        !full_borrow_ok,
        "full-vault borrow should fail due fee reservation"
    );
    assert_eq!(snapshot(&market), snap_before_overborrow);
    assert_eq!(vault, vault_before_overborrow);

    // Borrowing exactly the borrowable amount must succeed.
    let exact_borrow_ok = simulate_borrow(&mut market, &config, borrowable, accrual_ts, &mut vault);
    assert!(
        exact_borrow_ok,
        "borrowing exactly borrowable amount should succeed"
    );
    assert_eq!(market.total_borrowed(), borrowable);
    assert_eq!(market.accrued_protocol_fees(), expected_fee_delta);
    assert_eq!(
        vault, fees_reserved,
        "remaining vault should equal reserved fees"
    );

    // Collecting fees after borrow should drain remaining reserved liquidity.
    let (collected, collect_ok) =
        simulate_collect_fees(&mut market, &config, accrual_ts, &mut vault);
    assert!(collect_ok, "reserved fees should be collectible");
    assert_eq!(collected, fees_reserved);
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fee_delta.saturating_sub(collected)
    );
    assert_eq!(
        vault, 0,
        "collecting reserved fees should drain residual vault"
    );
    assert_supply_matches_lenders(&market, &[lender]);
}

/// 4d. Two lenders withdrawing simultaneously -- proportional fairness.
#[test]
fn chaos_concurrent_two_lenders_withdraw_proportional() {
    let maturity = 10_000i64;
    let config = make_config(0);

    let mut market = make_market(0, maturity, WAD, 0, 0, 0);
    let mut lender_a = LenderPosition::zeroed();
    let mut lender_b = LenderPosition::zeroed();
    let mut vault = 0u64;

    // Lender A deposits 1M, lender B deposits 3M (1:3 ratio).
    let (scaled_a, ok_dep_a) = simulate_deposit(
        &mut market,
        &config,
        &mut lender_a,
        1_000_000,
        1,
        &mut vault,
    );
    let (scaled_b, ok_dep_b) = simulate_deposit(
        &mut market,
        &config,
        &mut lender_b,
        3_000_000,
        1,
        &mut vault,
    );
    assert!(ok_dep_a && ok_dep_b);
    assert_eq!(scaled_a, 1_000_000);
    assert_eq!(scaled_b, 3_000_000);

    // Borrow half and repay half (partial recovery -- 50% settlement)
    let borrow_ok = simulate_borrow(&mut market, &config, 2_000_000, 2, &mut vault);
    assert!(borrow_ok);
    // Repay only 1M of the 2M borrowed.
    let post_mat = maturity + 1;
    market.set_total_repaid(market.total_repaid().saturating_add(1_000_000));
    vault = vault.saturating_add(1_000_000);

    accrue_interest(&mut market, &config, post_mat).unwrap();
    let total_normalized = market
        .scaled_total_supply()
        .checked_mul(market.scale_factor())
        .and_then(|v| v.checked_div(WAD))
        .unwrap();
    let expected_settlement =
        oracle_settlement_factor(total_normalized, vault, market.accrued_protocol_fees());
    assert_eq!(expected_settlement, (3u128 * WAD) / 4u128);

    let normalized_a = lender_a
        .scaled_balance()
        .checked_mul(market.scale_factor())
        .and_then(|v| v.checked_div(WAD))
        .unwrap();
    let normalized_b = lender_b
        .scaled_balance()
        .checked_mul(market.scale_factor())
        .and_then(|v| v.checked_div(WAD))
        .unwrap();
    let expected_payout_a = u64::try_from(normalized_a * expected_settlement / WAD).unwrap();
    let expected_payout_b = u64::try_from(normalized_b * expected_settlement / WAD).unwrap();
    assert_eq!(expected_payout_a, 750_000);
    assert_eq!(expected_payout_b, 2_250_000);

    // COAL-C01: no fee reservation; full vault is available for lenders
    let available_for_lenders = vault;

    // Both lenders withdraw
    let (payout_a, ok_a) =
        simulate_withdraw(&mut market, &config, &mut lender_a, post_mat, &mut vault);
    assert!(ok_a, "lender A should be able to withdraw");
    assert_eq!(payout_a, expected_payout_a);
    assert_eq!(
        market.settlement_factor_wad(),
        expected_settlement,
        "settlement factor should be locked on first withdrawal"
    );

    let (payout_b, ok_b) =
        simulate_withdraw(&mut market, &config, &mut lender_b, post_mat, &mut vault);
    assert!(ok_b, "lender B should be able to withdraw");
    assert_eq!(payout_b, expected_payout_b);
    assert_eq!(payout_b, payout_a * 3, "payouts must be exact 3:1");
    assert_eq!(market.scaled_total_supply(), 0);
    assert_eq!(lender_a.scaled_balance(), 0);
    assert_eq!(lender_b.scaled_balance(), 0);
    assert_eq!(payout_a.saturating_add(payout_b), available_for_lenders);
    assert_eq!(vault, 0, "all lender-available funds should be paid out");
}

// ===========================================================================
// 5. Extreme state transitions (4 tests)
// ===========================================================================

/// 5a. Market goes from zero supply to max supply in one transaction.
#[test]
fn chaos_extreme_zero_to_max_supply() {
    let config = make_config(0);
    let ts = 1_000i64;

    let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    market.set_max_total_supply(u64::MAX);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;

    // Deposit the maximum possible amount
    let max_deposit = u64::MAX / 2; // Leave room to avoid overflow in vault tracking
    let (scaled, ok) = simulate_deposit(
        &mut market,
        &config,
        &mut lender,
        max_deposit,
        ts,
        &mut vault,
    );
    assert!(ok, "max deposit should succeed");
    assert!(scaled > 0, "scaled amount should be positive");
    assert_eq!(vault, max_deposit);
    assert!(market.scale_factor() >= WAD);
}

/// 5b. Market goes from max supply to zero supply (full withdrawal).
#[test]
fn chaos_extreme_max_supply_to_zero() {
    let maturity = 1_000i64;
    let config = make_config(0);
    let deposit = 10_000_000u64;

    let mut market = make_market(0, maturity, WAD, 0, 0, 0);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;

    simulate_deposit(&mut market, &config, &mut lender, deposit, 1, &mut vault);
    assert_eq!(vault, deposit);
    assert!(market.scaled_total_supply() > 0);

    // Full withdrawal post-maturity
    let (payout, ok) =
        simulate_withdraw(&mut market, &config, &mut lender, maturity + 1, &mut vault);
    assert!(ok, "full withdrawal should succeed");
    assert_eq!(payout, deposit, "with 0% interest, payout equals deposit");
    assert_eq!(
        market.scaled_total_supply(),
        0,
        "supply should be zero after full withdrawal"
    );
    assert_eq!(lender.scaled_balance(), 0);
    assert_eq!(vault, 0, "vault should be empty");
}

/// 5c. Scale factor doubles in one accrual (high rate + long time).
#[test]
fn chaos_extreme_scale_factor_doubles() {
    let config = make_config(0);
    // 100% annual rate for 1 year compounds daily and should exceed 2x.
    let one_year = SECONDS_PER_YEAR as i64;
    let maturity = one_year * 2; // enough headroom
    let annual_bps = 10_000u16;

    let mut market_before = make_market(annual_bps, maturity, WAD, WAD, 0, 0);
    accrue_interest(&mut market_before, &config, one_year - 1).unwrap();
    let expected_before =
        oracle_scale_factor_after_step(WAD, annual_bps, 0, maturity, one_year - 1);
    assert_eq!(market_before.scale_factor(), expected_before);

    let mut market_exact = make_market(annual_bps, maturity, WAD, WAD, 0, 0);
    accrue_interest(&mut market_exact, &config, one_year).unwrap();
    let expected_exact = oracle_scale_factor_after_step(WAD, annual_bps, 0, maturity, one_year);
    assert_eq!(market_exact.scale_factor(), expected_exact);
    assert_eq!(
        market_exact.scale_factor(),
        expected_exact,
        "scale_factor must match oracle at 100% annual rate over 1 year"
    );
    assert!(
        market_exact.scale_factor() > WAD * 2,
        "daily compounding at 100% APR over a year should exceed 2x"
    );

    let mut market_after = make_market(annual_bps, maturity, WAD, WAD, 0, 0);
    accrue_interest(&mut market_after, &config, one_year + 1).unwrap();
    let expected_after = oracle_scale_factor_after_step(WAD, annual_bps, 0, maturity, one_year + 1);
    assert_eq!(market_after.scale_factor(), expected_after);

    assert!(
        market_before.scale_factor() < market_exact.scale_factor()
            && market_exact.scale_factor() < market_after.scale_factor(),
        "x-1/x/x+1 boundary around one-year accrual should be strictly increasing"
    );

    // Verify deposit scaling against the exact on-chain denominator.
    let amount: u128 = 1_000_001;
    let scaled = amount * WAD / market_exact.scale_factor();
    let expected_scaled = amount * WAD / expected_exact;
    assert_eq!(
        scaled, expected_scaled,
        "scaled mint amount must match floor division with accrued scale factor"
    );
}

/// 5d. Settlement from full coverage (WAD) to minimum (1) via re_settle
/// with changed conditions.
#[test]
fn chaos_extreme_settlement_wad_to_minimum() {
    let maturity = 10_000i64;

    // Scenario 1: full coverage (settlement_factor = WAD)
    let mut market_full = make_market(0, maturity, WAD, 0, 0, 0);
    let config = make_config(0);
    let mut lender_full = LenderPosition::zeroed();
    let mut vault_full = 0u64;
    simulate_deposit(
        &mut market_full,
        &config,
        &mut lender_full,
        1_000_000,
        1,
        &mut vault_full,
    );

    // All money in vault, no borrow => settlement_factor = WAD.
    accrue_interest(&mut market_full, &config, maturity + 1).unwrap();
    let total_normalized = market_full
        .scaled_total_supply()
        .checked_mul(market_full.scale_factor())
        .and_then(|v| v.checked_div(WAD))
        .unwrap();
    let settlement_full = oracle_settlement_factor(
        total_normalized,
        vault_full,
        market_full.accrued_protocol_fees(),
    );
    let settlement_full_logic =
        compute_settlement_factor(u128::from(vault_full), total_normalized).unwrap();
    assert_eq!(
        settlement_full, WAD,
        "full coverage should yield WAD settlement"
    );
    assert_eq!(
        settlement_full_logic, settlement_full,
        "oracle and production settlement logic must match"
    );

    // Scenario 2: near-total loss (borrowed everything, repaid 1 unit).
    let mut market_min = make_market(0, maturity, WAD, 0, 0, 0);
    let mut lender_min = LenderPosition::zeroed();
    let mut vault_min = 0u64;
    simulate_deposit(
        &mut market_min,
        &config,
        &mut lender_min,
        1_000_000,
        1,
        &mut vault_min,
    );
    simulate_borrow(&mut market_min, &config, 1_000_000, 2, &mut vault_min);
    // Repay only 1 unit
    market_min.set_total_repaid(1);
    vault_min = 1;

    accrue_interest(&mut market_min, &config, maturity + 1).unwrap();
    let total_normalized_min = market_min
        .scaled_total_supply()
        .checked_mul(market_min.scale_factor())
        .and_then(|v| v.checked_div(WAD))
        .unwrap();
    let settlement_min = oracle_settlement_factor(
        total_normalized_min,
        vault_min,
        market_min.accrued_protocol_fees(),
    );
    let settlement_min_logic =
        compute_settlement_factor(u128::from(vault_min), total_normalized_min).unwrap();
    assert!(
        settlement_min < WAD,
        "near-total-loss settlement ({}) should be well below WAD",
        settlement_min
    );
    assert_eq!(settlement_min_logic, settlement_min);
    assert!(
        settlement_min >= 1,
        "settlement factor should be at least 1 (clamped)"
    );

    // Scenario 3: clamp reaches absolute minimum of 1.
    let huge_total = WAD * 2;
    assert_eq!(compute_settlement_factor(0, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(1, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(2, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(3, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(4, huge_total).unwrap(), 2);
}

// ===========================================================================
// 6. State recovery after failures (3 tests)
// ===========================================================================

/// 6a. Failed transaction (overflow) leaves state unchanged -- verify atomicity.
#[test]
fn chaos_recovery_overflow_leaves_state_unchanged() {
    let config = make_config(500);

    // Create a market that will overflow on accrual (huge scale_factor)
    let huge_sf = u128::MAX / 2;
    let mut market = make_market(10_000, i64::MAX, huge_sf, WAD, 0, 0);
    let snap_before = snapshot(&market);

    // This should fail with MathOverflow
    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::MathOverflow as u32)),
        "overflow path should return explicit MathOverflow code"
    );

    // State must be completely unchanged
    let snap_after = snapshot(&market);
    assert_eq!(
        snap_before, snap_after,
        "state must be unchanged after overflow error"
    );

    // The market should remain usable for no-op accruals after the failure.
    let noop = accrue_interest(&mut market, &config, 0);
    assert_eq!(noop, Ok(()));
    assert_eq!(snapshot(&market), snap_before);
}

/// 6b. Sequence of valid operations, one fails mid-sequence -- partial state
/// is consistent up to the failure point.
#[test]
fn chaos_recovery_mid_sequence_failure_consistent() {
    let config = make_config(500);
    let maturity = 100_000i64;

    let mut market = make_market(1000, maturity, WAD, 0, 0, 0);
    market.set_max_total_supply(u64::MAX);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;

    // Step 1: Deposit 5M -- should succeed
    let (_, ok1) = simulate_deposit(
        &mut market,
        &config,
        &mut lender,
        5_000_000,
        100,
        &mut vault,
    );
    assert!(ok1);

    // Step 2: Accrue interest -- should succeed
    let ok2 = accrue_interest(&mut market, &config, 50_000).is_ok();
    assert!(ok2);
    let snap_after_accrue = snapshot(&market);

    // Step 3: Borrow 10M (exceeds vault) -- should fail
    let ok3 = simulate_borrow(&mut market, &config, 10_000_000, 50_000, &mut vault);
    assert!(!ok3, "borrow exceeding vault should fail");

    // State after failed borrow should match state before the failed borrow
    // (only the accrue in step 2 should have modified state, but the borrow's
    // accrue at same timestamp is idempotent)
    assert_eq!(
        snapshot(&market),
        snap_after_accrue,
        "state should be consistent after mid-sequence failure"
    );
    assert_eq!(
        vault, 5_000_000,
        "vault should be unchanged after failed borrow"
    );

    // Step 4: Borrow a valid amount -- should succeed
    let ok4 = simulate_borrow(&mut market, &config, 1_000_000, 50_000, &mut vault);
    assert!(ok4, "valid borrow after failed borrow should succeed");
    assert_eq!(vault, 4_000_000);

    // Verify solvency invariant
    let expected_vault = (market.total_deposited() as u128)
        .checked_sub(market.total_borrowed() as u128)
        .and_then(|v| v.checked_add(market.total_repaid() as u128))
        .unwrap();
    assert_eq!(
        vault as u128, expected_vault,
        "solvency invariant must hold"
    );
}

/// 6c. Rapid-fire operations (100 operations at 1-second intervals) --
/// verify no state drift.
#[test]
fn chaos_recovery_rapid_fire_100_ops_no_drift() {
    let annual_bps = 1000u16;
    let fee_bps = 500u16;
    let config = make_config(fee_bps);
    let maturity = 1_000_000i64;

    let mut market = make_market(annual_bps, maturity, WAD, 0, 0, 0);
    market.set_max_total_supply(u64::MAX);
    let mut lender = LenderPosition::zeroed();
    let mut vault = 0u64;

    // Initial deposit
    let (initial_scaled, initial_ok) =
        simulate_deposit(&mut market, &config, &mut lender, 10_000_000, 0, &mut vault);
    assert!(initial_ok);
    assert_eq!(initial_scaled, 10_000_000);
    assert_eq!(market.scale_factor(), WAD);
    assert_eq!(market.accrued_protocol_fees(), 0);
    assert_eq!(market.last_accrual_timestamp(), 0);

    for i in 1..=100 {
        let ts = i as i64;
        let pre_snap = snapshot(&market);
        let pre_lender = lender.scaled_balance();
        let pre_vault = vault;
        // COAL-L02: no fee reservation; full vault is borrowable
        let pre_borrowable = pre_vault;

        let expected_sf_after_accrual = oracle_scale_factor_after_step(
            pre_snap.scale_factor,
            annual_bps,
            pre_snap.last_accrual_timestamp,
            maturity,
            ts,
        );
        let expected_fee_delta = oracle_fee_delta_normalized(
            pre_snap.scaled_total_supply,
            pre_snap.scale_factor, // pre-accrual scale factor (Finding 10)
            annual_bps,
            pre_snap.last_accrual_timestamp,
            maturity,
            ts,
            fee_bps,
        );
        let expected_fees_after_accrual = pre_snap.accrued_protocol_fees + expected_fee_delta;
        let effective_now = ts.min(maturity);
        let expected_last_after_accrual = if effective_now > pre_snap.last_accrual_timestamp {
            effective_now
        } else {
            pre_snap.last_accrual_timestamp
        };

        // Alternate between accrue, small deposit, and small borrow
        match i % 3 {
            0 => {
                accrue_interest(&mut market, &config, ts).unwrap();
                assert_eq!(market.scale_factor(), expected_sf_after_accrual);
                assert_eq!(market.accrued_protocol_fees(), expected_fees_after_accrual);
                assert_eq!(market.last_accrual_timestamp(), expected_last_after_accrual);
                assert_eq!(market.scaled_total_supply(), pre_snap.scaled_total_supply);
                assert_eq!(market.total_deposited(), pre_snap.total_deposited);
                assert_eq!(market.total_borrowed(), pre_snap.total_borrowed);
                assert_eq!(vault, pre_vault);
            },
            1 => {
                let (scaled, ok) =
                    simulate_deposit(&mut market, &config, &mut lender, 1_000, ts, &mut vault);
                assert!(ok, "step {}: deposit should succeed", i);
                let expected_scaled = 1_000u128 * WAD / expected_sf_after_accrual;
                assert_eq!(
                    scaled, expected_scaled,
                    "step {}: scaled deposit mismatch",
                    i
                );
                assert_eq!(market.scale_factor(), expected_sf_after_accrual);
                assert_eq!(market.accrued_protocol_fees(), expected_fees_after_accrual);
                assert_eq!(market.last_accrual_timestamp(), expected_last_after_accrual);
                assert_eq!(
                    market.scaled_total_supply(),
                    pre_snap.scaled_total_supply + expected_scaled
                );
                assert_eq!(lender.scaled_balance(), pre_lender + expected_scaled);
                assert_eq!(market.total_deposited(), pre_snap.total_deposited + 1_000);
                assert_eq!(market.total_borrowed(), pre_snap.total_borrowed);
                assert_eq!(vault, pre_vault + 1_000);
            },
            2 => {
                // Only borrow if there are funds
                if pre_borrowable >= 500 {
                    // COAL-L02: no fee reservation; full vault is borrowable
                    let post_accrual_borrowable = pre_vault;
                    let should_succeed = post_accrual_borrowable >= 500;
                    let ok = simulate_borrow(&mut market, &config, 500, ts, &mut vault);
                    assert_eq!(
                        ok, should_succeed,
                        "step {}: borrow success mismatch after accrual",
                        i
                    );

                    assert_eq!(market.scale_factor(), expected_sf_after_accrual);
                    assert_eq!(market.accrued_protocol_fees(), expected_fees_after_accrual);
                    assert_eq!(market.last_accrual_timestamp(), expected_last_after_accrual);
                    assert_eq!(market.scaled_total_supply(), pre_snap.scaled_total_supply);
                    assert_eq!(lender.scaled_balance(), pre_lender);
                    assert_eq!(market.total_deposited(), pre_snap.total_deposited);

                    if should_succeed {
                        assert_eq!(market.total_borrowed(), pre_snap.total_borrowed + 500);
                        assert_eq!(vault, pre_vault - 500);
                    } else {
                        assert_eq!(market.total_borrowed(), pre_snap.total_borrowed);
                        assert_eq!(vault, pre_vault);
                    }
                } else {
                    // No borrow attempted: no accrual or state change.
                    assert_eq!(
                        snapshot(&market),
                        pre_snap,
                        "step {}: no-op borrow path drifted",
                        i
                    );
                    assert_eq!(vault, pre_vault);
                }
            },
            _ => {},
        }

        // Invariant: scale_factor is monotonically non-decreasing and never sub-WAD.
        assert!(
            market.scale_factor() >= pre_snap.scale_factor,
            "step {}: scale_factor decreased: {} -> {}",
            i,
            pre_snap.scale_factor,
            market.scale_factor()
        );
        assert!(
            market.accrued_protocol_fees() >= pre_snap.accrued_protocol_fees,
            "step {}: fees decreased: {} -> {}",
            i,
            pre_snap.accrued_protocol_fees,
            market.accrued_protocol_fees()
        );
        assert!(
            market.scale_factor() >= WAD,
            "step {}: scale_factor ({}) < WAD",
            i,
            market.scale_factor()
        );
        assert_supply_matches_lenders(&market, &[lender]);
        assert_solvency_invariant(&market, vault);
    }

    // Final solvency check
    assert_solvency_invariant(&market, vault);
    assert_supply_matches_lenders(&market, &[lender]);
}

// ===========================================================================
// 7. Proptest chaos sequences (2 tests)
// ===========================================================================

/// Operation types for proptest chaos sequences.
#[derive(Debug, Clone)]
enum ChaosOp {
    Deposit(u32),
    Borrow(u32),
    Repay(u32),
    Accrue(u16),
    CollectFees,
}

fn chaos_op_strategy() -> impl Strategy<Value = ChaosOp> {
    prop_oneof![
        3 => (1u32..=5_000_000u32).prop_map(ChaosOp::Deposit),
        2 => (1u32..=5_000_000u32).prop_map(ChaosOp::Borrow),
        2 => (1u32..=5_000_000u32).prop_map(ChaosOp::Repay),
        2 => (0u16..=3600u16).prop_map(ChaosOp::Accrue),
        1 => Just(ChaosOp::CollectFees),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// 7a. Random interleaving of operations from multiple actors with random
    /// timestamps -- all invariants hold.
    #[test]
    fn chaos_proptest_random_interleaving_invariants(
        annual_bps in 100u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        ops in prop::collection::vec(chaos_op_strategy(), 10..=60),
        actor_indices in prop::collection::vec(0usize..4, 10..=60),
    ) {
        let maturity = 500_000i64;
        let config = make_config(fee_bps);
        let mut market = make_market(annual_bps, maturity, WAD, 0, 0, 0);
        market.set_max_total_supply(u64::MAX);
        let mut lenders = [LenderPosition::zeroed(); 4];
        let mut vault = 0u64;
        let mut clock = 1i64;

        let mut prev_sf = WAD;
        let mut prev_fees = 0u64;

        for (step, op) in ops.iter().enumerate() {
            let actor_idx = actor_indices.get(step).copied().unwrap_or(0) % 4;

            match op {
                ChaosOp::Deposit(raw) => {
                    let amount = (*raw).max(1) as u64;
                    simulate_deposit(
                        &mut market, &config, &mut lenders[actor_idx],
                        amount, clock, &mut vault,
                    );
                }
                ChaosOp::Borrow(raw) => {
                    let amount = (*raw).max(1) as u64;
                    simulate_borrow(&mut market, &config, amount, clock, &mut vault);
                }
                ChaosOp::Repay(raw) => {
                    let amount = (*raw).max(1) as u64;
                    // Repay: accrue, then add to vault
                    let _ = accrue_interest(&mut market, &config, clock);
                    market.set_total_repaid(market.total_repaid().saturating_add(amount));
                    vault = vault.saturating_add(amount);
                }
                ChaosOp::Accrue(delta) => {
                    let d = (*delta).max(1) as i64;
                    clock = clock.saturating_add(d);
                    let _ = accrue_interest(&mut market, &config, clock);
                }
                ChaosOp::CollectFees => {
                    simulate_collect_fees(&mut market, &config, clock, &mut vault);
                }
            }

            // Invariant: scale_factor >= WAD
            prop_assert!(
                market.scale_factor() >= WAD,
                "step {}: scale_factor ({}) < WAD",
                step, market.scale_factor()
            );

            // Invariant: scale_factor monotonically non-decreasing
            prop_assert!(
                market.scale_factor() >= prev_sf,
                "step {}: scale_factor decreased: {} -> {}",
                step, prev_sf, market.scale_factor()
            );

            // Invariant: fees monotonically non-decreasing (before collection)
            // Note: fee collection reduces accrued_protocol_fees, so we only
            // check this after non-collection ops.
            if !matches!(op, ChaosOp::CollectFees) {
                prop_assert!(
                    market.accrued_protocol_fees() >= prev_fees,
                    "step {}: fees decreased without collection: {} -> {}",
                    step, prev_fees, market.accrued_protocol_fees()
                );
            }

            // Invariant: scaled_total_supply equals sum of lender positions
            let sum_lenders: u128 = lenders.iter().map(|l| l.scaled_balance()).sum();
            prop_assert_eq!(
                market.scaled_total_supply(), sum_lenders,
                "step {}: supply ({}) != sum of lenders ({})",
                step, market.scaled_total_supply(), sum_lenders
            );

            prev_sf = market.scale_factor();
            prev_fees = market.accrued_protocol_fees();
        }
    }

    /// 7b. Random corruption of individual market fields followed by invariant
    /// checking -- detect all corruptions.
    #[test]
    fn chaos_proptest_random_field_corruption_detected(
        field_idx in 0u8..=6u8,
        corrupt_value_u128 in any::<u128>(),
        corrupt_value_u64 in any::<u64>(),
        corrupt_value_i64 in any::<i64>(),
        _corrupt_value_u16 in any::<u16>(),
    ) {
        let config = make_config(0);
        let maturity = 1_000_000i64;
        let mut market = make_market(1000, maturity, WAD, 1_500_000, 500_000, 100_000);
        market.set_total_deposited(1_000_000);
        market.set_total_borrowed(500_000);
        market.set_total_repaid(300_000);
        market.set_max_total_supply(10_000_000);

        let vault = 900_000u64;
        let lenders_sum = 1_500_000u128;

        let invariants_hold = |m: &Market| -> bool {
            let scale_ok = m.scale_factor() >= WAD;
            let rate_ok = m.annual_interest_bps() <= MAX_ANNUAL_INTEREST_BPS;
            let time_ok = m.last_accrual_timestamp() <= m.maturity_timestamp();
            let supply_ok = m.scaled_total_supply() == lenders_sum;
            let fees_ok = m.accrued_protocol_fees() <= vault;
            let debt_ok = u128::from(m.total_borrowed())
                <= u128::from(m.total_deposited())
                    .checked_add(u128::from(m.total_repaid()))
                    .unwrap_or(0);
            scale_ok && rate_ok && time_ok && supply_ok && fees_ok && debt_ok
        };

        prop_assert!(invariants_hold(&market), "baseline market must satisfy invariants");
        let good_snap = snapshot(&market);

        // Corrupt one field with a guaranteed-invalid value.
        match field_idx {
            0 => market.set_scale_factor(corrupt_value_u128 % WAD), // always < WAD
            1 => {
                let bump = (corrupt_value_u128 % 1_000_000) + 1;
                market.set_scaled_total_supply(lenders_sum + bump);
            }
            2 => {
                let bump = (corrupt_value_u64 % 1_000_000) + 1;
                market.set_accrued_protocol_fees(vault + bump);
            }
            3 => {
                let bump = (corrupt_value_i64.unsigned_abs() % 10_000) as i64 + 1;
                market.set_last_accrual_timestamp(maturity + bump);
            }
            4 => market.set_total_deposited(0),
            5 => {
                let bump = (corrupt_value_u64 % 1_000_000) + 1;
                let invalid_borrow = market
                    .total_deposited()
                    .saturating_add(market.total_repaid())
                    .saturating_add(bump);
                market.set_total_borrowed(invalid_borrow);
            }
            6 => market.set_annual_interest_bps(MAX_ANNUAL_INTEREST_BPS.saturating_add(1)),
            _ => {}
        }

        let corrupted_snap = snapshot(&market);
        prop_assert_ne!(corrupted_snap, good_snap, "corruption should change snapshot");
        prop_assert!(
            !invariants_hold(&market),
            "field {} corruption should violate at least one invariant",
            field_idx
        );

        if field_idx == 3 {
            let err = accrue_interest(&mut market, &config, maturity).unwrap_err();
            prop_assert_eq!(
                err,
                ProgramError::Custom(LendingError::InvalidTimestamp as u32),
                "time corruption should trigger InvalidTimestamp"
            );
        }
    }
}

// ===========================================================================
// Additional edge cases within categories
// ===========================================================================

/// Verify that the bytemuck size constants match the actual struct sizes.
#[test]
fn chaos_meta_size_constants_match_structs() {
    assert_eq!(
        core::mem::size_of::<Market>(),
        MARKET_SIZE,
        "Market struct size mismatch"
    );
    assert_eq!(
        core::mem::size_of::<ProtocolConfig>(),
        PROTOCOL_CONFIG_SIZE,
        "ProtocolConfig struct size mismatch"
    );
    assert_eq!(
        core::mem::size_of::<LenderPosition>(),
        LENDER_POSITION_SIZE,
        "LenderPosition struct size mismatch"
    );
    assert_eq!(
        core::mem::size_of::<BorrowerWhitelist>(),
        BORROWER_WHITELIST_SIZE,
        "BorrowerWhitelist struct size mismatch"
    );
}

/// Verify that bytemuck casting with oversized buffers also fails.
#[test]
fn chaos_corrupted_oversized_buffer_rejected() {
    macro_rules! assert_oversized_rejected {
        ($ty:ty, $size:expr, $label:expr) => {{
            for extra in [1usize, 2usize, 7usize, 64usize] {
                let buf = vec![0u8; $size + extra];
                assert!(
                    bytemuck::try_from_bytes::<$ty>(&buf).is_err(),
                    "{} cast should reject oversized buffer ({} + {})",
                    $label,
                    $size,
                    extra
                );
            }
            let exact = vec![0u8; $size];
            assert!(
                bytemuck::try_from_bytes::<$ty>(&exact).is_ok(),
                "{} cast should accept exact size {}",
                $label,
                $size
            );
        }};
    }

    assert_oversized_rejected!(Market, MARKET_SIZE, "Market");
    assert_oversized_rejected!(
        BorrowerWhitelist,
        BORROWER_WHITELIST_SIZE,
        "BorrowerWhitelist"
    );
    assert_oversized_rejected!(LenderPosition, LENDER_POSITION_SIZE, "LenderPosition");
    assert_oversized_rejected!(ProtocolConfig, PROTOCOL_CONFIG_SIZE, "ProtocolConfig");
}

/// A market in a completely zeroed state should not crash accrue_interest.
#[test]
fn chaos_corrupted_fully_zeroed_market() {
    let config_zeroed = ProtocolConfig::zeroed();
    let mut market = Market::zeroed();
    let zero_snap = snapshot(&market);

    // current_ts > maturity(0) => effective_now = 0, time_elapsed = 0: no-op.
    let result = accrue_interest(&mut market, &config_zeroed, 1000);
    assert_eq!(
        result,
        Ok(()),
        "accrue on fully zeroed market should succeed as a no-op"
    );
    assert_eq!(snapshot(&market), zero_snap);

    // Repeated no-op accrual remains idempotent.
    let result_repeat = accrue_interest(&mut market, &config_zeroed, 0);
    assert_eq!(result_repeat, Ok(()));
    assert_eq!(snapshot(&market), zero_snap);

    // Negative current_ts produces effective_now < last_accrual and must return explicit error.
    let result_negative = accrue_interest(&mut market, &config_zeroed, -1);
    assert_eq!(
        result_negative,
        Err(ProgramError::Custom(LendingError::InvalidTimestamp as u32)),
        "negative timestamp on zeroed market should be rejected"
    );
    assert_eq!(
        market.scale_factor(),
        0,
        "scale_factor remains 0 on zeroed market"
    );
    assert_eq!(snapshot(&market), zero_snap);
}

/// Verify that extreme fee accrual near u64::MAX is handled properly.
#[test]
fn chaos_extreme_fee_near_u64_max() {
    let config = make_config(MAX_FEE_RATE_BPS); // 100% fee rate
    let one_year = SECONDS_PER_YEAR as i64;
    let annual_bps = MAX_ANNUAL_INTEREST_BPS;

    let new_sf = oracle_scale_factor_after_step(WAD, annual_bps, 0, i64::MAX, one_year);
    let interest_delta_wad = oracle_interest_delta_wad(annual_bps, 0, i64::MAX, one_year);
    let fee_delta_wad = interest_delta_wad * u128::from(MAX_FEE_RATE_BPS) / BPS;

    // Use pre-accrual scale factor (WAD) for fee computation (Finding 10)
    let fee_for_supply = |supply: u128| -> Option<u128> {
        supply
            .checked_mul(WAD)?
            .checked_div(WAD)?
            .checked_mul(fee_delta_wad)?
            .checked_div(WAD)
    };

    // Binary-search the largest supply whose single-step fee still fits in u64.
    let mut lo: u128 = 0;
    let mut hi: u128 = u128::from(u64::MAX);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        match fee_for_supply(mid) {
            Some(fee) if fee <= u128::from(u64::MAX) => lo = mid,
            _ => hi = mid - 1,
        }
    }
    let safe_supply = lo;
    let overflow_supply = safe_supply + 1;
    let safe_expected_fee = fee_for_supply(safe_supply).unwrap();
    assert!(safe_expected_fee <= u128::from(u64::MAX));

    // Boundary below overflow: fee should fit in u64 exactly.
    let mut market_safe = make_market(annual_bps, i64::MAX, WAD, safe_supply, 0, 0);
    let safe_result = accrue_interest(&mut market_safe, &config, one_year);
    assert_eq!(safe_result, Ok(()));
    assert_eq!(market_safe.scale_factor(), new_sf);
    assert_eq!(
        market_safe.accrued_protocol_fees(),
        u64::try_from(safe_expected_fee).unwrap(),
        "safe boundary should produce exact fee without overflow"
    );

    // Boundary above overflow: fee conversion to u64 must fail atomically.
    let mut market_overflow = make_market(annual_bps, i64::MAX, WAD, overflow_supply, 0, 0);
    let snap_before = snapshot(&market_overflow);
    let overflow_result = accrue_interest(&mut market_overflow, &config, one_year);
    assert_eq!(
        overflow_result,
        Err(ProgramError::Custom(LendingError::MathOverflow as u32)),
        "supply crossing boundary by 1 should overflow deterministically"
    );
    assert_eq!(
        snapshot(&market_overflow),
        snap_before,
        "overflow path must not mutate market state"
    );
}

/// Verify that settlement_factor_wad is clamped to [1, WAD] even under extreme conditions.
#[test]
fn chaos_extreme_settlement_factor_clamping() {
    let total = WAD;

    // Upper clamp boundaries.
    assert_eq!(
        compute_settlement_factor(total + 1, total).unwrap(),
        WAD,
        "overfunded settlement should clamp to WAD"
    );
    assert_eq!(
        compute_settlement_factor(total, total).unwrap(),
        WAD,
        "exactly funded settlement should be WAD"
    );
    assert_eq!(
        compute_settlement_factor(total - 1, total).unwrap(),
        WAD - 1,
        "x-1 boundary should reduce settlement by 1 at total=WAD"
    );

    // Lower clamp boundaries.
    let huge_total = WAD * 2;
    assert_eq!(compute_settlement_factor(0, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(1, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(2, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(3, huge_total).unwrap(), 1);
    assert_eq!(compute_settlement_factor(4, huge_total).unwrap(), 2);

    // Special-case boundary: no lenders -> settlement defaults to WAD.
    assert_eq!(
        compute_settlement_factor(123, 0).unwrap(),
        WAD,
        "empty market should report full settlement"
    );
}

/// Stress: interleaved deposits and borrows from many actors at very close timestamps.
#[test]
fn chaos_stress_interleaved_many_actors_close_timestamps() {
    let annual_bps = 1000u16;
    let fee_bps = 500u16;
    let config = make_config(fee_bps);
    let maturity = 1_000_000i64;

    let mut market = make_market(annual_bps, maturity, WAD, 0, 0, 0);
    market.set_max_total_supply(u64::MAX);
    let mut lenders = [LenderPosition::zeroed(); 4];
    let mut vault = 0u64;
    let mut successful_borrows = 0u64;

    // 200 operations over 200 seconds
    for i in 0..200 {
        let ts = (i + 1) as i64;
        let lender_idx = i % 4;
        let pre_snap = snapshot(&market);
        let pre_vault = vault;
        let pre_balances = lenders.map(|l| l.scaled_balance());
        // COAL-L02: no fee reservation; full vault is borrowable
        let pre_borrowable = pre_vault;

        let expected_sf_after_accrual = oracle_scale_factor_after_step(
            pre_snap.scale_factor,
            annual_bps,
            pre_snap.last_accrual_timestamp,
            maturity,
            ts,
        );
        let expected_fee_delta = oracle_fee_delta_normalized(
            pre_snap.scaled_total_supply,
            pre_snap.scale_factor, // pre-accrual scale factor (Finding 10)
            annual_bps,
            pre_snap.last_accrual_timestamp,
            maturity,
            ts,
            fee_bps,
        );
        let expected_fees_after_accrual = pre_snap.accrued_protocol_fees + expected_fee_delta;
        let effective_now = ts.min(maturity);
        let expected_last_after_accrual = if effective_now > pre_snap.last_accrual_timestamp {
            effective_now
        } else {
            pre_snap.last_accrual_timestamp
        };

        if i % 2 == 0 {
            // Even: deposit
            let (scaled, ok) = simulate_deposit(
                &mut market,
                &config,
                &mut lenders[lender_idx],
                10_000,
                ts,
                &mut vault,
            );
            assert!(ok, "step {}: deposit should succeed", i);
            let expected_scaled = 10_000u128 * WAD / expected_sf_after_accrual;
            assert_eq!(
                scaled, expected_scaled,
                "step {}: scaled deposit mismatch",
                i
            );
            assert_eq!(market.scale_factor(), expected_sf_after_accrual);
            assert_eq!(market.accrued_protocol_fees(), expected_fees_after_accrual);
            assert_eq!(market.last_accrual_timestamp(), expected_last_after_accrual);
            assert_eq!(market.total_deposited(), pre_snap.total_deposited + 10_000);
            assert_eq!(market.total_borrowed(), pre_snap.total_borrowed);
            assert_eq!(
                market.scaled_total_supply(),
                pre_snap.scaled_total_supply + expected_scaled
            );
            assert_eq!(vault, pre_vault + 10_000);
            for idx in 0..4 {
                let expected_balance = if idx == lender_idx {
                    pre_balances[idx] + expected_scaled
                } else {
                    pre_balances[idx]
                };
                assert_eq!(lenders[idx].scaled_balance(), expected_balance);
            }
        } else {
            // Odd: try to borrow
            if pre_borrowable >= 5_000 {
                // COAL-L02: no fee reservation; full vault is borrowable
                let post_accrual_borrowable = pre_vault;
                let should_succeed = post_accrual_borrowable >= 5_000;
                let ok = simulate_borrow(&mut market, &config, 5_000, ts, &mut vault);
                assert_eq!(ok, should_succeed, "step {}: borrow outcome mismatch", i);
                assert_eq!(market.scale_factor(), expected_sf_after_accrual);
                assert_eq!(market.accrued_protocol_fees(), expected_fees_after_accrual);
                assert_eq!(market.last_accrual_timestamp(), expected_last_after_accrual);
                assert_eq!(market.scaled_total_supply(), pre_snap.scaled_total_supply);
                assert_eq!(market.total_deposited(), pre_snap.total_deposited);
                if should_succeed {
                    successful_borrows = successful_borrows.saturating_add(1);
                    assert_eq!(market.total_borrowed(), pre_snap.total_borrowed + 5_000);
                    assert_eq!(vault, pre_vault - 5_000);
                } else {
                    assert_eq!(market.total_borrowed(), pre_snap.total_borrowed);
                    assert_eq!(vault, pre_vault);
                }
                for idx in 0..4 {
                    assert_eq!(lenders[idx].scaled_balance(), pre_balances[idx]);
                }
            } else {
                // No borrow attempted: no state transition.
                assert_eq!(
                    snapshot(&market),
                    pre_snap,
                    "step {}: skipped borrow drifted",
                    i
                );
                assert_eq!(vault, pre_vault);
            }
        }

        assert!(
            market.scale_factor() >= pre_snap.scale_factor,
            "step {}: scale_factor must be monotonic",
            i
        );
        assert!(
            market.scale_factor() >= WAD,
            "step {}: scale_factor below WAD",
            i
        );
        assert_supply_matches_lenders(&market, &lenders);
        assert_solvency_invariant(&market, vault);
    }

    // Verify invariants at the end
    assert_supply_matches_lenders(&market, &lenders);
    assert_solvency_invariant(&market, vault);
    assert_eq!(
        market.total_deposited(),
        100 * 10_000,
        "exactly 100 deposits should execute in 200 alternating steps"
    );
    assert_eq!(market.total_repaid(), 0);
    assert_eq!(market.total_borrowed(), successful_borrows * 5_000);
}
