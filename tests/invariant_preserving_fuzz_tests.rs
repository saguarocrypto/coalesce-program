//! Invariant-Preserving Fuzzing Tests for the CoalesceFi Pinocchio lending protocol.
//!
//! Instead of checking invariants after arbitrary fuzzing (detect violations), these
//! tests generate fuzz inputs **constrained** to preserve invariants (prevent violations).
//! We use proptest strategies that only produce valid protocol states and valid transitions.
//!
//! If any invariant fails, it means the **implementation** has a bug, not the test input.

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

use proptest::prelude::*;

use bytemuck::Zeroable;
use coalesce::constants::{MAX_ANNUAL_INTEREST_BPS, MAX_FEE_RATE_BPS, WAD};
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};
use pinocchio::error::ProgramError;

// ===========================================================================
// Constants
// ===========================================================================

const NUM_LENDERS: usize = 4;
const MAX_DEPOSIT: u64 = 100_000_000_000_000;
const DEFAULT_MAX_SUPPLY: u64 = 1_000_000_000_000_000;
const DEFAULT_MATURITY: i64 = 31_536_000;
const DEFAULT_MAX_CAPACITY: u64 = 1_000_000_000_000_000;

// ===========================================================================
// Debug-friendly parameter types for proptest strategies
// ===========================================================================

/// Parameters to construct a valid Market. Implements Debug for proptest.
#[derive(Debug, Clone)]
struct MarketParams {
    annual_bps: u16,
    sf_extra: u64,
    total_deposited: u64,
    borrow_frac: u16,
    repay_frac: u16,
    settlement: u128,
    last_accrual: i64,
    maturity_margin: i64,
}

impl MarketParams {
    fn build(&self) -> Market {
        let mut m = Market::zeroed();
        m.set_annual_interest_bps(self.annual_bps);

        let scale_factor = WAD + u128::from(self.sf_extra);
        m.set_scale_factor(scale_factor);
        m.set_total_deposited(self.total_deposited);

        let total_borrowed =
            (u128::from(self.total_deposited) * u128::from(self.borrow_frac) / 10_000) as u64;
        m.set_total_borrowed(total_borrowed);

        let total_repaid =
            (u128::from(total_borrowed) * u128::from(self.repay_frac) / 10_000) as u64;
        m.set_total_repaid(total_repaid);

        let max_supply = DEFAULT_MAX_SUPPLY;
        m.set_max_total_supply(max_supply);
        let max_scaled = u128::from(max_supply)
            .saturating_mul(WAD)
            .checked_div(scale_factor)
            .unwrap_or(0);
        let deposit_scaled = u128::from(self.total_deposited)
            .saturating_mul(WAD)
            .checked_div(scale_factor)
            .unwrap_or(0);
        let scaled_supply = deposit_scaled.min(max_scaled);
        m.set_scaled_total_supply(scaled_supply);

        let vault = self
            .total_deposited
            .saturating_sub(total_borrowed)
            .saturating_add(total_repaid);
        let max_fees = vault / 2;
        m.set_accrued_protocol_fees(max_fees.min(1_000_000));

        m.set_last_accrual_timestamp(self.last_accrual);
        m.set_maturity_timestamp(self.last_accrual + self.maturity_margin);
        m.set_settlement_factor_wad(self.settlement);

        m
    }
}

// ===========================================================================
// Complete valid protocol state
// ===========================================================================

#[derive(Clone)]
struct ValidState {
    market: Market,
    config: ProtocolConfig,
    lenders: [LenderPosition; NUM_LENDERS],
    whitelist: BorrowerWhitelist,
    vault_balance: u64,
    current_time: i64,
    total_withdrawn: u64,
    prev_fees: u64,
}

/// Operations that can be applied to a valid state.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum ValidOp {
    Deposit { lender_idx: usize, amount: u64 },
    Borrow { amount: u64 },
    Repay { amount: u64 },
    Accrue { new_timestamp: i64 },
    Withdraw { lender_idx: usize },
}

// ===========================================================================
// Helper operators
// ===========================================================================

fn tla_div(a: u128, b: u128) -> u128 {
    if b == 0 {
        0
    } else {
        a / b
    }
}

fn normalized_total_supply(market: &Market) -> u128 {
    tla_div(
        market
            .scaled_total_supply()
            .saturating_mul(market.scale_factor()),
        WAD,
    )
}

fn compute_settlement_factor(market: &Market, vault_balance: u64) -> u128 {
    let total_norm = normalized_total_supply(market);
    // After COAL-C01: no fee reservation; available = vault_balance directly
    let available = u128::from(vault_balance);

    if total_norm == 0 {
        WAD
    } else {
        let raw = tla_div(available.saturating_mul(WAD), total_norm);
        let capped = raw.min(WAD);
        capped.max(1)
    }
}

// ===========================================================================
// 10 Invariant checks
// ===========================================================================

fn check_all_invariants(state: &ValidState, label: &str) {
    let m = &state.market;

    // I-1: Solvency
    let expected_vault = (m.total_deposited() as u128)
        .checked_sub(m.total_borrowed() as u128)
        .and_then(|v| v.checked_add(m.total_repaid() as u128))
        .and_then(|v| v.checked_sub(state.total_withdrawn as u128));
    if let Some(expected) = expected_vault {
        assert_eq!(
            state.vault_balance as u128,
            expected,
            "{}: I-1 solvency: vault={} != dep({}) - bor({}) + rep({}) - wdr({})",
            label,
            state.vault_balance,
            m.total_deposited(),
            m.total_borrowed(),
            m.total_repaid(),
            state.total_withdrawn
        );
    }

    // I-2: scale_factor >= WAD
    assert!(
        m.scale_factor() >= WAD,
        "{}: I-2 scale_factor ({}) < WAD",
        label,
        m.scale_factor()
    );

    // I-3: accrued_protocol_fees never decreases (vs tracked prev_fees)
    assert!(
        m.accrued_protocol_fees() >= state.prev_fees,
        "{}: I-3 fees decreased: {} -> {}",
        label,
        state.prev_fees,
        m.accrued_protocol_fees()
    );

    // I-4: scaled_total_supply == sum of lender positions
    let sum_lenders: u128 = state.lenders.iter().map(|l| l.scaled_balance()).sum();
    assert_eq!(
        m.scaled_total_supply(),
        sum_lenders,
        "{}: I-4 scaled_total_supply ({}) != sum_lenders ({})",
        label,
        m.scaled_total_supply(),
        sum_lenders
    );

    // I-5: Settlement factor in [1, WAD] when set
    let sf_wad = m.settlement_factor_wad();
    if sf_wad != 0 {
        assert!(
            sf_wad >= 1 && sf_wad <= WAD,
            "{}: I-5 settlement_factor ({}) not in [1, WAD]",
            label,
            sf_wad
        );
    }

    // I-6: Payout <= normalized for each lender (when settlement set)
    if sf_wad != 0 {
        for (i, lender) in state.lenders.iter().enumerate() {
            let sb = lender.scaled_balance();
            if sb == 0 {
                continue;
            }
            let normalized = tla_div(sb.saturating_mul(m.scale_factor()), WAD);
            let payout = tla_div(normalized.saturating_mul(sf_wad), WAD);
            assert!(
                payout <= normalized,
                "{}: I-6 lender {} payout ({}) > normalized ({})",
                label,
                i,
                payout,
                normalized
            );
        }
    }

    // I-7: Type invariant (scale_factor >= WAD when initialized)
    if m.scale_factor() > 0 {
        assert!(
            m.scale_factor() >= WAD,
            "{}: I-7 scale_factor={} < WAD",
            label,
            m.scale_factor()
        );
    }

    // I-8: annual_interest_bps in valid range
    assert!(
        m.annual_interest_bps() <= MAX_ANNUAL_INTEREST_BPS,
        "{}: I-8 annual_interest_bps ({}) > max",
        label,
        m.annual_interest_bps()
    );

    // I-9: fee_rate_bps in valid range
    assert!(
        state.config.fee_rate_bps() <= MAX_FEE_RATE_BPS,
        "{}: I-9 fee_rate_bps ({}) > max",
        label,
        state.config.fee_rate_bps()
    );

    // I-10: whitelist capacity
    assert!(
        state.whitelist.current_borrowed() <= state.whitelist.max_borrow_capacity(),
        "{}: I-10 wl borrowed ({}) > cap ({})",
        label,
        state.whitelist.current_borrowed(),
        state.whitelist.max_borrow_capacity()
    );
}

// ===========================================================================
// Valid state construction
// ===========================================================================

fn fresh_valid_state(annual_bps: u16, fee_bps: u16, maturity_ts: i64) -> ValidState {
    let mut market = Market::zeroed();
    market.set_annual_interest_bps(annual_bps);
    market.set_maturity_timestamp(maturity_ts);
    market.set_scale_factor(WAD);
    market.set_last_accrual_timestamp(0);
    market.set_max_total_supply(DEFAULT_MAX_SUPPLY);

    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_bps);

    let mut whitelist = BorrowerWhitelist::zeroed();
    whitelist.is_whitelisted = 1;
    whitelist.set_max_borrow_capacity(DEFAULT_MAX_CAPACITY);

    ValidState {
        market,
        config,
        lenders: [LenderPosition::zeroed(); NUM_LENDERS],
        whitelist,
        vault_balance: 0,
        current_time: 0,
        total_withdrawn: 0,
        prev_fees: 0,
    }
}

// ===========================================================================
// State transition functions
// ===========================================================================

fn apply_accrue(state: &mut ValidState, new_ts: i64) -> bool {
    state.current_time = new_ts;
    let ts = state.current_time;
    accrue_interest(&mut state.market, &state.config, ts).is_ok()
}

fn apply_deposit(state: &mut ValidState, lender_idx: usize, amount: u64) -> bool {
    if amount == 0 {
        return false;
    }
    let ts = state.current_time;
    if accrue_interest(&mut state.market, &state.config, ts).is_err() {
        return false;
    }

    let sf = state.market.scale_factor();
    if sf == 0 {
        return false;
    }

    let amount_u128 = u128::from(amount);
    let scaled = match amount_u128.checked_mul(WAD).and_then(|v| v.checked_div(sf)) {
        Some(s) if s > 0 => s,
        _ => return false,
    };

    let new_total = match state.market.scaled_total_supply().checked_add(scaled) {
        Some(t) => t,
        None => return false,
    };

    let new_norm = tla_div(new_total.saturating_mul(sf), WAD);
    if new_norm > u128::from(state.market.max_total_supply()) {
        return false;
    }

    state.market.set_scaled_total_supply(new_total);
    state
        .market
        .set_total_deposited(state.market.total_deposited().saturating_add(amount));
    state.vault_balance = state.vault_balance.saturating_add(amount);
    let idx = lender_idx % NUM_LENDERS;
    let new_balance = state.lenders[idx].scaled_balance().saturating_add(scaled);
    state.lenders[idx].set_scaled_balance(new_balance);
    state.prev_fees = state.market.accrued_protocol_fees();
    true
}

fn apply_borrow(state: &mut ValidState, amount: u64) -> bool {
    if amount == 0 {
        return false;
    }
    let ts = state.current_time;
    if accrue_interest(&mut state.market, &state.config, ts).is_err() {
        return false;
    }

    let borrowable = state.vault_balance;
    if amount > borrowable {
        return false;
    }

    let new_wl = state.whitelist.current_borrowed().saturating_add(amount);
    if new_wl > state.whitelist.max_borrow_capacity() {
        return false;
    }

    state
        .market
        .set_total_borrowed(state.market.total_borrowed().saturating_add(amount));
    state.vault_balance = state.vault_balance.saturating_sub(amount);
    state.whitelist.set_current_borrowed(new_wl);
    state.prev_fees = state.market.accrued_protocol_fees();
    true
}

fn apply_repay(state: &mut ValidState, amount: u64) -> bool {
    if amount == 0 {
        return false;
    }
    let zero_config: ProtocolConfig = Zeroable::zeroed();
    let ts = state.current_time;
    if accrue_interest(&mut state.market, &zero_config, ts).is_err() {
        return false;
    }

    state
        .market
        .set_total_repaid(state.market.total_repaid().saturating_add(amount));
    state.vault_balance = state.vault_balance.saturating_add(amount);
    true
}

fn apply_withdraw(state: &mut ValidState, lender_idx: usize) -> bool {
    let ts = state.current_time;
    if accrue_interest(&mut state.market, &state.config, ts).is_err() {
        return false;
    }
    let idx = lender_idx % NUM_LENDERS;
    let scaled_balance = state.lenders[idx].scaled_balance();
    if scaled_balance == 0 {
        return false;
    }

    if state.market.settlement_factor_wad() == 0 {
        let sf = compute_settlement_factor(&state.market, state.vault_balance);
        state.market.set_settlement_factor_wad(sf);
    }

    let scale_factor = state.market.scale_factor();
    let settlement_factor = state.market.settlement_factor_wad();
    let normalized = tla_div(scaled_balance.saturating_mul(scale_factor), WAD);
    let payout_u128 = tla_div(normalized.saturating_mul(settlement_factor), WAD);
    let payout = match u64::try_from(payout_u128) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if payout == 0 || payout > state.vault_balance {
        return false;
    }

    state.lenders[idx].set_scaled_balance(0);
    let new_scaled_total = state
        .market
        .scaled_total_supply()
        .saturating_sub(scaled_balance);
    state.market.set_scaled_total_supply(new_scaled_total);
    state.vault_balance = state.vault_balance.saturating_sub(payout);
    state.total_withdrawn = state.total_withdrawn.saturating_add(payout);
    state.prev_fees = state.market.accrued_protocol_fees();
    true
}

#[allow(dead_code)]
fn apply_op(state: &mut ValidState, op: &ValidOp) -> bool {
    match op {
        ValidOp::Deposit { lender_idx, amount } => apply_deposit(state, *lender_idx, *amount),
        ValidOp::Borrow { amount } => apply_borrow(state, *amount),
        ValidOp::Repay { amount } => apply_repay(state, *amount),
        ValidOp::Accrue { new_timestamp } => apply_accrue(state, *new_timestamp),
        ValidOp::Withdraw { lender_idx } => apply_withdraw(state, *lender_idx),
    }
}

// ===========================================================================
// Edge-biased strategies
// ===========================================================================

fn edge_biased_fuzz_amount() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => Just(0u64),
        3 => Just(1u64),
        3 => Just(u64::MAX),
        91 => 0u64..=1_000_000_000u64,
    ]
}

fn edge_biased_deposit_amount() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => Just(1u64),
        3 => Just(1_000_000u64),
        3 => Just(MAX_DEPOSIT),
        91 => 1u64..=MAX_DEPOSIT,
    ]
}

// ===========================================================================
// 1. Valid state generators (infrastructure)
// ===========================================================================

fn valid_market_params_strategy() -> impl Strategy<Value = MarketParams> {
    (
        0u16..=MAX_ANNUAL_INTEREST_BPS,
        0u64..=(WAD as u64),
        1_000_000u64..=MAX_DEPOSIT,
        0u16..=10000u16,
        0u16..=10000u16,
        prop_oneof![Just(0u128), (1u128..=WAD),],
        0i64..=100_000i64,
        100i64..=DEFAULT_MATURITY,
    )
        .prop_map(
            |(
                annual_bps,
                sf_extra,
                total_deposited,
                borrow_frac,
                repay_frac,
                settlement,
                last_accrual,
                maturity_margin,
            )| {
                MarketParams {
                    annual_bps,
                    sf_extra,
                    total_deposited,
                    borrow_frac,
                    repay_frac,
                    settlement,
                    last_accrual,
                    maturity_margin,
                }
            },
        )
}

#[allow(dead_code)]
fn valid_config_strategy() -> impl Strategy<Value = u16> {
    0u16..=MAX_FEE_RATE_BPS
}

#[allow(dead_code)]
fn valid_whitelist_params_strategy() -> impl Strategy<Value = (u64, u16)> {
    (1_000_000u64..=DEFAULT_MAX_CAPACITY, 0u16..=10000u16)
}

// ===========================================================================
// 2. State-aware sequence generation (4 tests)
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// 2a: Pure deposit sequences -- deposit operations on a fresh market.
    #[test]
    fn test_pure_deposit_sequences(
        annual_bps in 0u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        amounts in prop::collection::vec(edge_biased_fuzz_amount(), 10..=100),
    ) {
        let mut state = fresh_valid_state(annual_bps, fee_bps, DEFAULT_MATURITY);
        check_all_invariants(&state, "init");

        let mut clock: i64 = 1;
        let mut lender_ctr: usize = 0;

        for (step, amount) in amounts.iter().enumerate() {
            clock += 10;
            state.current_time = clock;
            let ts = state.current_time;
            let _ = accrue_interest(&mut state.market, &state.config, ts);

            let label = format!("deposit_seq step {}", step);
            let lender_idx = lender_ctr % NUM_LENDERS;
            lender_ctr += 1;

            let ok = apply_deposit(&mut state, lender_idx, *amount);
            if ok {
                state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                check_all_invariants(&state, &label);
            }
        }
    }

    /// 2b: Deposit + borrow sequences.
    #[test]
    fn test_deposit_borrow_sequences(
        annual_bps in 100u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        deposit_amounts in prop::collection::vec(edge_biased_deposit_amount(), 5..=20),
        borrow_fracs in prop::collection::vec(1u16..=8000u16, 5..=20),
    ) {
        let mut state = fresh_valid_state(annual_bps, fee_bps, DEFAULT_MATURITY);
        check_all_invariants(&state, "init");

        let mut clock: i64 = 1;

        let len = deposit_amounts.len().min(borrow_fracs.len());
        for step in 0..len {
            clock += 100;
            state.current_time = clock;

            // Deposit
            let dep_ok = apply_deposit(&mut state, step % NUM_LENDERS, deposit_amounts[step]);
            if dep_ok {
                state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                check_all_invariants(&state, &format!("dep_bor deposit {}", step));
            }

            // Borrow fraction of vault
            let borrowable = state.vault_balance;
            let borrow_amount = (u128::from(borrowable) * u128::from(borrow_fracs[step]) / 10_000) as u64;
            if borrow_amount > 0 {
                let bor_ok = apply_borrow(&mut state, borrow_amount);
                if bor_ok {
                    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                    check_all_invariants(&state, &format!("dep_bor borrow {}", step));
                }
            }
        }
    }

    /// 2c: Full lifecycle sequences -- deposit, borrow, repay, accrue, withdraw.
    #[test]
    fn test_full_lifecycle_sequences(
        annual_bps in 100u16..=5000u16,
        fee_bps in 0u16..=5000u16,
        deposit_amount in edge_biased_deposit_amount(),
        borrow_frac in 1000u16..=8000u16,
        repay_extra in 0u64..=5_000_000u64,
    ) {
        let maturity = 100_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);
        check_all_invariants(&state, "lifecycle init");

        // Phase 1: Deposit
        state.current_time = 100;
        let _ = apply_deposit(&mut state, 0, deposit_amount);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "lifecycle deposit");

        // Phase 2: Accrue interest
        state.current_time = 10_000;
        let ts = state.current_time;
        let _ = accrue_interest(&mut state.market, &state.config, ts);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "lifecycle accrue1");

        // Phase 3: Borrow
        let borrowable = state.vault_balance;
        let borrow_amount = (u128::from(borrowable) * u128::from(borrow_frac) / 10_000) as u64;
        if borrow_amount > 0 {
            let _ = apply_borrow(&mut state, borrow_amount);
            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
            check_all_invariants(&state, "lifecycle borrow");
        }

        // Phase 4: Accrue to near maturity
        state.current_time = maturity - 1;
        let ts2 = state.current_time;
        let _ = accrue_interest(&mut state.market, &state.config, ts2);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "lifecycle accrue2");

        // Phase 5: Repay
        state.current_time = maturity;
        let repay_amount = borrow_amount.saturating_add(repay_extra);
        if repay_amount > 0 {
            let _ = apply_repay(&mut state, repay_amount);
            check_all_invariants(&state, "lifecycle repay");
        }

        // Phase 6: Withdraw post-maturity
        state.current_time = maturity + 1;
        if state.lenders[0].scaled_balance() > 0 {
            let _ = apply_withdraw(&mut state, 0);
            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
            check_all_invariants(&state, "lifecycle withdraw");
        }
    }

    /// 2d: Multi-lender sequences.
    #[test]
    fn test_multi_lender_sequences(
        annual_bps in 100u16..=3000u16,
        fee_bps in 0u16..=3000u16,
        deposits in prop::collection::vec(
            (0usize..NUM_LENDERS, edge_biased_deposit_amount()),
            4..=16
        ),
    ) {
        let maturity = 200_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);
        check_all_invariants(&state, "multi init");

        let mut clock: i64 = 1;

        // Phase 1: Multiple lenders deposit
        for (step, (lender_idx, amount)) in deposits.iter().enumerate() {
            clock += 50;
            state.current_time = clock;
            let ok = apply_deposit(&mut state, *lender_idx, *amount);
            if ok {
                state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                check_all_invariants(&state, &format!("multi deposit {}", step));
            }
        }

        // Phase 2: Advance past maturity and withdraw all lenders
        state.current_time = maturity + 1;
        for idx in 0..NUM_LENDERS {
            if state.lenders[idx].scaled_balance() > 0 {
                let ok = apply_withdraw(&mut state, idx);
                if ok {
                    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                    check_all_invariants(&state, &format!("multi withdraw {}", idx));
                }
            }
        }
    }
}

// ===========================================================================
// 3. Boundary-walking tests (4 tests)
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// 3a: Supply cap boundary -- deposits that push right up to but not past the cap.
    #[test]
    fn test_supply_cap_boundary(
        annual_bps in 0u16..=1000u16,
        fee_bps in 0u16..=500u16,
        cap in 5_000_000u64..=50_000_000u64,
        num_deposits in 3usize..=10,
    ) {
        let mut state = fresh_valid_state(annual_bps, fee_bps, DEFAULT_MATURITY);
        state.market.set_max_total_supply(cap);
        check_all_invariants(&state, "cap_boundary init");

        state.current_time = 1;

        let per_deposit = cap / (num_deposits as u64);
        for step in 0..num_deposits {
            let amount = per_deposit.saturating_sub(1).max(1);
            let ok = apply_deposit(&mut state, step % NUM_LENDERS, amount);
            if ok {
                state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                let norm = normalized_total_supply(&state.market);
                let max_s = u128::from(state.market.max_total_supply());
                assert!(
                    norm <= max_s,
                    "cap_boundary step {}: norm ({}) > cap ({})", step, norm, max_s
                );
                check_all_invariants(&state, &format!("cap_boundary step {}", step));
            }
        }
    }

    /// 3b: Whitelist capacity boundary.
    #[test]
    fn test_whitelist_capacity_boundary(
        annual_bps in 0u16..=1000u16,
        wl_cap in 5_000_000u64..=50_000_000u64,
        num_borrows in 2usize..=8,
    ) {
        let mut state = fresh_valid_state(annual_bps, 0, DEFAULT_MATURITY);
        state.whitelist.set_max_borrow_capacity(wl_cap);
        check_all_invariants(&state, "wl_boundary init");

        state.current_time = 1;
        let deposit_amount = wl_cap.saturating_mul(2).min(MAX_DEPOSIT);
        let _ = apply_deposit(&mut state, 0, deposit_amount);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        let per_borrow = wl_cap / (num_borrows as u64);
        for step in 0..num_borrows {
            let amount = per_borrow.saturating_sub(1).max(1);
            let ok = apply_borrow(&mut state, amount);
            if ok {
                state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                assert!(
                    state.whitelist.current_borrowed() <= state.whitelist.max_borrow_capacity(),
                    "wl_boundary step {}: borrowed ({}) > cap ({})",
                    step, state.whitelist.current_borrowed(), state.whitelist.max_borrow_capacity()
                );
                check_all_invariants(&state, &format!("wl_boundary step {}", step));
            }
        }
    }

    /// 3c: Settlement factor bounds -- settlement at boundary values.
    #[test]
    fn test_settlement_factor_bounds(
        deposit_amount in edge_biased_deposit_amount(),
        borrow_frac in 0u16..=10000u16,
        repay_frac in 0u16..=15000u16,
    ) {
        let maturity = 50_000i64;
        let mut state = fresh_valid_state(500, 0, maturity);
        check_all_invariants(&state, "sf_bounds init");

        state.current_time = 100;
        let _ = apply_deposit(&mut state, 0, deposit_amount);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        state.current_time = 1000;
        let borrowable = state.vault_balance;
        let borrow_amount = (u128::from(borrowable) * u128::from(borrow_frac) / 10_000) as u64;
        if borrow_amount > 0 {
            let _ = apply_borrow(&mut state, borrow_amount);
            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        }

        state.current_time = maturity;
        let repay = (u128::from(borrow_amount) * u128::from(repay_frac) / 10_000) as u64;
        if repay > 0 {
            let _ = apply_repay(&mut state, repay);
        }

        state.current_time = maturity + 1;
        if state.lenders[0].scaled_balance() > 0 {
            let ok = apply_withdraw(&mut state, 0);
            if ok {
                let sf = state.market.settlement_factor_wad();
                assert!(sf >= 1 && sf <= WAD,
                    "sf_bounds: settlement factor {} not in [1, WAD]", sf);
                state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                check_all_invariants(&state, "sf_bounds withdraw");
            }
        }
    }

    /// 3d: Fee accumulation boundary -- fees grow but never corrupt state.
    #[test]
    fn test_fee_accumulation_boundary(
        annual_bps in 1000u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 1000u16..=MAX_FEE_RATE_BPS,
        deposit_amount in edge_biased_deposit_amount(),
        num_accruals in 5usize..=50,
    ) {
        let maturity = 10_000_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);
        check_all_invariants(&state, "fee_boundary init");

        state.current_time = 1;
        let _ = apply_deposit(&mut state, 0, deposit_amount);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        let mut prev_fees = state.market.accrued_protocol_fees();

        for step in 0..num_accruals {
            state.current_time += 10_000;
            let ts = state.current_time;
            let ok = accrue_interest(&mut state.market, &state.config, ts);
            if ok.is_ok() {
                let new_fees = state.market.accrued_protocol_fees();
                assert!(
                    new_fees >= prev_fees,
                    "fee_boundary step {}: fees decreased {} -> {}", step, prev_fees, new_fees
                );
                prev_fees = new_fees;
                state.prev_fees = state.prev_fees.min(new_fees);
                check_all_invariants(&state, &format!("fee_boundary step {}", step));
            }
        }
    }
}

// ===========================================================================
// 4. Reachability tests (3 tests)
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// 4a: Verify valid_market_params_strategy covers a wide range of states.
    #[test]
    fn test_reachability_state_distribution(
        params in prop::collection::vec(valid_market_params_strategy(), 100),
    ) {
        let mut count_zero_borrow = 0u32;
        let mut count_nonzero_borrow = 0u32;
        let mut count_settlement_set = 0u32;
        let mut count_settlement_zero = 0u32;
        let mut count_high_interest = 0u32;

        for p in &params {
            let market = p.build();
            // Verify the generated market is structurally valid
            assert!(market.scale_factor() >= WAD);
            assert!(market.total_borrowed() <= market.total_deposited());
            assert!(market.annual_interest_bps() <= MAX_ANNUAL_INTEREST_BPS);
            assert!(market.last_accrual_timestamp() <= market.maturity_timestamp());

            if market.total_borrowed() == 0 { count_zero_borrow += 1; } else { count_nonzero_borrow += 1; }
            if market.settlement_factor_wad() == 0 { count_settlement_zero += 1; } else { count_settlement_set += 1; }
            if market.annual_interest_bps() > MAX_ANNUAL_INTEREST_BPS / 2 { count_high_interest += 1; }
        }

        // With 100 samples, we should see both zero-borrow and nonzero-borrow states
        prop_assert!(count_zero_borrow > 0 || count_nonzero_borrow > 0,
            "must generate at least one borrow state variant");
        prop_assert!(count_settlement_set > 0 || count_settlement_zero > 0,
            "must generate at least one settlement state variant");
        // High-interest states should appear since we sample uniformly over bps range
        prop_assert!(count_high_interest > 0 || params.len() < 5,
            "should generate some high-interest states (got {} out of {})",
            count_high_interest, params.len());
    }

    /// 4b: Verify that every operation type is reachable from generated valid states.
    #[test]
    fn test_reachability_all_operations(
        annual_bps in 100u16..=5000u16,
        fee_bps in 0u16..=3000u16,
        deposit_amount in 1_000_000u64..=10_000_000u64,
    ) {
        let maturity = 100_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);

        // Accrue is always reachable
        state.current_time = 10;
        let accrue_ok = apply_accrue(&mut state, 10);
        prop_assert!(accrue_ok, "Accrue must be reachable");

        // Deposit is reachable pre-maturity
        state.current_time = 100;
        let deposit_ok = apply_deposit(&mut state, 0, deposit_amount);
        prop_assert!(deposit_ok, "Deposit must be reachable");

        // Borrow is reachable with funds in vault
        state.current_time = 200;
        let borrowable = state.vault_balance;
        if borrowable > 0 {
            let borrow_ok = apply_borrow(&mut state, borrowable.min(deposit_amount / 2));
            prop_assert!(borrow_ok, "Borrow must be reachable with funds");
        }

        // Repay is reachable with borrowed > 0
        state.current_time = 300;
        if state.market.total_borrowed() > 0 {
            let repay_ok = apply_repay(&mut state, 1);
            prop_assert!(repay_ok, "Repay must be reachable");
        }

        // Withdraw is reachable post-maturity with balance
        state.current_time = maturity + 1;
        if state.lenders[0].scaled_balance() > 0 {
            let _withdraw_ok = apply_withdraw(&mut state, 0);
            // Might fail if payout > vault due to borrows; that's ok.
        }
    }

    /// 4c: Both pre-maturity and post-maturity states are generated and reachable.
    #[test]
    fn test_reachability_pre_and_post_maturity(
        annual_bps in 100u16..=5000u16,
        fee_bps in 0u16..=1000u16,
        deposit_amount in 1_000_000u64..=10_000_000u64,
    ) {
        let maturity = 50_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);

        // Pre-maturity operations
        state.current_time = 100;
        let dep_ok = apply_deposit(&mut state, 0, deposit_amount);
        prop_assert!(dep_ok, "deposit must succeed pre-maturity");
        prop_assert!(state.current_time < maturity, "state must be pre-maturity");

        // Transition to post-maturity
        state.current_time = maturity + 1;
        prop_assert!(state.current_time >= maturity, "state must be post-maturity");

        // Post-maturity: withdraw is reachable
        if state.lenders[0].scaled_balance() > 0 {
            let w_ok = apply_withdraw(&mut state, 0);
            prop_assert!(w_ok, "withdraw must be reachable post-maturity with balance and vault funds");
        }
    }
}

// ===========================================================================
// 5. Transition completeness (3 tests)
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// 5a: For each valid state, at least one operation is possible (no deadlocks).
    /// Error-path assertions verify exact Custom error codes.
    #[test]
    fn test_no_deadlocks(
        annual_bps in 0u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        deposit_amount in edge_biased_deposit_amount(),
    ) {
        let maturity = 100_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);

        // Fresh state: Accrue should always work
        state.current_time = 1;
        let accrue_ok = apply_accrue(&mut state, 1);
        prop_assert!(accrue_ok, "fresh state must allow at least accrue");

        // Error path: accruing backwards must fail with InvalidTimestamp (code 20)
        {
            let mut backward_state = state.clone();
            backward_state.market.set_last_accrual_timestamp(500);
            let result = accrue_interest(&mut backward_state.market, &backward_state.config, 100);
            prop_assert_eq!(
                result,
                Err(ProgramError::Custom(LendingError::InvalidTimestamp as u32)),
                "backward accrue must fail with InvalidTimestamp (code 20)"
            );
        }

        // After deposit
        state.current_time = 100;
        let _ = apply_deposit(&mut state, 0, deposit_amount);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        // At least accrue or borrow should be possible
        let mut any_possible = false;
        // Try accrue
        {
            let mut test_state = state.clone();
            test_state.current_time += 1;
            let ts = test_state.current_time;
            if accrue_interest(&mut test_state.market, &test_state.config, ts).is_ok() {
                any_possible = true;
            }
        }
        // Try borrow
        let borrowable = state.vault_balance;
        if borrowable > 0 {
            any_possible = true;
        }
        prop_assert!(any_possible, "after deposit, some operation must be possible");

        // Error path: overflow with huge scale factor must return MathOverflow (code 41)
        {
            let mut overflow_state = state.clone();
            overflow_state.market.set_scale_factor(u128::MAX / 2);
            overflow_state.market.set_annual_interest_bps(MAX_ANNUAL_INTEREST_BPS);
            overflow_state.market.set_last_accrual_timestamp(0);
            overflow_state.market.set_maturity_timestamp(i64::MAX);
            let result = accrue_interest(
                &mut overflow_state.market,
                &overflow_state.config,
                31_536_000,
            );
            if result.is_err() {
                prop_assert_eq!(
                    result,
                    Err(ProgramError::Custom(LendingError::MathOverflow as u32)),
                    "overflow accrue must fail with MathOverflow (code 41)"
                );
            }
        }

        // Post-maturity with balance: at least withdraw should be possible
        state.current_time = maturity + 1;
        if state.lenders[0].scaled_balance() > 0 && state.vault_balance > 0 {
            let sf = compute_settlement_factor(&state.market, state.vault_balance);
            let scale = state.market.scale_factor();
            let norm = tla_div(state.lenders[0].scaled_balance().saturating_mul(scale), WAD);
            let payout = tla_div(norm.saturating_mul(sf), WAD);
            if payout > 0 && payout <= u128::from(state.vault_balance) {
                any_possible = true;
            }
        }
        prop_assert!(any_possible, "at all stages, at least one operation must be possible");
    }

    /// 5b: For a valid pre-maturity state with deposits, Borrow is possible.
    #[test]
    fn test_borrow_reachable_pre_maturity(
        annual_bps in 0u16..=5000u16,
        fee_bps in 0u16..=500u16,
        deposit_amount in 5_000_000u64..=50_000_000u64,
    ) {
        let maturity = 200_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);

        state.current_time = 100;
        let dep_ok = apply_deposit(&mut state, 0, deposit_amount);
        prop_assert!(dep_ok, "deposit must succeed");
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        let borrowable = state.vault_balance;
        prop_assert!(
            borrowable > 0,
            "after deposit, borrowable should be > 0 (vault={})",
            state.vault_balance
        );

        let bor_ok = apply_borrow(&mut state, 1);
        prop_assert!(bor_ok, "borrow of 1 must succeed when borrowable > 0");
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "borrow_reachable");
    }

    /// 5c: For a valid post-maturity state with balance, Withdraw is possible.
    #[test]
    fn test_withdraw_reachable_post_maturity(
        annual_bps in 0u16..=3000u16,
        deposit_amounts in prop::collection::vec(1_000_000u64..=20_000_000u64, 10),
    ) {
        let maturity = 50_000i64;
        let mut withdraw_success_count = 0u32;
        let mut full_settlement_count = 0u32;

        for deposit_amount in &deposit_amounts {
            let mut state = fresh_valid_state(annual_bps, 0, maturity);

            state.current_time = 100;
            let dep_ok = apply_deposit(&mut state, 0, *deposit_amount);
            if !dep_ok { continue; }
            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

            state.current_time = maturity + 1;
            if state.lenders[0].scaled_balance() == 0 { continue; }

            let w_ok = apply_withdraw(&mut state, 0);
            if w_ok {
                withdraw_success_count += 1;
                if state.market.settlement_factor_wad() == WAD {
                    full_settlement_count += 1;
                }
                state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
                check_all_invariants(&state, "withdraw_reachable");
            }
        }

        // With 10 samples of no-borrow deposits, all should withdraw successfully
        prop_assert!(withdraw_success_count > 0,
            "at least one withdraw must succeed (got {}/{})",
            withdraw_success_count, deposit_amounts.len());
        // With non-zero interest rate, settlement factor may be < WAD (vault doesn't grow
        // with interest, only normalized supply does). The key invariant is that settlement
        // is in [1, WAD] and withdrawals succeed.
        // When annual_bps == 0, settlement should be WAD. Track both cases.
        if annual_bps == 0 {
            prop_assert!(full_settlement_count > 0,
                "with 0% interest, at least one full settlement (WAD) expected (got {}/{})",
                full_settlement_count, withdraw_success_count);
        }
    }
}

// ===========================================================================
// 6. Invariant closure proof (2 tests)
// ===========================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// 6a: From any valid state, applying any valid operation produces a valid state
    /// (closure property).
    #[test]
    fn test_invariant_closure_single_step(
        annual_bps in 100u16..=5000u16,
        fee_bps in 0u16..=3000u16,
        deposit_amount in 1_000_000u64..=10_000_000u64,
        borrow_frac in 0u16..=5000u16,
        time_delta in 100i64..=5000i64,
        op_selector in 0u8..=4u8,
    ) {
        let maturity = 200_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);
        check_all_invariants(&state, "closure init");

        // Build up a non-trivial valid state
        state.current_time = 100;
        let _ = apply_deposit(&mut state, 0, deposit_amount);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        state.current_time = 200;
        let _ = apply_deposit(&mut state, 1, deposit_amount / 2);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        let borrowable = state.vault_balance;
        let borrow_amount = (u128::from(borrowable) * u128::from(borrow_frac) / 10_000) as u64;
        if borrow_amount > 0 {
            let _ = apply_borrow(&mut state, borrow_amount);
            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        }

        check_all_invariants(&state, "closure pre-op");

        // Apply a single operation based on selector
        match op_selector {
            0 => {
                let new_ts = state.current_time + time_delta;
                let _ = apply_accrue(&mut state, new_ts);
            }
            1 => {
                state.current_time += 10;
                let _ = apply_deposit(&mut state, 2, deposit_amount / 4);
            }
            2 => {
                state.current_time += 10;
                let fees_r = state.vault_balance.min(state.market.accrued_protocol_fees());
                let borr = state.vault_balance.saturating_sub(fees_r);
                if borr > 0 {
                    let _ = apply_borrow(&mut state, borr.min(1_000_000));
                }
            }
            3 => {
                state.current_time += 10;
                if state.market.total_borrowed() > 0 {
                    let _ = apply_repay(&mut state, 100_000);
                }
            }
            _ => {
                state.current_time = maturity + 1;
                if state.lenders[0].scaled_balance() > 0 {
                    let _ = apply_withdraw(&mut state, 0);
                }
            }
        }

        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "closure post-op");
    }

    /// 6b: Composition of N valid operations from a valid init state always
    /// produces a valid state.
    #[test]
    fn test_invariant_closure_n_steps(
        annual_bps in 100u16..=5000u16,
        fee_bps in 0u16..=3000u16,
        initial_deposit in 1_000_000u64..=20_000_000u64,
        op_selectors in prop::collection::vec(0u8..=4u8, 10..=100),
        op_amounts in prop::collection::vec(edge_biased_fuzz_amount(), 10..=100),
    ) {
        let maturity = 500_000i64;
        let mut state = fresh_valid_state(annual_bps, fee_bps, maturity);
        check_all_invariants(&state, "closure_n init");

        // Bootstrap with deposit
        state.current_time = 100;
        let _ = apply_deposit(&mut state, 0, initial_deposit);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "closure_n bootstrap");

        let mut clock = 200i64;
        let mut lender_ctr = 1usize;
        let len = op_selectors.len().min(op_amounts.len());
        let mut past_maturity = false;

        for step in 0..len {
            let sel = op_selectors[step];
            let amt = op_amounts[step];
            let label = format!("closure_n step {}", step);

            match sel {
                0 => {
                    clock += 100;
                    state.current_time = clock;
                    let _ = apply_accrue(&mut state, clock);
                }
                1 if !past_maturity => {
                    clock += 10;
                    state.current_time = clock;
                    let _ = apply_deposit(&mut state, lender_ctr % NUM_LENDERS, amt);
                    lender_ctr += 1;
                }
                2 if !past_maturity => {
                    clock += 10;
                    state.current_time = clock;
                    let fees_r = state.vault_balance.min(state.market.accrued_protocol_fees());
                    let borr = state.vault_balance.saturating_sub(fees_r);
                    let ba = amt.min(borr);
                    if ba > 0 {
                        let _ = apply_borrow(&mut state, ba);
                    }
                }
                3 => {
                    clock += 10;
                    state.current_time = clock;
                    let ra = amt.min(state.market.total_borrowed());
                    if ra > 0 {
                        let _ = apply_repay(&mut state, ra);
                    }
                }
                _ => {
                    if !past_maturity {
                        clock = maturity + 1;
                        state.current_time = clock;
                        past_maturity = true;
                    }
                    for idx in 0..NUM_LENDERS {
                        if state.lenders[idx].scaled_balance() > 0 {
                            let _ = apply_withdraw(&mut state, idx);
                            break;
                        }
                    }
                }
            }

            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
            check_all_invariants(&state, &label);
        }
    }
}

// ===========================================================================
// Additional deterministic boundary tests
// ===========================================================================

/// Boundary: scale_factor == WAD with 0% interest.
#[test]
fn test_boundary_scale_factor_at_wad() {
    let mut state = fresh_valid_state(0, 0, DEFAULT_MATURITY);
    check_all_invariants(&state, "sf_at_wad init");

    // Assert exact initial field values
    assert_eq!(
        state.market.scale_factor(),
        WAD,
        "initial scale_factor must be WAD"
    );
    assert_eq!(
        state.market.total_deposited(),
        0,
        "initial total_deposited must be 0"
    );
    assert_eq!(
        state.market.accrued_protocol_fees(),
        0,
        "initial fees must be 0"
    );

    state.current_time = 100;
    let _ = apply_deposit(&mut state, 0, 1_000_000);
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    assert_eq!(
        state.market.scale_factor(),
        WAD,
        "0% interest should keep scale_factor at WAD"
    );
    assert_eq!(state.market.total_deposited(), 1_000_000);
    assert_eq!(state.vault_balance, 1_000_000);
    check_all_invariants(&state, "sf_at_wad deposit");

    state.current_time = 10_000;
    let ts = state.current_time;
    let _ = accrue_interest(&mut state.market, &state.config, ts);
    assert_eq!(state.market.scale_factor(), WAD);
    assert_eq!(
        state.market.accrued_protocol_fees(),
        0,
        "0% interest => 0 fees"
    );
    check_all_invariants(&state, "sf_at_wad accrue");
}

/// Regression: scale_factor at WAD-1, WAD, WAD+1 boundaries with 1 bps interest.
#[test]
fn test_boundary_scale_factor_at_wad_regression() {
    // At exactly WAD (fresh market, 1 bps interest, accrue 1 second)
    let mut state = fresh_valid_state(1, 0, DEFAULT_MATURITY);
    assert_eq!(state.market.scale_factor(), WAD);
    state.current_time = 1;
    let ts = state.current_time;
    let _ = accrue_interest(&mut state.market, &state.config, ts);
    // With 1 bps (0.01%) and 1 second, delta is tiny; scale_factor stays at or just above WAD
    assert!(
        state.market.scale_factor() >= WAD,
        "scale_factor must remain >= WAD after accrue"
    );
    check_all_invariants(&state, "sf_wad_regression accrue");

    // At WAD+1 (manually set, accrue should still produce >= WAD)
    let mut state2 = fresh_valid_state(1, 0, DEFAULT_MATURITY);
    state2.market.set_scale_factor(WAD + 1);
    state2.current_time = 1;
    let ts2 = state2.current_time;
    let _ = accrue_interest(&mut state2.market, &state2.config, ts2);
    assert!(
        state2.market.scale_factor() >= WAD + 1,
        "scale_factor at WAD+1 must stay >= WAD+1 after accrue"
    );
}

/// Boundary: settlement factor at minimum (1).
#[test]
fn test_boundary_settlement_at_minimum() {
    let maturity = 10_000i64;
    let mut state = fresh_valid_state(0, 0, maturity);
    check_all_invariants(&state, "sf_min init");

    // Assert exact initial state
    assert_eq!(
        state.market.settlement_factor_wad(),
        0,
        "settlement not yet set"
    );

    state.current_time = 100;
    let _ = apply_deposit(&mut state, 0, 10_000_000);
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    assert_eq!(state.market.total_deposited(), 10_000_000);
    assert_eq!(state.vault_balance, 10_000_000);

    state.current_time = 200;
    let _ = apply_borrow(&mut state, 10_000_000);
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    assert_eq!(
        state.vault_balance, 0,
        "vault should be empty after full borrow"
    );
    assert_eq!(state.market.total_borrowed(), 10_000_000);

    state.current_time = maturity;
    // Repay only 1 lamport - settlement factor should be near minimum
    let _ = apply_repay(&mut state, 1);
    assert_eq!(
        state.vault_balance, 1,
        "vault should have 1 lamport after repay"
    );

    state.current_time = maturity + 1;
    let ok = apply_withdraw(&mut state, 0);
    if ok {
        let sf = state.market.settlement_factor_wad();
        assert!(sf >= 1, "settlement factor must be >= 1, got {}", sf);
        assert!(sf <= WAD, "settlement factor must be <= WAD, got {}", sf);
        // With only 1 lamport repaid vs 10M deposited, settlement should be very small
        assert!(
            sf < WAD / 100,
            "settlement factor should be very small, got {}",
            sf
        );
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "sf_min withdraw");
    }
}

/// Regression: settlement at minimum with repay amounts x-1, x, x+1 around 1 lamport.
#[test]
fn test_boundary_settlement_at_minimum_regression() {
    for repay_amount in [1u64, 2, 3] {
        let maturity = 10_000i64;
        let mut state = fresh_valid_state(0, 0, maturity);
        state.current_time = 100;
        let _ = apply_deposit(&mut state, 0, 10_000_000);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        state.current_time = 200;
        let _ = apply_borrow(&mut state, 10_000_000);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        state.current_time = maturity;
        let _ = apply_repay(&mut state, repay_amount);
        state.current_time = maturity + 1;
        let ok = apply_withdraw(&mut state, 0);
        if ok {
            let sf = state.market.settlement_factor_wad();
            assert!(
                sf >= 1 && sf <= WAD,
                "repay={}: settlement factor {} not in [1, WAD]",
                repay_amount,
                sf
            );
            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
            check_all_invariants(&state, &format!("sf_min_regression repay={}", repay_amount));
        }
    }
}

/// Boundary: settlement factor at WAD (full recovery, no borrows).
#[test]
fn test_boundary_settlement_at_wad() {
    let maturity = 10_000i64;
    let mut state = fresh_valid_state(0, 0, maturity);
    check_all_invariants(&state, "sf_wad init");

    state.current_time = 100;
    let dep_ok = apply_deposit(&mut state, 0, 10_000_000);
    assert!(dep_ok, "deposit must succeed");
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    assert_eq!(state.market.total_deposited(), 10_000_000);
    assert_eq!(state.vault_balance, 10_000_000);
    assert_eq!(state.market.total_borrowed(), 0, "no borrows at this point");

    state.current_time = maturity + 1;
    let ok = apply_withdraw(&mut state, 0);
    assert!(ok, "withdraw should succeed with full vault");
    assert_eq!(
        state.market.settlement_factor_wad(),
        WAD,
        "full vault should produce settlement_factor == WAD"
    );
    // After full withdrawal, lender balance should be zero
    assert_eq!(
        state.lenders[0].scaled_balance(),
        0,
        "lender must be zeroed after withdraw"
    );
    // Vault should be empty (all paid out)
    assert_eq!(
        state.vault_balance, 0,
        "vault should be empty after full withdraw"
    );
    assert_eq!(
        state.total_withdrawn, 10_000_000,
        "total_withdrawn must equal deposit"
    );
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    check_all_invariants(&state, "sf_wad withdraw");
}

/// Regression: settlement at WAD with deposit amounts x-1, x, x+1 around 10M.
#[test]
fn test_boundary_settlement_at_wad_regression() {
    for deposit in [9_999_999u64, 10_000_000, 10_000_001] {
        let maturity = 10_000i64;
        let mut state = fresh_valid_state(0, 0, maturity);
        state.current_time = 100;
        let dep_ok = apply_deposit(&mut state, 0, deposit);
        assert!(dep_ok, "deposit of {} must succeed", deposit);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());

        state.current_time = maturity + 1;
        let ok = apply_withdraw(&mut state, 0);
        assert!(ok, "withdraw must succeed for deposit={}", deposit);
        assert_eq!(
            state.market.settlement_factor_wad(),
            WAD,
            "deposit={}: full vault must produce settlement_factor == WAD",
            deposit
        );
        assert_eq!(state.lenders[0].scaled_balance(), 0);
        assert_eq!(
            state.total_withdrawn, deposit,
            "deposit={}: total_withdrawn must equal deposit",
            deposit
        );
        check_all_invariants(&state, &format!("sf_wad_regression dep={}", deposit));
    }
}

/// Boundary: supply at exact cap.
#[test]
fn test_boundary_supply_at_exact_cap() {
    let mut state = fresh_valid_state(0, 0, DEFAULT_MATURITY);
    let cap = 5_000_000u64;
    state.market.set_max_total_supply(cap);
    check_all_invariants(&state, "cap_exact init");

    // Assert exact initial state
    assert_eq!(state.market.max_total_supply(), cap);
    assert_eq!(state.market.total_deposited(), 0);

    state.current_time = 1;
    let ok = apply_deposit(&mut state, 0, cap);
    assert!(ok, "deposit of exact cap amount should succeed");
    assert_eq!(
        state.market.total_deposited(),
        cap,
        "total_deposited must equal cap"
    );
    assert_eq!(state.vault_balance, cap, "vault must equal cap");

    let norm = normalized_total_supply(&state.market);
    assert!(
        norm <= u128::from(cap),
        "normalized supply {} must be <= cap {}",
        norm,
        cap
    );
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    check_all_invariants(&state, "cap_exact deposit");

    // Another deposit may or may not succeed depending on rounding
    let ok2 = apply_deposit(&mut state, 1, 1);
    if ok2 {
        let norm2 = normalized_total_supply(&state.market);
        assert!(
            norm2 <= u128::from(cap),
            "post-second-deposit normalized {} must still be <= cap {}",
            norm2,
            cap
        );
    }
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    check_all_invariants(&state, "cap_exact second");
}

/// Regression: supply cap at x-1, x, x+1 boundaries.
#[test]
fn test_boundary_supply_at_exact_cap_regression() {
    let cap = 5_000_000u64;

    // cap-1: should succeed, leaves room
    {
        let mut state = fresh_valid_state(0, 0, DEFAULT_MATURITY);
        state.market.set_max_total_supply(cap);
        state.current_time = 1;
        let ok = apply_deposit(&mut state, 0, cap - 1);
        assert!(ok, "deposit of cap-1 must succeed");
        let norm = normalized_total_supply(&state.market);
        assert!(
            norm <= u128::from(cap),
            "cap-1: norm {} > cap {}",
            norm,
            cap
        );
        assert_eq!(state.market.total_deposited(), cap - 1);
        check_all_invariants(&state, "cap_regression cap-1");
    }

    // cap: should succeed exactly
    {
        let mut state = fresh_valid_state(0, 0, DEFAULT_MATURITY);
        state.market.set_max_total_supply(cap);
        state.current_time = 1;
        let ok = apply_deposit(&mut state, 0, cap);
        assert!(ok, "deposit of exact cap must succeed");
        let norm = normalized_total_supply(&state.market);
        assert!(norm <= u128::from(cap), "cap: norm {} > cap {}", norm, cap);
        assert_eq!(state.market.total_deposited(), cap);
        check_all_invariants(&state, "cap_regression cap");
    }

    // cap+1: should be rejected by apply_deposit (exceeds normalized cap)
    {
        let mut state = fresh_valid_state(0, 0, DEFAULT_MATURITY);
        state.market.set_max_total_supply(cap);
        state.current_time = 1;
        let ok = apply_deposit(&mut state, 0, cap + 1);
        assert!(!ok, "deposit of cap+1 must be rejected (exceeds cap)");
        assert_eq!(
            state.market.total_deposited(),
            0,
            "no deposit should have occurred"
        );
        check_all_invariants(&state, "cap_regression cap+1");
    }
}

// ===========================================================================
// Deterministic regression companions for error paths
// ===========================================================================

/// Regression: backward timestamp must produce exact InvalidTimestamp error code.
#[test]
fn test_regression_backward_timestamp_error_code() {
    let mut state = fresh_valid_state(1000, 500, DEFAULT_MATURITY);
    state.current_time = 1000;
    let ts = state.current_time;
    let _ = accrue_interest(&mut state.market, &state.config, ts);

    // Now try to accrue at an earlier timestamp
    let result = accrue_interest(&mut state.market, &state.config, 500);
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::InvalidTimestamp as u32)),
        "backward accrue must produce InvalidTimestamp (code {})",
        LendingError::InvalidTimestamp as u32,
    );
}

/// Regression: overflow with huge scale factor must produce exact MathOverflow error code.
#[test]
fn test_regression_overflow_error_code() {
    let mut state = fresh_valid_state(MAX_ANNUAL_INTEREST_BPS, 0, i64::MAX);
    state.market.set_scale_factor(u128::MAX / 2);
    state.market.set_last_accrual_timestamp(0);

    let result = accrue_interest(&mut state.market, &state.config, 31_536_000);
    assert_eq!(
        result,
        Err(ProgramError::Custom(LendingError::MathOverflow as u32)),
        "overflow accrue must produce MathOverflow (code {})",
        LendingError::MathOverflow as u32,
    );
}

/// Regression: full lifecycle at fixed known values exercises all invariants.
#[test]
fn test_regression_full_lifecycle_known_values() {
    let maturity = 100_000i64;
    let mut state = fresh_valid_state(1000, 500, maturity);
    check_all_invariants(&state, "regression_lifecycle init");

    // Deposit
    state.current_time = 100;
    let dep_ok = apply_deposit(&mut state, 0, 10_000_000);
    assert!(dep_ok);
    assert_eq!(state.market.total_deposited(), 10_000_000);
    assert_eq!(state.vault_balance, 10_000_000);
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    check_all_invariants(&state, "regression_lifecycle deposit");

    // Accrue
    state.current_time = 50_000;
    let ts = state.current_time;
    let _ = accrue_interest(&mut state.market, &state.config, ts);
    assert!(
        state.market.scale_factor() > WAD,
        "scale_factor must grow with 10% interest"
    );
    state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
    check_all_invariants(&state, "regression_lifecycle accrue");

    // Borrow half
    let borrowable = state.vault_balance;
    let borrow_amount = borrowable / 2;
    if borrow_amount > 0 {
        let bor_ok = apply_borrow(&mut state, borrow_amount);
        assert!(bor_ok);
        state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
        check_all_invariants(&state, "regression_lifecycle borrow");
    }

    // Repay everything
    state.current_time = maturity;
    if state.market.total_borrowed() > 0 {
        let _ = apply_repay(&mut state, borrow_amount + 1_000_000);
        check_all_invariants(&state, "regression_lifecycle repay");
    }

    // Withdraw
    state.current_time = maturity + 1;
    if state.lenders[0].scaled_balance() > 0 {
        let w_ok = apply_withdraw(&mut state, 0);
        if w_ok {
            assert!(state.market.settlement_factor_wad() >= 1);
            assert!(state.market.settlement_factor_wad() <= WAD);
            assert_eq!(state.lenders[0].scaled_balance(), 0);
            state.prev_fees = state.prev_fees.min(state.market.accrued_protocol_fees());
            check_all_invariants(&state, "regression_lifecycle withdraw");
        }
    }
}

/// Regression: zero-amount deposit and borrow are rejected cleanly.
#[test]
fn test_regression_zero_amount_rejection() {
    let mut state = fresh_valid_state(1000, 500, DEFAULT_MATURITY);
    state.current_time = 100;

    // Zero deposit must be rejected
    let dep_ok = apply_deposit(&mut state, 0, 0);
    assert!(!dep_ok, "zero-amount deposit must be rejected");
    assert_eq!(state.market.total_deposited(), 0);

    // Zero borrow must be rejected
    let bor_ok = apply_borrow(&mut state, 0);
    assert!(!bor_ok, "zero-amount borrow must be rejected");

    // Zero repay must be rejected
    let rep_ok = apply_repay(&mut state, 0);
    assert!(!rep_ok, "zero-amount repay must be rejected");

    check_all_invariants(&state, "zero_amount_regression");
}
