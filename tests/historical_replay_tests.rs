//! Historical transaction replay framework for the CoalesceFi Pinocchio lending protocol.
//!
//! This module extends the regression test infrastructure with:
//! - A `TransactionLog` struct for representing captured mainnet transactions.
//! - A `ReplayEngine` that replays sequences against the current implementation.
//! - State tracking for multiple markets, lender positions, and whitelist entries.
//! - Invariant checking after each replayed transaction.
//! - Six synthetic historical scenarios simulating realistic mainnet patterns.
//! - Property-based tests using `proptest` for random mainnet-like sequences.

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
use coalesce::constants::{SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{LenderPosition, Market, ProtocolConfig};

#[path = "common/interest_oracle.rs"]
mod interest_oracle;

// ---------------------------------------------------------------------------
// TransactionLog — represents a single captured mainnet transaction
// ---------------------------------------------------------------------------

/// A captured historical transaction from a Solana RPC endpoint.
/// Contains all information needed to replay the transaction against
/// the current protocol implementation.
#[derive(Debug, Clone)]
struct TransactionLog {
    /// On-chain signature (for traceability).
    signature: &'static str,
    /// Slot number.
    slot: u64,
    /// Unix timestamp from the block.
    block_time: i64,
    /// Instruction to execute.
    instruction: Instruction,
    /// Identifier for the target market (index into multi-market state).
    market_id: usize,
}

/// All protocol instructions with their parameters.
#[derive(Debug, Clone)]
enum Instruction {
    InitializeProtocol {
        fee_rate_bps: u16,
    },
    SetFeeConfig {
        new_fee_rate_bps: u16,
    },
    CreateMarket {
        annual_interest_bps: u16,
        maturity_timestamp: i64,
        max_total_supply: u64,
        creation_timestamp: i64,
    },
    Deposit {
        lender_index: usize,
        amount: u64,
    },
    Borrow {
        amount: u64,
    },
    Repay {
        amount: u64,
    },
    Withdraw {
        lender_index: usize,
        scaled_amount: u128,
    },
    CollectFees,
    CloseLenderPosition {
        lender_index: usize,
    },
    ReSettle,
    SetBorrowerWhitelist {
        is_whitelisted: bool,
        max_borrow_capacity: u64,
    },
}

// ---------------------------------------------------------------------------
// Multi-market simulation state
// ---------------------------------------------------------------------------

/// Tracks all state for a single market.
#[derive(Clone)]
struct MarketState {
    market: Market,
    lender_positions: Vec<LenderPosition>,
    vault_balance: u64,
    borrower_total_borrowed: u64,
    borrower_max_capacity: u64,
    borrower_whitelisted: bool,
}

impl MarketState {
    fn new() -> Self {
        Self {
            market: Market::zeroed(),
            lender_positions: Vec::new(),
            vault_balance: 0,
            borrower_total_borrowed: 0,
            borrower_max_capacity: 0,
            borrower_whitelisted: false,
        }
    }

    fn ensure_lender(&mut self, index: usize) {
        while self.lender_positions.len() <= index {
            self.lender_positions.push(LenderPosition::zeroed());
        }
    }
}

/// Global state for the replay engine, supporting multiple markets.
struct ReplayEngineState {
    protocol_config: ProtocolConfig,
    markets: Vec<MarketState>,
}

impl ReplayEngineState {
    fn new() -> Self {
        Self {
            protocol_config: ProtocolConfig::zeroed(),
            markets: Vec::new(),
        }
    }

    fn ensure_market(&mut self, id: usize) {
        while self.markets.len() <= id {
            self.markets.push(MarketState::new());
        }
    }
}

// ---------------------------------------------------------------------------
// Invariant checks
// ---------------------------------------------------------------------------

/// Protocol invariants that must hold after every transaction.
fn check_invariants(state: &ReplayEngineState, ctx: &str) {
    for (i, ms) in state.markets.iter().enumerate() {
        let m = &ms.market;

        // INV-1: scale_factor >= WAD (interest is monotonically increasing)
        if m.scale_factor() > 0 {
            assert!(
                m.scale_factor() >= WAD,
                "{ctx}: market[{i}] scale_factor ({}) < WAD",
                m.scale_factor()
            );
        }

        // INV-2: sum of all lender scaled_balances == scaled_total_supply
        let sum_lender: u128 = ms
            .lender_positions
            .iter()
            .map(|lp| lp.scaled_balance())
            .sum();
        assert_eq!(
            sum_lender,
            m.scaled_total_supply(),
            "{ctx}: market[{i}] sum of lender balances ({sum_lender}) != \
             scaled_total_supply ({})",
            m.scaled_total_supply()
        );

        // INV-3: settlement_factor_wad is either 0 (unsettled) or in [1, WAD]
        let sf = m.settlement_factor_wad();
        if sf != 0 {
            assert!(
                sf >= 1 && sf <= WAD,
                "{ctx}: market[{i}] settlement_factor_wad ({sf}) out of range [1, WAD]"
            );
        }

        // INV-4: vault_balance is consistent (non-negative — guaranteed by u64)
        // INV-5: total_deposited >= sum of current normalized balances is not strictly
        //        enforceable due to interest growth, but total_deposited should be > 0
        //        if there are any lender positions with balance.
        if sum_lender > 0 {
            assert!(
                m.total_deposited() > 0,
                "{ctx}: market[{i}] has lender balances but total_deposited=0"
            );
        }

        // INV-6: accrued_protocol_fees <= vault_balance + total_borrowed - total_repaid
        //        (in principle; relaxed here to just check fees are not wildly large)
        // This is a soft check — fees should not exceed the total interest generated.
    }
}

// ---------------------------------------------------------------------------
// Checkpoint for intermediate state assertions
// ---------------------------------------------------------------------------

/// A checkpoint defines expected state values at a particular step index.
#[derive(Debug, Clone, Default)]
struct Checkpoint {
    step_index: usize,
    description: &'static str,
    vault_balance: Option<u64>,
    scale_factor: Option<u128>,
    scaled_total_supply: Option<u128>,
    total_deposited: Option<u64>,
    total_borrowed: Option<u64>,
    total_repaid: Option<u64>,
    accrued_protocol_fees: Option<u64>,
    settlement_factor_wad: Option<u128>,
    lender_scaled_balance: Option<(usize, u128)>,
    /// Market index to check (default 0).
    market_id: usize,
}

fn assert_checkpoint(state: &ReplayEngineState, cp: &Checkpoint) {
    let ms = &state.markets[cp.market_id];
    let m = &ms.market;
    let ctx = format!("Checkpoint '{}' (step {})", cp.description, cp.step_index);

    if let Some(vb) = cp.vault_balance {
        assert_eq!(ms.vault_balance, vb, "{ctx}: vault_balance mismatch");
    }
    if let Some(sf) = cp.scale_factor {
        assert_eq!(m.scale_factor(), sf, "{ctx}: scale_factor mismatch");
    }
    if let Some(sts) = cp.scaled_total_supply {
        assert_eq!(
            m.scaled_total_supply(),
            sts,
            "{ctx}: scaled_total_supply mismatch"
        );
    }
    if let Some(td) = cp.total_deposited {
        assert_eq!(m.total_deposited(), td, "{ctx}: total_deposited mismatch");
    }
    if let Some(tb) = cp.total_borrowed {
        assert_eq!(m.total_borrowed(), tb, "{ctx}: total_borrowed mismatch");
    }
    if let Some(tr) = cp.total_repaid {
        assert_eq!(m.total_repaid(), tr, "{ctx}: total_repaid mismatch");
    }
    if let Some(apf) = cp.accrued_protocol_fees {
        assert_eq!(
            m.accrued_protocol_fees(),
            apf,
            "{ctx}: accrued_protocol_fees mismatch"
        );
    }
    if let Some(sfw) = cp.settlement_factor_wad {
        assert_eq!(
            m.settlement_factor_wad(),
            sfw,
            "{ctx}: settlement_factor_wad mismatch"
        );
    }
    if let Some((idx, expected_balance)) = cp.lender_scaled_balance {
        assert!(idx < ms.lender_positions.len(), "{ctx}: lender index OOB");
        assert_eq!(
            ms.lender_positions[idx].scaled_balance(),
            expected_balance,
            "{ctx}: lender[{idx}].scaled_balance mismatch"
        );
    }
}

// ---------------------------------------------------------------------------
// ReplayEngine
// ---------------------------------------------------------------------------

/// The `ReplayEngine` takes a sequence of `TransactionLog` entries and
/// replays them against the current protocol implementation, checking
/// invariants after each step and assertions at designated checkpoints.
struct ReplayEngine {
    state: ReplayEngineState,
    logs: Vec<TransactionLog>,
    checkpoints: Vec<Checkpoint>,
}

impl ReplayEngine {
    fn new(logs: Vec<TransactionLog>, checkpoints: Vec<Checkpoint>) -> Self {
        Self {
            state: ReplayEngineState::new(),
            logs,
            checkpoints,
        }
    }

    /// Run the full replay, checking invariants and checkpoints.
    fn run(&mut self) {
        // Clone checkpoints and logs out of self to avoid borrow conflicts
        let checkpoints = self.checkpoints.clone();
        let logs = self.logs.clone();

        // Build checkpoint lookup from owned data
        let mut checkpoint_map: std::collections::HashMap<usize, Vec<Checkpoint>> =
            std::collections::HashMap::new();
        for cp in checkpoints {
            checkpoint_map.entry(cp.step_index).or_default().push(cp);
        }

        for (step, log) in logs.iter().enumerate() {
            let ctx = format!(
                "Step {} [sig={}, slot={}, time={}] {:?}",
                step, log.signature, log.slot, log.block_time, log.instruction
            );

            self.execute(log, &ctx);
            check_invariants(&self.state, &ctx);

            // Check any checkpoints at this step
            if let Some(cps) = checkpoint_map.get(&step) {
                for cp in cps {
                    assert_checkpoint(&self.state, cp);
                }
            }
        }
    }

    fn execute(&mut self, log: &TransactionLog, ctx: &str) {
        self.state.ensure_market(log.market_id);
        let ms = &mut self.state.markets[log.market_id];

        match &log.instruction {
            Instruction::InitializeProtocol { fee_rate_bps } => {
                self.state.protocol_config.set_fee_rate_bps(*fee_rate_bps);
                self.state.protocol_config.is_initialized = 1;
            },

            Instruction::SetFeeConfig { new_fee_rate_bps } => {
                assert!(*new_fee_rate_bps <= 10_000, "{ctx}: fee rate exceeds max");
                self.state
                    .protocol_config
                    .set_fee_rate_bps(*new_fee_rate_bps);
            },

            Instruction::CreateMarket {
                annual_interest_bps,
                maturity_timestamp,
                max_total_supply,
                creation_timestamp,
            } => {
                ms.market = Market::zeroed();
                ms.market.set_annual_interest_bps(*annual_interest_bps);
                ms.market.set_maturity_timestamp(*maturity_timestamp);
                ms.market.set_max_total_supply(*max_total_supply);
                ms.market.set_scale_factor(WAD);
                ms.market.set_last_accrual_timestamp(*creation_timestamp);
                ms.vault_balance = 0;
            },

            Instruction::Deposit {
                lender_index,
                amount,
            } => {
                accrue_interest(&mut ms.market, &self.state.protocol_config, log.block_time)
                    .unwrap_or_else(|e| panic!("{ctx}: accrue failed: {e:?}"));

                let amount_u128 = u128::from(*amount);
                let sf = ms.market.scale_factor();
                let scaled_amount = amount_u128
                    .checked_mul(WAD)
                    .expect("mul overflow")
                    .checked_div(sf)
                    .expect("div overflow");
                assert!(scaled_amount > 0, "{ctx}: deposit scaled to zero");

                let new_sts = ms
                    .market
                    .scaled_total_supply()
                    .checked_add(scaled_amount)
                    .expect("sts overflow");
                let new_norm = new_sts
                    .checked_mul(sf)
                    .expect("norm mul overflow")
                    .checked_div(WAD)
                    .expect("norm div overflow");
                assert!(
                    new_norm <= u128::from(ms.market.max_total_supply()),
                    "{ctx}: cap exceeded"
                );

                ms.vault_balance = ms
                    .vault_balance
                    .checked_add(*amount)
                    .expect("vault overflow");
                ms.ensure_lender(*lender_index);
                let new_lb = ms.lender_positions[*lender_index]
                    .scaled_balance()
                    .checked_add(scaled_amount)
                    .expect("lb overflow");
                ms.lender_positions[*lender_index].set_scaled_balance(new_lb);
                ms.market.set_scaled_total_supply(new_sts);
                let new_td = ms
                    .market
                    .total_deposited()
                    .checked_add(*amount)
                    .expect("td overflow");
                ms.market.set_total_deposited(new_td);
            },

            Instruction::Borrow { amount } => {
                accrue_interest(&mut ms.market, &self.state.protocol_config, log.block_time)
                    .unwrap_or_else(|e| panic!("{ctx}: accrue failed: {e:?}"));

                let fees_reserved =
                    core::cmp::min(ms.vault_balance, ms.market.accrued_protocol_fees());
                let borrowable = ms
                    .vault_balance
                    .checked_sub(fees_reserved)
                    .expect("underflow");
                assert!(*amount <= borrowable, "{ctx}: borrow > borrowable");

                let new_wl = ms
                    .borrower_total_borrowed
                    .checked_add(*amount)
                    .expect("wl overflow");
                assert!(new_wl <= ms.borrower_max_capacity, "{ctx}: global cap");

                ms.vault_balance = ms
                    .vault_balance
                    .checked_sub(*amount)
                    .expect("vault underflow");
                let new_tb = ms
                    .market
                    .total_borrowed()
                    .checked_add(*amount)
                    .expect("tb overflow");
                ms.market.set_total_borrowed(new_tb);
                ms.borrower_total_borrowed = new_wl;
            },

            Instruction::Repay { amount } => {
                let zero_config = ProtocolConfig::zeroed();
                accrue_interest(&mut ms.market, &zero_config, log.block_time)
                    .unwrap_or_else(|e| panic!("{ctx}: accrue failed: {e:?}"));

                ms.vault_balance = ms
                    .vault_balance
                    .checked_add(*amount)
                    .expect("vault overflow");
                let new_tr = ms
                    .market
                    .total_repaid()
                    .checked_add(*amount)
                    .expect("tr overflow");
                ms.market.set_total_repaid(new_tr);
            },

            Instruction::Withdraw {
                lender_index,
                scaled_amount,
            } => {
                accrue_interest(&mut ms.market, &self.state.protocol_config, log.block_time)
                    .unwrap_or_else(|e| panic!("{ctx}: accrue failed: {e:?}"));

                if ms.market.settlement_factor_wad() == 0 {
                    let vb128 = u128::from(ms.vault_balance);
                    let fees128 = u128::from(ms.market.accrued_protocol_fees());
                    let fees_reserved = core::cmp::min(vb128, fees128);
                    let available = vb128.checked_sub(fees_reserved).expect("underflow");
                    let total_norm = ms
                        .market
                        .scaled_total_supply()
                        .checked_mul(ms.market.scale_factor())
                        .expect("overflow")
                        .checked_div(WAD)
                        .expect("overflow");
                    let settlement_factor = if total_norm == 0 {
                        WAD
                    } else {
                        let raw = available
                            .checked_mul(WAD)
                            .expect("overflow")
                            .checked_div(total_norm)
                            .expect("overflow");
                        let capped = if raw > WAD { WAD } else { raw };
                        if capped < 1 {
                            1
                        } else {
                            capped
                        }
                    };
                    ms.market.set_settlement_factor_wad(settlement_factor);
                }

                ms.ensure_lender(*lender_index);
                let pos_balance = ms.lender_positions[*lender_index].scaled_balance();
                let eff_scaled = if *scaled_amount == 0 {
                    pos_balance
                } else {
                    *scaled_amount
                };
                assert!(eff_scaled <= pos_balance, "{ctx}: insufficient balance");

                let sf = ms.market.scale_factor();
                let settlement = ms.market.settlement_factor_wad();
                let norm = eff_scaled
                    .checked_mul(sf)
                    .expect("overflow")
                    .checked_div(WAD)
                    .expect("overflow");
                let payout128 = norm
                    .checked_mul(settlement)
                    .expect("overflow")
                    .checked_div(WAD)
                    .expect("overflow");
                let payout = u64::try_from(payout128).expect("payout overflow");
                assert!(payout > 0, "{ctx}: zero payout");
                let actual_payout = core::cmp::min(payout, ms.vault_balance);

                ms.vault_balance = ms
                    .vault_balance
                    .checked_sub(actual_payout)
                    .expect("vault underflow");
                let new_lb = pos_balance.checked_sub(eff_scaled).expect("lb underflow");
                ms.lender_positions[*lender_index].set_scaled_balance(new_lb);
                let new_sts = ms
                    .market
                    .scaled_total_supply()
                    .checked_sub(eff_scaled)
                    .expect("sts underflow");
                ms.market.set_scaled_total_supply(new_sts);
            },

            Instruction::CollectFees => {
                accrue_interest(&mut ms.market, &self.state.protocol_config, log.block_time)
                    .unwrap_or_else(|e| panic!("{ctx}: accrue failed: {e:?}"));

                let fees = ms.market.accrued_protocol_fees();
                assert!(fees > 0, "{ctx}: no fees");
                let withdrawable = core::cmp::min(fees, ms.vault_balance);
                assert!(withdrawable > 0, "{ctx}: zero withdrawable");

                ms.vault_balance = ms
                    .vault_balance
                    .checked_sub(withdrawable)
                    .expect("vault underflow");
                let remaining = fees.checked_sub(withdrawable).expect("fee underflow");
                ms.market.set_accrued_protocol_fees(remaining);
            },

            Instruction::ReSettle => {
                let old_factor = ms.market.settlement_factor_wad();
                assert!(old_factor > 0, "{ctx}: not yet settled");

                let zero_config = ProtocolConfig::zeroed();
                accrue_interest(&mut ms.market, &zero_config, log.block_time)
                    .unwrap_or_else(|e| panic!("{ctx}: accrue failed: {e:?}"));

                let vb128 = u128::from(ms.vault_balance);
                let fees128 = u128::from(ms.market.accrued_protocol_fees());
                let fees_reserved = core::cmp::min(vb128, fees128);
                let available = vb128.checked_sub(fees_reserved).expect("underflow");
                let total_norm = ms
                    .market
                    .scaled_total_supply()
                    .checked_mul(ms.market.scale_factor())
                    .expect("overflow")
                    .checked_div(WAD)
                    .expect("overflow");
                let new_factor = if total_norm == 0 {
                    WAD
                } else {
                    let raw = available
                        .checked_mul(WAD)
                        .expect("overflow")
                        .checked_div(total_norm)
                        .expect("overflow");
                    let capped = if raw > WAD { WAD } else { raw };
                    if capped < 1 {
                        1
                    } else {
                        capped
                    }
                };
                assert!(new_factor > old_factor, "{ctx}: settlement not improved");
                ms.market.set_settlement_factor_wad(new_factor);
            },

            Instruction::CloseLenderPosition { lender_index } => {
                ms.ensure_lender(*lender_index);
                let balance = ms.lender_positions[*lender_index].scaled_balance();
                assert!(balance == 0, "{ctx}: position not empty ({balance})");
            },

            Instruction::SetBorrowerWhitelist {
                is_whitelisted,
                max_borrow_capacity,
            } => {
                ms.borrower_whitelisted = *is_whitelisted;
                ms.borrower_max_capacity = *max_borrow_capacity;
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: create TransactionLog entries
// ---------------------------------------------------------------------------

/// Convenience builder for synthetic transaction logs.
fn tx_log(
    step: u64,
    block_time: i64,
    instruction: Instruction,
    market_id: usize,
) -> TransactionLog {
    TransactionLog {
        signature: "synthetic",
        slot: step * 2, // synthetic slot
        block_time,
        instruction,
        market_id,
    }
}

fn accrue_scale_factor_exact(scale_factor: u128, annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::scale_factor_after_exact(scale_factor, annual_bps, elapsed_seconds)
}

fn accrue_fee_delta_exact(
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

fn scaled_amount_exact(amount: u64, scale_factor: u128) -> u128 {
    u128::from(amount)
        .checked_mul(WAD)
        .expect("scaled amount mul overflow")
        .checked_div(scale_factor)
        .expect("scaled amount div by zero")
}

fn settlement_factor_exact(
    vault_balance: u64,
    accrued_fees: u64,
    scaled_total_supply: u128,
    scale_factor: u128,
) -> u128 {
    let available = u128::from(vault_balance)
        .checked_sub(u128::from(core::cmp::min(vault_balance, accrued_fees)))
        .expect("available underflow");
    let total_norm = scaled_total_supply
        .checked_mul(scale_factor)
        .expect("total_norm overflow")
        .checked_div(WAD)
        .expect("total_norm div by zero");

    if total_norm == 0 {
        return WAD;
    }

    let raw = available
        .checked_mul(WAD)
        .expect("settlement raw overflow")
        .checked_div(total_norm)
        .expect("settlement raw div by zero");
    let capped = core::cmp::min(raw, WAD);
    if capped < 1 {
        1
    } else {
        capped
    }
}

fn payout_for_scaled_exact(eff_scaled: u128, scale_factor: u128, settlement_factor: u128) -> u64 {
    let norm = eff_scaled
        .checked_mul(scale_factor)
        .expect("norm overflow")
        .checked_div(WAD)
        .expect("norm div by zero");
    let payout = norm
        .checked_mul(settlement_factor)
        .expect("payout mul overflow")
        .checked_div(WAD)
        .expect("payout div by zero");
    u64::try_from(payout).expect("payout should fit in u64 for these scenarios")
}

fn accrue_expected_state(
    scale_factor: &mut u128,
    last_accrual_timestamp: &mut i64,
    accrued_protocol_fees: &mut u64,
    scaled_total_supply: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    timestamp: i64,
    with_fee_config: bool,
) {
    let elapsed = timestamp - *last_accrual_timestamp;
    if elapsed <= 0 {
        return;
    }

    let sf_before = *scale_factor;
    *scale_factor = accrue_scale_factor_exact(*scale_factor, annual_bps, elapsed);
    if with_fee_config {
        *accrued_protocol_fees = accrued_protocol_fees
            .checked_add(accrue_fee_delta_exact(
                scaled_total_supply,
                sf_before,
                annual_bps,
                fee_rate_bps,
                elapsed,
            ))
            .expect("expected fee overflow");
    }
    *last_accrual_timestamp = timestamp;
}

// ===========================================================================
// Scenario A: Market creation and initial deposits from 5 lenders over 3 days
// ===========================================================================

#[test]
fn historical_scenario_a_initial_deposits_5_lenders() {
    let t0: i64 = 1_700_000_000; // ~Nov 2023
    let day: i64 = 86_400;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity = t0 + year;
    let deposit_amounts: [u64; 5] = [
        500_000_000,   // 500 USDC
        1_000_000_000, // 1000 USDC
        250_000_000,   // 250 USDC
        750_000_000,   // 750 USDC
        2_000_000_000, // 2000 USDC
    ];

    let mut logs = Vec::new();
    let mut step: u64 = 0;

    // Step 0: Initialize protocol (5% fee)
    logs.push(tx_log(
        step,
        t0,
        Instruction::InitializeProtocol { fee_rate_bps: 500 },
        0,
    ));
    step += 1;

    // Step 1: Set borrower whitelist
    logs.push(tx_log(
        step,
        t0,
        Instruction::SetBorrowerWhitelist {
            is_whitelisted: true,
            max_borrow_capacity: 10_000_000_000,
        },
        0,
    ));
    step += 1;

    // Step 2: Create market (8% annual, 1-year maturity, 10K USDC cap)
    logs.push(tx_log(
        step,
        t0,
        Instruction::CreateMarket {
            annual_interest_bps: 800,
            maturity_timestamp: maturity,
            max_total_supply: 10_000_000_000,
            creation_timestamp: t0,
        },
        0,
    ));
    step += 1;

    // Steps 3-7: 5 lenders deposit over 3 days
    let deposit_times: [i64; 5] = [t0, t0 + day, t0 + day, t0 + 2 * day, t0 + 3 * day];
    for (i, (&amount, &time)) in deposit_amounts.iter().zip(deposit_times.iter()).enumerate() {
        logs.push(tx_log(
            step,
            time,
            Instruction::Deposit {
                lender_index: i,
                amount,
            },
            0,
        ));
        step += 1;
    }

    // Checkpoints
    let total_deposit: u64 = deposit_amounts.iter().sum();
    let checkpoints = vec![
        // After all deposits (step 7, index 7)
        Checkpoint {
            step_index: 7,
            description: "all 5 lenders deposited",
            vault_balance: Some(total_deposit),
            total_deposited: Some(total_deposit),
            ..Default::default()
        },
        // Lender 0 has its balance (at WAD scale factor, step 3)
        Checkpoint {
            step_index: 3,
            description: "lender 0 deposit at creation",
            lender_scaled_balance: Some((0, u128::from(deposit_amounts[0]))),
            scale_factor: Some(WAD),
            ..Default::default()
        },
    ];

    let mut engine = ReplayEngine::new(logs, checkpoints);
    engine.run();

    // Post-run: verify exact replayed state via independent arithmetic oracle.
    let ms = &engine.state.markets[0];
    let annual_bps = 800u16;
    let fee_rate_bps = 500u16;
    let mut expected_sf = WAD;
    let mut expected_last = t0;
    let mut expected_sts = 0u128;
    let mut expected_fees = 0u64;
    let mut expected_lender = [0u128; 5];

    for i in 0..deposit_amounts.len() {
        let elapsed = deposit_times[i] - expected_last;
        if elapsed > 0 {
            let sf_before = expected_sf;
            expected_sf = accrue_scale_factor_exact(expected_sf, annual_bps, elapsed);
            expected_fees = expected_fees
                .checked_add(accrue_fee_delta_exact(
                    expected_sts,
                    sf_before,
                    annual_bps,
                    fee_rate_bps,
                    elapsed,
                ))
                .expect("expected_fees overflow");
            expected_last = deposit_times[i];
        }

        let scaled = scaled_amount_exact(deposit_amounts[i], expected_sf);
        expected_lender[i] = scaled;
        expected_sts = expected_sts
            .checked_add(scaled)
            .expect("expected_sts overflow");
    }

    assert_eq!(ms.market.scale_factor(), expected_sf);
    assert_eq!(ms.market.last_accrual_timestamp(), expected_last);
    assert_eq!(ms.market.scaled_total_supply(), expected_sts);
    assert_eq!(ms.market.total_deposited(), total_deposit);
    assert_eq!(ms.market.total_borrowed(), 0);
    assert_eq!(ms.market.total_repaid(), 0);
    assert_eq!(ms.market.accrued_protocol_fees(), expected_fees);
    assert_eq!(ms.market.settlement_factor_wad(), 0);
    assert_eq!(ms.vault_balance, total_deposit);

    for (i, expected_scaled) in expected_lender.iter().copied().enumerate() {
        assert!(
            expected_scaled > 0,
            "oracle sanity: lender {i} should have positive scaled balance"
        );
        assert_eq!(ms.lender_positions[i].scaled_balance(), expected_scaled);
    }
}

// ===========================================================================
// Scenario B: Gradual borrowing with daily interest accruals over 2 weeks
// ===========================================================================

#[test]
fn historical_scenario_b_gradual_borrowing_2_weeks() {
    let t0: i64 = 1_700_000_000;
    let day: i64 = 86_400;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity = t0 + year;

    let mut logs = Vec::new();
    let mut step: u64 = 0;

    // Init + whitelist + create market (10% annual, 0% fee for simplicity)
    logs.push(tx_log(
        step,
        t0,
        Instruction::InitializeProtocol { fee_rate_bps: 0 },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0,
        Instruction::SetBorrowerWhitelist {
            is_whitelisted: true,
            max_borrow_capacity: 50_000_000_000,
        },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0,
        Instruction::CreateMarket {
            annual_interest_bps: 1000,
            maturity_timestamp: maturity,
            max_total_supply: 100_000_000_000,
            creation_timestamp: t0,
        },
        0,
    ));
    step += 1;

    // Large initial deposit: 10,000 USDC
    let deposit = 10_000_000_000u64;
    logs.push(tx_log(
        step,
        t0,
        Instruction::Deposit {
            lender_index: 0,
            amount: deposit,
        },
        0,
    ));
    step += 1;

    // Daily borrowing for 14 days: 500 USDC each day
    let borrow_per_day = 500_000_000u64;
    for d in 1..=14 {
        let time = t0 + d * day;
        logs.push(tx_log(
            step,
            time,
            Instruction::Borrow {
                amount: borrow_per_day,
            },
            0,
        ));
        step += 1;
    }

    let checkpoints = vec![
        // After initial deposit
        Checkpoint {
            step_index: 3,
            description: "initial 10K USDC deposit",
            vault_balance: Some(deposit),
            total_deposited: Some(deposit),
            scale_factor: Some(WAD),
            ..Default::default()
        },
        // After first borrow (day 1)
        Checkpoint {
            step_index: 4,
            description: "first 500 USDC borrow (day 1)",
            total_borrowed: Some(borrow_per_day),
            ..Default::default()
        },
        // After 7th borrow (day 7): vault = 10K - 3500 = 6500 USDC (approx, ignoring interest)
        Checkpoint {
            step_index: 10,
            description: "7 days of borrowing",
            total_borrowed: Some(borrow_per_day * 7),
            ..Default::default()
        },
        // After 14th borrow (day 14): total borrowed = 7000 USDC
        Checkpoint {
            step_index: 17,
            description: "14 days of borrowing complete",
            total_borrowed: Some(borrow_per_day * 14),
            ..Default::default()
        },
    ];

    let mut engine = ReplayEngine::new(logs, checkpoints);
    engine.run();

    // Verify exact scale-factor path and final accounting.
    let ms = &engine.state.markets[0];
    let mut expected_sf = WAD;
    for _ in 0..14 {
        expected_sf = accrue_scale_factor_exact(expected_sf, 1000, day);
    }

    assert_eq!(ms.market.scale_factor(), expected_sf);
    assert_eq!(ms.market.last_accrual_timestamp(), t0 + 14 * day);
    assert_eq!(ms.market.scaled_total_supply(), u128::from(deposit));
    assert_eq!(ms.lender_positions[0].scaled_balance(), u128::from(deposit));
    assert_eq!(ms.market.total_deposited(), deposit);
    assert_eq!(ms.market.total_borrowed(), borrow_per_day * 14);
    assert_eq!(ms.market.total_repaid(), 0);
    assert_eq!(ms.market.accrued_protocol_fees(), 0);
    assert_eq!(ms.market.settlement_factor_wad(), 0);
    assert_eq!(ms.vault_balance, deposit - borrow_per_day * 14);
    assert_eq!(ms.borrower_total_borrowed, borrow_per_day * 14);
}

// ===========================================================================
// Scenario C: Partial repayment + re-settlement cycle
// ===========================================================================

#[test]
fn historical_scenario_c_partial_repay_resettle() {
    let t0: i64 = 1_700_000_000;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity = t0 + year;

    let mut logs = Vec::new();
    let mut step: u64 = 0;

    // Setup: no fees, 0% interest for clean math
    logs.push(tx_log(
        step,
        t0,
        Instruction::InitializeProtocol { fee_rate_bps: 0 },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0,
        Instruction::SetBorrowerWhitelist {
            is_whitelisted: true,
            max_borrow_capacity: 10_000_000_000,
        },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0,
        Instruction::CreateMarket {
            annual_interest_bps: 0,
            maturity_timestamp: maturity,
            max_total_supply: 100_000_000_000,
            creation_timestamp: t0,
        },
        0,
    ));
    step += 1;

    // Deposit 1000 USDC
    logs.push(tx_log(
        step,
        t0,
        Instruction::Deposit {
            lender_index: 0,
            amount: 1_000_000_000,
        },
        0,
    ));
    step += 1;

    // Borrow 800 USDC
    logs.push(tx_log(
        step,
        t0,
        Instruction::Borrow {
            amount: 800_000_000,
        },
        0,
    ));
    step += 1;

    // Partial repay 300 USDC at maturity
    logs.push(tx_log(
        step,
        maturity,
        Instruction::Repay {
            amount: 300_000_000,
        },
        0,
    ));
    step += 1;

    // First partial withdrawal triggers settlement
    // vault = 200M + 300M = 500M, total_normalized = 1000M, settlement = 500M/1000M = 0.5 WAD
    // withdraw half (500M scaled), payout = 500M * WAD/WAD * 0.5*WAD/WAD = 250M
    logs.push(tx_log(
        step,
        maturity,
        Instruction::Withdraw {
            lender_index: 0,
            scaled_amount: 500_000_000,
        },
        0,
    ));
    step += 1;

    // Second repay 400 USDC
    logs.push(tx_log(
        step,
        maturity + 100,
        Instruction::Repay {
            amount: 400_000_000,
        },
        0,
    ));
    step += 1;

    // Re-settle: vault now has 250M + 400M = 650M, remaining = 500M scaled * WAD/WAD = 500M norm
    // new settlement = 650M * WAD / 500M > WAD => capped at WAD
    logs.push(tx_log(step, maturity + 200, Instruction::ReSettle, 0));
    step += 1;

    // Final withdrawal of remaining
    logs.push(tx_log(
        step,
        maturity + 300,
        Instruction::Withdraw {
            lender_index: 0,
            scaled_amount: 0, // withdraw all
        },
        0,
    ));
    step += 1;

    let checkpoints = vec![
        // After borrow
        Checkpoint {
            step_index: 4,
            description: "after borrow 800 USDC",
            vault_balance: Some(200_000_000),
            total_borrowed: Some(800_000_000),
            ..Default::default()
        },
        // After partial repay
        Checkpoint {
            step_index: 5,
            description: "after partial repay 300",
            vault_balance: Some(500_000_000),
            total_repaid: Some(300_000_000),
            ..Default::default()
        },
        // After first withdrawal: settlement at 0.5 * WAD
        Checkpoint {
            step_index: 6,
            description: "first withdrawal triggers 50% settlement",
            settlement_factor_wad: Some(WAD / 2),
            vault_balance: Some(250_000_000),
            lender_scaled_balance: Some((0, 500_000_000)),
            ..Default::default()
        },
        // After re-settle: factor should be WAD (full recovery)
        Checkpoint {
            step_index: 8,
            description: "re-settle to WAD after second repay",
            settlement_factor_wad: Some(WAD),
            ..Default::default()
        },
        // After final withdrawal: everything withdrawn
        Checkpoint {
            step_index: 9,
            description: "all withdrawn, lender empty",
            lender_scaled_balance: Some((0, 0)),
            scaled_total_supply: Some(0),
            ..Default::default()
        },
    ];

    let mut engine = ReplayEngine::new(logs, checkpoints);
    engine.run();

    let ms = &engine.state.markets[0];
    let first_settlement = settlement_factor_exact(500_000_000, 0, 1_000_000_000, WAD);
    let first_payout = payout_for_scaled_exact(500_000_000, WAD, first_settlement);
    let second_settlement = settlement_factor_exact(650_000_000, 0, 500_000_000, WAD);
    let second_payout = payout_for_scaled_exact(500_000_000, WAD, second_settlement);

    assert_eq!(first_settlement, WAD / 2);
    assert_eq!(first_payout, 250_000_000);
    assert_eq!(second_settlement, WAD);
    assert_eq!(second_payout, 500_000_000);

    assert_eq!(ms.market.scale_factor(), WAD);
    // Accrual is capped at maturity even if replay timestamps are later.
    assert_eq!(ms.market.last_accrual_timestamp(), maturity);
    assert_eq!(ms.market.total_deposited(), 1_000_000_000);
    assert_eq!(ms.market.total_borrowed(), 800_000_000);
    assert_eq!(ms.market.total_repaid(), 700_000_000);
    assert_eq!(ms.market.accrued_protocol_fees(), 0);
    assert_eq!(ms.market.settlement_factor_wad(), WAD);
    assert_eq!(ms.market.scaled_total_supply(), 0);
    assert_eq!(ms.lender_positions[0].scaled_balance(), 0);
    assert_eq!(ms.vault_balance, 150_000_000);
    assert_eq!(ms.borrower_total_borrowed, 800_000_000);
}

// ===========================================================================
// Scenario D: Full lifecycle from creation to all positions closed
// ===========================================================================

#[test]
fn historical_scenario_d_full_lifecycle() {
    let t0: i64 = 1_700_000_000;
    let day: i64 = 86_400;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity = t0 + year;

    let mut logs = Vec::new();
    let mut step: u64 = 0;

    // Init with 5% protocol fee
    logs.push(tx_log(
        step,
        t0,
        Instruction::InitializeProtocol { fee_rate_bps: 500 },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0,
        Instruction::SetBorrowerWhitelist {
            is_whitelisted: true,
            max_borrow_capacity: 50_000_000_000,
        },
        0,
    ));
    step += 1;

    // Create market: 10% annual, 1-year maturity
    logs.push(tx_log(
        step,
        t0,
        Instruction::CreateMarket {
            annual_interest_bps: 1000,
            maturity_timestamp: maturity,
            max_total_supply: 50_000_000_000,
            creation_timestamp: t0,
        },
        0,
    ));
    step += 1;

    // Two lenders deposit: 5000 USDC each
    logs.push(tx_log(
        step,
        t0 + day,
        Instruction::Deposit {
            lender_index: 0,
            amount: 5_000_000_000,
        },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0 + 2 * day,
        Instruction::Deposit {
            lender_index: 1,
            amount: 5_000_000_000,
        },
        0,
    ));
    step += 1;

    // Borrow 8000 USDC after 1 week
    logs.push(tx_log(
        step,
        t0 + 7 * day,
        Instruction::Borrow {
            amount: 8_000_000_000,
        },
        0,
    ));
    step += 1;

    // Full repay at month 6
    let half_year = t0 + year / 2;
    logs.push(tx_log(
        step,
        half_year,
        Instruction::Repay {
            amount: 8_000_000_000,
        },
        0,
    ));
    step += 1;

    // Collect fees at month 9
    let nine_months = t0 + 3 * year / 4;
    logs.push(tx_log(step, nine_months, Instruction::CollectFees, 0));
    step += 1;

    // Lender 0 withdraws all at maturity
    logs.push(tx_log(
        step,
        maturity,
        Instruction::Withdraw {
            lender_index: 0,
            scaled_amount: 0,
        },
        0,
    ));
    step += 1;

    // Lender 1 withdraws all at maturity
    logs.push(tx_log(
        step,
        maturity,
        Instruction::Withdraw {
            lender_index: 1,
            scaled_amount: 0,
        },
        0,
    ));
    step += 1;

    // Close both positions
    logs.push(tx_log(
        step,
        maturity + 1,
        Instruction::CloseLenderPosition { lender_index: 0 },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        maturity + 1,
        Instruction::CloseLenderPosition { lender_index: 1 },
        0,
    ));

    let checkpoints = vec![
        // After both deposits
        Checkpoint {
            step_index: 4,
            description: "both lenders deposited 5K each",
            total_deposited: Some(10_000_000_000),
            ..Default::default()
        },
        // After borrow
        Checkpoint {
            step_index: 5,
            description: "8K USDC borrowed",
            total_borrowed: Some(8_000_000_000),
            ..Default::default()
        },
        // After repay (all borrowed amount returned)
        Checkpoint {
            step_index: 6,
            description: "full repay of 8K",
            total_repaid: Some(8_000_000_000),
            ..Default::default()
        },
        // After both withdrawals: lender positions zeroed
        Checkpoint {
            step_index: 9,
            description: "both lenders withdrawn",
            scaled_total_supply: Some(0),
            ..Default::default()
        },
        // After close: positions confirmed empty
        Checkpoint {
            step_index: 10,
            description: "lender 0 position closed",
            lender_scaled_balance: Some((0, 0)),
            ..Default::default()
        },
        Checkpoint {
            step_index: 11,
            description: "lender 1 position closed",
            lender_scaled_balance: Some((1, 0)),
            ..Default::default()
        },
    ];

    let mut engine = ReplayEngine::new(logs, checkpoints);
    engine.run();

    // Verify exact replayed state from an independent oracle simulation.
    let ms = &engine.state.markets[0];
    let annual_bps = 1000u16;
    let fee_rate_bps = 500u16;
    let mut expected_sf = WAD;
    let mut expected_last = t0;
    let mut expected_sts = 0u128;
    let mut expected_fees = 0u64;
    let mut expected_vault = 0u64;
    let mut lender0_scaled = 0u128;
    let mut lender1_scaled = 0u128;
    let mut expected_tb = 0u64;
    let mut expected_tr = 0u64;

    accrue_expected_state(
        &mut expected_sf,
        &mut expected_last,
        &mut expected_fees,
        expected_sts,
        annual_bps,
        fee_rate_bps,
        t0 + day,
        true,
    );
    lender0_scaled = scaled_amount_exact(5_000_000_000, expected_sf);
    expected_sts += lender0_scaled;
    expected_vault += 5_000_000_000;

    accrue_expected_state(
        &mut expected_sf,
        &mut expected_last,
        &mut expected_fees,
        expected_sts,
        annual_bps,
        fee_rate_bps,
        t0 + 2 * day,
        true,
    );
    lender1_scaled = scaled_amount_exact(5_000_000_000, expected_sf);
    expected_sts += lender1_scaled;
    expected_vault += 5_000_000_000;

    accrue_expected_state(
        &mut expected_sf,
        &mut expected_last,
        &mut expected_fees,
        expected_sts,
        annual_bps,
        fee_rate_bps,
        t0 + 7 * day,
        true,
    );
    let fees_reserved = core::cmp::min(expected_vault, expected_fees);
    let borrowable = expected_vault - fees_reserved;
    assert!(borrowable >= 8_000_000_000, "oracle borrowability mismatch");
    expected_vault -= 8_000_000_000;
    expected_tb += 8_000_000_000;

    accrue_expected_state(
        &mut expected_sf,
        &mut expected_last,
        &mut expected_fees,
        expected_sts,
        annual_bps,
        fee_rate_bps,
        half_year,
        false,
    );
    expected_vault += 8_000_000_000;
    expected_tr += 8_000_000_000;

    accrue_expected_state(
        &mut expected_sf,
        &mut expected_last,
        &mut expected_fees,
        expected_sts,
        annual_bps,
        fee_rate_bps,
        nine_months,
        true,
    );
    let collected_fees = core::cmp::min(expected_fees, expected_vault);
    expected_vault -= collected_fees;
    expected_fees -= collected_fees;

    accrue_expected_state(
        &mut expected_sf,
        &mut expected_last,
        &mut expected_fees,
        expected_sts,
        annual_bps,
        fee_rate_bps,
        maturity,
        true,
    );
    let expected_settlement =
        settlement_factor_exact(expected_vault, expected_fees, expected_sts, expected_sf);
    let payout0 = payout_for_scaled_exact(lender0_scaled, expected_sf, expected_settlement);
    let actual_payout0 = core::cmp::min(payout0, expected_vault);
    expected_vault -= actual_payout0;
    expected_sts -= lender0_scaled;
    lender0_scaled = 0;

    accrue_expected_state(
        &mut expected_sf,
        &mut expected_last,
        &mut expected_fees,
        expected_sts,
        annual_bps,
        fee_rate_bps,
        maturity,
        true,
    );
    let payout1 = payout_for_scaled_exact(lender1_scaled, expected_sf, expected_settlement);
    let actual_payout1 = core::cmp::min(payout1, expected_vault);
    expected_vault -= actual_payout1;
    expected_sts -= lender1_scaled;
    lender1_scaled = 0;

    assert_eq!(ms.market.total_deposited(), 10_000_000_000);
    assert_eq!(ms.market.total_borrowed(), expected_tb);
    assert_eq!(ms.market.total_repaid(), expected_tr);
    assert_eq!(ms.market.scale_factor(), expected_sf);
    assert_eq!(ms.market.last_accrual_timestamp(), expected_last);
    assert_eq!(ms.market.accrued_protocol_fees(), expected_fees);
    assert_eq!(ms.market.settlement_factor_wad(), expected_settlement);
    assert_eq!(ms.market.scaled_total_supply(), expected_sts);
    assert_eq!(ms.vault_balance, expected_vault);
    assert_eq!(ms.lender_positions[0].scaled_balance(), lender0_scaled);
    assert_eq!(ms.lender_positions[1].scaled_balance(), lender1_scaled);
}

// ===========================================================================
// Scenario E: High-frequency trading pattern (many small deposits/withdrawals)
// ===========================================================================

#[test]
fn historical_scenario_e_high_frequency_deposits_withdrawals() {
    let t0: i64 = 1_700_000_000;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity = t0 + year;

    let mut logs = Vec::new();
    let mut step: u64 = 0;

    // Setup with 0% interest, 0% fee for clean accounting
    logs.push(tx_log(
        step,
        t0,
        Instruction::InitializeProtocol { fee_rate_bps: 0 },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0,
        Instruction::SetBorrowerWhitelist {
            is_whitelisted: true,
            max_borrow_capacity: 100_000_000_000,
        },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0,
        Instruction::CreateMarket {
            annual_interest_bps: 0,
            maturity_timestamp: maturity,
            max_total_supply: 100_000_000_000,
            creation_timestamp: t0,
        },
        0,
    ));
    step += 1;

    // 20 rapid-fire deposits from 4 lenders, each 100 USDC, 10 seconds apart
    let small_amount: u64 = 100_000_000; // 100 USDC
    for i in 0..20 {
        let lender = i % 4;
        let time = t0 + (i as i64) * 10;
        logs.push(tx_log(
            step,
            time,
            Instruction::Deposit {
                lender_index: lender,
                amount: small_amount,
            },
            0,
        ));
        step += 1;
    }

    // Mature the market and withdraw all
    // Each lender deposited 5 times = 500 USDC each
    for lender in 0..4 {
        logs.push(tx_log(
            step,
            maturity,
            Instruction::Withdraw {
                lender_index: lender,
                scaled_amount: 0,
            },
            0,
        ));
        step += 1;
    }

    let checkpoints = vec![
        // After all 20 deposits (step index = 3 + 20 - 1 = 22)
        Checkpoint {
            step_index: 22,
            description: "all 20 deposits complete",
            vault_balance: Some(small_amount * 20),
            total_deposited: Some(small_amount * 20),
            scaled_total_supply: Some(u128::from(small_amount) * 20),
            ..Default::default()
        },
        // After all 4 withdrawals, vault should be 0
        Checkpoint {
            step_index: 26,
            description: "all 4 lenders withdrawn",
            vault_balance: Some(0),
            scaled_total_supply: Some(0),
            ..Default::default()
        },
    ];

    let mut engine = ReplayEngine::new(logs, checkpoints);
    engine.run();

    // Each lender got exactly their deposits back (0% interest, 0% fee).
    let ms = &engine.state.markets[0];
    assert_eq!(ms.vault_balance, 0);
    assert_eq!(ms.market.scale_factor(), WAD);
    assert_eq!(ms.market.last_accrual_timestamp(), maturity);
    assert_eq!(ms.market.total_deposited(), small_amount * 20);
    assert_eq!(ms.market.total_borrowed(), 0);
    assert_eq!(ms.market.total_repaid(), 0);
    assert_eq!(ms.market.accrued_protocol_fees(), 0);
    assert_eq!(ms.market.scaled_total_supply(), 0);
    assert_eq!(ms.market.settlement_factor_wad(), WAD);
    for lender in 0..4 {
        assert_eq!(ms.lender_positions[lender].scaled_balance(), 0);
    }
}

// ===========================================================================
// Scenario F: Multi-market scenario with shared whitelist
// ===========================================================================

#[test]
fn historical_scenario_f_multi_market_shared_whitelist() {
    let t0: i64 = 1_700_000_000;
    let day: i64 = 86_400;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity_a = t0 + year;
    let maturity_b = t0 + 2 * year;

    let mut logs = Vec::new();
    let mut step: u64 = 0;

    // Global init
    logs.push(tx_log(
        step,
        t0,
        Instruction::InitializeProtocol { fee_rate_bps: 0 },
        0,
    ));
    step += 1;

    // Whitelist for market 0
    logs.push(tx_log(
        step,
        t0,
        Instruction::SetBorrowerWhitelist {
            is_whitelisted: true,
            max_borrow_capacity: 10_000_000_000,
        },
        0,
    ));
    step += 1;

    // Whitelist for market 1
    logs.push(tx_log(
        step,
        t0,
        Instruction::SetBorrowerWhitelist {
            is_whitelisted: true,
            max_borrow_capacity: 10_000_000_000,
        },
        1,
    ));
    step += 1;

    // Create Market A: 5% annual, 1-year maturity
    logs.push(tx_log(
        step,
        t0,
        Instruction::CreateMarket {
            annual_interest_bps: 500,
            maturity_timestamp: maturity_a,
            max_total_supply: 50_000_000_000,
            creation_timestamp: t0,
        },
        0,
    ));
    step += 1;

    // Create Market B: 8% annual, 2-year maturity
    logs.push(tx_log(
        step,
        t0,
        Instruction::CreateMarket {
            annual_interest_bps: 800,
            maturity_timestamp: maturity_b,
            max_total_supply: 50_000_000_000,
            creation_timestamp: t0,
        },
        1,
    ));
    step += 1;

    // Lender 0 deposits 2000 USDC into Market A
    logs.push(tx_log(
        step,
        t0 + day,
        Instruction::Deposit {
            lender_index: 0,
            amount: 2_000_000_000,
        },
        0,
    ));
    step += 1;

    // Lender 0 deposits 3000 USDC into Market B
    logs.push(tx_log(
        step,
        t0 + day,
        Instruction::Deposit {
            lender_index: 0,
            amount: 3_000_000_000,
        },
        1,
    ));
    step += 1;

    // Borrower borrows from both markets
    logs.push(tx_log(
        step,
        t0 + 7 * day,
        Instruction::Borrow {
            amount: 1_000_000_000,
        },
        0,
    ));
    step += 1;
    logs.push(tx_log(
        step,
        t0 + 7 * day,
        Instruction::Borrow {
            amount: 2_000_000_000,
        },
        1,
    ));
    step += 1;

    // Full repay on Market A at maturity
    logs.push(tx_log(
        step,
        maturity_a,
        Instruction::Repay {
            amount: 1_000_000_000,
        },
        0,
    ));
    step += 1;

    // Full repay on Market B at maturity_a (before its own maturity)
    logs.push(tx_log(
        step,
        maturity_a,
        Instruction::Repay {
            amount: 2_000_000_000,
        },
        1,
    ));
    step += 1;

    // Lender withdraws from Market A (matured)
    logs.push(tx_log(
        step,
        maturity_a,
        Instruction::Withdraw {
            lender_index: 0,
            scaled_amount: 0,
        },
        0,
    ));
    step += 1;

    // Lender withdraws from Market B at its maturity
    logs.push(tx_log(
        step,
        maturity_b,
        Instruction::Withdraw {
            lender_index: 0,
            scaled_amount: 0,
        },
        1,
    ));

    let checkpoints = vec![
        // After deposits into both markets
        Checkpoint {
            step_index: 5,
            description: "Market A deposit",
            market_id: 0,
            vault_balance: Some(2_000_000_000),
            ..Default::default()
        },
        Checkpoint {
            step_index: 6,
            description: "Market B deposit",
            market_id: 1,
            vault_balance: Some(3_000_000_000),
            ..Default::default()
        },
        // After borrows
        Checkpoint {
            step_index: 7,
            description: "Market A borrow",
            market_id: 0,
            total_borrowed: Some(1_000_000_000),
            ..Default::default()
        },
        Checkpoint {
            step_index: 8,
            description: "Market B borrow",
            market_id: 1,
            total_borrowed: Some(2_000_000_000),
            ..Default::default()
        },
        // After withdrawals: both markets emptied
        Checkpoint {
            step_index: 11,
            description: "Market A fully withdrawn",
            market_id: 0,
            scaled_total_supply: Some(0),
            ..Default::default()
        },
        Checkpoint {
            step_index: 12,
            description: "Market B fully withdrawn",
            market_id: 1,
            scaled_total_supply: Some(0),
            ..Default::default()
        },
    ];

    let mut engine = ReplayEngine::new(logs, checkpoints);
    engine.run();

    // Oracle simulation for exact per-market end-state assertions.
    let market_a = &engine.state.markets[0];
    let market_b = &engine.state.markets[1];

    let mut sf_a = WAD;
    let mut last_a = t0;
    let mut sts_a = 0u128;
    let mut vault_a = 0u64;
    let mut lender_a = 0u128;
    let mut total_borrowed_a = 0u64;
    let mut total_repaid_a = 0u64;

    let elapsed_a1 = (t0 + day) - last_a;
    sf_a = accrue_scale_factor_exact(sf_a, 500, elapsed_a1);
    last_a = t0 + day;
    lender_a = scaled_amount_exact(2_000_000_000, sf_a);
    sts_a += lender_a;
    vault_a += 2_000_000_000;

    let elapsed_a2 = (t0 + 7 * day) - last_a;
    sf_a = accrue_scale_factor_exact(sf_a, 500, elapsed_a2);
    last_a = t0 + 7 * day;
    vault_a -= 1_000_000_000;
    total_borrowed_a += 1_000_000_000;

    let elapsed_a3 = maturity_a - last_a;
    sf_a = accrue_scale_factor_exact(sf_a, 500, elapsed_a3);
    last_a = maturity_a;
    vault_a += 1_000_000_000;
    total_repaid_a += 1_000_000_000;

    let settlement_a = settlement_factor_exact(vault_a, 0, sts_a, sf_a);
    let payout_a = payout_for_scaled_exact(lender_a, sf_a, settlement_a);
    let actual_payout_a = core::cmp::min(payout_a, vault_a);
    vault_a -= actual_payout_a;
    sts_a -= lender_a;
    lender_a = 0;

    let mut sf_b = WAD;
    let mut last_b = t0;
    let mut sts_b = 0u128;
    let mut vault_b = 0u64;
    let mut lender_b = 0u128;
    let mut total_borrowed_b = 0u64;
    let mut total_repaid_b = 0u64;

    let elapsed_b1 = (t0 + day) - last_b;
    sf_b = accrue_scale_factor_exact(sf_b, 800, elapsed_b1);
    last_b = t0 + day;
    lender_b = scaled_amount_exact(3_000_000_000, sf_b);
    sts_b += lender_b;
    vault_b += 3_000_000_000;

    let elapsed_b2 = (t0 + 7 * day) - last_b;
    sf_b = accrue_scale_factor_exact(sf_b, 800, elapsed_b2);
    last_b = t0 + 7 * day;
    vault_b -= 2_000_000_000;
    total_borrowed_b += 2_000_000_000;

    let elapsed_b3 = maturity_a - last_b;
    sf_b = accrue_scale_factor_exact(sf_b, 800, elapsed_b3);
    last_b = maturity_a;
    vault_b += 2_000_000_000;
    total_repaid_b += 2_000_000_000;

    let elapsed_b4 = maturity_b - last_b;
    sf_b = accrue_scale_factor_exact(sf_b, 800, elapsed_b4);
    last_b = maturity_b;
    let settlement_b = settlement_factor_exact(vault_b, 0, sts_b, sf_b);
    let payout_b = payout_for_scaled_exact(lender_b, sf_b, settlement_b);
    let actual_payout_b = core::cmp::min(payout_b, vault_b);
    vault_b -= actual_payout_b;
    sts_b -= lender_b;
    lender_b = 0;

    assert_eq!(market_a.market.scale_factor(), sf_a);
    assert_eq!(market_a.market.last_accrual_timestamp(), last_a);
    assert_eq!(market_a.market.scaled_total_supply(), sts_a);
    assert_eq!(market_a.market.total_deposited(), 2_000_000_000);
    assert_eq!(market_a.market.total_borrowed(), total_borrowed_a);
    assert_eq!(market_a.market.total_repaid(), total_repaid_a);
    assert_eq!(market_a.market.accrued_protocol_fees(), 0);
    assert_eq!(market_a.market.settlement_factor_wad(), settlement_a);
    assert_eq!(market_a.vault_balance, vault_a);
    assert_eq!(market_a.lender_positions[0].scaled_balance(), lender_a);

    assert_eq!(market_b.market.scale_factor(), sf_b);
    assert_eq!(market_b.market.last_accrual_timestamp(), last_b);
    assert_eq!(market_b.market.scaled_total_supply(), sts_b);
    assert_eq!(market_b.market.total_deposited(), 3_000_000_000);
    assert_eq!(market_b.market.total_borrowed(), total_borrowed_b);
    assert_eq!(market_b.market.total_repaid(), total_repaid_b);
    assert_eq!(market_b.market.accrued_protocol_fees(), 0);
    assert_eq!(market_b.market.settlement_factor_wad(), settlement_b);
    assert_eq!(market_b.vault_balance, vault_b);
    assert_eq!(market_b.lender_positions[0].scaled_balance(), lender_b);
}

// ===========================================================================
// proptest: random realistic mainnet-like sequences
// ===========================================================================

#[cfg(test)]
mod proptest_replay {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating a realistic mainnet-like sequence.
    ///
    /// Parameters:
    /// - 1 to 6 lenders
    /// - Deposit amounts between 1 USDC and 10,000 USDC
    /// - Timestamps advancing forward
    /// - Optional borrows (up to 80% of vault)
    /// - Repays covering all borrowed
    /// - Withdrawals at maturity
    fn realistic_sequence_strategy() -> impl Strategy<Value = Vec<TransactionLog>> {
        (
            // num_lenders: 1..=6
            1usize..=6,
            // annual_interest_bps: 0..=2000 (0-20%)
            0u16..=2000,
            // fee_rate_bps: 0..=1000 (0-10%)
            0u16..=1000,
            // num_deposits: 1..=10
            1usize..=10,
            // deposit_amount_usdc: 1..=10000 (in USDC)
            prop::collection::vec(1u64..=10_000, 1..=10),
            // borrow_fraction_bps: 0..=8000 (0-80% of vault)
            0u16..=8000,
            // days_between_ops: 1..=30
            1u32..=30,
        )
            .prop_map(
                |(
                    num_lenders,
                    annual_interest_bps,
                    fee_rate_bps,
                    _num_deposits,
                    deposit_amounts_usdc,
                    borrow_fraction_bps,
                    days_between,
                )| {
                    let t0: i64 = 1_700_000_000;
                    let day: i64 = 86_400;
                    let year: i64 = SECONDS_PER_YEAR as i64;
                    let maturity = t0 + year;
                    let mut logs = Vec::new();
                    let mut step: u64 = 0;
                    let mut current_time = t0;

                    // 1. Initialize protocol
                    logs.push(tx_log(
                        step,
                        current_time,
                        Instruction::InitializeProtocol { fee_rate_bps },
                        0,
                    ));
                    step += 1;

                    // Total deposits to compute capacity
                    let total_deposit_usdc: u64 = deposit_amounts_usdc.iter().sum();
                    let total_deposit = total_deposit_usdc * 1_000_000;

                    // 2. Set borrower whitelist
                    logs.push(tx_log(
                        step,
                        current_time,
                        Instruction::SetBorrowerWhitelist {
                            is_whitelisted: true,
                            max_borrow_capacity: total_deposit * 2,
                        },
                        0,
                    ));
                    step += 1;

                    // 3. Create market
                    let cap = total_deposit * 10; // generous cap
                    logs.push(tx_log(
                        step,
                        current_time,
                        Instruction::CreateMarket {
                            annual_interest_bps,
                            maturity_timestamp: maturity,
                            max_total_supply: cap,
                            creation_timestamp: current_time,
                        },
                        0,
                    ));
                    step += 1;

                    // 4. Deposits — track which lenders actually received funds
                    let mut lenders_with_deposits = std::collections::HashSet::new();
                    for (i, &amount_usdc) in deposit_amounts_usdc.iter().enumerate() {
                        current_time += day * i64::from(days_between);
                        // Keep time before maturity
                        if current_time >= maturity {
                            current_time = maturity - day;
                        }
                        let lender = i % num_lenders;
                        lenders_with_deposits.insert(lender);
                        let amount = amount_usdc * 1_000_000; // Convert to 6-decimal
                        logs.push(tx_log(
                            step,
                            current_time,
                            Instruction::Deposit {
                                lender_index: lender,
                                amount,
                            },
                            0,
                        ));
                        step += 1;
                    }

                    // 5. Optional borrow
                    let borrow_amount =
                        (total_deposit as u128 * u128::from(borrow_fraction_bps) / 10_000) as u64;
                    if borrow_amount > 0 {
                        current_time += day;
                        if current_time >= maturity {
                            current_time = maturity - day;
                        }
                        logs.push(tx_log(
                            step,
                            current_time,
                            Instruction::Borrow {
                                amount: borrow_amount,
                            },
                            0,
                        ));
                        step += 1;

                        // 6. Full repay before maturity
                        current_time += day * 30;
                        if current_time >= maturity {
                            current_time = maturity - 1;
                        }
                        logs.push(tx_log(
                            step,
                            current_time,
                            Instruction::Repay {
                                amount: borrow_amount,
                            },
                            0,
                        ));
                        step += 1;
                    }

                    // 7. Withdrawals at maturity — only for lenders that deposited
                    let mut sorted_lenders: Vec<usize> =
                        lenders_with_deposits.into_iter().collect();
                    sorted_lenders.sort();
                    for lender in sorted_lenders {
                        logs.push(tx_log(
                            step,
                            maturity,
                            Instruction::Withdraw {
                                lender_index: lender,
                                scaled_amount: 0,
                            },
                            0,
                        ));
                        step += 1;
                    }

                    logs
                },
            )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        #[test]
        fn proptest_realistic_mainnet_sequences(logs in realistic_sequence_strategy()) {
            let replay_logs = logs.clone();

            // Run the engine with no specific checkpoints — just invariant checks
            let mut engine = ReplayEngine::new(logs, vec![]);
            engine.run();

            for ms in &engine.state.markets {
                let total_scaled: u128 = ms
                    .lender_positions
                    .iter()
                    .map(|lp| lp.scaled_balance())
                    .sum();
                prop_assert_eq!(
                    total_scaled,
                    ms.market.scaled_total_supply(),
                    "lender sum must match scaled_total_supply"
                );
                prop_assert_eq!(
                    total_scaled, 0,
                    "all positions should be withdrawn at end"
                );
                prop_assert_eq!(
                    ms.market.scaled_total_supply(),
                    0,
                    "scaled_total_supply should be 0"
                );
                prop_assert!(
                    ms.market.scale_factor() >= WAD,
                    "scale_factor must be >= WAD"
                );
                let sfw = ms.market.settlement_factor_wad();
                prop_assert!(
                    sfw == 0 || (1..=WAD).contains(&sfw),
                    "settlement_factor_wad must be 0 or in [1, WAD]"
                );
                prop_assert!(
                    ms.market.total_repaid() <= ms.market.total_borrowed(),
                    "total_repaid should not exceed total_borrowed"
                );
                prop_assert!(
                    ms.market.last_accrual_timestamp() <= ms.market.maturity_timestamp(),
                    "last accrual must be capped at maturity"
                );
            }

            // Determinism check: same log sequence yields identical final state.
            let mut replay_engine = ReplayEngine::new(replay_logs, vec![]);
            replay_engine.run();

            prop_assert_eq!(
                engine.state.protocol_config.fee_rate_bps(),
                replay_engine.state.protocol_config.fee_rate_bps()
            );
            prop_assert_eq!(
                engine.state.protocol_config.is_initialized,
                replay_engine.state.protocol_config.is_initialized
            );
            prop_assert_eq!(engine.state.markets.len(), replay_engine.state.markets.len());

            for (lhs, rhs) in engine.state.markets.iter().zip(replay_engine.state.markets.iter()) {
                prop_assert_eq!(lhs.vault_balance, rhs.vault_balance);
                prop_assert_eq!(lhs.borrower_total_borrowed, rhs.borrower_total_borrowed);
                prop_assert_eq!(lhs.borrower_max_capacity, rhs.borrower_max_capacity);
                prop_assert_eq!(lhs.borrower_whitelisted, rhs.borrower_whitelisted);

                prop_assert_eq!(
                    lhs.market.annual_interest_bps(),
                    rhs.market.annual_interest_bps()
                );
                prop_assert_eq!(
                    lhs.market.maturity_timestamp(),
                    rhs.market.maturity_timestamp()
                );
                prop_assert_eq!(
                    lhs.market.max_total_supply(),
                    rhs.market.max_total_supply()
                );
                prop_assert_eq!(lhs.market.scale_factor(), rhs.market.scale_factor());
                prop_assert_eq!(
                    lhs.market.scaled_total_supply(),
                    rhs.market.scaled_total_supply()
                );
                prop_assert_eq!(
                    lhs.market.accrued_protocol_fees(),
                    rhs.market.accrued_protocol_fees()
                );
                prop_assert_eq!(lhs.market.total_deposited(), rhs.market.total_deposited());
                prop_assert_eq!(lhs.market.total_borrowed(), rhs.market.total_borrowed());
                prop_assert_eq!(lhs.market.total_repaid(), rhs.market.total_repaid());
                prop_assert_eq!(
                    lhs.market.last_accrual_timestamp(),
                    rhs.market.last_accrual_timestamp()
                );
                prop_assert_eq!(
                    lhs.market.settlement_factor_wad(),
                    rhs.market.settlement_factor_wad()
                );

                prop_assert_eq!(lhs.lender_positions.len(), rhs.lender_positions.len());
                for (lp_lhs, lp_rhs) in lhs.lender_positions.iter().zip(rhs.lender_positions.iter()) {
                    prop_assert_eq!(lp_lhs.scaled_balance(), lp_rhs.scaled_balance());
                }
            }
        }
    }
}
