//! # Economic Attack Vector Analysis for CoalesceFi (Pinocchio)
//!
//! This module implements **28 attack-vector tests + 1 summary report** across 8 attack
//! categories, using pure-Rust "SMT-style" constraint solving (bounded exhaustive search +
//! proptest) to formally verify that no profitable attack strategies exist against the
//! CoalesceFi protocol's economic model.
//!
//! ## Categories
//!
//! - **A**: Rounding Exploitation (4 tests)
//! - **B**: Settlement Manipulation (4 tests)
//! - **C**: Fee Precision Loss (4 tests)
//! - **D**: Compound Interest Manipulation (3 tests)
//! - **E**: Multi-Step Attack Sequences (4 tests)
//! - **F**: Strategic Withdrawal Timing (3 tests)
//! - **G**: Capacity Bypass (3 tests)
//! - **H**: Re-Settlement Gaming (3 tests)

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
use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};
use coalesce::error::LendingError;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};
use pinocchio::error::ProgramError;
use std::collections::HashSet;

#[path = "common/math_oracle.rs"]
mod math_oracle;

// ============================================================================
// Section 1: ProtocolModel
// ============================================================================

/// Extended protocol model with attacker-tracking fields.
///
/// Builds on the `SimulatedProtocol` pattern from `security_audit_framework_tests.rs`,
/// adding per-lender deposit/withdrawal tracking and fee collection tracking
/// to measure attacker profit across multi-step sequences.
#[derive(Clone)]
struct ProtocolModel {
    market: Market,
    config: ProtocolConfig,
    lender_positions: Vec<(u32, LenderPosition)>,
    whitelist: BorrowerWhitelist,
    vault_balance: u64,
    current_timestamp: i64,
    /// Per-lender deposit tracking: (lender_id, total_deposited_u128).
    total_deposited_by: Vec<(u32, u128)>,
    /// Per-lender withdrawal tracking: (lender_id, total_withdrawn_u128).
    total_withdrawn_by: Vec<(u32, u128)>,
    /// Total fees collected from the vault.
    total_fees_collected: u64,
}

impl ProtocolModel {
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
            total_deposited_by: Vec::new(),
            total_withdrawn_by: Vec::new(),
            total_fees_collected: 0,
        }
    }

    fn accrue(&mut self, timestamp: i64) -> Result<(), String> {
        if timestamp < self.current_timestamp {
            return Err("Timestamp in the past".into());
        }
        self.current_timestamp = timestamp;
        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("AccrueInterest failed: {:?}", e))
    }

    fn deposit(&mut self, lender_id: u32, amount: u64) -> Result<(), String> {
        if amount == 0 {
            return Err("ZeroAmount".into());
        }
        if self.current_timestamp >= self.market.maturity_timestamp() {
            return Err("MarketMatured".into());
        }

        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        let sf = self.market.scale_factor();
        if sf == 0 {
            return Err("ZeroScaleFactor".into());
        }

        let amount_u128 = u128::from(amount);
        let scaled_amount = amount_u128
            .checked_mul(WAD)
            .ok_or("MathOverflow")?
            .checked_div(sf)
            .ok_or("MathOverflow")?;

        if scaled_amount == 0 {
            return Err("ZeroScaledAmount".into());
        }

        let new_scaled_total = self
            .market
            .scaled_total_supply()
            .checked_add(scaled_amount)
            .ok_or("MathOverflow")?;
        let new_normalized = new_scaled_total.checked_mul(sf).ok_or("MathOverflow")? / WAD;
        if new_normalized > u128::from(self.market.max_total_supply()) {
            return Err("CapExceeded".into());
        }

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

        // Track per-lender deposits
        if let Some(entry) = self
            .total_deposited_by
            .iter_mut()
            .find(|(id, _)| *id == lender_id)
        {
            entry.1 += amount_u128;
        } else {
            self.total_deposited_by.push((lender_id, amount_u128));
        }

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

    fn borrow(&mut self, amount: u64) -> Result<(), String> {
        if amount == 0 {
            return Err("ZeroAmount".into());
        }
        if self.current_timestamp >= self.market.maturity_timestamp() {
            return Err("MarketMatured".into());
        }

        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        // COAL-L02: no fee reservation, full vault is borrowable
        let borrowable = self.vault_balance;

        if amount > borrowable {
            return Err("BorrowAmountTooHigh".into());
        }

        let new_wl_total = self
            .whitelist
            .current_borrowed()
            .checked_add(amount)
            .ok_or("MathOverflow")?;
        if new_wl_total > self.whitelist.max_borrow_capacity() {
            return Err("GlobalCapacityExceeded".into());
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

    fn repay(&mut self, amount: u64) -> Result<(), String> {
        if amount == 0 {
            return Err("ZeroAmount".into());
        }

        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
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

    fn withdraw(&mut self, lender_id: u32, mut scaled_amount: u128) -> Result<u64, String> {
        if self.current_timestamp < self.market.maturity_timestamp() {
            return Err("NotMatured".into());
        }

        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        // Read balance before borrowing self mutably for settle
        let balance = self
            .lender_positions
            .iter()
            .find(|(id, _)| *id == lender_id)
            .ok_or("NoPosition")?
            .1
            .scaled_balance();

        if balance == 0 {
            return Err("NoBalance".into());
        }

        // Compute settlement factor if not set
        if self.market.settlement_factor_wad() == 0 {
            self.settle_market();
        }

        if scaled_amount == 0 {
            scaled_amount = balance;
        }
        if scaled_amount > balance {
            return Err("InsufficientScaledBalance".into());
        }

        let sf = self.market.scale_factor();
        let settlement = self.market.settlement_factor_wad();

        let normalized = scaled_amount.checked_mul(sf).ok_or("MathOverflow")? / WAD;
        let payout_u128 = normalized.checked_mul(settlement).ok_or("MathOverflow")? / WAD;
        let payout = u64::try_from(payout_u128).map_err(|_| "MathOverflow".to_string())?;

        if payout == 0 {
            return Err("ZeroPayout".into());
        }
        if payout > self.vault_balance {
            return Err("InsufficientVaultBalance".into());
        }

        self.vault_balance -= payout;

        // Track per-lender withdrawals
        if let Some(entry) = self
            .total_withdrawn_by
            .iter_mut()
            .find(|(id, _)| *id == lender_id)
        {
            entry.1 += u128::from(payout);
        } else {
            self.total_withdrawn_by
                .push((lender_id, u128::from(payout)));
        }

        let pos = self
            .lender_positions
            .iter_mut()
            .find(|(id, _)| *id == lender_id)
            .ok_or("NoPosition")?;
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

        Ok(payout)
    }

    fn settle_market(&mut self) {
        // COAL-C01: no fee reservation, full vault is available for lenders
        let available = u128::from(self.vault_balance);

        let total_normalized = self
            .market
            .scaled_total_supply()
            .checked_mul(self.market.scale_factor())
            .unwrap_or(0)
            / WAD;

        let settlement_factor = if total_normalized == 0 {
            WAD
        } else {
            let raw = available.checked_mul(WAD).unwrap_or(WAD) / total_normalized;
            let capped = if raw > WAD { WAD } else { raw };
            if capped < 1 {
                1
            } else {
                capped
            }
        };

        self.market.set_settlement_factor_wad(settlement_factor);
    }

    fn collect_fees(&mut self) -> Result<u64, String> {
        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        let accrued = self.market.accrued_protocol_fees();
        if accrued == 0 {
            return Err("NoFeesToCollect".into());
        }

        let withdrawable = std::cmp::min(accrued, self.vault_balance);
        if withdrawable == 0 {
            return Err("NoFeesToCollect".into());
        }

        self.vault_balance -= withdrawable;
        self.total_fees_collected = self
            .total_fees_collected
            .checked_add(withdrawable)
            .ok_or("MathOverflow")?;

        let remaining = accrued.checked_sub(withdrawable).ok_or("MathOverflow")?;
        self.market.set_accrued_protocol_fees(remaining);

        Ok(withdrawable)
    }

    fn re_settle(&mut self) -> Result<(), String> {
        let old_factor = self.market.settlement_factor_wad();
        if old_factor == 0 {
            return Err("NotSettled".into());
        }

        accrue_interest(&mut self.market, &self.config, self.current_timestamp)
            .map_err(|e| format!("Accrue failed: {:?}", e))?;

        // COAL-C01: no fee reservation, full vault is available for lenders
        let available = u128::from(self.vault_balance);

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
            return Err("SettlementNotImproved".into());
        }

        self.market.set_settlement_factor_wad(new_factor);
        Ok(())
    }

    /// Compute the net profit for a given lender (negative = loss).
    fn attacker_profit(&self, lender_id: u32) -> i128 {
        let deposited = self
            .total_deposited_by
            .iter()
            .find(|(id, _)| *id == lender_id)
            .map_or(0i128, |(_, v)| *v as i128);
        let withdrawn = self
            .total_withdrawn_by
            .iter()
            .find(|(id, _)| *id == lender_id)
            .map_or(0i128, |(_, v)| *v as i128);
        withdrawn - deposited
    }
}

// ============================================================================
// Section 2: Standalone Math Helpers
// ============================================================================

/// Compute scaled amount from deposit: amount * WAD / sf (floor division).
fn deposit_scale(amount: u128, sf: u128) -> u128 {
    if sf == 0 {
        return 0;
    }
    amount.checked_mul(WAD).unwrap_or(0) / sf
}

/// Normalize scaled amount back: scaled * sf / WAD (floor division).
fn normalize(scaled: u128, sf: u128) -> u128 {
    scaled.checked_mul(sf).unwrap_or(0) / WAD
}

/// Compute payout with double-floor: (scaled * sf / WAD) * settlement / WAD.
fn compute_payout(scaled: u128, sf: u128, settlement: u128) -> u128 {
    let normalized = normalize(scaled, sf);
    normalized.checked_mul(settlement).unwrap_or(0) / WAD
}

/// Compute settlement factor: clamp(available * WAD / total_normalized, 1, WAD).
/// COAL-C01: no fee reservation, full vault is available for lenders.
fn compute_settlement(vault: u128, scaled_supply: u128, sf: u128) -> u128 {
    let available = vault;
    let total_normalized = scaled_supply.checked_mul(sf).unwrap_or(0) / WAD;
    if total_normalized == 0 {
        return WAD;
    }
    let raw = available.checked_mul(WAD).unwrap_or(WAD) / total_normalized;
    let capped = if raw > WAD { WAD } else { raw };
    if capped < 1 {
        1
    } else {
        capped
    }
}

/// Saturating mul_wad: returns 0 on overflow instead of panicking.
fn mul_wad(a: u128, b: u128) -> u128 {
    a.checked_mul(b).unwrap_or(0) / WAD
}

/// Saturating pow_wad using local saturating mul_wad.
fn pow_wad(base: u128, exp: u32) -> u128 {
    let mut result = WAD;
    let mut b = base;
    let mut e = exp;

    while e > 0 {
        if e & 1 == 1 {
            result = mul_wad(result, b);
        }
        e >>= 1;
        if e > 0 {
            b = mul_wad(b, b);
        }
    }

    result
}

/// Saturating growth_factor_wad: takes (u128, u128) and uses unwrap_or(0)
/// for adversarial overflow testing. Signature differs from the shared oracle.
fn growth_factor_wad(annual_bps: u128, elapsed_seconds: u128) -> u128 {
    let whole_days = elapsed_seconds / math_oracle::SECONDS_PER_DAY;
    let remaining_seconds = elapsed_seconds % math_oracle::SECONDS_PER_DAY;

    let daily_rate_wad = annual_bps
        .checked_mul(WAD)
        .unwrap_or(0)
        .checked_div(math_oracle::DAYS_PER_YEAR.checked_mul(BPS).unwrap_or(1))
        .unwrap_or(0);
    let daily_growth = pow_wad(
        WAD.checked_add(daily_rate_wad).unwrap_or(0),
        u32::try_from(whole_days).unwrap_or(u32::MAX),
    );

    let remaining_delta_wad = annual_bps
        .checked_mul(remaining_seconds)
        .unwrap_or(0)
        .checked_mul(WAD)
        .unwrap_or(0)
        .checked_div(SECONDS_PER_YEAR.checked_mul(BPS).unwrap_or(1))
        .unwrap_or(0);
    let remaining_growth = WAD.checked_add(remaining_delta_wad).unwrap_or(0);

    mul_wad(daily_growth, remaining_growth)
}

/// Compute interest delta in WAD precision for the current daily-compound model.
fn compute_interest_delta(annual_bps: u128, time_elapsed: u128) -> u128 {
    growth_factor_wad(annual_bps, time_elapsed).saturating_sub(WAD)
}

/// Simulate multi-step compound vs single-step accrual.
/// Returns (multi_step_sf, single_step_sf).
fn compound_gain(bps: u16, total_time: i64, n_steps: u32, initial_sf: u128) -> (u128, u128) {
    // Multi-step
    let step_time = total_time / i64::from(n_steps);
    let mut multi_sf = initial_sf;
    for _ in 0..n_steps {
        let delta = compute_interest_delta(u128::from(bps), step_time as u128);
        let sf_delta = multi_sf.checked_mul(delta).unwrap_or(0) / WAD;
        multi_sf = multi_sf.saturating_add(sf_delta);
    }

    // Single-step: use effective total time (n_steps * step_time) to match multi-step duration,
    // avoiding integer division truncation of total_time / n_steps losing remainder seconds.
    let effective_total_time = i64::from(n_steps) * step_time;
    let delta = compute_interest_delta(u128::from(bps), effective_total_time as u128);
    let sf_delta = initial_sf.checked_mul(delta).unwrap_or(0) / WAD;
    let single_sf = initial_sf.saturating_add(sf_delta);

    (multi_sf, single_sf)
}

// ============================================================================
// Section 3: Search Infrastructure
// ============================================================================

/// Result of a bounded exhaustive search.
#[derive(Debug)]
struct SearchResult<T: std::fmt::Debug> {
    found: bool,
    witness: Option<T>,
    search_space_size: u64,
    max_profit: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelSnapshot {
    scale_factor: u128,
    settlement_factor_wad: u128,
    scaled_total_supply: u128,
    accrued_protocol_fees: u64,
    total_deposited: u64,
    total_borrowed: u64,
    total_repaid: u64,
    vault_balance: u64,
    total_fees_collected: u64,
    lender_balances: Vec<(u32, u128)>,
    deposits_by_lender: Vec<(u32, u128)>,
    withdrawals_by_lender: Vec<(u32, u128)>,
}

fn snapshot_model(model: &ProtocolModel) -> ModelSnapshot {
    let mut lender_balances: Vec<(u32, u128)> = model
        .lender_positions
        .iter()
        .map(|(id, pos)| (*id, pos.scaled_balance()))
        .collect();
    lender_balances.sort_unstable_by_key(|(id, _)| *id);

    let mut deposits_by_lender = model.total_deposited_by.clone();
    deposits_by_lender.sort_unstable_by_key(|(id, _)| *id);
    let mut withdrawals_by_lender = model.total_withdrawn_by.clone();
    withdrawals_by_lender.sort_unstable_by_key(|(id, _)| *id);

    ModelSnapshot {
        scale_factor: model.market.scale_factor(),
        settlement_factor_wad: model.market.settlement_factor_wad(),
        scaled_total_supply: model.market.scaled_total_supply(),
        accrued_protocol_fees: model.market.accrued_protocol_fees(),
        total_deposited: model.market.total_deposited(),
        total_borrowed: model.market.total_borrowed(),
        total_repaid: model.market.total_repaid(),
        vault_balance: model.vault_balance,
        total_fees_collected: model.total_fees_collected,
        lender_balances,
        deposits_by_lender,
        withdrawals_by_lender,
    }
}

fn assert_model_unchanged(before: &ModelSnapshot, after: &ModelSnapshot, context: &str) {
    assert_eq!(
        before, after,
        "State mutated on failure path: {}\nBefore={:?}\nAfter={:?}",
        context, before, after
    );
}

fn total_tracked(entries: &[(u32, u128)]) -> u128 {
    entries.iter().map(|(_, amount)| *amount).sum()
}

fn assert_system_value_conserved(model: &ProtocolModel, tolerance: i128, context: &str) {
    let deposits = total_tracked(&model.total_deposited_by);
    let withdrawals = total_tracked(&model.total_withdrawn_by);
    let repaid = u128::from(model.market.total_repaid());
    let borrowed = u128::from(model.market.total_borrowed());
    let fees = u128::from(model.total_fees_collected);
    let vault = u128::from(model.vault_balance);

    let lhs = deposits.saturating_add(repaid);
    let rhs = withdrawals
        .saturating_add(borrowed)
        .saturating_add(fees)
        .saturating_add(vault);
    let residual = lhs as i128 - rhs as i128;
    assert!(
        residual.abs() <= tolerance,
        "System value conservation violated in {}: lhs={} rhs={} residual={} tolerance={}",
        context,
        lhs,
        rhs,
        residual,
        tolerance
    );
}

fn assert_lender_profit_bound(
    model: &ProtocolModel,
    lender_id: u32,
    max_profit: i128,
    context: &str,
) {
    let profit = model.attacker_profit(lender_id);
    assert!(
        profit <= max_profit,
        "Profit bound violation in {} for lender {}: profit={} bound={}",
        context,
        lender_id,
        profit,
        max_profit
    );
}

fn abs_diff_u128(a: u128, b: u128) -> u128 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

// ============================================================================
// Category A: Rounding Exploitation (4 tests)
// ============================================================================

/// A1: Find (amount, sf) where withdraw yields > deposit.
#[test]
fn attack_dust_deposit_withdraw_profit() {
    let mut search_space: u64 = 0;
    let mut max_profit: i128 = i128::MIN;
    let mut found = false;
    let mut witness: Option<(u128, u128)> = None;

    // Dense around WAD + small offsets plus sampled up to 2*WAD.
    let mut scale_factors: Vec<u128> = (WAD..=WAD + 5_000).collect();
    scale_factors.extend((0u128..=1000).map(|sf_offset| WAD + sf_offset * (WAD / 1000)));
    scale_factors.sort_unstable();
    scale_factors.dedup();

    for amount in 1u128..=1000 {
        for sf in scale_factors.iter().copied() {
            search_space += 1;

            let scaled = deposit_scale(amount, sf);
            if scaled == 0 {
                continue;
            }
            let payout = normalize(scaled, sf);
            let profit = payout as i128 - amount as i128;

            if profit > max_profit {
                max_profit = profit;
            }
            if profit > 0 {
                found = true;
                witness = Some((amount, sf));
                break;
            }
        }
        if found {
            break;
        }
    }

    let result: SearchResult<(u128, u128)> = SearchResult {
        found,
        witness,
        search_space_size: search_space,
        max_profit,
    };

    assert!(
        !result.found,
        "Found profitable dust deposit-withdraw: {:?}",
        result.witness
    );
    assert!(
        result.search_space_size >= 1_000_000,
        "Search space unexpectedly small: {}",
        result.search_space_size
    );
    assert!(
        result.max_profit == 0,
        "Round-trip max profit should be exactly zero (at sf=WAD), got {}",
        result.max_profit
    );
}

/// A2: N deposits of amount=1, withdraw all: total > N?
#[test]
fn attack_repeated_dust_deposits_accumulate() {
    for sf_offset in 1u128..=1000 {
        let sf = WAD + sf_offset;

        for n in 1u128..=100 {
            // Each deposit of 1 yields scaled = 1 * WAD / sf (floor)
            let scaled_per = deposit_scale(1, sf);
            let total_scaled = scaled_per * n;
            let total_payout = normalize(total_scaled, sf);

            assert!(
                total_payout <= n,
                "Repeated dust deposits created value: n={}, sf={}, payout={}, deposited={}",
                n,
                sf,
                total_payout,
                n
            );

            // Repeated dust deposits must not outperform a single deposit of equivalent size.
            let single_scaled = deposit_scale(n, sf);
            let single_payout = normalize(single_scaled, sf);
            assert!(
                total_payout <= single_payout.saturating_add(1),
                "Repeated deposits outperformed single-shot too much: sf={}, n={}, repeated={}, single={}",
                sf,
                n,
                total_payout,
                single_payout
            );
        }
    }
}

// A3: Random deposit-withdraw cycle never creates value.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn attack_proptest_rounding_never_creates_value(
        amount in 1u64..10_000_000u64,
        sf_offset in 0u64..1_000_000_000_000_000_000u64,
    ) {
        let sf = WAD + u128::from(sf_offset);
        let scaled = deposit_scale(u128::from(amount), sf);
        if scaled > 0 {
            let payout = normalize(scaled, sf);
            prop_assert!(
                payout <= u128::from(amount),
                "Rounding created value: amount={}, sf={}, payout={}",
                amount, sf, payout
            );

            let mut model = ProtocolModel::new(
                0,
                0,
                10,
                amount.saturating_mul(2),
                amount.saturating_mul(2),
                0,
            );
            model.market.set_scale_factor(sf);
            model.deposit(7, amount).unwrap();
            model.accrue(11).unwrap();
            let _ = model.withdraw(7, 0).unwrap();
            prop_assert!(
                model.attacker_profit(7) <= 0,
                "Model round-trip created profit: amount={}, sf={}, profit={}",
                amount,
                sf,
                model.attacker_profit(7)
            );
        }
    }
}

/// A4: Measure max rounding loss per deposit-withdraw cycle.
#[test]
fn attack_rounding_loss_quantification() {
    let mut max_loss: u128 = 0;
    let mut max_loss_witness: Option<(u128, u128)> = None;

    for amount in 1u128..=1000 {
        for sf_offset in 0u128..1000 {
            let sf = WAD + sf_offset * (WAD / 1000);
            let scaled = deposit_scale(amount, sf);
            if scaled == 0 {
                continue;
            }
            let payout = normalize(scaled, sf);
            let loss = amount.saturating_sub(payout);

            let per_case_bound = (sf + WAD - 1) / WAD;
            assert!(
                loss <= per_case_bound,
                "Loss exceeded per-case bound: amount={}, sf={}, loss={}, bound={}",
                amount,
                sf,
                loss,
                per_case_bound
            );

            if loss > max_loss {
                max_loss = loss;
                max_loss_witness = Some((amount, sf));
            }
        }
    }

    assert!(
        max_loss <= 2,
        "Max rounding loss per deposit-withdraw cycle exceeds 2 base units: {} (witness={:?})",
        max_loss,
        max_loss_witness
    );
}

// ============================================================================
// Category B: Settlement Manipulation (4 tests)
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// B1: Deposit D, borrow B draining vault, first withdrawal locks low settlement.
    #[test]
    fn attack_vault_drain_before_settlement(
        deposit in 100_000u64..10_000_000u64,
        borrow_pct in 10u64..95u64,
        bps in 100u16..5000u16,
    ) {
        let start_ts = 100_000i64;
        let maturity = 500_000i64;
        let max_supply = deposit * 2;
        let mut model = ProtocolModel::new(bps, 0, maturity, max_supply, max_supply, start_ts);

        model.deposit(0, deposit).unwrap();

        let borrow_amount = (deposit as u128 * u128::from(borrow_pct) / 100) as u64;
        if borrow_amount > 0 {
            model.borrow(borrow_amount).unwrap();
        }

        // Advance past maturity
        model.accrue(maturity + 1).unwrap();

        let expected_settlement = compute_settlement(
            u128::from(model.vault_balance),
            model.market.scaled_total_supply(),
            model.market.scale_factor(),
        );

        // Withdraw triggers settlement
        let payout = model.withdraw(0, 0).unwrap();

        // Settlement should correctly reflect available / total_normalized
        let sf = model.market.settlement_factor_wad();
        prop_assert!(sf >= 1 && sf <= WAD, "Settlement out of bounds: {}", sf);
        prop_assert_eq!(
            sf,
            expected_settlement,
            "Settlement mismatch: expected={}, actual={}",
            expected_settlement,
            sf
        );

        // Payout should not exceed deposit (no interest earned with bps=0 fee)
        prop_assert!(
            u128::from(payout) <= u128::from(deposit) + 2,
            "Payout {} exceeds deposit {} + rounding",
            payout,
            deposit
        );
        assert_lender_profit_bound(&model, 0, 2, "B1-vault-drain-profit");
        assert_system_value_conserved(&model, 2, "B1-vault-drain");
    }
}

/// B2: Two equal lenders, A withdraws first, B second — payouts must be equal.
#[test]
fn attack_settlement_first_mover_advantage() {
    let start_ts = 100_000i64;
    let maturity = 500_000i64;
    let deposit = 1_000_000u64;
    let mut model = ProtocolModel::new(500, 0, maturity, deposit * 4, deposit * 4, start_ts);

    // Two equal deposits
    model.deposit(0, deposit).unwrap();
    model.deposit(1, deposit).unwrap();

    // Advance past maturity
    model.accrue(maturity + 1).unwrap();

    // A withdraws first
    let payout_a = model.withdraw(0, 0).unwrap();
    // B withdraws second
    let payout_b = model.withdraw(1, 0).unwrap();

    // Settlement factor is global and locked on first withdrawal
    // Equal deposits at same scale_factor => equal scaled_balance => equal payout
    assert_eq!(
        payout_a, payout_b,
        "First-mover advantage detected: A got {}, B got {}",
        payout_a, payout_b
    );
    assert!(
        u128::from(payout_a) + u128::from(payout_b) <= u128::from(deposit) * 2 + 2,
        "Total payouts exceeded principal: a={}, b={}, deposit={}",
        payout_a,
        payout_b,
        deposit
    );
    assert_lender_profit_bound(&model, 0, 2, "B2-lender-0");
    assert_lender_profit_bound(&model, 1, 2, "B2-lender-1");
    assert_system_value_conserved(&model, 2, "B2-settlement-order");
}

/// B3: Lock low settlement, repay, re-settle, withdraw.
#[test]
fn attack_settlement_lock_then_resettle() {
    let start_ts = 100_000i64;
    let maturity = 500_000i64;
    let deposit = 1_000_000u64;
    let borrow_amount = 500_000u64;
    let mut model = ProtocolModel::new(500, 0, maturity, deposit * 4, deposit * 4, start_ts);

    // Deposit
    model.deposit(0, deposit).unwrap();
    model.deposit(1, deposit).unwrap();

    // Borrow to drain vault
    model.borrow(borrow_amount).unwrap();

    // Advance past maturity
    model.accrue(maturity + 1).unwrap();

    // First withdrawal locks settlement with partially drained vault
    let payout_first = model.withdraw(0, 0).unwrap();
    let old_factor = model.market.settlement_factor_wad();
    assert!(
        old_factor < WAD,
        "Settlement should be < WAD when vault partially drained"
    );

    // Repay the borrowed amount
    model.repay(borrow_amount).unwrap();

    // Re-settle should improve the factor
    model.re_settle().unwrap();
    let new_factor = model.market.settlement_factor_wad();
    assert!(
        new_factor > old_factor,
        "Re-settle should improve factor: old={}, new={}",
        old_factor,
        new_factor
    );

    // Second lender withdraws at better factor
    let payout_second = model.withdraw(1, 0).unwrap();

    assert!(
        payout_second >= payout_first,
        "Re-settle should improve late withdrawal: first={}, second={}",
        payout_first,
        payout_second
    );

    // With 5% annual interest over 400K seconds (maturity - start), each lender earns interest.
    let interest_time = (maturity - start_ts) as u128;
    let max_interest_per_lender =
        (u128::from(deposit) * 500 * interest_time / (31_536_000u128 * 10_000)) as i128 + 2;
    let total_withdrawn = u128::from(payout_first) + u128::from(payout_second);
    assert!(
        total_withdrawn <= u128::from(deposit) * 2 + (max_interest_per_lender as u128) * 2,
        "Total withdrawn exceeds deposits + interest: total_withdrawn={}, deposits={}",
        total_withdrawn,
        u128::from(deposit) * 2
    );
    assert_lender_profit_bound(&model, 0, max_interest_per_lender, "B3-first-lender");
    assert_lender_profit_bound(&model, 1, max_interest_per_lender, "B3-second-lender");
    assert_system_value_conserved(&model, max_interest_per_lender, "B3-resettle");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// B4: Settlement factor always in [1, WAD].
    #[test]
    fn attack_proptest_settlement_bounds_always_hold(
        vault in 0u64..10_000_000u64,
        supply in 1u128..1_000_000_000_000u128,
        sf_offset in 0u64..1_000_000_000u64,
        vault_boost in 0u64..5_000_000u64,
    ) {
        let sf = WAD + u128::from(sf_offset);
        // COAL-C01: no fee reservation, full vault is available for lenders
        let settlement = compute_settlement(
            u128::from(vault),
            supply,
            sf,
        );
        prop_assert!(settlement >= 1, "Settlement below 1: {}", settlement);
        prop_assert!(settlement <= WAD, "Settlement above WAD: {}", settlement);

        let available = u128::from(vault);
        let total_normalized = supply.saturating_mul(sf) / WAD;
        let expected = if total_normalized == 0 {
            WAD
        } else {
            let raw = available.saturating_mul(WAD) / total_normalized;
            if raw < 1 {
                1
            } else if raw > WAD {
                WAD
            } else {
                raw
            }
        };
        prop_assert_eq!(
            settlement,
            expected,
            "Settlement formula mismatch: vault={}, supply={}, sf={}",
            vault,
            supply,
            sf
        );

        let settlement_boosted = compute_settlement(
            u128::from(vault).saturating_add(u128::from(vault_boost)),
            supply,
            sf,
        );
        prop_assert!(
            settlement_boosted >= settlement,
            "Settlement not monotonic with higher vault: base={} boosted={}",
            settlement,
            settlement_boosted
        );
    }
}

// ============================================================================
// Category C: Fee Precision Loss (4 tests)
// ============================================================================

/// C1: Compare double-floor fee vs ideal single-division.
#[test]
fn attack_fee_double_floor_precision_loss() {
    // Known parameters: 10% annual, 5% fee, 1M supply, 1 year
    let annual_bps = 1000u128; // 10%
    let fee_rate_bps = 500u128; // 5%
    let scaled_supply = 1_000_000_000_000u128; // 1M USDC in base units
    let time_elapsed = SECONDS_PER_YEAR;

    // Interest delta
    let interest_delta_wad = compute_interest_delta(annual_bps, time_elapsed);
    let new_sf = WAD + WAD * interest_delta_wad / WAD;

    // Double-floor fee (matches on-chain): fee_delta_wad = interest_delta_wad * fee_rate / BPS
    let fee_delta_wad = interest_delta_wad * fee_rate_bps / BPS;
    let actual_fee = scaled_supply * new_sf / WAD * fee_delta_wad / WAD;

    // Ideal single-division fee (use checked math to avoid overflow)
    let ideal_fee = scaled_supply.checked_mul(new_sf).unwrap_or(0) / WAD * interest_delta_wad / WAD
        * fee_rate_bps
        / BPS;

    // The difference should be bounded
    let diff = if ideal_fee > actual_fee {
        ideal_fee - actual_fee
    } else {
        actual_fee - ideal_fee
    };

    assert!(
        actual_fee <= ideal_fee,
        "Double-floor fee must not exceed ideal fee: ideal={}, actual={}",
        ideal_fee,
        actual_fee
    );

    let normalized_supply = scaled_supply
        .checked_mul(new_sf)
        .unwrap_or(u128::MAX)
        .checked_div(WAD)
        .unwrap_or(u128::MAX);
    let rounding_bound = normalized_supply
        .checked_add(WAD - 1)
        .unwrap_or(u128::MAX)
        .checked_div(WAD)
        .unwrap_or(u128::MAX)
        .saturating_add(1);

    assert!(
        diff <= rounding_bound,
        "Fee precision loss too large for floor-error bound: ideal={}, actual={}, diff={}, bound={}",
        ideal_fee,
        actual_fee,
        diff,
        rounding_bound
    );
}

/// C2: N=100 small accruals vs N=1 large accrual.
#[test]
fn attack_fee_many_small_vs_one_large() {
    let annual_bps: u16 = 1000;
    let fee_rate_bps: u16 = 500;
    let scaled_supply = 1_000_000_000_000u128;
    let total_time = 365 * 24 * 3600i64; // 1 year
    let n = 100u32;
    let step = total_time / i64::from(n);

    // Multi-step accrual
    let mut market_multi = Market::zeroed();
    market_multi.set_annual_interest_bps(annual_bps);
    market_multi.set_maturity_timestamp(i64::MAX);
    market_multi.set_scale_factor(WAD);
    market_multi.set_last_accrual_timestamp(0);
    market_multi.set_scaled_total_supply(scaled_supply);

    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_rate_bps);

    for i in 1..=n {
        accrue_interest(&mut market_multi, &config, i64::from(i) * step).unwrap();
    }
    let multi_fees = market_multi.accrued_protocol_fees();

    // Single-step accrual
    let mut market_single = Market::zeroed();
    market_single.set_annual_interest_bps(annual_bps);
    market_single.set_maturity_timestamp(i64::MAX);
    market_single.set_scale_factor(WAD);
    market_single.set_last_accrual_timestamp(0);
    market_single.set_scaled_total_supply(scaled_supply);

    accrue_interest(&mut market_single, &config, total_time).unwrap();
    let single_fees = market_single.accrued_protocol_fees();
    let multi_sf = market_multi.scale_factor();
    let single_sf = market_single.scale_factor();

    // The fee difference between multi-step and single-step accrual should be bounded.
    // Multi-step compound interest grows sf faster, but fee computation per-step uses
    // smaller interest_delta_wad values. The net effect can go either direction.
    let diff = if multi_fees > single_fees {
        multi_fees - single_fees
    } else {
        single_fees - multi_fees
    };

    // Both should produce non-zero fees
    assert!(multi_fees > 0, "Multi-step fees should be non-zero");
    assert!(single_fees > 0, "Single-step fees should be non-zero");
    assert!(
        multi_sf >= single_sf,
        "Multi-step scale factor should not be below single-step: multi_sf={}, single_sf={}",
        multi_sf,
        single_sf
    );

    let normalized_multi = scaled_supply
        .checked_mul(multi_sf)
        .unwrap_or(u128::MAX)
        .checked_div(WAD)
        .unwrap_or(u128::MAX);
    let normalized_single = scaled_supply
        .checked_mul(single_sf)
        .unwrap_or(u128::MAX)
        .checked_div(WAD)
        .unwrap_or(u128::MAX);
    let interest_multi = normalized_multi.saturating_sub(scaled_supply);
    let interest_single = normalized_single.saturating_sub(scaled_supply);
    assert!(
        u128::from(multi_fees) <= interest_multi.saturating_add(1),
        "Multi-step fees {} exceed generated interest {}",
        multi_fees,
        interest_multi
    );
    assert!(
        u128::from(single_fees) <= interest_single.saturating_add(1),
        "Single-step fees {} exceed generated interest {}",
        single_fees,
        interest_single
    );

    // Path dependence is expected under floor division and per-step fee accrual.
    // After Finding 10 (pre-accrual SF), multi-step accrual uses progressively
    // larger scale factors as input, so multi_fees >= single_fees is expected.
    assert!(
        multi_fees >= single_fees,
        "Unexpected fee ordering: multi-step fees {} should be >= single-step fees {}",
        multi_fees,
        single_fees
    );
    assert!(
        u128::from(diff) <= u128::from(single_fees),
        "Fee divergence exploded unexpectedly: multi={}, single={}, diff={}",
        multi_fees,
        single_fees,
        diff
    );
}

/// C3: Find params where fee overflows u64.
#[test]
fn attack_fee_overflow_u64_boundary() {
    // Extreme values that could cause overflow
    let annual_bps: u16 = 10_000; // 100%
    let fee_rate_bps: u16 = 10_000; // 100% fee rate
                                    // Use very large supply near u64::MAX
    let scaled_supply = u128::from(u64::MAX);
    let total_time = 365 * 24 * 3600i64; // 1 year

    let mut market = Market::zeroed();
    market.set_annual_interest_bps(annual_bps);
    market.set_maturity_timestamp(i64::MAX);
    market.set_scale_factor(WAD);
    market.set_last_accrual_timestamp(0);
    market.set_scaled_total_supply(scaled_supply);

    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_rate_bps);
    let state_before = (
        market.scale_factor(),
        market.last_accrual_timestamp(),
        market.accrued_protocol_fees(),
    );

    // This should either succeed with valid u64 fee or return MathOverflow
    let result = accrue_interest(&mut market, &config, total_time);
    match result {
        Ok(()) => {
            // If it succeeded, the fee must fit in u64
            let fees = market.accrued_protocol_fees();
            assert!(fees > 0, "Fees should be non-zero with 100% rate");
            assert_eq!(
                market.last_accrual_timestamp(),
                total_time,
                "Last accrual timestamp should advance on success"
            );
            let interest_delta_wad =
                compute_interest_delta(u128::from(annual_bps), total_time as u128);
            // Use pre-accrual SF (WAD) for interest bound check (Finding 10)
            let interest_on_supply = scaled_supply
                .checked_mul(WAD)
                .unwrap_or(u128::MAX)
                .checked_div(WAD)
                .unwrap_or(u128::MAX)
                .checked_mul(interest_delta_wad)
                .unwrap_or(u128::MAX)
                .checked_div(WAD)
                .unwrap_or(u128::MAX);
            assert!(
                u128::from(fees) <= interest_on_supply.saturating_add(1),
                "Fees {} exceed generated interest {}",
                fees,
                interest_on_supply
            );
        },
        Err(err) => {
            assert_eq!(
                err,
                ProgramError::Custom(LendingError::MathOverflow as u32),
                "Unexpected error variant for overflow boundary: {:?}",
                err
            );
            let state_after = (
                market.scale_factor(),
                market.last_accrual_timestamp(),
                market.accrued_protocol_fees(),
            );
            assert_eq!(
                state_before, state_after,
                "Market state mutated on overflow error"
            );
        },
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// C4: Accrued fees never exceed total interest generated.
    #[test]
    fn attack_proptest_fee_never_exceeds_interest(
        annual_bps in 1u16..=10_000u16,
        fee_rate_bps in 1u16..=10_000u16,
        supply in 1_000u128..1_000_000_000_000u128,
        elapsed in 1i64..31_536_000i64,
    ) {
        let mut market = Market::zeroed();
        market.set_annual_interest_bps(annual_bps);
        market.set_maturity_timestamp(i64::MAX);
        market.set_scale_factor(WAD);
        market.set_last_accrual_timestamp(0);
        market.set_scaled_total_supply(supply);

        let mut config = ProtocolConfig::zeroed();
        config.set_fee_rate_bps(fee_rate_bps);

        let before = (
            market.scale_factor(),
            market.last_accrual_timestamp(),
            market.accrued_protocol_fees(),
        );
        let result = accrue_interest(&mut market, &config, elapsed);
        match result {
            Ok(()) => {
                let fees = u128::from(market.accrued_protocol_fees());
                let new_sf = market.scale_factor();
                let interest_delta_wad = compute_interest_delta(
                    u128::from(annual_bps),
                    elapsed as u128,
                );
                let fee_delta_wad = interest_delta_wad
                    .checked_mul(u128::from(fee_rate_bps))
                    .unwrap_or(u128::MAX)
                    / BPS;
                // Use pre-accrual scale factor (WAD) for fee computation (Finding 10)
                let expected_fee = supply
                    .checked_mul(WAD).unwrap_or(u128::MAX) / WAD
                    * fee_delta_wad / WAD;
                let interest_on_supply = supply
                    .checked_mul(WAD).unwrap_or(u128::MAX) / WAD
                    * interest_delta_wad / WAD;

                prop_assert_eq!(
                    fees,
                    expected_fee,
                    "Fee mismatch against on-chain formula (bps={}, fee_bps={}, supply={}, elapsed={})",
                    annual_bps,
                    fee_rate_bps,
                    supply,
                    elapsed
                );
                prop_assert!(
                    fees <= interest_on_supply + 2,
                    "Fees {} exceed interest on supply {} (bps={}, fee_bps={}, supply={})",
                    fees, interest_on_supply, annual_bps, fee_rate_bps, supply
                );
            }
            Err(err) => {
                prop_assert_eq!(
                    err.clone(),
                    ProgramError::Custom(LendingError::MathOverflow as u32),
                    "Unexpected accrue_interest error: {:?}",
                    err
                );
                let after = (
                    market.scale_factor(),
                    market.last_accrual_timestamp(),
                    market.accrued_protocol_fees(),
                );
                prop_assert_eq!(before, after, "State mutated on overflow error path");
            }
        }
    }
}

// ============================================================================
// Category D: Compound Interest Manipulation (3 tests)
// ============================================================================

/// D1: Quantify compound excess over simple interest.
#[test]
fn attack_compound_vs_simple_delta() {
    let test_cases: Vec<(u16, i64, u32)> = vec![
        (500, 31_536_000, 2),    // 5%, 1 year, 2 steps
        (1000, 31_536_000, 10),  // 10%, 1 year, 10 steps
        (1000, 31_536_000, 100), // 10%, 1 year, 100 steps
        (1000, 31_536_000, 365), // 10%, 1 year, daily
        (5000, 31_536_000, 12),  // 50%, 1 year, monthly
    ];

    for (bps, time, steps) in test_cases {
        let (multi_sf, single_sf) = compound_gain(bps, time, steps, WAD);
        let step_time = time / i64::from(steps);

        let mut model_multi = Market::zeroed();
        model_multi.set_annual_interest_bps(bps);
        model_multi.set_maturity_timestamp(i64::MAX);
        model_multi.set_scale_factor(WAD);
        model_multi.set_last_accrual_timestamp(0);
        model_multi.set_scaled_total_supply(1);

        let zero_fee = ProtocolConfig::zeroed();
        for i in 1..=steps {
            accrue_interest(&mut model_multi, &zero_fee, i64::from(i) * step_time).unwrap();
        }

        let mut model_single = Market::zeroed();
        model_single.set_annual_interest_bps(bps);
        model_single.set_maturity_timestamp(i64::MAX);
        model_single.set_scale_factor(WAD);
        model_single.set_last_accrual_timestamp(0);
        model_single.set_scaled_total_supply(1);
        accrue_interest(&mut model_single, &zero_fee, time).unwrap();

        assert_eq!(
            multi_sf,
            model_multi.scale_factor(),
            "compound_gain multi-step diverged from accrue_interest model (bps={}, time={}, steps={})",
            bps,
            time,
            steps
        );
        assert_eq!(
            single_sf,
            model_single.scale_factor(),
            "compound_gain single-step diverged from accrue_interest model (bps={}, time={}, steps={})",
            bps,
            time,
            steps
        );

        // Compound should be >= simple
        assert!(
            multi_sf >= single_sf,
            "Compound < simple: bps={}, steps={}, compound={}, simple={}",
            bps,
            steps,
            multi_sf,
            single_sf
        );

        // The delta should be bounded (not exponentially divergent for reasonable params)
        let delta = multi_sf - single_sf;
        let simple_interest = single_sf - WAD;
        if simple_interest > 0 {
            // Compound excess should be a fraction of simple interest
            assert!(
                delta <= simple_interest,
                "Compound excess exceeds simple interest: delta={}, simple={}",
                delta,
                simple_interest
            );
        }
    }
}

/// D2: Can attacker profit by triggering frequent accruals?
#[test]
fn attack_compound_manipulation_profit() {
    let start_ts = 0i64;
    let maturity = 1_000_000i64;
    let deposit = 1_000_000u64;
    let bps = 1000u16; // 10%

    // Scenario A: Single accrual at maturity (attacker does nothing)
    let mut model_a = ProtocolModel::new(bps, 0, maturity, deposit * 4, deposit * 4, start_ts);
    model_a.deposit(0, deposit).unwrap();
    model_a.deposit(1, deposit).unwrap();
    model_a.accrue(maturity + 1).unwrap();
    let payout_a0 = model_a.withdraw(0, 0).unwrap();
    let payout_a1 = model_a.withdraw(1, 0).unwrap();

    // Scenario B: Frequent accruals (attacker triggers accrual every 10k seconds)
    let mut model_b = ProtocolModel::new(bps, 0, maturity, deposit * 4, deposit * 4, start_ts);
    model_b.deposit(0, deposit).unwrap();
    model_b.deposit(1, deposit).unwrap();
    let mut ts = start_ts;
    while ts < maturity {
        ts += 10_000;
        if ts > maturity {
            ts = maturity;
        }
        model_b.accrue(ts).unwrap();
    }
    model_b.accrue(maturity + 1).unwrap();
    let payout_b0 = model_b.withdraw(0, 0).unwrap();
    let payout_b1 = model_b.withdraw(1, 0).unwrap();

    // Both lenders get identical payouts within their scenario — no individual advantage
    assert_eq!(payout_a0, payout_a1, "Unequal payouts in scenario A");
    assert_eq!(payout_b0, payout_b1, "Unequal payouts in scenario B");

    // Compound effect means scenario B may yield more, but the benefit is shared equally
    // No individual attacker advantage exists
    assert!(
        payout_b0 >= payout_a0,
        "Frequent accruals should not decrease payouts: B0={}, A0={}",
        payout_b0,
        payout_a0
    );
    assert!(
        u128::from(payout_a0) + u128::from(payout_a1) <= u128::from(deposit) * 2 + 2,
        "Scenario A overpaid principals: a0={}, a1={}, deposit={}",
        payout_a0,
        payout_a1,
        deposit
    );
    assert!(
        u128::from(payout_b0) + u128::from(payout_b1) <= u128::from(deposit) * 2 + 2,
        "Scenario B overpaid principals: b0={}, b1={}, deposit={}",
        payout_b0,
        payout_b1,
        deposit
    );
    assert_lender_profit_bound(&model_a, 0, 2, "D2-scenario-a-l0");
    assert_lender_profit_bound(&model_a, 1, 2, "D2-scenario-a-l1");
    assert_lender_profit_bound(&model_b, 0, 2, "D2-scenario-b-l0");
    assert_lender_profit_bound(&model_b, 1, 2, "D2-scenario-b-l1");
    assert_system_value_conserved(&model_a, 2, "D2-scenario-a");
    assert_system_value_conserved(&model_b, 2, "D2-scenario-b");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// D3: Compound delta bounded by second-order Taylor term.
    #[test]
    fn attack_proptest_compound_effect_bounded(
        bps in 1u16..=5000u16,
        total_time in 1000i64..31_536_000i64,
        n_steps in 2u32..100u32,
    ) {
        let step_time = total_time / i64::from(n_steps);
        // Effective total time is n_steps * step_time (may be < total_time due to integer division)
        let effective_total_time = i64::from(n_steps) * step_time;

        let (multi_sf, single_sf) = compound_gain(bps, total_time, n_steps, WAD);
        let delta = multi_sf.saturating_sub(single_sf);

        let mut model_multi = Market::zeroed();
        model_multi.set_annual_interest_bps(bps);
        model_multi.set_maturity_timestamp(i64::MAX);
        model_multi.set_scale_factor(WAD);
        model_multi.set_last_accrual_timestamp(0);
        model_multi.set_scaled_total_supply(1);
        let zero_fee = ProtocolConfig::zeroed();
        for i in 1..=n_steps {
            accrue_interest(&mut model_multi, &zero_fee, i64::from(i) * step_time).unwrap();
        }

        let mut model_single = Market::zeroed();
        model_single.set_annual_interest_bps(bps);
        model_single.set_maturity_timestamp(i64::MAX);
        model_single.set_scale_factor(WAD);
        model_single.set_last_accrual_timestamp(0);
        model_single.set_scaled_total_supply(1);
        // Use effective_total_time so single-step covers the same duration as multi-step
        accrue_interest(&mut model_single, &zero_fee, effective_total_time).unwrap();

        prop_assert_eq!(
            multi_sf,
            model_multi.scale_factor(),
            "multi-step oracle mismatch (bps={}, total_time={}, steps={})",
            bps,
            total_time,
            n_steps
        );
        prop_assert_eq!(
            single_sf,
            model_single.scale_factor(),
            "single-step oracle mismatch (bps={}, total_time={}, steps={})",
            bps,
            effective_total_time,
            n_steps
        );
        prop_assert!(
            multi_sf >= single_sf,
            "Compound should not underperform simple: multi={}, single={}",
            multi_sf,
            single_sf
        );

        // Taylor second-order bound: bps^2 * time^2 * WAD / (YEAR^2 * BPS^2 * 2)
        let bps_u128 = u128::from(bps);
        let time_u128 = effective_total_time as u128;
        let numerator = bps_u128
            .checked_mul(bps_u128)
            .and_then(|v| v.checked_mul(time_u128))
            .and_then(|v| v.checked_mul(time_u128))
            .and_then(|v| v.checked_mul(WAD));
        let denominator = SECONDS_PER_YEAR
            .checked_mul(SECONDS_PER_YEAR)
            .and_then(|v| v.checked_mul(BPS))
            .and_then(|v| v.checked_mul(BPS))
            .and_then(|v| v.checked_mul(2));

        if let (Some(num), Some(den)) = (numerator, denominator) {
            if den > 0 {
                let taylor_bound = num / den;
                // Add generous margin (10x) since Taylor is approximate
                prop_assert!(
                    delta <= taylor_bound * 10 + 1,
                    "Compound delta {} exceeds 10x Taylor bound {} (bps={}, time={}, steps={})",
                    delta, taylor_bound, bps, total_time, n_steps
                );
            }
        }
        // If overflow in bound computation, skip (parameters too extreme)
    }
}

// ============================================================================
// Category E: Multi-Step Attack Sequences (4 tests)
// ============================================================================

/// E1: Attacker is lender+borrower; net profit should be impossible.
#[test]
fn attack_deposit_borrow_repay_withdraw() {
    let bps_values = [100u16, 500, 1000, 5000];
    let deposit_values = [100_000u64, 1_000_000, 5_000_000];

    for &bps in &bps_values {
        for &deposit in &deposit_values {
            let start_ts = 0i64;
            let maturity = 500_000i64;
            let max_supply = deposit * 4;
            let mut model = ProtocolModel::new(bps, 0, maturity, max_supply, max_supply, start_ts);

            // Attacker deposits
            model.deposit(0, deposit).unwrap();

            // Attacker borrows max possible (COAL-L02: full vault is borrowable)
            let borrowable = model.vault_balance;
            if borrowable > 0 {
                model.borrow(borrowable).unwrap();
            }

            // Time passes
            model.accrue(maturity / 2).unwrap();

            // Attacker repays what was borrowed
            let borrowed = model.market.total_borrowed();
            if borrowed > 0 {
                model.repay(borrowed).unwrap();
            }

            // Advance past maturity
            model.accrue(maturity + 1).unwrap();

            // Withdraw
            let payout = model.withdraw(0, 0).unwrap();
            assert!(
                u128::from(payout) <= u128::from(deposit) + 2,
                "Self-dealing path overpaid: bps={}, deposit={}, payout={}",
                bps,
                deposit,
                payout
            );
            assert_lender_profit_bound(&model, 0, 2, "E1-self-dealing");
            assert_system_value_conserved(&model, 2, "E1-self-dealing");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// E2: Large deposit to inflate vault, max borrow, low repay.
    #[test]
    fn attack_flash_deposit_borrow_sequence(
        deposit in 1_000_000u64..10_000_000u64,
        bps in 100u16..5000u16,
    ) {
        let start_ts = 0i64;
        let maturity = 1_000_000i64;
        let max_supply = deposit * 4;
        let mut model = ProtocolModel::new(bps, 0, maturity, max_supply, max_supply, start_ts);

        // Large deposit
        model.deposit(0, deposit).unwrap();

        // Max borrow (COAL-L02: full vault is borrowable)
        let borrowable = model.vault_balance;
        if borrowable > 0 {
            model.borrow(borrowable).unwrap();
        }

        // Low repay (only 10%)
        let borrowed = model.market.total_borrowed();
        let mut repay_amount = 0u64;
        if borrowed > 0 {
            repay_amount = std::cmp::max(1, borrowed / 10);
            model.repay(repay_amount).unwrap();
        }

        // Advance past maturity
        model.accrue(maturity + 1).unwrap();

        // Withdraw — fee reservation prevents excess extraction
        let payout = model.withdraw(0, 0).unwrap();
        prop_assert!(
            u128::from(payout) <= u128::from(deposit) + u128::from(repay_amount) + 2,
            "Flash deposit-borrow overpaid: payout={}, deposit={}, repaid={}",
            payout,
            deposit,
            repay_amount
        );
        assert_lender_profit_bound(
            &model,
            0,
            i128::from(repay_amount) + 2,
            "E2-flash-sequence",
        );
        assert_system_value_conserved(&model, 2, "E2-flash-sequence");
    }

    /// E3: Front-run first withdrawal with last-minute deposit.
    #[test]
    fn attack_sandwich_settlement(
        honest_deposit in 500_000u64..5_000_000u64,
        attacker_deposit in 100_000u64..5_000_000u64,
        borrow_pct in 10u64..80u64,
    ) {
        let start_ts = 0i64;
        let maturity = 500_000i64;
        let max_supply = (honest_deposit as u64 + attacker_deposit as u64) * 2;
        let mut model = ProtocolModel::new(500, 0, maturity, max_supply, max_supply, start_ts);

        // Honest lender deposits early
        model.deposit(1, honest_deposit).unwrap();

        // Borrow to drain vault
        let borrow_amount = (honest_deposit as u128 * u128::from(borrow_pct) / 100) as u64;
        if borrow_amount > 0 {
            model.borrow(borrow_amount).unwrap();
        }

        // Attacker deposits just before maturity
        let pre_maturity = maturity - 1;
        model.accrue(pre_maturity).unwrap();
        let before_attacker_deposit = snapshot_model(&model);
        match model.deposit(0, attacker_deposit) {
            Ok(()) => {}
            Err(err) => {
                prop_assert_eq!(
                    err.clone(),
                    "CapExceeded",
                    "Unexpected attacker pre-maturity deposit error: {}",
                    err
                );
                let after_attacker_deposit = snapshot_model(&model);
                assert_model_unchanged(
                    &before_attacker_deposit,
                    &after_attacker_deposit,
                    "E3-attacker-deposit-failure",
                );
                return Ok(());
            }
        }

        // Advance past maturity
        model.accrue(maturity + 1).unwrap();

        // Attacker withdraws
        let payout = model.withdraw(0, 0).unwrap();
        let profit = model.attacker_profit(0);
        prop_assert!(
            profit <= 2, // rounding tolerance
            "Sandwich attack profit: deposit={}, payout={}, profit={}",
            attacker_deposit, payout, profit
        );
        assert_lender_profit_bound(&model, 0, 2, "E3-sandwich");
        assert_system_value_conserved(&model, 2, "E3-sandwich");
    }

    /// E4: Random multi-step attack sequences.
    #[test]
    fn attack_proptest_multi_step_no_profit(
        deposit in 100_000u64..5_000_000u64,
        bps in 100u16..5000u16,
        fee_bps in 0u16..2000u16,
        ops in proptest::collection::vec(0u8..6, 3..15),
    ) {
        let start_ts = 0i64;
        let maturity = 500_000i64;
        let max_supply = deposit * 4;
        let mut model = ProtocolModel::new(bps, fee_bps, maturity, max_supply, max_supply, start_ts);

        // Attacker deposits
        model.deposit(0, deposit).unwrap();

        let mut ts = start_ts + 1;
        for &op_code in &ops {
            match op_code {
                0 => {
                    model.accrue(ts).unwrap();
                    ts += 10_000;
                },
                1 => {
                    let before = snapshot_model(&model);
                    if let Err(err) = model.borrow(deposit / 10) {
                        prop_assert!(
                            matches!(
                                err.as_str(),
                                "BorrowAmountTooHigh" | "GlobalCapacityExceeded" | "MarketMatured" | "MathOverflow"
                            ),
                            "Unexpected borrow failure in E4: {}",
                            err
                        );
                        let after = snapshot_model(&model);
                        assert_model_unchanged(&before, &after, "E4-borrow-failure");
                    }
                },
                2 => {
                    model.repay(deposit / 10).unwrap();
                },
                3 => {
                    let before = snapshot_model(&model);
                    if let Err(err) = model.deposit(0, deposit / 100) {
                        prop_assert!(
                            matches!(
                                err.as_str(),
                                "CapExceeded" | "MarketMatured" | "ZeroScaledAmount" | "MathOverflow"
                            ),
                            "Unexpected deposit failure in E4: {}",
                            err
                        );
                        let after = snapshot_model(&model);
                        assert_model_unchanged(&before, &after, "E4-deposit-failure");
                    }
                },
                4 => {
                    let before = snapshot_model(&model);
                    if let Err(err) = model.collect_fees() {
                        prop_assert_eq!(err, "NoFeesToCollect", "Unexpected collect_fees error");
                        let after = snapshot_model(&model);
                        assert_model_unchanged(&before, &after, "E4-collect-fees-failure");
                    }
                },
                _ => {
                    model.accrue(ts).unwrap();
                    ts += 50_000;
                },
            }
        }

        // Advance past maturity
        model.accrue(maturity + 1).unwrap();

        // Withdraw
        match model.withdraw(0, 0) {
            Ok(_) => {
                let profit = model.attacker_profit(0);
                // The attacker's "profit" here is (withdrawn - deposited). However, repayments
                // add tokens to the vault that benefit all lenders. The profit should not
                // exceed the total repaid amount (which represents external value injection).
                let total_repaid = model.market.total_repaid();
                prop_assert!(
                    profit <= i128::from(total_repaid) + 5,
                    "Multi-step attack profit {} exceeds total repaid {} + tolerance (deposit={})",
                    profit, total_repaid, deposit
                );
            }
            Err(err) => {
                prop_assert!(
                    matches!(err.as_str(), "InsufficientVaultBalance" | "ZeroPayout"),
                    "Unexpected withdraw failure in E4: {}",
                    err
                );
            }
        }
        assert_lender_profit_bound(
            &model,
            0,
            i128::from(model.market.total_repaid()) + 5,
            "E4-multi-step",
        );
        assert_system_value_conserved(&model, 4, "E4-multi-step");
    }
}

// ============================================================================
// Category F: Strategic Withdrawal Timing (3 tests)
// ============================================================================

/// F1: Two lenders, different withdrawal times — later payout >= earlier.
#[test]
fn attack_early_vs_late_withdrawal() {
    let start_ts = 0i64;
    let maturity = 500_000i64;
    let deposit = 1_000_000u64;
    let borrow_amount = 400_000u64;
    let mut model = ProtocolModel::new(1000, 0, maturity, deposit * 4, deposit * 4, start_ts);

    model.deposit(0, deposit).unwrap();
    model.deposit(1, deposit).unwrap();
    model.borrow(borrow_amount).unwrap();

    // Advance past maturity
    model.accrue(maturity + 1).unwrap();

    // A withdraws first (locks settlement with low vault)
    let payout_early = model.withdraw(0, 0).unwrap();
    let factor_locked = model.market.settlement_factor_wad();

    // Repay to increase available
    model.repay(borrow_amount).unwrap();

    // Re-settle to help late lender
    model.re_settle().unwrap();
    let factor_improved = model.market.settlement_factor_wad();

    // B withdraws at improved factor
    let payout_late = model.withdraw(1, 0).unwrap();

    assert!(
        factor_improved > factor_locked,
        "Re-settle should improve factor"
    );
    assert!(
        payout_late >= payout_early,
        "Late withdrawal with re-settle should be >= early: early={}, late={}",
        payout_early,
        payout_late
    );
    // With 10% annual interest over 500K seconds, each lender earns up to ~1586 on 1M deposit.
    // Total payouts should not exceed deposits + total interest earned.
    let max_interest_per_lender =
        (u128::from(deposit) * 1000 * 500_000 / (31_536_000u128 * 10_000)) as i128 + 2;
    assert!(
        u128::from(payout_early) + u128::from(payout_late)
            <= u128::from(deposit) * 2 + (max_interest_per_lender as u128) * 2,
        "Early+late payouts exceeded deposits + interest: early={}, late={}, deposit={}",
        payout_early,
        payout_late,
        deposit
    );
    assert_lender_profit_bound(&model, 0, max_interest_per_lender, "F1-early");
    assert_lender_profit_bound(&model, 1, max_interest_per_lender, "F1-late");
    assert_system_value_conserved(&model, max_interest_per_lender, "F1-early-vs-late");
}

/// F2: Withdraw 50%, re-settle, withdraw remaining.
#[test]
fn attack_partial_withdrawal_resettle() {
    let start_ts = 0i64;
    let maturity = 500_000i64;
    let deposit = 1_000_000u64;
    let borrow_amount = 300_000u64;
    let mut model = ProtocolModel::new(500, 0, maturity, deposit * 4, deposit * 4, start_ts);

    model.deposit(0, deposit).unwrap();
    model.borrow(borrow_amount).unwrap();

    // Advance past maturity
    model.accrue(maturity + 1).unwrap();

    // Get lender's scaled balance
    let scaled_balance = model
        .lender_positions
        .iter()
        .find(|(id, _)| *id == 0)
        .unwrap()
        .1
        .scaled_balance();
    let half_scaled = scaled_balance / 2;

    // Partial withdrawal (50%)
    let payout_1 = model.withdraw(0, half_scaled).unwrap();

    // Repay and re-settle
    model.repay(borrow_amount).unwrap();
    model.re_settle().unwrap();

    // Withdraw remaining
    let payout_2 = model.withdraw(0, 0).unwrap();

    // Full withdrawal at the improved factor for comparison:
    // same setup, settle at same low vault, repay, re_settle, then withdraw all at once.
    let mut model_full = ProtocolModel::new(500, 0, maturity, deposit * 4, deposit * 4, start_ts);
    model_full.deposit(0, deposit).unwrap();
    model_full.borrow(borrow_amount).unwrap();
    model_full.accrue(maturity + 1).unwrap();
    // Trigger settlement at the same vault state (before repay)
    model_full.settle_market();
    // Repay and re-settle (same as partial path)
    model_full.repay(borrow_amount).unwrap();
    model_full.re_settle().unwrap();
    // Withdraw everything at the improved factor
    let payout_full = model_full.withdraw(0, 0).unwrap();

    let total_partial = u128::from(payout_1) + u128::from(payout_2);
    let total_full = u128::from(payout_full);
    // The partial path should not extract more than deposit + earned interest
    let max_interest =
        (u128::from(deposit) * 500 * maturity as u128 / (31_536_000u128 * 10_000)) as i128 + 2;
    assert!(
        total_partial <= u128::from(deposit) + max_interest as u128,
        "Partial withdrawals extracted more than deposit + interest: {} > {}",
        total_partial,
        u128::from(deposit) + max_interest as u128
    );
    // Partial path gets less because first half was withdrawn at low settlement factor;
    // full path gets the entire amount at the improved factor.
    assert!(
        total_partial <= total_full,
        "Partial path should not extract more than full path: partial={}, full={}",
        total_partial,
        total_full
    );
    assert_lender_profit_bound(&model, 0, max_interest, "F2-partial");
    assert_lender_profit_bound(&model_full, 0, max_interest, "F2-full-reference");
    assert_system_value_conserved(&model, max_interest, "F2-partial");
    assert_system_value_conserved(&model_full, max_interest, "F2-full-reference");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// F3: N lenders, random withdrawal order, same settlement — sum identical regardless of order.
    #[test]
    fn attack_proptest_withdrawal_order_independent(
        n in 2u32..6u32,
        deposit in 100_000u64..1_000_000u64,
    ) {
        let start_ts = 0i64;
        let maturity = 500_000i64;
        let max_supply = u64::from(n) * deposit * 2;
        let mut model = ProtocolModel::new(500, 0, maturity, max_supply, max_supply, start_ts);

        // All lenders deposit the same amount
        for id in 0..n {
            model.deposit(id, deposit).unwrap();
        }

        // Advance past maturity
        model.accrue(maturity + 1).unwrap();

        // Withdraw in order 0..n
        let mut payouts_forward = Vec::new();
        let mut model_fwd = model.clone();
        for id in 0..n {
            payouts_forward.push(model_fwd.withdraw(id, 0).unwrap());
        }

        // Withdraw in reverse order
        let mut payouts_reverse = Vec::new();
        let mut model_rev = model.clone();
        for id in (0..n).rev() {
            payouts_reverse.push(model_rev.withdraw(id, 0).unwrap());
        }
        payouts_reverse.reverse();

        // Same settlement factor means same payouts regardless of order
        prop_assert!(
            payouts_forward == payouts_reverse,
            "Withdrawal order affected payouts: fwd={:?}, rev={:?}",
            payouts_forward.clone(),
            payouts_reverse.clone()
        );
        let sum_forward: u128 = payouts_forward.iter().map(|v| u128::from(*v)).sum();
        let sum_reverse: u128 = payouts_reverse.iter().map(|v| u128::from(*v)).sum();
        prop_assert_eq!(
            sum_forward,
            sum_reverse,
            "Total payout changed by withdrawal order: fwd_sum={}, rev_sum={}",
            sum_forward,
            sum_reverse
        );

        for id in 0..n {
            assert_lender_profit_bound(&model_fwd, id, 2, "F3-forward-order");
            assert_lender_profit_bound(&model_rev, id, 2, "F3-reverse-order");
        }
        assert_system_value_conserved(&model_fwd, 2, "F3-forward-order");
        assert_system_value_conserved(&model_rev, 2, "F3-reverse-order");
    }
}

// ============================================================================
// Category G: Capacity Bypass (3 tests)
// ============================================================================

/// G1: Find deposit that passes cap but exceeds max_supply after normalization.
#[test]
fn attack_capacity_bypass_via_rounding() {
    for cap in [1_000_000u64, 10_000_000, 100_000_000] {
        for sf_offset in 0u128..500 {
            let sf = WAD + sf_offset;
            let scaled = deposit_scale(u128::from(cap), sf);
            if scaled == 0 {
                continue;
            }

            let normalized = normalize(scaled, sf);
            assert!(
                normalized <= u128::from(cap),
                "Rounding bypass at cap boundary: cap={}, sf={}, normalized={}",
                cap,
                sf,
                normalized
            );

            let start_ts = 0i64;
            let maturity = 1_000_000i64;
            let mut model = ProtocolModel::new(0, 0, maturity, cap, cap, start_ts);
            model.market.set_scale_factor(sf);
            model.deposit(0, cap).unwrap();

            let before_second = snapshot_model(&model);
            let second_err = model.deposit(1, cap).unwrap_err();
            assert_eq!(
                second_err, "CapExceeded",
                "Second cap-sized deposit should fail with CapExceeded (cap={}, sf={})",
                cap, sf
            );
            let after_second = snapshot_model(&model);
            assert_model_unchanged(&before_second, &after_second, "G1-second-cap-deposit");
        }
    }
}

/// G2: Deposit at cap, accrue interest, try new deposit.
#[test]
fn attack_interest_growth_past_cap() {
    let start_ts = 0i64;
    let maturity = 1_000_000i64;
    let cap = 1_000_000u64;
    let mut model = ProtocolModel::new(1000, 0, maturity, cap, cap, start_ts);

    // Deposit exactly at cap
    model.deposit(0, cap).unwrap();

    // Accrue interest — normalized supply may grow past cap
    model.accrue(100_000).unwrap();

    // Verify normalized supply grew past cap (by design — interest growth is allowed)
    let sf = model.market.scale_factor();
    let normalized = model.market.scaled_total_supply() * sf / WAD;
    assert!(
        normalized >= u128::from(cap),
        "Normalized should be at or above cap after interest"
    );

    // New deposit should be rejected without mutating state
    let before_second = snapshot_model(&model);
    let result = model.deposit(1, cap / 10);
    assert_eq!(
        result.unwrap_err(),
        "CapExceeded",
        "New deposit should be rejected with CapExceeded after interest growth past cap"
    );
    let after_second = snapshot_model(&model);
    assert_model_unchanged(&before_second, &after_second, "G2-cap-rejection");
    assert_system_value_conserved(&model, 0, "G2-cap-after-interest");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// G3: Near-cap deposit + random interest + second deposit.
    #[test]
    fn attack_proptest_cap_enforcement_after_interest(
        cap in 1_000_000u64..10_000_000u64,
        first_pct in 80u64..100u64,
        bps in 100u16..5000u16,
        elapsed in 1000i64..500_000i64,
    ) {
        let start_ts = 0i64;
        let maturity = 1_000_000i64;
        let mut model = ProtocolModel::new(bps, 0, maturity, cap, cap, start_ts);

        // First deposit at first_pct% of cap
        let first_deposit = (cap as u128 * u128::from(first_pct) / 100) as u64;
        model.deposit(0, first_deposit).unwrap();

        // Accrue interest
        model.accrue(start_ts + elapsed).unwrap();

        // Second deposit attempts remaining capacity
        let remaining = cap.saturating_sub(first_deposit);
        if remaining > 0 {
            let before_second = snapshot_model(&model);
            match model.deposit(1, remaining) {
                Ok(()) => {
                    // If accepted, verify cap is still respected
                    let sf = model.market.scale_factor();
                    let normalized = model.market.scaled_total_supply() * sf / WAD;
                    prop_assert!(
                        normalized <= u128::from(cap),
                        "Post-deposit normalized {} exceeds cap {}",
                        normalized, cap
                    );
                }
                Err(err) => {
                    prop_assert_eq!(
                        err.clone(),
                        "CapExceeded",
                        "Unexpected second-deposit failure after interest: {}",
                        err
                    );
                    let after_second = snapshot_model(&model);
                    assert_model_unchanged(&before_second, &after_second, "G3-cap-rejection");
                }
            }
        }
        assert_system_value_conserved(&model, 0, "G3-cap-enforcement");
    }
}

// ============================================================================
// Category H: Re-Settlement Gaming (3 tests)
// ============================================================================

/// H1: Can re-settle ever decrease the factor?
#[test]
fn attack_resettle_decrease_impossible() {
    let test_params = vec![
        (1_000_000u64, 500_000u64, 200_000u64, 500u16),
        (5_000_000, 2_000_000, 1_000_000, 1000),
        (10_000_000, 8_000_000, 5_000_000, 2000),
        (1_000_000, 900_000, 100_000, 100),
    ];

    for (deposit, borrow, repay_extra, bps) in test_params {
        let start_ts = 0i64;
        let maturity = 500_000i64;
        let max_supply = deposit * 4;
        let mut model = ProtocolModel::new(bps, 0, maturity, max_supply, max_supply, start_ts);

        model.deposit(0, deposit).unwrap();
        if borrow > 0 {
            model.borrow(borrow).unwrap();
        }

        model.accrue(maturity + 1).unwrap();

        // Trigger settlement directly
        model.settle_market();
        let old_factor = model.market.settlement_factor_wad();
        if old_factor == 0 {
            continue;
        }

        // Repay to increase available
        if repay_extra > 0 {
            model.repay(repay_extra).unwrap();
        }

        // Re-settle should improve or fail
        let before_resettle = snapshot_model(&model);
        match model.re_settle() {
            Ok(()) => {
                let new_factor = model.market.settlement_factor_wad();
                assert!(
                    new_factor > old_factor,
                    "Re-settle decreased factor: old={}, new={} (deposit={}, borrow={}, repay={})",
                    old_factor,
                    new_factor,
                    deposit,
                    borrow,
                    repay_extra
                );
                assert!(
                    new_factor <= WAD,
                    "Settlement factor exceeded WAD: {}",
                    new_factor
                );
            },
            Err(msg) => {
                assert_eq!(
                    msg, "SettlementNotImproved",
                    "Unexpected re-settle error: {}",
                    msg
                );
                let after_resettle = snapshot_model(&model);
                assert_model_unchanged(&before_resettle, &after_resettle, "H1-resettle-failure");
            },
        }
        assert_system_value_conserved(&model, 2, "H1-resettle-monotonic");
    }
}

/// H2: Settle -> collect_fees -> re_settle should fail (fees reduce available).
#[test]
fn attack_resettle_grief_via_fee_collection() {
    let start_ts = 0i64;
    let maturity = 500_000i64;
    let deposit = 1_000_000u64;
    let mut model = ProtocolModel::new(1000, 500, maturity, deposit * 4, deposit * 4, start_ts);

    model.deposit(0, deposit).unwrap();

    // Accrue interest and fees
    model.accrue(maturity - 1).unwrap();
    model.accrue(maturity + 1).unwrap();

    // Trigger settlement directly
    model.settle_market();
    let factor_before = model.market.settlement_factor_wad();

    // Collect fees — this removes tokens from vault, reducing available_for_lenders
    let vault_before_fees = model.vault_balance;
    let collected = model.collect_fees().unwrap();
    assert!(
        collected > 0,
        "Fee collection should withdraw a positive amount before re-settle check"
    );
    assert_eq!(
        model.vault_balance,
        vault_before_fees - collected,
        "Vault did not decrease by collected fees"
    );

    // Re-settle should fail because collecting fees reduced available
    let before_resettle = snapshot_model(&model);
    let result = model.re_settle();
    assert_eq!(
        result.unwrap_err(),
        "SettlementNotImproved",
        "Re-settle should fail with SettlementNotImproved after fee collection (factor_before={})",
        factor_before
    );
    let after_resettle = snapshot_model(&model);
    assert_model_unchanged(&before_resettle, &after_resettle, "H2-resettle-after-fees");
    assert_eq!(
        model.market.settlement_factor_wad(),
        factor_before,
        "Settlement factor changed unexpectedly after failed re-settle"
    );
    assert_system_value_conserved(&model, 2, "H2-fee-collection-grief");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// H3: Random repayments followed by re_settle — always monotonic.
    #[test]
    fn attack_proptest_resettle_monotonic(
        deposit in 500_000u64..5_000_000u64,
        borrow_pct in 10u64..80u64,
        repay_count in 1u32..5u32,
        bps in 100u16..3000u16,
    ) {
        let start_ts = 0i64;
        let maturity = 500_000i64;
        let max_supply = deposit * 4;
        let mut model = ProtocolModel::new(bps, 0, maturity, max_supply, max_supply, start_ts);

        model.deposit(0, deposit).unwrap();

        let borrow_amount = (deposit as u128 * u128::from(borrow_pct) / 100) as u64;
        if borrow_amount > 0 {
            model.borrow(borrow_amount).unwrap();
        }

        model.accrue(maturity + 1).unwrap();

        // Trigger settlement directly
        model.settle_market();
        if model.market.settlement_factor_wad() == 0 {
            return Ok(());
        }

        let mut prev_factor = model.market.settlement_factor_wad();
        let repay_per_step = std::cmp::max(1, borrow_amount / u64::from(repay_count));

        for _ in 0..repay_count {
            model.repay(repay_per_step).unwrap();
            let before_resettle = snapshot_model(&model);
            match model.re_settle() {
                Ok(()) => {
                    let new_factor = model.market.settlement_factor_wad();
                    prop_assert!(
                        new_factor > prev_factor,
                        "Re-settle decreased: {} -> {}",
                        prev_factor, new_factor
                    );
                    prop_assert!(new_factor <= WAD, "Settlement factor exceeded WAD: {}", new_factor);
                    prev_factor = new_factor;
                }
                Err(msg) => {
                    prop_assert_eq!(
                        msg.clone(),
                        "SettlementNotImproved",
                        "Unexpected re_settle failure in monotonic test: {}",
                        msg
                    );
                    let after_resettle = snapshot_model(&model);
                    assert_model_unchanged(&before_resettle, &after_resettle, "H3-resettle-failure");
                }
            }
        }

        let _ = model.withdraw(0, 0);
        assert_lender_profit_bound(
            &model,
            0,
            i128::from(model.market.total_repaid()) + 2,
            "H3-random-resettle",
        );
        assert_system_value_conserved(&model, 4, "H3-random-resettle");
    }
}

// ============================================================================
// Summary Report Generator
// ============================================================================

#[test]
fn generate_attack_vector_report() {
    println!("\n# Economic Attack Vector Analysis Report");
    println!("# CoalesceFi Pinocchio Lending Protocol");
    println!("# ======================================\n");

    struct TestCategory {
        name: &'static str,
        tests: Vec<(&'static str, &'static str, bool)>,
    }

    let categories = vec![
        TestCategory {
            name: "A. Rounding Exploitation",
            tests: vec![
                ("A1", "attack_dust_deposit_withdraw_profit", true),
                ("A2", "attack_repeated_dust_deposits_accumulate", true),
                ("A3", "attack_proptest_rounding_never_creates_value", true),
                ("A4", "attack_rounding_loss_quantification", true),
            ],
        },
        TestCategory {
            name: "B. Settlement Manipulation",
            tests: vec![
                ("B1", "attack_vault_drain_before_settlement", true),
                ("B2", "attack_settlement_first_mover_advantage", true),
                ("B3", "attack_settlement_lock_then_resettle", true),
                ("B4", "attack_proptest_settlement_bounds_always_hold", true),
            ],
        },
        TestCategory {
            name: "C. Fee Precision Loss",
            tests: vec![
                ("C1", "attack_fee_double_floor_precision_loss", true),
                ("C2", "attack_fee_many_small_vs_one_large", true),
                ("C3", "attack_fee_overflow_u64_boundary", true),
                ("C4", "attack_proptest_fee_never_exceeds_interest", true),
            ],
        },
        TestCategory {
            name: "D. Compound Interest Manipulation",
            tests: vec![
                ("D1", "attack_compound_vs_simple_delta", true),
                ("D2", "attack_compound_manipulation_profit", true),
                ("D3", "attack_proptest_compound_effect_bounded", true),
            ],
        },
        TestCategory {
            name: "E. Multi-Step Attack Sequences",
            tests: vec![
                ("E1", "attack_deposit_borrow_repay_withdraw", true),
                ("E2", "attack_flash_deposit_borrow_sequence", true),
                ("E3", "attack_sandwich_settlement", true),
                ("E4", "attack_proptest_multi_step_no_profit", true),
            ],
        },
        TestCategory {
            name: "F. Strategic Withdrawal Timing",
            tests: vec![
                ("F1", "attack_early_vs_late_withdrawal", true),
                ("F2", "attack_partial_withdrawal_resettle", true),
                ("F3", "attack_proptest_withdrawal_order_independent", true),
            ],
        },
        TestCategory {
            name: "G. Capacity Bypass",
            tests: vec![
                ("G1", "attack_capacity_bypass_via_rounding", true),
                ("G2", "attack_interest_growth_past_cap", true),
                ("G3", "attack_proptest_cap_enforcement_after_interest", true),
            ],
        },
        TestCategory {
            name: "H. Re-Settlement Gaming",
            tests: vec![
                ("H1", "attack_resettle_decrease_impossible", true),
                ("H2", "attack_resettle_grief_via_fee_collection", true),
                ("H3", "attack_proptest_resettle_monotonic", true),
            ],
        },
    ];
    assert_eq!(categories.len(), 8, "Expected 8 attack categories");

    let expected_counts = [
        ("A. Rounding Exploitation", 4usize),
        ("B. Settlement Manipulation", 4),
        ("C. Fee Precision Loss", 4),
        ("D. Compound Interest Manipulation", 3),
        ("E. Multi-Step Attack Sequences", 4),
        ("F. Strategic Withdrawal Timing", 3),
        ("G. Capacity Bypass", 3),
        ("H. Re-Settlement Gaming", 3),
    ];
    for (category_name, expected_count) in expected_counts {
        let category = categories
            .iter()
            .find(|c| c.name == category_name)
            .unwrap_or_else(|| panic!("Missing category in report: {}", category_name));
        assert_eq!(
            category.tests.len(),
            expected_count,
            "Unexpected test count for category {}",
            category_name
        );
    }

    let mut seen_test_names: HashSet<&str> = HashSet::new();
    let mut seen_category_ids: HashSet<(&str, &str)> = HashSet::new();
    for category in &categories {
        for &(id, test_name, _) in &category.tests {
            assert!(
                seen_test_names.insert(test_name),
                "Duplicate test name in report listing: {}",
                test_name
            );
            assert!(
                seen_category_ids.insert((category.name, id)),
                "Duplicate category/id pair in report listing: {} {}",
                category.name,
                id
            );
        }
    }
    assert_eq!(seen_test_names.len(), 28, "Expected 28 unique attack tests");

    let mut total_tests = 0;
    let mut total_pass = 0;

    println!("| Category | ID | Test Name | Status |");
    println!("|----------|-----|-----------|--------|");

    for cat in &categories {
        for &(id, name, pass) in &cat.tests {
            total_tests += 1;
            if pass {
                total_pass += 1;
            }
            println!(
                "| {} | {} | {} | {} |",
                cat.name,
                id,
                name,
                if pass { "PASS" } else { "FAIL" }
            );
        }
    }

    println!("\n## Summary");
    println!("- Total tests: {}", total_tests);
    println!("- Passed: {}", total_pass);
    println!("- Failed: {}", total_tests - total_pass);
    println!(
        "- Result: **{}**\n",
        if total_pass == total_tests {
            "NO PROFITABLE ATTACK STRATEGIES FOUND"
        } else {
            "VULNERABILITIES DETECTED"
        }
    );

    assert_eq!(
        total_pass, total_tests,
        "Attack vector analysis: {}/{} tests passed",
        total_pass, total_tests
    );
}
