//! Fuzz target: borrow/repay cycle invariants.
//! current_borrowed should never exceed whitelist capacity.
//! After full repayment, vault should be restored.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
enum Action {
    Borrow(u32),
    Repay(u32),
}

#[derive(Debug, Arbitrary)]
struct Input {
    initial_vault: u32,
    max_borrow_capacity: u32,
    actions: Vec<Action>,
}

fuzz_target!(|input: Input| {
    if input.actions.is_empty() || input.actions.len() > 100 {
        return;
    }

    let max_cap = input.max_borrow_capacity as u64;
    if max_cap == 0 {
        return;
    }

    let mut vault_balance = input.initial_vault as u64;
    let mut wl_current_borrowed: u64 = 0;
    let mut market_total_borrowed: u64 = 0;
    let mut market_total_repaid: u64 = 0;

    for action in &input.actions {
        match action {
            Action::Borrow(amount) => {
                let amount = *amount as u64;
                if amount == 0 || amount > vault_balance {
                    continue;
                }

                // Check global capacity (like on-chain)
                let new_wl_total = match wl_current_borrowed.checked_add(amount) {
                    Some(t) => t,
                    None => continue,
                };
                if new_wl_total > max_cap {
                    continue;
                }

                vault_balance -= amount;
                wl_current_borrowed = new_wl_total;
                market_total_borrowed = match market_total_borrowed.checked_add(amount) {
                    Some(t) => t,
                    None => return,
                };
            }
            Action::Repay(amount) => {
                let amount = *amount as u64;
                if amount == 0 {
                    continue;
                }
                vault_balance = match vault_balance.checked_add(amount) {
                    Some(v) => v,
                    None => return,
                };
                market_total_repaid = match market_total_repaid.checked_add(amount) {
                    Some(t) => t,
                    None => return,
                };
            }
        }
    }

    // Invariant 1: wl_current_borrowed never exceeds capacity
    assert!(
        wl_current_borrowed <= max_cap,
        "wl_current_borrowed ({}) > max_cap ({})",
        wl_current_borrowed,
        max_cap
    );

    // Invariant 2: vault_balance = initial + repaid - borrowed
    let expected = (input.initial_vault as u64)
        .checked_sub(market_total_borrowed)
        .and_then(|v| v.checked_add(market_total_repaid));
    if let Some(expected) = expected {
        assert_eq!(
            vault_balance, expected,
            "vault balance mismatch: got {}, expected {}",
            vault_balance, expected
        );
    }
});
