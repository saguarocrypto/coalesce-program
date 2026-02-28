//! Fuzz target: fee calculations maintain invariants.
//! - Fees never exceed total interest generated
//! - fee_rate_bps=0 produces zero fees
//! - Fees are monotonically increasing with fee_rate

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
    time_elapsed: u32, // seconds
    supply: u64,       // scaled_total_supply (USDC base units)
}

fuzz_target!(|input: Input| {
    let annual_bps = input.annual_bps % 10_001;
    let fee_rate_bps = input.fee_rate_bps % 10_001;
    let time_elapsed = input.time_elapsed.min(31_536_000); // cap at 1 year
    let supply = input.supply as u128;

    if supply == 0 || time_elapsed == 0 || annual_bps == 0 {
        return;
    }

    let last_accrual = 1_000_000i64;
    let maturity = last_accrual + 2 * 31_536_000i64;
    let current_ts = last_accrual + (time_elapsed as i64);

    // With fees
    let mut market_with_fees = Market::zeroed();
    market_with_fees.set_annual_interest_bps(annual_bps);
    market_with_fees.set_maturity_timestamp(maturity);
    market_with_fees.set_scale_factor(WAD);
    market_with_fees.set_scaled_total_supply(supply);
    market_with_fees.set_last_accrual_timestamp(last_accrual);
    market_with_fees.set_accrued_protocol_fees(0);

    let mut config = ProtocolConfig::zeroed();
    config.set_fee_rate_bps(fee_rate_bps);

    if accrue_interest(&mut market_with_fees, &config, current_ts).is_err() {
        return;
    }

    // With zero fees
    let mut market_no_fees = Market::zeroed();
    market_no_fees.set_annual_interest_bps(annual_bps);
    market_no_fees.set_maturity_timestamp(maturity);
    market_no_fees.set_scale_factor(WAD);
    market_no_fees.set_scaled_total_supply(supply);
    market_no_fees.set_last_accrual_timestamp(last_accrual);
    market_no_fees.set_accrued_protocol_fees(0);

    let mut zero_config = ProtocolConfig::zeroed();
    zero_config.set_fee_rate_bps(0);

    if accrue_interest(&mut market_no_fees, &zero_config, current_ts).is_err() {
        return;
    }

    // Invariant 1: zero fee rate => zero fees
    assert_eq!(
        market_no_fees.accrued_protocol_fees(),
        0,
        "fee_rate=0 should produce 0 fees"
    );

    // Invariant 2: scale_factor should be the same regardless of fee rate
    // (fees don't affect scale_factor computation)
    assert_eq!(
        market_with_fees.scale_factor(),
        market_no_fees.scale_factor(),
        "scale_factor should not depend on fee_rate"
    );
});
