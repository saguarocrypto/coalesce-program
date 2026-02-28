//! Fuzz target: market state consistency after interest accrual sequences.
//! Verifies that repeated accruals with varying timestamps maintain
//! all state invariants.

#![no_main]

use arbitrary::Arbitrary;
use bytemuck::Zeroable;
use libfuzzer_sys::fuzz_target;

use coalesce::constants::WAD;
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

#[derive(Debug, Arbitrary)]
struct Input {
    annual_bps: u16,
    fee_rate_bps: u16,
    supply: u32,
    timestamps: Vec<u16>, // deltas to add sequentially
}

fuzz_target!(|input: Input| {
    if input.timestamps.is_empty() || input.timestamps.len() > 50 {
        return;
    }

    let annual_bps = input.annual_bps % 10_001;
    let fee_rate_bps = input.fee_rate_bps % 10_001;
    let supply = input.supply as u128;

    if supply == 0 {
        return;
    }

    let mut market = Market::zeroed();
    market.set_annual_interest_bps(annual_bps);
    market.set_maturity_timestamp(i64::MAX); // never matures
    market.set_scale_factor(WAD);
    market.set_scaled_total_supply(supply);
    market.set_last_accrual_timestamp(1_000_000);
    market.set_accrued_protocol_fees(0);

    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_rate_bps);

    let mut current_ts = 1_000_000i64;
    let mut prev_scale_factor = WAD;
    let mut prev_fees = 0u64;

    for delta in &input.timestamps {
        let delta = *delta as i64;
        current_ts = current_ts.saturating_add(delta);

        let result = accrue_interest(&mut market, &config, current_ts);
        if result.is_err() {
            return; // overflow is expected for extreme inputs
        }

        let sf = market.scale_factor();
        let fees = market.accrued_protocol_fees();

        // Invariant 1: scale_factor monotonically increases
        assert!(
            sf >= prev_scale_factor,
            "scale_factor decreased: {} -> {} at ts={}",
            prev_scale_factor,
            sf,
            current_ts
        );

        // Invariant 2: fees monotonically increase
        assert!(
            fees >= prev_fees,
            "fees decreased: {} -> {} at ts={}",
            prev_fees,
            fees,
            current_ts
        );

        // Invariant 3: last_accrual_timestamp <= current_ts
        assert!(
            market.last_accrual_timestamp() <= current_ts,
            "last_accrual ({}) > current_ts ({})",
            market.last_accrual_timestamp(),
            current_ts
        );

        prev_scale_factor = sf;
        prev_fees = fees;
    }
});
