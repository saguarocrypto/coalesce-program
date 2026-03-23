//! Regression test infrastructure for the CoalesceFi Pinocchio lending protocol.
//!
//! This module provides a framework for replaying captured transaction sequences
//! against pure protocol logic (state structs + interest accrual), without
//! requiring the Solana runtime. Each regression scenario defines an initial
//! state, a sequence of transactions, and expected outcomes that are asserted
//! at each step.
//!
//! To add a new regression test:
//! 1. Define a `RegressionScenario` with a descriptive name.
//! 2. Populate its `initial_state` and `transactions` vector.
//! 3. Call `replay_scenario(&scenario)` in a `#[test]` function.

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

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{LenderPosition, Market, ProtocolConfig};

#[path = "common/math_oracle.rs"]
mod math_oracle;

fn expected_scale_factor(
    initial_scale_factor: u128,
    annual_interest_bps: u16,
    elapsed_seconds: i64,
) -> u128 {
    math_oracle::mul_wad(
        initial_scale_factor,
        math_oracle::growth_factor_wad(annual_interest_bps, elapsed_seconds),
    )
}

fn expected_fee_delta(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_interest_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> u64 {
    if scaled_total_supply == 0 || fee_rate_bps == 0 || elapsed_seconds <= 0 {
        return 0;
    }

    let interest_delta_wad = math_oracle::growth_factor_wad(annual_interest_bps, elapsed_seconds)
        .checked_sub(WAD)
        .unwrap();
    let fee_delta_wad = interest_delta_wad * u128::from(fee_rate_bps) / BPS;
    // Use pre-accrual scale_factor_before (matches on-chain Finding 10 fix)
    let fee_normalized = scaled_total_supply * scale_factor_before / WAD * fee_delta_wad / WAD;
    u64::try_from(fee_normalized).unwrap()
}

// ---------------------------------------------------------------------------
// Transaction enum — mirrors all protocol instructions
// ---------------------------------------------------------------------------

/// Represents a single protocol instruction with its parameters.
/// Fields that are only relevant to on-chain validation (PDAs, signers,
/// blacklist checks) are omitted; this enum captures the *logical* effect.
#[derive(Debug, Clone)]
enum Transaction {
    /// Disc 0 — set up the singleton protocol config.
    InitializeProtocol { fee_rate_bps: u16 },

    /// Disc 2 — create a new market with fixed terms.
    CreateMarket {
        annual_interest_bps: u16,
        maturity_timestamp: i64,
        max_total_supply: u64,
        creation_timestamp: i64,
    },

    /// Disc 5 — lender deposits USDC into the vault.
    Deposit {
        lender_index: usize,
        amount: u64,
        current_timestamp: i64,
    },

    /// Disc 6 — borrower withdraws USDC from the vault.
    Borrow { amount: u64, current_timestamp: i64 },

    /// Disc 7 — anyone repays USDC to the vault.
    Repay { amount: u64, current_timestamp: i64 },

    /// Disc 8 — lender withdraws proportional share after maturity.
    Withdraw {
        lender_index: usize,
        /// 0 means full withdrawal (all scaled balance).
        scaled_amount: u128,
        current_timestamp: i64,
    },

    /// Disc 9 — fee authority collects accrued protocol fees.
    CollectFees { current_timestamp: i64 },

    /// Disc 11 — recompute settlement factor upward.
    ReSettle { current_timestamp: i64 },

    /// Disc 10 — close an empty lender position (logic: assert zero balance).
    CloseLenderPosition { lender_index: usize },

    /// Disc 1 — update fee config (fee_rate_bps on the protocol config).
    SetFeeConfig { new_fee_rate_bps: u16 },

    /// Disc 12 — add/update borrower whitelist entry.
    SetBorrowerWhitelist {
        is_whitelisted: bool,
        max_borrow_capacity: u64,
    },

    /// Pseudo-instruction: advance time and accrue interest explicitly.
    AccrueInterest { current_timestamp: i64 },
}

// ---------------------------------------------------------------------------
// Expected outcome after each transaction step
// ---------------------------------------------------------------------------

/// Snapshot of expected state values after a transaction executes.
/// Fields set to `None` are not checked.
#[derive(Debug, Clone, Default)]
struct ExpectedOutcome {
    vault_balance: Option<u64>,
    scale_factor: Option<u128>,
    scaled_total_supply: Option<u128>,
    total_deposited: Option<u64>,
    total_borrowed: Option<u64>,
    total_repaid: Option<u64>,
    accrued_protocol_fees: Option<u64>,
    settlement_factor_wad: Option<u128>,
    /// Expected scaled_balance for a specific lender (index, expected).
    lender_scaled_balance: Option<(usize, u128)>,
    /// Optional human-readable description for assertion messages.
    description: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Protocol simulation state
// ---------------------------------------------------------------------------

/// In-memory simulation state for replaying transactions.
struct SimulationState {
    protocol_config: ProtocolConfig,
    market: Market,
    lender_positions: Vec<LenderPosition>,
    vault_balance: u64,
    /// Track borrower whitelist capacity consumed.
    borrower_total_borrowed: u64,
    borrower_max_capacity: u64,
    borrower_whitelisted: bool,
}

impl SimulationState {
    fn new() -> Self {
        Self {
            protocol_config: ProtocolConfig::zeroed(),
            market: Market::zeroed(),
            lender_positions: Vec::new(),
            vault_balance: 0,
            borrower_total_borrowed: 0,
            borrower_max_capacity: 0,
            borrower_whitelisted: false,
        }
    }

    /// Ensure the lender_positions vec has at least `n` entries.
    fn ensure_lender(&mut self, index: usize) {
        while self.lender_positions.len() <= index {
            self.lender_positions.push(LenderPosition::zeroed());
        }
    }
}

// ---------------------------------------------------------------------------
// Regression scenario definition
// ---------------------------------------------------------------------------

struct RegressionScenario {
    name: &'static str,
    initial_state: InitialState,
    transactions: Vec<(Transaction, ExpectedOutcome)>,
}

/// Initial state applied before replaying any transactions.
#[derive(Debug, Clone)]
struct InitialState {
    fee_rate_bps: u16,
    annual_interest_bps: u16,
    maturity_timestamp: i64,
    max_total_supply: u64,
    creation_timestamp: i64,
    num_lenders: usize,
    /// Optionally pre-set the borrower whitelist.
    borrower_whitelisted: bool,
    borrower_max_capacity: u64,
}

// ---------------------------------------------------------------------------
// Replay engine
// ---------------------------------------------------------------------------

/// Build a simulation state from an `InitialState` fixture.
fn initialize_simulation_state(initial: &InitialState) -> SimulationState {
    let mut state = SimulationState::new();

    // Apply initial state: protocol config
    state.protocol_config.set_fee_rate_bps(initial.fee_rate_bps);
    state.protocol_config.is_initialized = 1;

    // Apply initial state: market
    state
        .market
        .set_annual_interest_bps(initial.annual_interest_bps);
    state
        .market
        .set_maturity_timestamp(initial.maturity_timestamp);
    state.market.set_max_total_supply(initial.max_total_supply);
    state.market.set_scale_factor(WAD);
    state
        .market
        .set_last_accrual_timestamp(initial.creation_timestamp);
    state.market.set_settlement_factor_wad(0);

    // Pre-allocate lender positions
    for _ in 0..initial.num_lenders {
        state.lender_positions.push(LenderPosition::zeroed());
    }

    // Borrower whitelist
    state.borrower_whitelisted = initial.borrower_whitelisted;
    state.borrower_max_capacity = initial.borrower_max_capacity;

    state
}

/// Apply initial state and replay all transactions, asserting expected outcomes.
fn replay_scenario(scenario: &RegressionScenario) {
    let mut state = initialize_simulation_state(&scenario.initial_state);

    // Replay each transaction
    for (step_idx, (tx, expected)) in scenario.transactions.iter().enumerate() {
        let step_desc = expected.description.unwrap_or("(no description)");
        let ctx = format!(
            "Scenario '{}', step {} — {:?} — {}",
            scenario.name, step_idx, tx, step_desc
        );

        execute_transaction(&mut state, tx, &ctx);
        assert_outcome(&state, expected, &ctx);
    }
}

/// Execute a single transaction against the simulation state.
fn execute_transaction(state: &mut SimulationState, tx: &Transaction, ctx: &str) {
    match tx {
        Transaction::InitializeProtocol { fee_rate_bps } => {
            state.protocol_config.set_fee_rate_bps(*fee_rate_bps);
            state.protocol_config.is_initialized = 1;
        },

        Transaction::CreateMarket {
            annual_interest_bps,
            maturity_timestamp,
            max_total_supply,
            creation_timestamp,
        } => {
            state.market = Market::zeroed();
            state.market.set_annual_interest_bps(*annual_interest_bps);
            state.market.set_maturity_timestamp(*maturity_timestamp);
            state.market.set_max_total_supply(*max_total_supply);
            state.market.set_scale_factor(WAD);
            state.market.set_last_accrual_timestamp(*creation_timestamp);
            state.vault_balance = 0;
        },

        Transaction::Deposit {
            lender_index,
            amount,
            current_timestamp,
        } => {
            // Step 1: Accrue interest
            accrue_interest(
                &mut state.market,
                &state.protocol_config,
                *current_timestamp,
            )
            .unwrap_or_else(|e| panic!("{ctx}: accrue_interest failed: {e:?}"));

            // Step 2: Compute scaled amount = amount * WAD / scale_factor
            let amount_u128 = u128::from(*amount);
            let scale_factor = state.market.scale_factor();
            let scaled_amount = amount_u128
                .checked_mul(WAD)
                .expect("overflow in deposit scaling (mul)")
                .checked_div(scale_factor)
                .expect("overflow in deposit scaling (div)");
            assert!(scaled_amount > 0, "{ctx}: deposit scaled to zero");

            // Step 3: Validate cap
            let new_scaled_total = state
                .market
                .scaled_total_supply()
                .checked_add(scaled_amount)
                .expect("overflow in scaled_total_supply");
            let new_normalized = new_scaled_total
                .checked_mul(scale_factor)
                .expect("overflow in cap check (mul)")
                .checked_div(WAD)
                .expect("overflow in cap check (div)");
            let max_supply_u128 = u128::from(state.market.max_total_supply());
            assert!(
                new_normalized <= max_supply_u128,
                "{ctx}: cap exceeded ({new_normalized} > {max_supply_u128})"
            );

            // Step 4: Update vault balance
            state.vault_balance = state
                .vault_balance
                .checked_add(*amount)
                .expect("vault overflow");

            // Step 5: Update lender position
            state.ensure_lender(*lender_index);
            let pos = &mut state.lender_positions[*lender_index];
            let new_balance = pos
                .scaled_balance()
                .checked_add(scaled_amount)
                .expect("lender balance overflow");
            pos.set_scaled_balance(new_balance);

            // Step 6: Update market
            state.market.set_scaled_total_supply(new_scaled_total);
            let new_total_deposited = state
                .market
                .total_deposited()
                .checked_add(*amount)
                .expect("total_deposited overflow");
            state.market.set_total_deposited(new_total_deposited);
        },

        Transaction::Borrow {
            amount,
            current_timestamp,
        } => {
            // Step 1: Accrue interest
            accrue_interest(
                &mut state.market,
                &state.protocol_config,
                *current_timestamp,
            )
            .unwrap_or_else(|e| panic!("{ctx}: accrue_interest failed: {e:?}"));

            // Step 2: Borrowable check (COAL-L02: no fee reservation, full vault is borrowable)
            let borrowable = state.vault_balance;
            assert!(
                *amount <= borrowable,
                "{ctx}: borrow amount {amount} > borrowable {borrowable}"
            );

            // Step 3: Global capacity check
            let new_wl_total = state
                .borrower_total_borrowed
                .checked_add(*amount)
                .expect("wl total overflow");
            assert!(
                new_wl_total <= state.borrower_max_capacity,
                "{ctx}: global capacity exceeded"
            );

            // Step 4: Transfer vault -> borrower
            state.vault_balance = state
                .vault_balance
                .checked_sub(*amount)
                .expect("vault underflow on borrow");

            // Step 5: Update market
            let new_total_borrowed = state
                .market
                .total_borrowed()
                .checked_add(*amount)
                .expect("total_borrowed overflow");
            state.market.set_total_borrowed(new_total_borrowed);

            // Step 6: Update whitelist tracker
            state.borrower_total_borrowed = new_wl_total;
        },

        Transaction::Repay {
            amount,
            current_timestamp,
        } => {
            // Repay accrues with zero fee config (matches on-chain behavior)
            let zero_config: ProtocolConfig = ProtocolConfig::zeroed();
            accrue_interest(&mut state.market, &zero_config, *current_timestamp)
                .unwrap_or_else(|e| panic!("{ctx}: accrue_interest failed: {e:?}"));

            // Transfer payer -> vault
            state.vault_balance = state
                .vault_balance
                .checked_add(*amount)
                .expect("vault overflow on repay");

            // Update market
            let new_total_repaid = state
                .market
                .total_repaid()
                .checked_add(*amount)
                .expect("total_repaid overflow");
            state.market.set_total_repaid(new_total_repaid);
        },

        Transaction::Withdraw {
            lender_index,
            scaled_amount,
            current_timestamp,
        } => {
            // Step 1: Accrue interest (capped at maturity)
            accrue_interest(
                &mut state.market,
                &state.protocol_config,
                *current_timestamp,
            )
            .unwrap_or_else(|e| panic!("{ctx}: accrue_interest failed: {e:?}"));

            // Step 2: Compute settlement factor if not yet settled
            if state.market.settlement_factor_wad() == 0 {
                // COAL-C01: no fee reservation, full vault is available for lenders
                let available_for_lenders = u128::from(state.vault_balance);

                let total_normalized = state
                    .market
                    .scaled_total_supply()
                    .checked_mul(state.market.scale_factor())
                    .expect("overflow in total_normalized (mul)")
                    .checked_div(WAD)
                    .expect("overflow in total_normalized (div)");

                let settlement_factor = if total_normalized == 0 {
                    WAD
                } else {
                    let raw = available_for_lenders
                        .checked_mul(WAD)
                        .expect("overflow in settlement (mul)")
                        .checked_div(total_normalized)
                        .expect("overflow in settlement (div)");
                    let capped = if raw > WAD { WAD } else { raw };
                    if capped < 1 {
                        1
                    } else {
                        capped
                    }
                };

                state.market.set_settlement_factor_wad(settlement_factor);
            }

            // Step 3: Resolve scaled amount
            state.ensure_lender(*lender_index);
            let pos_balance = state.lender_positions[*lender_index].scaled_balance();
            let effective_scaled = if *scaled_amount == 0 {
                pos_balance
            } else {
                *scaled_amount
            };

            assert!(
                effective_scaled <= pos_balance,
                "{ctx}: insufficient scaled balance ({effective_scaled} > {pos_balance})"
            );

            // Step 4: Compute payout
            let scale_factor = state.market.scale_factor();
            let settlement_factor = state.market.settlement_factor_wad();

            let normalized_amount = effective_scaled
                .checked_mul(scale_factor)
                .expect("overflow in normalized (mul)")
                .checked_div(WAD)
                .expect("overflow in normalized (div)");

            let payout_u128 = normalized_amount
                .checked_mul(settlement_factor)
                .expect("overflow in payout (mul)")
                .checked_div(WAD)
                .expect("overflow in payout (div)");

            let payout = u64::try_from(payout_u128).expect("payout overflow to u64");
            assert!(payout > 0, "{ctx}: payout is zero");

            // Clamp payout to actual vault balance (safety)
            let actual_payout = core::cmp::min(payout, state.vault_balance);

            // Step 5: Transfer vault -> lender
            state.vault_balance = state
                .vault_balance
                .checked_sub(actual_payout)
                .expect("vault underflow on withdraw");

            // Step 6: Update lender position
            let new_balance = pos_balance
                .checked_sub(effective_scaled)
                .expect("lender balance underflow");
            state.lender_positions[*lender_index].set_scaled_balance(new_balance);

            // Step 7: Update market
            let new_scaled_total = state
                .market
                .scaled_total_supply()
                .checked_sub(effective_scaled)
                .expect("scaled_total_supply underflow");
            state.market.set_scaled_total_supply(new_scaled_total);
        },

        Transaction::CollectFees { current_timestamp } => {
            // Step 1: Accrue interest
            accrue_interest(
                &mut state.market,
                &state.protocol_config,
                *current_timestamp,
            )
            .unwrap_or_else(|e| panic!("{ctx}: accrue_interest failed: {e:?}"));

            // Step 2: Compute withdrawable fees
            let accrued_fees = state.market.accrued_protocol_fees();
            assert!(accrued_fees > 0, "{ctx}: no fees to collect");

            let withdrawable = core::cmp::min(accrued_fees, state.vault_balance);
            assert!(withdrawable > 0, "{ctx}: zero withdrawable fees");

            // Step 3: Transfer vault -> fee destination
            state.vault_balance = state
                .vault_balance
                .checked_sub(withdrawable)
                .expect("vault underflow on fee collect");

            // Step 4: Reduce accrued fees
            let remaining = accrued_fees
                .checked_sub(withdrawable)
                .expect("fee underflow");
            state.market.set_accrued_protocol_fees(remaining);
        },

        Transaction::ReSettle { current_timestamp } => {
            let old_factor = state.market.settlement_factor_wad();
            assert!(old_factor > 0, "{ctx}: market not yet settled");

            // Accrue with zero fee (matches on-chain)
            let zero_config: ProtocolConfig = ProtocolConfig::zeroed();
            accrue_interest(&mut state.market, &zero_config, *current_timestamp)
                .unwrap_or_else(|e| panic!("{ctx}: accrue_interest failed: {e:?}"));

            // Recompute settlement factor (COAL-C01: no fee reservation, full vault available)
            let available = u128::from(state.vault_balance);

            let total_normalized = state
                .market
                .scaled_total_supply()
                .checked_mul(state.market.scale_factor())
                .expect("overflow")
                .checked_div(WAD)
                .expect("overflow");

            let new_factor = if total_normalized == 0 {
                WAD
            } else {
                let raw = available
                    .checked_mul(WAD)
                    .expect("overflow")
                    .checked_div(total_normalized)
                    .expect("overflow");
                let capped = if raw > WAD { WAD } else { raw };
                if capped < 1 {
                    1
                } else {
                    capped
                }
            };

            assert!(
                new_factor > old_factor,
                "{ctx}: settlement not improved ({new_factor} <= {old_factor})"
            );

            state.market.set_settlement_factor_wad(new_factor);
        },

        Transaction::CloseLenderPosition { lender_index } => {
            state.ensure_lender(*lender_index);
            let balance = state.lender_positions[*lender_index].scaled_balance();
            assert!(
                balance == 0,
                "{ctx}: position not empty (balance = {balance})"
            );
            // In simulation, we just verify the invariant. On-chain the account
            // would be zeroed and lamports returned.
        },

        Transaction::SetFeeConfig { new_fee_rate_bps } => {
            assert!(*new_fee_rate_bps <= 10_000, "{ctx}: fee rate exceeds max");
            state.protocol_config.set_fee_rate_bps(*new_fee_rate_bps);
        },

        Transaction::SetBorrowerWhitelist {
            is_whitelisted,
            max_borrow_capacity,
        } => {
            state.borrower_whitelisted = *is_whitelisted;
            state.borrower_max_capacity = *max_borrow_capacity;
        },

        Transaction::AccrueInterest { current_timestamp } => {
            accrue_interest(
                &mut state.market,
                &state.protocol_config,
                *current_timestamp,
            )
            .unwrap_or_else(|e| panic!("{ctx}: accrue_interest failed: {e:?}"));
        },
    }
}

/// Assert that simulation state matches expected outcome.
fn assert_outcome(state: &SimulationState, expected: &ExpectedOutcome, ctx: &str) {
    if let Some(vb) = expected.vault_balance {
        assert_eq!(
            state.vault_balance, vb,
            "{ctx}: vault_balance mismatch (got {}, expected {})",
            state.vault_balance, vb
        );
    }
    if let Some(sf) = expected.scale_factor {
        assert_eq!(
            state.market.scale_factor(),
            sf,
            "{ctx}: scale_factor mismatch (got {}, expected {})",
            state.market.scale_factor(),
            sf
        );
    }
    if let Some(sts) = expected.scaled_total_supply {
        assert_eq!(
            state.market.scaled_total_supply(),
            sts,
            "{ctx}: scaled_total_supply mismatch (got {}, expected {})",
            state.market.scaled_total_supply(),
            sts
        );
    }
    if let Some(td) = expected.total_deposited {
        assert_eq!(
            state.market.total_deposited(),
            td,
            "{ctx}: total_deposited mismatch (got {}, expected {})",
            state.market.total_deposited(),
            td
        );
    }
    if let Some(tb) = expected.total_borrowed {
        assert_eq!(
            state.market.total_borrowed(),
            tb,
            "{ctx}: total_borrowed mismatch (got {}, expected {})",
            state.market.total_borrowed(),
            tb
        );
    }
    if let Some(tr) = expected.total_repaid {
        assert_eq!(
            state.market.total_repaid(),
            tr,
            "{ctx}: total_repaid mismatch (got {}, expected {})",
            state.market.total_repaid(),
            tr
        );
    }
    if let Some(apf) = expected.accrued_protocol_fees {
        assert_eq!(
            state.market.accrued_protocol_fees(),
            apf,
            "{ctx}: accrued_protocol_fees mismatch (got {}, expected {})",
            state.market.accrued_protocol_fees(),
            apf
        );
    }
    if let Some(sfw) = expected.settlement_factor_wad {
        assert_eq!(
            state.market.settlement_factor_wad(),
            sfw,
            "{ctx}: settlement_factor_wad mismatch (got {}, expected {})",
            state.market.settlement_factor_wad(),
            sfw
        );
    }
    if let Some((idx, expected_balance)) = expected.lender_scaled_balance {
        assert!(
            idx < state.lender_positions.len(),
            "{ctx}: lender index {idx} out of range"
        );
        let actual = state.lender_positions[idx].scaled_balance();
        assert_eq!(
            actual, expected_balance,
            "{ctx}: lender[{idx}].scaled_balance mismatch (got {actual}, expected {expected_balance})"
        );
    }
}

// ===========================================================================
// Regression Scenario 1: Basic Lending Cycle (Happy Path)
// ===========================================================================
//
// Timeline:
//   T=1000     — market created, 10% annual, maturity at T=1000+YEAR
//   T=1000     — lender deposits 1,000,000 USDC (6 decimals = 1_000_000_000_000 lamports... no, 1M USDC = 1_000_000 * 10^6 = 1_000_000_000_000)
//                 Wait, USDC has 6 decimals, so 1 USDC = 1_000_000. 1000 USDC = 1_000_000_000.
//   T=1000     — borrower borrows 500 USDC
//   T=1000+YEAR — borrower repays 500 USDC (no interest accrued in repay because zero fee config in repay)
//   T=1000+YEAR — lender withdraws all (full settlement)

#[test]
fn regression_basic_lending_cycle() {
    let creation_ts: i64 = 1_000;
    let maturity_ts: i64 = creation_ts + SECONDS_PER_YEAR as i64;
    let deposit_amount: u64 = 1_000_000_000; // 1000 USDC
    let borrow_amount: u64 = 500_000_000; // 500 USDC

    // At WAD scale factor and deposit at creation time, scaled_amount = amount * WAD / WAD = amount.
    let expected_scaled_deposit = u128::from(deposit_amount);

    let scenario = RegressionScenario {
        name: "basic_lending_cycle",
        initial_state: InitialState {
            fee_rate_bps: 0,           // No protocol fees for simplicity
            annual_interest_bps: 1000, // 10%
            maturity_timestamp: maturity_ts,
            max_total_supply: 10_000_000_000, // 10,000 USDC cap
            creation_timestamp: creation_ts,
            num_lenders: 1,
            borrower_whitelisted: true,
            borrower_max_capacity: 5_000_000_000, // 5000 USDC
        },
        transactions: vec![
            // Step 0: Deposit 1000 USDC at creation time
            (
                Transaction::Deposit {
                    lender_index: 0,
                    amount: deposit_amount,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_amount),
                    scaled_total_supply: Some(expected_scaled_deposit),
                    total_deposited: Some(deposit_amount),
                    scale_factor: Some(WAD), // No time elapsed, no interest
                    lender_scaled_balance: Some((0, expected_scaled_deposit)),
                    description: Some("deposit 1000 USDC at creation"),
                    ..Default::default()
                },
            ),
            // Step 1: Borrow 500 USDC at creation time
            (
                Transaction::Borrow {
                    amount: borrow_amount,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_amount - borrow_amount),
                    total_borrowed: Some(borrow_amount),
                    description: Some("borrow 500 USDC"),
                    ..Default::default()
                },
            ),
            // Step 2: Repay 500 USDC at maturity
            // Repay uses zero fee config for accrual. The scale factor still
            // grows by 10% (full year at 10% annual).
            (
                Transaction::Repay {
                    amount: borrow_amount,
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    // vault = (1000M - 500M) + 500M = 1000M
                    vault_balance: Some(deposit_amount),
                    total_repaid: Some(borrow_amount),
                    description: Some("repay 500 USDC at maturity"),
                    ..Default::default()
                },
            ),
            // Step 3: Withdraw all at maturity
            // scale_factor after repay accrual (zero fee):
            //   interest_delta_wad = 1000 * YEAR * WAD / (YEAR * BPS) = WAD / 10
            //   new_sf = WAD + WAD * (WAD/10) / WAD = WAD + WAD/10 = 1.1 * WAD
            // Settlement factor: vault=1000M, fees=0, available=1000M
            //   total_normalized = deposit * 1.1*WAD / WAD = deposit * 1.1 = 1_100_000_000
            //   raw = 1_000_000_000 * WAD / 1_100_000_000
            //   Since available < total_normalized, settlement < WAD.
            // Payout = scaled_balance * scale_factor / WAD * settlement / WAD
            (
                Transaction::Withdraw {
                    lender_index: 0,
                    scaled_amount: 0, // full withdrawal
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    // After withdraw, lender balance = 0
                    lender_scaled_balance: Some((0, 0)),
                    scaled_total_supply: Some(0),
                    description: Some("withdraw all at maturity"),
                    ..Default::default()
                },
            ),
            // Step 4: Close the empty position
            (
                Transaction::CloseLenderPosition { lender_index: 0 },
                ExpectedOutcome {
                    lender_scaled_balance: Some((0, 0)),
                    description: Some("close empty lender position"),
                    ..Default::default()
                },
            ),
        ],
    };

    replay_scenario(&scenario);

    // Strengthened postconditions: exact maturity accrual and settlement math.
    let mut state = initialize_simulation_state(&scenario.initial_state);
    for (tx, _) in &scenario.transactions {
        execute_transaction(&mut state, tx, "basic_lending_cycle strengthened replay");
    }

    let expected_scale_factor = expected_scale_factor(WAD, 1000, SECONDS_PER_YEAR as i64);
    assert_eq!(state.market.scale_factor(), expected_scale_factor);

    let total_normalized = u128::from(deposit_amount) * expected_scale_factor / WAD;
    let expected_settlement = (u128::from(deposit_amount) * WAD) / total_normalized;
    assert_eq!(state.market.settlement_factor_wad(), expected_settlement);
    assert!(state.market.settlement_factor_wad() < WAD);

    assert_eq!(state.market.total_repaid(), borrow_amount);
    assert_eq!(state.market.scaled_total_supply(), 0);
    assert!(
        state.vault_balance <= 1,
        "full cycle should leave at most rounding dust, got {}",
        state.vault_balance
    );
}

// ===========================================================================
// Regression Scenario 2: Partial Default
// ===========================================================================
//
// The borrower only repays part of what they owe. Lenders take a loss.
//
// Timeline:
//   T=0       — market created, 10% annual, maturity at T=YEAR, zero fees
//   T=0       — lender deposits 1,000,000 (1000 USDC, 6 dec)
//   T=0       — borrower borrows 800,000 (800 USDC)
//   T=YEAR    — borrower repays only 400,000 (partial — 400 USDC out of 800)
//   T=YEAR    — lender withdraws; settlement factor < WAD, taking a loss

#[test]
fn regression_partial_default() {
    let creation_ts: i64 = 0;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity_ts: i64 = year;
    let deposit_amount: u64 = 1_000_000; // 1 USDC (simpler numbers for clarity)
    let borrow_amount: u64 = 800_000; // 0.8 USDC
    let repay_amount: u64 = 400_000; // 0.4 USDC (partial)

    let scenario = RegressionScenario {
        name: "partial_default",
        initial_state: InitialState {
            fee_rate_bps: 0,
            annual_interest_bps: 1000, // 10%
            maturity_timestamp: maturity_ts,
            max_total_supply: 100_000_000,
            creation_timestamp: creation_ts,
            num_lenders: 1,
            borrower_whitelisted: true,
            borrower_max_capacity: 10_000_000,
        },
        transactions: vec![
            // Step 0: Deposit
            (
                Transaction::Deposit {
                    lender_index: 0,
                    amount: deposit_amount,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_amount),
                    total_deposited: Some(deposit_amount),
                    lender_scaled_balance: Some((0, u128::from(deposit_amount))),
                    description: Some("deposit 1 USDC"),
                    ..Default::default()
                },
            ),
            // Step 1: Borrow 0.8 USDC
            (
                Transaction::Borrow {
                    amount: borrow_amount,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_amount - borrow_amount), // 200,000
                    total_borrowed: Some(borrow_amount),
                    description: Some("borrow 0.8 USDC"),
                    ..Default::default()
                },
            ),
            // Step 2: Partial repay at maturity
            (
                Transaction::Repay {
                    amount: repay_amount,
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    // vault = 200_000 + 400_000 = 600_000
                    vault_balance: Some(200_000 + repay_amount),
                    total_repaid: Some(repay_amount),
                    description: Some("partial repay 0.4 USDC"),
                    ..Default::default()
                },
            ),
            // Step 3: Withdraw all
            // After repay accrual (zero fee), scale_factor = WAD + WAD/10 = 1.1*WAD
            // total_normalized = 1_000_000 * 1.1*WAD / WAD = 1_100_000
            // available = 600_000 (vault) - 0 (fees) = 600_000
            // settlement_factor = 600_000 * WAD / 1_100_000
            //   = 600_000 / 1_100_000 * WAD = (6/11) * WAD
            // payout = 1_000_000 * 1.1*WAD / WAD * (6/11)*WAD / WAD
            //        = 1_100_000 * (6/11) = 600_000
            // Lender recovers 600,000 out of deposited 1,000,000 => 40% loss
            (
                Transaction::Withdraw {
                    lender_index: 0,
                    scaled_amount: 0,
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    lender_scaled_balance: Some((0, 0)),
                    scaled_total_supply: Some(0),
                    description: Some("withdraw with partial default loss"),
                    ..Default::default()
                },
            ),
        ],
    };

    replay_scenario(&scenario);

    // Strengthened postconditions: exact settlement math for a partial default.
    let mut state = initialize_simulation_state(&scenario.initial_state);

    for (tx, _) in &scenario.transactions {
        execute_transaction(&mut state, tx, "partial_default re-run");
    }

    let expected_scale_factor = expected_scale_factor(WAD, 1000, SECONDS_PER_YEAR as i64);
    assert_eq!(state.market.scale_factor(), expected_scale_factor);
    let total_normalized = u128::from(deposit_amount) * expected_scale_factor / WAD; // 1_100_000
    let expected_settlement = (u128::from(200_000 + repay_amount) * WAD) / total_normalized;
    assert_eq!(state.market.settlement_factor_wad(), expected_settlement);

    // Settlement factor should be < WAD (lender haircut).
    assert!(
        state.market.settlement_factor_wad() < WAD,
        "partial default should result in settlement_factor < WAD, got {}",
        state.market.settlement_factor_wad()
    );

    assert_eq!(state.market.scaled_total_supply(), 0);
    assert_eq!(state.lender_positions[0].scaled_balance(), 0);

    // Verify vault is nearly empty after full withdrawal (allowing rounding dust).
    assert!(
        state.vault_balance <= 1,
        "vault should be nearly empty after full withdrawal, got {}",
        state.vault_balance
    );
}

// ===========================================================================
// Regression Scenario 3: Fee Collection
// ===========================================================================
//
// Tests that protocol fees accrue correctly with interest and can be collected.
//
// Timeline:
//   T=0       — market created, 10% annual, 5% protocol fee, maturity at T=2*YEAR
//   T=0       — lender deposits 1,000,000 (1 USDC, 6 dec)
//   T=YEAR    — accrue interest (explicit) to lock in fees
//   T=YEAR    — collect fees

#[test]
fn regression_fee_collection() {
    let creation_ts: i64 = 0;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity_ts: i64 = 2 * year; // maturity far enough to accrue for a full year
    let deposit_amount: u64 = 1_000_000_000_000; // 1M USDC

    let scaled_supply: u128 = u128::from(deposit_amount);
    let expected_sf = expected_scale_factor(WAD, 1000, year);
    let expected_fees = expected_fee_delta(scaled_supply, WAD, 1000, 500, year);

    let scenario = RegressionScenario {
        name: "fee_collection",
        initial_state: InitialState {
            fee_rate_bps: 500,         // 5% of interest goes to protocol
            annual_interest_bps: 1000, // 10% annual
            maturity_timestamp: maturity_ts,
            max_total_supply: 10_000_000_000_000, // 10M USDC cap
            creation_timestamp: creation_ts,
            num_lenders: 1,
            borrower_whitelisted: true,
            borrower_max_capacity: 10_000_000_000_000,
        },
        transactions: vec![
            // Step 0: Deposit 1M USDC
            (
                Transaction::Deposit {
                    lender_index: 0,
                    amount: deposit_amount,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_amount),
                    scale_factor: Some(WAD),
                    accrued_protocol_fees: Some(0),
                    description: Some("deposit 1M USDC"),
                    ..Default::default()
                },
            ),
            // Step 1: Accrue interest for 1 full year
            (
                Transaction::AccrueInterest {
                    current_timestamp: year,
                },
                ExpectedOutcome {
                    scale_factor: Some(expected_sf),
                    accrued_protocol_fees: Some(expected_fees),
                    description: Some("accrue 1 year of interest with 5% fee"),
                    ..Default::default()
                },
            ),
            // Step 2: Collect fees
            (
                Transaction::CollectFees {
                    current_timestamp: year,
                },
                ExpectedOutcome {
                    // Vault reduced by collected fees
                    vault_balance: Some(deposit_amount - expected_fees),
                    accrued_protocol_fees: Some(0), // All fees collected
                    description: Some("collect all accrued protocol fees"),
                    ..Default::default()
                },
            ),
        ],
    };

    replay_scenario(&scenario);

    // Strengthened postconditions: exact intermediate and final fee behavior.
    let mut state = initialize_simulation_state(&scenario.initial_state);

    execute_transaction(
        &mut state,
        &scenario.transactions[0].0,
        "fee_collection deposit replay",
    );
    assert_eq!(state.vault_balance, deposit_amount);
    assert_eq!(state.market.scale_factor(), WAD);
    assert_eq!(state.market.accrued_protocol_fees(), 0);

    execute_transaction(
        &mut state,
        &scenario.transactions[1].0,
        "fee_collection accrue replay",
    );
    assert_eq!(state.market.scale_factor(), expected_sf);
    assert_eq!(state.market.accrued_protocol_fees(), expected_fees);

    execute_transaction(
        &mut state,
        &scenario.transactions[2].0,
        "fee_collection collect replay",
    );
    assert_eq!(state.market.accrued_protocol_fees(), 0);
    assert_eq!(state.vault_balance, deposit_amount - expected_fees);

    // Verify fees are non-trivial for this parameter set.
    assert!(
        expected_fees > 0,
        "expected non-zero protocol fees for 1M USDC at 10% rate, 5% fee"
    );
}

// ===========================================================================
// Additional Regression: Multi-Lender Proportional Withdrawal
// ===========================================================================
//
// Two lenders deposit different amounts. After maturity they each withdraw
// their proportional share, verifying the scaled balance math.

#[test]
fn regression_multi_lender_proportional() {
    let creation_ts: i64 = 0;
    let year: i64 = SECONDS_PER_YEAR as i64;
    let maturity_ts: i64 = year;
    let deposit_a: u64 = 3_000_000; // 3 USDC
    let deposit_b: u64 = 7_000_000; // 7 USDC

    let scenario = RegressionScenario {
        name: "multi_lender_proportional",
        initial_state: InitialState {
            fee_rate_bps: 0,
            annual_interest_bps: 0, // 0% interest for clean proportional math
            maturity_timestamp: maturity_ts,
            max_total_supply: 100_000_000,
            creation_timestamp: creation_ts,
            num_lenders: 2,
            borrower_whitelisted: true,
            borrower_max_capacity: 100_000_000,
        },
        transactions: vec![
            // Lender A deposits 3 USDC
            (
                Transaction::Deposit {
                    lender_index: 0,
                    amount: deposit_a,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_a),
                    lender_scaled_balance: Some((0, u128::from(deposit_a))),
                    description: Some("lender A deposits 3 USDC"),
                    ..Default::default()
                },
            ),
            // Lender B deposits 7 USDC
            (
                Transaction::Deposit {
                    lender_index: 1,
                    amount: deposit_b,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_a + deposit_b),
                    lender_scaled_balance: Some((1, u128::from(deposit_b))),
                    scaled_total_supply: Some(u128::from(deposit_a + deposit_b)),
                    description: Some("lender B deposits 7 USDC"),
                    ..Default::default()
                },
            ),
            // Lender A withdraws all at maturity
            // With 0% interest, scale_factor = WAD, settlement_factor = WAD
            // Payout = 3_000_000 * WAD / WAD * WAD / WAD = 3_000_000
            (
                Transaction::Withdraw {
                    lender_index: 0,
                    scaled_amount: 0,
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_b), // 7M left
                    lender_scaled_balance: Some((0, 0)),
                    settlement_factor_wad: Some(WAD), // Full settlement
                    description: Some("lender A withdraws 3 USDC"),
                    ..Default::default()
                },
            ),
            // Lender B withdraws all at maturity
            (
                Transaction::Withdraw {
                    lender_index: 1,
                    scaled_amount: 0,
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(0),
                    lender_scaled_balance: Some((1, 0)),
                    scaled_total_supply: Some(0),
                    description: Some("lender B withdraws 7 USDC"),
                    ..Default::default()
                },
            ),
        ],
    };

    replay_scenario(&scenario);

    // Strengthened postconditions: exact proportional payouts and conservation.
    let mut state = initialize_simulation_state(&scenario.initial_state);
    execute_transaction(
        &mut state,
        &scenario.transactions[0].0,
        "multi_lender_proportional deposit A replay",
    );
    execute_transaction(
        &mut state,
        &scenario.transactions[1].0,
        "multi_lender_proportional deposit B replay",
    );
    assert_eq!(
        state.market.scaled_total_supply(),
        u128::from(deposit_a + deposit_b)
    );
    assert_eq!(state.market.settlement_factor_wad(), 0);

    let vault_before_a = state.vault_balance;
    execute_transaction(
        &mut state,
        &scenario.transactions[2].0,
        "multi_lender_proportional withdraw A replay",
    );
    let payout_a = vault_before_a - state.vault_balance;
    assert_eq!(payout_a, deposit_a);
    assert_eq!(state.market.settlement_factor_wad(), WAD);

    let vault_before_b = state.vault_balance;
    execute_transaction(
        &mut state,
        &scenario.transactions[3].0,
        "multi_lender_proportional withdraw B replay",
    );
    let payout_b = vault_before_b - state.vault_balance;
    assert_eq!(payout_b, deposit_b);

    assert_eq!(state.vault_balance, 0);
    assert_eq!(state.market.scaled_total_supply(), 0);
    assert_eq!(state.lender_positions[0].scaled_balance(), 0);
    assert_eq!(state.lender_positions[1].scaled_balance(), 0);
}

// ===========================================================================
// Additional Regression: Interest Accrual Capped at Maturity
// ===========================================================================

#[test]
fn regression_interest_capped_at_maturity() {
    let creation_ts: i64 = 0;
    let half_year: i64 = SECONDS_PER_YEAR as i64 / 2;
    let maturity_ts: i64 = half_year; // maturity at half year
    let far_future: i64 = SECONDS_PER_YEAR as i64 * 10; // 10 years later

    let expected_sf: u128 = expected_scale_factor(WAD, 1000, half_year);

    let scenario = RegressionScenario {
        name: "interest_capped_at_maturity",
        initial_state: InitialState {
            fee_rate_bps: 0,
            annual_interest_bps: 1000,
            maturity_timestamp: maturity_ts,
            max_total_supply: 100_000_000_000,
            creation_timestamp: creation_ts,
            num_lenders: 0,
            borrower_whitelisted: true,
            borrower_max_capacity: 100_000_000_000,
        },
        transactions: vec![
            // Accrue at far future -- should cap at maturity
            (
                Transaction::AccrueInterest {
                    current_timestamp: far_future,
                },
                ExpectedOutcome {
                    scale_factor: Some(expected_sf),
                    description: Some("interest capped at maturity despite far future timestamp"),
                    ..Default::default()
                },
            ),
            // Accrue again at an even later time -- should be a no-op
            (
                Transaction::AccrueInterest {
                    current_timestamp: far_future + 1_000_000,
                },
                ExpectedOutcome {
                    scale_factor: Some(expected_sf), // unchanged
                    description: Some("second accrual after maturity is no-op"),
                    ..Default::default()
                },
            ),
        ],
    };

    replay_scenario(&scenario);

    // Strengthened postconditions: x-1/x/x+1 maturity-boundary accrual checks.
    let sf_at_boundary = {
        let mut state = initialize_simulation_state(&scenario.initial_state);
        execute_transaction(
            &mut state,
            &Transaction::AccrueInterest {
                current_timestamp: maturity_ts,
            },
            "interest_capped boundary x",
        );
        state.market.scale_factor()
    };

    let sf_before_boundary = {
        let mut state = initialize_simulation_state(&scenario.initial_state);
        execute_transaction(
            &mut state,
            &Transaction::AccrueInterest {
                current_timestamp: maturity_ts - 1,
            },
            "interest_capped boundary x-1",
        );
        state.market.scale_factor()
    };

    let sf_after_boundary = {
        let mut state = initialize_simulation_state(&scenario.initial_state);
        execute_transaction(
            &mut state,
            &Transaction::AccrueInterest {
                current_timestamp: maturity_ts + 1,
            },
            "interest_capped boundary x+1",
        );
        state.market.scale_factor()
    };

    let expected_before = expected_scale_factor(WAD, 1000, maturity_ts - 1);
    assert_eq!(sf_before_boundary, expected_before);
    assert_eq!(sf_at_boundary, expected_sf);
    assert_eq!(sf_after_boundary, expected_sf);
    assert!(sf_before_boundary < sf_at_boundary);
}

// ===========================================================================
// Additional Regression: ReSettle After Additional Repayment
// ===========================================================================

#[test]
fn regression_resettle_after_repay() {
    let creation_ts: i64 = 0;
    let maturity_ts: i64 = SECONDS_PER_YEAR as i64;
    let deposit_amount: u64 = 1_000_000; // 1 USDC
    let borrow_amount: u64 = 900_000; // 0.9 USDC
    let partial_repay: u64 = 200_000; // first partial repay
    let second_repay: u64 = 500_000; // second repay after settlement

    let scenario = RegressionScenario {
        name: "resettle_after_repay",
        initial_state: InitialState {
            fee_rate_bps: 0,
            annual_interest_bps: 0, // 0% for clean math
            maturity_timestamp: maturity_ts,
            max_total_supply: 100_000_000,
            creation_timestamp: creation_ts,
            num_lenders: 1,
            borrower_whitelisted: true,
            borrower_max_capacity: 10_000_000,
        },
        transactions: vec![
            // Deposit
            (
                Transaction::Deposit {
                    lender_index: 0,
                    amount: deposit_amount,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_amount),
                    description: Some("deposit 1 USDC"),
                    ..Default::default()
                },
            ),
            // Borrow
            (
                Transaction::Borrow {
                    amount: borrow_amount,
                    current_timestamp: creation_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(deposit_amount - borrow_amount), // 100,000
                    description: Some("borrow 0.9 USDC"),
                    ..Default::default()
                },
            ),
            // Partial repay at maturity
            (
                Transaction::Repay {
                    amount: partial_repay,
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    vault_balance: Some(100_000 + partial_repay), // 300,000
                    description: Some("partial repay 0.2 USDC"),
                    ..Default::default()
                },
            ),
            // First withdrawal triggers settlement
            // With 0% interest, sf = WAD, total_normalized = 1_000_000
            // available = 300_000, settlement = 300_000 * WAD / 1_000_000 = 0.3 * WAD
            // payout = 1_000_000 * WAD / WAD * 0.3*WAD / WAD = 300_000
            // But we do a PARTIAL withdraw of half the balance to keep some for re-settle test
            (
                Transaction::Withdraw {
                    lender_index: 0,
                    scaled_amount: 500_000, // half of 1_000_000
                    current_timestamp: maturity_ts,
                },
                ExpectedOutcome {
                    lender_scaled_balance: Some((0, 500_000)),
                    description: Some("partial withdraw triggers settlement at 30%"),
                    ..Default::default()
                },
            ),
            // Second repay (after settlement)
            (
                Transaction::Repay {
                    amount: second_repay,
                    current_timestamp: maturity_ts + 100,
                },
                ExpectedOutcome {
                    description: Some("second repay after settlement"),
                    ..Default::default()
                },
            ),
            // ReSettle — factor should improve
            (
                Transaction::ReSettle {
                    current_timestamp: maturity_ts + 200,
                },
                ExpectedOutcome {
                    description: Some("re-settle improves settlement factor"),
                    ..Default::default()
                },
            ),
        ],
    };

    replay_scenario(&scenario);

    // Strengthened postconditions: exact first-settlement and re-settlement math.
    let mut state = initialize_simulation_state(&scenario.initial_state);

    execute_transaction(
        &mut state,
        &scenario.transactions[0].0,
        "resettle_after_repay deposit replay",
    );
    execute_transaction(
        &mut state,
        &scenario.transactions[1].0,
        "resettle_after_repay borrow replay",
    );
    execute_transaction(
        &mut state,
        &scenario.transactions[2].0,
        "resettle_after_repay first repay replay",
    );
    execute_transaction(
        &mut state,
        &scenario.transactions[3].0,
        "resettle_after_repay first withdraw replay",
    );

    let first_settlement = state.market.settlement_factor_wad();
    assert_eq!(first_settlement, (3 * WAD) / 10);
    assert_eq!(state.lender_positions[0].scaled_balance(), 500_000);
    assert_eq!(state.vault_balance, 150_000);

    execute_transaction(
        &mut state,
        &scenario.transactions[4].0,
        "resettle_after_repay second repay replay",
    );
    assert_eq!(state.vault_balance, 650_000);
    assert_eq!(state.market.total_repaid(), partial_repay + second_repay);

    execute_transaction(
        &mut state,
        &scenario.transactions[5].0,
        "resettle_after_repay resettle replay",
    );

    let improved_settlement = state.market.settlement_factor_wad();
    assert!(improved_settlement > first_settlement);
    assert_eq!(improved_settlement, WAD);
}
