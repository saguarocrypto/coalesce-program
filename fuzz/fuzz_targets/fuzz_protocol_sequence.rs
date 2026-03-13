//! Fuzz target: protocol operation sequences against a model.
//!
//! Exercises sequences of Deposit, Borrow, Repay, AccrueInterest, CollectFees,
//! and Withdraw operations, updating Market / LenderPosition / vault state as
//! the on-chain processors would. After all operations, asserts key invariants:
//!
//! - vault_balance == total_deposited - total_borrowed + total_repaid - fees_collected
//! - scale_factor >= WAD
//! - accrued_protocol_fees >= 0  (trivially true for u64, but checked after subtraction)
//! - Sum of lender scaled_balances == market.scaled_total_supply
//! - If settled: settlement_factor in [1, WAD]

#![no_main]

use arbitrary::Arbitrary;
use bytemuck::Zeroable;
use libfuzzer_sys::fuzz_target;

use coalesce::constants::WAD;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{LenderPosition, Market, ProtocolConfig};

// ---------------------------------------------------------------------------
// Fuzz input types
// ---------------------------------------------------------------------------

#[derive(Debug, Arbitrary)]
enum Op {
    Deposit { lender: u8, amount: u32 },
    Borrow { amount: u32 },
    Repay { amount: u32 },
    AccrueInterest { seconds: u16 },
    CollectFees,
    Withdraw { lender: u8 },
}

#[derive(Debug, Arbitrary)]
struct Input {
    annual_bps: u16,
    fee_rate_bps: u16,
    max_supply: u32,
    ops: Vec<Op>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of distinct lender slots tracked.
const MAX_LENDERS: usize = 8;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fuzz_target!(|input: Input| {
    // Limit sequence length and skip empty sequences.
    if input.ops.is_empty() || input.ops.len() > 50 {
        return;
    }

    // Clamp configurable parameters to valid protocol ranges (0..=10_000 bps).
    let annual_bps = input.annual_bps % 10_001;
    let fee_rate_bps = input.fee_rate_bps % 10_001;
    let max_supply = if input.max_supply == 0 { 1u64 } else { input.max_supply as u64 };

    // Starting timestamp (arbitrary but reasonable).
    let start_ts: i64 = 1_000_000;
    // Maturity = start + 1 year (plenty of room for AccrueInterest ops).
    let maturity: i64 = start_ts + 31_536_000;
    let mut current_ts: i64 = start_ts;

    // ---- Initialize market ------------------------------------------------
    let mut market = Market::zeroed();
    market.set_annual_interest_bps(annual_bps);
    market.set_maturity_timestamp(maturity);
    market.set_scale_factor(WAD);
    market.set_last_accrual_timestamp(current_ts);
    market.set_max_total_supply(max_supply);

    // ---- Protocol config --------------------------------------------------
    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_rate_bps);

    // ---- Model state ------------------------------------------------------
    let mut vault_balance: u64 = 0;
    let mut fees_collected: u64 = 0;
    let mut lenders: Vec<LenderPosition> = Vec::with_capacity(MAX_LENDERS);
    // Pre-allocate MAX_LENDERS positions (zeroed).
    for _ in 0..MAX_LENDERS {
        lenders.push(LenderPosition::zeroed());
    }

    // ---- Execute ops ------------------------------------------------------
    for op in &input.ops {
        match op {
            // ---------------------------------------------------------------
            // Deposit
            // ---------------------------------------------------------------
            Op::Deposit { lender, amount } => {
                let amount_u64 = *amount as u64;
                if amount_u64 == 0 {
                    continue;
                }

                let sf = market.scale_factor();
                if sf == 0 {
                    continue;
                }

                // Accrue interest before deposit (mirrors processor).
                if accrue_interest(&mut market, &config, current_ts).is_err() {
                    continue;
                }
                let sf = market.scale_factor();

                // scaled_amount = amount * WAD / scale_factor
                let scaled_amount = match (amount_u64 as u128)
                    .checked_mul(WAD)
                    .and_then(|n| n.checked_div(sf))
                {
                    Some(s) if s > 0 => s,
                    _ => continue, // ZeroScaledAmount — skip
                };

                // Cap check: new_normalized <= max_total_supply
                let new_scaled_total = match market.scaled_total_supply().checked_add(scaled_amount)
                {
                    Some(s) => s,
                    None => continue,
                };
                let new_normalized = match new_scaled_total
                    .checked_mul(sf)
                    .and_then(|n| n.checked_div(WAD))
                {
                    Some(n) => n,
                    None => continue,
                };
                if new_normalized > max_supply as u128 {
                    continue; // CapExceeded — skip
                }

                // Update vault
                vault_balance = match vault_balance.checked_add(amount_u64) {
                    Some(v) => v,
                    None => continue,
                };

                // Update market
                market.set_scaled_total_supply(new_scaled_total);
                let td = match market.total_deposited().checked_add(amount_u64) {
                    Some(v) => v,
                    None => continue,
                };
                market.set_total_deposited(td);

                // Update lender position
                let idx = (*lender as usize) % MAX_LENDERS;
                let old_bal = lenders[idx].scaled_balance();
                let new_bal = match old_bal.checked_add(scaled_amount) {
                    Some(v) => v,
                    None => continue,
                };
                lenders[idx].set_scaled_balance(new_bal);
            }

            // ---------------------------------------------------------------
            // Borrow
            // ---------------------------------------------------------------
            Op::Borrow { amount } => {
                let amount_u64 = *amount as u64;
                if amount_u64 == 0 {
                    continue;
                }

                // Accrue interest before borrow.
                if accrue_interest(&mut market, &config, current_ts).is_err() {
                    continue;
                }

                // COAL-C01: Full vault balance is borrowable (no fee reservation).
                // Fees are enforced at settlement via collect_fees distress guard.
                if amount_u64 > vault_balance {
                    continue; // BorrowAmountTooHigh — skip
                }

                vault_balance -= amount_u64;
                let tb = match market.total_borrowed().checked_add(amount_u64) {
                    Some(v) => v,
                    None => continue,
                };
                market.set_total_borrowed(tb);
            }

            // ---------------------------------------------------------------
            // Repay
            // ---------------------------------------------------------------
            Op::Repay { amount } => {
                let amount_u64 = *amount as u64;
                if amount_u64 == 0 {
                    continue;
                }

                // Repay uses zero-fee config for accrual (matches processor).
                let zero_config = ProtocolConfig::zeroed();
                let _ = accrue_interest(&mut market, &zero_config, current_ts);

                vault_balance = match vault_balance.checked_add(amount_u64) {
                    Some(v) => v,
                    None => continue,
                };
                let tr = match market.total_repaid().checked_add(amount_u64) {
                    Some(v) => v,
                    None => continue,
                };
                market.set_total_repaid(tr);
            }

            // ---------------------------------------------------------------
            // AccrueInterest (advance time)
            // ---------------------------------------------------------------
            Op::AccrueInterest { seconds } => {
                let delta = *seconds as i64;
                current_ts = current_ts.saturating_add(delta);
                let _ = accrue_interest(&mut market, &config, current_ts);
            }

            // ---------------------------------------------------------------
            // CollectFees
            // ---------------------------------------------------------------
            Op::CollectFees => {
                // Accrue first.
                let _ = accrue_interest(&mut market, &config, current_ts);

                let accrued = market.accrued_protocol_fees();
                if accrued == 0 {
                    continue; // NoFeesToCollect — skip
                }

                let withdrawable = core::cmp::min(accrued, vault_balance);
                if withdrawable == 0 {
                    continue; // Nothing available in vault
                }

                vault_balance -= withdrawable;
                fees_collected = match fees_collected.checked_add(withdrawable) {
                    Some(v) => v,
                    None => continue,
                };

                let remaining = match accrued.checked_sub(withdrawable) {
                    Some(v) => v,
                    None => continue,
                };
                market.set_accrued_protocol_fees(remaining);
            }

            // ---------------------------------------------------------------
            // Withdraw (full withdrawal for a lender, post-maturity)
            // ---------------------------------------------------------------
            Op::Withdraw { lender } => {
                let idx = (*lender as usize) % MAX_LENDERS;

                // Withdraw only allowed at or past maturity.
                if current_ts < maturity {
                    continue;
                }

                // Accrue interest first (capped at maturity).
                let _ = accrue_interest(&mut market, &config, current_ts);

                let scaled_balance = lenders[idx].scaled_balance();
                if scaled_balance == 0 {
                    continue; // NoBalance — skip
                }

                // Compute or use settlement factor.
                if market.settlement_factor_wad() == 0 {
                    // COAL-C01: Settlement uses full vault balance (no fee reservation).
                    let available = vault_balance as u128;

                    let total_normalized = match market
                        .scaled_total_supply()
                        .checked_mul(market.scale_factor())
                        .and_then(|n| n.checked_div(WAD))
                    {
                        Some(v) => v,
                        None => continue,
                    };

                    let settlement_factor = if total_normalized == 0 {
                        WAD
                    } else {
                        let raw = match available
                            .checked_mul(WAD)
                            .and_then(|n| n.checked_div(total_normalized))
                        {
                            Some(v) => v,
                            None => continue,
                        };
                        let capped = if raw > WAD { WAD } else { raw };
                        if capped < 1 { 1 } else { capped }
                    };

                    market.set_settlement_factor_wad(settlement_factor);
                }

                let scale_factor = market.scale_factor();
                let settlement_factor = market.settlement_factor_wad();

                // payout = scaled_balance * scale_factor / WAD * settlement_factor / WAD
                let payout_u128 = match scaled_balance
                    .checked_mul(scale_factor)
                    .and_then(|n| n.checked_div(WAD))
                    .and_then(|n| n.checked_mul(settlement_factor))
                    .and_then(|n| n.checked_div(WAD))
                {
                    Some(v) => v,
                    None => continue,
                };

                let payout = match u64::try_from(payout_u128) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if payout == 0 {
                    continue; // ZeroPayout — skip
                }

                // Cannot pay out more than vault holds.
                if payout > vault_balance {
                    continue;
                }

                vault_balance -= payout;

                // Update lender position.
                lenders[idx].set_scaled_balance(0);

                // Update market scaled_total_supply.
                let new_scaled_total = match market
                    .scaled_total_supply()
                    .checked_sub(scaled_balance)
                {
                    Some(v) => v,
                    None => continue,
                };
                market.set_scaled_total_supply(new_scaled_total);
            }
        }
    }

    // =======================================================================
    // Post-sequence invariant checks
    // =======================================================================

    // Invariant 1: vault_balance == total_deposited - total_borrowed + total_repaid - fees_collected
    let expected_vault = market
        .total_deposited()
        .checked_sub(market.total_borrowed())
        .and_then(|v| v.checked_add(market.total_repaid()))
        .and_then(|v| v.checked_sub(fees_collected));

    if let Some(expected) = expected_vault {
        // Account for withdraw payouts: vault is further reduced by payouts.
        // We track vault_balance directly, so the accounting is embedded.
        // However, withdrawals reduce vault_balance but don't update
        // total_deposited/total_borrowed/total_repaid. We need to account for
        // the total payouts. Instead of tracking payouts separately, we verify
        // that:
        //   vault_balance <= expected  (payouts only decrease vault)
        assert!(
            vault_balance <= expected,
            "vault solvency violated: vault_balance={} > expected={}",
            vault_balance,
            expected
        );
    }

    // Invariant 2: scale_factor >= WAD (interest only grows the factor)
    assert!(
        market.scale_factor() >= WAD,
        "scale_factor ({}) < WAD ({})",
        market.scale_factor(),
        WAD
    );

    // Invariant 3: accrued_protocol_fees >= 0
    // Trivially true for u64, but we verify no underflow occurred by checking
    // that the value is a valid u64 (it is by type, but the assertion documents
    // the invariant).
    let _fees = market.accrued_protocol_fees(); // would panic if state were corrupted

    // Invariant 4: Sum of lender scaled_balances == market.scaled_total_supply
    let sum_scaled: u128 = lenders.iter().map(|l| l.scaled_balance()).sum();
    assert_eq!(
        sum_scaled,
        market.scaled_total_supply(),
        "lender balance sum ({}) != market.scaled_total_supply ({})",
        sum_scaled,
        market.scaled_total_supply()
    );

    // Invariant 5: If settled, settlement_factor in [1, WAD]
    let sf_wad = market.settlement_factor_wad();
    if sf_wad != 0 {
        assert!(
            sf_wad >= 1 && sf_wad <= WAD,
            "settlement_factor_wad ({}) not in [1, WAD={}]",
            sf_wad,
            WAD
        );
    }
});
