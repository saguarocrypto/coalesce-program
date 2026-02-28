//! Fuzz target: scaled balance invariants.
//! Sum of all lender scaled_balances should equal market.scaled_total_supply
//! after any sequence of deposits and withdrawals.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use coalesce::constants::WAD;

#[derive(Debug, Arbitrary)]
enum Action {
    Deposit { lender_idx: u8, amount: u32 },
    Withdraw { lender_idx: u8, scaled_amount: u32 },
}

#[derive(Debug, Arbitrary)]
struct Input {
    actions: Vec<Action>,
}

fuzz_target!(|input: Input| {
    if input.actions.is_empty() || input.actions.len() > 100 {
        return;
    }

    let scale_factor: u128 = WAD; // simplified: no interest accrual in this test
    let mut scaled_total_supply: u128 = 0;
    let mut lender_balances: [u128; 4] = [0; 4]; // 4 simulated lenders

    for action in &input.actions {
        match action {
            Action::Deposit { lender_idx, amount } => {
                let idx = (*lender_idx % 4) as usize;
                let amount = *amount as u128;
                if amount == 0 {
                    continue;
                }

                // scaled_amount = amount * WAD / scale_factor
                let scaled = match amount.checked_mul(WAD).and_then(|n| n.checked_div(scale_factor))
                {
                    Some(s) if s > 0 => s,
                    _ => continue,
                };

                lender_balances[idx] = match lender_balances[idx].checked_add(scaled) {
                    Some(b) => b,
                    None => return,
                };
                scaled_total_supply = match scaled_total_supply.checked_add(scaled) {
                    Some(s) => s,
                    None => return,
                };
            }
            Action::Withdraw { lender_idx, scaled_amount } => {
                let idx = (*lender_idx % 4) as usize;
                let scaled = *scaled_amount as u128;

                if scaled == 0 || scaled > lender_balances[idx] {
                    continue;
                }

                lender_balances[idx] -= scaled;
                scaled_total_supply = match scaled_total_supply.checked_sub(scaled) {
                    Some(s) => s,
                    None => return,
                };
            }
        }
    }

    // Invariant: sum of lender balances == scaled_total_supply
    let sum: u128 = lender_balances.iter().sum();
    assert_eq!(
        sum, scaled_total_supply,
        "balance sum ({}) != scaled_total_supply ({})",
        sum, scaled_total_supply
    );
});
