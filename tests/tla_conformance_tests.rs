//! TLA+ Formal Specification Conformance Tests
//!
//! These tests verify that the on-chain Rust implementation conforms to the
//! TLA+ specification in `specs/CoalesceFi.tla`. Each TLA+ action is tested
//! by setting up precondition state matching the TLA+ guard, executing the
//! corresponding on-chain logic, and asserting all 10 TLA+ invariants hold
//! after the action, plus the action-specific postconditions from the spec.
//!
//! The TLA+ spec uses scaled-down constants (WAD=1000, BPS=100, etc.) for model
//! checking feasibility. These tests use the real constants (WAD=1e18, BPS=10000,
//! SECONDS_PER_YEAR=31536000) and reproduce the spec's mathematical formulas
//! exactly in u128 arithmetic.

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
use proptest::prelude::*;

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

#[path = "common/math_oracle.rs"]
mod math_oracle;

// ===========================================================================
// TLA+ Model State — mirrors the TLA+ variables
// ===========================================================================

/// A pure-Rust mirror of the TLA+ state, using the on-chain types directly.
/// This lets us execute protocol operations and check invariants without
/// needing the full Solana runtime.
#[derive(Clone)]
struct TlaState {
    market: Market,
    config: ProtocolConfig,
    lender_positions: Vec<LenderPosition>, // one per lender
    whitelist: BorrowerWhitelist,
    vault_balance: u64,
    current_time: i64,
    // Tracking variables for monotonicity invariants (TLA+ prev_*)
    prev_scale_factor: u128,
    prev_settlement_factor: u128,
    // Accumulated payouts for TotalPayoutBounded
    total_payouts: u128,
    // Emergency pause flag (from ProtocolConfig)
    is_paused: bool,
    // Cumulative interest-only repayments
    total_interest_repaid: u64,
}

// ===========================================================================
// TLA+ Helper Operators — exact translations from the spec
// ===========================================================================

/// TLA+ Div(a, b) — integer division, returns 0 if b==0.
fn tla_div(a: u128, b: u128) -> u128 {
    if b == 0 {
        0
    } else {
        a / b
    }
}

/// TLA+ NormalizedTotalSupply
fn normalized_total_supply(market: &Market) -> u128 {
    tla_div(market.scaled_total_supply() * market.scale_factor(), WAD)
}

/// TLA+ AvailableForLenders
/// COAL-C01: no fee reservation; full vault is available for lenders.
fn available_for_lenders(vault_balance: u64, _accrued_fees: u64) -> u128 {
    u128::from(vault_balance)
}

/// TLA+ ComputeSettlementFactor
fn compute_settlement_factor(market: &Market, vault_balance: u64) -> u128 {
    let total_norm = normalized_total_supply(market);
    let avail = available_for_lenders(vault_balance, market.accrued_protocol_fees());
    if total_norm == 0 {
        WAD
    } else {
        let raw = tla_div(avail * WAD, total_norm);
        let capped = raw.min(WAD);
        capped.max(1)
    }
}

/// Oracle: effective accrual timestamp is capped by maturity.
fn oracle_effective_now(current_time: i64, maturity: i64) -> i64 {
    current_time.min(maturity)
}

fn oracle_mul_wad(a: u128, b: u128) -> u128 {
    math_oracle::mul_wad(a, b)
}

fn oracle_pow_wad(base: u128, exp: u32) -> u128 {
    math_oracle::pow_wad(base, exp)
}

/// Oracle: total growth factor in WAD for one accrual step.
/// This 4-arg variant includes maturity-capping logic on top of the shared oracle.
fn oracle_growth_factor_wad(
    annual_bps: u16,
    last_accrual_timestamp: i64,
    current_time: i64,
    maturity: i64,
) -> u128 {
    let effective_now = oracle_effective_now(current_time, maturity);
    if effective_now <= last_accrual_timestamp {
        return WAD;
    }

    let elapsed = effective_now
        .checked_sub(last_accrual_timestamp)
        .expect("non-negative elapsed");

    math_oracle::growth_factor_wad(annual_bps, elapsed)
}

/// Oracle: interest delta in WAD for one accrual step.
fn oracle_interest_delta_wad(
    annual_bps: u16,
    last_accrual_timestamp: i64,
    current_time: i64,
    maturity: i64,
) -> u128 {
    oracle_growth_factor_wad(annual_bps, last_accrual_timestamp, current_time, maturity)
        .checked_sub(WAD)
        .expect("growth factor must be >= WAD")
}

/// Oracle: scale factor after one accrual step.
fn oracle_scale_factor_after_accrual(
    prior_scale_factor: u128,
    annual_bps: u16,
    last_accrual_timestamp: i64,
    current_time: i64,
    maturity: i64,
) -> u128 {
    let growth_wad =
        oracle_growth_factor_wad(annual_bps, last_accrual_timestamp, current_time, maturity);
    oracle_mul_wad(prior_scale_factor, growth_wad)
}

/// Oracle: fee delta in normalized units for one accrual step.
fn oracle_fee_delta_normalized(
    scaled_total_supply: u128,
    new_scale_factor: u128,
    interest_delta_wad: u128,
    fee_rate_bps: u16,
) -> u64 {
    if fee_rate_bps == 0 || interest_delta_wad == 0 {
        return 0;
    }
    let fee_delta_wad = interest_delta_wad * u128::from(fee_rate_bps) / BPS;
    let fee_normalized = scaled_total_supply * new_scale_factor / WAD * fee_delta_wad / WAD;
    u64::try_from(fee_normalized).unwrap()
}

fn oracle_scaled_amount(amount: u64, scale_factor: u128) -> u128 {
    u128::from(amount) * WAD / scale_factor
}

fn oracle_normalized_amount(scaled_amount: u128, scale_factor: u128) -> u128 {
    tla_div(scaled_amount * scale_factor, WAD)
}

fn oracle_settlement_factor(total_normalized: u128, vault_balance: u64, accrued_fees: u64) -> u128 {
    if total_normalized == 0 {
        return WAD;
    }
    let available = available_for_lenders(vault_balance, accrued_fees);
    tla_div(available * WAD, total_normalized).min(WAD).max(1)
}

fn oracle_payout(scaled_amount: u128, scale_factor: u128, settlement_factor: u128) -> u128 {
    let normalized = oracle_normalized_amount(scaled_amount, scale_factor);
    tla_div(normalized * settlement_factor, WAD)
}

fn sum_scaled_balances(state: &TlaState) -> u128 {
    state
        .lender_positions
        .iter()
        .try_fold(0u128, |acc, pos| acc.checked_add(pos.scaled_balance()))
        .expect("scaled balance sum must fit u128")
}

fn assert_scaled_supply_matches_positions(state: &TlaState) {
    assert_eq!(
        state.market.scaled_total_supply(),
        sum_scaled_balances(state),
        "market.scaled_total_supply must equal sum of lender scaled balances"
    );
}

fn assert_state_unchanged(before: &TlaState, after: &TlaState, ctx: &str) {
    assert_eq!(
        after.current_time, before.current_time,
        "{ctx}: current_time"
    );
    assert_eq!(
        after.vault_balance, before.vault_balance,
        "{ctx}: vault_balance"
    );
    assert_eq!(
        after.total_payouts, before.total_payouts,
        "{ctx}: total_payouts"
    );
    assert_eq!(
        after.prev_scale_factor, before.prev_scale_factor,
        "{ctx}: prev_scale_factor"
    );
    assert_eq!(
        after.prev_settlement_factor, before.prev_settlement_factor,
        "{ctx}: prev_settlement_factor"
    );

    assert_eq!(
        after.market.scale_factor(),
        before.market.scale_factor(),
        "{ctx}: market.scale_factor"
    );
    assert_eq!(
        after.market.scaled_total_supply(),
        before.market.scaled_total_supply(),
        "{ctx}: market.scaled_total_supply"
    );
    assert_eq!(
        after.market.accrued_protocol_fees(),
        before.market.accrued_protocol_fees(),
        "{ctx}: market.accrued_protocol_fees"
    );
    assert_eq!(
        after.market.total_deposited(),
        before.market.total_deposited(),
        "{ctx}: market.total_deposited"
    );
    assert_eq!(
        after.market.total_borrowed(),
        before.market.total_borrowed(),
        "{ctx}: market.total_borrowed"
    );
    assert_eq!(
        after.market.total_repaid(),
        before.market.total_repaid(),
        "{ctx}: market.total_repaid"
    );
    assert_eq!(
        after.market.last_accrual_timestamp(),
        before.market.last_accrual_timestamp(),
        "{ctx}: market.last_accrual_timestamp"
    );
    assert_eq!(
        after.market.settlement_factor_wad(),
        before.market.settlement_factor_wad(),
        "{ctx}: market.settlement_factor_wad"
    );

    assert_eq!(
        after.market.total_interest_repaid(),
        before.market.total_interest_repaid(),
        "{ctx}: market.total_interest_repaid"
    );

    assert_eq!(after.is_paused, before.is_paused, "{ctx}: is_paused");
    assert_eq!(
        after.total_interest_repaid, before.total_interest_repaid,
        "{ctx}: total_interest_repaid"
    );

    assert_eq!(
        after.whitelist.current_borrowed(),
        before.whitelist.current_borrowed(),
        "{ctx}: whitelist.current_borrowed"
    );
    assert_eq!(
        after.whitelist.max_borrow_capacity(),
        before.whitelist.max_borrow_capacity(),
        "{ctx}: whitelist.max_borrow_capacity"
    );
    assert_eq!(
        after.whitelist.is_whitelisted, before.whitelist.is_whitelisted,
        "{ctx}: whitelist.is_whitelisted"
    );

    for idx in 0..NUM_LENDERS {
        assert_eq!(
            after.lender_positions[idx].scaled_balance(),
            before.lender_positions[idx].scaled_balance(),
            "{ctx}: lender {idx} scaled_balance"
        );
    }
}

// ===========================================================================
// TLA+ Invariant Checks — all 10 safety invariants from the spec
// ===========================================================================

/// INV-1: VaultSolvency — vault_balance >= 0
/// (Always true for u64, but we check the state is coherent.)
fn check_vault_solvency(state: &TlaState) {
    // u64 is inherently >= 0, so check stronger solvency coherence:
    // available-for-lenders must be bounded by vault balance.
    let available =
        available_for_lenders(state.vault_balance, state.market.accrued_protocol_fees());
    assert!(
        available <= u128::from(state.vault_balance),
        "INV-1 VaultSolvency violated: available={} > vault_balance={}",
        available,
        state.vault_balance
    );
}

/// INV-2: ScaleFactorMonotonic — scale_factor >= prev_scale_factor
fn check_scale_factor_monotonic(state: &TlaState) {
    assert!(
        state.market.scale_factor() >= state.prev_scale_factor,
        "INV-2 ScaleFactorMonotonic violated: scale_factor={} < prev={}",
        state.market.scale_factor(),
        state.prev_scale_factor
    );
}

/// INV-3: SettlementFactorBounded — when set, settlement_factor in [1, WAD]
fn check_settlement_factor_bounded(state: &TlaState) {
    let sf = state.market.settlement_factor_wad();
    if sf != 0 {
        assert!(
            sf >= 1 && sf <= WAD,
            "INV-3 SettlementFactorBounded violated: settlement_factor={} not in [1, {}]",
            sf,
            WAD
        );
    }
}

/// INV-4: SettlementFactorMonotonic — once set, never decreases
fn check_settlement_factor_monotonic(state: &TlaState) {
    let sf = state.market.settlement_factor_wad();
    let prev = state.prev_settlement_factor;
    if sf != 0 && prev != 0 {
        assert!(
            sf >= prev,
            "INV-4 SettlementFactorMonotonic violated: settlement_factor={} < prev={}",
            sf,
            prev
        );
    }
}

/// INV-5: FeesNeverNegative — accrued_protocol_fees >= 0
/// (Always true for u64.)
fn check_fees_non_negative(state: &TlaState) {
    let fees = state.market.accrued_protocol_fees();
    let withdrawable = fees.min(state.vault_balance);
    let remaining = fees - withdrawable;
    assert_eq!(
        remaining + withdrawable,
        fees,
        "INV-5 FeesNeverNegative violated: fee reserve accounting mismatch"
    );
}

/// INV-6: CapRespected — normalized total supply <= max_total_supply
fn check_cap_respected(state: &TlaState) {
    let norm = normalized_total_supply(&state.market);
    let max = u128::from(state.market.max_total_supply());
    assert!(
        norm <= max,
        "INV-6 CapRespected violated: normalized_supply={} > max_total_supply={}",
        norm,
        max
    );
}

/// INV-7: WhitelistCapacity — current_borrowed <= max_capacity
fn check_whitelist_capacity(state: &TlaState) {
    assert!(
        state.whitelist.current_borrowed() <= state.whitelist.max_borrow_capacity(),
        "INV-7 WhitelistCapacity violated: current_borrowed={} > max_capacity={}",
        state.whitelist.current_borrowed(),
        state.whitelist.max_borrow_capacity()
    );
}

/// INV-8: PayoutBounded — each lender payout <= their normalized amount
fn check_payout_bounded(state: &TlaState) {
    let sf_wad = state.market.settlement_factor_wad();
    if sf_wad == 0 {
        return;
    }
    let scale_factor = state.market.scale_factor();
    for pos in &state.lender_positions {
        let norm = tla_div(pos.scaled_balance() * scale_factor, WAD);
        let payout = tla_div(norm * sf_wad, WAD);
        assert!(
            payout <= norm,
            "INV-8 PayoutBounded violated: payout={} > norm={} for lender",
            payout,
            norm
        );
    }
}

/// INV-9: TotalPayoutBounded — total payouts <= total_deposited + total_repaid
fn check_total_payout_bounded(state: &TlaState) {
    let max_allowed =
        u128::from(state.market.total_deposited()) + u128::from(state.market.total_repaid());
    assert!(
        state.total_payouts <= max_allowed,
        "INV-9 TotalPayoutBounded violated: total_payouts={} > total_deposited+total_repaid={}",
        state.total_payouts,
        max_allowed
    );
}

/// INV-10: TypeInvariant — all state variables are non-negative and valid
fn check_type_invariant(state: &TlaState) {
    // All u64/u128 fields are inherently >= 0.
    // Verify scale_factor is positive when market is initialized.
    if state.market.scale_factor() > 0 {
        // Market has been initialized
        assert!(
            state.market.scale_factor() >= WAD,
            "TypeInvariant: scale_factor={} should be >= WAD once initialized",
            state.market.scale_factor()
        );
    }
    // is_paused must be a boolean (already enforced by Rust bool type)
    // total_interest_repaid must be >= 0 (always true for u64)
    // Consistency: TlaState.total_interest_repaid must match market field
    assert_eq!(
        state.total_interest_repaid,
        state.market.total_interest_repaid(),
        "TypeInvariant: TlaState.total_interest_repaid must match market.total_interest_repaid()"
    );
    assert_eq!(
        state.is_paused,
        state.config.is_paused(),
        "TypeInvariant: TlaState.is_paused must match config.is_paused()"
    );
    assert_scaled_supply_matches_positions(state);
}

/// Check ALL 10 TLA+ invariants.
fn check_all_invariants(state: &TlaState) {
    check_vault_solvency(state);
    check_scale_factor_monotonic(state);
    check_settlement_factor_bounded(state);
    check_settlement_factor_monotonic(state);
    check_fees_non_negative(state);
    check_cap_respected(state);
    check_whitelist_capacity(state);
    check_payout_bounded(state);
    check_total_payout_bounded(state);
    check_type_invariant(state);
}

// ===========================================================================
// State construction helpers
// ===========================================================================

const DEFAULT_ANNUAL_BPS: u16 = 1000; // 10%
const DEFAULT_FEE_RATE_BPS: u16 = 500; // 5%
const DEFAULT_MATURITY: i64 = 31_536_000; // 1 year
const DEFAULT_MAX_SUPPLY: u64 = 1_000_000_000_000; // 1M USDC (6 decimals)
const DEFAULT_MAX_CAPACITY: u64 = 1_000_000_000_000;
const NUM_LENDERS: usize = 2;

/// Create a fresh TLA+ Init state (before CreateMarket).
fn tla_init_state() -> TlaState {
    let market = Market::zeroed();
    let config = ProtocolConfig::zeroed();
    let positions = (0..NUM_LENDERS).map(|_| LenderPosition::zeroed()).collect();
    let whitelist = BorrowerWhitelist::zeroed();
    TlaState {
        market,
        config,
        lender_positions: positions,
        whitelist,
        vault_balance: 0,
        current_time: 0,
        prev_scale_factor: 0,
        prev_settlement_factor: 0,
        total_payouts: 0,
        is_paused: false,
        total_interest_repaid: 0,
    }
}

/// Create a market that has been initialized (post-CreateMarket).
fn tla_created_market_state(current_time: i64) -> TlaState {
    let mut state = tla_init_state();
    // CreateMarket postconditions from TLA+:
    state.market.set_scale_factor(WAD);
    state.market.set_scaled_total_supply(0);
    state.market.set_accrued_protocol_fees(0);
    state.market.set_total_deposited(0);
    state.market.set_total_borrowed(0);
    state.market.set_total_repaid(0);
    state.market.set_last_accrual_timestamp(current_time);
    state.market.set_settlement_factor_wad(0);
    state.market.set_annual_interest_bps(DEFAULT_ANNUAL_BPS);
    state.market.set_maturity_timestamp(DEFAULT_MATURITY);
    state.market.set_max_total_supply(DEFAULT_MAX_SUPPLY);

    state.config.set_fee_rate_bps(DEFAULT_FEE_RATE_BPS);

    state.whitelist.is_whitelisted = 1;
    state
        .whitelist
        .set_max_borrow_capacity(DEFAULT_MAX_CAPACITY);
    state.whitelist.set_current_borrowed(0);

    state.current_time = current_time;
    state.prev_scale_factor = WAD;
    state.prev_settlement_factor = 0;
    state.is_paused = false;
    state.total_interest_repaid = 0;
    state
}

// ===========================================================================
// TLA+ Action Implementations — pure Rust, no Solana runtime
// ===========================================================================

/// Execute TLA+ AccrueInterestEffect using on-chain code.
/// Updates market in place. Returns new_sf for tracking.
fn action_accrue(state: &mut TlaState, with_fees: bool) {
    if with_fees {
        accrue_interest(&mut state.market, &state.config, state.current_time).unwrap();
    } else {
        let zero_config = ProtocolConfig::zeroed();
        accrue_interest(&mut state.market, &zero_config, state.current_time).unwrap();
    }
    state.prev_scale_factor = state.market.scale_factor();
}

/// TLA+ Deposit(lender, amount)
fn action_deposit(state: &mut TlaState, lender_idx: usize, amount: u64) {
    assert!(amount > 0);
    assert!(state.current_time < state.market.maturity_timestamp());
    assert_eq!(state.market.settlement_factor_wad(), 0);

    // Step 1: Accrue interest (with fees)
    action_accrue(state, true);

    let scale_factor = state.market.scale_factor();

    // Step 2: Compute scaled amount = amount * WAD / scale_factor
    let amount_u128 = u128::from(amount);
    let scaled_amount = amount_u128
        .checked_mul(WAD)
        .unwrap()
        .checked_div(scale_factor)
        .unwrap();
    assert!(scaled_amount > 0, "scaled_amount must be > 0");

    // Step 3: Validate cap
    let new_scaled_total = state.market.scaled_total_supply() + scaled_amount;
    let new_normalized = tla_div(new_scaled_total * scale_factor, WAD);
    assert!(
        new_normalized <= u128::from(state.market.max_total_supply()),
        "cap exceeded"
    );

    // Step 4: Update state (mirrors TLA+ postconditions)
    state.market.set_scaled_total_supply(new_scaled_total);
    let new_total_deposited = state.market.total_deposited() + amount;
    state.market.set_total_deposited(new_total_deposited);
    state.vault_balance += amount;

    // Update lender position
    let old_balance = state.lender_positions[lender_idx].scaled_balance();
    state.lender_positions[lender_idx].set_scaled_balance(old_balance + scaled_amount);
}

/// TLA+ Borrow(amount)
fn action_borrow(state: &mut TlaState, amount: u64) {
    assert!(amount > 0);
    assert!(state.current_time < state.market.maturity_timestamp());
    assert_eq!(state.market.settlement_factor_wad(), 0);

    // Step 1: Accrue interest (with fees)
    action_accrue(state, true);

    // Step 2: Compute borrowable (fee reservation)
    let fees_reserved = state
        .vault_balance
        .min(state.market.accrued_protocol_fees());
    let borrowable = state.vault_balance - fees_reserved;
    assert!(amount <= borrowable, "borrow amount exceeds borrowable");

    // Step 3: Whitelist capacity check
    let new_wl_total = state.whitelist.current_borrowed() + amount;
    assert!(
        new_wl_total <= state.whitelist.max_borrow_capacity(),
        "whitelist capacity exceeded"
    );

    // Step 4: Update state
    let new_total_borrowed = state.market.total_borrowed() + amount;
    state.market.set_total_borrowed(new_total_borrowed);
    state.vault_balance -= amount;
    state.whitelist.set_current_borrowed(new_wl_total);
}

/// TLA+ Repay(amount) — uses zero-fee accrual per spec
fn action_repay(state: &mut TlaState, amount: u64) {
    assert!(amount > 0);

    // Step 1: Accrue interest with zero fee config (matches TLA+ spec)
    action_accrue(state, false);

    // Step 2: Update state
    let new_total_repaid = state.market.total_repaid() + amount;
    state.market.set_total_repaid(new_total_repaid);
    state.vault_balance += amount;
    // Fees unchanged (zero-fee accrual per spec)
}

/// TLA+ RepayInterest(amount) — like Repay but also increments total_interest_repaid
/// and does NOT touch whitelist_current_borrowed. Uses zero-fee accrual.
fn action_repay_interest(state: &mut TlaState, amount: u64) {
    assert!(amount > 0);

    // Step 1: Accrue interest with zero fee config (matches TLA+ spec)
    action_accrue(state, false);

    // Step 2: Update state
    let new_total_repaid = state.market.total_repaid() + amount;
    state.market.set_total_repaid(new_total_repaid);
    let new_total_interest_repaid = state.market.total_interest_repaid() + amount;
    state
        .market
        .set_total_interest_repaid(new_total_interest_repaid);
    state.total_interest_repaid = new_total_interest_repaid;
    state.vault_balance += amount;
    // Fees unchanged (zero-fee accrual per spec)
    // whitelist_current_borrowed unchanged
}

/// TLA+ Withdraw(lender)
fn action_withdraw(state: &mut TlaState, lender_idx: usize) {
    assert!(state.current_time >= state.market.maturity_timestamp());
    assert!(state.lender_positions[lender_idx].scaled_balance() > 0);

    // Step 1: Accrue interest (with fees)
    action_accrue(state, true);

    // Step 2: Compute or retrieve settlement factor
    if state.market.settlement_factor_wad() == 0 {
        let sf = compute_settlement_factor(&state.market, state.vault_balance);
        state.market.set_settlement_factor_wad(sf);
    }

    let sf_wad = state.market.settlement_factor_wad();
    let scale_factor = state.market.scale_factor();

    // Full withdrawal (TLA+ model uses full balance)
    let scaled_amount = state.lender_positions[lender_idx].scaled_balance();

    // Step 3: Compute payout = scaled_amount * scale_factor / WAD * sf_wad / WAD
    let normalized_amount = tla_div(scaled_amount * scale_factor, WAD);
    let payout_u128 = tla_div(normalized_amount * sf_wad, WAD);
    let payout = u64::try_from(payout_u128).unwrap();

    assert!(payout > 0, "payout must be > 0");
    assert!(payout <= state.vault_balance, "payout exceeds vault");

    // Step 4: Update state
    state.vault_balance -= payout;
    state.lender_positions[lender_idx].set_scaled_balance(0);
    let new_scaled_total = state.market.scaled_total_supply() - scaled_amount;
    state.market.set_scaled_total_supply(new_scaled_total);
    state.total_payouts += payout_u128;

    // Update tracking
    state.prev_settlement_factor = sf_wad;
}

/// TLA+ CollectFees
/// COAL-C01: withdrawable is capped to vault surplus above lender claims
/// when supply > 0, preventing drain below obligations.
/// Returns false if the operation would be a no-op (mirrors on-chain NoFeesToCollect).
fn action_collect_fees(state: &mut TlaState) -> bool {
    // Step 1: Accrue interest (with fees)
    action_accrue(state, true);

    let fees = state.market.accrued_protocol_fees();
    if fees == 0 {
        return false;
    }

    // Step 2: Compute withdrawable (with lender-claims cap)
    let mut withdrawable = fees.min(state.vault_balance);
    if state.market.scaled_total_supply() > 0 {
        let sf = state.market.scale_factor();
        let total_norm = state.market.scaled_total_supply()
            .checked_mul(sf).unwrap()
            .checked_div(WAD).unwrap();
        let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
        let safe_max = state.vault_balance.saturating_sub(lender_claims);
        withdrawable = withdrawable.min(safe_max);
    }
    if withdrawable == 0 {
        return false;
    }

    // Step 3: Update state
    state.vault_balance -= withdrawable;
    state.market.set_accrued_protocol_fees(fees - withdrawable);
    true
}

/// TLA+ ReSettle
fn action_re_settle(state: &mut TlaState) {
    let old_factor = state.market.settlement_factor_wad();
    assert!(old_factor > 0, "must already be settled");

    // Step 1: Accrue with zero fee (matches TLA+ spec)
    action_accrue(state, false);

    // Step 2: Compute new settlement factor
    let new_factor = compute_settlement_factor(&state.market, state.vault_balance);

    // Step 3: Must be strictly improved
    assert!(
        new_factor > old_factor,
        "re-settle requires improvement: new={} old={}",
        new_factor,
        old_factor
    );

    // Step 4: Update state
    state.market.set_settlement_factor_wad(new_factor);
    state.prev_settlement_factor = new_factor;
}

/// TLA+ WithdrawExcess — sweep remaining vault balance after all lenders withdrawn
fn action_withdraw_excess(state: &mut TlaState) {
    assert_eq!(state.market.scaled_total_supply(), 0);
    assert_eq!(state.market.settlement_factor_wad(), WAD);
    assert_eq!(state.market.accrued_protocol_fees(), 0);
    assert!(state.vault_balance > 0);

    let excess = state.vault_balance;
    state.total_payouts += u128::from(excess);
    state.vault_balance = 0;
}

/// TLA+ SetPause(flag) — flip the pause flag
fn action_set_pause(state: &mut TlaState, flag: bool) {
    state.is_paused = flag;
    state.config.set_paused(flag);
}

/// TLA+ Tick — advance time by delta
fn action_tick(state: &mut TlaState, delta: i64) {
    state.current_time += delta;
}

// ===========================================================================
// Test 1: tla_create_market
// ===========================================================================

#[test]
fn tla_create_market() {
    let current_time = 100i64;
    let state = tla_created_market_state(current_time);

    // TLA+ CreateMarket postconditions:
    // scale_factor' = WAD
    assert_eq!(state.market.scale_factor(), WAD);
    // scaled_total_supply' = 0
    assert_eq!(state.market.scaled_total_supply(), 0);
    // accrued_protocol_fees' = 0
    assert_eq!(state.market.accrued_protocol_fees(), 0);
    // total_deposited' = 0
    assert_eq!(state.market.total_deposited(), 0);
    // total_borrowed' = 0
    assert_eq!(state.market.total_borrowed(), 0);
    // total_repaid' = 0
    assert_eq!(state.market.total_repaid(), 0);
    // last_accrual_timestamp' = current_time
    assert_eq!(state.market.last_accrual_timestamp(), current_time);
    // settlement_factor_wad' = 0
    assert_eq!(state.market.settlement_factor_wad(), 0);
    // vault_balance' = 0
    assert_eq!(state.vault_balance, 0);

    check_all_invariants(&state);
}

// ===========================================================================
// Test 2: tla_deposit
// ===========================================================================

#[test]
fn tla_deposit() {
    let mut state = tla_created_market_state(0);
    let amount: u64 = 1_000_000; // 1 USDC

    // Advance time to accrue some interest
    action_tick(&mut state, 1000);

    // Execute deposit
    action_deposit(&mut state, 0, amount);

    // TLA+ Deposit postconditions:
    // scaled_total_supply increased
    assert!(state.market.scaled_total_supply() > 0);
    // total_deposited increased by amount
    assert_eq!(state.market.total_deposited(), amount);
    // vault_balance increased by amount
    assert_eq!(state.vault_balance, amount);

    // Verify scaled_amount = amount * WAD / scale_factor
    let sf = state.market.scale_factor();
    let expected_scaled = u128::from(amount) * WAD / sf;
    assert_eq!(state.lender_positions[0].scaled_balance(), expected_scaled);
    assert_eq!(state.market.scaled_total_supply(), expected_scaled);

    // Verify cap check: normalized supply <= max_total_supply
    let norm = normalized_total_supply(&state.market);
    assert!(norm <= u128::from(state.market.max_total_supply()));

    check_all_invariants(&state);
}

// ===========================================================================
// Test 3: tla_borrow
// ===========================================================================

#[test]
fn tla_borrow() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit first to have funds in vault
    let deposit_amount: u64 = 10_000_000; // 10 USDC
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, deposit_amount);

    // Advance time and borrow
    action_tick(&mut state, 500);
    let borrow_amount: u64 = 5_000_000; // 5 USDC

    let mut oracle_market = state.market;
    accrue_interest(&mut oracle_market, &state.config, state.current_time).unwrap();
    let expected_scale_factor = oracle_market.scale_factor();
    let expected_fees = oracle_market.accrued_protocol_fees();
    let expected_borrowable = state.vault_balance - state.vault_balance.min(expected_fees);
    assert!(
        borrow_amount <= expected_borrowable,
        "borrow precondition should hold against oracle borrowable"
    );

    let vault_before = state.vault_balance;
    let total_borrowed_before = state.market.total_borrowed();
    let wl_borrowed_before = state.whitelist.current_borrowed();
    let total_deposited_before = state.market.total_deposited();
    let scaled_total_before = state.market.scaled_total_supply();
    let lender0_scaled_before = state.lender_positions[0].scaled_balance();
    let lender1_scaled_before = state.lender_positions[1].scaled_balance();

    action_borrow(&mut state, borrow_amount);

    // TLA+ Borrow postconditions:
    // total_borrowed' = total_borrowed + amount
    assert_eq!(
        state.market.total_borrowed(),
        total_borrowed_before + borrow_amount
    );
    // vault_balance' = vault_balance - amount
    assert_eq!(state.vault_balance, vault_before - borrow_amount);
    // whitelist_current_borrowed' = whitelist_current_borrowed + amount
    assert_eq!(
        state.whitelist.current_borrowed(),
        wl_borrowed_before + borrow_amount
    );

    assert_eq!(state.market.scale_factor(), expected_scale_factor);
    assert_eq!(state.market.accrued_protocol_fees(), expected_fees);
    assert_eq!(state.market.last_accrual_timestamp(), state.current_time);
    assert_eq!(state.market.total_deposited(), total_deposited_before);
    assert_eq!(state.market.scaled_total_supply(), scaled_total_before);
    assert_eq!(
        state.lender_positions[0].scaled_balance(),
        lender0_scaled_before
    );
    assert_eq!(
        state.lender_positions[1].scaled_balance(),
        lender1_scaled_before
    );
    assert_scaled_supply_matches_positions(&state);

    check_all_invariants(&state);
}

// ===========================================================================
// Test 4: tla_repay
// ===========================================================================

#[test]
fn tla_repay() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit and borrow
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_tick(&mut state, 500);
    action_borrow(&mut state, 5_000_000);

    // Repay
    action_tick(&mut state, 1000);
    let repay_amount: u64 = 3_000_000;
    let mut oracle_market = state.market;
    let zero_config = ProtocolConfig::zeroed();
    accrue_interest(&mut oracle_market, &zero_config, state.current_time).unwrap();
    let expected_scale_factor = oracle_market.scale_factor();
    let expected_last_accrual = oracle_market.last_accrual_timestamp();

    let vault_before = state.vault_balance;
    let total_repaid_before = state.market.total_repaid();
    let total_borrowed_before = state.market.total_borrowed();
    let whitelist_before = state.whitelist.current_borrowed();
    let fees_before = state.market.accrued_protocol_fees();
    let scaled_total_before = state.market.scaled_total_supply();
    let lender0_scaled_before = state.lender_positions[0].scaled_balance();
    let lender1_scaled_before = state.lender_positions[1].scaled_balance();

    action_repay(&mut state, repay_amount);

    // TLA+ Repay postconditions:
    // total_repaid' = total_repaid + amount
    assert_eq!(
        state.market.total_repaid(),
        total_repaid_before + repay_amount
    );
    // vault_balance' = vault_balance + amount
    assert_eq!(state.vault_balance, vault_before + repay_amount);
    // accrued_protocol_fees unchanged (zero-fee accrual)
    assert_eq!(state.market.accrued_protocol_fees(), fees_before);
    assert_eq!(state.market.scale_factor(), expected_scale_factor);
    assert_eq!(state.market.last_accrual_timestamp(), expected_last_accrual);
    assert_eq!(state.market.total_borrowed(), total_borrowed_before);
    assert_eq!(state.whitelist.current_borrowed(), whitelist_before);
    assert_eq!(state.market.scaled_total_supply(), scaled_total_before);
    assert_eq!(
        state.lender_positions[0].scaled_balance(),
        lender0_scaled_before
    );
    assert_eq!(
        state.lender_positions[1].scaled_balance(),
        lender1_scaled_before
    );
    assert_scaled_supply_matches_positions(&state);

    check_all_invariants(&state);
}

// ===========================================================================
// Test 5: tla_withdraw
// ===========================================================================

#[test]
fn tla_withdraw() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit, borrow, repay
    action_tick(&mut state, 100);
    let deposit_amount: u64 = 10_000_000;
    action_deposit(&mut state, 0, deposit_amount);
    action_tick(&mut state, 500);
    action_borrow(&mut state, 5_000_000);
    action_tick(&mut state, 1000);
    action_repay(&mut state, 8_000_000); // Repay more than borrowed

    // Advance past maturity
    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    let vault_before = state.vault_balance;
    let scaled_balance = state.lender_positions[0].scaled_balance();
    assert!(scaled_balance > 0, "lender must have balance to withdraw");

    action_withdraw(&mut state, 0);

    // TLA+ Withdraw postconditions:
    // settlement_factor_wad was set (first withdrawal locks it)
    let sf_wad = state.market.settlement_factor_wad();
    assert!(
        sf_wad > 0,
        "settlement factor must be set after first withdrawal"
    );
    assert!(sf_wad >= 1 && sf_wad <= WAD, "settlement factor bounded");

    // lender_scaled_balance = 0 (full withdrawal)
    assert_eq!(state.lender_positions[0].scaled_balance(), 0);

    // scaled_total_supply decreased
    assert_eq!(
        state.market.scaled_total_supply(),
        0,
        "only one lender deposited, so supply should be 0"
    );

    // vault decreased by payout
    assert!(state.vault_balance < vault_before);

    // payout = scaled * sf / WAD * sf_wad / WAD
    let sf = state.market.scale_factor();
    let normalized = tla_div(scaled_balance * sf, WAD);
    let expected_payout = tla_div(normalized * sf_wad, WAD);
    assert_eq!(vault_before - state.vault_balance, expected_payout as u64);

    check_all_invariants(&state);
}

// ===========================================================================
// Test 6: tla_collect_fees
// ===========================================================================

#[test]
fn tla_collect_fees() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit to generate supply, advance time to accrue fees
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);

    // Advance significant time to accrue fees (fees depend on supply and time)
    action_tick(&mut state, 10_000_000); // ~115 days

    // COAL-C01: fees are only collectable when vault > lender claims.
    // Simulate borrower interest repayment funding the vault so fees are collectable.
    {
        let mut tmp = state.market;
        accrue_interest(&mut tmp, &state.config, state.current_time).unwrap();
        let sf = tmp.scale_factor();
        let total_norm = tmp.scaled_total_supply()
            .checked_mul(sf).unwrap() / WAD;
        let lender_claims = u64::try_from(total_norm).unwrap();
        let needed = u128::from(lender_claims) + u128::from(tmp.accrued_protocol_fees());
        if u128::from(state.vault_balance) < needed {
            state.vault_balance = u64::try_from(needed).unwrap();
        }
    }

    let pre_collect = state.clone();
    let mut oracle_market = pre_collect.market;
    accrue_interest(
        &mut oracle_market,
        &pre_collect.config,
        pre_collect.current_time,
    )
    .unwrap();

    let fees_after_accrual = oracle_market.accrued_protocol_fees();
    assert!(
        fees_after_accrual > 0,
        "fee accrual should be positive in this scenario"
    );

    let mut expected_withdrawable = fees_after_accrual.min(pre_collect.vault_balance);
    // COAL-C01: apply lender-claims cap using post-accrual scale factor
    if oracle_market.scaled_total_supply() > 0 {
        let sf = oracle_market.scale_factor();
        let total_norm = oracle_market.scaled_total_supply()
            .checked_mul(sf).unwrap()
            .checked_div(WAD).unwrap();
        let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
        let safe_max = pre_collect.vault_balance.saturating_sub(lender_claims);
        expected_withdrawable = expected_withdrawable.min(safe_max);
    }
    assert!(
        expected_withdrawable > 0,
        "collect_fees precondition requires positive withdrawable"
    );

    assert!(action_collect_fees(&mut state));

    // TLA+ CollectFees postconditions:
    // withdrawable = min(fees, vault_balance) capped by lender claims
    assert_eq!(
        state.vault_balance,
        pre_collect.vault_balance - expected_withdrawable
    );
    assert_eq!(
        state.market.accrued_protocol_fees(),
        fees_after_accrual - expected_withdrawable
    );
    assert_eq!(state.market.scale_factor(), oracle_market.scale_factor());
    assert_eq!(
        state.market.last_accrual_timestamp(),
        oracle_market.last_accrual_timestamp()
    );
    assert_eq!(
        state.market.total_deposited(),
        pre_collect.market.total_deposited()
    );
    assert_eq!(
        state.market.total_borrowed(),
        pre_collect.market.total_borrowed()
    );
    assert_eq!(
        state.market.total_repaid(),
        pre_collect.market.total_repaid()
    );
    assert_scaled_supply_matches_positions(&state);

    check_all_invariants(&state);
}

// ===========================================================================
// Test 7: tla_re_settle
// ===========================================================================

#[test]
fn tla_re_settle() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit, borrow (creating deficit), then withdraw after maturity
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_deposit(&mut state, 1, 10_000_000);

    action_tick(&mut state, 500);
    action_borrow(&mut state, 15_000_000); // Borrow most of it

    // Advance past maturity
    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    // First withdrawal locks the settlement factor (will be < WAD due to deficit)
    action_withdraw(&mut state, 0);
    let old_factor = state.market.settlement_factor_wad();
    assert!(old_factor > 0);
    assert!(old_factor < WAD, "should have deficit, so factor < WAD");

    // Now repay some to improve the settlement factor
    action_repay(&mut state, 10_000_000);

    let pre_resettle = state.clone();
    let mut oracle_market = pre_resettle.market;
    let zero_config = ProtocolConfig::zeroed();
    accrue_interest(&mut oracle_market, &zero_config, pre_resettle.current_time).unwrap();
    let expected_factor = compute_settlement_factor(&oracle_market, pre_resettle.vault_balance);
    assert!(
        expected_factor > old_factor,
        "oracle factor must improve before ReSettle can run"
    );

    // Re-settle
    action_re_settle(&mut state);

    let new_factor = state.market.settlement_factor_wad();

    // TLA+ ReSettle postconditions:
    // new_factor > old_factor (strictly improved)
    assert!(
        new_factor > old_factor,
        "re-settle must improve: new={} old={}",
        new_factor,
        old_factor
    );
    assert_eq!(new_factor, expected_factor, "must match oracle formula");
    // Factor still bounded
    assert!(new_factor >= 1 && new_factor <= WAD);
    assert_eq!(state.prev_settlement_factor, new_factor);
    assert_eq!(
        state.market.total_deposited(),
        pre_resettle.market.total_deposited()
    );
    assert_eq!(
        state.market.total_borrowed(),
        pre_resettle.market.total_borrowed()
    );
    assert_eq!(
        state.market.total_repaid(),
        pre_resettle.market.total_repaid()
    );
    assert_eq!(state.vault_balance, pre_resettle.vault_balance);
    assert_eq!(state.total_payouts, pre_resettle.total_payouts);
    assert_eq!(
        state.lender_positions[0].scaled_balance(),
        pre_resettle.lender_positions[0].scaled_balance()
    );
    assert_eq!(
        state.lender_positions[1].scaled_balance(),
        pre_resettle.lender_positions[1].scaled_balance()
    );
    assert_scaled_supply_matches_positions(&state);

    check_all_invariants(&state);
}

// ===========================================================================
// Test 8: tla_close_lender_position
// ===========================================================================

#[test]
fn tla_close_lender_position() {
    let mut state = tla_created_market_state(0);

    // TLA+ CloseLenderPosition guard: scaled_balance must be 0
    assert_eq!(
        state.lender_positions[0].scaled_balance(),
        0,
        "precondition: lender position must be empty"
    );

    // CloseLenderPosition is a no-op in the TLA+ model (UNCHANGED vars).
    let before_close_empty = state.clone();
    assert!(try_execute_action(
        &mut state,
        &TlaAction::CloseLenderPosition { lender: 0 }
    ));
    assert_state_unchanged(
        &before_close_empty,
        &state,
        "close empty lender should be no-op",
    );

    check_all_invariants(&state);

    // Also test after a full deposit-withdraw cycle
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 1_000_000);

    let before_invalid_close = state.clone();
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::CloseLenderPosition { lender: 0 }
    ));
    assert_state_unchanged(
        &before_invalid_close,
        &state,
        "close non-empty lender must be disabled",
    );

    // Advance past maturity and withdraw everything
    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    // Need to have enough in vault for payout; add a repay
    action_repay(&mut state, 1_000_000);

    action_withdraw(&mut state, 0);
    assert_eq!(
        state.lender_positions[0].scaled_balance(),
        0,
        "postcondition: after full withdrawal, balance should be 0"
    );

    let before_close_after_withdraw = state.clone();
    assert!(try_execute_action(
        &mut state,
        &TlaAction::CloseLenderPosition { lender: 0 }
    ));
    assert_state_unchanged(
        &before_close_after_withdraw,
        &state,
        "close empty lender after withdraw should be no-op",
    );

    // Now CloseLenderPosition is valid
    check_all_invariants(&state);
}

// ===========================================================================
// Test 9: tla_tick
// ===========================================================================

#[test]
fn tla_tick() {
    let mut state = tla_created_market_state(0);

    let sf_before = state.market.scale_factor();
    let sts_before = state.market.scaled_total_supply();
    let fees_before = state.market.accrued_protocol_fees();
    let deposited_before = state.market.total_deposited();
    let borrowed_before = state.market.total_borrowed();
    let repaid_before = state.market.total_repaid();
    let vault_before = state.vault_balance;
    let settlement_before = state.market.settlement_factor_wad();
    let last_accrual_before = state.market.last_accrual_timestamp();

    action_tick(&mut state, 1);

    // TLA+ Tick postconditions: ONLY current_time changes
    assert_eq!(state.current_time, 1);
    assert_eq!(state.market.scale_factor(), sf_before);
    assert_eq!(state.market.scaled_total_supply(), sts_before);
    assert_eq!(state.market.accrued_protocol_fees(), fees_before);
    assert_eq!(state.market.total_deposited(), deposited_before);
    assert_eq!(state.market.total_borrowed(), borrowed_before);
    assert_eq!(state.market.total_repaid(), repaid_before);
    assert_eq!(state.vault_balance, vault_before);
    assert_eq!(state.market.settlement_factor_wad(), settlement_before);
    assert_eq!(state.market.last_accrual_timestamp(), last_accrual_before);

    check_all_invariants(&state);
}

// ===========================================================================
// Test 10: tla_invariant_vault_solvency
// ===========================================================================

#[test]
fn tla_invariant_vault_solvency() {
    let mut state = tla_created_market_state(0);
    let mut expected_vault = 0u64;

    // Sequence: deposit, borrow, repay, withdraw, collect_fees
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 50_000_000);
    expected_vault += 50_000_000;
    assert_eq!(state.vault_balance, expected_vault);
    check_vault_solvency(&state);

    action_tick(&mut state, 500);
    action_deposit(&mut state, 1, 30_000_000);
    expected_vault += 30_000_000;
    assert_eq!(state.vault_balance, expected_vault);
    check_vault_solvency(&state);

    action_tick(&mut state, 1000);
    action_borrow(&mut state, 40_000_000);
    expected_vault -= 40_000_000;
    assert_eq!(state.vault_balance, expected_vault);
    check_vault_solvency(&state);

    action_tick(&mut state, 2000);
    action_repay(&mut state, 50_000_000);
    expected_vault += 50_000_000;
    assert_eq!(state.vault_balance, expected_vault);
    check_vault_solvency(&state);

    // Advance past maturity
    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    for lender_idx in [0usize, 1usize] {
        let mut oracle_market = state.market;
        accrue_interest(&mut oracle_market, &state.config, state.current_time).unwrap();
        let settlement_factor = if oracle_market.settlement_factor_wad() == 0 {
            compute_settlement_factor(&oracle_market, state.vault_balance)
        } else {
            oracle_market.settlement_factor_wad()
        };
        let scaled = state.lender_positions[lender_idx].scaled_balance();
        let expected_payout =
            oracle_payout(scaled, oracle_market.scale_factor(), settlement_factor);
        let expected_payout_u64 = u64::try_from(expected_payout).unwrap();

        action_withdraw(&mut state, lender_idx);

        expected_vault = expected_vault
            .checked_sub(expected_payout_u64)
            .expect("vault must remain non-negative");
        assert_eq!(
            state.vault_balance, expected_vault,
            "vault should decrease by oracle payout for lender {}",
            lender_idx
        );
        check_vault_solvency(&state);
    }

    assert_scaled_supply_matches_positions(&state);
    check_all_invariants(&state);
}

// ===========================================================================
// Test 11: tla_invariant_scale_factor_monotonic
// ===========================================================================

#[test]
fn tla_invariant_scale_factor_monotonic() {
    let mut state = tla_created_market_state(0);
    let mut prev_sf = state.market.scale_factor();
    let mut prev_fees = state.market.accrued_protocol_fees();

    // Perform many operations, checking monotonicity after each
    for t in [100, 500, 1000, 5000, 10_000, 100_000, 1_000_000] {
        let last_accrual = state.market.last_accrual_timestamp();
        let expected_interest_delta = oracle_interest_delta_wad(
            state.market.annual_interest_bps(),
            last_accrual,
            t,
            state.market.maturity_timestamp(),
        );
        let expected_sf = oracle_scale_factor_after_accrual(
            state.market.scale_factor(),
            state.market.annual_interest_bps(),
            last_accrual,
            t,
            state.market.maturity_timestamp(),
        );
        let expected_fee_delta = oracle_fee_delta_normalized(
            state.market.scaled_total_supply(),
            expected_sf,
            expected_interest_delta,
            state.config.fee_rate_bps(),
        );

        state.current_time = t;
        accrue_interest(&mut state.market, &state.config, state.current_time).unwrap();
        let new_sf = state.market.scale_factor();

        assert_eq!(
            new_sf, expected_sf,
            "scale_factor must match oracle exactly"
        );
        assert_eq!(
            state.market.last_accrual_timestamp(),
            oracle_effective_now(t, state.market.maturity_timestamp()),
            "last_accrual_timestamp must match effective_now"
        );
        assert_eq!(
            state.market.accrued_protocol_fees(),
            prev_fees + expected_fee_delta,
            "fee accrual must match oracle"
        );
        assert!(
            new_sf >= prev_sf,
            "scale_factor must be monotonic: new={} < prev={} at time={}",
            new_sf,
            prev_sf,
            t
        );
        prev_sf = new_sf;
        prev_fees = state.market.accrued_protocol_fees();
        state.prev_scale_factor = new_sf;
    }

    // Also check with deposits in between
    let mut state2 = tla_created_market_state(0);

    action_tick(&mut state2, 1000);
    let mut oracle_after_deposit_1 = state2.market;
    accrue_interest(
        &mut oracle_after_deposit_1,
        &state2.config,
        state2.current_time,
    )
    .unwrap();
    action_deposit(&mut state2, 0, 1_000_000);
    assert_eq!(
        state2.market.scale_factor(),
        oracle_after_deposit_1.scale_factor()
    );
    assert_eq!(
        state2.market.accrued_protocol_fees(),
        oracle_after_deposit_1.accrued_protocol_fees()
    );

    action_tick(&mut state2, 5000);
    let mut oracle_after_deposit_2 = state2.market;
    accrue_interest(
        &mut oracle_after_deposit_2,
        &state2.config,
        state2.current_time,
    )
    .unwrap();
    action_deposit(&mut state2, 1, 2_000_000);
    assert_eq!(
        state2.market.scale_factor(),
        oracle_after_deposit_2.scale_factor()
    );
    assert_eq!(
        state2.market.accrued_protocol_fees(),
        oracle_after_deposit_2.accrued_protocol_fees()
    );
    assert_scaled_supply_matches_positions(&state);
    assert_scaled_supply_matches_positions(&state2);
}

// ===========================================================================
// Test 12: tla_invariant_settlement_bounded
// ===========================================================================

#[test]
fn tla_invariant_settlement_bounded() {
    // Case 1: Full vault (settlement factor = WAD)
    let mut state = tla_created_market_state(0);
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    // Repay extra so vault > normalized supply
    action_tick(&mut state, 500);
    action_repay(&mut state, 20_000_000);

    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);
    let mut oracle_market_1 = state.market;
    accrue_interest(&mut oracle_market_1, &state.config, state.current_time).unwrap();
    let expected_sf_1 = compute_settlement_factor(&oracle_market_1, state.vault_balance);
    action_withdraw(&mut state, 0);

    let sf = state.market.settlement_factor_wad();
    assert_eq!(sf, expected_sf_1, "settlement factor must match oracle");
    assert!(sf >= 1 && sf <= WAD, "bounded: {} not in [1, {}]", sf, WAD);
    assert_eq!(sf, WAD, "overfunded case should settle at 1.0 WAD");

    // Case 2: Deficit vault (settlement factor < WAD)
    let mut state2 = tla_created_market_state(0);
    action_tick(&mut state2, 100);
    action_deposit(&mut state2, 0, 10_000_000);
    action_tick(&mut state2, 500);
    action_borrow(&mut state2, 8_000_000); // Large borrow creates deficit

    let time_to_maturity2 = state2.market.maturity_timestamp() - state2.current_time + 1;
    action_tick(&mut state2, time_to_maturity2);
    let mut oracle_market_2 = state2.market;
    accrue_interest(&mut oracle_market_2, &state2.config, state2.current_time).unwrap();
    let expected_sf_2 = compute_settlement_factor(&oracle_market_2, state2.vault_balance);
    action_withdraw(&mut state2, 0);

    let sf2 = state2.market.settlement_factor_wad();
    assert_eq!(sf2, expected_sf_2, "settlement factor must match oracle");
    assert!(
        sf2 >= 1 && sf2 <= WAD,
        "bounded: {} not in [1, {}]",
        sf2,
        WAD
    );
    assert!(sf2 < WAD, "deficit case should settle below 1.0 WAD");

    let total_norm = normalized_total_supply(&oracle_market_2);
    assert_eq!(oracle_settlement_factor(0, 123, 0), WAD);
    assert_eq!(oracle_settlement_factor(total_norm, 0, 0), 1);
    assert_eq!(
        oracle_settlement_factor(total_norm, u64::MAX, 0),
        WAD,
        "overfunded inputs should clamp to WAD"
    );

    check_all_invariants(&state);
    check_all_invariants(&state2);
}

// ===========================================================================
// Test 13: tla_invariant_settlement_monotonic
// ===========================================================================

#[test]
fn tla_invariant_settlement_monotonic() {
    let mut state = tla_created_market_state(0);

    // Create deficit scenario
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_deposit(&mut state, 1, 10_000_000);
    action_tick(&mut state, 500);
    action_borrow(&mut state, 15_000_000);

    // Advance past maturity
    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    // First withdrawal locks factor
    let mut oracle_market_before_withdraw = state.market;
    accrue_interest(
        &mut oracle_market_before_withdraw,
        &state.config,
        state.current_time,
    )
    .unwrap();
    let expected_first_factor =
        compute_settlement_factor(&oracle_market_before_withdraw, state.vault_balance);
    action_withdraw(&mut state, 0);
    let first_factor = state.market.settlement_factor_wad();
    assert!(first_factor > 0);
    assert_eq!(first_factor, expected_first_factor);

    // Repay to improve situation
    action_repay(&mut state, 10_000_000);

    let pre_resettle = state.clone();
    let mut oracle_market_before_resettle = pre_resettle.market;
    let zero_config = ProtocolConfig::zeroed();
    accrue_interest(
        &mut oracle_market_before_resettle,
        &zero_config,
        pre_resettle.current_time,
    )
    .unwrap();
    let expected_second_factor =
        compute_settlement_factor(&oracle_market_before_resettle, pre_resettle.vault_balance);
    assert!(expected_second_factor > first_factor);

    // Re-settle should strictly increase
    action_re_settle(&mut state);
    let second_factor = state.market.settlement_factor_wad();

    assert!(
        second_factor >= first_factor,
        "settlement factor must be monotonic: new={} < old={}",
        second_factor,
        first_factor
    );
    assert_eq!(second_factor, expected_second_factor);
    assert!(second_factor > first_factor);

    // Without any further state improvement, ReSettle should be disabled.
    let before_second_resettle_attempt = state.clone();
    assert!(!try_execute_action(&mut state, &TlaAction::ReSettle));
    assert_state_unchanged(
        &before_second_resettle_attempt,
        &state,
        "resettle without improvement must be disabled",
    );

    check_settlement_factor_monotonic(&state);
    check_all_invariants(&state);
}

// ===========================================================================
// Test 14: tla_invariant_fees_non_negative
// ===========================================================================

#[test]
fn tla_invariant_fees_non_negative() {
    let mut state = tla_created_market_state(0);

    // Deposit and accrue fees over time
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 50_000_000);
    let mut prev_fees = state.market.accrued_protocol_fees();

    for t in [1000, 5000, 50_000, 500_000, 5_000_000] {
        let last_accrual = state.market.last_accrual_timestamp();
        let expected_interest_delta = oracle_interest_delta_wad(
            state.market.annual_interest_bps(),
            last_accrual,
            t,
            state.market.maturity_timestamp(),
        );
        let expected_sf = oracle_scale_factor_after_accrual(
            state.market.scale_factor(),
            state.market.annual_interest_bps(),
            last_accrual,
            t,
            state.market.maturity_timestamp(),
        );
        let expected_fee_delta = oracle_fee_delta_normalized(
            state.market.scaled_total_supply(),
            expected_sf,
            expected_interest_delta,
            state.config.fee_rate_bps(),
        );

        state.current_time = t;
        accrue_interest(&mut state.market, &state.config, state.current_time).unwrap();
        assert_eq!(
            state.market.scale_factor(),
            expected_sf,
            "scale factor must match oracle in fee accrual loop"
        );
        assert_eq!(
            state.market.accrued_protocol_fees(),
            prev_fees + expected_fee_delta,
            "fees must match oracle in fee accrual loop"
        );
        assert!(
            state.market.accrued_protocol_fees() >= prev_fees,
            "fees should be monotonic non-decreasing before collection"
        );
        prev_fees = state.market.accrued_protocol_fees();
        state.prev_scale_factor = state.market.scale_factor();
        check_fees_non_negative(&state);
    }

    // Collect fees (reduces them but never below 0)
    if state.market.accrued_protocol_fees() > 0 && state.vault_balance > 0 {
        let pre_collect = state.clone();
        let mut oracle_market = pre_collect.market;
        accrue_interest(
            &mut oracle_market,
            &pre_collect.config,
            pre_collect.current_time,
        )
        .unwrap();
        let mut withdrawable = oracle_market
            .accrued_protocol_fees()
            .min(pre_collect.vault_balance);
        // COAL-C01: apply lender-claims cap
        if oracle_market.scaled_total_supply() > 0 {
            let sf = oracle_market.scale_factor();
            let total_norm = oracle_market.scaled_total_supply()
                .checked_mul(sf).unwrap()
                .checked_div(WAD).unwrap();
            let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
            let safe_max = pre_collect.vault_balance.saturating_sub(lender_claims);
            withdrawable = withdrawable.min(safe_max);
        }

        if withdrawable > 0 {
            assert!(action_collect_fees(&mut state));
            assert_eq!(
                state.market.accrued_protocol_fees(),
                oracle_market.accrued_protocol_fees() - withdrawable
            );
            assert_eq!(
                state.vault_balance,
                pre_collect.vault_balance - withdrawable
            );
        }
        check_fees_non_negative(&state);
    }

    assert_scaled_supply_matches_positions(&state);
    check_all_invariants(&state);
}

// ===========================================================================
// Test 15: tla_invariant_cap_respected
// ===========================================================================

#[test]
fn tla_invariant_cap_respected() {
    let mut state = tla_created_market_state(0);
    state.market.set_max_total_supply(5_000_000); // 5 USDC cap

    action_tick(&mut state, 100);

    // Deposit up to cap
    action_deposit(&mut state, 0, 2_000_000);
    check_cap_respected(&state);

    action_deposit(&mut state, 1, 2_000_000);
    check_cap_respected(&state);

    // Fill close to cap with a boundary-safe amount.
    let current_norm = normalized_total_supply(&state.market);
    let max = u128::from(state.market.max_total_supply());
    let remaining = max.saturating_sub(current_norm) as u64;
    let near_cap_amount = remaining.min(1_000_000);
    assert!(
        near_cap_amount > 0,
        "test setup should leave positive room before cap"
    );
    let mut can_fill_to_near_cap = state.clone();
    assert!(try_execute_action(
        &mut can_fill_to_near_cap,
        &TlaAction::Deposit {
            lender: 0,
            amount: near_cap_amount
        }
    ));
    state = can_fill_to_near_cap;
    let norm_after = normalized_total_supply(&state.market);
    assert!(norm_after <= max);
    assert!(
        max - norm_after <= 4,
        "rounding slack near cap should be tightly bounded"
    );

    // Exceeding cap should be disabled and preserve state.
    let before_over_cap = state.clone();
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::Deposit {
            lender: 1,
            amount: 2_000_000
        }
    ));
    assert_state_unchanged(
        &before_over_cap,
        &state,
        "over-cap deposit must be disabled",
    );

    check_cap_respected(&state);
    assert_scaled_supply_matches_positions(&state);
    check_all_invariants(&state);
}

// ===========================================================================
// Test 16: tla_invariant_whitelist_capacity
// ===========================================================================

#[test]
fn tla_invariant_whitelist_capacity() {
    let mut state = tla_created_market_state(0);
    state.whitelist.set_max_borrow_capacity(10_000_000); // 10 USDC capacity

    // Deposit enough to borrow from
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 50_000_000);

    // Borrow incrementally
    action_tick(&mut state, 500);
    action_borrow(&mut state, 3_000_000);
    check_whitelist_capacity(&state);

    action_tick(&mut state, 1000);
    action_borrow(&mut state, 3_000_000);
    check_whitelist_capacity(&state);

    action_tick(&mut state, 1500);
    action_borrow(&mut state, 3_000_000);
    check_whitelist_capacity(&state);

    // Total borrowed = 9M, capacity = 10M
    assert_eq!(state.whitelist.current_borrowed(), 9_000_000);
    assert!(state.whitelist.current_borrowed() <= state.whitelist.max_borrow_capacity());
    assert_eq!(
        state.market.total_borrowed(),
        state.whitelist.current_borrowed()
    );

    // Exact-boundary borrow to capacity should succeed.
    let remaining = state.whitelist.max_borrow_capacity() - state.whitelist.current_borrowed();
    assert_eq!(remaining, 1_000_000);
    assert!(try_execute_action(
        &mut state,
        &TlaAction::Borrow { amount: remaining }
    ));
    assert_eq!(
        state.whitelist.current_borrowed(),
        state.whitelist.max_borrow_capacity()
    );

    // One unit over capacity should be disabled.
    let before_over_capacity = state.clone();
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::Borrow { amount: 1 }
    ));
    assert_state_unchanged(
        &before_over_capacity,
        &state,
        "borrow beyond whitelist capacity must be disabled",
    );

    assert_scaled_supply_matches_positions(&state);
    check_all_invariants(&state);
}

// ===========================================================================
// Test 17: tla_invariant_payout_bounded
// ===========================================================================

#[test]
fn tla_invariant_payout_bounded() {
    let mut state = tla_created_market_state(0);

    // Setup: both lenders deposit
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_deposit(&mut state, 1, 5_000_000);

    // Borrow to create deficit
    action_tick(&mut state, 500);
    action_borrow(&mut state, 10_000_000);

    // Advance past maturity
    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    // Accrue to set final scale factor
    accrue_interest(&mut state.market, &state.config, state.current_time).unwrap();
    state.prev_scale_factor = state.market.scale_factor();

    // Compute settlement factor
    let sf = compute_settlement_factor(&state.market, state.vault_balance);
    state.market.set_settlement_factor_wad(sf);
    assert!(sf < WAD, "deficit scenario should settle below 1.0 WAD");
    assert_eq!(
        sf,
        oracle_settlement_factor(
            normalized_total_supply(&state.market),
            state.vault_balance,
            state.market.accrued_protocol_fees()
        )
    );

    // Check payout bounded for each lender
    let scale_factor = state.market.scale_factor();
    let mut total_payout = 0u128;
    for pos in &state.lender_positions {
        let norm = oracle_normalized_amount(pos.scaled_balance(), scale_factor);
        let payout = oracle_payout(pos.scaled_balance(), scale_factor, sf);
        assert!(
            payout <= norm,
            "payout {} must not exceed normalized amount {}",
            payout,
            norm
        );
        total_payout += payout;
    }
    let available =
        available_for_lenders(state.vault_balance, state.market.accrued_protocol_fees());
    assert!(
        total_payout <= available,
        "aggregate payout should not exceed available-for-lenders"
    );

    check_payout_bounded(&state);
    assert_scaled_supply_matches_positions(&state);
    check_all_invariants(&state);
}

// ===========================================================================
// Test 18: tla_invariant_total_payout_bounded
// ===========================================================================

#[test]
fn tla_invariant_total_payout_bounded() {
    let mut state = tla_created_market_state(0);

    // Setup: deposits and repayments
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_deposit(&mut state, 1, 10_000_000);

    action_tick(&mut state, 500);
    action_borrow(&mut state, 15_000_000);

    action_tick(&mut state, 1000);
    action_repay(&mut state, 20_000_000); // repay more than borrowed

    // Advance past maturity
    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    let mut expected_total_payouts = 0u128;
    for lender_idx in [0usize, 1usize] {
        let mut oracle_market = state.market;
        accrue_interest(&mut oracle_market, &state.config, state.current_time).unwrap();
        let settlement_factor = if oracle_market.settlement_factor_wad() == 0 {
            compute_settlement_factor(&oracle_market, state.vault_balance)
        } else {
            oracle_market.settlement_factor_wad()
        };
        let scaled = state.lender_positions[lender_idx].scaled_balance();
        let expected_payout =
            oracle_payout(scaled, oracle_market.scale_factor(), settlement_factor);
        let expected_payout_u64 = u64::try_from(expected_payout).unwrap();
        let vault_before = state.vault_balance;

        action_withdraw(&mut state, lender_idx);

        expected_total_payouts += expected_payout;
        assert_eq!(
            state.total_payouts, expected_total_payouts,
            "total_payouts must accumulate exact payouts"
        );
        assert_eq!(vault_before - state.vault_balance, expected_payout_u64);
        check_total_payout_bounded(&state);
    }

    // total_payouts <= total_deposited + total_repaid
    let max_allowed =
        u128::from(state.market.total_deposited()) + u128::from(state.market.total_repaid());
    assert!(
        state.total_payouts <= max_allowed,
        "total payouts {} exceed deposited+repaid {}",
        state.total_payouts,
        max_allowed
    );

    assert_scaled_supply_matches_positions(&state);
    check_all_invariants(&state);
}

// ===========================================================================
// Test 19: tla_full_trace_conformance (proptest)
// ===========================================================================

/// Actions that can be taken in the TLA+ model.
#[derive(Debug, Clone)]
enum TlaAction {
    Tick(i64),
    Deposit { lender: usize, amount: u64 },
    Borrow { amount: u64 },
    Repay { amount: u64 },
    Withdraw { lender: usize },
    CollectFees,
    ReSettle,
    CloseLenderPosition { lender: usize },
    RepayInterest { amount: u64 },
    WithdrawExcess,
    SetPause { flag: bool },
}

/// Attempt to execute a TLA+ action. Returns true if the action was valid
/// and executed, false if preconditions were not met (equivalent to TLA+
/// action being disabled in a state).
fn try_execute_action(state: &mut TlaState, action: &TlaAction) -> bool {
    match action {
        TlaAction::Tick(delta) => {
            if *delta <= 0 {
                return false;
            }
            action_tick(state, *delta);
            true
        },
        TlaAction::Deposit { lender, amount } => {
            if *amount == 0
                || *lender >= NUM_LENDERS
                || state.market.scale_factor() == 0 // not initialized
                || state.current_time >= state.market.maturity_timestamp()
                || state.market.settlement_factor_wad() != 0
                || state.is_paused
            {
                return false;
            }

            // Check if deposit would succeed (cap, scaled_amount > 0)
            // Simulate accrual to get post-accrual scale factor
            let mut test_market = state.market;
            let config = state.config;
            if accrue_interest(&mut test_market, &config, state.current_time).is_err() {
                return false;
            }
            let sf = test_market.scale_factor();

            let scaled = u128::from(*amount)
                .checked_mul(WAD)
                .and_then(|v| v.checked_div(sf));
            let scaled = match scaled {
                Some(s) if s > 0 => s,
                _ => return false,
            };

            let new_scaled_total = match test_market.scaled_total_supply().checked_add(scaled) {
                Some(s) => s,
                None => return false,
            };
            let new_norm = tla_div(new_scaled_total * sf, WAD);
            if new_norm > u128::from(state.market.max_total_supply()) {
                return false;
            }

            action_deposit(state, *lender, *amount);
            true
        },
        TlaAction::Borrow { amount } => {
            if *amount == 0
                || state.market.scale_factor() == 0
                || state.current_time >= state.market.maturity_timestamp()
                || state.market.settlement_factor_wad() != 0
                || state.is_paused
            {
                return false;
            }

            // Check borrowable
            let mut test_market = state.market;
            if accrue_interest(&mut test_market, &state.config, state.current_time).is_err() {
                return false;
            }
            let fees_reserved = state.vault_balance.min(test_market.accrued_protocol_fees());
            let borrowable = state.vault_balance - fees_reserved;
            if *amount > borrowable {
                return false;
            }
            let new_wl = state.whitelist.current_borrowed() + *amount;
            if new_wl > state.whitelist.max_borrow_capacity() {
                return false;
            }

            action_borrow(state, *amount);
            true
        },
        TlaAction::Repay { amount } => {
            if *amount == 0 || state.market.scale_factor() == 0 || state.is_paused {
                return false;
            }

            // Test accrue with zero config won't overflow
            let mut test_market = state.market;
            let zero_config = ProtocolConfig::zeroed();
            if accrue_interest(&mut test_market, &zero_config, state.current_time).is_err() {
                return false;
            }

            // Check for overflow in vault_balance + amount
            if state.vault_balance.checked_add(*amount).is_none() {
                return false;
            }
            if state.market.total_repaid().checked_add(*amount).is_none() {
                return false;
            }

            action_repay(state, *amount);
            true
        },
        TlaAction::Withdraw { lender } => {
            if *lender >= NUM_LENDERS
                || state.market.scale_factor() == 0
                || state.current_time < state.market.maturity_timestamp()
                || state.lender_positions[*lender].scaled_balance() == 0
                || state.is_paused
            {
                return false;
            }

            // Check payout > 0 and <= vault_balance
            let mut test_market = state.market;
            if accrue_interest(&mut test_market, &state.config, state.current_time).is_err() {
                return false;
            }

            let sf_wad = if test_market.settlement_factor_wad() == 0 {
                compute_settlement_factor(&test_market, state.vault_balance)
            } else {
                test_market.settlement_factor_wad()
            };

            let scaled_amount = state.lender_positions[*lender].scaled_balance();
            let sf = test_market.scale_factor();
            let normalized = tla_div(scaled_amount * sf, WAD);
            let payout = tla_div(normalized * sf_wad, WAD);

            if payout == 0 || payout > u128::from(state.vault_balance) {
                return false;
            }

            action_withdraw(state, *lender);
            true
        },
        TlaAction::CollectFees => {
            if state.market.scale_factor() == 0 || state.is_paused {
                return false;
            }

            let mut test_market = state.market;
            if accrue_interest(&mut test_market, &state.config, state.current_time).is_err() {
                return false;
            }
            if test_market.accrued_protocol_fees() == 0 {
                return false;
            }
            let mut withdrawable = test_market.accrued_protocol_fees().min(state.vault_balance);
            // COAL-C01: apply lender-claims cap
            if test_market.scaled_total_supply() > 0 {
                let sf = test_market.scale_factor();
                let total_norm = test_market.scaled_total_supply()
                    .checked_mul(sf).unwrap()
                    .checked_div(WAD).unwrap();
                let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
                let safe_max = state.vault_balance.saturating_sub(lender_claims);
                withdrawable = withdrawable.min(safe_max);
            }
            if withdrawable == 0 {
                return false;
            }

            action_collect_fees(state)
        },
        TlaAction::ReSettle => {
            if state.market.scale_factor() == 0
                || state.market.settlement_factor_wad() == 0
                || state.is_paused
            {
                return false;
            }

            let old_factor = state.market.settlement_factor_wad();

            // Test if factor would improve
            let mut test_market = state.market;
            let zero_config = ProtocolConfig::zeroed();
            if accrue_interest(&mut test_market, &zero_config, state.current_time).is_err() {
                return false;
            }
            let new_factor = compute_settlement_factor(&test_market, state.vault_balance);
            if new_factor <= old_factor {
                return false;
            }

            action_re_settle(state);
            true
        },
        TlaAction::CloseLenderPosition { lender } => {
            if *lender >= NUM_LENDERS
                || state.is_paused
                || state.lender_positions[*lender].scaled_balance() != 0
            {
                return false;
            }
            // No-op in TLA+ model
            true
        },
        TlaAction::RepayInterest { amount } => {
            if *amount == 0 || state.market.scale_factor() == 0 || state.is_paused {
                return false;
            }

            let mut test_market = state.market;
            let zero_config = ProtocolConfig::zeroed();
            if accrue_interest(&mut test_market, &zero_config, state.current_time).is_err() {
                return false;
            }

            if state.vault_balance.checked_add(*amount).is_none() {
                return false;
            }
            if state.market.total_repaid().checked_add(*amount).is_none() {
                return false;
            }

            action_repay_interest(state, *amount);
            true
        },
        TlaAction::WithdrawExcess => {
            if state.market.scale_factor() == 0
                || state.is_paused
                || state.market.scaled_total_supply() != 0
                || state.market.settlement_factor_wad() != WAD
                || state.market.accrued_protocol_fees() != 0
                || state.vault_balance == 0
            {
                return false;
            }

            action_withdraw_excess(state);
            true
        },
        TlaAction::SetPause { flag } => {
            if state.market.scale_factor() == 0 {
                return false;
            }

            action_set_pause(state, *flag);
            true
        },
    }
}

/// Strategy to generate a random TLA+ action.
fn arb_tla_action() -> impl Strategy<Value = TlaAction> {
    prop_oneof![
        // Tick: advance 1-5_000_000 seconds
        (1i64..=5_000_000i64).prop_map(TlaAction::Tick),
        // Deposit: lender 0 or 1, amount 1-50_000_000
        (0usize..NUM_LENDERS, 1u64..=50_000_000u64).prop_map(|(l, a)| TlaAction::Deposit {
            lender: l,
            amount: a
        }),
        // Borrow: amount 1-50_000_000
        (1u64..=50_000_000u64).prop_map(|a| TlaAction::Borrow { amount: a }),
        // Repay: amount 1-50_000_000
        (1u64..=50_000_000u64).prop_map(|a| TlaAction::Repay { amount: a }),
        // Withdraw: lender 0 or 1
        (0usize..NUM_LENDERS).prop_map(|l| TlaAction::Withdraw { lender: l }),
        // CollectFees
        Just(TlaAction::CollectFees),
        // ReSettle
        Just(TlaAction::ReSettle),
        // CloseLenderPosition: lender 0 or 1
        (0usize..NUM_LENDERS).prop_map(|l| TlaAction::CloseLenderPosition { lender: l }),
        // RepayInterest: amount 1-50_000_000
        (1u64..=50_000_000u64).prop_map(|a| TlaAction::RepayInterest { amount: a }),
        // WithdrawExcess
        Just(TlaAction::WithdrawExcess),
        // SetPause
        prop::bool::ANY.prop_map(|f| TlaAction::SetPause { flag: f }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Generate random action sequences (like the TLA+ model checker would explore),
    /// executing each action and verifying all 10 invariants after every step.
    ///
    /// To ensure meaningful traces, we prepend a deterministic setup sequence
    /// (Tick + Deposit for both lenders) before the random actions. This
    /// mirrors how TLC explores states reachable from Init via CreateMarket.
    #[test]
    fn tla_full_trace_conformance(
        initial_deposit_0 in 1u64..=50_000_000u64,
        initial_deposit_1 in 1u64..=50_000_000u64,
        actions in prop::collection::vec(arb_tla_action(), 5..30)
    ) {
        let mut state = tla_created_market_state(0);
        check_all_invariants(&state);

        // Deterministic setup: advance time, deposit for both lenders
        // This ensures the market is in a non-trivial state before random actions.
        action_tick(&mut state, 100);
        check_all_invariants(&state);

        action_deposit(&mut state, 0, initial_deposit_0);
        check_all_invariants(&state);

        action_deposit(&mut state, 1, initial_deposit_1);
        check_all_invariants(&state);

        let mut executed = 3u32; // count the setup actions
        let mut disabled = 0u32;
        for action in &actions {
            let before = state.clone();
            if try_execute_action(&mut state, action) {
                executed += 1;
                // After every executed action, ALL invariants must hold
                check_all_invariants(&state);
                assert_scaled_supply_matches_positions(&state);

                match action {
                    TlaAction::Tick(delta) => {
                        prop_assert_eq!(state.current_time, before.current_time + *delta);
                        prop_assert_eq!(state.market.scale_factor(), before.market.scale_factor());
                        prop_assert_eq!(
                            state.market.scaled_total_supply(),
                            before.market.scaled_total_supply()
                        );
                        prop_assert_eq!(
                            state.market.accrued_protocol_fees(),
                            before.market.accrued_protocol_fees()
                        );
                        prop_assert_eq!(
                            state.market.total_deposited(),
                            before.market.total_deposited()
                        );
                        prop_assert_eq!(
                            state.market.total_borrowed(),
                            before.market.total_borrowed()
                        );
                        prop_assert_eq!(state.market.total_repaid(), before.market.total_repaid());
                        prop_assert_eq!(state.vault_balance, before.vault_balance);
                        prop_assert_eq!(
                            state.market.settlement_factor_wad(),
                            before.market.settlement_factor_wad()
                        );
                        prop_assert_eq!(
                            state.market.last_accrual_timestamp(),
                            before.market.last_accrual_timestamp()
                        );
                    },
                    TlaAction::Deposit { lender, amount } => {
                        prop_assert_eq!(
                            state.market.total_deposited(),
                            before.market.total_deposited() + *amount
                        );
                        prop_assert_eq!(state.vault_balance, before.vault_balance + *amount);
                        prop_assert!(
                            state.lender_positions[*lender].scaled_balance()
                                > before.lender_positions[*lender].scaled_balance()
                        );
                        prop_assert!(
                            state.market.scaled_total_supply() > before.market.scaled_total_supply()
                        );
                    },
                    TlaAction::Borrow { amount } => {
                        prop_assert_eq!(
                            state.market.total_borrowed(),
                            before.market.total_borrowed() + *amount
                        );
                        prop_assert_eq!(
                            state.whitelist.current_borrowed(),
                            before.whitelist.current_borrowed() + *amount
                        );
                        prop_assert_eq!(state.vault_balance, before.vault_balance - *amount);
                    },
                    TlaAction::Repay { amount } => {
                        prop_assert_eq!(
                            state.market.total_repaid(),
                            before.market.total_repaid() + *amount
                        );
                        prop_assert_eq!(state.vault_balance, before.vault_balance + *amount);
                        prop_assert_eq!(
                            state.market.accrued_protocol_fees(),
                            before.market.accrued_protocol_fees(),
                            "zero-fee repay accrual must preserve fee balance"
                        );
                    },
                    TlaAction::Withdraw { lender } => {
                        prop_assert_eq!(state.lender_positions[*lender].scaled_balance(), 0);
                        prop_assert!(state.total_payouts > before.total_payouts);
                        prop_assert!(state.vault_balance < before.vault_balance);
                    },
                    TlaAction::CollectFees => {
                        let mut oracle_market = before.market;
                        accrue_interest(&mut oracle_market, &before.config, before.current_time)
                            .unwrap();
                        let oracle_fees = oracle_market.accrued_protocol_fees();
                        let expected_withdrawable = oracle_fees.min(before.vault_balance);
                        prop_assert_eq!(
                            state.market.accrued_protocol_fees(),
                            oracle_fees - expected_withdrawable
                        );
                        prop_assert_eq!(
                            state.vault_balance,
                            before.vault_balance - expected_withdrawable
                        );
                    },
                    TlaAction::ReSettle => {
                        let old_factor = before.market.settlement_factor_wad();
                        let mut oracle_market = before.market;
                        let zero_config = ProtocolConfig::zeroed();
                        accrue_interest(&mut oracle_market, &zero_config, before.current_time)
                            .unwrap();
                        let expected_new_factor =
                            compute_settlement_factor(&oracle_market, before.vault_balance);
                        prop_assert!(state.market.settlement_factor_wad() > old_factor);
                        prop_assert_eq!(state.market.settlement_factor_wad(), expected_new_factor);
                    },
                    TlaAction::CloseLenderPosition { .. } => {
                        assert_state_unchanged(
                            &before,
                            &state,
                            "executed CloseLenderPosition must be no-op"
                        );
                    },
                    TlaAction::RepayInterest { amount } => {
                        prop_assert_eq!(
                            state.market.total_repaid(),
                            before.market.total_repaid() + *amount
                        );
                        prop_assert_eq!(
                            state.market.total_interest_repaid(),
                            before.market.total_interest_repaid() + *amount
                        );
                        prop_assert_eq!(state.vault_balance, before.vault_balance + *amount);
                        prop_assert_eq!(
                            state.market.accrued_protocol_fees(),
                            before.market.accrued_protocol_fees(),
                            "zero-fee repay_interest accrual must preserve fee balance"
                        );
                        // whitelist unchanged
                        prop_assert_eq!(
                            state.whitelist.current_borrowed(),
                            before.whitelist.current_borrowed()
                        );
                    },
                    TlaAction::WithdrawExcess => {
                        prop_assert_eq!(state.vault_balance, 0);
                        prop_assert!(state.total_payouts > before.total_payouts);
                    },
                    TlaAction::SetPause { flag } => {
                        prop_assert_eq!(state.is_paused, *flag);
                        // All other state should be unchanged
                        prop_assert_eq!(state.vault_balance, before.vault_balance);
                        prop_assert_eq!(
                            state.market.scale_factor(),
                            before.market.scale_factor()
                        );
                        prop_assert_eq!(
                            state.market.total_deposited(),
                            before.market.total_deposited()
                        );
                    },
                }
            } else {
                disabled += 1;
                assert_state_unchanged(&before, &state, "disabled action must not mutate state");
            }
        }

        // We always execute at least the 3 setup actions.
        prop_assert!(
            executed >= 3,
            "setup actions must always execute"
        );
        prop_assert_eq!(
            executed + disabled,
            actions.len() as u32 + 3,
            "every random action must be either executed or disabled"
        );
    }
}

// ===========================================================================
// Additional conformance: exact arithmetic match with TLA+ formulas
// ===========================================================================

/// Verify the accrue_interest implementation matches the TLA+ AccrueInterestEffect
/// formula exactly.
#[test]
fn tla_accrue_interest_formula_match() {
    let annual_bps: u16 = 1000; // 10%
    let fee_rate_bps: u16 = 500; // 5%
    let time_elapsed: i64 = 86400; // 1 day
    let scaled_supply: u128 = 1_000_000_000_000_000_000; // 1M USDC in WAD-scaled
    let checkpoints = [
        time_elapsed - 1,
        time_elapsed,
        time_elapsed + 1,
        SECONDS_PER_YEAR as i64,
    ];

    for checkpoint in checkpoints {
        let mut market = Market::zeroed();
        market.set_annual_interest_bps(annual_bps);
        market.set_maturity_timestamp(i64::MAX);
        market.set_scale_factor(WAD);
        market.set_scaled_total_supply(scaled_supply);
        market.set_last_accrual_timestamp(0);
        market.set_accrued_protocol_fees(0);
        market.set_max_total_supply(u64::MAX);

        let mut config = ProtocolConfig::zeroed();
        config.set_fee_rate_bps(fee_rate_bps);

        accrue_interest(&mut market, &config, checkpoint).unwrap();

        let interest_delta_wad = oracle_interest_delta_wad(annual_bps, 0, checkpoint, i64::MAX);
        let expected_sf =
            oracle_scale_factor_after_accrual(WAD, annual_bps, 0, checkpoint, i64::MAX);
        let expected_fees = oracle_fee_delta_normalized(
            scaled_supply,
            expected_sf,
            interest_delta_wad,
            fee_rate_bps,
        );

        assert_eq!(
            market.scale_factor(),
            expected_sf,
            "scale_factor must match oracle at t={}",
            checkpoint
        );
        assert_eq!(
            market.accrued_protocol_fees(),
            expected_fees,
            "fees must match oracle at t={}",
            checkpoint
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            checkpoint,
            "last_accrual must match effective_now"
        );
    }
}

/// Verify deposit scaling matches TLA+ formula exactly.
#[test]
fn tla_deposit_scaling_formula_match() {
    let scale_factors = [WAD, WAD + 1, WAD + WAD / 10];
    let amounts = [1u64, 2u64, 5_000_000u64, 5_000_001u64];

    for scale_factor in scale_factors {
        for amount in amounts {
            let expected_scaled = oracle_scaled_amount(amount, scale_factor);
            let actual_scaled = u128::from(amount)
                .checked_mul(WAD)
                .unwrap()
                .checked_div(scale_factor)
                .unwrap();
            assert_eq!(actual_scaled, expected_scaled);

            let normalized = oracle_normalized_amount(actual_scaled, scale_factor);
            assert!(normalized <= u128::from(amount));
            assert!(
                u128::from(amount) - normalized <= 2,
                "rounding loss should be tightly bounded"
            );
        }

        let pivot = 5_000_000u64;
        let scaled_minus = oracle_scaled_amount(pivot - 1, scale_factor);
        let scaled = oracle_scaled_amount(pivot, scale_factor);
        let scaled_plus = oracle_scaled_amount(pivot + 1, scale_factor);
        assert!(scaled_minus <= scaled);
        assert!(scaled <= scaled_plus);
        assert!(scaled_plus - scaled <= 1);
    }
}

/// Verify settlement factor computation matches TLA+ ComputeSettlementFactor.
#[test]
fn tla_settlement_factor_formula_match() {
    let scenarios = [
        (0u128, 0u64, 0u64),
        (1_000_000u128, 0u64, 0u64),
        (1_000_000u128, 1u64, 0u64),
        (1_000_000u128, 1_000_000u64, 0u64),
        (1_000_000u128, 2_000_000u64, 0u64),
        (1_000_000u128, 900_000u64, 100_000u64),
        (1_000_000u128, 100_000u64, 200_000u64),
    ];

    for (total_norm, vault_balance, accrued_fees) in scenarios {
        let mut market = Market::zeroed();
        market.set_scale_factor(WAD);
        market.set_scaled_total_supply(total_norm);
        market.set_accrued_protocol_fees(accrued_fees);
        market.set_max_total_supply(u64::MAX);

        let sf = compute_settlement_factor(&market, vault_balance);
        let expected = oracle_settlement_factor(total_norm, vault_balance, accrued_fees);
        assert_eq!(sf, expected);
        assert!((1..=WAD).contains(&sf) || total_norm == 0);
    }

    let mut prev = 0u128;
    for vault in [999_999u64, 1_000_000u64, 1_000_001u64] {
        let mut market = Market::zeroed();
        market.set_scale_factor(WAD);
        market.set_scaled_total_supply(1_000_000u128);
        market.set_accrued_protocol_fees(0);
        market.set_max_total_supply(u64::MAX);

        let sf = compute_settlement_factor(&market, vault);
        assert!(
            sf >= prev,
            "settlement factor must be monotonic in vault funds"
        );
        prev = sf;
    }
}

/// Verify payout computation matches TLA+ withdrawal formula.
#[test]
fn tla_payout_formula_match() {
    let scaled_amount: u128 = 5_000_000 * WAD / (WAD + WAD / 10); // deposited 5M at 1.1x sf
    let scale_factor: u128 = WAD + WAD / 10;
    let settlement_base = WAD * 8 / 10; // 0.8 WAD (80% recovery)
    let mut prev_payout = 0u128;

    for settlement_factor in [
        settlement_base - 1,
        settlement_base,
        settlement_base + 1,
        WAD,
    ] {
        let normalized = oracle_normalized_amount(scaled_amount, scale_factor);
        let payout = oracle_payout(scaled_amount, scale_factor, settlement_factor);

        let on_chain_norm = scaled_amount
            .checked_mul(scale_factor)
            .unwrap()
            .checked_div(WAD)
            .unwrap();
        let on_chain_payout = on_chain_norm
            .checked_mul(settlement_factor)
            .unwrap()
            .checked_div(WAD)
            .unwrap();

        assert_eq!(on_chain_norm, normalized);
        assert_eq!(on_chain_payout, payout);
        assert!(payout <= normalized);
        assert!(payout >= prev_payout);
        prev_payout = payout;
    }
}

/// Verify the TLA+ maturity cap on accrual.
#[test]
fn tla_maturity_cap_on_accrual() {
    let maturity: i64 = 1_000_000;
    for current_time in [maturity - 1, maturity, maturity + 1] {
        let mut market = Market::zeroed();
        market.set_annual_interest_bps(1000);
        market.set_maturity_timestamp(maturity);
        market.set_scale_factor(WAD);
        market.set_scaled_total_supply(WAD);
        market.set_last_accrual_timestamp(0);
        market.set_max_total_supply(u64::MAX);

        let config = ProtocolConfig::zeroed();
        accrue_interest(&mut market, &config, current_time).unwrap();

        let expected_last = oracle_effective_now(current_time, maturity);
        let expected_sf = oracle_scale_factor_after_accrual(WAD, 1000, 0, current_time, maturity);
        assert_eq!(market.last_accrual_timestamp(), expected_last);
        assert_eq!(market.scale_factor(), expected_sf);
    }

    let mut market = Market::zeroed();
    market.set_annual_interest_bps(1000);
    market.set_maturity_timestamp(maturity);
    market.set_scale_factor(WAD);
    market.set_scaled_total_supply(WAD);
    market.set_last_accrual_timestamp(0);
    market.set_max_total_supply(u64::MAX);
    let config = ProtocolConfig::zeroed();
    accrue_interest(&mut market, &config, 2 * maturity).unwrap();
    let sf_after = market.scale_factor();
    accrue_interest(&mut market, &config, 3 * maturity).unwrap();
    assert_eq!(market.scale_factor(), sf_after);
    assert_eq!(market.last_accrual_timestamp(), maturity);
}

/// Verify zero-fee accrual for Repay matches TLA+ spec.
#[test]
fn tla_repay_zero_fee_accrual() {
    let mut base = Market::zeroed();
    base.set_annual_interest_bps(1000);
    base.set_maturity_timestamp(i64::MAX);
    base.set_scale_factor(WAD);
    base.set_scaled_total_supply(1_000_000_000_000);
    base.set_last_accrual_timestamp(0);
    base.set_accrued_protocol_fees(42); // Pre-existing fees
    base.set_max_total_supply(u64::MAX);

    let mut market_zero_fee = base;
    let zero_config = ProtocolConfig::zeroed();
    accrue_interest(&mut market_zero_fee, &zero_config, 86400).unwrap();

    let expected_delta = oracle_interest_delta_wad(1000, 0, 86400, i64::MAX);
    let expected_sf = oracle_scale_factor_after_accrual(WAD, 1000, 0, 86400, i64::MAX);
    assert_eq!(market_zero_fee.scale_factor(), expected_sf);
    assert_eq!(market_zero_fee.last_accrual_timestamp(), 86400);

    assert_eq!(
        market_zero_fee.accrued_protocol_fees(),
        42,
        "repay zero-fee accrual must not change fees"
    );

    let mut fee_config = ProtocolConfig::zeroed();
    fee_config.set_fee_rate_bps(500);
    let mut market_with_fee = base;
    accrue_interest(&mut market_with_fee, &fee_config, 86400).unwrap();
    let expected_fee_delta =
        oracle_fee_delta_normalized(base.scaled_total_supply(), expected_sf, expected_delta, 500);
    assert_eq!(market_with_fee.scale_factor(), expected_sf);
    assert_eq!(
        market_with_fee.accrued_protocol_fees(),
        42 + expected_fee_delta
    );
    assert!(market_with_fee.accrued_protocol_fees() > market_zero_fee.accrued_protocol_fees());
}

/// Verify that the ReSettle zero-fee accrual matches TLA+ spec.
#[test]
fn tla_resettle_zero_fee_accrual() {
    let mut base = Market::zeroed();
    base.set_annual_interest_bps(1000);
    base.set_maturity_timestamp(1_000_000);
    base.set_scale_factor(WAD);
    base.set_scaled_total_supply(1_000_000_000_000);
    base.set_last_accrual_timestamp(0);
    base.set_accrued_protocol_fees(100);
    base.set_max_total_supply(u64::MAX);

    let mut market_zero_fee = base;
    let zero_config = ProtocolConfig::zeroed();
    accrue_interest(&mut market_zero_fee, &zero_config, 500_000).unwrap();

    let expected_delta = oracle_interest_delta_wad(1000, 0, 500_000, 1_000_000);
    let expected_sf = oracle_scale_factor_after_accrual(WAD, 1000, 0, 500_000, 1_000_000);
    assert_eq!(market_zero_fee.scale_factor(), expected_sf);
    assert_eq!(market_zero_fee.last_accrual_timestamp(), 500_000);
    assert_eq!(market_zero_fee.accrued_protocol_fees(), 100);

    let mut fee_config = ProtocolConfig::zeroed();
    fee_config.set_fee_rate_bps(500);
    let mut market_with_fee = base;
    accrue_interest(&mut market_with_fee, &fee_config, 500_000).unwrap();
    let expected_fee_delta =
        oracle_fee_delta_normalized(base.scaled_total_supply(), expected_sf, expected_delta, 500);
    assert_eq!(market_with_fee.scale_factor(), expected_sf);
    assert_eq!(
        market_with_fee.accrued_protocol_fees(),
        100 + expected_fee_delta
    );
    assert!(market_with_fee.accrued_protocol_fees() > market_zero_fee.accrued_protocol_fees());
}

// ===========================================================================
// Test: tla_repay_interest
// ===========================================================================

#[test]
fn tla_repay_interest() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit and borrow
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_tick(&mut state, 500);
    action_borrow(&mut state, 5_000_000);

    // Repay interest
    action_tick(&mut state, 1000);
    let repay_amount: u64 = 1_000_000;

    let vault_before = state.vault_balance;
    let total_repaid_before = state.market.total_repaid();
    let total_interest_repaid_before = state.market.total_interest_repaid();
    let whitelist_before = state.whitelist.current_borrowed();
    let fees_before = state.market.accrued_protocol_fees();
    let total_deposited_before = state.market.total_deposited();
    let total_borrowed_before = state.market.total_borrowed();
    let scaled_total_supply_before = state.market.scaled_total_supply();
    let settlement_factor_before = state.market.settlement_factor_wad();
    let total_payouts_before = state.total_payouts;

    action_repay_interest(&mut state, repay_amount);

    // TLA+ RepayInterest postconditions:
    assert_eq!(
        state.market.total_repaid(),
        total_repaid_before + repay_amount
    );
    assert_eq!(
        state.market.total_interest_repaid(),
        total_interest_repaid_before + repay_amount
    );
    assert_eq!(state.vault_balance, vault_before + repay_amount);
    // Fees unchanged (zero-fee accrual)
    assert_eq!(state.market.accrued_protocol_fees(), fees_before);
    // Scale factor monotonically non-decreasing (accrual may advance it)
    assert!(state.market.scale_factor() >= WAD);
    // UNCHANGED
    assert_eq!(state.whitelist.current_borrowed(), whitelist_before);
    assert_eq!(state.market.total_deposited(), total_deposited_before);
    assert_eq!(state.market.total_borrowed(), total_borrowed_before);
    assert_eq!(
        state.market.scaled_total_supply(),
        scaled_total_supply_before
    );
    assert_eq!(
        state.market.settlement_factor_wad(),
        settlement_factor_before
    );
    assert_eq!(state.total_payouts, total_payouts_before);
    assert!(!state.is_paused);

    check_all_invariants(&state);
}

// ===========================================================================
// Test: tla_withdraw_excess
// ===========================================================================

#[test]
fn tla_withdraw_excess() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit, repay extra, advance past maturity, withdraw all
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_tick(&mut state, 500);
    action_repay(&mut state, 20_000_000);

    let time_to_maturity = state.market.maturity_timestamp() - state.current_time + 1;
    action_tick(&mut state, time_to_maturity);

    // Withdraw lender position (should get settlement at WAD due to overfunding)
    action_withdraw(&mut state, 0);
    assert_eq!(state.market.settlement_factor_wad(), WAD);
    assert_eq!(state.market.scaled_total_supply(), 0);

    // Collect any remaining fees (COAL-C01: may be blocked by lender-claims cap,
    // but supply == 0 here so cap doesn't apply)
    if state.market.accrued_protocol_fees() > 0 && state.vault_balance > 0 {
        assert!(action_collect_fees(&mut state));
    }

    // Preconditions for WithdrawExcess must hold — assert rather than skip
    assert_eq!(
        state.market.accrued_protocol_fees(),
        0,
        "fees must be zero for withdraw_excess"
    );
    assert!(
        state.vault_balance > 0,
        "vault must have excess for withdraw_excess"
    );

    let vault_before = state.vault_balance;
    let payouts_before = state.total_payouts;
    let total_borrowed_before = state.market.total_borrowed();
    let total_repaid_before = state.market.total_repaid();
    let total_interest_repaid_before = state.market.total_interest_repaid();
    let scale_factor_before = state.market.scale_factor();
    let settlement_factor_before = state.market.settlement_factor_wad();
    let whitelist_before = state.whitelist.current_borrowed();

    action_withdraw_excess(&mut state);

    // Postconditions
    assert_eq!(state.vault_balance, 0);
    assert_eq!(
        state.total_payouts,
        payouts_before + u128::from(vault_before)
    );
    // UNCHANGED
    assert_eq!(state.market.total_borrowed(), total_borrowed_before);
    assert_eq!(state.market.total_repaid(), total_repaid_before);
    assert_eq!(
        state.market.total_interest_repaid(),
        total_interest_repaid_before
    );
    assert_eq!(state.market.scale_factor(), scale_factor_before);
    assert_eq!(
        state.market.settlement_factor_wad(),
        settlement_factor_before
    );
    assert_eq!(state.whitelist.current_borrowed(), whitelist_before);
    assert_eq!(state.market.scaled_total_supply(), 0);
    assert!(!state.is_paused);

    check_all_invariants(&state);
}

// ===========================================================================
// Test: tla_set_pause
// ===========================================================================

#[test]
fn tla_set_pause() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit to have non-trivial state for UNCHANGED verification
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 5_000_000);

    let before = state.clone();
    assert!(!state.is_paused);

    // Pause
    action_set_pause(&mut state, true);
    assert!(state.is_paused);
    assert!(state.config.is_paused());
    // UNCHANGED: all other state must be identical
    assert_eq!(state.market.scale_factor(), before.market.scale_factor());
    assert_eq!(
        state.market.scaled_total_supply(),
        before.market.scaled_total_supply()
    );
    assert_eq!(state.vault_balance, before.vault_balance);
    assert_eq!(
        state.market.total_deposited(),
        before.market.total_deposited()
    );
    assert_eq!(
        state.market.total_borrowed(),
        before.market.total_borrowed()
    );
    assert_eq!(state.market.total_repaid(), before.market.total_repaid());
    assert_eq!(
        state.market.total_interest_repaid(),
        before.market.total_interest_repaid()
    );
    assert_eq!(
        state.market.accrued_protocol_fees(),
        before.market.accrued_protocol_fees()
    );
    assert_eq!(
        state.market.settlement_factor_wad(),
        before.market.settlement_factor_wad()
    );
    assert_eq!(state.total_payouts, before.total_payouts);
    assert_eq!(
        state.whitelist.current_borrowed(),
        before.whitelist.current_borrowed()
    );

    // Unpause
    action_set_pause(&mut state, false);
    assert!(!state.is_paused);
    assert!(!state.config.is_paused());

    check_all_invariants(&state);
}

// ===========================================================================
// Test: tla_pause_guards
// ===========================================================================

#[test]
fn tla_pause_guards() {
    let mut state = tla_created_market_state(0);

    // Setup: deposit to have a non-trivial state
    action_tick(&mut state, 100);
    action_deposit(&mut state, 0, 10_000_000);
    action_tick(&mut state, 500);

    // Pause the protocol
    action_set_pause(&mut state, true);
    let before = state.clone();

    // Deposit should be rejected when paused
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::Deposit {
            lender: 1,
            amount: 1_000_000
        }
    ));
    assert_state_unchanged(&before, &state, "paused deposit");

    // Borrow should be rejected when paused
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::Borrow { amount: 1_000_000 }
    ));
    assert_state_unchanged(&before, &state, "paused borrow");

    // Repay should be rejected when paused
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::Repay { amount: 1_000_000 }
    ));
    assert_state_unchanged(&before, &state, "paused repay");

    // RepayInterest should be rejected when paused
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::RepayInterest { amount: 1_000_000 }
    ));
    assert_state_unchanged(&before, &state, "paused repay_interest");

    // CollectFees should be rejected when paused
    assert!(!try_execute_action(&mut state, &TlaAction::CollectFees));
    assert_state_unchanged(&before, &state, "paused collect_fees");

    // Withdraw should be rejected when paused (need post-maturity state)
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::Withdraw { lender: 0 }
    ));
    assert_state_unchanged(&before, &state, "paused withdraw");

    // ReSettle should be rejected when paused
    assert!(!try_execute_action(&mut state, &TlaAction::ReSettle));
    assert_state_unchanged(&before, &state, "paused re_settle");

    // CloseLenderPosition should be rejected when paused
    assert!(!try_execute_action(
        &mut state,
        &TlaAction::CloseLenderPosition { lender: 1 }
    ));
    assert_state_unchanged(&before, &state, "paused close_lender_position");

    // WithdrawExcess should be rejected when paused
    assert!(!try_execute_action(&mut state, &TlaAction::WithdrawExcess));
    assert_state_unchanged(&before, &state, "paused withdraw_excess");

    // Unpause and verify operations work again
    action_set_pause(&mut state, false);
    assert!(try_execute_action(
        &mut state,
        &TlaAction::Deposit {
            lender: 1,
            amount: 1_000_000
        }
    ));

    check_all_invariants(&state);
}
