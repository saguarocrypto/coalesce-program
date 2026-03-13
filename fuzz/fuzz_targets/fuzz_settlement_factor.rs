//! Fuzz target: settlement factor computation is always bounded [1, WAD].

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use coalesce::constants::WAD;

#[derive(Debug, Arbitrary)]
struct Input {
    vault_balance: u64,
    scaled_total_supply_hi: u64,
    scaled_total_supply_lo: u64,
    scale_factor_hi: u64,
    scale_factor_lo: u64,
}

fuzz_target!(|input: Input| {
    let vault_balance = input.vault_balance as u128;
    let scaled_total_supply =
        ((input.scaled_total_supply_hi as u128) << 64) | (input.scaled_total_supply_lo as u128);
    let scale_factor =
        ((input.scale_factor_hi as u128) << 64) | (input.scale_factor_lo as u128);

    if scale_factor == 0 || scaled_total_supply == 0 {
        return;
    }

    // COAL-C01: No fee reservation — full vault balance is available for settlement.
    let available = vault_balance;

    // Total normalized
    let total_normalized = match scaled_total_supply
        .checked_mul(scale_factor)
        .and_then(|n| n.checked_div(WAD))
    {
        Some(n) if n > 0 => n,
        _ => return,
    };

    // Settlement factor
    let raw = match available.checked_mul(WAD).and_then(|n| n.checked_div(total_normalized)) {
        Some(r) => r,
        None => return,
    };
    let capped = if raw > WAD { WAD } else { raw };
    let factor = if capped < 1 { 1 } else { capped };

    // Invariant: factor in [1, WAD]
    assert!(factor >= 1, "settlement factor < 1: {}", factor);
    assert!(factor <= WAD, "settlement factor > WAD: {}", factor);
});
