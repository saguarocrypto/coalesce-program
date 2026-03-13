//! Multi-market interaction tests.
//!
//! Verify that multiple markets sharing a ProtocolConfig or BorrowerWhitelist
//! maintain correct isolation of per-market state (scale_factor, fees,
//! settlement, supply, borrows) while correctly sharing global state
//! (fee rate, whitelist capacity).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_lossless,
    clippy::unnecessary_cast,
    clippy::useless_conversion,
    clippy::identity_op,
    clippy::manual_range_contains,
    clippy::single_match,
    clippy::single_match_else,
    clippy::needless_bool,
    clippy::nonminimal_bool,
    clippy::too_many_arguments,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_abs_diff,
    clippy::unreadable_literal,
    clippy::allow_attributes,
    clippy::struct_field_names,
    clippy::explicit_iter_loop,
    clippy::needless_for_each,
    clippy::absurd_extreme_comparisons,
    clippy::duplicated_attributes,
    clippy::manual_saturating_arithmetic,
    clippy::implicit_saturating_sub,
    clippy::stable_sort_primitive,
    clippy::type_complexity,
    clippy::iter_over_hash_type,
    clippy::bool_to_int_with_if,
    clippy::map_unwrap_or,
    clippy::explicit_counter_loop,
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::if_not_else,
    clippy::int_plus_one,
    clippy::range_plus_one,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::assertions_on_constants,
    clippy::for_kv_map,
    clippy::unnecessary_literal_bound,
    clippy::useless_vec,
    clippy::almost_complete_range,
    clippy::cloned_ref_to_slice_refs,
    unused_comparisons,
    unused_imports,
    unused_doc_comments,
    unused_variables,
    unused_mut,
    unused_assignments,
    dead_code,
    deprecated
)]

use bytemuck::Zeroable;

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 1 USDC in base units (6 decimals).
const USDC: u64 = 1_000_000;
#[path = "common/math_oracle.rs"]
mod math_oracle;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a zeroed Market with commonly needed fields.
fn make_market(annual_interest_bps: u16, maturity_timestamp: i64, max_total_supply: u64) -> Market {
    let mut m = Market::zeroed();
    m.set_annual_interest_bps(annual_interest_bps);
    m.set_maturity_timestamp(maturity_timestamp);
    m.set_max_total_supply(max_total_supply);
    m.set_scale_factor(WAD);
    m.set_last_accrual_timestamp(0);
    m
}

/// Create a zeroed ProtocolConfig with the given fee rate.
fn make_config(fee_rate_bps: u16) -> ProtocolConfig {
    let mut c = ProtocolConfig::zeroed();
    c.set_fee_rate_bps(fee_rate_bps);
    c
}

/// Create a zeroed BorrowerWhitelist with the given capacity.
fn make_whitelist(max_borrow_capacity: u64) -> BorrowerWhitelist {
    let mut wl = BorrowerWhitelist::zeroed();
    wl.is_whitelisted = 1;
    wl.set_max_borrow_capacity(max_borrow_capacity);
    wl
}

/// Simulate a deposit: computes scaled_amount, updates market and lender position.
/// Returns the scaled_amount credited to the lender.
fn sim_deposit(
    market: &mut Market,
    position: &mut LenderPosition,
    vault_balance: &mut u64,
    amount: u64,
) -> u128 {
    let amount_u128 = u128::from(amount);
    let scale_factor = market.scale_factor();

    // scaled_amount = amount * WAD / scale_factor
    let scaled_amount = amount_u128
        .checked_mul(WAD)
        .unwrap()
        .checked_div(scale_factor)
        .unwrap();

    assert!(
        scaled_amount > 0,
        "deposit would produce zero scaled amount"
    );

    // Update lender position
    let new_balance = position
        .scaled_balance()
        .checked_add(scaled_amount)
        .unwrap();
    position.set_scaled_balance(new_balance);

    // Update market
    let new_scaled_total = market
        .scaled_total_supply()
        .checked_add(scaled_amount)
        .unwrap();
    market.set_scaled_total_supply(new_scaled_total);

    let new_total_deposited = market.total_deposited().checked_add(amount).unwrap();
    market.set_total_deposited(new_total_deposited);

    // Update vault
    *vault_balance = vault_balance.checked_add(amount).unwrap();

    scaled_amount
}

/// Simulate a borrow: deducts from vault, updates market and whitelist.
fn sim_borrow(
    market: &mut Market,
    whitelist: &mut BorrowerWhitelist,
    vault_balance: &mut u64,
    amount: u64,
) {
    // Fee reservation check
    let fees_reserved = core::cmp::min(*vault_balance, market.accrued_protocol_fees());
    let borrowable = vault_balance.checked_sub(fees_reserved).unwrap();
    assert!(
        amount <= borrowable,
        "borrow amount {} exceeds borrowable {}",
        amount,
        borrowable
    );

    // Global capacity check
    let new_wl_total = whitelist.current_borrowed().checked_add(amount).unwrap();
    assert!(
        new_wl_total <= whitelist.max_borrow_capacity(),
        "borrow would exceed global capacity: new_total={}, max={}",
        new_wl_total,
        whitelist.max_borrow_capacity()
    );

    // Update vault
    *vault_balance = vault_balance.checked_sub(amount).unwrap();

    // Update market
    let new_total_borrowed = market.total_borrowed().checked_add(amount).unwrap();
    market.set_total_borrowed(new_total_borrowed);

    // Update whitelist
    whitelist.set_current_borrowed(new_wl_total);
}

/// Check if a borrow would exceed whitelist capacity (returns true if it would fail).
fn would_exceed_capacity(whitelist: &BorrowerWhitelist, amount: u64) -> bool {
    match whitelist.current_borrowed().checked_add(amount) {
        Some(new_total) => new_total > whitelist.max_borrow_capacity(),
        None => true,
    }
}

/// Simulate a repayment: adds to vault, updates market.
fn sim_repay(market: &mut Market, vault_balance: &mut u64, amount: u64) {
    *vault_balance = vault_balance.checked_add(amount).unwrap();
    let new_total_repaid = market.total_repaid().checked_add(amount).unwrap();
    market.set_total_repaid(new_total_repaid);
}

/// Compute the settlement factor given current vault state.
fn compute_settlement_factor(market: &Market, vault_balance: u64) -> u128 {
    let vault_u128 = u128::from(vault_balance);
    let fees_u128 = u128::from(market.accrued_protocol_fees());
    let fees_reserved = if vault_u128 < fees_u128 {
        vault_u128
    } else {
        fees_u128
    };
    let available_for_lenders = vault_u128.checked_sub(fees_reserved).unwrap();

    let total_normalized = market
        .scaled_total_supply()
        .checked_mul(market.scale_factor())
        .unwrap()
        .checked_div(WAD)
        .unwrap();

    if total_normalized == 0 {
        return WAD;
    }

    let raw = available_for_lenders
        .checked_mul(WAD)
        .unwrap()
        .checked_div(total_normalized)
        .unwrap();

    let capped = if raw > WAD { WAD } else { raw };
    if capped < 1 {
        1
    } else {
        capped
    }
}

/// Compute the payout for a lender position given the market state.
fn compute_payout(market: &Market, position: &LenderPosition) -> u64 {
    let scale_factor = market.scale_factor();
    let settlement_factor = market.settlement_factor_wad();

    let normalized = position
        .scaled_balance()
        .checked_mul(scale_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();

    let payout_u128 = normalized
        .checked_mul(settlement_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();

    u64::try_from(payout_u128).unwrap()
}

/// Normalize a scaled balance: scaled_balance * scale_factor / WAD.
fn normalize_balance(scaled_balance: u128, scale_factor: u128) -> u128 {
    scaled_balance
        .checked_mul(scale_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap()
}

fn scale_factor_after_elapsed(
    scale_factor_before: u128,
    annual_bps: u16,
    elapsed_seconds: i64,
) -> u128 {
    let growth = math_oracle::growth_factor_wad(annual_bps, elapsed_seconds);
    math_oracle::mul_wad(scale_factor_before, growth)
}

fn fee_delta_after_elapsed(
    scaled_supply: u128,
    scale_factor_before: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> u64 {
    if scaled_supply == 0 || fee_rate_bps == 0 || elapsed_seconds <= 0 {
        return 0;
    }

    let growth = math_oracle::growth_factor_wad(annual_bps, elapsed_seconds);
    let interest_delta_wad = growth.checked_sub(WAD).unwrap();
    let fee_delta_wad = interest_delta_wad
        .checked_mul(u128::from(fee_rate_bps))
        .unwrap()
        .checked_div(BPS)
        .unwrap();
    // Use pre-accrual scale_factor_before (matches on-chain Finding 10 fix)
    let fee_normalized = scaled_supply
        .checked_mul(scale_factor_before)
        .unwrap()
        .checked_div(WAD)
        .unwrap()
        .checked_mul(fee_delta_wad)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    u64::try_from(fee_normalized).unwrap()
}

// ===========================================================================
// Test 1: State isolation -- basic
//
// Two markets with identical parameters. Deposit into market A, verify
// market B state is unchanged.
// ===========================================================================

#[test]
fn state_isolation_basic_deposit() {
    let config = make_config(500);
    let maturity = SECONDS_PER_YEAR as i64 * 2;

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    let market_b = make_market(1000, maturity, 10_000_000 * USDC);

    let mut vault_a: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();

    // Snapshot market B before deposit into A
    let b_sf_before = market_b.scale_factor();
    let b_supply_before = market_b.scaled_total_supply();
    let b_deposited_before = market_b.total_deposited();
    let b_borrowed_before = market_b.total_borrowed();
    let b_fees_before = market_b.accrued_protocol_fees();
    let b_settlement_before = market_b.settlement_factor_wad();

    // Deposit 1M USDC into market A
    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, 1_000_000 * USDC);

    // Accrue interest on market A
    accrue_interest(&mut market_a, &config, SECONDS_PER_YEAR as i64).unwrap();

    // Verify market A was affected
    assert!(market_a.scaled_total_supply() > 0);
    assert!(market_a.total_deposited() > 0);
    assert_eq!(vault_a, 1_000_000 * USDC);

    // Verify market B is completely unchanged
    assert_eq!(market_b.scale_factor(), b_sf_before);
    assert_eq!(market_b.scaled_total_supply(), b_supply_before);
    assert_eq!(market_b.total_deposited(), b_deposited_before);
    assert_eq!(market_b.total_borrowed(), b_borrowed_before);
    assert_eq!(market_b.accrued_protocol_fees(), b_fees_before);
    assert_eq!(market_b.settlement_factor_wad(), b_settlement_before);
}

// ===========================================================================
// Test 2: State isolation -- interest accrual
//
// Accrue interest on market A, verify market B's scale_factor and fees
// are unchanged.
// ===========================================================================

#[test]
fn state_isolation_interest_accrual() {
    let config = make_config(500); // 5% fee
    let maturity = SECONDS_PER_YEAR as i64 * 2;

    // Both markets start with some deposits for fee accrual to be nonzero
    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    market_a.set_scaled_total_supply(1_000_000_000_000); // 1M USDC worth
    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);
    market_b.set_scaled_total_supply(1_000_000_000_000);

    // Snapshot market B
    let b_sf = market_b.scale_factor();
    let b_fees = market_b.accrued_protocol_fees();
    let b_last_ts = market_b.last_accrual_timestamp();

    // Accrue interest on market A only
    accrue_interest(&mut market_a, &config, SECONDS_PER_YEAR as i64).unwrap();

    // Verify market A changed
    assert!(market_a.scale_factor() > WAD);
    assert!(market_a.accrued_protocol_fees() > 0);
    assert_eq!(market_a.last_accrual_timestamp(), SECONDS_PER_YEAR as i64);

    // Verify market B is unchanged
    assert_eq!(market_b.scale_factor(), b_sf);
    assert_eq!(market_b.accrued_protocol_fees(), b_fees);
    assert_eq!(market_b.last_accrual_timestamp(), b_last_ts);
}

// ===========================================================================
// Test 3: State isolation -- settlement
//
// Settle market A (partial default), verify market B's settlement_factor
// is still 0.
// ===========================================================================

#[test]
fn state_isolation_settlement() {
    let config = make_config(0); // no fees for simplicity
    let maturity = SECONDS_PER_YEAR as i64;

    // Market A: lender deposits 1M, borrower borrows 800K, repays 400K (50% default)
    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    let mut wl = make_whitelist(10_000_000 * USDC);
    let mut vault_a: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();

    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, 1_000_000 * USDC);
    sim_borrow(&mut market_a, &mut wl, &mut vault_a, 800_000 * USDC);

    // Accrue interest to maturity
    accrue_interest(&mut market_a, &config, maturity).unwrap();

    // Repay 400K (partial)
    sim_repay(&mut market_a, &mut vault_a, 400_000 * USDC);

    // Settle market A
    let sf_a = compute_settlement_factor(&market_a, vault_a);
    market_a.set_settlement_factor_wad(sf_a);

    assert!(sf_a > 0);
    assert!(sf_a < WAD, "partial default should yield settlement < WAD");

    // Market B: separate market, never touched beyond creation
    let market_b = make_market(1000, maturity, 10_000_000 * USDC);

    // Market B settlement_factor_wad should still be 0 (unsettled)
    assert_eq!(
        market_b.settlement_factor_wad(),
        0,
        "market B should be unsettled"
    );

    // Also verify no cross-contamination of other fields
    assert_eq!(market_b.total_borrowed(), 0);
    assert_eq!(market_b.total_repaid(), 0);
}

// ===========================================================================
// Test 4: State isolation -- borrow
//
// Borrow from market A, verify market B's total_borrowed and vault
// are unchanged.
// ===========================================================================

#[test]
fn state_isolation_borrow() {
    let maturity = SECONDS_PER_YEAR as i64 * 2;

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);
    let mut wl = make_whitelist(10_000_000 * USDC);

    let mut vault_a: u64 = 0;
    let mut vault_b: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();
    let mut pos_b = LenderPosition::zeroed();

    // Deposit into both markets
    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, 1_000_000 * USDC);
    sim_deposit(&mut market_b, &mut pos_b, &mut vault_b, 1_000_000 * USDC);

    // Snapshot market B state
    let b_borrowed = market_b.total_borrowed();
    let b_vault = vault_b;
    let b_supply = market_b.scaled_total_supply();

    // Borrow from market A only
    sim_borrow(&mut market_a, &mut wl, &mut vault_a, 500_000 * USDC);

    // Verify market A changed
    assert_eq!(market_a.total_borrowed(), 500_000 * USDC);
    assert_eq!(vault_a, 500_000 * USDC);

    // Verify market B unchanged
    assert_eq!(market_b.total_borrowed(), b_borrowed);
    assert_eq!(vault_b, b_vault);
    assert_eq!(market_b.scaled_total_supply(), b_supply);
}

// ===========================================================================
// Test 5: Shared protocol config -- fee rate applies to both
//
// Same ProtocolConfig with 5% fee. Both markets accrue fees at the same rate.
// ===========================================================================

#[test]
fn shared_config_fee_rate_applies_to_both() {
    let config = make_config(500); // 5% fee
    let maturity = SECONDS_PER_YEAR as i64 * 2;
    let supply = 1_000_000_000_000u128; // 1M USDC worth of scaled tokens

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    market_a.set_scaled_total_supply(supply);

    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);
    market_b.set_scaled_total_supply(supply);

    // Accrue one year on both using the SAME config
    accrue_interest(&mut market_a, &config, SECONDS_PER_YEAR as i64).unwrap();
    accrue_interest(&mut market_b, &config, SECONDS_PER_YEAR as i64).unwrap();

    // Both should have identical scale factors
    assert_eq!(
        market_a.scale_factor(),
        market_b.scale_factor(),
        "identical markets with same config should have same scale_factor"
    );

    // Both should have identical fees
    assert_eq!(
        market_a.accrued_protocol_fees(),
        market_b.accrued_protocol_fees(),
        "identical markets with same config should accrue identical fees"
    );

    // Verify fees are nonzero
    assert!(market_a.accrued_protocol_fees() > 0);

    let expected_fee = fee_delta_after_elapsed(supply, WAD, 1000, 500, SECONDS_PER_YEAR as i64);

    assert_eq!(market_a.accrued_protocol_fees(), expected_fee);
    assert_eq!(market_b.accrued_protocol_fees(), expected_fee);
}

// ===========================================================================
// Test 6: Shared protocol config -- fee rate change
//
// Update fee rate in shared config. Verify both markets use the new rate
// on next accrual.
// ===========================================================================

#[test]
fn shared_config_fee_rate_change() {
    let mut config = make_config(500); // 5% fee initially
    let maturity = SECONDS_PER_YEAR as i64 * 4;
    let supply = 1_000_000_000_000u128;

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    market_a.set_scaled_total_supply(supply);

    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);
    market_b.set_scaled_total_supply(supply);

    // Phase 1: Accrue with 5% fee for one year
    let year1 = SECONDS_PER_YEAR as i64;
    accrue_interest(&mut market_a, &config, year1).unwrap();
    accrue_interest(&mut market_b, &config, year1).unwrap();

    let fees_a_after_year1 = market_a.accrued_protocol_fees();
    let fees_b_after_year1 = market_b.accrued_protocol_fees();
    assert_eq!(fees_a_after_year1, fees_b_after_year1);

    // Phase 2: Change fee rate to 10%
    config.set_fee_rate_bps(1000);
    assert_eq!(config.fee_rate_bps(), 1000);

    // Accrue another year with the new rate
    let year2 = SECONDS_PER_YEAR as i64 * 2;
    accrue_interest(&mut market_a, &config, year2).unwrap();
    accrue_interest(&mut market_b, &config, year2).unwrap();

    // Both should still have identical fees (both see the same config change)
    assert_eq!(
        market_a.accrued_protocol_fees(),
        market_b.accrued_protocol_fees(),
        "after fee rate change, both markets should accrue same new fees"
    );

    // Fees in year 2 should be higher per-period than year 1 (doubled fee rate,
    // but also slightly higher scale factor in year 2, so the ratio is > 2x
    // for the fee increment).
    let fees_a_increment_year2 = market_a.accrued_protocol_fees() - fees_a_after_year1;
    let fees_b_increment_year2 = market_b.accrued_protocol_fees() - fees_b_after_year1;

    // Year 2 fee increment should be strictly larger than year 1 because
    // the fee rate doubled AND the scale factor is higher.
    assert!(
        fees_a_increment_year2 > fees_a_after_year1,
        "year 2 fees ({}) should exceed year 1 fees ({}) with doubled rate and compound growth",
        fees_a_increment_year2,
        fees_a_after_year1
    );
    assert_eq!(fees_a_increment_year2, fees_b_increment_year2);
}

// ===========================================================================
// Test 7: Shared whitelist -- capacity tracking
//
// Borrower has whitelist with 1M capacity. Borrow 600K from market A,
// then try to borrow 500K from market B -- should fail (exceeds capacity).
// 400K should succeed.
// ===========================================================================

#[test]
fn shared_whitelist_capacity_tracking() {
    let maturity = SECONDS_PER_YEAR as i64 * 2;

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);
    let mut wl = make_whitelist(1_000_000 * USDC); // 1M USDC capacity

    let mut vault_a: u64 = 0;
    let mut vault_b: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();
    let mut pos_b = LenderPosition::zeroed();

    // Deposit enough in both markets
    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, 2_000_000 * USDC);
    sim_deposit(&mut market_b, &mut pos_b, &mut vault_b, 2_000_000 * USDC);

    // Borrow 600K from market A
    sim_borrow(&mut market_a, &mut wl, &mut vault_a, 600_000 * USDC);
    assert_eq!(wl.current_borrowed(), 600_000 * USDC);

    // Try to borrow 500K from market B -- should fail (600K + 500K = 1.1M > 1M)
    assert!(
        would_exceed_capacity(&wl, 500_000 * USDC),
        "borrowing 500K when 600K already used should exceed 1M capacity"
    );

    // Borrow 400K from market B -- should succeed (600K + 400K = 1M exactly)
    sim_borrow(&mut market_b, &mut wl, &mut vault_b, 400_000 * USDC);
    assert_eq!(wl.current_borrowed(), 1_000_000 * USDC);
    assert_eq!(market_b.total_borrowed(), 400_000 * USDC);
}

// ===========================================================================
// Test 8: Shared whitelist -- cumulative tracking
//
// Multiple borrows across markets, verify whitelist.current_borrowed is the sum.
// ===========================================================================

#[test]
fn shared_whitelist_cumulative_tracking() {
    let maturity = SECONDS_PER_YEAR as i64 * 2;

    let mut market_a = make_market(500, maturity, 10_000_000 * USDC);
    let mut market_b = make_market(800, maturity, 10_000_000 * USDC);
    let mut wl = make_whitelist(5_000_000 * USDC);

    let mut vault_a: u64 = 0;
    let mut vault_b: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();
    let mut pos_b = LenderPosition::zeroed();

    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, 3_000_000 * USDC);
    sim_deposit(&mut market_b, &mut pos_b, &mut vault_b, 3_000_000 * USDC);

    // Borrow from market A: 200K
    sim_borrow(&mut market_a, &mut wl, &mut vault_a, 200_000 * USDC);
    assert_eq!(wl.current_borrowed(), 200_000 * USDC);

    // Borrow from market B: 300K
    sim_borrow(&mut market_b, &mut wl, &mut vault_b, 300_000 * USDC);
    assert_eq!(wl.current_borrowed(), 500_000 * USDC);

    // Borrow from market A again: 150K
    sim_borrow(&mut market_a, &mut wl, &mut vault_a, 150_000 * USDC);
    assert_eq!(wl.current_borrowed(), 650_000 * USDC);

    // Borrow from market B again: 350K
    sim_borrow(&mut market_b, &mut wl, &mut vault_b, 350_000 * USDC);
    assert_eq!(wl.current_borrowed(), 1_000_000 * USDC);

    // Verify individual market tracking
    assert_eq!(market_a.total_borrowed(), 350_000 * USDC);
    assert_eq!(market_b.total_borrowed(), 650_000 * USDC);

    // Verify whitelist total = sum of market borrows
    assert_eq!(
        wl.current_borrowed(),
        market_a.total_borrowed() + market_b.total_borrowed()
    );
}

// ===========================================================================
// Test 9: Independent maturity
//
// Market A matures at T1, market B at T2 > T1. At time between T1 and T2,
// market A should be settleable but market B should still accrue.
// ===========================================================================

#[test]
fn independent_maturity() {
    let config = make_config(500);
    let t1 = SECONDS_PER_YEAR as i64; // Market A maturity: 1 year
    let t2 = SECONDS_PER_YEAR as i64 * 2; // Market B maturity: 2 years
    let supply = 1_000_000_000_000u128;

    let mut market_a = make_market(1000, t1, 10_000_000 * USDC);
    market_a.set_scaled_total_supply(supply);
    let mut market_b = make_market(1000, t2, 10_000_000 * USDC);
    market_b.set_scaled_total_supply(supply);

    // Advance to 1.5 years (between T1 and T2)
    let mid_time = (SECONDS_PER_YEAR as i64 * 3) / 2;

    accrue_interest(&mut market_a, &config, mid_time).unwrap();
    accrue_interest(&mut market_b, &config, mid_time).unwrap();

    // Market A: interest capped at maturity (T1 = 1 year)
    assert_eq!(
        market_a.last_accrual_timestamp(),
        t1,
        "market A accrual should be capped at its maturity"
    );

    // Market B: interest should accrue up to mid_time (1.5 years)
    assert_eq!(
        market_b.last_accrual_timestamp(),
        mid_time,
        "market B should accrue up to mid_time since it has not matured"
    );

    // Market B should have strictly more interest accrued (1.5 years vs 1 year)
    assert!(
        market_b.scale_factor() > market_a.scale_factor(),
        "market B (1.5y accrual) should have higher scale_factor than A (1y capped)"
    );

    // Market A is past maturity -> eligible for settlement
    // Market B is not yet at maturity -> should continue accruing
    // Accrue again at T2
    accrue_interest(&mut market_a, &config, t2).unwrap();
    accrue_interest(&mut market_b, &config, t2).unwrap();

    // Market A should still be capped at T1 (no further accrual)
    assert_eq!(market_a.last_accrual_timestamp(), t1);
    let sf_a_at_t1 = market_a.scale_factor();

    // Re-accrue doesn't change market A further
    let fees_a_before = market_a.accrued_protocol_fees();
    accrue_interest(&mut market_a, &config, t2 + 1000).unwrap();
    assert_eq!(market_a.scale_factor(), sf_a_at_t1);
    assert_eq!(market_a.accrued_protocol_fees(), fees_a_before);

    // Market B accrued all the way to T2
    assert_eq!(market_b.last_accrual_timestamp(), t2);
    assert!(market_b.scale_factor() > market_a.scale_factor());
}

// ===========================================================================
// Test 10: Different interest rates
//
// Market A at 5%, market B at 20%. After same time period, verify market B
// has strictly higher scale_factor.
// ===========================================================================

#[test]
fn different_interest_rates() {
    let config = make_config(0); // no fees
    let maturity = SECONDS_PER_YEAR as i64 * 3;

    let mut market_a = make_market(500, maturity, 10_000_000 * USDC); // 5%
    let mut market_b = make_market(2000, maturity, 10_000_000 * USDC); // 20%

    // Both start at WAD
    assert_eq!(market_a.scale_factor(), WAD);
    assert_eq!(market_b.scale_factor(), WAD);

    // Accrue one year
    accrue_interest(&mut market_a, &config, SECONDS_PER_YEAR as i64).unwrap();
    accrue_interest(&mut market_b, &config, SECONDS_PER_YEAR as i64).unwrap();

    let expected_a = scale_factor_after_elapsed(WAD, 500, SECONDS_PER_YEAR as i64);
    assert_eq!(market_a.scale_factor(), expected_a);

    let expected_b = scale_factor_after_elapsed(WAD, 2000, SECONDS_PER_YEAR as i64);
    assert_eq!(market_b.scale_factor(), expected_b);

    assert!(
        market_b.scale_factor() > market_a.scale_factor(),
        "20% rate market should have strictly higher scale_factor than 5% rate market"
    );

    // Verify the high-rate market accrues materially more than low-rate market.
    let delta_a = market_a.scale_factor() - WAD;
    let delta_b = market_b.scale_factor() - WAD;
    assert!(
        delta_b >= delta_a.checked_mul(4).unwrap(),
        "20% rate should produce at least 4x the absolute interest delta of 5% rate"
    );
}

// ===========================================================================
// Test 11: Cross-market lender
//
// Same lender deposits in both markets. Verify positions are independent --
// withdrawing from A doesn't affect B position.
// ===========================================================================

#[test]
fn cross_market_lender_independence() {
    let maturity = SECONDS_PER_YEAR as i64 * 2;

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);

    // Same lender, but two separate LenderPosition accounts (one per market)
    let mut pos_a = LenderPosition::zeroed();
    let mut pos_b = LenderPosition::zeroed();

    let mut vault_a: u64 = 0;
    let mut vault_b: u64 = 0;

    // Deposit into market A: 500K
    let scaled_a = sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, 500_000 * USDC);
    // Deposit into market B: 800K
    let scaled_b = sim_deposit(&mut market_b, &mut pos_b, &mut vault_b, 800_000 * USDC);

    assert!(pos_a.scaled_balance() > 0);
    assert!(pos_b.scaled_balance() > 0);
    assert_eq!(pos_a.scaled_balance(), scaled_a);
    assert_eq!(pos_b.scaled_balance(), scaled_b);

    // Simulate "withdraw" from market A: zero out position A
    let pos_b_before_withdrawal = pos_b.scaled_balance();
    let market_b_supply_before = market_b.scaled_total_supply();

    // Withdraw from A: reduce market A supply and zero position A
    let withdraw_scaled = pos_a.scaled_balance();
    let new_supply_a = market_a
        .scaled_total_supply()
        .checked_sub(withdraw_scaled)
        .unwrap();
    market_a.set_scaled_total_supply(new_supply_a);
    pos_a.set_scaled_balance(0);

    // Verify position A is zeroed
    assert_eq!(pos_a.scaled_balance(), 0);
    assert_eq!(market_a.scaled_total_supply(), 0);

    // Verify position B and market B are completely unaffected
    assert_eq!(pos_b.scaled_balance(), pos_b_before_withdrawal);
    assert_eq!(market_b.scaled_total_supply(), market_b_supply_before);
    assert_eq!(pos_b.scaled_balance(), scaled_b);
}

// ===========================================================================
// Test 12: Proportional fairness
//
// Two identical markets, same deposits, same borrows, same repayments.
// Verify identical outcomes (scale_factor, fees, settlement).
// ===========================================================================

#[test]
fn proportional_fairness_identical_markets() {
    let config = make_config(500); // 5% fee
    let maturity = SECONDS_PER_YEAR as i64;

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC);
    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);

    let mut wl_a = make_whitelist(10_000_000 * USDC);
    let mut wl_b = make_whitelist(10_000_000 * USDC);

    let mut vault_a: u64 = 0;
    let mut vault_b: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();
    let mut pos_b = LenderPosition::zeroed();

    // Same deposits
    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, 1_000_000 * USDC);
    sim_deposit(&mut market_b, &mut pos_b, &mut vault_b, 1_000_000 * USDC);

    // Same borrows
    sim_borrow(&mut market_a, &mut wl_a, &mut vault_a, 500_000 * USDC);
    sim_borrow(&mut market_b, &mut wl_b, &mut vault_b, 500_000 * USDC);

    // Same interest accrual (to maturity)
    accrue_interest(&mut market_a, &config, maturity).unwrap();
    accrue_interest(&mut market_b, &config, maturity).unwrap();

    // Same repayments
    sim_repay(&mut market_a, &mut vault_a, 500_000 * USDC);
    sim_repay(&mut market_b, &mut vault_b, 500_000 * USDC);

    // Verify identical outcomes
    assert_eq!(market_a.scale_factor(), market_b.scale_factor());
    assert_eq!(
        market_a.accrued_protocol_fees(),
        market_b.accrued_protocol_fees()
    );
    assert_eq!(market_a.total_deposited(), market_b.total_deposited());
    assert_eq!(market_a.total_borrowed(), market_b.total_borrowed());
    assert_eq!(market_a.total_repaid(), market_b.total_repaid());
    assert_eq!(vault_a, vault_b);

    // Settlement factors should be identical
    let sf_a = compute_settlement_factor(&market_a, vault_a);
    let sf_b = compute_settlement_factor(&market_b, vault_b);
    assert_eq!(sf_a, sf_b);

    // Lender positions should be identical
    assert_eq!(pos_a.scaled_balance(), pos_b.scaled_balance());

    // Payouts should be identical
    market_a.set_settlement_factor_wad(sf_a);
    market_b.set_settlement_factor_wad(sf_b);
    let payout_a = compute_payout(&market_a, &pos_a);
    let payout_b = compute_payout(&market_b, &pos_b);
    assert_eq!(payout_a, payout_b);
}

// ===========================================================================
// Test 13: Mixed settlement outcomes
//
// Market A: borrower repays principal + interest (settlement=WAD).
// Market B: borrower repays only half of principal (partial default).
// Verify each market's lenders get correct payouts independently.
//
// Note: For settlement_factor == WAD, the vault must hold enough to cover
// the full normalized value (deposit + accrued interest). The borrower must
// repay principal plus the interest owed, not just the principal.
// ===========================================================================

#[test]
fn mixed_settlement_outcomes() {
    let config = make_config(0); // no fees for clarity
    let maturity = SECONDS_PER_YEAR as i64;
    let deposit_amount = 1_000_000 * USDC;

    // --- Market A: full repayment (principal + interest) ---
    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC); // 10% rate
    let mut wl_a = make_whitelist(10_000_000 * USDC);
    let mut vault_a: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();

    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, deposit_amount);
    sim_borrow(&mut market_a, &mut wl_a, &mut vault_a, 800_000 * USDC);

    // Accrue interest to maturity (10% for 1 year -> scale_factor = 1.1 * WAD)
    accrue_interest(&mut market_a, &config, maturity).unwrap();

    // Compute the total normalized owed to lenders
    let total_normalized_a =
        normalize_balance(market_a.scaled_total_supply(), market_a.scale_factor());
    // The vault currently has 200K. Lenders are owed ~1.1M.
    // Borrower must repay enough to cover the full normalized amount.
    let vault_before_repay_a = vault_a;
    let needed_a = u64::try_from(total_normalized_a)
        .unwrap()
        .saturating_sub(vault_before_repay_a);
    sim_repay(&mut market_a, &mut vault_a, needed_a);

    let sf_a = compute_settlement_factor(&market_a, vault_a);
    market_a.set_settlement_factor_wad(sf_a);

    // --- Market B: partial default (repay only half of principal) ---
    let mut market_b = make_market(1000, maturity, 10_000_000 * USDC);
    let mut wl_b = make_whitelist(10_000_000 * USDC);
    let mut vault_b: u64 = 0;
    let mut pos_b = LenderPosition::zeroed();

    sim_deposit(&mut market_b, &mut pos_b, &mut vault_b, deposit_amount);
    sim_borrow(&mut market_b, &mut wl_b, &mut vault_b, 800_000 * USDC);

    // Accrue interest to maturity
    accrue_interest(&mut market_b, &config, maturity).unwrap();

    // Only repay 400K of 800K borrowed (50% of principal, ignoring interest)
    sim_repay(&mut market_b, &mut vault_b, 400_000 * USDC);

    let sf_b = compute_settlement_factor(&market_b, vault_b);
    market_b.set_settlement_factor_wad(sf_b);

    // Verify settlement factors
    assert_eq!(
        sf_a, WAD,
        "market A (full repayment including interest) should have settlement factor = WAD"
    );
    assert!(
        sf_b < WAD,
        "defaulted market should have settlement factor < WAD"
    );
    assert!(sf_b > 0, "settlement factor should be positive");

    // Compute payouts
    let payout_a = compute_payout(&market_a, &pos_a);
    let payout_b = compute_payout(&market_b, &pos_b);

    // Lender A should get full normalized value (deposit + interest)
    let normalized_a = normalize_balance(pos_a.scaled_balance(), market_a.scale_factor());
    assert_eq!(
        u128::from(payout_a),
        normalized_a,
        "full settlement payout should equal normalized balance"
    );

    // Lender B should get less than normalized (due to default)
    let normalized_b = normalize_balance(pos_b.scaled_balance(), market_b.scale_factor());
    assert!(
        u128::from(payout_b) < normalized_b,
        "defaulted market payout should be less than normalized balance"
    );

    // Lender A payout should be strictly greater than lender B payout
    assert!(
        payout_a > payout_b,
        "fully repaid market payout ({}) should exceed defaulted market payout ({})",
        payout_a,
        payout_b
    );

    // Verify independence: market A settlement does not affect market B
    assert_ne!(sf_a, sf_b);
    assert_ne!(payout_a, payout_b);
}

// ===========================================================================
// Test 14: Sequential operations
//
// Interleave operations across markets (deposit A, deposit B, borrow A,
// accrue both, borrow B, repay A, etc.) and verify each market's state
// is consistent with its own operation history.
// ===========================================================================

#[test]
fn sequential_interleaved_operations() {
    let config = make_config(500); // 5% fee
    let maturity = SECONDS_PER_YEAR as i64 * 2;

    let mut market_a = make_market(1000, maturity, 10_000_000 * USDC); // 10% rate
    let mut market_b = make_market(500, maturity, 10_000_000 * USDC); // 5% rate

    let mut wl = make_whitelist(5_000_000 * USDC);

    let mut vault_a: u64 = 0;
    let mut vault_b: u64 = 0;
    let mut pos_a1 = LenderPosition::zeroed(); // lender 1 in market A
    let mut pos_b1 = LenderPosition::zeroed(); // lender 1 in market B

    // Step 1: Deposit 1M into market A
    sim_deposit(&mut market_a, &mut pos_a1, &mut vault_a, 1_000_000 * USDC);
    assert_eq!(market_a.total_deposited(), 1_000_000 * USDC);
    assert_eq!(vault_a, 1_000_000 * USDC);

    // Step 2: Deposit 500K into market B
    sim_deposit(&mut market_b, &mut pos_b1, &mut vault_b, 500_000 * USDC);
    assert_eq!(market_b.total_deposited(), 500_000 * USDC);
    assert_eq!(vault_b, 500_000 * USDC);

    // Step 3: Borrow 300K from market A
    sim_borrow(&mut market_a, &mut wl, &mut vault_a, 300_000 * USDC);
    assert_eq!(market_a.total_borrowed(), 300_000 * USDC);
    assert_eq!(vault_a, 700_000 * USDC);
    assert_eq!(wl.current_borrowed(), 300_000 * USDC);

    // Step 4: Accrue 6 months on both
    let six_months = SECONDS_PER_YEAR as i64 / 2;
    accrue_interest(&mut market_a, &config, six_months).unwrap();
    accrue_interest(&mut market_b, &config, six_months).unwrap();

    // Market A (10% annual) should have more interest than B (5% annual) at 6 months
    let sf_a_6m = market_a.scale_factor();
    let sf_b_6m = market_b.scale_factor();
    assert!(
        sf_a_6m > sf_b_6m,
        "10% market should accrue more than 5% market"
    );

    // Step 5: Borrow 200K from market B
    sim_borrow(&mut market_b, &mut wl, &mut vault_b, 200_000 * USDC);
    assert_eq!(market_b.total_borrowed(), 200_000 * USDC);
    assert_eq!(vault_b, 300_000 * USDC);
    assert_eq!(wl.current_borrowed(), 500_000 * USDC);

    // Step 6: Repay 300K to market A
    sim_repay(&mut market_a, &mut vault_a, 300_000 * USDC);
    assert_eq!(market_a.total_repaid(), 300_000 * USDC);
    assert_eq!(vault_a, 1_000_000 * USDC);

    // Step 7: Accrue another 6 months on both (total 1 year)
    let one_year = SECONDS_PER_YEAR as i64;
    accrue_interest(&mut market_a, &config, one_year).unwrap();
    accrue_interest(&mut market_b, &config, one_year).unwrap();

    // Verify market A state consistency
    assert_eq!(market_a.total_deposited(), 1_000_000 * USDC);
    assert_eq!(market_a.total_borrowed(), 300_000 * USDC);
    assert_eq!(market_a.total_repaid(), 300_000 * USDC);
    assert_eq!(vault_a, 1_000_000 * USDC);
    assert!(market_a.scale_factor() > sf_a_6m);
    assert!(market_a.accrued_protocol_fees() > 0);

    // Verify market B state consistency
    assert_eq!(market_b.total_deposited(), 500_000 * USDC);
    assert_eq!(market_b.total_borrowed(), 200_000 * USDC);
    assert_eq!(market_b.total_repaid(), 0);
    assert_eq!(vault_b, 300_000 * USDC);
    assert!(market_b.scale_factor() > sf_b_6m);
    assert!(market_b.accrued_protocol_fees() > 0);

    // Market A still has higher scale factor than B (10% vs 5% for full year)
    assert!(market_a.scale_factor() > market_b.scale_factor());

    // Whitelist total should be sum of both market borrows
    assert_eq!(
        wl.current_borrowed(),
        market_a.total_borrowed() + market_b.total_borrowed()
    );

    // Step 8: Deposit more into market B
    sim_deposit(&mut market_b, &mut pos_b1, &mut vault_b, 100_000 * USDC);
    assert_eq!(market_b.total_deposited(), 600_000 * USDC);
    assert_eq!(vault_b, 400_000 * USDC);

    // Step 9: Repay market B
    sim_repay(&mut market_b, &mut vault_b, 200_000 * USDC);
    assert_eq!(market_b.total_repaid(), 200_000 * USDC);
    assert_eq!(vault_b, 600_000 * USDC);

    // Step 10: Accrue to maturity
    accrue_interest(&mut market_a, &config, maturity).unwrap();
    accrue_interest(&mut market_b, &config, maturity).unwrap();

    // Both should be at maturity
    assert_eq!(market_a.last_accrual_timestamp(), maturity);
    assert_eq!(market_b.last_accrual_timestamp(), maturity);

    // Settle both markets
    let sf_a = compute_settlement_factor(&market_a, vault_a);
    let sf_b = compute_settlement_factor(&market_b, vault_b);

    // Market A was fully repaid (no default)
    // Market B was fully repaid too
    // Both should have settlement factor = WAD (or very close, accounting for fees)
    // Note: fees reduce available_for_lenders, so settlement might be < WAD
    // when fees are nonzero
    market_a.set_settlement_factor_wad(sf_a);
    market_b.set_settlement_factor_wad(sf_b);

    assert!(sf_a > 0);
    assert!(sf_b > 0);

    // Both lenders should get nonzero payouts
    let payout_a = compute_payout(&market_a, &pos_a1);
    let payout_b = compute_payout(&market_b, &pos_b1);
    assert!(payout_a > 0, "market A lender should get nonzero payout");
    assert!(payout_b > 0, "market B lender should get nonzero payout");
}

// ===========================================================================
// Additional edge case tests using proptest
// ===========================================================================

use proptest::prelude::*;

fn edge_bps_strategy() -> impl Strategy<Value = u16> {
    prop_oneof![
        Just(0u16),
        Just(1u16),
        Just(2u16),
        Just(9_999u16),
        Just(10_000u16),
        (0u16..=10_000u16),
    ]
}

fn edge_time_strategy() -> impl Strategy<Value = i64> {
    prop_oneof![
        Just(0i64),
        Just(1i64),
        Just((SECONDS_PER_YEAR as i64) - 1),
        Just(SECONDS_PER_YEAR as i64),
        Just((SECONDS_PER_YEAR as i64) * 3 - 1),
        Just((SECONDS_PER_YEAR as i64) * 3),
        Just((SECONDS_PER_YEAR as i64) * 3 + 1),
        (0i64..=((SECONDS_PER_YEAR as i64) * 4)),
    ]
}

fn edge_scaled_supply_strategy() -> impl Strategy<Value = u128> {
    prop_oneof![
        Just(0u128),
        Just(1u128),
        Just(WAD - 1),
        Just(WAD),
        Just(WAD + 1),
        Just(u128::from(u64::MAX)),
        (0u128..=1_000_000_000_000u128),
    ]
}

fn edge_capacity_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(0u64),
        Just(1u64),
        Just(2u64),
        Just(100_000u64),
        Just(5_000_000u64),
        Just(10_000_000u64),
        Just(u64::MAX),
        (0u64..=10_000_000u64),
    ]
}

fn edge_borrow_amount_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(0u64),
        Just(1u64),
        Just(2u64),
        Just(100_000u64),
        Just(5_000_000u64),
        Just(10_000_000u64),
        Just(u64::MAX),
        (0u64..=10_000_000u64),
    ]
}

// ---------------------------------------------------------------------------
// Proptest: State isolation under random parameters
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn prop_state_isolation_random_params(
        rate_a in edge_bps_strategy(),
        rate_b in edge_bps_strategy(),
        fee_bps in edge_bps_strategy(),
        deposit_a in edge_scaled_supply_strategy(),
        time_elapsed in edge_time_strategy(),
    ) {
        let config = make_config(fee_bps);
        let maturity = SECONDS_PER_YEAR as i64 * 3;

        let mut market_a = make_market(rate_a, maturity, u64::MAX / 2);
        market_a.set_scaled_total_supply(deposit_a);

        let mut market_b = make_market(rate_b, maturity, u64::MAX / 2);
        market_b.set_scaled_total_supply(1_000_000_000u128);

        // Snapshot A and B
        let a_sf_before = market_a.scale_factor();
        let a_fees_before = market_a.accrued_protocol_fees();
        let a_ts_before = market_a.last_accrual_timestamp();
        // Snapshot B
        let b_sf = market_b.scale_factor();
        let b_fees = market_b.accrued_protocol_fees();
        let b_ts = market_b.last_accrual_timestamp();

        // Accrue only A
        let res = accrue_interest(&mut market_a, &config, time_elapsed);
        match res {
            Ok(()) => {
                let expected_now = core::cmp::min(time_elapsed, maturity);
                prop_assert_eq!(
                    market_a.last_accrual_timestamp(),
                    expected_now,
                    "accrual timestamp should be capped at maturity"
                );
                prop_assert!(
                    market_a.scale_factor() >= a_sf_before,
                    "scale factor must be monotonic non-decreasing"
                );
                prop_assert!(
                    market_a.accrued_protocol_fees() >= a_fees_before,
                    "accrued fees must be monotonic non-decreasing"
                );
            }
            Err(_) => {
                // On rejection, all market A fields should remain unchanged.
                prop_assert_eq!(market_a.scale_factor(), a_sf_before);
                prop_assert_eq!(market_a.accrued_protocol_fees(), a_fees_before);
                prop_assert_eq!(market_a.last_accrual_timestamp(), a_ts_before);
            }
        }

        // B unchanged
        prop_assert_eq!(market_b.scale_factor(), b_sf);
        prop_assert_eq!(market_b.accrued_protocol_fees(), b_fees);
        prop_assert_eq!(market_b.last_accrual_timestamp(), b_ts);
    }
}

// ---------------------------------------------------------------------------
// Proptest: Different rates produce different scale factors
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn prop_different_rates_different_outcomes(
        rate_low in prop_oneof![Just(0u16), Just(1u16), Just(2u16), Just(9_998u16), Just(9_999u16), (0u16..=9_999u16)],
        rate_extra in prop_oneof![Just(1u16), Just(2u16), Just(5_000u16), (1u16..=5_000u16)],
        fee_bps in edge_bps_strategy(),
        scaled_supply in edge_scaled_supply_strategy(),
        time_elapsed in edge_time_strategy(),
    ) {
        let rate_high = rate_low.saturating_add(rate_extra).min(10_000);
        if rate_high <= rate_low {
            return Ok(());
        }

        let config = make_config(fee_bps);
        let maturity = SECONDS_PER_YEAR as i64 * 3;

        let mut market_low = make_market(rate_low, maturity, u64::MAX / 2);
        market_low.set_scaled_total_supply(scaled_supply);
        let mut market_high = make_market(rate_high, maturity, u64::MAX / 2);
        market_high.set_scaled_total_supply(scaled_supply);

        let low_before = (
            market_low.scale_factor(),
            market_low.accrued_protocol_fees(),
            market_low.last_accrual_timestamp(),
        );
        let high_before = (
            market_high.scale_factor(),
            market_high.accrued_protocol_fees(),
            market_high.last_accrual_timestamp(),
        );

        let low_res = accrue_interest(&mut market_low, &config, time_elapsed);
        let high_res = accrue_interest(&mut market_high, &config, time_elapsed);

        match (low_res, high_res) {
            (Ok(()), Ok(())) => {
                let expected_now = core::cmp::min(time_elapsed, maturity);
                prop_assert_eq!(market_low.last_accrual_timestamp(), expected_now);
                prop_assert_eq!(market_high.last_accrual_timestamp(), expected_now);
                prop_assert!(
                    market_high.scale_factor() >= market_low.scale_factor(),
                    "higher rate ({}) should not yield lower scale_factor than lower rate ({}): high={}, low={}",
                    rate_high, rate_low, market_high.scale_factor(), market_low.scale_factor()
                );
                prop_assert!(
                    market_high.accrued_protocol_fees() >= market_low.accrued_protocol_fees(),
                    "higher rate ({}) should not yield lower fee accrual than lower rate ({}): high_fees={}, low_fees={}",
                    rate_high, rate_low, market_high.accrued_protocol_fees(), market_low.accrued_protocol_fees()
                );
            }
            (Ok(()), Err(_)) => {
                // Higher-rate path can overflow before lower-rate path.
                prop_assert_eq!(market_low.last_accrual_timestamp(), core::cmp::min(time_elapsed, maturity));
                prop_assert_eq!(market_high.scale_factor(), high_before.0);
                prop_assert_eq!(market_high.accrued_protocol_fees(), high_before.1);
                prop_assert_eq!(market_high.last_accrual_timestamp(), high_before.2);
            }
            (Err(_), Err(_)) => {
                // Both paths rejected; state must remain unchanged.
                prop_assert_eq!(market_low.scale_factor(), low_before.0);
                prop_assert_eq!(market_low.accrued_protocol_fees(), low_before.1);
                prop_assert_eq!(market_low.last_accrual_timestamp(), low_before.2);
                prop_assert_eq!(market_high.scale_factor(), high_before.0);
                prop_assert_eq!(market_high.accrued_protocol_fees(), high_before.1);
                prop_assert_eq!(market_high.last_accrual_timestamp(), high_before.2);
            }
            (Err(_), Ok(())) => {
                prop_assert!(
                    false,
                    "lower rate failed while higher rate succeeded: rate_low={}, rate_high={}, time_elapsed={}",
                    rate_low,
                    rate_high,
                    time_elapsed
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Proptest: Shared whitelist capacity is properly bounded
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn prop_shared_whitelist_capacity_bounded(
        capacity in edge_capacity_strategy(),
        borrow_a in edge_borrow_amount_strategy(),
        borrow_b in edge_borrow_amount_strategy(),
    ) {
        let mut wl = make_whitelist(capacity);

        // First borrow attempt.
        let before_first = wl.current_borrowed();
        let first_exceeds = would_exceed_capacity(&wl, borrow_a);
        let first_direct = match before_first.checked_add(borrow_a) {
            Some(total) => total > wl.max_borrow_capacity(),
            None => true,
        };
        prop_assert_eq!(
            first_exceeds, first_direct,
            "capacity oracle should match direct check for first borrow"
        );

        if !first_exceeds {
            let first_total = before_first.checked_add(borrow_a).unwrap();
            wl.set_current_borrowed(first_total);
            prop_assert_eq!(wl.current_borrowed(), first_total);
        } else {
            prop_assert_eq!(wl.current_borrowed(), before_first);
        }

        // Second borrow attempt.
        let before_second = wl.current_borrowed();
        let second_exceeds = would_exceed_capacity(&wl, borrow_b);
        let second_direct = match before_second.checked_add(borrow_b) {
            Some(total) => total > wl.max_borrow_capacity(),
            None => true,
        };
        prop_assert_eq!(
            second_exceeds, second_direct,
            "capacity oracle should match direct check for second borrow"
        );

        if !second_exceeds {
            let second_total = before_second.checked_add(borrow_b).unwrap();
            wl.set_current_borrowed(second_total);
            prop_assert_eq!(wl.current_borrowed(), second_total);
        } else {
            prop_assert_eq!(wl.current_borrowed(), before_second);
        }

        prop_assert!(
            wl.current_borrowed() <= wl.max_borrow_capacity(),
            "current_borrowed must remain bounded by max capacity"
        );
    }
}

// ---------------------------------------------------------------------------
// Proptest: Proportional fairness -- identical setup yields identical result
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn prop_proportional_fairness(
        rate in 1u16..=10_000u16,
        fee_bps in 0u16..=10_000u16,
        supply in 1u128..=1_000_000_000_000u128,
        time_elapsed in 1i64..=31_536_000i64,
    ) {
        let config = make_config(fee_bps);
        let maturity = SECONDS_PER_YEAR as i64 * 3;

        let mut market_a = make_market(rate, maturity, u64::MAX / 2);
        market_a.set_scaled_total_supply(supply);
        let mut market_b = make_market(rate, maturity, u64::MAX / 2);
        market_b.set_scaled_total_supply(supply);

        let res_a = accrue_interest(&mut market_a, &config, time_elapsed);
        let res_b = accrue_interest(&mut market_b, &config, time_elapsed);

        // Both should succeed or both should fail
        match (res_a, res_b) {
            (Ok(()), Ok(())) => {
                prop_assert_eq!(market_a.scale_factor(), market_b.scale_factor());
                prop_assert_eq!(market_a.accrued_protocol_fees(), market_b.accrued_protocol_fees());
                prop_assert_eq!(market_a.last_accrual_timestamp(), market_b.last_accrual_timestamp());
            }
            (Err(_), Err(_)) => { /* both failed, ok */ }
            _ => {
                prop_assert!(false, "identical markets should have identical success/failure");
            }
        }
    }
}
