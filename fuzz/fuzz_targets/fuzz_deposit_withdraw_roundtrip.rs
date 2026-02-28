//! Fuzz target: deposit(X) then normalize back yields close to X.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use coalesce::constants::WAD;

#[derive(Debug, Arbitrary)]
struct Input {
    amount: u64,
    sf_offset: u64, // offset above WAD for scale_factor
}

fuzz_target!(|input: Input| {
    if input.amount == 0 {
        return;
    }

    // scale_factor in [WAD, WAD + sf_offset], capped to avoid overflow
    let sf_offset = (input.sf_offset as u128).min(WAD);
    let scale_factor = WAD + sf_offset;

    let amount_u128 = input.amount as u128;

    // Deposit scaling: scaled = amount * WAD / scale_factor
    let scaled = match amount_u128.checked_mul(WAD).and_then(|n| n.checked_div(scale_factor)) {
        Some(s) if s > 0 => s,
        _ => return,
    };

    // Normalize back: recovered = scaled * scale_factor / WAD
    let recovered = match scaled.checked_mul(scale_factor).and_then(|n| n.checked_div(WAD)) {
        Some(r) => r,
        None => return,
    };

    // Invariant: recovered <= original (rounding down)
    assert!(
        recovered <= amount_u128,
        "recovered ({}) > original ({})",
        recovered,
        amount_u128
    );

    // Rounding loss is at most 1 in normalized (token-unit) space.
    // The WAD factor (1e18) provides 18 digits of fractional precision in the
    // scaled representation, so the single floor-division in normalize() can
    // lose at most 1 unit of the original token amount.
    let loss = amount_u128 - recovered;
    assert!(
        loss <= 1,
        "rounding loss too large: {}, amount={}, sf={}",
        loss,
        amount_u128,
        scale_factor
    );
});
