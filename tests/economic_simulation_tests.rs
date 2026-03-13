//! Monte Carlo Economic Simulation Tests
//!
//! These tests simulate full economic scenarios for the CoalesceFi lending
//! protocol: deposits, borrows, repayments, interest accrual, settlement,
//! and withdrawal. All state tracking (vault balance, lender positions, etc.)
//! is done manually in the test to mirror what the on-chain program would do.
//!
//! Each scenario is deterministic with explicit amounts (no randomized inputs).
//! The real `accrue_interest` function from the crate is used for interest.

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

/// Create a zeroed Market with the most commonly needed fields set.
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
        "borrow would exceed global capacity"
    );

    // Update vault
    *vault_balance = vault_balance.checked_sub(amount).unwrap();

    // Update market
    let new_total_borrowed = market.total_borrowed().checked_add(amount).unwrap();
    market.set_total_borrowed(new_total_borrowed);

    // Update whitelist
    whitelist.set_current_borrowed(new_wl_total);
}

/// Simulate a repayment: adds to vault, updates market.
fn sim_repay(market: &mut Market, vault_balance: &mut u64, amount: u64) {
    *vault_balance = vault_balance.checked_add(amount).unwrap();
    let new_total_repaid = market.total_repaid().checked_add(amount).unwrap();
    market.set_total_repaid(new_total_repaid);
}

/// Compute the settlement factor given current vault state.
/// Follows the exact logic from `processor/withdraw.rs` and `processor/re_settle.rs`.
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
fn normalize(scaled_balance: u128, scale_factor: u128) -> u128 {
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
// Scenario 1: Full Repayment
//
// 5 lenders deposit varying amounts (100K - 1M USDC), borrower borrows 80%,
// full repayment at maturity. Each lender should get back at least their
// deposit (settlement_factor == WAD).
// ===========================================================================

#[test]
fn scenario_1_full_repayment() {
    let annual_bps: u16 = 1000; // 10% annual interest
    let fee_bps: u16 = 500; // 5% protocol fee on interest
    let maturity: i64 = SECONDS_PER_YEAR as i64; // 1 year from epoch 0
    let config = make_config(fee_bps);

    // Lender deposit amounts (USDC base units)
    let deposit_amounts: [u64; 5] = [
        100_000 * USDC,   // 100K
        250_000 * USDC,   // 250K
        500_000 * USDC,   // 500K
        750_000 * USDC,   // 750K
        1_000_000 * USDC, // 1M
    ];
    let total_deposits: u64 = deposit_amounts.iter().sum();
    let max_supply = total_deposits + 1_000 * USDC; // a little headroom

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut positions: Vec<LenderPosition> = (0..5).map(|_| LenderPosition::zeroed()).collect();

    // Step 1: All 5 lenders deposit
    for (i, &amount) in deposit_amounts.iter().enumerate() {
        sim_deposit(&mut market, &mut positions[i], &mut vault_balance, amount);
    }

    assert_eq!(vault_balance, total_deposits);
    assert_eq!(market.total_deposited(), total_deposits);

    // Step 2: Accrue interest halfway through the term (6 months)
    let midpoint = maturity / 2;
    accrue_interest(&mut market, &config, midpoint).unwrap();

    // Step 3: Borrower borrows 80% of deposits
    let borrow_amount = (total_deposits as u128 * 80 / 100) as u64;
    let mut whitelist = BorrowerWhitelist::zeroed();
    whitelist.is_whitelisted = 1;
    whitelist.set_max_borrow_capacity(borrow_amount + 1);
    sim_borrow(
        &mut market,
        &mut whitelist,
        &mut vault_balance,
        borrow_amount,
    );

    // Step 4: Accrue interest to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();

    // Step 5: Full repayment -- borrower repays the original borrow amount
    // plus enough to cover the interest that accrued
    let scale_factor = market.scale_factor();
    let total_normalized = normalize(market.scaled_total_supply(), scale_factor);
    let fees = u128::from(market.accrued_protocol_fees());

    // To achieve settlement_factor == WAD, vault must hold >= total_normalized + fees
    let needed = total_normalized.checked_add(fees).unwrap();
    let needed_u64 = u64::try_from(needed).unwrap();
    let repay_amount = if needed_u64 > vault_balance {
        needed_u64 - vault_balance
    } else {
        0
    };
    sim_repay(&mut market, &mut vault_balance, repay_amount);

    // Step 6: Settle the market
    let settlement_factor = compute_settlement_factor(&market, vault_balance);
    market.set_settlement_factor_wad(settlement_factor);

    // Assertions
    assert_eq!(
        settlement_factor, WAD,
        "Full repayment should yield settlement_factor == WAD"
    );

    // Each lender gets back at least their deposit
    for (i, &deposit_amount) in deposit_amounts.iter().enumerate() {
        let payout = compute_payout(&market, &positions[i]);
        assert!(
            payout >= deposit_amount,
            "Lender {} deposited {} but payout is only {}",
            i,
            deposit_amount,
            payout
        );
    }

    // Sum of all payouts should not exceed vault balance
    let total_payouts: u64 = positions.iter().map(|p| compute_payout(&market, p)).sum();
    assert!(
        u128::from(total_payouts) <= u128::from(vault_balance),
        "Total payouts {} exceed vault balance {}",
        total_payouts,
        vault_balance
    );
}

// ===========================================================================
// Scenario 2: Partial Default (50% repayment)
//
// Same setup as Scenario 1 but borrower only repays 50%.
// Each lender gets exactly their pro-rata share scaled by the settlement
// factor. No lender gets more than their proportional normalized amount.
// ===========================================================================

#[test]
fn scenario_2_partial_default() {
    let annual_bps: u16 = 1000; // 10%
    let fee_bps: u16 = 0; // 0% fee to simplify default math
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);

    let deposit_amounts: [u64; 5] = [
        100_000 * USDC,
        250_000 * USDC,
        500_000 * USDC,
        750_000 * USDC,
        1_000_000 * USDC,
    ];
    let total_deposits: u64 = deposit_amounts.iter().sum();
    let max_supply = total_deposits + 1_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut positions: Vec<LenderPosition> = (0..5).map(|_| LenderPosition::zeroed()).collect();

    // All lenders deposit
    for (i, &amount) in deposit_amounts.iter().enumerate() {
        sim_deposit(&mut market, &mut positions[i], &mut vault_balance, amount);
    }

    // Accrue interest to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();

    // Borrower borrows 80%
    let borrow_amount = (total_deposits as u128 * 80 / 100) as u64;
    // Note: borrow happens before maturity in real flow, but for the purpose
    // of this simulation we can set it directly since we've already accrued.
    // We'll adjust vault balance manually.
    let mut whitelist = BorrowerWhitelist::zeroed();
    whitelist.is_whitelisted = 1;
    whitelist.set_max_borrow_capacity(borrow_amount + 1);
    // Simulate the borrow by reducing vault
    vault_balance = vault_balance.checked_sub(borrow_amount).unwrap();
    market.set_total_borrowed(borrow_amount);
    whitelist.set_current_borrowed(borrow_amount);

    // Borrower only repays 50% of what was borrowed
    let repay_amount = borrow_amount / 2;
    sim_repay(&mut market, &mut vault_balance, repay_amount);

    // Settle
    let settlement_factor = compute_settlement_factor(&market, vault_balance);
    market.set_settlement_factor_wad(settlement_factor);

    // Settlement factor should be less than WAD (there's a shortfall)
    assert!(
        settlement_factor < WAD,
        "Partial default should produce settlement_factor < WAD, got {}",
        settlement_factor
    );

    // Compute the expected total normalized amount
    let scale_factor = market.scale_factor();
    let total_normalized = normalize(market.scaled_total_supply(), scale_factor);
    let expected_factor = {
        let raw = u128::from(vault_balance)
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
    };
    assert_eq!(
        settlement_factor, expected_factor,
        "settlement factor must match exact shortfall formula"
    );

    // Boundary-neighbor monotonicity around available lender funds.
    if vault_balance > 0 {
        let lower = compute_settlement_factor(&market, vault_balance - 1);
        assert!(
            lower <= settlement_factor,
            "x-1 available funds should not increase settlement factor"
        );
    }
    let higher = compute_settlement_factor(&market, vault_balance.saturating_add(1));
    assert!(
        higher >= settlement_factor,
        "x+1 available funds should not decrease settlement factor"
    );
    assert!(higher <= WAD, "settlement factor must remain capped at WAD");

    // Each lender's payout should be proportional to their share of total_normalized
    let total_payouts: u64 = positions.iter().map(|p| compute_payout(&market, p)).sum();
    let vault_u128 = u128::from(vault_balance);

    // Pro-rata: no lender gets more than their share
    for (i, position) in positions.iter().enumerate() {
        let lender_normalized = normalize(position.scaled_balance(), scale_factor);
        let payout = compute_payout(&market, position);

        // Payout should be approximately (lender_normalized / total_normalized) * available
        // Using settlement_factor: payout = lender_normalized * settlement_factor / WAD
        let expected_payout = lender_normalized
            .checked_mul(settlement_factor)
            .unwrap()
            .checked_div(WAD)
            .unwrap();
        let expected_payout_u64 = u64::try_from(expected_payout).unwrap();

        assert_eq!(
            payout, expected_payout_u64,
            "Lender {} payout mismatch: got {}, expected {}",
            i, payout, expected_payout_u64
        );

        // No lender gets more than their full normalized amount
        assert!(
            u128::from(payout) <= lender_normalized,
            "Lender {} payout {} exceeds normalized amount {}",
            i,
            payout,
            lender_normalized
        );
    }

    // Total payouts should not exceed vault balance
    assert!(
        u128::from(total_payouts) <= vault_u128,
        "Total payouts {} exceed vault balance {}",
        total_payouts,
        vault_balance
    );
    let ideal_total = total_normalized
        .checked_mul(settlement_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    let ideal_total_u64 = u64::try_from(ideal_total).unwrap();
    assert!(
        total_payouts <= ideal_total_u64,
        "sum of floored individual payouts must not exceed floored aggregate payout"
    );
    assert!(
        ideal_total_u64.saturating_sub(total_payouts) <= positions.len() as u64,
        "floor-rounding loss should be bounded by number of lenders"
    );

    // Verify the proportionality: each lender's share of total payouts matches
    // their share of total normalized supply
    for (i, position) in positions.iter().enumerate() {
        let lender_normalized = normalize(position.scaled_balance(), scale_factor);
        let payout = u128::from(compute_payout(&market, position));

        // lender_payout / total_payouts ~= lender_normalized / total_normalized
        // Cross multiply to avoid floating point:
        // lender_payout * total_normalized ~= lender_normalized * total_payouts
        let lhs = payout.checked_mul(total_normalized).unwrap();
        let rhs = lender_normalized
            .checked_mul(u128::from(total_payouts))
            .unwrap();

        // Allow rounding difference of at most 1 unit per lender per multiplication
        let diff = if lhs > rhs { lhs - rhs } else { rhs - lhs };
        assert!(
            diff <= total_normalized + u128::from(total_payouts),
            "Lender {} proportionality violated: lhs={}, rhs={}, diff={}",
            i,
            lhs,
            rhs,
            diff
        );
    }
}

// ===========================================================================
// Scenario 3: Fee Impact
//
// Two identical markets: one with 0% fee, one with 10% fee.
// Fees should reduce lender payouts and produce collectable fee amounts.
// ===========================================================================

#[test]
fn scenario_3_fee_impact() {
    let annual_bps: u16 = 1000; // 10%
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let deposit_amount: u64 = 1_000_000 * USDC; // 1M USDC
    let max_supply = deposit_amount + 1_000 * USDC;

    // --- Market A: 0% fee ---
    let config_a = make_config(0);
    let mut market_a = make_market(annual_bps, maturity, max_supply);
    let mut vault_a: u64 = 0;
    let mut pos_a = LenderPosition::zeroed();

    sim_deposit(&mut market_a, &mut pos_a, &mut vault_a, deposit_amount);
    accrue_interest(&mut market_a, &config_a, maturity).unwrap();

    let fees_a = market_a.accrued_protocol_fees();
    let sf_a = market_a.scale_factor();
    let normalized_a = normalize(pos_a.scaled_balance(), sf_a);

    // --- Market B: 10% fee ---
    let config_b = make_config(1000); // 10% = 1000 bps
    let mut market_b = make_market(annual_bps, maturity, max_supply);
    let mut vault_b: u64 = 0;
    let mut pos_b = LenderPosition::zeroed();

    sim_deposit(&mut market_b, &mut pos_b, &mut vault_b, deposit_amount);
    accrue_interest(&mut market_b, &config_b, maturity).unwrap();

    let fees_b = market_b.accrued_protocol_fees();
    let sf_b = market_b.scale_factor();
    let normalized_b = normalize(pos_b.scaled_balance(), sf_b);

    // Assertion 1: Both markets should have the same scale factor
    // (fees don't change the scale factor, only the accrued_protocol_fees)
    assert_eq!(
        sf_a, sf_b,
        "Scale factors should be identical regardless of fee rate"
    );

    // Assertion 2: Market A should have zero fees
    assert_eq!(fees_a, 0, "0% fee market should have zero fees");

    // Assertion 3: Market B should have non-zero fees
    assert!(
        fees_b > 0,
        "10% fee market should have non-zero fees, got {}",
        fees_b
    );

    // Assertion 4: Normalized amounts should be the same (same scale factor)
    assert_eq!(
        normalized_a, normalized_b,
        "Normalized amounts should be identical"
    );

    // Now settle both markets. Note: neither market had any borrowing or
    // repayment, so the vault only holds the original deposit. But interest
    // has grown the normalized total supply by ~10%. Therefore both markets
    // will have settlement_factor < WAD (the vault is underfunded relative
    // to the interest-grown claims). The key insight is that fees make
    // market B *even worse* for lenders than market A.
    let factor_a = compute_settlement_factor(&market_a, vault_a);
    market_a.set_settlement_factor_wad(factor_a);

    let factor_b = compute_settlement_factor(&market_b, vault_b);
    market_b.set_settlement_factor_wad(factor_b);

    // Assertion 5: Both factors should be < WAD because the vault holds
    // the original deposit but interest grew the normalized claims.
    assert!(
        factor_a < WAD,
        "0% fee market factor should be < WAD since vault doesn't grow with interest (got {})",
        factor_a
    );

    // Assertion 6: Market B factor should be strictly less than A because
    // fees are reserved from the vault, reducing available_for_lenders.
    assert!(
        factor_b < factor_a,
        "Fee market factor {} should be less than no-fee factor {} due to fee reservation",
        factor_b,
        factor_a
    );

    // Assertion 7: Payout from Market B is less than Market A
    let payout_a = compute_payout(&market_a, &pos_a);
    let payout_b = compute_payout(&market_b, &pos_b);
    assert!(
        payout_b < payout_a,
        "Fee market payout {} should be less than no-fee payout {}",
        payout_b,
        payout_a
    );

    // Assertion 8: Vault solvency -- payout should not exceed vault
    assert!(
        u128::from(payout_a) <= u128::from(vault_a),
        "Market A payout should not exceed vault"
    );
    assert!(
        u128::from(payout_b) + u128::from(fees_b) <= u128::from(vault_b),
        "Market B payout + fees should not exceed vault"
    );

    // Assertion 9: Verify fee amount using the exact on-chain formula.
    let expected_fee =
        fee_delta_after_elapsed(u128::from(deposit_amount), WAD, annual_bps, 1000, maturity);
    assert_eq!(
        fees_b, expected_fee,
        "Fee amount should match the exact on-chain formula"
    );
    assert!(fees_b > 0, "Fee collection amount must be > 0");
}

// ===========================================================================
// Scenario 4: Multiple Borrow-Repay Cycles
//
// Borrower borrows, repays, borrows again within limits.
// Whitelist.current_borrowed is cumulative. Vault solvency after each step.
// ===========================================================================

#[test]
fn scenario_4_multiple_borrow_repay_cycles() {
    let annual_bps: u16 = 500; // 5%
    let fee_bps: u16 = 0;
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);
    let deposit_amount: u64 = 1_000_000 * USDC;
    let max_supply = deposit_amount + 1_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut pos = LenderPosition::zeroed();

    // Deposit
    sim_deposit(&mut market, &mut pos, &mut vault_balance, deposit_amount);

    // Set up whitelist with enough capacity for multiple borrows
    let mut whitelist = BorrowerWhitelist::zeroed();
    whitelist.is_whitelisted = 1;
    whitelist.set_max_borrow_capacity(2_000_000 * USDC); // 2M lifetime cap

    // Cycle 1: borrow 200K, then repay 200K
    let borrow_1 = 200_000 * USDC;
    let t1 = SECONDS_PER_YEAR as i64 / 12; // 1 month in
    accrue_interest(&mut market, &config, t1).unwrap();

    sim_borrow(&mut market, &mut whitelist, &mut vault_balance, borrow_1);
    assert_eq!(whitelist.current_borrowed(), borrow_1);
    assert_eq!(market.total_borrowed(), borrow_1);
    // Vault solvency: vault should have deposit - borrow
    assert_eq!(vault_balance, deposit_amount - borrow_1);

    // Repay after 1 more month
    let t2 = 2 * SECONDS_PER_YEAR as i64 / 12;
    accrue_interest(&mut market, &config, t2).unwrap();
    sim_repay(&mut market, &mut vault_balance, borrow_1);
    assert_eq!(market.total_repaid(), borrow_1);
    // Vault restored to original deposit amount
    assert_eq!(vault_balance, deposit_amount);

    // Cycle 2: borrow 500K, then repay 500K
    let borrow_2 = 500_000 * USDC;
    let t3 = 3 * SECONDS_PER_YEAR as i64 / 12;
    accrue_interest(&mut market, &config, t3).unwrap();

    sim_borrow(&mut market, &mut whitelist, &mut vault_balance, borrow_2);

    // Whitelist current_borrowed is CUMULATIVE (never decremented)
    assert_eq!(
        whitelist.current_borrowed(),
        borrow_1 + borrow_2,
        "Whitelist current_borrowed should be cumulative"
    );
    assert_eq!(
        market.total_borrowed(),
        borrow_1 + borrow_2,
        "Market total_borrowed should be cumulative"
    );
    assert_eq!(vault_balance, deposit_amount - borrow_2);

    // Repay
    let t4 = 4 * SECONDS_PER_YEAR as i64 / 12;
    accrue_interest(&mut market, &config, t4).unwrap();
    sim_repay(&mut market, &mut vault_balance, borrow_2);
    assert_eq!(market.total_repaid(), borrow_1 + borrow_2);
    assert_eq!(vault_balance, deposit_amount);

    // Cycle 3: borrow 300K -- tests that we can borrow again after full repay
    let borrow_3 = 300_000 * USDC;
    let t5 = 5 * SECONDS_PER_YEAR as i64 / 12;
    accrue_interest(&mut market, &config, t5).unwrap();

    sim_borrow(&mut market, &mut whitelist, &mut vault_balance, borrow_3);
    assert_eq!(
        whitelist.current_borrowed(),
        borrow_1 + borrow_2 + borrow_3,
        "Whitelist current_borrowed should be cumulative across all cycles"
    );

    // Repay before maturity
    let t6 = 6 * SECONDS_PER_YEAR as i64 / 12;
    accrue_interest(&mut market, &config, t6).unwrap();
    sim_repay(&mut market, &mut vault_balance, borrow_3);
    assert_eq!(vault_balance, deposit_amount);

    // Final accrual to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();

    // Verify vault solvency at settlement
    let settlement_factor = compute_settlement_factor(&market, vault_balance);
    // No fees, vault has original deposit, scale_factor grew, so settlement < WAD
    // because the normalized supply is larger than vault (interest grew supply but
    // vault only has original tokens).
    // This is expected: interest makes the claim larger but vault doesn't grow without repayment.
    assert!(
        settlement_factor > 0,
        "Settlement factor should be positive"
    );
}

// ===========================================================================
// Scenario 5: Late Repayment / Re-settle
//
// Settlement happens with only 70% available. Borrower then repays remaining 30%.
// Re-settle should improve the factor. New factor > old factor, new factor <= WAD.
// ===========================================================================

#[test]
fn scenario_5_late_repayment_resettle() {
    let annual_bps: u16 = 0; // 0% interest (simplifies math for this test)
    let fee_bps: u16 = 0;
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);
    let deposit_amount: u64 = 1_000_000 * USDC;
    let max_supply = deposit_amount + 1_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut pos = LenderPosition::zeroed();

    sim_deposit(&mut market, &mut pos, &mut vault_balance, deposit_amount);

    // Accrue (no-op since 0% rate, but keeps timestamps consistent)
    accrue_interest(&mut market, &config, maturity / 2).unwrap();

    // Borrower borrows the full amount
    let mut whitelist = BorrowerWhitelist::zeroed();
    whitelist.is_whitelisted = 1;
    whitelist.set_max_borrow_capacity(deposit_amount + 1);
    sim_borrow(
        &mut market,
        &mut whitelist,
        &mut vault_balance,
        deposit_amount,
    );
    assert_eq!(vault_balance, 0);

    // At maturity, borrower has only repaid 70%
    accrue_interest(&mut market, &config, maturity).unwrap();
    let repay_70 = (deposit_amount as u128 * 70 / 100) as u64;
    sim_repay(&mut market, &mut vault_balance, repay_70);

    // First settlement with 70% available
    let initial_factor = compute_settlement_factor(&market, vault_balance);
    market.set_settlement_factor_wad(initial_factor);

    // Verify initial factor is approximately 0.7 * WAD
    let expected_70_pct = WAD * 70 / 100;
    assert_eq!(
        initial_factor, expected_70_pct,
        "Initial settlement factor should be ~70% of WAD"
    );
    assert!(
        initial_factor < WAD,
        "Initial factor should be less than WAD"
    );

    // Borrower repays the remaining 30%
    let repay_30 = deposit_amount - repay_70;
    sim_repay(&mut market, &mut vault_balance, repay_30);
    assert_eq!(vault_balance, deposit_amount);

    // Re-settle
    let new_factor = compute_settlement_factor(&market, vault_balance);

    // Assertions
    assert!(
        new_factor > initial_factor,
        "Re-settle factor {} should be greater than initial factor {}",
        new_factor,
        initial_factor
    );
    assert!(
        new_factor <= WAD,
        "Re-settle factor {} should not exceed WAD",
        new_factor
    );
    assert_eq!(
        new_factor, WAD,
        "After full repayment, re-settle factor should equal WAD"
    );

    // Update market with new factor
    market.set_settlement_factor_wad(new_factor);

    // Verify lender gets full payout
    let payout = compute_payout(&market, &pos);
    assert_eq!(
        payout, deposit_amount,
        "After full repayment via re-settle, lender should get full deposit back"
    );
}

// ===========================================================================
// Scenario 5b: Late Repayment with interest (non-trivial re-settle)
//
// Same concept but with interest so the math is more interesting.
// ===========================================================================

#[test]
fn scenario_5b_late_repayment_resettle_with_interest() {
    let annual_bps: u16 = 1000; // 10%
    let fee_bps: u16 = 0;
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);
    let deposit_amount: u64 = 1_000_000 * USDC;
    let max_supply = deposit_amount + 1_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut pos = LenderPosition::zeroed();

    sim_deposit(&mut market, &mut pos, &mut vault_balance, deposit_amount);

    // Borrower borrows 80% before maturity
    let borrow_amount = 800_000 * USDC;
    let mut whitelist = BorrowerWhitelist::zeroed();
    whitelist.is_whitelisted = 1;
    whitelist.set_max_borrow_capacity(borrow_amount + 1);
    sim_borrow(
        &mut market,
        &mut whitelist,
        &mut vault_balance,
        borrow_amount,
    );

    // Accrue to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();

    let scale_factor = market.scale_factor();
    let total_normalized = normalize(market.scaled_total_supply(), scale_factor);

    // Borrower partially repays (70% of total_normalized which is the "owed" amount)
    let partial_repay = (total_normalized * 70 / 100) as u64;
    sim_repay(&mut market, &mut vault_balance, partial_repay);

    // Initial settlement
    let initial_factor = compute_settlement_factor(&market, vault_balance);
    market.set_settlement_factor_wad(initial_factor);
    let expected_initial_factor = {
        let raw = u128::from(vault_balance)
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
    };
    assert_eq!(
        initial_factor, expected_initial_factor,
        "initial factor should match exact formula"
    );

    assert!(initial_factor < WAD, "Should be underfunded initially");
    assert!(initial_factor > 0, "Factor must be positive");
    let initial_payout = compute_payout(&market, &pos);
    let normalized_claim = normalize(pos.scaled_balance(), scale_factor);
    let expected_initial_payout = normalized_claim
        .checked_mul(initial_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    assert_eq!(
        u128::from(initial_payout),
        expected_initial_payout,
        "initial payout should use exact payout formula"
    );

    // Borrower repays more (another 20% of total_normalized)
    let additional_repay = (total_normalized * 20 / 100) as u64;
    sim_repay(&mut market, &mut vault_balance, additional_repay);

    // Re-settle
    let new_factor = compute_settlement_factor(&market, vault_balance);
    let lower_neighbor = compute_settlement_factor(&market, vault_balance.saturating_sub(1));
    let upper_neighbor = compute_settlement_factor(&market, vault_balance.saturating_add(1));

    assert!(
        new_factor > initial_factor,
        "Re-settle factor {} should exceed initial {}",
        new_factor,
        initial_factor
    );
    assert!(
        new_factor <= WAD,
        "Re-settle factor {} should not exceed WAD",
        new_factor
    );
    assert!(
        lower_neighbor <= new_factor && upper_neighbor >= new_factor,
        "x-1/x/x+1 available funds must preserve settlement-factor monotonicity"
    );
    assert_eq!(
        new_factor, WAD,
        "after sufficient late repayment, settlement factor should be fully restored"
    );

    market.set_settlement_factor_wad(new_factor);
    let payout_after_resettle = compute_payout(&market, &pos);
    let expected_full_payout = u64::try_from(normalized_claim).unwrap();
    assert_eq!(
        payout_after_resettle, expected_full_payout,
        "WAD settlement should restore full normalized claim"
    );
    assert!(
        payout_after_resettle >= initial_payout,
        "re-settle should not reduce lender payout"
    );
}

// ===========================================================================
// Scenario 6: Dust Amounts
//
// 10 lenders each deposit 1 USDC (1_000_000 base units).
// Rounding should not create or destroy more than N units (N = lender count).
// ===========================================================================

#[test]
fn scenario_6_dust_amounts() {
    let annual_bps: u16 = 1000; // 10%
    let fee_bps: u16 = 500; // 5%
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);
    let num_lenders: usize = 10;
    let deposit_per_lender: u64 = USDC; // 1 USDC each
    let total_deposits = deposit_per_lender * num_lenders as u64;
    let max_supply = total_deposits + 1_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut positions: Vec<LenderPosition> =
        (0..num_lenders).map(|_| LenderPosition::zeroed()).collect();

    // All 10 lenders deposit 1 USDC each
    for i in 0..num_lenders {
        sim_deposit(
            &mut market,
            &mut positions[i],
            &mut vault_balance,
            deposit_per_lender,
        );
    }

    assert_eq!(vault_balance, total_deposits);

    // Accrue interest to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();

    // Full repayment scenario: borrower would need to repay enough to cover
    // normalized total + fees. For dust test, we just keep vault as-is (no borrow).
    let settlement_factor = compute_settlement_factor(&market, vault_balance);
    market.set_settlement_factor_wad(settlement_factor);

    // Compute total payouts
    let total_payouts: u64 = positions.iter().map(|p| compute_payout(&market, p)).sum();

    // All lenders deposited the same amount, so all payouts should be equal
    let first_payout = compute_payout(&market, &positions[0]);
    for (i, position) in positions.iter().enumerate() {
        let payout = compute_payout(&market, position);
        assert_eq!(
            payout, first_payout,
            "Lender {} payout {} differs from lender 0 payout {}",
            i, payout, first_payout
        );
    }

    // Rounding check: total payouts should not exceed vault balance
    assert!(
        total_payouts <= vault_balance,
        "Total payouts {} exceed vault balance {}",
        total_payouts,
        vault_balance
    );

    // Value should not be created: payouts per lender should not exceed what
    // their normalized share could entitle them to from the vault
    let scale_factor = market.scale_factor();
    let total_normalized = normalize(market.scaled_total_supply(), scale_factor);

    // The difference between ideal payouts and actual payouts should be
    // at most N units (one rounding error per lender)
    let ideal_total = total_normalized
        .checked_mul(settlement_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    let ideal_total_u64 = u64::try_from(ideal_total).unwrap();

    let rounding_diff = if ideal_total_u64 > total_payouts {
        ideal_total_u64 - total_payouts
    } else {
        total_payouts - ideal_total_u64
    };

    let rounding_tolerance = 2 * num_lenders as u64;
    assert!(
        rounding_diff <= rounding_tolerance,
        "Rounding error {} exceeds tolerance of {} units (two floors per lender path)",
        rounding_diff,
        rounding_tolerance
    );

    // Verify no individual lender gains value from rounding
    for (i, position) in positions.iter().enumerate() {
        let individual_normalized = normalize(position.scaled_balance(), scale_factor);
        let individual_ideal = individual_normalized
            .checked_mul(settlement_factor)
            .unwrap()
            .checked_div(WAD)
            .unwrap();
        let payout = u128::from(compute_payout(&market, position));

        // Integer division floors, so payout <= ideal is expected
        assert!(
            payout <= individual_ideal,
            "Lender {} payout {} exceeds ideal {} -- rounding created value!",
            i,
            payout,
            individual_ideal
        );
    }
}

// ===========================================================================
// Scenario 6b: Dust Amounts with extreme small values
//
// 10 lenders deposit 1 base unit (0.000001 USDC) each.
// ===========================================================================

#[test]
fn scenario_6b_extreme_dust() {
    let annual_bps: u16 = 1000;
    let fee_bps: u16 = 0; // No fees for cleaner dust analysis
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);
    let num_lenders: usize = 10;
    let deposit_per_lender: u64 = 1; // 1 base unit = 0.000001 USDC
    let total_deposits = deposit_per_lender * num_lenders as u64;
    let max_supply = total_deposits + 1_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut positions: Vec<LenderPosition> =
        (0..num_lenders).map(|_| LenderPosition::zeroed()).collect();

    for i in 0..num_lenders {
        sim_deposit(
            &mut market,
            &mut positions[i],
            &mut vault_balance,
            deposit_per_lender,
        );
    }

    // Accrue interest
    accrue_interest(&mut market, &config, maturity).unwrap();
    let expected_scale_factor = scale_factor_after_elapsed(WAD, annual_bps, maturity);
    assert_eq!(
        market.scale_factor(),
        expected_scale_factor,
        "single-step annual accrual should match exact daily-compound growth"
    );
    let total_normalized = normalize(market.scaled_total_supply(), market.scale_factor());
    assert_eq!(
        total_normalized, 11,
        "ten 1-unit deposits should normalize to 11 after exact 10% annual growth"
    );

    let settlement_factor = compute_settlement_factor(&market, vault_balance);
    market.set_settlement_factor_wad(settlement_factor);
    let expected_settlement = (u128::from(vault_balance) * WAD) / total_normalized;
    assert_eq!(
        settlement_factor, expected_settlement,
        "dust settlement factor should match exact formula"
    );

    let total_payouts: u64 = positions.iter().map(|p| compute_payout(&market, p)).sum();
    for (idx, position) in positions.iter().enumerate() {
        assert_eq!(
            position.scaled_balance(),
            1,
            "each extreme-dust lender should hold exactly one scaled unit"
        );
        assert_eq!(
            compute_payout(&market, position),
            0,
            "each extreme-dust lender payout should floor to zero (idx={})",
            idx
        );
    }
    assert_eq!(
        total_payouts, 0,
        "split extreme-dust lenders should all floor to zero payout in this setup"
    );

    // Total payouts must not exceed vault
    assert!(
        total_payouts <= vault_balance,
        "Dust payouts {} exceed vault {}",
        total_payouts,
        vault_balance
    );

    // Rounding should not destroy more than N units
    let value_lost = vault_balance.saturating_sub(total_payouts);
    assert!(
        value_lost <= num_lenders as u64,
        "Dust rounding lost {} units, max allowed {} (one per lender)",
        value_lost,
        num_lenders
    );
    assert_eq!(
        value_lost, num_lenders as u64,
        "in this exact extreme-dust case, loss should hit the per-lender floor bound"
    );

    // Compare against unsplit aggregate claim to ensure no value is created by splitting.
    let aggregate_claim = total_normalized
        .checked_mul(settlement_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    let aggregate_claim_u64 = u64::try_from(aggregate_claim).unwrap();
    assert!(
        aggregate_claim_u64 <= vault_balance,
        "even aggregated floor payout must remain bounded by available funds"
    );
    assert!(
        vault_balance - aggregate_claim_u64 <= 1,
        "aggregated single-claim floor loss should be at most one unit"
    );
    assert!(
        total_payouts <= aggregate_claim_u64,
        "splitting into many dust lenders must not increase aggregate payout"
    );
}

// ===========================================================================
// Scenario 7: Maximum Capacity
//
// Fill market exactly to max_total_supply. Verify that one more unit
// would be rejected (by checking the math manually).
// ===========================================================================

#[test]
fn scenario_7_maximum_capacity() {
    let annual_bps: u16 = 0; // 0% interest keeps scale_factor at WAD
    let _fee_bps: u16 = 0;
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let max_supply: u64 = 1_000_000 * USDC; // 1M USDC cap

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut pos = LenderPosition::zeroed();

    // Deposit exactly max_supply
    sim_deposit(&mut market, &mut pos, &mut vault_balance, max_supply);

    // Verify the normalized total equals max_supply
    let scale_factor = market.scale_factor();
    let total_normalized = normalize(market.scaled_total_supply(), scale_factor);
    assert_eq!(
        total_normalized,
        u128::from(max_supply),
        "Normalized total should equal max_supply after exact deposit"
    );

    // Now try to deposit 1 more base unit -- check that it would exceed the cap
    let additional_amount: u128 = 1;
    let additional_scaled = additional_amount
        .checked_mul(WAD)
        .unwrap()
        .checked_div(scale_factor)
        .unwrap();

    let new_scaled_total = market
        .scaled_total_supply()
        .checked_add(additional_scaled)
        .unwrap();
    let new_normalized = new_scaled_total
        .checked_mul(scale_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    let max_supply_u128 = u128::from(max_supply);

    assert!(
        new_normalized > max_supply_u128,
        "One more unit should exceed cap: new_normalized={}, max={}",
        new_normalized,
        max_supply_u128
    );

    // x-1/x/x+1 boundary neighbors around exact cap.
    let mut market_x_minus_1 = make_market(annual_bps, maturity, max_supply);
    let mut vault_x_minus_1: u64 = 0;
    let mut pos_x_minus_1 = LenderPosition::zeroed();
    sim_deposit(
        &mut market_x_minus_1,
        &mut pos_x_minus_1,
        &mut vault_x_minus_1,
        max_supply - 1,
    );
    let norm_x_minus_1 = normalize(
        market_x_minus_1.scaled_total_supply(),
        market_x_minus_1.scale_factor(),
    );
    assert_eq!(
        norm_x_minus_1,
        u128::from(max_supply - 1),
        "x-1 deposit should leave one base unit of normalized headroom"
    );
    let plus_one_scaled = u128::from(1u64)
        .checked_mul(WAD)
        .unwrap()
        .checked_div(market_x_minus_1.scale_factor())
        .unwrap();
    let norm_hit_cap = market_x_minus_1
        .scaled_total_supply()
        .checked_add(plus_one_scaled)
        .unwrap()
        .checked_mul(market_x_minus_1.scale_factor())
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    assert_eq!(
        norm_hit_cap,
        u128::from(max_supply),
        "x-1 then +1 should hit cap exactly (not exceed)"
    );

    let mut market_x_plus_1 = make_market(annual_bps, maturity, max_supply);
    let mut vault_x_plus_1: u64 = 0;
    let mut pos_x_plus_1 = LenderPosition::zeroed();
    sim_deposit(
        &mut market_x_plus_1,
        &mut pos_x_plus_1,
        &mut vault_x_plus_1,
        max_supply + 1,
    );
    let norm_x_plus_1 = normalize(
        market_x_plus_1.scaled_total_supply(),
        market_x_plus_1.scale_factor(),
    );
    assert!(
        norm_x_plus_1 > u128::from(max_supply),
        "x+1 direct deposit should exceed cap in normalized units"
    );
}

// ===========================================================================
// Scenario 7b: Maximum Capacity with interest-induced scale factor growth
//
// Market starts with deposits, interest accrues (growing scale_factor),
// then we verify the cap is checked against normalized (not raw) amounts.
// ===========================================================================

#[test]
fn scenario_7b_maximum_capacity_with_interest() {
    let annual_bps: u16 = 1000; // 10%
    let fee_bps: u16 = 0;
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);
    let max_supply: u64 = 1_000_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut pos = LenderPosition::zeroed();

    // Deposit 900K (leaving 100K of cap room)
    let initial_deposit = 900_000 * USDC;
    sim_deposit(&mut market, &mut pos, &mut vault_balance, initial_deposit);

    // Accrue interest for 6 months -- scale_factor grows
    let half_year = maturity / 2;
    accrue_interest(&mut market, &config, half_year).unwrap();

    let scale_factor = market.scale_factor();
    assert!(scale_factor > WAD, "Scale factor should grow with interest");

    // The normalized total supply is now > 900K due to interest
    let total_normalized = normalize(market.scaled_total_supply(), scale_factor);
    assert!(
        total_normalized > u128::from(initial_deposit),
        "Interest should grow the normalized total"
    );

    // Calculate how much more we can deposit before hitting the cap
    let remaining_cap = u128::from(max_supply).saturating_sub(total_normalized);

    if remaining_cap > 0 {
        // We can deposit up to remaining_cap more (in normalized terms)
        // But note: depositing `remaining_cap` base units will produce fewer
        // scaled units due to the grown scale_factor, so the actual normalized
        // addition equals remaining_cap (approximately).
        let can_deposit = remaining_cap;
        let can_deposit_u64 = u64::try_from(can_deposit).unwrap();

        // This should succeed
        let mut pos2 = LenderPosition::zeroed();
        sim_deposit(&mut market, &mut pos2, &mut vault_balance, can_deposit_u64);

        let new_total_normalized = normalize(market.scaled_total_supply(), scale_factor);
        assert!(
            new_total_normalized <= u128::from(max_supply),
            "Deposit within cap should not exceed max_supply"
        );

        // Now one more unit should exceed
        let extra_scaled = u128::from(1u64)
            .checked_mul(WAD)
            .unwrap()
            .checked_div(scale_factor)
            .unwrap();

        if extra_scaled > 0 {
            let hypothetical_total = market.scaled_total_supply() + extra_scaled;
            let hypothetical_normalized = normalize(hypothetical_total, scale_factor);
            assert!(
                hypothetical_normalized > u128::from(max_supply),
                "One more unit should exceed cap after filling to max"
            );
        }
    }
}

// ===========================================================================
// Additional Scenario: Interest accrual precision across full lifecycle
//
// Verify that accrue_interest produces monotonically increasing scale_factor
// when called at multiple intermediate timestamps.
// ===========================================================================

#[test]
fn scenario_interest_monotonicity() {
    let annual_bps: u16 = 2000; // 20%
    let fee_bps: u16 = 1000; // 10%
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);

    let mut market = make_market(annual_bps, maturity, 10_000_000 * USDC);
    market.set_scaled_total_supply(5_000_000 * USDC as u128);
    market.set_total_deposited(5_000_000 * USDC);

    let mut prev_sf = market.scale_factor();
    let mut prev_fees = market.accrued_protocol_fees();
    let mut expected_sf = prev_sf;
    let mut expected_fees = prev_fees;
    let mut expected_last_accrual: i64 = 0;
    let scaled_supply = market.scaled_total_supply();

    // Accrue at 12 monthly intervals
    for month in 1..=12 {
        let ts = (SECONDS_PER_YEAR as i64) * month / 12;

        // Independent oracle step using exact same formulas as accrue_interest.
        let effective_now = core::cmp::min(ts, maturity);
        let dt = effective_now - expected_last_accrual;
        if dt > 0 {
            let sf_before = expected_sf;
            expected_sf = scale_factor_after_elapsed(expected_sf, annual_bps, dt);
            let fee_delta =
                fee_delta_after_elapsed(scaled_supply, sf_before, annual_bps, fee_bps, dt);
            expected_fees = expected_fees.checked_add(fee_delta).unwrap();
            expected_last_accrual = effective_now;
        }

        accrue_interest(&mut market, &config, ts).unwrap();

        let sf = market.scale_factor();
        let fees = market.accrued_protocol_fees();

        assert!(
            sf >= prev_sf,
            "Scale factor decreased at month {}: {} < {}",
            month,
            sf,
            prev_sf
        );
        assert!(
            fees >= prev_fees,
            "Fees decreased at month {}: {} < {}",
            month,
            fees,
            prev_fees
        );
        assert_eq!(
            sf, expected_sf,
            "scale factor should match independent oracle at month {}",
            month
        );
        assert_eq!(
            fees, expected_fees,
            "accrued fees should match independent oracle at month {}",
            month
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            expected_last_accrual,
            "last_accrual timestamp should match oracle at month {}",
            month
        );

        prev_sf = sf;
        prev_fees = fees;
    }

    // After full year of 20% interest, scale_factor should be WAD * 1.2
    let expected_sf = WAD + WAD / 5; // WAD * 1.20
                                     // Note: Due to monthly compounding (12 calls), actual will be slightly
                                     // higher than simple interest. Just verify it's close.
    assert!(
        prev_sf >= expected_sf,
        "Final scale_factor {} should be >= simple interest {} (compound effect)",
        prev_sf,
        expected_sf
    );

    // Verify scale_factor doesn't grow past maturity
    let sf_at_maturity = market.scale_factor();
    let fees_at_maturity = market.accrued_protocol_fees();
    let last_accrual_at_maturity = market.last_accrual_timestamp();
    accrue_interest(&mut market, &config, maturity + 100_000).unwrap();
    assert_eq!(
        market.scale_factor(),
        sf_at_maturity,
        "Scale factor should not grow past maturity"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        fees_at_maturity,
        "Fees should not grow past maturity"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        last_accrual_at_maturity,
        "last_accrual_timestamp should remain pinned at maturity"
    );
}

// ===========================================================================
// Additional Scenario: Settlement factor bounds
//
// Verify settlement factor is always in [1, WAD] across various vault states.
// ===========================================================================

#[test]
fn scenario_settlement_factor_bounds() {
    // Use a realistic scaled supply: 1M USDC in base units (u64-sized),
    // which when multiplied by WAD in normalize() stays within u128.
    let supply_base_units: u128 = 1_000_000 * USDC as u128; // 1M USDC in base units
    let mut market = make_market(0, SECONDS_PER_YEAR as i64, 10_000_000 * USDC);
    market.set_scaled_total_supply(supply_base_units);
    market.set_scale_factor(WAD);

    // Case 1: Vault empty
    let factor = compute_settlement_factor(&market, 0);
    assert_eq!(factor, 1, "Empty vault should produce factor = 1 (minimum)");

    // Case 2: Vault has 50% of normalized
    let total_normalized_u64 = u64::try_from(normalize(
        market.scaled_total_supply(),
        market.scale_factor(),
    ))
    .unwrap();
    let half_vault = total_normalized_u64 / 2;
    let factor = compute_settlement_factor(&market, half_vault);
    assert!(
        factor > 0 && factor <= WAD,
        "50% funded factor should be in (0, WAD]"
    );
    let expected_half = WAD / 2;
    assert_eq!(factor, expected_half, "50% funded should give WAD/2");

    // Case 3: Vault has exactly the normalized amount
    let factor = compute_settlement_factor(&market, total_normalized_u64);
    assert_eq!(factor, WAD, "Fully funded should give WAD");

    // Case 4: Vault has more than normalized (overfunded)
    let overfunded = total_normalized_u64 + 1_000_000 * USDC;
    let factor = compute_settlement_factor(&market, overfunded);
    assert_eq!(
        factor, WAD,
        "Overfunded vault should be capped at WAD, got {}",
        factor
    );

    // Case 5: Vault has 1 lamport
    let factor = compute_settlement_factor(&market, 1);
    assert!(factor >= 1, "Factor should be at least 1");
    assert!(factor <= WAD, "Factor should be at most WAD");
}

// ===========================================================================
// Additional Scenario: End-to-end with all 5 lenders withdrawing
//
// Verifies that sequential withdrawals properly deduct from scaled supply
// and all payouts sum to <= vault balance.
// ===========================================================================

#[test]
fn scenario_end_to_end_sequential_withdrawals() {
    let annual_bps: u16 = 800; // 8%
    let fee_bps: u16 = 200; // 2%
    let maturity: i64 = SECONDS_PER_YEAR as i64;
    let config = make_config(fee_bps);

    let deposit_amounts: [u64; 5] = [
        100_000 * USDC,
        200_000 * USDC,
        300_000 * USDC,
        400_000 * USDC,
        500_000 * USDC,
    ];
    let total_deposits: u64 = deposit_amounts.iter().sum(); // 1.5M
    let max_supply = total_deposits + 10_000 * USDC;

    let mut market = make_market(annual_bps, maturity, max_supply);
    let mut vault_balance: u64 = 0;
    let mut positions: Vec<LenderPosition> = (0..5).map(|_| LenderPosition::zeroed()).collect();

    // Deposits
    for (i, &amount) in deposit_amounts.iter().enumerate() {
        sim_deposit(&mut market, &mut positions[i], &mut vault_balance, amount);
    }

    // Borrow 50%
    let borrow_amount = total_deposits / 2;
    let mut whitelist = BorrowerWhitelist::zeroed();
    whitelist.is_whitelisted = 1;
    whitelist.set_max_borrow_capacity(borrow_amount + 1);
    sim_borrow(
        &mut market,
        &mut whitelist,
        &mut vault_balance,
        borrow_amount,
    );

    // Accrue to maturity
    accrue_interest(&mut market, &config, maturity).unwrap();

    // Full repayment: repay enough to cover everything
    let scale_factor = market.scale_factor();
    let total_normalized = normalize(market.scaled_total_supply(), scale_factor);
    let fees = u128::from(market.accrued_protocol_fees());
    let needed = u64::try_from(total_normalized.checked_add(fees).unwrap()).unwrap();
    if needed > vault_balance {
        let shortfall = needed - vault_balance;
        sim_repay(&mut market, &mut vault_balance, shortfall);
    }

    // Settle
    let settlement_factor = compute_settlement_factor(&market, vault_balance);
    market.set_settlement_factor_wad(settlement_factor);
    assert_eq!(settlement_factor, WAD, "Should be fully funded");

    // Sequential withdrawals
    let mut total_withdrawn: u64 = 0;
    for (i, position) in positions.iter_mut().enumerate() {
        let payout = compute_payout(&market, position);
        assert!(payout > 0, "Lender {} should have non-zero payout", i);

        // Simulate withdrawal: deduct from vault, clear position
        vault_balance = vault_balance.checked_sub(payout).unwrap();
        let scaled = position.scaled_balance();
        let new_market_total = market.scaled_total_supply().checked_sub(scaled).unwrap();
        market.set_scaled_total_supply(new_market_total);
        position.set_scaled_balance(0);

        total_withdrawn += payout;
    }

    // All positions should be zero
    for (i, position) in positions.iter().enumerate() {
        assert_eq!(
            position.scaled_balance(),
            0,
            "Lender {} position should be zero after withdrawal",
            i
        );
    }

    // Market scaled_total_supply should be zero
    assert_eq!(
        market.scaled_total_supply(),
        0,
        "Market scaled_total_supply should be zero after all withdrawals"
    );

    // Vault should still be non-negative (fees remain)
    assert!(
        vault_balance >= market.accrued_protocol_fees(),
        "Vault {} should still cover remaining fees {}",
        vault_balance,
        market.accrued_protocol_fees()
    );

    // Each lender got at least their deposit back (because settlement_factor == WAD)
    // We verify this by checking total_withdrawn >= total_deposits
    assert!(
        total_withdrawn >= total_deposits,
        "Total withdrawn {} should be >= total deposits {} (interest was earned)",
        total_withdrawn,
        total_deposits
    );
}
