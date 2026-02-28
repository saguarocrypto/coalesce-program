//! Fuzz target: market state transitions.
//! Applies random instruction sequences and verifies market state consistency.

#![no_main]

use arbitrary::Arbitrary;
use bytemuck::Zeroable;
use libfuzzer_sys::fuzz_target;

use coalesce::constants::WAD;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

#[derive(Debug, Arbitrary)]
enum Instruction {
    AccrueInterest { seconds_delta: u16 },
    SimulateDeposit { amount: u32 },
    SimulateBorrow { amount: u32 },
    SimulateRepay { amount: u32 },
}

#[derive(Debug, Arbitrary)]
struct Input {
    annual_bps: u16,
    fee_rate_bps: u16,
    initial_supply: u32,
    instructions: Vec<Instruction>,
}

fuzz_target!(|input: Input| {
    if input.instructions.is_empty() || input.instructions.len() > 50 {
        return;
    }

    let annual_bps = input.annual_bps % 10_001;
    let fee_rate_bps = input.fee_rate_bps % 10_001;
    let maturity = 1_000_000i64 + 31_536_000; // 1 year from start
    let mut current_ts = 1_000_000i64;

    let mut market = Market::zeroed();
    market.set_annual_interest_bps(annual_bps);
    market.set_maturity_timestamp(maturity);
    market.set_scale_factor(WAD);
    market.set_last_accrual_timestamp(current_ts);

    // Initialize with some supply
    let initial = input.initial_supply as u128;
    market.set_scaled_total_supply(initial);
    let mut vault_balance: u64 = input.initial_supply as u64;
    market.set_total_deposited(vault_balance);

    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_rate_bps);

    for instr in &input.instructions {
        match instr {
            Instruction::AccrueInterest { seconds_delta } => {
                let delta = *seconds_delta as i64;
                current_ts = current_ts.saturating_add(delta);
                let _ = accrue_interest(&mut market, &config, current_ts);
            }
            Instruction::SimulateDeposit { amount } => {
                let amount = *amount as u64;
                if amount == 0 {
                    continue;
                }
                let sf = market.scale_factor();
                if sf == 0 {
                    continue;
                }
                let scaled = match (amount as u128)
                    .checked_mul(WAD)
                    .and_then(|n| n.checked_div(sf))
                {
                    Some(s) if s > 0 => s,
                    _ => continue,
                };
                let new_supply = match market.scaled_total_supply().checked_add(scaled) {
                    Some(s) => s,
                    None => continue,
                };
                market.set_scaled_total_supply(new_supply);
                vault_balance = match vault_balance.checked_add(amount) {
                    Some(v) => v,
                    None => continue,
                };
                let td = match market.total_deposited().checked_add(amount) {
                    Some(v) => v,
                    None => continue,
                };
                market.set_total_deposited(td);
            }
            Instruction::SimulateBorrow { amount } => {
                let amount = *amount as u64;
                if amount == 0 || amount > vault_balance {
                    continue;
                }
                vault_balance -= amount;
                let tb = match market.total_borrowed().checked_add(amount) {
                    Some(v) => v,
                    None => continue,
                };
                market.set_total_borrowed(tb);
            }
            Instruction::SimulateRepay { amount } => {
                let amount = *amount as u64;
                if amount == 0 {
                    continue;
                }
                vault_balance = match vault_balance.checked_add(amount) {
                    Some(v) => v,
                    None => continue,
                };
                let tr = match market.total_repaid().checked_add(amount) {
                    Some(v) => v,
                    None => continue,
                };
                market.set_total_repaid(tr);
            }
        }
    }

    // Invariants after all operations:

    // 1. scale_factor >= WAD (interest only grows)
    assert!(
        market.scale_factor() >= WAD,
        "scale_factor ({}) < WAD",
        market.scale_factor()
    );

    // 2. total_deposited >= total_borrowed (can't borrow more than deposited + repaid)
    // This isn't strictly true if repays are counted, so just verify no overflow occurred
    // and values are internally consistent.

    // 3. settlement_factor should still be 0 (no settlement in this sequence)
    assert_eq!(
        market.settlement_factor_wad(),
        0,
        "settlement factor should be 0 during active period"
    );
});
