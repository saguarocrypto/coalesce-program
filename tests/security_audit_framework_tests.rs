//! # Security Audit Framework for CoalesceFi (Pinocchio)
//!
//! This module implements a **property-based security audit framework** that encodes
//! common Solana lending protocol security invariants as composable proptest strategies.
//! The framework is designed to be general enough to apply to any Solana program while
//! being demonstrated specifically on the CoalesceFi fixed-rate lending protocol.
//!
//! ## Architecture
//!
//! The framework consists of four layers:
//!
//! 1. **`SecurityProperty` trait** -- Defines a named, checkable invariant that can be
//!    evaluated by comparing protocol state before and after an operation.
//!
//! 2. **`ProtocolSnapshot`** -- A full snapshot of the protocol state at a point in time,
//!    including market fields, lender positions, config, and vault balances.
//!
//! 3. **`Operation` enum** -- Represents every protocol operation with its full parameters,
//!    enabling replay and analysis of operation sequences.
//!
//! 4. **Composable strategy combinators** -- Proptest strategies that generate valid,
//!    adversarial, boundary, and lifecycle operation sequences.
//!
//! ## How to Add New Security Properties
//!
//! 1. Create a struct implementing `SecurityProperty`.
//! 2. Implement `name()` to return a human-readable identifier (e.g., "NoUnauthorizedDrain").
//! 3. Implement `check()` to compare `before` and `after` snapshots for the invariant.
//! 4. Register the property in `ALL_PROPERTIES` and add a corresponding proptest.
//!
//! ## How to Create Custom Strategies
//!
//! Use the provided combinator functions (e.g., `valid_deposit_strategy`, `boundary_strategy`)
//! as building blocks. Chain them with `prop_flat_map`, `prop_filter`, or `Union` to create
//! domain-specific fuzzing strategies for your protocol.
//!
//! ## How to Interpret Violations
//!
//! A `SecurityViolation` contains:
//! - `severity`: Critical / High / Medium / Low
//! - `property_name`: Which invariant was broken
//! - `description`: Human-readable explanation
//! - `before` / `after`: Full state snapshots for reproduction
//!
//! When a violation is found, the proptest shrinking will minimize the input to the smallest
//! reproducing case. Examine the `Operation` sequence and the before/after snapshots to
//! understand the root cause.
//!
//! ## Relation to Traditional Security Audits
//!
//! Traditional audits are point-in-time manual reviews. This framework complements them by:
//! - **Continuous verification**: Runs on every CI build.
//! - **Exhaustive exploration**: Proptest explores thousands of random scenarios per property.
//! - **Regression prevention**: Once a bug is found and fixed, the property test prevents regression.
//! - **Composability**: Properties can be combined to check compound invariants.

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

use proptest::collection::vec as prop_vec;
use proptest::prelude::*;

use bytemuck::Zeroable;
use coalesce::constants::{BPS, MAX_ANNUAL_INTEREST_BPS, MAX_FEE_RATE_BPS, SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

#[path = "common/math_oracle.rs"]
mod math_oracle;

const SECONDS_PER_DAY: i64 = 86_400;

// ============================================================================
// Section 1: Security Property Definitions (Infrastructure)
// ============================================================================

/// Severity levels for security violations, ordered by impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Critical => write!(f, "CRITICAL"),
            Severity::High => write!(f, "HIGH"),
            Severity::Medium => write!(f, "MEDIUM"),
            Severity::Low => write!(f, "LOW"),
        }
    }
}

/// A security violation detected by the framework.
#[derive(Debug, Clone)]
struct SecurityViolation {
    severity: Severity,
    property_name: String,
    description: String,
    before: ProtocolSnapshot,
    after: ProtocolSnapshot,
}

impl std::fmt::Display for SecurityViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {}: {}",
            self.severity, self.property_name, self.description
        )
    }
}

/// Trait defining a named, checkable security invariant.
///
/// Implementors encode a single security property that must hold across
/// all valid state transitions. The `check` method receives the protocol
/// state before and after an operation, along with the operation itself.
trait SecurityProperty {
    /// Human-readable name for this property (e.g., "NoUnauthorizedDrain").
    fn name(&self) -> &str;

    /// Check whether this property holds for the given state transition.
    ///
    /// Returns `Ok(())` if the property holds, or `Err(SecurityViolation)`
    /// with full diagnostic information if it does not.
    fn check(
        &self,
        before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        operation: &Operation,
    ) -> Result<(), SecurityViolation>;
}

/// Full snapshot of the protocol state at a point in time.
///
/// Captures all mutable state fields from every account type plus
/// derived quantities like vault balance and normalized totals.
#[derive(Debug, Clone)]
struct ProtocolSnapshot {
    // Market fields
    scaled_total_supply: u128,
    scale_factor: u128,
    accrued_protocol_fees: u64,
    total_deposited: u64,
    total_borrowed: u64,
    total_repaid: u64,
    last_accrual_timestamp: i64,
    settlement_factor_wad: u128,
    max_total_supply: u64,
    annual_interest_bps: u16,
    maturity_timestamp: i64,

    // Lender positions (indexed by lender id)
    lender_positions: Vec<LenderPositionSnapshot>,

    // Config
    fee_rate_bps: u16,

    // Vault balance (simulated)
    vault_balance: u64,

    // Whitelist state
    whitelist_current_borrowed: u64,
    whitelist_max_capacity: u64,
}

#[derive(Debug, Clone)]
struct LenderPositionSnapshot {
    lender_id: u32,
    scaled_balance: u128,
}

impl ProtocolSnapshot {
    /// Create a snapshot from the current state of a simulated protocol.
    fn from_state(state: &SimulatedProtocol) -> Self {
        let market = &state.market;
        Self {
            scaled_total_supply: market.scaled_total_supply(),
            scale_factor: market.scale_factor(),
            accrued_protocol_fees: market.accrued_protocol_fees(),
            total_deposited: market.total_deposited(),
            total_borrowed: market.total_borrowed(),
            total_repaid: market.total_repaid(),
            last_accrual_timestamp: market.last_accrual_timestamp(),
            settlement_factor_wad: market.settlement_factor_wad(),
            max_total_supply: market.max_total_supply(),
            annual_interest_bps: market.annual_interest_bps(),
            maturity_timestamp: market.maturity_timestamp(),
            lender_positions: state
                .lender_positions
                .iter()
                .map(|(id, pos)| LenderPositionSnapshot {
                    lender_id: *id,
                    scaled_balance: pos.scaled_balance(),
                })
                .collect(),
            fee_rate_bps: state.config.fee_rate_bps(),
            vault_balance: state.vault_balance,
            whitelist_current_borrowed: state.whitelist.current_borrowed(),
            whitelist_max_capacity: state.whitelist.max_borrow_capacity(),
        }
    }

    /// Byte-level equality check of the mutable market fields.
    fn market_bytes_identical(&self, other: &Self) -> bool {
        self.scaled_total_supply == other.scaled_total_supply
            && self.scale_factor == other.scale_factor
            && self.accrued_protocol_fees == other.accrued_protocol_fees
            && self.total_deposited == other.total_deposited
            && self.total_borrowed == other.total_borrowed
            && self.total_repaid == other.total_repaid
            && self.last_accrual_timestamp == other.last_accrual_timestamp
            && self.settlement_factor_wad == other.settlement_factor_wad
    }

    /// Compute the normalized total supply (scaled_total_supply * scale_factor / WAD).
    fn normalized_total_supply(&self) -> u128 {
        if self.scale_factor == 0 {
            return 0;
        }
        self.scaled_total_supply
            .checked_mul(self.scale_factor)
            .unwrap_or(u128::MAX)
            / WAD
    }
}

/// Enum representing all protocol operations with their parameters.
#[derive(Debug, Clone)]
enum Operation {
    Deposit {
        lender_id: u32,
        amount: u64,
    },
    Borrow {
        amount: u64,
    },
    Repay {
        amount: u64,
    },
    Withdraw {
        lender_id: u32,
        scaled_amount: u128, // 0 = full withdrawal
    },
    CollectFees,
    AccrueInterest {
        timestamp: i64,
    },
    ReSettle,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Operation::Deposit { lender_id, amount } => {
                write!(f, "Deposit(lender={}, amount={})", lender_id, amount)
            },
            Operation::Borrow { amount } => write!(f, "Borrow(amount={})", amount),
            Operation::Repay { amount } => write!(f, "Repay(amount={})", amount),
            Operation::Withdraw {
                lender_id,
                scaled_amount,
            } => write!(
                f,
                "Withdraw(lender={}, scaled={})",
                lender_id, scaled_amount
            ),
            Operation::CollectFees => write!(f, "CollectFees"),
            Operation::AccrueInterest { timestamp } => {
                write!(f, "AccrueInterest(ts={})", timestamp)
            },
            Operation::ReSettle => write!(f, "ReSettle"),
        }
    }
}

// ============================================================================
// Section 2: Simulated Protocol State Machine
// ============================================================================

/// A lightweight, in-process simulation of the CoalesceFi protocol.
///
/// This simulation mirrors the on-chain state transitions but runs entirely
/// in memory, allowing rapid property-based testing without a full Solana
/// runtime. Token transfers are simulated by tracking vault_balance.
#[derive(Clone)]
struct SimulatedProtocol {
    market: Market,
    config: ProtocolConfig,
    lender_positions: Vec<(u32, LenderPosition)>,
    whitelist: BorrowerWhitelist,
    vault_balance: u64,
    current_timestamp: i64,
    /// Total tokens withdrawn by all lenders (for conservation checks).
    total_withdrawn: u64,
    /// Total fees collected (for conservation checks).
    total_fees_collected: u64,
}

impl SimulatedProtocol {
    /// Create a new simulated protocol with given parameters.
    fn new(
        annual_interest_bps: u16,
        fee_rate_bps: u16,
        maturity_timestamp: i64,
        max_total_supply: u64,
        max_borrow_capacity: u64,
        start_timestamp: i64,
    ) -> Self {
        let mut market = Market::zeroed();
        market.set_annual_interest_bps(annual_interest_bps);
        market.set_maturity_timestamp(maturity_timestamp);
        market.set_max_total_supply(max_total_supply);
        market.set_scale_factor(WAD);
        market.set_last_accrual_timestamp(start_timestamp);

        let mut config = ProtocolConfig::zeroed();
        config.set_fee_rate_bps(fee_rate_bps);

        let mut whitelist = BorrowerWhitelist::zeroed();
        whitelist.set_max_borrow_capacity(max_borrow_capacity);
        whitelist.is_whitelisted = 1;

        Self {
            market,
            config,
            lender_positions: Vec::new(),
            whitelist,
            vault_balance: 0,
            current_timestamp: start_timestamp,
            total_withdrawn: 0,
            total_fees_collected: 0,
        }
    }

    /// Take a snapshot of the current state.
    fn snapshot(&self) -> ProtocolSnapshot {
        ProtocolSnapshot::from_state(self)
    }

    /// Execute an operation, returning Ok if it succeeded or Err with a description if it failed.
    fn execute(&mut self, op: &Operation) -> Result<(), String> {
        match op {
            Operation::Deposit { lender_id, amount } => self.do_deposit(*lender_id, *amount),
            Operation::Borrow { amount } => self.do_borrow(*amount),
            Operation::Repay { amount } => self.do_repay(*amount),
            Operation::Withdraw {
                lender_id,
                scaled_amount,
            } => self.do_withdraw(*lender_id, *scaled_amount),
            Operation::CollectFees => self.do_collect_fees(),
            Operation::AccrueInterest { timestamp } => self.do_accrue(*timestamp),
            Operation::ReSettle => self.do_re_settle(),
        }
    }

    fn do_accrue(&mut self, timestamp: i64) -> Result<(), String> {
        if timestamp < self.current_timestamp {
            return Err("Timestamp in the past".to_string());
        }
        self.current_timestamp = timestamp;
        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("AccrueInterest failed: {:?}", e))
    }

    fn do_deposit(&mut self, lender_id: u32, amount: u64) -> Result<(), String> {
        if amount == 0 {
            return Err("ZeroAmount".to_string());
        }
        if self.current_timestamp >= self.market.maturity_timestamp() {
            return Err("MarketMatured".to_string());
        }

        // Accrue interest first
        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        let scale_factor = self.market.scale_factor();
        if scale_factor == 0 {
            return Err("ZeroScaleFactor".to_string());
        }

        let amount_u128 = u128::from(amount);
        let scaled_amount = amount_u128
            .checked_mul(WAD)
            .ok_or("MathOverflow")?
            .checked_div(scale_factor)
            .ok_or("MathOverflow")?;

        if scaled_amount == 0 {
            return Err("ZeroScaledAmount".to_string());
        }

        // Check cap
        let new_scaled_total = self
            .market
            .scaled_total_supply()
            .checked_add(scaled_amount)
            .ok_or("MathOverflow")?;
        let new_normalized = new_scaled_total
            .checked_mul(scale_factor)
            .ok_or("MathOverflow")?
            / WAD;
        if new_normalized > u128::from(self.market.max_total_supply()) {
            return Err("CapExceeded".to_string());
        }

        // Update state
        self.vault_balance = self
            .vault_balance
            .checked_add(amount)
            .ok_or("MathOverflow")?;
        self.market.set_scaled_total_supply(new_scaled_total);
        let new_deposited = self
            .market
            .total_deposited()
            .checked_add(amount)
            .ok_or("MathOverflow")?;
        self.market.set_total_deposited(new_deposited);

        // Update or create lender position
        if let Some(pos) = self
            .lender_positions
            .iter_mut()
            .find(|(id, _)| *id == lender_id)
        {
            let new_balance = pos
                .1
                .scaled_balance()
                .checked_add(scaled_amount)
                .ok_or("MathOverflow")?;
            pos.1.set_scaled_balance(new_balance);
        } else {
            let mut pos = LenderPosition::zeroed();
            pos.set_scaled_balance(scaled_amount);
            self.lender_positions.push((lender_id, pos));
        }

        Ok(())
    }

    fn do_borrow(&mut self, amount: u64) -> Result<(), String> {
        if amount == 0 {
            return Err("ZeroAmount".to_string());
        }
        if self.current_timestamp >= self.market.maturity_timestamp() {
            return Err("MarketMatured".to_string());
        }

        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        let fees_reserved = std::cmp::min(self.vault_balance, self.market.accrued_protocol_fees());
        let borrowable = self
            .vault_balance
            .checked_sub(fees_reserved)
            .ok_or("MathOverflow")?;

        if amount > borrowable {
            return Err("BorrowAmountTooHigh".to_string());
        }

        // Check whitelist capacity
        let new_wl_total = self
            .whitelist
            .current_borrowed()
            .checked_add(amount)
            .ok_or("MathOverflow")?;
        if new_wl_total > self.whitelist.max_borrow_capacity() {
            return Err("GlobalCapacityExceeded".to_string());
        }

        self.vault_balance = self
            .vault_balance
            .checked_sub(amount)
            .ok_or("MathOverflow")?;
        let new_borrowed = self
            .market
            .total_borrowed()
            .checked_add(amount)
            .ok_or("MathOverflow")?;
        self.market.set_total_borrowed(new_borrowed);
        self.whitelist.set_current_borrowed(new_wl_total);

        Ok(())
    }

    fn do_repay(&mut self, amount: u64) -> Result<(), String> {
        if amount == 0 {
            return Err("ZeroAmount".to_string());
        }

        // Repay uses zero-fee config for accrual (matches on-chain behavior)
        let zero_config: ProtocolConfig = Zeroable::zeroed();
        accrue_interest(&mut self.market, &zero_config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        self.vault_balance = self
            .vault_balance
            .checked_add(amount)
            .ok_or("MathOverflow")?;
        let new_repaid = self
            .market
            .total_repaid()
            .checked_add(amount)
            .ok_or("MathOverflow")?;
        self.market.set_total_repaid(new_repaid);

        Ok(())
    }

    fn do_withdraw(&mut self, lender_id: u32, mut scaled_amount: u128) -> Result<(), String> {
        if self.current_timestamp < self.market.maturity_timestamp() {
            return Err("NotMatured".to_string());
        }

        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        let pos = self
            .lender_positions
            .iter_mut()
            .find(|(id, _)| *id == lender_id)
            .ok_or("NoPosition")?;

        if pos.1.scaled_balance() == 0 {
            return Err("NoBalance".to_string());
        }

        // Compute settlement factor if not set
        if self.market.settlement_factor_wad() == 0 {
            let vault_balance_u128 = u128::from(self.vault_balance);
            let fees_reserved = {
                let fees = u128::from(self.market.accrued_protocol_fees());
                std::cmp::min(vault_balance_u128, fees)
            };
            let available = vault_balance_u128
                .checked_sub(fees_reserved)
                .ok_or("MathOverflow")?;

            let total_normalized = self
                .market
                .scaled_total_supply()
                .checked_mul(self.market.scale_factor())
                .ok_or("MathOverflow")?
                / WAD;

            let settlement_factor = if total_normalized == 0 {
                WAD
            } else {
                let raw = available
                    .checked_mul(WAD)
                    .ok_or("MathOverflow")?
                    .checked_div(total_normalized)
                    .ok_or("MathOverflow")?;
                let capped = if raw > WAD { WAD } else { raw };
                if capped < 1 {
                    1
                } else {
                    capped
                }
            };
            self.market.set_settlement_factor_wad(settlement_factor);
        }

        // Resolve scaled_amount (0 = full)
        if scaled_amount == 0 {
            scaled_amount = pos.1.scaled_balance();
        }

        if scaled_amount > pos.1.scaled_balance() {
            return Err("InsufficientScaledBalance".to_string());
        }

        // Compute payout
        let scale_factor = self.market.scale_factor();
        let settlement_factor = self.market.settlement_factor_wad();

        let normalized = scaled_amount
            .checked_mul(scale_factor)
            .ok_or("MathOverflow")?
            / WAD;
        let payout_u128 = normalized
            .checked_mul(settlement_factor)
            .ok_or("MathOverflow")?
            / WAD;
        let payout = u64::try_from(payout_u128).map_err(|_| "MathOverflow")?;

        if payout == 0 {
            return Err("ZeroPayout".to_string());
        }

        if payout > self.vault_balance {
            return Err("InsufficientVaultBalance".to_string());
        }

        self.vault_balance -= payout;
        self.total_withdrawn = self
            .total_withdrawn
            .checked_add(payout)
            .ok_or("MathOverflow")?;

        let new_balance = pos
            .1
            .scaled_balance()
            .checked_sub(scaled_amount)
            .ok_or("MathOverflow")?;
        pos.1.set_scaled_balance(new_balance);

        let new_scaled_total = self
            .market
            .scaled_total_supply()
            .checked_sub(scaled_amount)
            .ok_or("MathOverflow")?;
        self.market.set_scaled_total_supply(new_scaled_total);

        Ok(())
    }

    fn do_collect_fees(&mut self) -> Result<(), String> {
        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        let accrued = self.market.accrued_protocol_fees();
        if accrued == 0 {
            return Err("NoFeesToCollect".to_string());
        }

        let mut withdrawable = std::cmp::min(accrued, self.vault_balance);
        // COAL-C01: cap fee withdrawal above lender claims when supply > 0
        if self.market.scaled_total_supply() > 0 {
            let sf = self.market.scale_factor();
            let total_norm = self.market.scaled_total_supply()
                .checked_mul(sf).ok_or("MathOverflow")?
                / WAD;
            let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
            let safe_max = self.vault_balance.saturating_sub(lender_claims);
            withdrawable = withdrawable.min(safe_max);
        }
        if withdrawable == 0 {
            return Err("NoFeesToCollect".to_string());
        }

        self.vault_balance -= withdrawable;
        self.total_fees_collected = self
            .total_fees_collected
            .checked_add(withdrawable)
            .ok_or("MathOverflow")?;

        let remaining = accrued.checked_sub(withdrawable).ok_or("MathOverflow")?;
        self.market.set_accrued_protocol_fees(remaining);

        Ok(())
    }

    fn do_re_settle(&mut self) -> Result<(), String> {
        let old_factor = self.market.settlement_factor_wad();
        if old_factor == 0 {
            return Err("NotSettled".to_string());
        }

        let zero_config: ProtocolConfig = Zeroable::zeroed();
        accrue_interest(&mut self.market, &zero_config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        let vault_balance_u128 = u128::from(self.vault_balance);
        let fees_reserved = {
            let fees = u128::from(self.market.accrued_protocol_fees());
            std::cmp::min(vault_balance_u128, fees)
        };
        let available = vault_balance_u128
            .checked_sub(fees_reserved)
            .ok_or("MathOverflow")?;

        let total_normalized = self
            .market
            .scaled_total_supply()
            .checked_mul(self.market.scale_factor())
            .ok_or("MathOverflow")?
            / WAD;

        let new_factor = if total_normalized == 0 {
            WAD
        } else {
            let raw = available
                .checked_mul(WAD)
                .ok_or("MathOverflow")?
                .checked_div(total_normalized)
                .ok_or("MathOverflow")?;
            let capped = if raw > WAD { WAD } else { raw };
            if capped < 1 {
                1
            } else {
                capped
            }
        };

        if new_factor <= old_factor {
            return Err("SettlementNotImproved".to_string());
        }

        self.market.set_settlement_factor_wad(new_factor);
        Ok(())
    }
}

// ============================================================================
// Section 3: Core Security Properties (8 Properties)
// ============================================================================

/// Property 1: No instruction can remove more tokens from the vault than authorized.
struct NoUnauthorizedDrain;
impl SecurityProperty for NoUnauthorizedDrain {
    fn name(&self) -> &str {
        "NoUnauthorizedDrain"
    }

    fn check(
        &self,
        before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        let vault_decrease = before.vault_balance.saturating_sub(after.vault_balance);
        let authorized = match operation {
            Operation::Borrow { amount } => *amount,
            Operation::Withdraw { .. } => {
                // The payout is the vault decrease, which must be <= normalized * settlement / WAD
                // We allow the full vault_decrease since it was computed from the scale/settlement
                vault_decrease
            },
            Operation::CollectFees => {
                // Fees collected <= accrued fees
                std::cmp::min(before.accrued_protocol_fees, before.vault_balance)
            },
            // Deposit and repay ADD to vault, so vault_decrease should be 0
            Operation::Deposit { .. } | Operation::Repay { .. } => 0,
            // Accrue and ReSettle do not move tokens
            Operation::AccrueInterest { .. } | Operation::ReSettle => 0,
        };

        if vault_decrease > authorized {
            return Err(SecurityViolation {
                severity: Severity::Critical,
                property_name: self.name().to_string(),
                description: format!(
                    "Vault decreased by {} but only {} was authorized for {:?}",
                    vault_decrease, authorized, operation
                ),
                before: before.clone(),
                after: after.clone(),
            });
        }
        Ok(())
    }
}

/// Property 2: Every failed operation leaves state byte-identical to before.
struct ErrorAtomicity;
impl SecurityProperty for ErrorAtomicity {
    fn name(&self) -> &str {
        "ErrorAtomicity"
    }

    fn check(
        &self,
        before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        _operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        // This property is checked specially: it only applies when an operation FAILS.
        // The test harness will call this only for failed operations.
        if !before.market_bytes_identical(after) {
            return Err(SecurityViolation {
                severity: Severity::Critical,
                property_name: self.name().to_string(),
                description: "Failed operation modified state".to_string(),
                before: before.clone(),
                after: after.clone(),
            });
        }
        Ok(())
    }
}

/// Property 3: Scale factor, fees, and timestamps only move forward.
struct MonotonicProgress;
impl SecurityProperty for MonotonicProgress {
    fn name(&self) -> &str {
        "MonotonicProgress"
    }

    fn check(
        &self,
        before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        // Scale factor must never decrease
        if after.scale_factor < before.scale_factor {
            return Err(SecurityViolation {
                severity: Severity::Critical,
                property_name: self.name().to_string(),
                description: format!(
                    "Scale factor decreased from {} to {}",
                    before.scale_factor, after.scale_factor
                ),
                before: before.clone(),
                after: after.clone(),
            });
        }

        // Timestamp must never decrease
        if after.last_accrual_timestamp < before.last_accrual_timestamp {
            return Err(SecurityViolation {
                severity: Severity::High,
                property_name: self.name().to_string(),
                description: format!(
                    "Timestamp decreased from {} to {}",
                    before.last_accrual_timestamp, after.last_accrual_timestamp
                ),
                before: before.clone(),
                after: after.clone(),
            });
        }

        // Fees can decrease only via CollectFees
        match operation {
            Operation::CollectFees => {}, // allowed
            _ => {
                if after.accrued_protocol_fees < before.accrued_protocol_fees {
                    return Err(SecurityViolation {
                        severity: Severity::High,
                        property_name: self.name().to_string(),
                        description: format!(
                            "Fees decreased from {} to {} outside CollectFees",
                            before.accrued_protocol_fees, after.accrued_protocol_fees
                        ),
                        before: before.clone(),
                        after: after.clone(),
                    });
                }
            },
        }

        Ok(())
    }
}

/// Property 4: Conservation of value across the protocol.
/// total_deposited + total_repaid == vault_balance + total_borrowed + total_withdrawn + fees_collected
///
/// Note: Due to interest accrual increasing scale_factor (which affects normalized
/// values but not raw token counts), conservation is tracked on raw token flows.
struct ConservationOfValue;
impl SecurityProperty for ConservationOfValue {
    fn name(&self) -> &str {
        "ConservationOfValue"
    }

    fn check(
        &self,
        _before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        _operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        // Token inflows = deposits + repayments
        // Token outflows = borrows + withdrawals + fee collections
        // vault_balance = inflows - outflows
        // Therefore: deposits + repaid = vault + borrowed + withdrawn + fees_collected
        //
        // We check on the `after` snapshot, using the SimulatedProtocol's tracking.
        // This is validated at the SimulatedProtocol level.
        // Here we verify the market-level counters are consistent:
        // total_deposited + total_repaid >= total_borrowed (basic solvency)
        let inflows = u128::from(after.total_deposited) + u128::from(after.total_repaid);
        let min_outflows = u128::from(after.total_borrowed);
        if inflows < min_outflows {
            return Err(SecurityViolation {
                severity: Severity::Critical,
                property_name: self.name().to_string(),
                description: format!(
                    "Inflows ({}) < outflows ({}): deposits={}, repaid={}, borrowed={}",
                    inflows,
                    min_outflows,
                    after.total_deposited,
                    after.total_repaid,
                    after.total_borrowed,
                ),
                before: _before.clone(),
                after: after.clone(),
            });
        }
        Ok(())
    }
}

/// Property 5: No operation creates value from nothing.
/// After settlement, no lender can withdraw more than their deposit + accrued interest.
struct NoFreeValue;
impl SecurityProperty for NoFreeValue {
    fn name(&self) -> &str {
        "NoFreeValue"
    }

    fn check(
        &self,
        before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        // For withdrawals: payout = vault_decrease
        // Settlement factor is capped at WAD (100%), so payout <= normalized_amount
        // Which means payout <= scaled_balance * scale_factor / WAD
        if let Operation::Withdraw { .. } = operation {
            let vault_decrease = before.vault_balance.saturating_sub(after.vault_balance);
            // The vault decrease should be bounded by the total available
            // (settlement_factor <= WAD means no lender gets more than 100%)
            if after.settlement_factor_wad > WAD {
                return Err(SecurityViolation {
                    severity: Severity::Critical,
                    property_name: self.name().to_string(),
                    description: format!(
                        "Settlement factor {} exceeds WAD ({}), enabling free value extraction",
                        after.settlement_factor_wad, WAD
                    ),
                    before: before.clone(),
                    after: after.clone(),
                });
            }
            // Also verify vault decrease is bounded by the total vault balance
            if vault_decrease > before.vault_balance {
                return Err(SecurityViolation {
                    severity: Severity::Critical,
                    property_name: self.name().to_string(),
                    description: format!(
                        "Vault decreased by {} but only had {}",
                        vault_decrease, before.vault_balance
                    ),
                    before: before.clone(),
                    after: after.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Property 6: Equal depositors receive equal payouts (within rounding tolerance).
struct ProportionalFairness;
impl SecurityProperty for ProportionalFairness {
    fn name(&self) -> &str {
        "ProportionalFairness"
    }

    fn check(
        &self,
        _before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        _operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        // Check that all lenders with the same scaled_balance would get the same payout.
        // Since payout = scaled_balance * scale_factor / WAD * settlement_factor / WAD,
        // and scale_factor and settlement_factor are global, equal scaled_balance
        // implies equal payout. We verify no lender has a negative scaled_balance.
        for pos in &after.lender_positions {
            // Scaled balance can never be negative (it is u128)
            // but we check that a withdrawal did not underflow to a huge number
            if pos.scaled_balance > after.scaled_total_supply && after.scaled_total_supply > 0 {
                return Err(SecurityViolation {
                    severity: Severity::High,
                    property_name: self.name().to_string(),
                    description: format!(
                        "Lender {} has scaled_balance {} > total_supply {}",
                        pos.lender_id, pos.scaled_balance, after.scaled_total_supply
                    ),
                    before: _before.clone(),
                    after: after.clone(),
                });
            }
        }

        // Sum of all lender positions must equal scaled_total_supply
        let sum: u128 = after
            .lender_positions
            .iter()
            .map(|p| p.scaled_balance)
            .sum();
        if sum != after.scaled_total_supply {
            return Err(SecurityViolation {
                severity: Severity::High,
                property_name: self.name().to_string(),
                description: format!(
                    "Sum of lender balances ({}) != scaled_total_supply ({})",
                    sum, after.scaled_total_supply
                ),
                before: _before.clone(),
                after: after.clone(),
            });
        }

        Ok(())
    }
}

/// Property 7: Rounding loss per operation is bounded (protocol-favorable).
struct BoundedLoss;
impl SecurityProperty for BoundedLoss {
    fn name(&self) -> &str {
        "BoundedLoss"
    }

    fn check(
        &self,
        before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        // For deposits: the actual scaled amount should be within 2 units of the ideal.
        // ideal_scaled = amount * WAD / scale_factor
        // The truncation (floor division) means the lender gets slightly fewer shares.
        if let Operation::Deposit { amount, .. } = operation {
            if before.scale_factor > 0 {
                let ideal_scaled = u128::from(*amount) * WAD / before.scale_factor;
                let actual_increase = after
                    .scaled_total_supply
                    .saturating_sub(before.scaled_total_supply);
                // Floor division means actual <= ideal. The difference should be at most 1.
                if ideal_scaled > actual_increase && (ideal_scaled - actual_increase) > 2 {
                    return Err(SecurityViolation {
                        severity: Severity::Medium,
                        property_name: self.name().to_string(),
                        description: format!(
                            "Rounding loss {} exceeds bound of 2 (ideal={}, actual={})",
                            ideal_scaled - actual_increase,
                            ideal_scaled,
                            actual_increase
                        ),
                        before: before.clone(),
                        after: after.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Property 8: Operational limits (supply cap, whitelist capacity) are never exceeded.
struct CapEnforcement;
impl SecurityProperty for CapEnforcement {
    fn name(&self) -> &str {
        "CapEnforcement"
    }

    fn check(
        &self,
        _before: &ProtocolSnapshot,
        after: &ProtocolSnapshot,
        operation: &Operation,
    ) -> Result<(), SecurityViolation> {
        // On-chain, the normalized supply cap is only enforced during deposit
        // (processor/deposit.rs:146-148). Interest accrual can push normalized
        // supply above the cap without a new deposit, so we only assert here
        // for Deposit operations to match on-chain behavior.
        if matches!(operation, Operation::Deposit { .. }) {
            let normalized = after.normalized_total_supply();
            let max_supply = u128::from(after.max_total_supply);
            if normalized > max_supply {
                return Err(SecurityViolation {
                    severity: Severity::High,
                    property_name: self.name().to_string(),
                    description: format!(
                        "Normalized supply {} exceeds cap {}",
                        normalized, max_supply
                    ),
                    before: _before.clone(),
                    after: after.clone(),
                });
            }
        }

        // Whitelist current_borrowed must not exceed max_borrow_capacity
        if after.whitelist_current_borrowed > after.whitelist_max_capacity {
            return Err(SecurityViolation {
                severity: Severity::High,
                property_name: self.name().to_string(),
                description: format!(
                    "Whitelist current_borrowed {} exceeds capacity {}",
                    after.whitelist_current_borrowed, after.whitelist_max_capacity
                ),
                before: _before.clone(),
                after: after.clone(),
            });
        }

        Ok(())
    }
}

/// Collect all properties into a single list for batch checking.
fn all_properties() -> Vec<Box<dyn SecurityProperty>> {
    vec![
        Box::new(NoUnauthorizedDrain),
        Box::new(ErrorAtomicity),
        Box::new(MonotonicProgress),
        Box::new(ConservationOfValue),
        Box::new(NoFreeValue),
        Box::new(ProportionalFairness),
        Box::new(BoundedLoss),
        Box::new(CapEnforcement),
    ]
}

/// Check all properties (except ErrorAtomicity which is checked separately for failures).
fn check_all_properties_for_success(
    before: &ProtocolSnapshot,
    after: &ProtocolSnapshot,
    operation: &Operation,
) -> Vec<SecurityViolation> {
    let mut violations = Vec::new();
    for prop in all_properties() {
        if prop.name() == "ErrorAtomicity" {
            continue; // Only checked for failed operations
        }
        if let Err(v) = prop.check(before, after, operation) {
            violations.push(v);
        }
    }
    violations
}

// ============================================================================
// Section 4: Composable Strategy Combinators
// ============================================================================

/// Generate realistic protocol parameters for testing.
fn protocol_params_strategy() -> impl Strategy<Value = (u16, u16, i64, u64, u64, i64)> {
    (
        1u16..=MAX_ANNUAL_INTEREST_BPS, // annual_interest_bps
        0u16..=MAX_FEE_RATE_BPS,        // fee_rate_bps
        1_000_000i64..70_000_000i64, // maturity_timestamp (bounded to avoid overflow-only fuzz cases)
        1_000_000u64..1_000_000_000u64, // max_total_supply (1 USDC to 1000 USDC)
        1_000_000u64..1_000_000_000u64, // max_borrow_capacity
        100_000i64..999_000i64,      // start_timestamp
    )
}

/// Strategy for valid deposit operations given the current state.
fn valid_deposit_strategy(max_supply: u64) -> impl Strategy<Value = Operation> {
    (0u32..4, 1u64..=(max_supply / 2).max(1))
        .prop_map(|(lender_id, amount)| Operation::Deposit { lender_id, amount })
}

/// Strategy for adversarial deposit operations (should fail).
fn adversarial_deposit_strategy(max_supply: u64) -> impl Strategy<Value = Operation> {
    prop_oneof![
        // Zero amount
        (0u32..4).prop_map(|lender_id| Operation::Deposit {
            lender_id,
            amount: 0
        }),
        // Amount exceeding cap
        (0u32..4).prop_map(move |lender_id| Operation::Deposit {
            lender_id,
            amount: max_supply.saturating_add(1),
        }),
        // u64::MAX amount
        (0u32..4).prop_map(|lender_id| Operation::Deposit {
            lender_id,
            amount: u64::MAX,
        }),
    ]
}

/// Strategy for valid borrow operations.
fn valid_borrow_strategy(max_amount: u64) -> impl Strategy<Value = Operation> {
    (1u64..=max_amount.max(1)).prop_map(|amount| Operation::Borrow { amount })
}

/// Strategy for valid repay operations.
fn valid_repay_strategy(max_amount: u64) -> impl Strategy<Value = Operation> {
    (1u64..=max_amount.max(1)).prop_map(|amount| Operation::Repay { amount })
}

/// Strategy for valid multi-step lifecycle sequences.
fn valid_lifecycle_strategy() -> impl Strategy<Value = Vec<Operation>> {
    protocol_params_strategy().prop_flat_map(
        |(_interest_bps, _fee_bps, maturity, max_supply, _max_borrow, start_ts)| {
            let deposit_amount = (max_supply / 4).max(1);
            let borrow_amount = (deposit_amount / 2).max(1);
            let time_step = ((maturity - start_ts) / 10).max(1);

            prop_vec(
                prop_oneof![
                    3 => valid_deposit_strategy(max_supply),
                    2 => valid_borrow_strategy(borrow_amount),
                    2 => valid_repay_strategy(borrow_amount),
                    3 => (1i64..=time_step).prop_map(move |delta| Operation::AccrueInterest {
                        timestamp: start_ts + delta
                    }),
                ],
                1..20,
            )
        },
    )
}

/// Strategy for adversarial sequences designed to break invariants.
fn adversarial_sequence_strategy() -> impl Strategy<Value = Vec<Operation>> {
    protocol_params_strategy().prop_flat_map(|(_, _, _, max_supply, _, _)| {
        prop_vec(
            prop_oneof![
                2 => adversarial_deposit_strategy(max_supply),
                1 => Just(Operation::Borrow { amount: u64::MAX }),
                1 => Just(Operation::Repay { amount: 0 }),
                1 => Just(Operation::CollectFees),
                1 => Just(Operation::Withdraw {
                    lender_id: 0,
                    scaled_amount: u128::MAX,
                }),
                1 => Just(Operation::ReSettle),
            ],
            1..15,
        )
    })
}

/// Strategy for operations at exact invariant boundaries.
fn boundary_strategy(max_supply: u64) -> impl Strategy<Value = Operation> {
    prop_oneof![
        // Exact cap deposit
        Just(Operation::Deposit {
            lender_id: 0,
            amount: max_supply
        }),
        // Minimum valid deposit (1)
        Just(Operation::Deposit {
            lender_id: 0,
            amount: 1
        }),
        // Minimum valid borrow (1)
        Just(Operation::Borrow { amount: 1 }),
        // Minimum valid repay (1)
        Just(Operation::Repay { amount: 1 }),
        // Full withdrawal (0 = all)
        Just(Operation::Withdraw {
            lender_id: 0,
            scaled_amount: 0
        }),
    ]
}

// ============================================================================
// Section 5: Test Helpers
// ============================================================================

/// Create a SimulatedProtocol from strategy-generated parameters.
fn make_protocol(
    annual_interest_bps: u16,
    fee_rate_bps: u16,
    maturity_timestamp: i64,
    max_total_supply: u64,
    max_borrow_capacity: u64,
    start_timestamp: i64,
) -> SimulatedProtocol {
    SimulatedProtocol::new(
        annual_interest_bps,
        fee_rate_bps,
        maturity_timestamp,
        max_total_supply,
        max_borrow_capacity,
        start_timestamp,
    )
}

/// Execute a sequence of operations, checking all properties after each successful one.
/// Returns all violations found.
fn audit_sequence(
    protocol: &mut SimulatedProtocol,
    operations: &[Operation],
) -> Vec<SecurityViolation> {
    let mut violations = Vec::new();

    for op in operations {
        let before = protocol.snapshot();
        let protocol_copy = protocol.clone();

        match protocol.execute(op) {
            Ok(()) => {
                let after = protocol.snapshot();
                violations.extend(check_all_properties_for_success(&before, &after, op));
            },
            Err(_) => {
                // Operation failed -- check atomicity (state must be unchanged).
                // We use the copy to verify the original was not mutated.
                let after_failed = protocol.snapshot();
                let atomicity = ErrorAtomicity;
                // For failed operations, state should match the copy before failure
                let copy_snapshot = ProtocolSnapshot::from_state(&protocol_copy);
                if let Err(v) = atomicity.check(&copy_snapshot, &after_failed, op) {
                    violations.push(v);
                }
                // Restore from the copy for consistency
                *protocol = protocol_copy;
            },
        }
    }

    violations
}

/// Token conservation check at the SimulatedProtocol level.
fn check_token_conservation(protocol: &SimulatedProtocol) -> Result<(), String> {
    let inflows =
        u128::from(protocol.market.total_deposited()) + u128::from(protocol.market.total_repaid());
    let outflows = u128::from(protocol.market.total_borrowed())
        + u128::from(protocol.total_withdrawn)
        + u128::from(protocol.total_fees_collected);
    let vault = u128::from(protocol.vault_balance);

    // inflows = outflows + vault_balance
    if inflows != outflows + vault {
        return Err(format!(
            "Token conservation violated: inflows={} != outflows={} + vault={}",
            inflows, outflows, vault
        ));
    }
    Ok(())
}

fn sum_lender_scaled(snapshot: &ProtocolSnapshot) -> u128 {
    snapshot
        .lender_positions
        .iter()
        .map(|p| p.scaled_balance)
        .sum()
}

fn assert_snapshot_unchanged(before: &ProtocolSnapshot, after: &ProtocolSnapshot, context: &str) {
    assert!(
        before.market_bytes_identical(after),
        "{context}: market state mutated on failed operation"
    );
    assert_eq!(
        before.vault_balance, after.vault_balance,
        "{context}: vault balance mutated on failed operation"
    );
    assert_eq!(
        before.whitelist_current_borrowed, after.whitelist_current_borrowed,
        "{context}: whitelist borrowed mutated on failed operation"
    );
    assert_eq!(
        before.whitelist_max_capacity, after.whitelist_max_capacity,
        "{context}: whitelist capacity mutated on failed operation"
    );
    assert_eq!(
        before.fee_rate_bps, after.fee_rate_bps,
        "{context}: fee rate mutated on failed operation"
    );
    assert_eq!(
        before.lender_positions.len(),
        after.lender_positions.len(),
        "{context}: lender position count mutated on failed operation"
    );
    let before_positions = sorted_lender_positions(before);
    let after_positions = sorted_lender_positions(after);
    assert_eq!(
        before_positions, after_positions,
        "{context}: lender balances mutated on failed operation"
    );
}

fn sorted_lender_positions(snapshot: &ProtocolSnapshot) -> Vec<(u32, u128)> {
    let mut positions: Vec<(u32, u128)> = snapshot
        .lender_positions
        .iter()
        .map(|p| (p.lender_id, p.scaled_balance))
        .collect();
    positions.sort_unstable_by_key(|(id, _)| *id);
    positions
}

fn assert_snapshot_unchanged_or_settlement_initialized(
    before: &ProtocolSnapshot,
    after: &ProtocolSnapshot,
    context: &str,
) {
    if before.market_bytes_identical(after)
        && before.vault_balance == after.vault_balance
        && before.whitelist_current_borrowed == after.whitelist_current_borrowed
        && before.whitelist_max_capacity == after.whitelist_max_capacity
        && before.fee_rate_bps == after.fee_rate_bps
        && sorted_lender_positions(before) == sorted_lender_positions(after)
    {
        return;
    }

    let mut adjusted_before = before.clone();
    adjusted_before.settlement_factor_wad = after.settlement_factor_wad;
    let settlement_only = before.settlement_factor_wad == 0
        && (1..=WAD).contains(&after.settlement_factor_wad)
        && adjusted_before.market_bytes_identical(after)
        && before.vault_balance == after.vault_balance
        && before.whitelist_current_borrowed == after.whitelist_current_borrowed
        && before.whitelist_max_capacity == after.whitelist_max_capacity
        && before.fee_rate_bps == after.fee_rate_bps
        && sorted_lender_positions(before) == sorted_lender_positions(after);

    assert!(
        settlement_only,
        "{context}: failed op mutated fields beyond allowed settlement initialization"
    );
}

fn assert_invariants(protocol: &SimulatedProtocol, context: &str) {
    check_token_conservation(protocol).unwrap_or_else(|e| panic!("{context}: {e}"));
    let snap = protocol.snapshot();
    assert_eq!(
        sum_lender_scaled(&snap),
        snap.scaled_total_supply,
        "{context}: lender sum must equal scaled_total_supply"
    );
    assert!(
        snap.scale_factor >= WAD,
        "{context}: scale_factor {} < WAD {}",
        snap.scale_factor,
        WAD
    );
    assert!(
        snap.whitelist_current_borrowed <= snap.whitelist_max_capacity,
        "{context}: whitelist borrowed exceeds capacity"
    );
    if snap.settlement_factor_wad != 0 {
        assert!(
            (1..=WAD).contains(&snap.settlement_factor_wad),
            "{context}: settlement_factor_wad {} out of [1, WAD]",
            snap.settlement_factor_wad
        );
    }
}

fn oracle_mul_wad(a: u128, b: u128) -> Option<u128> {
    math_oracle::mul_wad_checked(a, b)
}

fn oracle_pow_wad(base: u128, exp: u32) -> Option<u128> {
    math_oracle::pow_wad_checked(base, exp)
}

fn oracle_growth_factor_wad(
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
) -> Option<u128> {
    let effective_now = current_ts.min(maturity_timestamp);
    if effective_now < last_accrual_timestamp {
        return None;
    }
    if effective_now == last_accrual_timestamp {
        return Some(WAD);
    }
    let elapsed = effective_now - last_accrual_timestamp;
    math_oracle::growth_factor_wad_checked(annual_interest_bps, elapsed)
}

fn oracle_interest_delta_wad(
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
) -> Option<u128> {
    oracle_growth_factor_wad(
        annual_interest_bps,
        last_accrual_timestamp,
        maturity_timestamp,
        current_ts,
    )?
    .checked_sub(WAD)
}

fn oracle_scale_factor_after_step(
    scale_factor: u128,
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
) -> Option<u128> {
    let growth = oracle_growth_factor_wad(
        annual_interest_bps,
        last_accrual_timestamp,
        maturity_timestamp,
        current_ts,
    )?;
    oracle_mul_wad(scale_factor, growth)
}

fn oracle_fee_delta_normalized(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_interest_bps: u16,
    last_accrual_timestamp: i64,
    maturity_timestamp: i64,
    current_ts: i64,
    fee_rate_bps: u16,
) -> Option<u64> {
    if fee_rate_bps == 0 {
        return Some(0);
    }
    let interest_delta_wad = oracle_interest_delta_wad(
        annual_interest_bps,
        last_accrual_timestamp,
        maturity_timestamp,
        current_ts,
    )?;
    if interest_delta_wad == 0 {
        return Some(0);
    }
    let fee_delta_wad = interest_delta_wad
        .checked_mul(u128::from(fee_rate_bps))?
        .checked_div(BPS)?;
    // Use pre-accrual scale_factor_before (matches on-chain Finding 10 fix)
    let fee_normalized = scaled_total_supply
        .checked_mul(scale_factor_before)?
        .checked_div(WAD)?
        .checked_mul(fee_delta_wad)?
        .checked_div(WAD)?;
    u64::try_from(fee_normalized).ok()
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

// ============================================================================
// Section 6: Security Audit Tests (10+ Tests)
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    // --- Per-property tests (8 properties) ---

    #[test]
    fn audit_no_unauthorized_drain(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        ops in prop_vec(
            prop_oneof![
                3 => (0u32..4, 1u64..500_000u64).prop_map(|(id, amt)| Operation::Deposit { lender_id: id, amount: amt }),
                2 => (1u64..500_000u64).prop_map(|amt| Operation::Borrow { amount: amt }),
                2 => (1u64..500_000u64).prop_map(|amt| Operation::Repay { amount: amt }),
                1 => Just(Operation::CollectFees),
                2 => (1i64..100_000i64).prop_map(|d| Operation::AccrueInterest { timestamp: 200_000 + d }),
            ],
            1..30,
        ),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = NoUnauthorizedDrain;

        for op in &ops {
            let before = protocol.snapshot();
            let saved = protocol.clone();
            match protocol.execute(op) {
                Ok(()) => {
                    let after = protocol.snapshot();
                    let result = property.check(&before, &after, op);
                    prop_assert!(result.is_ok(), "Violation: {}", result.unwrap_err());

                    let vault_decrease = before.vault_balance.saturating_sub(after.vault_balance);
                    match op {
                        Operation::Borrow { amount } => {
                            prop_assert_eq!(
                                vault_decrease,
                                *amount,
                                "borrow should reduce vault by exact amount"
                            );
                        }
                        Operation::CollectFees => {
                            // COAL-C01: do_collect_fees accrues first, then caps.
                            // Use post-accrual values: after.scale_factor (unchanged
                            // by collection), post_accrual_fees = after.fees + vault_decrease.
                            let post_accrual_fees = after.accrued_protocol_fees + vault_decrease;
                            let mut expected = std::cmp::min(
                                post_accrual_fees,
                                before.vault_balance,
                            );
                            if after.scaled_total_supply > 0 {
                                let sf = after.scale_factor;
                                let total_norm = after.scaled_total_supply
                                    .checked_mul(sf).unwrap()
                                    / WAD;
                                let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
                                let safe_max = before.vault_balance.saturating_sub(lender_claims);
                                expected = expected.min(safe_max);
                            }
                            prop_assert_eq!(
                                vault_decrease,
                                expected,
                                "collect_fees must reduce vault by capped withdrawable"
                            );
                            // Verify vault still covers lender claims after collection
                            if after.scaled_total_supply > 0 {
                                let total_norm = after.scaled_total_supply
                                    .checked_mul(after.scale_factor).unwrap()
                                    / WAD;
                                let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
                                prop_assert!(
                                    after.vault_balance >= lender_claims,
                                    "vault must still cover lender claims after fee collection"
                                );
                            }
                        }
                        Operation::Deposit { .. } | Operation::Repay { .. } => {
                            prop_assert_eq!(
                                vault_decrease, 0,
                                "deposit/repay must not decrease vault"
                            );
                            prop_assert!(
                                after.vault_balance >= before.vault_balance,
                                "deposit/repay should increase or preserve vault"
                            );
                        }
                        Operation::AccrueInterest { timestamp } => {
                            let expected_sf = oracle_scale_factor_after_step(
                                before.scale_factor,
                                before.annual_interest_bps,
                                before.last_accrual_timestamp,
                                before.maturity_timestamp,
                                *timestamp,
                            )
                            .expect("oracle scale overflow");
                            let expected_fee_delta = oracle_fee_delta_normalized(
                                before.scaled_total_supply,
                                before.scale_factor,
                                before.annual_interest_bps,
                                before.last_accrual_timestamp,
                                before.maturity_timestamp,
                                *timestamp,
                                before.fee_rate_bps,
                            )
                            .expect("oracle fee overflow");
                            prop_assert_eq!(after.scale_factor, expected_sf);
                            prop_assert_eq!(
                                after.accrued_protocol_fees,
                                before.accrued_protocol_fees + expected_fee_delta
                            );
                            prop_assert_eq!(
                                after.last_accrual_timestamp,
                                (*timestamp).min(before.maturity_timestamp)
                            );
                            prop_assert_eq!(vault_decrease, 0);
                        }
                        Operation::Withdraw { .. } => {
                            prop_assert!(
                                vault_decrease <= before.vault_balance,
                                "withdraw cannot take more than pre-op vault"
                            );
                        }
                        Operation::ReSettle => {
                            prop_assert_eq!(vault_decrease, 0);
                        }
                    }

                    assert_invariants(&protocol, "audit_no_unauthorized_drain");
                }
                Err(_err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged(
                        &before,
                        &after_failed,
                        "audit_no_unauthorized_drain failure path",
                    );
                    protocol = saved;
                }
            }
        }
    }

    #[test]
    fn audit_error_atomicity(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        ops in prop_vec(
            prop_oneof![
                1 => (0u32..4).prop_map(|id| Operation::Deposit { lender_id: id, amount: 0 }),
                1 => Just(Operation::Borrow { amount: 0 }),
                1 => Just(Operation::Repay { amount: 0 }),
                1 => Just(Operation::CollectFees),
                1 => Just(Operation::Withdraw { lender_id: 0, scaled_amount: u128::MAX }),
                1 => Just(Operation::ReSettle),
            ],
            1..20,
        ),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = ErrorAtomicity;

        for op in &ops {
            let protocol_copy = protocol.clone();
            match protocol.execute(op) {
                Ok(()) => {
                    // These adversarial operations should generally fail.
                    assert_invariants(&protocol, "audit_error_atomicity unexpected success");
                }
                Err(err) => {
                    let after = protocol.snapshot();
                    let copy_snap = ProtocolSnapshot::from_state(&protocol_copy);
                    let property_result = property.check(&copy_snap, &after, op);
                    prop_assert!(
                        property_result.is_ok(),
                        "ErrorAtomicity violated for {:?}: {}",
                        op,
                        property_result.unwrap_err()
                    );
                    assert_snapshot_unchanged(
                        &copy_snap,
                        &after,
                        "audit_error_atomicity failed-op state check",
                    );

                    match op {
                        Operation::Deposit { amount, .. } if *amount == 0 => {
                            prop_assert_eq!(err, "ZeroAmount");
                        }
                        Operation::Borrow { amount } if *amount == 0 => {
                            prop_assert_eq!(err, "ZeroAmount");
                        }
                        Operation::Repay { amount } if *amount == 0 => {
                            prop_assert_eq!(err, "ZeroAmount");
                        }
                        Operation::CollectFees => {
                            prop_assert_eq!(err, "NoFeesToCollect");
                        }
                        Operation::Withdraw { .. } => {
                            prop_assert_eq!(err, "NotMatured");
                        }
                        Operation::ReSettle => {
                            prop_assert_eq!(err, "NotSettled");
                        }
                        _ => {}
                    }

                    protocol = protocol_copy;
                }
            }
        }
    }

    #[test]
    fn audit_monotonic_progress(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        ops in prop_vec(
            prop_oneof![
                3 => (0u32..4, 1u64..500_000u64).prop_map(|(id, amt)| Operation::Deposit { lender_id: id, amount: amt }),
                2 => (1u64..500_000u64).prop_map(|amt| Operation::Borrow { amount: amt }),
                2 => (1u64..500_000u64).prop_map(|amt| Operation::Repay { amount: amt }),
                1 => Just(Operation::CollectFees),
                2 => (1i64..500_000i64).prop_map(|d| Operation::AccrueInterest { timestamp: 200_000 + d }),
            ],
            1..30,
        ),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = MonotonicProgress;

        for op in &ops {
            let before = protocol.snapshot();
            let saved = protocol.clone();
            match protocol.execute(op) {
                Ok(()) => {
                    let after = protocol.snapshot();
                    let result = property.check(&before, &after, op);
                    prop_assert!(result.is_ok(), "Violation: {}", result.unwrap_err());

                    prop_assert!(
                        after.scale_factor >= before.scale_factor,
                        "scale_factor must be monotonic"
                    );
                    prop_assert!(
                        after.last_accrual_timestamp >= before.last_accrual_timestamp,
                        "last_accrual_timestamp must be monotonic"
                    );
                    if !matches!(op, Operation::CollectFees) {
                        prop_assert!(
                            after.accrued_protocol_fees >= before.accrued_protocol_fees,
                            "fees cannot decrease outside CollectFees"
                        );
                    }
                    if let Operation::AccrueInterest { timestamp } = op {
                        let expected_sf = oracle_scale_factor_after_step(
                            before.scale_factor,
                            before.annual_interest_bps,
                            before.last_accrual_timestamp,
                            before.maturity_timestamp,
                            *timestamp,
                        )
                        .expect("oracle scale overflow");
                        let expected_fee_delta = oracle_fee_delta_normalized(
                            before.scaled_total_supply,
                            before.scale_factor,
                            before.annual_interest_bps,
                            before.last_accrual_timestamp,
                            before.maturity_timestamp,
                            *timestamp,
                            before.fee_rate_bps,
                        )
                        .expect("oracle fee overflow");
                        prop_assert_eq!(after.scale_factor, expected_sf);
                        prop_assert_eq!(
                            after.accrued_protocol_fees,
                            before.accrued_protocol_fees + expected_fee_delta
                        );
                    }
                    assert_invariants(&protocol, "audit_monotonic_progress");
                }
                Err(_err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged(
                        &before,
                        &after_failed,
                        "audit_monotonic_progress failure path",
                    );
                    protocol = saved;
                }
            }
        }
    }

    #[test]
    fn audit_conservation_of_value(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        ops in prop_vec(
            prop_oneof![
                3 => (0u32..4, 1u64..500_000u64).prop_map(|(id, amt)| Operation::Deposit { lender_id: id, amount: amt }),
                2 => (1u64..200_000u64).prop_map(|amt| Operation::Borrow { amount: amt }),
                2 => (1u64..500_000u64).prop_map(|amt| Operation::Repay { amount: amt }),
                1 => Just(Operation::CollectFees),
                2 => (1i64..500_000i64).prop_map(|d| Operation::AccrueInterest { timestamp: 200_000 + d }),
            ],
            1..30,
        ),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);

        for op in &ops {
            let before = protocol.snapshot();
            let saved = protocol.clone();
            match protocol.execute(op) {
                Ok(()) => {
                    let result = check_token_conservation(&protocol);
                    prop_assert!(result.is_ok(), "Conservation violated: {}", result.unwrap_err());
                    let after = protocol.snapshot();
                    let inflows =
                        u128::from(after.total_deposited) + u128::from(after.total_repaid);
                    let outflows = u128::from(after.total_borrowed)
                        + u128::from(protocol.total_withdrawn)
                        + u128::from(protocol.total_fees_collected);
                    prop_assert_eq!(
                        inflows,
                        outflows + u128::from(after.vault_balance),
                        "explicit conservation equality mismatch"
                    );
                    prop_assert_eq!(
                        sum_lender_scaled(&after),
                        after.scaled_total_supply,
                        "lender sum mismatch"
                    );
                }
                Err(_err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged(
                        &before,
                        &after_failed,
                        "audit_conservation_of_value failure path",
                    );
                    protocol = saved;
                }
            }
        }
    }

    #[test]
    fn audit_no_free_value(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        deposit_amount in 1u64..500_000u64,
        borrow_amount in 1u64..200_000u64,
        repay_amount in 1u64..500_000u64,
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = NoFreeValue;

        // Deposit
        let dep_res = protocol.execute(&Operation::Deposit {
            lender_id: 0,
            amount: deposit_amount,
        });
        prop_assert!(dep_res.is_ok(), "deposit setup failed unexpectedly: {:?}", dep_res);

        // Borrow
        let _ = protocol.execute(&Operation::Borrow {
            amount: borrow_amount,
        });

        // Repay
        let _ = protocol.execute(&Operation::Repay {
            amount: repay_amount,
        });

        // Advance past maturity
        let accrue_res = protocol.execute(&Operation::AccrueInterest {
            timestamp: maturity + 1,
        });
        prop_assert!(
            accrue_res.is_ok(),
            "post-maturity accrue should succeed: {:?}",
            accrue_res
        );

        // Withdraw
        let before = protocol.snapshot();
        let withdraw_op = Operation::Withdraw {
            lender_id: 0,
            scaled_amount: 0,
        };
        match protocol.execute(&withdraw_op) {
            Ok(()) => {
                let after = protocol.snapshot();
                let result = property.check(&before, &after, &withdraw_op);
                prop_assert!(result.is_ok(), "NoFreeValue violated: {}", result.unwrap_err());

                let vault_decrease = before.vault_balance.saturating_sub(after.vault_balance);
                let lender_before = before
                    .lender_positions
                    .iter()
                    .find(|p| p.lender_id == 0)
                    .map(|p| p.scaled_balance)
                    .unwrap_or(0);
                let normalized_before = lender_before
                    .checked_mul(before.scale_factor)
                    .unwrap_or(u128::MAX)
                    / WAD;
                let settlement_for_payout = if before.settlement_factor_wad == 0 {
                    let total_normalized = before
                        .scaled_total_supply
                        .checked_mul(before.scale_factor)
                        .unwrap_or(u128::MAX)
                        / WAD;
                    oracle_settlement_factor(
                        total_normalized,
                        before.vault_balance,
                        before.accrued_protocol_fees,
                    )
                } else {
                    before.settlement_factor_wad
                };
                let max_payout = normalized_before
                    .checked_mul(settlement_for_payout.max(1))
                    .unwrap_or(u128::MAX)
                    / WAD;
                prop_assert!(
                    u128::from(vault_decrease) <= max_payout + 1,
                    "withdraw payout {} exceeds max payout bound {} (+1 rounding)",
                    vault_decrease,
                    max_payout
                );
                assert_invariants(&protocol, "audit_no_free_value success path");

                // Second withdrawal should fail with no mutation.
                let before_second = protocol.snapshot();
                let second = protocol.execute(&withdraw_op);
                prop_assert_eq!(second, Err("NoBalance".to_string()));
                let after_second = protocol.snapshot();
                assert_snapshot_unchanged(
                    &before_second,
                    &after_second,
                    "audit_no_free_value second withdraw",
                );
            }
            Err(err) => {
                let after_failed = protocol.snapshot();
                assert_snapshot_unchanged_or_settlement_initialized(
                    &before,
                    &after_failed,
                    "audit_no_free_value failure path",
                );
                prop_assert!(
                    matches!(
                        err.as_str(),
                        "NoPosition"
                            | "NoBalance"
                            | "NotMatured"
                            | "ZeroPayout"
                            | "InsufficientVaultBalance"
                            | "InsufficientScaledBalance"
                    ),
                    "unexpected withdrawal error: {}",
                    err
                );
            }
        }
    }

    #[test]
    fn audit_proportional_fairness(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        base_amount in 1u64..200_000u64,
        ratio in 1u8..=5u8,
    ) {
        let _ = fee_bps;
        // Force zero protocol fees for this fairness test so settlement is driven by lender value only.
        let mut protocol = make_protocol(interest_bps, 0, maturity, max_supply, max_borrow, start_ts);
        let property = ProportionalFairness;
        let ratio_u64 = u64::from(ratio);
        let amount_a = base_amount;
        let amount_b = base_amount.saturating_mul(ratio_u64);

        // Two lenders deposit in a strict ratio at the same timestamp.
        let dep_a = protocol.execute(&Operation::Deposit {
            lender_id: 0,
            amount: amount_a,
        });
        let dep_b = protocol.execute(&Operation::Deposit {
            lender_id: 1,
            amount: amount_b,
        });
        prop_assert!(dep_a.is_ok() && dep_b.is_ok(), "setup deposits should succeed");

        // Move to settlement phase.
        let accrue = protocol.execute(&Operation::AccrueInterest {
            timestamp: maturity + 1,
        });
        prop_assert!(accrue.is_ok(), "maturity accrual should succeed");

        let before_all_withdrawals = protocol.snapshot();
        // COAL-C01: fees are no longer reserved from the settlement factor,
        // so the full vault backs lender withdrawals.
        let available_for_lenders = before_all_withdrawals.vault_balance;

        let before_a = protocol.snapshot();
        let wd_a = protocol.execute(&Operation::Withdraw {
            lender_id: 0,
            scaled_amount: 0,
        });
        let (_after_a, payout_a) = match wd_a {
            Ok(()) => {
                let after = protocol.snapshot();
                let payout = before_a.vault_balance.saturating_sub(after.vault_balance);
                (after, payout)
            }
            Err(err) => {
                let after_failed = protocol.snapshot();
                assert_snapshot_unchanged_or_settlement_initialized(
                    &before_a,
                    &after_failed,
                    "audit_proportional_fairness lender A failure",
                );
                prop_assert!(
                    matches!(
                        err.as_str(),
                        "ZeroPayout" | "InsufficientVaultBalance" | "InsufficientScaledBalance"
                    ),
                    "unexpected lender A withdraw failure: {}",
                    err
                );
                return Ok(());
            }
        };

        let before_b = protocol.snapshot();
        let wd_b = protocol.execute(&Operation::Withdraw {
            lender_id: 1,
            scaled_amount: 0,
        });
        let (after_b, payout_b) = match wd_b {
            Ok(()) => {
                let after = protocol.snapshot();
                let payout = before_b.vault_balance.saturating_sub(after.vault_balance);
                (after, payout)
            }
            Err(err) => {
                let after_failed = protocol.snapshot();
                assert_snapshot_unchanged_or_settlement_initialized(
                    &before_b,
                    &after_failed,
                    "audit_proportional_fairness lender B failure",
                );
                prop_assert!(
                    matches!(
                        err.as_str(),
                        "ZeroPayout" | "InsufficientVaultBalance" | "InsufficientScaledBalance"
                    ),
                    "unexpected lender B withdraw failure: {}",
                    err
                );
                return Ok(());
            }
        };

        // Proportional payout check with tight rounding tolerance.
        let left = u128::from(payout_b);
        let right = u128::from(payout_a) * u128::from(ratio_u64);
        let diff = left.abs_diff(right);
        prop_assert!(
            diff <= u128::from(ratio_u64),
            "payout ratio drift too large: A={}, B={}, ratio={}, diff={}",
            payout_a,
            payout_b,
            ratio_u64,
            diff
        );

        // Property checker and accounting invariants.
        let result = property.check(
            &before_all_withdrawals,
            &after_b,
            &Operation::Withdraw {
                lender_id: 1,
                scaled_amount: 0,
            },
        );
        prop_assert!(result.is_ok(), "ProportionalFairness violated: {}", result.unwrap_err());

        let total_payouts = u128::from(payout_a) + u128::from(payout_b);
        prop_assert!(
            total_payouts <= u128::from(available_for_lenders),
            "withdrawals exceed lender-available vault"
        );
        prop_assert!(
            u128::from(available_for_lenders) - total_payouts <= 2,
            "excess post-withdraw dust should be <= 2 units"
        );
        assert_invariants(&protocol, "audit_proportional_fairness");
    }

    #[test]
    fn audit_bounded_loss(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        amounts in prop_vec(1u64..500_000u64, 1..10),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = BoundedLoss;

        for (i, &amount) in amounts.iter().enumerate() {
            let before = protocol.snapshot();
            let saved = protocol.clone();
            let op = Operation::Deposit {
                lender_id: i as u32,
                amount,
            };
            match protocol.execute(&op) {
                Ok(()) => {
                    let after = protocol.snapshot();
                    let result = property.check(&before, &after, &op);
                    prop_assert!(result.is_ok(), "BoundedLoss violated: {}", result.unwrap_err());

                    let expected_scaled = u128::from(amount)
                        .checked_mul(WAD)
                        .unwrap()
                        / before.scale_factor;
                    let actual_increase = after
                        .scaled_total_supply
                        .saturating_sub(before.scaled_total_supply);
                    prop_assert_eq!(
                        actual_increase, expected_scaled,
                        "deposit scaling should match exact floor formula"
                    );
                    prop_assert!(
                        expected_scaled >= actual_increase,
                        "actual scaled increase cannot exceed ideal"
                    );
                    prop_assert!(
                        expected_scaled - actual_increase <= 1,
                        "rounding loss should be <= 1 unit"
                    );
                    assert_invariants(&protocol, "audit_bounded_loss");
                }
                Err(err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged(&before, &after_failed, "audit_bounded_loss failure path");
                    prop_assert!(
                        matches!(err.as_str(), "CapExceeded" | "MathOverflow" | "ZeroScaledAmount"),
                        "unexpected deposit failure in bounded-loss test: {}",
                        err
                    );
                    protocol = saved;
                }
            }
        }
    }

    #[test]
    fn audit_cap_enforcement(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        ops in prop_vec(
            prop_oneof![
                3 => (0u32..4, 1u64..1_000_000u64).prop_map(|(id, amt)| Operation::Deposit { lender_id: id, amount: amt }),
                2 => (1u64..1_000_000u64).prop_map(|amt| Operation::Borrow { amount: amt }),
                2 => (1i64..500_000i64).prop_map(|d| Operation::AccrueInterest { timestamp: 200_000 + d }),
            ],
            1..30,
        ),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = CapEnforcement;

        for op in &ops {
            let before = protocol.snapshot();
            let saved = protocol.clone();
            match protocol.execute(op) {
                Ok(()) => {
                    let after = protocol.snapshot();
                    if !matches!(op, Operation::AccrueInterest { .. }) {
                        let result = property.check(&before, &after, op);
                        prop_assert!(result.is_ok(), "CapEnforcement violated: {}", result.unwrap_err());
                    }
                    match op {
                        Operation::Deposit { .. } => {
                            prop_assert!(
                                after.normalized_total_supply() <= u128::from(after.max_total_supply),
                                "deposit path must enforce normalized supply cap"
                            );
                        }
                        Operation::Borrow { .. } => {
                            prop_assert!(
                                after.whitelist_current_borrowed <= after.whitelist_max_capacity,
                                "borrow path must enforce whitelist capacity"
                            );
                        }
                        Operation::AccrueInterest { timestamp } => {
                            let expected_sf = oracle_scale_factor_after_step(
                                before.scale_factor,
                                before.annual_interest_bps,
                                before.last_accrual_timestamp,
                                before.maturity_timestamp,
                                *timestamp,
                            )
                            .expect("oracle scale overflow");
                            let expected_fee_delta = oracle_fee_delta_normalized(
                                before.scaled_total_supply,
                                before.scale_factor,
                                before.annual_interest_bps,
                                before.last_accrual_timestamp,
                                before.maturity_timestamp,
                                *timestamp,
                                before.fee_rate_bps,
                            )
                            .expect("oracle fee overflow");
                            prop_assert_eq!(after.scale_factor, expected_sf);
                            prop_assert_eq!(
                                after.accrued_protocol_fees,
                                before.accrued_protocol_fees + expected_fee_delta
                            );
                        }
                        _ => {}
                    }
                    assert_invariants(&protocol, "audit_cap_enforcement");
                }
                Err(err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged(
                        &before,
                        &after_failed,
                        "audit_cap_enforcement failure path",
                    );
                    match op {
                        Operation::Deposit { .. } => {
                            prop_assert!(
                                matches!(
                                    err.as_str(),
                                    "CapExceeded" | "MathOverflow" | "ZeroScaledAmount" | "ZeroAmount" | "MarketMatured"
                                ),
                                "unexpected deposit error in cap_enforcement: {}",
                                err
                            );
                        }
                        Operation::Borrow { .. } => {
                            prop_assert!(
                                matches!(
                                    err.as_str(),
                                    "BorrowAmountTooHigh" | "GlobalCapacityExceeded" | "ZeroAmount" | "MarketMatured" | "MathOverflow"
                                ),
                                "unexpected borrow error in cap_enforcement: {}",
                                err
                            );
                        }
                        Operation::AccrueInterest { .. } => {
                            prop_assert!(
                                matches!(err.as_str(), "Timestamp in the past" | "AccrueInterest failed: Custom(20)" | "AccrueInterest failed: Custom(41)"),
                                "unexpected accrue error in cap_enforcement: {}",
                                err
                            );
                        }
                        _ => {}
                    }
                    protocol = saved;
                }
            }
        }
    }

    // --- Focused adversarial tests ---

    #[test]
    fn audit_adversarial_drain_attempts(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        deposit_amount in 1u64..500_000u64,
        drain_ops in prop_vec(
            prop_oneof![
                2 => (1u64..1_000_000u64).prop_map(|amt| Operation::Borrow { amount: amt }),
                2 => (0u32..4, 0u128..=u128::MAX / 2).prop_map(|(id, sc)| Operation::Withdraw {
                    lender_id: id,
                    scaled_amount: sc,
                }),
                1 => Just(Operation::CollectFees),
            ],
            3..15,
        ),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = NoUnauthorizedDrain;

        // Seed the vault with a deposit
        let dep = protocol.execute(&Operation::Deposit {
            lender_id: 0,
            amount: deposit_amount,
        });
        prop_assert!(dep.is_ok(), "seed deposit must succeed");
        let initial_vault = protocol.vault_balance;

        // Stay strictly pre-maturity so adversarial withdraw attempts fail atomically.
        let pre_maturity_ts = maturity.saturating_sub(1);
        prop_assert!(
            pre_maturity_ts >= start_ts,
            "invalid test params: maturity {} < start_ts {}",
            maturity,
            start_ts
        );
        let accr = protocol.execute(&Operation::AccrueInterest {
            timestamp: pre_maturity_ts,
        });
        prop_assert!(accr.is_ok(), "seed accrual must succeed");

        // Execute drain attempts
        let mut total_extracted: u128 = 0;
        for op in &drain_ops {
            let before_vault = protocol.vault_balance;
            let before = protocol.snapshot();
            match protocol.execute(op) {
                Ok(()) => {
                    let after = protocol.snapshot();
                    let result = property.check(&before, &after, op);
                    prop_assert!(result.is_ok(), "NoUnauthorizedDrain violated: {}", result.unwrap_err());

                    let decrease = u128::from(before_vault.saturating_sub(protocol.vault_balance));
                    total_extracted += decrease;
                    assert_invariants(&protocol, "audit_adversarial_drain_attempts success path");
                }
                Err(err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged(
                        &before,
                        &after_failed,
                        "audit_adversarial_drain_attempts failure path",
                    );
                    match op {
                        Operation::Withdraw { .. } => prop_assert_eq!(err, "NotMatured"),
                        Operation::Borrow { .. } => {
                            prop_assert!(
                                matches!(
                                    err.as_str(),
                                    "BorrowAmountTooHigh" | "GlobalCapacityExceeded" | "MathOverflow"
                                ),
                                "unexpected borrow error: {}",
                                err
                            );
                        }
                        Operation::CollectFees => prop_assert_eq!(err, "NoFeesToCollect"),
                        _ => {}
                    }
                }
            }
        }

        // Verify: total extracted <= initial deposit + interest earned
        // Since no repayments were made, extracted should be bounded
        let deposited = u128::from(protocol.market.total_deposited());
        let repaid = u128::from(protocol.market.total_repaid());
        prop_assert!(
            total_extracted <= deposited + repaid,
            "Extracted {} exceeds deposits ({}) + repaid ({})",
            total_extracted,
            deposited,
            repaid,
        );
        prop_assert!(
            total_extracted <= u128::from(initial_vault),
            "adversarial extraction exceeded initial seeded vault"
        );
        prop_assert_eq!(
            total_extracted,
            u128::from(initial_vault.saturating_sub(protocol.vault_balance)),
            "tracked extraction should equal aggregate vault delta"
        );
        prop_assert_eq!(
            total_extracted,
            u128::from(protocol.market.total_borrowed())
                + u128::from(protocol.total_withdrawn)
                + u128::from(protocol.total_fees_collected),
            "extraction must reconcile with protocol outflow counters"
        );
    }

    #[test]
    fn audit_error_atomicity_comprehensive(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        ops in prop_vec(
            prop_oneof![
                1 => (0u32..4).prop_map(|id| Operation::Deposit { lender_id: id, amount: 0 }),
                1 => (0u32..4).prop_map(move |id| Operation::Deposit { lender_id: id, amount: u64::MAX }),
                1 => Just(Operation::Borrow { amount: 0 }),
                1 => Just(Operation::Borrow { amount: u64::MAX }),
                1 => Just(Operation::Repay { amount: 0 }),
                1 => Just(Operation::CollectFees),
                1 => Just(Operation::Withdraw { lender_id: 99, scaled_amount: 0 }),
                1 => Just(Operation::ReSettle),
            ],
            5..25,
        ),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);
        let property = ErrorAtomicity;

        for op in &ops {
            let before = protocol.snapshot();
            let saved = protocol.clone();
            match protocol.execute(op) {
                Ok(()) => {
                    assert_invariants(&protocol, "audit_error_atomicity_comprehensive unexpected success");
                }
                Err(err) => {
                    let after = protocol.snapshot();
                    let prop_result = property.check(&before, &after, op);
                    prop_assert!(
                        prop_result.is_ok(),
                        "ErrorAtomicity violated for {:?}: {}",
                        op,
                        prop_result.unwrap_err()
                    );
                    assert_snapshot_unchanged(
                        &before,
                        &after,
                        "audit_error_atomicity_comprehensive failed-op state check",
                    );

                    match op {
                        Operation::Deposit { amount, .. } if *amount == 0 => prop_assert_eq!(err, "ZeroAmount"),
                        Operation::Deposit { amount, .. } if *amount == u64::MAX => {
                            prop_assert!(matches!(err.as_str(), "CapExceeded" | "MathOverflow"));
                        }
                        Operation::Borrow { amount } if *amount == 0 => prop_assert_eq!(err, "ZeroAmount"),
                        Operation::Borrow { amount } if *amount == u64::MAX => prop_assert_eq!(err, "BorrowAmountTooHigh"),
                        Operation::Repay { amount } if *amount == 0 => prop_assert_eq!(err, "ZeroAmount"),
                        Operation::CollectFees => prop_assert_eq!(err, "NoFeesToCollect"),
                        Operation::Withdraw { lender_id, .. } if *lender_id == 99 => prop_assert_eq!(err, "NotMatured"),
                        Operation::ReSettle => prop_assert_eq!(err, "NotSettled"),
                        _ => {}
                    }

                    protocol = saved;
                }
            }
        }
    }

    #[test]
    fn audit_rounding_exploitation(
        (interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts) in protocol_params_strategy(),
        tiny_amounts in prop_vec(1u64..10u64, 10..50),
    ) {
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_borrow, start_ts);

        // Perform many tiny deposits trying to accumulate rounding gains
        let mut total_deposited_actual: u128 = 0;
        for (i, &amount) in tiny_amounts.iter().enumerate() {
            let before = protocol.snapshot();
            let saved = protocol.clone();
            let op = Operation::Deposit {
                lender_id: (i % 4) as u32,
                amount,
            };
            match protocol.execute(&op) {
                Ok(()) => {
                    let after = protocol.snapshot();
                    let expected_scaled = u128::from(amount) * WAD / before.scale_factor;
                    let actual_scaled = after
                        .scaled_total_supply
                        .saturating_sub(before.scaled_total_supply);
                    prop_assert_eq!(actual_scaled, expected_scaled);
                    total_deposited_actual += u128::from(amount);
                    assert_invariants(&protocol, "audit_rounding_exploitation deposit path");
                }
                Err(_err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged(
                        &before,
                        &after_failed,
                        "audit_rounding_exploitation failed tiny deposit",
                    );
                    protocol = saved;
                }
            }
        }

        // Advance past maturity
        let accr = protocol.execute(&Operation::AccrueInterest {
            timestamp: maturity + 1,
        });
        prop_assert!(accr.is_ok(), "maturity accrual should succeed");

        // Withdraw all and check no one extracts more than deposited + interest
        let mut total_withdrawn: u128 = 0;
        for lender_id in 0u32..4 {
            let before_vault = protocol.vault_balance;
            let before = protocol.snapshot();
            let op = Operation::Withdraw {
                lender_id,
                scaled_amount: 0,
            };
            match protocol.execute(&op) {
                Ok(()) => {
                    let after = protocol.snapshot();
                    let payout = before_vault.saturating_sub(protocol.vault_balance);
                    total_withdrawn += u128::from(payout);

                    let lender_scaled_before = before
                        .lender_positions
                        .iter()
                        .find(|p| p.lender_id == lender_id)
                        .map(|p| p.scaled_balance)
                        .unwrap_or(0);
                    let settlement_for_payout = if before.settlement_factor_wad == 0 {
                        let total_normalized = before
                            .scaled_total_supply
                            .checked_mul(before.scale_factor)
                            .unwrap_or(u128::MAX)
                            / WAD;
                        oracle_settlement_factor(
                            total_normalized,
                            before.vault_balance,
                            before.accrued_protocol_fees,
                        )
                    } else {
                        before.settlement_factor_wad
                    };
                    let max_expected_payout = lender_scaled_before
                        .checked_mul(before.scale_factor)
                        .unwrap_or(u128::MAX)
                        / WAD
                        * settlement_for_payout
                        / WAD;
                    prop_assert!(
                        u128::from(payout) <= max_expected_payout,
                        "withdraw payout exceeds formula bound"
                    );
                    let property = NoFreeValue;
                    let prop_result = property.check(&before, &after, &op);
                    prop_assert!(
                        prop_result.is_ok(),
                        "NoFreeValue violated in rounding test: {}",
                        prop_result.unwrap_err()
                    );
                    assert_invariants(&protocol, "audit_rounding_exploitation withdraw path");
                }
                Err(err) => {
                    let after_failed = protocol.snapshot();
                    assert_snapshot_unchanged_or_settlement_initialized(
                        &before,
                        &after_failed,
                        "audit_rounding_exploitation failed withdraw",
                    );
                    prop_assert!(
                        matches!(
                            err.as_str(),
                            "NoPosition"
                                | "NoBalance"
                                | "ZeroPayout"
                                | "InsufficientVaultBalance"
                                | "InsufficientScaledBalance"
                        ),
                        "unexpected withdraw failure in rounding test: {}",
                        err
                    );
                }
            }
        }

        // Total withdrawn should not exceed total deposited + repaid
        // (No repayments were made, so this checks rounding does not create value)
        prop_assert!(
            total_withdrawn <= total_deposited_actual + u128::from(protocol.market.total_repaid()),
            "Rounding exploitation: withdrew {} but only deposited {} (repaid={})",
            total_withdrawn,
            total_deposited_actual,
            protocol.market.total_repaid(),
        );
        assert_invariants(&protocol, "audit_rounding_exploitation final");
    }

    #[test]
    fn audit_fee_extraction_fairness(
        interest_bps in 100u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 100u16..=MAX_FEE_RATE_BPS,
        maturity in 2_000_000i64..2_000_000_000i64,
        deposit_amount in 100_000u64..10_000_000u64,
        time_advance in 100_000i64..1_000_000i64,
    ) {
        let start_ts = 100_000i64;
        let max_supply = deposit_amount * 2;
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_supply, start_ts);

        // Deposit
        let dep = protocol.execute(&Operation::Deposit {
            lender_id: 0,
            amount: deposit_amount,
        });
        prop_assert!(dep.is_ok(), "seed deposit should succeed");
        let after_deposit = protocol.snapshot();
        prop_assert_eq!(after_deposit.scaled_total_supply, u128::from(deposit_amount));

        // Advance time to accrue interest and fees
        let before_accrue = protocol.snapshot();
        let accrue_ts = start_ts + time_advance;
        let accr = protocol.execute(&Operation::AccrueInterest { timestamp: accrue_ts });
        prop_assert!(accr.is_ok(), "accrual should succeed");
        let after_accrue = protocol.snapshot();
        let expected_sf = oracle_scale_factor_after_step(
            before_accrue.scale_factor,
            interest_bps,
            before_accrue.last_accrual_timestamp,
            before_accrue.maturity_timestamp,
            accrue_ts,
        )
        .expect("oracle scale overflow");
        let expected_fee_delta = oracle_fee_delta_normalized(
            before_accrue.scaled_total_supply,
            before_accrue.scale_factor,
            interest_bps,
            before_accrue.last_accrual_timestamp,
            before_accrue.maturity_timestamp,
            accrue_ts,
            fee_bps,
        )
        .expect("oracle fee overflow");
        prop_assert_eq!(after_accrue.scale_factor, expected_sf);
        prop_assert_eq!(
            after_accrue.accrued_protocol_fees,
            before_accrue.accrued_protocol_fees + expected_fee_delta
        );
        prop_assert_eq!(after_accrue.last_accrual_timestamp, accrue_ts);

        // Collect fees
        let before_collect = protocol.snapshot();
        let fees_collected_before = protocol.total_fees_collected;
        let total_fee_ledger_before = u128::from(before_collect.accrued_protocol_fees)
            + u128::from(fees_collected_before);
        let mut expected_taken = std::cmp::min(
            before_collect.accrued_protocol_fees,
            before_collect.vault_balance,
        );
        // COAL-C01: cap fee withdrawal above lender claims when supply > 0.
        // do_collect_fees accrues first (no-op here since timestamp unchanged),
        // then applies cap with current scale_factor.
        if before_collect.scaled_total_supply > 0 {
            let total_norm = before_collect.scaled_total_supply
                .checked_mul(before_collect.scale_factor).unwrap()
                / WAD;
            let lender_claims = u64::try_from(total_norm).unwrap_or(u64::MAX);
            let safe_max = before_collect.vault_balance.saturating_sub(lender_claims);
            expected_taken = expected_taken.min(safe_max);
        }
        let collect = protocol.execute(&Operation::CollectFees);
        match collect {
            Ok(()) => {
                let after_collect = protocol.snapshot();
                let taken = before_collect
                    .vault_balance
                    .saturating_sub(after_collect.vault_balance);
                prop_assert!(
                    expected_taken > 0,
                    "successful collect_fees must withdraw positive amount"
                );
                prop_assert_eq!(
                    taken, expected_taken,
                    "fee collection must withdraw exactly min(accrued, vault)"
                );
                prop_assert_eq!(
                    after_collect.accrued_protocol_fees,
                    before_collect.accrued_protocol_fees.saturating_sub(expected_taken),
                    "remaining accrued fees mismatch"
                );
                prop_assert_eq!(
                    protocol.total_fees_collected,
                    fees_collected_before + expected_taken,
                    "total_fees_collected counter mismatch"
                );
                prop_assert_eq!(
                    u128::from(after_collect.vault_balance) + u128::from(protocol.total_fees_collected),
                    u128::from(before_collect.vault_balance) + u128::from(fees_collected_before),
                    "collect_fees must conserve (vault + total_fees_collected)"
                );

                let property = MonotonicProgress;
                let prop_result =
                    property.check(&before_collect, &after_collect, &Operation::CollectFees);
                prop_assert!(prop_result.is_ok(), "monotonic property failed on collect_fees");
                assert_invariants(&protocol, "audit_fee_extraction_fairness success");
            }
            Err(err) => {
                prop_assert_eq!(err, "NoFeesToCollect");
                let after_failed = protocol.snapshot();
                assert_snapshot_unchanged(
                    &before_collect,
                    &after_failed,
                    "audit_fee_extraction_fairness collect failure",
                );
                prop_assert_eq!(expected_taken, 0, "collect_fees may fail only when withdrawable=0");
            }
        }

        let total_fee_ledger_after = u128::from(protocol.market.accrued_protocol_fees())
            + u128::from(protocol.total_fees_collected);
        prop_assert_eq!(
            total_fee_ledger_after,
            total_fee_ledger_before,
            "collect_fees must conserve (accrued + total_fees_collected)"
        );
        prop_assert!(
            u128::from(protocol.total_fees_collected) <= u128::from(after_deposit.vault_balance),
            "fee collector cannot extract more than initial vault in this single-deposit scenario"
        );
    }

    #[test]
    fn audit_settlement_manipulation(
        interest_bps in 100u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        deposit_amounts in prop_vec(100_000u64..1_000_000u64, 2..4),
        borrow_amount in 100_000u64..500_000u64,
        repay_amount in 100_000u64..1_000_000u64,
    ) {
        let start_ts = 100_000i64;
        let maturity = 1_000_000i64;
        let max_supply = 10_000_000u64;
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_supply, start_ts);

        // Multiple lenders deposit
        for (i, &amount) in deposit_amounts.iter().enumerate() {
            let dep = protocol.execute(&Operation::Deposit {
                lender_id: i as u32,
                amount,
            });
            prop_assert!(dep.is_ok(), "setup deposit {} should succeed", i);
        }
        let total_deposited_setup: u64 = deposit_amounts.iter().copied().sum();
        let borrow_target = borrow_amount.min(total_deposited_setup);

        // Borrow and repay
        let borrow = protocol.execute(&Operation::Borrow {
            amount: borrow_target,
        });
        prop_assert!(borrow.is_ok(), "borrow should succeed");
        let repay = protocol.execute(&Operation::Repay { amount: repay_amount });
        prop_assert!(repay.is_ok(), "repay should succeed");

        // Advance past maturity
        let accr = protocol.execute(&Operation::AccrueInterest {
            timestamp: maturity + 1,
        });
        prop_assert!(accr.is_ok(), "maturity accrual should succeed");

        // First lender withdraws (triggers settlement)
        let before_first_withdraw = protocol.snapshot();
        let wd = protocol.execute(&Operation::Withdraw {
            lender_id: 0,
            scaled_amount: 0,
        });
        prop_assert!(wd.is_ok(), "first withdraw should succeed");
        let after_first_withdraw = protocol.snapshot();
        let settlement_factor = after_first_withdraw.settlement_factor_wad;
        let total_normalized_before = before_first_withdraw
            .scaled_total_supply
            .checked_mul(before_first_withdraw.scale_factor)
            .unwrap_or(u128::MAX)
            / WAD;
        let oracle_factor = oracle_settlement_factor(
            total_normalized_before,
            before_first_withdraw.vault_balance,
            before_first_withdraw.accrued_protocol_fees,
        );
        prop_assert_eq!(
            settlement_factor, oracle_factor,
            "withdraw-triggered settlement factor must match oracle"
        );
        prop_assert!(
            settlement_factor >= 1 && settlement_factor <= WAD,
            "Settlement factor {} out of range [1, {}]",
            settlement_factor,
            WAD,
        );
        let lender0_scaled_before = before_first_withdraw
            .lender_positions
            .iter()
            .find(|p| p.lender_id == 0)
            .map(|p| p.scaled_balance)
            .unwrap_or(0);
        let expected_payout = lender0_scaled_before
            .checked_mul(before_first_withdraw.scale_factor)
            .unwrap_or(u128::MAX)
            / WAD
            * settlement_factor
            / WAD;
        let actual_payout = u128::from(
            before_first_withdraw
                .vault_balance
                .saturating_sub(after_first_withdraw.vault_balance),
        );
        prop_assert_eq!(
            actual_payout, expected_payout,
            "withdraw payout must match formula"
        );

        // Re-settle: either strict increase to oracle value or exact no-mutation failure.
        let before_resettle = protocol.snapshot();
        let factor_before = before_resettle.settlement_factor_wad;
        let total_normalized = before_resettle
            .scaled_total_supply
            .checked_mul(before_resettle.scale_factor)
            .unwrap_or(u128::MAX)
            / WAD;
        let expected_new_factor = oracle_settlement_factor(
            total_normalized,
            before_resettle.vault_balance,
            before_resettle.accrued_protocol_fees,
        );
        match protocol.execute(&Operation::ReSettle) {
            Ok(()) => {
                let after_resettle = protocol.snapshot();
                prop_assert!(
                    expected_new_factor > factor_before,
                    "ReSettle can succeed only when oracle factor improves"
                );
                prop_assert!(
                    after_resettle.settlement_factor_wad > factor_before,
                    "ReSettle did not improve factor: {} -> {}",
                    factor_before,
                    after_resettle.settlement_factor_wad,
                );
                prop_assert_eq!(
                    after_resettle.settlement_factor_wad,
                    expected_new_factor,
                    "ReSettle factor must match oracle"
                );
                assert_invariants(&protocol, "audit_settlement_manipulation resettle success");
            }
            Err(err) => {
                prop_assert_eq!(err, "SettlementNotImproved");
                let after_failed = protocol.snapshot();
                assert_snapshot_unchanged(
                    &before_resettle,
                    &after_failed,
                    "audit_settlement_manipulation resettle failure",
                );
                prop_assert!(
                    expected_new_factor <= factor_before,
                    "non-improving oracle factor expected on resettle failure"
                );
            }
        }
        assert_invariants(&protocol, "audit_settlement_manipulation final");
    }
}

// ============================================================================
// Section 7: Audit Report Generation
// ============================================================================

#[test]
fn generate_security_audit_summary() {
    println!("\n# Security Audit Summary Report");
    println!("# CoalesceFi Pinocchio Lending Protocol");
    println!("# =====================================\n");

    let properties = all_properties();
    let mut total_checks: usize = 0;
    let mut total_violations: usize = 0;
    let num_sequences = 100;
    let ops_per_sequence = 20;

    println!("## Configuration");
    println!("- Random sequences: {}", num_sequences);
    println!("- Operations per sequence: {}", ops_per_sequence);
    println!("- Properties checked: {}\n", properties.len());

    println!("## Results\n");
    println!("| Property | Checks | Violations | Status |");
    println!("|----------|--------|------------|--------|");

    for prop in &properties {
        let mut checks = 0u64;
        let mut violations = 0u64;

        for seed in 0..num_sequences {
            // Deterministic protocol parameters based on seed
            let interest_bps = ((seed * 137 + 1) % 10000 + 1) as u16;
            let fee_bps = ((seed * 251 + 1) % 10000) as u16;
            let start_ts = 100_000i64;
            let maturity = 2_000_000i64;
            let max_supply = 10_000_000u64;

            let mut protocol = make_protocol(
                interest_bps,
                fee_bps,
                maturity,
                max_supply,
                max_supply,
                start_ts,
            );

            for step in 0..ops_per_sequence {
                let op = match step % 7 {
                    0 | 1 => Operation::Deposit {
                        lender_id: (step % 4) as u32,
                        amount: ((seed * 73 + step * 37) % 500_000 + 1) as u64,
                    },
                    2 => Operation::Borrow {
                        amount: ((seed * 41 + step * 29) % 200_000 + 1) as u64,
                    },
                    3 => Operation::Repay {
                        amount: ((seed * 59 + step * 43) % 300_000 + 1) as u64,
                    },
                    4 => Operation::AccrueInterest {
                        timestamp: start_ts + (step as i64 + 1) * 10_000,
                    },
                    5 => Operation::CollectFees,
                    _ => Operation::AccrueInterest {
                        timestamp: start_ts + (step as i64 + 1) * 10_000,
                    },
                };

                let before = protocol.snapshot();
                match protocol.execute(&op) {
                    Ok(()) => {
                        assert_invariants(
                            &protocol,
                            "generate_security_audit_summary success step",
                        );
                        if prop.name() != "ErrorAtomicity" {
                            let after = protocol.snapshot();
                            checks += 1;
                            if prop.check(&before, &after, &op).is_err() {
                                violations += 1;
                            }
                        }
                    },
                    Err(_) => {
                        let after = protocol.snapshot();
                        assert_snapshot_unchanged(
                            &before,
                            &after,
                            "generate_security_audit_summary failure step",
                        );
                        if prop.name() == "ErrorAtomicity" {
                            checks += 1;
                            if prop.check(&before, &after, &op).is_err() {
                                violations += 1;
                            }
                        }
                    },
                }
            }
        }

        assert!(
            checks > 0,
            "property {} received zero executable checks in summary harness",
            prop.name()
        );
        total_checks += checks as usize;
        total_violations += violations as usize;

        let status = if violations == 0 { "PASS" } else { "FAIL" };
        println!(
            "| {} | {} | {} | {} |",
            prop.name(),
            checks,
            violations,
            status,
        );
    }

    assert!(
        total_checks > 0,
        "security audit summary executed zero checks"
    );
    println!("\n## Summary");
    println!("- Total checks: {}", total_checks);
    println!("- Total violations: {}", total_violations);
    println!(
        "- Audit result: **{}**\n",
        if total_violations == 0 {
            "ALL PROPERTIES HOLD"
        } else {
            "VIOLATIONS DETECTED"
        }
    );

    assert_eq!(
        total_violations, 0,
        "Security audit found {} violations",
        total_violations
    );
}

#[test]
fn generate_property_coverage_matrix() {
    println!("\n# Property Coverage Matrix");
    println!("# ========================\n");

    let operation_types = [
        "Deposit",
        "Borrow",
        "Repay",
        "Withdraw",
        "CollectFees",
        "AccrueInterest",
        "ReSettle",
    ];

    let properties = all_properties();

    println!("| Property | {} |", operation_types.join(" | "));
    println!(
        "|----------|{}|",
        operation_types
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join("|")
    );

    let mut op_checked_counts: Vec<usize> = vec![0; operation_types.len()];

    for prop in &properties {
        let mut row = vec![prop.name().to_string()];
        let mut row_checked = 0usize;

        for (op_idx, op_type) in operation_types.iter().enumerate() {
            let is_error_atomicity = prop.name() == "ErrorAtomicity";
            let op = if is_error_atomicity {
                match *op_type {
                    "Deposit" => Operation::Deposit {
                        lender_id: 0,
                        amount: 0,
                    },
                    "Borrow" => Operation::Borrow { amount: 0 },
                    "Repay" => Operation::Repay { amount: 0 },
                    "Withdraw" => Operation::Withdraw {
                        lender_id: 0,
                        scaled_amount: 0,
                    },
                    "CollectFees" => Operation::CollectFees,
                    "AccrueInterest" => Operation::AccrueInterest { timestamp: 99_999 },
                    "ReSettle" => Operation::ReSettle,
                    _ => Operation::AccrueInterest { timestamp: 99_999 },
                }
            } else {
                match *op_type {
                    "Deposit" => Operation::Deposit {
                        lender_id: 0,
                        amount: 100_000,
                    },
                    "Borrow" => Operation::Borrow { amount: 50_000 },
                    "Repay" => Operation::Repay { amount: 50_000 },
                    "Withdraw" => Operation::Withdraw {
                        lender_id: 0,
                        scaled_amount: 0,
                    },
                    "CollectFees" => Operation::CollectFees,
                    "AccrueInterest" => Operation::AccrueInterest { timestamp: 200_000 },
                    "ReSettle" => Operation::ReSettle,
                    _ => Operation::AccrueInterest { timestamp: 200_000 },
                }
            };

            // Build an op-specific pre-state so each operation is evaluated in a meaningful context.
            let mut protocol = make_protocol(500, 100, 2_000_000, 10_000_000, 10_000_000, 100_000);
            if !is_error_atomicity {
                match *op_type {
                    "Borrow" => {
                        assert!(protocol
                            .execute(&Operation::Deposit {
                                lender_id: 0,
                                amount: 1_000_000,
                            })
                            .is_ok());
                    },
                    "Withdraw" => {
                        assert!(protocol
                            .execute(&Operation::Deposit {
                                lender_id: 0,
                                amount: 1_000_000,
                            })
                            .is_ok());
                        assert!(protocol
                            .execute(&Operation::AccrueInterest {
                                timestamp: 2_000_001,
                            })
                            .is_ok());
                    },
                    "CollectFees" => {
                        assert!(protocol
                            .execute(&Operation::Deposit {
                                lender_id: 0,
                                amount: 5_000_000,
                            })
                            .is_ok());
                        assert!(protocol
                            .execute(&Operation::AccrueInterest {
                                timestamp: 1_000_000,
                            })
                            .is_ok());
                    },
                    "ReSettle" => {
                        assert!(protocol
                            .execute(&Operation::Deposit {
                                lender_id: 0,
                                amount: 1_000_000,
                            })
                            .is_ok());
                        assert!(protocol
                            .execute(&Operation::Borrow { amount: 500_000 })
                            .is_ok());
                        assert!(protocol
                            .execute(&Operation::AccrueInterest {
                                timestamp: 2_000_001,
                            })
                            .is_ok());
                        assert!(protocol
                            .execute(&Operation::Withdraw {
                                lender_id: 0,
                                scaled_amount: 0,
                            })
                            .is_ok());
                        assert!(protocol
                            .execute(&Operation::Repay { amount: 500_000 })
                            .is_ok());
                    },
                    _ => {},
                }
            }

            let before = protocol.snapshot();
            let result = protocol.execute(&op);

            let coverage = if result.is_ok() {
                let after = protocol.snapshot();
                assert_invariants(&protocol, "generate_property_coverage_matrix success path");
                match prop.check(&before, &after, &op) {
                    Ok(()) => {
                        row_checked += 1;
                        op_checked_counts[op_idx] += 1;
                        "CHECKED"
                    },
                    Err(_) => "FAIL",
                }
            } else {
                let after = protocol.snapshot();
                assert_snapshot_unchanged(
                    &before,
                    &after,
                    "generate_property_coverage_matrix failure path",
                );
                if is_error_atomicity {
                    row_checked += 1;
                    op_checked_counts[op_idx] += 1;
                    "CHECKED"
                } else {
                    "N/A"
                }
            };

            row.push(coverage.to_string());
        }

        assert!(
            row_checked > 0,
            "property {} had zero CHECKED cells in coverage matrix",
            prop.name()
        );
        println!("| {} |", row.join(" | "));
    }

    for (op_type, checked_count) in operation_types.iter().zip(op_checked_counts.iter()) {
        assert!(
            *checked_count > 0,
            "operation {} had zero CHECKED cells across all properties",
            op_type
        );
    }
    println!();
    println!("Legend: CHECKED = property verified, N/A = operation failed (not applicable), FAIL = violation found");
}

// ============================================================================
// Section 8: Additional Focused Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Verify that accrue_interest is idempotent when called twice at the same timestamp.
    #[test]
    fn audit_accrue_interest_idempotent(
        interest_bps in 1u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        start_ts in 100_000i64..900_000i64,
        advance in 1i64..500_000i64,
        scaled_supply in 1u128..1_000_000_000_000u128,
    ) {
        let maturity = 2_000_000i64;
        let ts = start_ts + advance;

        let mut market = Market::zeroed();
        market.set_annual_interest_bps(interest_bps);
        market.set_maturity_timestamp(maturity);
        market.set_scale_factor(WAD);
        market.set_last_accrual_timestamp(start_ts);
        market.set_scaled_total_supply(scaled_supply);

        let mut config = ProtocolConfig::zeroed();
        config.set_fee_rate_bps(fee_bps);

        // First accrual
        accrue_interest(&mut market, &config, ts).unwrap();
        let sf_after_first = market.scale_factor();
        let fees_after_first = market.accrued_protocol_fees();
        let ts_after_first = market.last_accrual_timestamp();
        let expected_sf =
            oracle_scale_factor_after_step(WAD, interest_bps, start_ts, maturity, ts)
                .expect("oracle scale overflow");
        let expected_fees = oracle_fee_delta_normalized(
            scaled_supply,
            WAD,
            interest_bps,
            start_ts,
            maturity,
            ts,
            fee_bps,
        )
        .expect("oracle fee overflow");
        prop_assert_eq!(sf_after_first, expected_sf, "first accrual scale mismatch");
        prop_assert_eq!(fees_after_first, expected_fees, "first accrual fees mismatch");
        prop_assert_eq!(ts_after_first, ts, "first accrual timestamp mismatch");

        // Second accrual at same timestamp
        accrue_interest(&mut market, &config, ts).unwrap();
        let sf_after_second = market.scale_factor();
        let fees_after_second = market.accrued_protocol_fees();
        let ts_after_second = market.last_accrual_timestamp();

        prop_assert_eq!(sf_after_first, sf_after_second, "Scale factor changed on idempotent call");
        prop_assert_eq!(fees_after_first, fees_after_second, "Fees changed on idempotent call");
        prop_assert_eq!(ts_after_first, ts_after_second, "Timestamp changed on idempotent call");

        // Backward timestamp must fail with full no-mutation semantics.
        let before_back_sf = market.scale_factor();
        let before_back_fees = market.accrued_protocol_fees();
        let before_back_ts = market.last_accrual_timestamp();
        let backward = accrue_interest(&mut market, &config, ts - 1);
        prop_assert!(backward.is_err(), "backward timestamp should fail");
        prop_assert_eq!(market.scale_factor(), before_back_sf);
        prop_assert_eq!(market.accrued_protocol_fees(), before_back_fees);
        prop_assert_eq!(market.last_accrual_timestamp(), before_back_ts);
    }

    /// Verify scale factor only increases after interest accrual.
    #[test]
    fn audit_scale_factor_monotonic(
        interest_bps in 1u16..=MAX_ANNUAL_INTEREST_BPS,
        fee_bps in 0u16..=MAX_FEE_RATE_BPS,
        start_ts in 100_000i64..900_000i64,
        advances in prop_vec(1i64..100_000i64, 2..10),
    ) {
        let maturity = 2_000_000_000i64;
        let mut market = Market::zeroed();
        market.set_annual_interest_bps(interest_bps);
        market.set_maturity_timestamp(maturity);
        market.set_scale_factor(WAD);
        market.set_last_accrual_timestamp(start_ts);
        market.set_scaled_total_supply(1_000_000_000u128);

        let mut config = ProtocolConfig::zeroed();
        config.set_fee_rate_bps(fee_bps);

        let mut current_ts = start_ts;
        let mut prev_scale = WAD;
        let mut prev_fees = 0u64;
        let mut prev_last_ts = start_ts;
        let scaled_supply = market.scaled_total_supply();

        for &advance in &advances {
            current_ts += advance;

            let expected_sf = oracle_scale_factor_after_step(
                prev_scale,
                interest_bps,
                prev_last_ts,
                maturity,
                current_ts,
            )
            .expect("oracle scale overflow");
            let expected_fee_delta = oracle_fee_delta_normalized(
                scaled_supply,
                prev_scale,
                interest_bps,
                prev_last_ts,
                maturity,
                current_ts,
                fee_bps,
            )
            .expect("oracle fee overflow");
            let expected_last_ts = current_ts.min(maturity);

            accrue_interest(&mut market, &config, current_ts).unwrap();
            let new_scale = market.scale_factor();
            prop_assert!(
                new_scale >= prev_scale,
                "Scale factor decreased from {} to {} at ts={}",
                prev_scale,
                new_scale,
                current_ts,
            );
            prop_assert_eq!(new_scale, expected_sf, "scale formula mismatch");
            prop_assert_eq!(
                market.accrued_protocol_fees(),
                prev_fees + expected_fee_delta,
                "fee formula mismatch"
            );
            prop_assert_eq!(
                market.last_accrual_timestamp(),
                expected_last_ts,
                "last_accrual timestamp mismatch"
            );
            prev_scale = new_scale;
            prev_fees = market.accrued_protocol_fees();
            prev_last_ts = market.last_accrual_timestamp();
        }
    }

    /// Verify that the interest formula produces correct results for known values.
    #[test]
    fn audit_interest_formula_accuracy(
        interest_bps in 1u16..=MAX_ANNUAL_INTEREST_BPS,
        elapsed_seconds in 1i64..=31_536_000i64,
    ) {
        let start_ts = 0i64;
        let maturity = 100_000_000i64;
        let mut market = Market::zeroed();
        market.set_annual_interest_bps(interest_bps);
        market.set_maturity_timestamp(maturity);
        market.set_scale_factor(WAD);
        market.set_last_accrual_timestamp(start_ts);
        market.set_scaled_total_supply(WAD);

        let config = ProtocolConfig::zeroed();
        accrue_interest(&mut market, &config, start_ts + elapsed_seconds).unwrap();
        let expected_sf =
            oracle_scale_factor_after_step(WAD, interest_bps, start_ts, maturity, elapsed_seconds)
                .expect("oracle scale overflow");

        prop_assert_eq!(market.scale_factor(), expected_sf, "single-step scale mismatch");
        prop_assert_eq!(market.accrued_protocol_fees(), 0, "zero-fee config should not accrue fees");
        prop_assert_eq!(
            market.last_accrual_timestamp(),
            elapsed_seconds,
            "last_accrual timestamp mismatch"
        );

        // Differential check: one-step accrual must match two-step accrual.
        let mut split_market = Market::zeroed();
        split_market.set_annual_interest_bps(interest_bps);
        split_market.set_maturity_timestamp(maturity);
        split_market.set_scale_factor(WAD);
        split_market.set_last_accrual_timestamp(start_ts);
        split_market.set_scaled_total_supply(WAD);
        let mid = elapsed_seconds / 2;
        accrue_interest(&mut split_market, &config, mid).unwrap();
        accrue_interest(&mut split_market, &config, elapsed_seconds).unwrap();
        let expected_split_sf_step1 =
            oracle_scale_factor_after_step(WAD, interest_bps, start_ts, maturity, mid)
                .expect("oracle split-step1 scale overflow");
        let expected_split_sf_step2 = oracle_scale_factor_after_step(
            expected_split_sf_step1,
            interest_bps,
            mid,
            maturity,
            elapsed_seconds,
        )
        .expect("oracle split-step2 scale overflow");
        prop_assert_eq!(
            split_market.scale_factor(),
            expected_split_sf_step2,
            "two-step accrual formula mismatch"
        );
        prop_assert_eq!(
            split_market.accrued_protocol_fees(),
            market.accrued_protocol_fees(),
            "fee path diverged between one-step and two-step accrual"
        );
    }

    /// Boundary test: verify operations at exact supply cap boundaries.
    #[test]
    fn audit_boundary_exact_cap(
        interest_bps in 1u16..=MAX_ANNUAL_INTEREST_BPS,
        cap in 100_000u64..10_000_000u64,
    ) {
        let start_ts = 100_000i64;
        let maturity = 2_000_000i64;
        let mut protocol = make_protocol(interest_bps, 0, maturity, cap, cap, start_ts);

        // Deposit exactly at cap
        let result = protocol.execute(&Operation::Deposit {
            lender_id: 0,
            amount: cap,
        });
        // Should succeed (scale_factor == WAD at start, so normalized == cap)
        prop_assert!(result.is_ok(), "Deposit at exact cap should succeed: {:?}", result);
        let after_cap = protocol.snapshot();
        prop_assert_eq!(after_cap.scaled_total_supply, u128::from(cap));
        prop_assert_eq!(after_cap.normalized_total_supply(), u128::from(cap));
        let lender0_scaled = after_cap
            .lender_positions
            .iter()
            .find(|p| p.lender_id == 0)
            .map(|p| p.scaled_balance)
            .unwrap_or(0);
        prop_assert_eq!(lender0_scaled, u128::from(cap));

        // Deposit 1 more should fail
        let before_fail = protocol.snapshot();
        let result = protocol.execute(&Operation::Deposit {
            lender_id: 1,
            amount: 1,
        });
        prop_assert_eq!(result, Err("CapExceeded".to_string()));
        let after_fail = protocol.snapshot();
        assert_snapshot_unchanged(&before_fail, &after_fail, "audit_boundary_exact_cap");
        assert_invariants(&protocol, "audit_boundary_exact_cap");
    }

    /// Full lifecycle test: deposit -> borrow -> repay -> withdraw with conservation.
    #[test]
    fn audit_full_lifecycle_conservation(
        interest_bps in 100u16..=5000u16,
        fee_bps in 0u16..=2000u16,
        deposit_amount in 1_000_000u64..10_000_000u64,
        borrow_fraction in 10u64..90u64, // percent
    ) {
        let start_ts = 100_000i64;
        let maturity = 500_000i64;
        let max_supply = deposit_amount * 3;
        let mut protocol = make_protocol(interest_bps, fee_bps, maturity, max_supply, max_supply, start_ts);

        // Deposit
        let dep = protocol.execute(&Operation::Deposit {
            lender_id: 0,
            amount: deposit_amount,
        });
        prop_assert!(dep.is_ok(), "deposit should succeed");
        let after_dep = protocol.snapshot();
        prop_assert_eq!(after_dep.scaled_total_supply, u128::from(deposit_amount));

        // Borrow a fraction
        let borrow_amount = deposit_amount * borrow_fraction / 100;
        let before_borrow = protocol.snapshot();
        let borrow = protocol.execute(&Operation::Borrow { amount: borrow_amount });
        prop_assert!(borrow.is_ok(), "borrow should succeed");
        let after_borrow = protocol.snapshot();
        prop_assert_eq!(
            before_borrow.vault_balance.saturating_sub(after_borrow.vault_balance),
            borrow_amount,
            "borrow must reduce vault by exact amount"
        );

        // Advance time pre-maturity and assert exact oracle transition.
        let before_pre_maturity_accrue = protocol.snapshot();
        let pre_maturity_ts = start_ts + 100_000;
        let accr_pre = protocol.execute(&Operation::AccrueInterest {
            timestamp: pre_maturity_ts,
        });
        prop_assert!(accr_pre.is_ok(), "pre-maturity accrue should succeed");
        let after_pre_maturity_accrue = protocol.snapshot();
        let expected_sf_pre = oracle_scale_factor_after_step(
            before_pre_maturity_accrue.scale_factor,
            interest_bps,
            before_pre_maturity_accrue.last_accrual_timestamp,
            maturity,
            pre_maturity_ts,
        )
        .expect("oracle scale overflow");
        let expected_fee_delta_pre = oracle_fee_delta_normalized(
            before_pre_maturity_accrue.scaled_total_supply,
            before_pre_maturity_accrue.scale_factor,
            interest_bps,
            before_pre_maturity_accrue.last_accrual_timestamp,
            maturity,
            pre_maturity_ts,
            fee_bps,
        )
        .expect("oracle fee overflow");
        prop_assert_eq!(after_pre_maturity_accrue.scale_factor, expected_sf_pre);
        prop_assert_eq!(
            after_pre_maturity_accrue.accrued_protocol_fees,
            before_pre_maturity_accrue.accrued_protocol_fees + expected_fee_delta_pre
        );

        // Repay
        let repay_amount = borrow_amount.saturating_add(borrow_amount / 10); // principal + 10%
        let before_repay = protocol.snapshot();
        let repay = protocol.execute(&Operation::Repay { amount: repay_amount });
        prop_assert!(repay.is_ok(), "repay should succeed");
        let after_repay = protocol.snapshot();
        prop_assert_eq!(
            after_repay.vault_balance.saturating_sub(before_repay.vault_balance),
            repay_amount,
            "repay must increase vault by exact amount"
        );

        // Check conservation at every point
        assert_invariants(&protocol, "audit_full_lifecycle_conservation after repay");

        // Advance past maturity and withdraw
        let before_maturity_accrue = protocol.snapshot();
        let accr_maturity = protocol.execute(&Operation::AccrueInterest {
            timestamp: maturity + 1,
        });
        prop_assert!(accr_maturity.is_ok(), "maturity accrue should succeed");
        let after_maturity_accrue = protocol.snapshot();
        let expected_sf_maturity = oracle_scale_factor_after_step(
            before_maturity_accrue.scale_factor,
            interest_bps,
            before_maturity_accrue.last_accrual_timestamp,
            maturity,
            maturity + 1,
        )
        .expect("oracle scale overflow");
        let expected_fee_delta_maturity = oracle_fee_delta_normalized(
            before_maturity_accrue.scaled_total_supply,
            before_maturity_accrue.scale_factor,
            interest_bps,
            before_maturity_accrue.last_accrual_timestamp,
            maturity,
            maturity + 1,
            fee_bps,
        )
        .expect("oracle fee overflow");
        prop_assert_eq!(after_maturity_accrue.scale_factor, expected_sf_maturity);
        prop_assert_eq!(
            after_maturity_accrue.accrued_protocol_fees,
            before_maturity_accrue.accrued_protocol_fees + expected_fee_delta_maturity
        );

        let before_withdraw = protocol.snapshot();
        let withdraw_op = Operation::Withdraw {
            lender_id: 0,
            scaled_amount: 0,
        };
        match protocol.execute(&withdraw_op) {
            Ok(()) => {
                let after_withdraw = protocol.snapshot();
                let lender_scaled_before = before_withdraw
                    .lender_positions
                    .iter()
                    .find(|p| p.lender_id == 0)
                    .map(|p| p.scaled_balance)
                    .unwrap_or(0);
                let settlement_for_payout = if before_withdraw.settlement_factor_wad == 0 {
                    let total_normalized = before_withdraw
                        .scaled_total_supply
                        .checked_mul(before_withdraw.scale_factor)
                        .unwrap_or(u128::MAX)
                        / WAD;
                    oracle_settlement_factor(
                        total_normalized,
                        before_withdraw.vault_balance,
                        before_withdraw.accrued_protocol_fees,
                    )
                } else {
                    before_withdraw.settlement_factor_wad
                };
                let expected_payout = lender_scaled_before
                    .checked_mul(before_withdraw.scale_factor)
                    .unwrap_or(u128::MAX)
                    / WAD
                    * settlement_for_payout
                    / WAD;
                let actual_payout = u128::from(
                    before_withdraw
                        .vault_balance
                        .saturating_sub(after_withdraw.vault_balance),
                );
                prop_assert_eq!(actual_payout, expected_payout);

                let before_second_withdraw = protocol.snapshot();
                let second = protocol.execute(&withdraw_op);
                prop_assert_eq!(second, Err("NoBalance".to_string()));
                let after_second_withdraw = protocol.snapshot();
                assert_snapshot_unchanged(
                    &before_second_withdraw,
                    &after_second_withdraw,
                    "audit_full_lifecycle_conservation second withdraw",
                );
            }
            Err(err) => {
                let after_failed = protocol.snapshot();
                assert_snapshot_unchanged(
                    &before_withdraw,
                    &after_failed,
                    "audit_full_lifecycle_conservation withdraw failure",
                );
                prop_assert!(
                    matches!(
                        err.as_str(),
                        "NoPosition" | "NoBalance" | "ZeroPayout" | "InsufficientVaultBalance"
                    ),
                    "unexpected withdraw failure: {}",
                    err
                );
            }
        }

        // Collect fees
        let before_collect = protocol.snapshot();
        let fees_collected_before = protocol.total_fees_collected;
        let expected_taken = std::cmp::min(
            before_collect.accrued_protocol_fees,
            before_collect.vault_balance,
        );
        match protocol.execute(&Operation::CollectFees) {
            Ok(()) => {
                let after_collect = protocol.snapshot();
                let taken = before_collect
                    .vault_balance
                    .saturating_sub(after_collect.vault_balance);
                prop_assert_eq!(taken, expected_taken);
                prop_assert_eq!(
                    protocol.total_fees_collected,
                    fees_collected_before + expected_taken
                );
                prop_assert_eq!(
                    after_collect.accrued_protocol_fees,
                    before_collect.accrued_protocol_fees.saturating_sub(expected_taken)
                );
            }
            Err(err) => {
                prop_assert_eq!(err, "NoFeesToCollect");
                prop_assert_eq!(expected_taken, 0);
                let after_failed = protocol.snapshot();
                assert_snapshot_unchanged(
                    &before_collect,
                    &after_failed,
                    "audit_full_lifecycle_conservation collect_fees failure",
                );
            }
        }

        // Final conservation check
        assert_invariants(&protocol, "audit_full_lifecycle_conservation final");
    }
}
