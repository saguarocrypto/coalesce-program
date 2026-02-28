//! Fuzz target: differential testing of interest accrual.
//! Compare N-step accrual vs 1-step accrual.
//! N-step should always yield >= single-step (compound effect).

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
    num_steps: u8,      // number of intermediate steps (2..16)
    total_seconds: u32,  // total time period
}

fn make_market(annual_bps: u16, last_accrual: i64) -> Market {
    let mut m = Market::zeroed();
    m.set_annual_interest_bps(annual_bps);
    m.set_maturity_timestamp(i64::MAX);
    m.set_scale_factor(WAD);
    m.set_scaled_total_supply(WAD); // 1 unit supply
    m.set_last_accrual_timestamp(last_accrual);
    m.set_accrued_protocol_fees(0);
    m
}

fuzz_target!(|input: Input| {
    let annual_bps = input.annual_bps % 10_001;
    let num_steps = (input.num_steps % 15).max(2) as i64; // 2..16 steps
    let total_seconds = (input.total_seconds % 31_536_000).max(1) as i64;

    if annual_bps == 0 {
        return;
    }

    let zero_config = ProtocolConfig::zeroed();
    let start = 1_000_000i64;

    // Single-step: accrue all at once
    let mut m_single = make_market(annual_bps, start);
    if accrue_interest(&mut m_single, &zero_config, start + total_seconds).is_err() {
        return;
    }

    // Multi-step: accrue in `num_steps` equal increments
    let step_size = total_seconds / num_steps;
    if step_size == 0 {
        return;
    }

    let mut m_multi = make_market(annual_bps, start);
    let mut current = start;
    for _ in 0..num_steps {
        current += step_size;
        if accrue_interest(&mut m_multi, &zero_config, current).is_err() {
            return;
        }
    }
    // Accrue any remaining seconds
    let final_ts = start + total_seconds;
    if current < final_ts {
        if accrue_interest(&mut m_multi, &zero_config, final_ts).is_err() {
            return;
        }
    }

    // Multi-step (compound) should always yield >= single-step
    assert!(
        m_multi.scale_factor() >= m_single.scale_factor(),
        "compound ({}) < single ({}), annual_bps={}, steps={}, total={}",
        m_multi.scale_factor(),
        m_single.scale_factor(),
        annual_bps,
        num_steps,
        total_seconds
    );
});
