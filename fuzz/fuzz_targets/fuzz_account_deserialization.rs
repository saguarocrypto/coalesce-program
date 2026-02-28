//! Fuzz target: random bytes into state structs should never panic
//! when using bytemuck::try_from_bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

use coalesce::constants::{
    BORROWER_WHITELIST_SIZE, LENDER_POSITION_SIZE, MARKET_SIZE, PROTOCOL_CONFIG_SIZE,
};
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

fuzz_target!(|data: &[u8]| {
    // Try to deserialize as Market
    if data.len() >= MARKET_SIZE {
        let slice = &data[..MARKET_SIZE];
        if let Ok(market) = bytemuck::try_from_bytes::<Market>(slice) {
            // Exercise getters — none should panic
            let _ = market.annual_interest_bps();
            let _ = market.maturity_timestamp();
            let _ = market.max_total_supply();
            let _ = market.market_nonce();
            let _ = market.scaled_total_supply();
            let _ = market.scale_factor();
            let _ = market.accrued_protocol_fees();
            let _ = market.total_deposited();
            let _ = market.total_borrowed();
            let _ = market.total_repaid();
            let _ = market.last_accrual_timestamp();
            let _ = market.settlement_factor_wad();
            let _ = market.total_interest_repaid();
        }
    }

    // Try to deserialize as ProtocolConfig
    if data.len() >= PROTOCOL_CONFIG_SIZE {
        let slice = &data[..PROTOCOL_CONFIG_SIZE];
        if let Ok(config) = bytemuck::try_from_bytes::<ProtocolConfig>(slice) {
            let _ = config.fee_rate_bps();
            let _ = config.is_paused();
            let _ = config.is_blacklist_fail_closed();
        }
    }

    // Try to deserialize as LenderPosition
    if data.len() >= LENDER_POSITION_SIZE {
        let slice = &data[..LENDER_POSITION_SIZE];
        if let Ok(pos) = bytemuck::try_from_bytes::<LenderPosition>(slice) {
            let _ = pos.scaled_balance();
        }
    }

    // Try to deserialize as BorrowerWhitelist
    if data.len() >= BORROWER_WHITELIST_SIZE {
        let slice = &data[..BORROWER_WHITELIST_SIZE];
        if let Ok(wl) = bytemuck::try_from_bytes::<BorrowerWhitelist>(slice) {
            let _ = wl.max_borrow_capacity();
            let _ = wl.current_borrowed();
        }
    }
});
