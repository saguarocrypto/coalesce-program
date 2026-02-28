//! Fuzz target: vault solvency invariant.
//! After any sequence of deposit/borrow/repay operations,
//! the vault balance should equal deposits - borrows + repays.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
enum Op {
    Deposit(u32),   // amount in USDC base units
    Borrow(u32),
    Repay(u32),
}

#[derive(Debug, Arbitrary)]
struct Input {
    ops: Vec<Op>,
}

fuzz_target!(|input: Input| {
    if input.ops.is_empty() || input.ops.len() > 100 {
        return;
    }

    let mut vault_balance: u64 = 0;
    let mut total_deposited: u64 = 0;
    let mut total_borrowed: u64 = 0;
    let mut total_repaid: u64 = 0;

    for op in &input.ops {
        match op {
            Op::Deposit(amount) => {
                let amount = *amount as u64;
                if amount == 0 {
                    continue;
                }
                vault_balance = match vault_balance.checked_add(amount) {
                    Some(v) => v,
                    None => return, // overflow, stop
                };
                total_deposited = match total_deposited.checked_add(amount) {
                    Some(v) => v,
                    None => return,
                };
            }
            Op::Borrow(amount) => {
                let amount = *amount as u64;
                if amount == 0 || amount > vault_balance {
                    continue; // skip invalid borrows
                }
                vault_balance -= amount;
                total_borrowed = match total_borrowed.checked_add(amount) {
                    Some(v) => v,
                    None => return,
                };
            }
            Op::Repay(amount) => {
                let amount = *amount as u64;
                if amount == 0 {
                    continue;
                }
                vault_balance = match vault_balance.checked_add(amount) {
                    Some(v) => v,
                    None => return,
                };
                total_repaid = match total_repaid.checked_add(amount) {
                    Some(v) => v,
                    None => return,
                };
            }
        }
    }

    // Invariant: vault_balance == total_deposited - total_borrowed + total_repaid
    let expected = total_deposited
        .checked_sub(total_borrowed)
        .and_then(|v| v.checked_add(total_repaid));

    if let Some(expected) = expected {
        assert_eq!(
            vault_balance, expected,
            "vault solvency violated: balance={}, expected={}",
            vault_balance, expected
        );
    }
});
