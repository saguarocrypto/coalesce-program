//! Fuzz target: interest accrual never panics and maintains monotonicity.

#![no_main]

use arbitrary::Arbitrary;
use bytemuck::Zeroable;
use libfuzzer_sys::fuzz_target;

use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

#[derive(Debug, Arbitrary)]
struct Input {
    annual_bps: u16,
    maturity_timestamp: i64,
    scale_factor_hi: u64,
    scale_factor_lo: u64,
    scaled_total_supply_hi: u64,
    scaled_total_supply_lo: u64,
    last_accrual_timestamp: i64,
    accrued_protocol_fees: u64,
    fee_rate_bps: u16,
    current_timestamp: i64,
}

fuzz_target!(|input: Input| {
    // Clamp annual_bps to valid range
    let annual_bps = input.annual_bps % 10_001;
    let fee_rate_bps = input.fee_rate_bps % 10_001;

    // Construct scale_factor from two u64s (covers full u128 range)
    let scale_factor = ((input.scale_factor_hi as u128) << 64) | (input.scale_factor_lo as u128);
    let scaled_total_supply =
        ((input.scaled_total_supply_hi as u128) << 64) | (input.scaled_total_supply_lo as u128);

    // Skip degenerate cases
    if scale_factor == 0 {
        return;
    }

    let mut market = Market::zeroed();
    market.set_annual_interest_bps(annual_bps);
    market.set_maturity_timestamp(input.maturity_timestamp);
    market.set_scale_factor(scale_factor);
    market.set_scaled_total_supply(scaled_total_supply);
    market.set_last_accrual_timestamp(input.last_accrual_timestamp);
    market.set_accrued_protocol_fees(input.accrued_protocol_fees);

    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_rate_bps);

    let sf_before = market.scale_factor();

    // Must not panic
    let result = accrue_interest(&mut market, &config, input.current_timestamp);

    if result.is_ok() {
        // scale_factor must never decrease
        assert!(
            market.scale_factor() >= sf_before,
            "scale_factor decreased: {} -> {}",
            sf_before,
            market.scale_factor()
        );
    }
});
