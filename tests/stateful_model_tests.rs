//! Stateful model-based tests for the CoalesceFi Pinocchio lending protocol.
//!
//! Generates random sequences of operations (Deposit, Borrow, Repay,
//! AccrueInterest, Withdraw) via `proptest` and replays them against:
//!
//! 1. The **real on-chain logic** (`accrue_interest` + manual state updates
//!    that replicate what the processors do).
//! 2. A **simplified Rust model** that tracks state independently using plain
//!    `u128`/`u64` arithmetic.
//!
//! After every operation the test asserts critical protocol invariants hold on
//! the real state, and cross-checks the model against the on-chain state.

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
use coalesce::constants::{BPS, MAX_ANNUAL_INTEREST_BPS, MAX_FEE_RATE_BPS, SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{LenderPosition, Market, ProtocolConfig};

#[path = "common/math_oracle.rs"]
mod math_oracle;

const SECONDS_PER_DAY: i64 = 86_400;

fn mul_wad(a: u128, b: u128) -> Option<u128> {
    math_oracle::mul_wad_checked(a, b)
}

fn pow_wad(base: u128, exp: u32) -> Option<u128> {
    math_oracle::pow_wad_checked(base, exp)
}

fn growth_factor_wad(annual_interest_bps: u16, elapsed_seconds: i64) -> Option<u128> {
    math_oracle::growth_factor_wad_checked(annual_interest_bps, elapsed_seconds)
}

// ---------------------------------------------------------------------------
// Op enum -- the random operations proptest will generate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    /// Deposit `amount` USDC (scaled to a realistic range inside the runner).
    Deposit(u32),
    /// Borrow `amount` USDC from the vault.
    Borrow(u32),
    /// Repay `amount` USDC to the vault.
    Repay(u32),
    /// Advance the clock by `seconds` and accrue interest.
    Accrue(u16),
    /// Withdraw for lender at index `idx % num_lenders`.
    Withdraw(u8),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Weights: deposits and borrows more common to build up state
        3 => (1u32..=5_000_000u32).prop_map(Op::Deposit),
        2 => (1u32..=5_000_000u32).prop_map(Op::Borrow),
        2 => (1u32..=5_000_000u32).prop_map(Op::Repay),
        2 => (1u16..=3600u16).prop_map(Op::Accrue),
        1 => (0u8..=9u8).prop_map(Op::Withdraw),
    ]
}

// ---------------------------------------------------------------------------
// Simplified model -- tracks the same quantities with plain integers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ModelState {
    scale_factor: u128,
    scaled_total_supply: u128,
    accrued_protocol_fees: u64,
    total_deposited: u64,
    total_borrowed: u64,
    total_repaid: u64,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    annual_interest_bps: u16,
    fee_rate_bps: u16,
    /// vault_balance is an *independent* tracker in the model -- it matches
    /// the conceptual token account that the on-chain code reads.
    vault_balance: u64,
    /// Per-lender scaled balances (max 4 lenders for test simplicity).
    lender_scaled: [u128; 4],
    /// Settlement factor (0 = unsettled).
    settlement_factor_wad: u128,
    /// Total tokens paid out to lenders via withdrawals.
    total_withdrawn: u64,
}

impl ModelState {
    fn new(annual_interest_bps: u16, fee_rate_bps: u16, maturity_ts: i64) -> Self {
        Self {
            scale_factor: WAD,
            scaled_total_supply: 0,
            accrued_protocol_fees: 0,
            total_deposited: 0,
            total_borrowed: 0,
            total_repaid: 0,
            last_accrual_timestamp: 0,
            maturity_timestamp: maturity_ts,
            annual_interest_bps,
            fee_rate_bps,
            vault_balance: 0,
            lender_scaled: [0u128; 4],
            settlement_factor_wad: 0,
            total_withdrawn: 0,
        }
    }

    /// Mirror of `accrue_interest` logic. Returns `false` on overflow,
    /// matching the on-chain code's `Err(MathOverflow)` behavior.
    fn accrue(&mut self, current_ts: i64) -> bool {
        let effective_now = current_ts.min(self.maturity_timestamp);
        if effective_now < self.last_accrual_timestamp {
            return false;
        }
        let elapsed = effective_now - self.last_accrual_timestamp;
        if elapsed == 0 {
            return true;
        }

        let total_growth_wad = match growth_factor_wad(self.annual_interest_bps, elapsed) {
            Some(v) => v,
            None => return false,
        };
        let new_sf = match mul_wad(self.scale_factor, total_growth_wad) {
            Some(v) => v,
            None => return false,
        };
        let interest_delta_wad = match total_growth_wad.checked_sub(WAD) {
            Some(v) => v,
            None => return false,
        };

        // Fee accrual
        if self.fee_rate_bps > 0 {
            let fee_rate = u128::from(self.fee_rate_bps);
            let fee_delta_wad = match interest_delta_wad
                .checked_mul(fee_rate)
                .and_then(|v| v.checked_div(BPS))
            {
                Some(v) => v,
                None => return false,
            };

            // Use pre-accrual scale_factor (matches on-chain Finding 10 fix)
            let fee_normalized = match self
                .scaled_total_supply
                .checked_mul(self.scale_factor)
                .and_then(|v| v.checked_div(WAD))
                .and_then(|v| v.checked_mul(fee_delta_wad))
                .and_then(|v| v.checked_div(WAD))
            {
                Some(v) => v,
                None => return false,
            };

            let fee_u64 = match u64::try_from(fee_normalized) {
                Ok(v) => v,
                Err(_) => return false,
            };
            self.accrued_protocol_fees = match self.accrued_protocol_fees.checked_add(fee_u64) {
                Some(v) => v,
                None => return false,
            };
        }

        self.scale_factor = new_sf;
        self.last_accrual_timestamp = effective_now;
        true
    }

    fn deposit(&mut self, amount: u64, lender_idx: usize, current_ts: i64) -> bool {
        if !self.accrue(current_ts) {
            return false;
        }

        if self.scale_factor == 0 {
            return false;
        }
        let amount_u128 = u128::from(amount);
        let scaled = amount_u128
            .checked_mul(WAD)
            .and_then(|v| v.checked_div(self.scale_factor));
        let scaled = match scaled {
            Some(s) if s > 0 => s,
            _ => return false,
        };

        self.scaled_total_supply = self.scaled_total_supply.saturating_add(scaled);
        self.total_deposited = self.total_deposited.saturating_add(amount);
        self.vault_balance = self.vault_balance.saturating_add(amount);
        let idx = lender_idx % 4;
        self.lender_scaled[idx] = self.lender_scaled[idx].saturating_add(scaled);
        true
    }

    fn borrow(&mut self, amount: u64, current_ts: i64) -> bool {
        if !self.accrue(current_ts) {
            return false;
        }

        // COAL-L02: Full vault balance is borrowable (no fee reservation)
        if amount > self.vault_balance || amount == 0 {
            return false;
        }

        self.total_borrowed = self.total_borrowed.saturating_add(amount);
        self.vault_balance = self.vault_balance.saturating_sub(amount);
        true
    }

    fn repay(&mut self, amount: u64, current_ts: i64) -> bool {
        if amount == 0 {
            return false;
        }
        // Repay accrues with zero-fee config (mirrors processor/repay.rs)
        let saved_fee_bps = self.fee_rate_bps;
        self.fee_rate_bps = 0;
        let ok = self.accrue(current_ts);
        self.fee_rate_bps = saved_fee_bps;
        if !ok {
            return false;
        }

        self.total_repaid = self.total_repaid.saturating_add(amount);
        self.vault_balance = self.vault_balance.saturating_add(amount);
        true
    }

    fn withdraw(&mut self, lender_idx: usize, current_ts: i64) -> bool {
        if !self.accrue(current_ts) {
            return false;
        }
        let idx = lender_idx % 4;
        let scaled_balance = self.lender_scaled[idx];
        if scaled_balance == 0 {
            return false;
        }

        // Compute settlement factor if needed
        if self.settlement_factor_wad == 0 {
            let vault_u128 = u128::from(self.vault_balance);
            // COAL-C01: No fee reservation; use full vault balance
            let available = vault_u128;

            let total_normalized = self
                .scaled_total_supply
                .checked_mul(self.scale_factor)
                .and_then(|v| v.checked_div(WAD))
                .unwrap_or(0);

            let sf = if total_normalized == 0 {
                WAD
            } else {
                let raw = available
                    .checked_mul(WAD)
                    .and_then(|v| v.checked_div(total_normalized))
                    .unwrap_or(0);
                let capped = raw.min(WAD);
                capped.max(1)
            };
            self.settlement_factor_wad = sf;
        }

        // Compute payout
        let normalized = scaled_balance
            .checked_mul(self.scale_factor)
            .and_then(|v| v.checked_div(WAD))
            .unwrap_or(0);
        let payout_u128 = normalized
            .checked_mul(self.settlement_factor_wad)
            .and_then(|v| v.checked_div(WAD))
            .unwrap_or(0);
        let payout = u64::try_from(payout_u128).unwrap_or(u64::MAX);
        if payout == 0 || payout > self.vault_balance {
            return false;
        }

        self.lender_scaled[idx] = 0;
        self.scaled_total_supply = self.scaled_total_supply.saturating_sub(scaled_balance);
        self.vault_balance = self.vault_balance.saturating_sub(payout);
        self.total_withdrawn = self.total_withdrawn.saturating_add(payout);
        true
    }
}

// ---------------------------------------------------------------------------
// Real on-chain state harness
// ---------------------------------------------------------------------------

struct OnChainState {
    market: Market,
    config: ProtocolConfig,
    lenders: [LenderPosition; 4],
    /// We track vault balance manually since there is no real SPL token
    /// account in this unit-test harness.
    vault_balance: u64,
    /// Total tokens paid out to lenders via withdrawals.
    total_withdrawn: u64,
}

impl OnChainState {
    fn new(annual_interest_bps: u16, fee_rate_bps: u16, maturity_ts: i64) -> Self {
        let mut market = Market::zeroed();
        market.set_annual_interest_bps(annual_interest_bps);
        market.set_maturity_timestamp(maturity_ts);
        market.set_scale_factor(WAD);
        market.set_last_accrual_timestamp(0);
        market.set_max_total_supply(u64::MAX);

        let mut config = ProtocolConfig::zeroed();
        config.set_fee_rate_bps(fee_rate_bps);

        let lenders = [LenderPosition::zeroed(); 4];

        Self {
            market,
            config,
            lenders,
            vault_balance: 0,
            total_withdrawn: 0,
        }
    }

    fn accrue(&mut self, current_ts: i64) -> bool {
        accrue_interest(&mut self.market, &self.config, current_ts).is_ok()
    }

    fn deposit(&mut self, amount: u64, lender_idx: usize, current_ts: i64) -> bool {
        if amount == 0 {
            return false;
        }
        if accrue_interest(&mut self.market, &self.config, current_ts).is_err() {
            return false;
        }

        let sf = self.market.scale_factor();
        if sf == 0 {
            return false;
        }

        let amount_u128 = u128::from(amount);
        let scaled = match amount_u128.checked_mul(WAD).and_then(|v| v.checked_div(sf)) {
            Some(s) if s > 0 => s,
            _ => return false,
        };

        let new_total = match self.market.scaled_total_supply().checked_add(scaled) {
            Some(t) => t,
            None => return false,
        };

        self.market.set_scaled_total_supply(new_total);
        self.market
            .set_total_deposited(self.market.total_deposited().saturating_add(amount));
        self.vault_balance = self.vault_balance.saturating_add(amount);

        let idx = lender_idx % 4;
        let new_balance = self.lenders[idx].scaled_balance().saturating_add(scaled);
        self.lenders[idx].set_scaled_balance(new_balance);

        true
    }

    fn borrow(&mut self, amount: u64, current_ts: i64) -> bool {
        if amount == 0 {
            return false;
        }
        if accrue_interest(&mut self.market, &self.config, current_ts).is_err() {
            return false;
        }

        // COAL-L02: No fee reservation; use full vault balance
        if amount > self.vault_balance {
            return false;
        }

        self.market
            .set_total_borrowed(self.market.total_borrowed().saturating_add(amount));
        self.vault_balance = self.vault_balance.saturating_sub(amount);
        true
    }

    fn repay(&mut self, amount: u64, current_ts: i64) -> bool {
        if amount == 0 {
            return false;
        }
        // Repay uses zero-fee config for accrual (mirrors processor/repay.rs)
        let zero_config: ProtocolConfig = Zeroable::zeroed();
        if accrue_interest(&mut self.market, &zero_config, current_ts).is_err() {
            return false;
        }

        self.market
            .set_total_repaid(self.market.total_repaid().saturating_add(amount));
        self.vault_balance = self.vault_balance.saturating_add(amount);
        true
    }

    fn withdraw(&mut self, lender_idx: usize, current_ts: i64) -> bool {
        if accrue_interest(&mut self.market, &self.config, current_ts).is_err() {
            return false;
        }
        let idx = lender_idx % 4;
        let scaled_balance = self.lenders[idx].scaled_balance();
        if scaled_balance == 0 {
            return false;
        }

        // Settlement factor
        if self.market.settlement_factor_wad() == 0 {
            let vault_u128 = u128::from(self.vault_balance);
            // COAL-C01: No fee reservation; use full vault balance
            let available = vault_u128;

            let total_normalized = self
                .market
                .scaled_total_supply()
                .checked_mul(self.market.scale_factor())
                .and_then(|v| v.checked_div(WAD))
                .unwrap_or(0);

            let sf = if total_normalized == 0 {
                WAD
            } else {
                let raw = available
                    .checked_mul(WAD)
                    .and_then(|v| v.checked_div(total_normalized))
                    .unwrap_or(0);
                let capped = raw.min(WAD);
                capped.max(1)
            };
            self.market.set_settlement_factor_wad(sf);
        }

        // Compute payout
        let scale_factor = self.market.scale_factor();
        let settlement_factor = self.market.settlement_factor_wad();
        let normalized = scaled_balance
            .checked_mul(scale_factor)
            .and_then(|v| v.checked_div(WAD))
            .unwrap_or(0);
        let payout_u128 = normalized
            .checked_mul(settlement_factor)
            .and_then(|v| v.checked_div(WAD))
            .unwrap_or(0);
        let payout = match u64::try_from(payout_u128) {
            Ok(p) => p,
            Err(_) => return false,
        };
        if payout == 0 || payout > self.vault_balance {
            return false;
        }

        // Update lender
        self.lenders[idx].set_scaled_balance(0);

        // Update market
        let new_scaled_total = self
            .market
            .scaled_total_supply()
            .saturating_sub(scaled_balance);
        self.market.set_scaled_total_supply(new_scaled_total);
        self.vault_balance = self.vault_balance.saturating_sub(payout);
        self.total_withdrawn = self.total_withdrawn.saturating_add(payout);

        true
    }
}

// ---------------------------------------------------------------------------
// Invariant checkers
// ---------------------------------------------------------------------------

fn check_invariants(on_chain: &OnChainState, model: &ModelState, step: usize) {
    let m = &on_chain.market;

    // I-1: Solvency -- vault == deposited - borrowed + repaid - withdrawn
    //   Token transfers: deposits add, borrows subtract, repays add, withdrawals subtract.
    //   Interest accrual does not move tokens.
    let expected_vault = (m.total_deposited() as u128)
        .checked_sub(m.total_borrowed() as u128)
        .and_then(|v| v.checked_add(m.total_repaid() as u128))
        .and_then(|v| v.checked_sub(on_chain.total_withdrawn as u128));
    if let Some(expected) = expected_vault {
        assert_eq!(
            on_chain.vault_balance as u128,
            expected,
            "step {}: solvency invariant: vault_balance ({}) != deposited ({}) - borrowed ({}) + repaid ({}) - withdrawn ({})",
            step,
            on_chain.vault_balance,
            m.total_deposited(),
            m.total_borrowed(),
            m.total_repaid(),
            on_chain.total_withdrawn
        );
    }

    // I-2: scale_factor >= WAD (monotonically non-decreasing from WAD)
    assert!(
        m.scale_factor() >= WAD,
        "step {}: scale_factor ({}) < WAD ({})",
        step,
        m.scale_factor(),
        WAD
    );

    // I-3: accrued_protocol_fees never decreases
    // (Checked across steps in the main loop; here we confirm it is tracked.)

    // I-4: scaled_total_supply matches sum of lender positions
    let sum_lenders: u128 = on_chain.lenders.iter().map(|l| l.scaled_balance()).sum();
    assert_eq!(
        m.scaled_total_supply(),
        sum_lenders,
        "step {}: scaled_total_supply ({}) != sum of lender positions ({})",
        step,
        m.scaled_total_supply(),
        sum_lenders
    );

    // I-5: Settlement factor in [1, WAD] when set
    let sf_wad = m.settlement_factor_wad();
    if sf_wad != 0 {
        assert!(
            sf_wad >= 1 && sf_wad <= WAD,
            "step {}: settlement_factor ({}) not in [1, WAD]",
            step,
            sf_wad
        );
    }

    // I-6: Payout for any lender <= their normalized deposit amount
    //   (only meaningful when settlement factor is set)
    if sf_wad != 0 {
        for (i, lender) in on_chain.lenders.iter().enumerate() {
            let sb = lender.scaled_balance();
            if sb == 0 {
                continue;
            }
            let normalized = sb
                .checked_mul(m.scale_factor())
                .and_then(|v| v.checked_div(WAD));
            let payout =
                normalized.and_then(|n| n.checked_mul(sf_wad).and_then(|v| v.checked_div(WAD)));
            if let (Some(norm), Some(pay)) = (normalized, payout) {
                assert!(
                    pay <= norm,
                    "step {}: lender {} payout ({}) > normalized ({})",
                    step,
                    i,
                    pay,
                    norm
                );
            }
        }
    }

    // Cross-check model vs on-chain
    assert_eq!(
        m.scale_factor(),
        model.scale_factor,
        "step {}: scale_factor mismatch: on-chain={}, model={}",
        step,
        m.scale_factor(),
        model.scale_factor
    );
    assert_eq!(
        m.scaled_total_supply(),
        model.scaled_total_supply,
        "step {}: scaled_total_supply mismatch: on-chain={}, model={}",
        step,
        m.scaled_total_supply(),
        model.scaled_total_supply
    );
    assert_eq!(
        m.total_deposited(),
        model.total_deposited,
        "step {}: total_deposited mismatch: on-chain={}, model={}",
        step,
        m.total_deposited(),
        model.total_deposited
    );
    assert_eq!(
        m.total_borrowed(),
        model.total_borrowed,
        "step {}: total_borrowed mismatch: on-chain={}, model={}",
        step,
        m.total_borrowed(),
        model.total_borrowed
    );
    assert_eq!(
        m.total_repaid(),
        model.total_repaid,
        "step {}: total_repaid mismatch: on-chain={}, model={}",
        step,
        m.total_repaid(),
        model.total_repaid
    );
    assert_eq!(
        on_chain.vault_balance, model.vault_balance,
        "step {}: vault_balance mismatch: on-chain={}, model={}",
        step, on_chain.vault_balance, model.vault_balance
    );
    assert_eq!(
        m.accrued_protocol_fees(),
        model.accrued_protocol_fees,
        "step {}: accrued_protocol_fees mismatch: on-chain={}, model={}",
        step,
        m.accrued_protocol_fees(),
        model.accrued_protocol_fees
    );
    assert_eq!(
        on_chain.total_withdrawn, model.total_withdrawn,
        "step {}: total_withdrawn mismatch: on-chain={}, model={}",
        step, on_chain.total_withdrawn, model.total_withdrawn
    );
}

// ---------------------------------------------------------------------------
// Main proptest
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn test_stateful_model_based(
        annual_bps in 100u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        ops in prop::collection::vec(op_strategy(), 1..=50),
    ) {
        // Use a maturity far in the future so deposits/borrows are allowed
        // until we trigger withdrawals post-maturity.
        let maturity_ts: i64 = 1_000_000;

        let mut on_chain = OnChainState::new(annual_bps, fee_bps, maturity_ts);
        let mut model = ModelState::new(annual_bps, fee_bps, maturity_ts);

        // Track monotonicity of accrued_protocol_fees
        let mut prev_fees: u64 = 0;

        // Monotonic clock -- we advance time with each operation
        let mut clock: i64 = 1;
        // Lender round-robin counter
        let mut lender_counter: usize = 0;
        // Track whether any deposit has occurred (needed for withdraw to make sense)
        let mut any_deposits = false;
        // Track whether we have triggered the post-maturity phase
        let mut past_maturity = false;

        for (step, op) in ops.iter().enumerate() {
            match op {
                Op::Deposit(raw_amount) => {
                    if past_maturity {
                        // No deposits after maturity
                        continue;
                    }
                    let amount = (*raw_amount).max(1) as u64;
                    let lender_idx = lender_counter % 4;
                    lender_counter += 1;

                    let ok_onchain = on_chain.deposit(amount, lender_idx, clock);
                    let ok_model = model.deposit(amount, lender_idx, clock);
                    assert_eq!(
                        ok_onchain, ok_model,
                        "step {}: deposit({}) success mismatch: on-chain={}, model={}",
                        step, amount, ok_onchain, ok_model
                    );
                    if ok_onchain {
                        any_deposits = true;
                    }
                }

                Op::Borrow(raw_amount) => {
                    if past_maturity {
                        // No borrows after maturity
                        continue;
                    }
                    let amount = (*raw_amount).max(1) as u64;

                    let ok_onchain = on_chain.borrow(amount, clock);
                    let ok_model = model.borrow(amount, clock);
                    assert_eq!(
                        ok_onchain, ok_model,
                        "step {}: borrow({}) success mismatch: on-chain={}, model={}",
                        step, amount, ok_onchain, ok_model
                    );
                }

                Op::Repay(raw_amount) => {
                    let amount = (*raw_amount).max(1) as u64;

                    let ok_onchain = on_chain.repay(amount, clock);
                    let ok_model = model.repay(amount, clock);
                    assert_eq!(
                        ok_onchain, ok_model,
                        "step {}: repay({}) success mismatch: on-chain={}, model={}",
                        step, amount, ok_onchain, ok_model
                    );
                }

                Op::Accrue(seconds) => {
                    let delta = (*seconds).max(1) as i64;
                    clock = clock.saturating_add(delta);

                    let ok_onchain = on_chain.accrue(clock);
                    let ok_model = model.accrue(clock);
                    assert_eq!(
                        ok_onchain, ok_model,
                        "step {}: accrue({}) success mismatch: on-chain={}, model={}",
                        step, clock, ok_onchain, ok_model
                    );
                    if !ok_onchain {
                        continue;
                    }
                }

                Op::Withdraw(lender_idx_raw) => {
                    if !any_deposits {
                        continue;
                    }
                    // Push clock past maturity if not already
                    if !past_maturity {
                        clock = maturity_ts + 1;
                        past_maturity = true;
                    }

                    let lender_idx = (*lender_idx_raw as usize) % 4;
                    let ok_onchain = on_chain.withdraw(lender_idx, clock);
                    let ok_model = model.withdraw(lender_idx, clock);
                    assert_eq!(
                        ok_onchain, ok_model,
                        "step {}: withdraw(lender {}) success mismatch: on-chain={}, model={}",
                        step, lender_idx, ok_onchain, ok_model
                    );
                }
            }

            // --- Invariant checks ---
            check_invariants(&on_chain, &model, step);

            // I-3: accrued_protocol_fees never decreases
            let current_fees = on_chain.market.accrued_protocol_fees();
            assert!(
                current_fees >= prev_fees,
                "step {}: accrued_protocol_fees decreased: {} -> {}",
                step,
                prev_fees,
                current_fees
            );
            prev_fees = current_fees;
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministic regression: a known fixed scenario
// ---------------------------------------------------------------------------

#[test]
fn test_deterministic_deposit_borrow_repay_withdraw() {
    let maturity = 100_000i64;
    let mut on_chain = OnChainState::new(1000, 500, maturity); // 10% annual, 5% fee
    let mut model = ModelState::new(1000, 500, maturity);

    // Step 0: Deposit 1_000_000 at t=1
    assert!(on_chain.deposit(1_000_000, 0, 1));
    assert!(model.deposit(1_000_000, 0, 1));
    check_invariants(&on_chain, &model, 0);

    // Step 1: Accrue 10000 seconds
    assert!(on_chain.accrue(10_001));
    assert!(model.accrue(10_001));
    check_invariants(&on_chain, &model, 1);

    // Step 2: Borrow 500_000
    assert!(on_chain.borrow(500_000, 10_001));
    assert!(model.borrow(500_000, 10_001));
    check_invariants(&on_chain, &model, 2);

    // Step 3: Accrue to maturity
    assert!(on_chain.accrue(maturity));
    assert!(model.accrue(maturity));
    check_invariants(&on_chain, &model, 3);

    // Step 4: Repay everything borrowed
    assert!(on_chain.repay(500_000, maturity + 1));
    assert!(model.repay(500_000, maturity + 1));
    check_invariants(&on_chain, &model, 4);

    // Step 5: Withdraw lender 0
    assert!(on_chain.withdraw(0, maturity + 2));
    assert!(model.withdraw(0, maturity + 2));
    check_invariants(&on_chain, &model, 5);

    // After full withdrawal, lender 0 should have 0 scaled balance
    assert_eq!(on_chain.lenders[0].scaled_balance(), 0);
    assert_eq!(model.lender_scaled[0], 0);
    assert_eq!(on_chain.market.scaled_total_supply(), 0);
    assert_eq!(model.scaled_total_supply, 0);
}

// ---------------------------------------------------------------------------
// Edge case: no interest, no fees -- deposit and withdraw should be exact
// ---------------------------------------------------------------------------

#[test]
fn test_zero_interest_roundtrip() {
    let maturity = 1_000i64;
    let mut on_chain = OnChainState::new(0, 0, maturity);
    let mut model = ModelState::new(0, 0, maturity);

    let deposit_amount: u64 = 1_000_000;

    // Deposit
    assert!(on_chain.deposit(deposit_amount, 0, 1));
    assert!(model.deposit(deposit_amount, 0, 1));
    check_invariants(&on_chain, &model, 0);

    // Jump past maturity
    assert!(on_chain.accrue(maturity + 1));
    assert!(model.accrue(maturity + 1));
    check_invariants(&on_chain, &model, 1);

    // With 0% interest and no borrowing, scale_factor should still be WAD
    assert_eq!(on_chain.market.scale_factor(), WAD);
    assert_eq!(model.scale_factor, WAD);

    // Withdraw -- should get back exactly the deposit amount
    assert!(on_chain.withdraw(0, maturity + 2));
    assert!(model.withdraw(0, maturity + 2));
    check_invariants(&on_chain, &model, 2);

    // Settlement factor should be WAD (full recovery)
    assert_eq!(on_chain.market.settlement_factor_wad(), WAD);
    assert_eq!(model.settlement_factor_wad, WAD);

    // Vault should be empty
    assert_eq!(on_chain.vault_balance, 0);
    assert_eq!(model.vault_balance, 0);
}

// ---------------------------------------------------------------------------
// Edge case: borrow everything then partial repay => settlement < WAD
// ---------------------------------------------------------------------------

#[test]
fn test_partial_repay_settlement_below_wad() {
    let maturity = 10_000i64;
    let mut on_chain = OnChainState::new(0, 0, maturity); // 0% interest for simplicity
    let mut model = ModelState::new(0, 0, maturity);

    // Deposit 1M
    assert!(on_chain.deposit(1_000_000, 0, 1));
    assert!(model.deposit(1_000_000, 0, 1));

    // Borrow everything
    assert!(on_chain.borrow(1_000_000, 2));
    assert!(model.borrow(1_000_000, 2));
    check_invariants(&on_chain, &model, 1);

    // Repay only half
    assert!(on_chain.repay(500_000, maturity + 1));
    assert!(model.repay(500_000, maturity + 1));
    check_invariants(&on_chain, &model, 2);

    // Withdraw -- settlement factor should be < WAD (only 50% recovered)
    assert!(on_chain.withdraw(0, maturity + 2));
    assert!(model.withdraw(0, maturity + 2));
    check_invariants(&on_chain, &model, 3);

    let sf = on_chain.market.settlement_factor_wad();
    assert!(
        sf > 0 && sf < WAD,
        "settlement factor should be in (0, WAD)"
    );
    assert_eq!(sf, model.settlement_factor_wad);
}

// ---------------------------------------------------------------------------
// Edge case: multiple lenders get proportional payouts
// ---------------------------------------------------------------------------

fn run_full_recovery_two_lender_withdraw_order(first: usize, second: usize) -> [u64; 2] {
    let maturity = 10_000i64;
    let mut on_chain = OnChainState::new(0, 0, maturity);
    let mut model = ModelState::new(0, 0, maturity);

    // Lender 0 deposits 1M, lender 1 deposits 3M.
    assert!(on_chain.deposit(1_000_000, 0, 1));
    assert!(model.deposit(1_000_000, 0, 1));
    assert!(on_chain.deposit(3_000_000, 1, 1));
    assert!(model.deposit(3_000_000, 1, 1));
    check_invariants(&on_chain, &model, 0);

    // With 0% interest and same timestamp, scaled balances are exact.
    assert_eq!(on_chain.market.scale_factor(), WAD);
    assert_eq!(on_chain.lenders[0].scaled_balance(), 1_000_000);
    assert_eq!(on_chain.lenders[1].scaled_balance(), 3_000_000);
    assert_eq!(
        on_chain.lenders[1].scaled_balance(),
        on_chain.lenders[0].scaled_balance() * 3
    );

    // Borrow 2M and repay 2M, so settlement must be full (WAD).
    assert!(on_chain.borrow(2_000_000, 2));
    assert!(model.borrow(2_000_000, 2));
    assert!(on_chain.repay(2_000_000, maturity + 1));
    assert!(model.repay(2_000_000, maturity + 1));
    check_invariants(&on_chain, &model, 1);

    let mut payouts = [0u64; 2];
    for (step_offset, lender_idx) in [first, second].into_iter().enumerate() {
        let on_chain_vault_before = on_chain.vault_balance;
        let model_vault_before = model.vault_balance;

        assert!(on_chain.withdraw(lender_idx, maturity + 2));
        assert!(model.withdraw(lender_idx, maturity + 2));

        let payout_on_chain = on_chain_vault_before - on_chain.vault_balance;
        let payout_model = model_vault_before - model.vault_balance;
        assert_eq!(
            payout_on_chain, payout_model,
            "withdraw payout mismatch for lender {}",
            lender_idx
        );
        payouts[lender_idx] = payout_on_chain;

        // Full recovery path must pin settlement at WAD on first and subsequent withdrawals.
        assert_eq!(on_chain.market.settlement_factor_wad(), WAD);
        assert_eq!(model.settlement_factor_wad, WAD);
        check_invariants(&on_chain, &model, 2 + step_offset);
    }

    // Exact expected payouts and accounting.
    assert_eq!(payouts[0], 1_000_000);
    assert_eq!(payouts[1], 3_000_000);
    assert_eq!(payouts[1], payouts[0] * 3);
    assert_eq!(on_chain.vault_balance, 0);
    assert_eq!(model.vault_balance, 0);
    assert_eq!(on_chain.total_withdrawn, 4_000_000);
    assert_eq!(model.total_withdrawn, 4_000_000);

    // Re-withdraw after lender position is zero should fail without state mutation.
    let fail_idx = first;
    let on_chain_snapshot = (
        on_chain.vault_balance,
        on_chain.total_withdrawn,
        on_chain.market.scaled_total_supply(),
        on_chain.market.settlement_factor_wad(),
        on_chain.lenders[fail_idx].scaled_balance(),
    );
    let model_snapshot = (
        model.vault_balance,
        model.total_withdrawn,
        model.scaled_total_supply,
        model.settlement_factor_wad,
        model.lender_scaled[fail_idx % 4],
    );

    assert!(!on_chain.withdraw(fail_idx, maturity + 3));
    assert!(!model.withdraw(fail_idx, maturity + 3));

    assert_eq!(
        (
            on_chain.vault_balance,
            on_chain.total_withdrawn,
            on_chain.market.scaled_total_supply(),
            on_chain.market.settlement_factor_wad(),
            on_chain.lenders[fail_idx].scaled_balance(),
        ),
        on_chain_snapshot
    );
    assert_eq!(
        (
            model.vault_balance,
            model.total_withdrawn,
            model.scaled_total_supply,
            model.settlement_factor_wad,
            model.lender_scaled[fail_idx % 4],
        ),
        model_snapshot
    );

    payouts
}

#[test]
fn test_multiple_lenders_proportional() {
    // Tight bound: payout per lender is exact and independent of withdrawal order.
    let payouts_01 = run_full_recovery_two_lender_withdraw_order(0, 1);
    let payouts_10 = run_full_recovery_two_lender_withdraw_order(1, 0);

    assert_eq!(payouts_01, [1_000_000, 3_000_000]);
    assert_eq!(payouts_10, [1_000_000, 3_000_000]);
    assert_eq!(payouts_01, payouts_10);
}

// ---------------------------------------------------------------------------
// Stress: many small accruals produce scale_factor >= single large one
// ---------------------------------------------------------------------------

#[test]
fn test_compound_interest_many_steps() {
    let maturity = 1_000_000i64;
    let annual_bps: u16 = 1000; // 10%
    let horizon_seconds = 100_000i64;
    let step_seconds = 100i64;
    let step_count =
        usize::try_from(horizon_seconds / step_seconds).expect("step count fits usize");

    let step_growth_wad =
        growth_factor_wad(annual_bps, step_seconds).expect("step growth should not overflow");
    let single_growth_wad =
        growth_factor_wad(annual_bps, horizon_seconds).expect("single growth should not overflow");
    assert!(step_growth_wad > WAD, "step growth should be non-zero");

    let expected_compound_sf = |steps: usize| -> u128 {
        let mut sf = WAD;
        for _ in 0..steps {
            sf = mul_wad(sf, step_growth_wad).expect("compound sf should not overflow");
        }
        sf
    };

    // Single accrual: 0 -> 100_000 seconds.
    let mut oc_single = OnChainState::new(annual_bps, 0, maturity);
    assert!(oc_single.deposit(1_000_000, 0, 0));
    assert!(oc_single.accrue(horizon_seconds));
    let expected_single_sf =
        mul_wad(WAD, single_growth_wad).expect("single-step sf should not overflow");
    assert_eq!(oc_single.market.scale_factor(), expected_single_sf);
    assert_eq!(oc_single.market.last_accrual_timestamp(), horizon_seconds);

    // Many small accruals with explicit x-1/x/x+1 step-boundary checks.
    let mut oc_many = OnChainState::new(annual_bps, 0, maturity);
    assert!(oc_many.deposit(1_000_000, 0, 0));
    let mut t = 0i64;
    for _ in 0..step_count {
        t += step_seconds;
        assert!(oc_many.accrue(t));
    }
    let sf_1000 = oc_many.market.scale_factor();
    assert_eq!(sf_1000, expected_compound_sf(step_count));
    assert_eq!(oc_many.market.last_accrual_timestamp(), horizon_seconds);

    // x-1 boundary (999 steps / 99_900 seconds).
    let mut oc_many_minus_one = OnChainState::new(annual_bps, 0, maturity);
    assert!(oc_many_minus_one.deposit(1_000_000, 0, 0));
    let mut t_minus = 0i64;
    for _ in 0..(step_count - 1) {
        t_minus += step_seconds;
        assert!(oc_many_minus_one.accrue(t_minus));
    }
    let sf_999 = oc_many_minus_one.market.scale_factor();
    assert_eq!(sf_999, expected_compound_sf(step_count - 1));

    // x+1 boundary (1001 steps / 100_100 seconds).
    assert!(oc_many.accrue(horizon_seconds + step_seconds));
    let sf_1001 = oc_many.market.scale_factor();
    assert_eq!(sf_1001, expected_compound_sf(step_count + 1));
    let step_interest_delta_wad = step_growth_wad
        .checked_sub(WAD)
        .expect("step growth must exceed WAD");
    assert_eq!(
        sf_1001 - sf_1000,
        sf_1000
            .checked_mul(step_interest_delta_wad)
            .and_then(|v| v.checked_div(WAD))
            .expect("x+1 delta should not overflow")
    );

    // Tight ordering constraints.
    assert!(sf_999 < sf_1000, "x-1 should be strictly less than x");
    assert!(sf_1000 < sf_1001, "x should be strictly less than x+1");
    assert!(
        sf_1000 > oc_single.market.scale_factor(),
        "compound ({sf_1000}) should be > single-step ({})",
        oc_single.market.scale_factor()
    );

    // Re-accruing at identical timestamp must be a no-op.
    let sf_before_noop = oc_many.market.scale_factor();
    let ts_before_noop = oc_many.market.last_accrual_timestamp();
    assert!(oc_many.accrue(horizon_seconds + step_seconds));
    assert_eq!(oc_many.market.scale_factor(), sf_before_noop);
    assert_eq!(oc_many.market.last_accrual_timestamp(), ts_before_noop);
}
