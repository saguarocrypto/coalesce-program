//! Error code coverage tests for the CoalesceFi protocol.
//!
//! Tests specific error codes that previously lacked dedicated BPF integration
//! test coverage: ZeroScaledAmount (18), PayoutBelowMinimum (42),
//! SettlementNotComplete (33), FeesNotCollected (39), NoExcessToWithdraw (40),
//! BorrowerHasActiveDebt (9), RepaymentExceedsDebt (35), SettlementGracePeriod (32).

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

mod common;

use common::*;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------
const MATURITY_OFFSET: i64 = 365 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// Standard test setup
// ---------------------------------------------------------------------------
struct Setup {
    admin: Keypair,
    borrower: Keypair,
    lender: Keypair,
    whitelist_manager: Keypair,
    fee_authority: Keypair,
    blacklist_program: Pubkey,
    mint: Pubkey,
    mint_authority: Keypair,
    market: Pubkey,
    lender_ta: Pubkey,
    borrower_ta: Pubkey,
    fee_ta: Pubkey,
}

async fn setup(ctx: &mut solana_program_test::ProgramTestContext) -> Setup {
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(ctx, &[&admin, &borrower, &lender, &wm, &fa], 10_000_000_000).await;
    let mint = create_mint(ctx, &ma, 6).await;
    let lta = create_token_account(ctx, &mint, &lender.pubkey()).await;
    let bta = create_token_account(ctx, &mint, &borrower.pubkey()).await;
    let fta = create_token_account(ctx, &mint, &fa.pubkey()).await;

    mint_to_account(ctx, &mint, &lta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(ctx, &mint, &bta.pubkey(), &ma, 10_000_000_000).await;

    setup_protocol(ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(ctx, &bp, &lender.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + MATURITY_OFFSET;

    let market = setup_market_full(
        ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        1000,
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    Setup {
        admin,
        borrower,
        lender,
        whitelist_manager: wm,
        fee_authority: fa,
        blacklist_program: bp,
        mint,
        mint_authority: ma,
        market,
        lender_ta: lta.pubkey(),
        borrower_ta: bta.pubkey(),
        fee_ta: fta.pubkey(),
    }
}

// ===========================================================================
// Test: Error 17 — ZeroAmount
// Deposit with amount=0 triggers ZeroAmount validation error.
// Note: Error 18 (ZeroScaledAmount) requires scale_factor >> WAD which
// cannot be achieved in a normal BPF test without artificially injected state.
// ===========================================================================
#[tokio::test]
async fn test_error_17_zero_amount() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    let dep_zero = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        0,
    );
    send_expect_error(&mut ctx, dep_zero, &[&s.lender], 17).await;
}

// ===========================================================================
// Test: Error 42 — PayoutBelowMinimum
// Withdraw with min_payout set higher than actual payout.
// ===========================================================================
#[tokio::test]
async fn test_error_42_payout_below_minimum() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit
    let dep_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000, // 100 USDC
    );
    send_ok(&mut ctx, dep_ix, &[&s.lender]).await;

    // Borrow half (so vault is partially empty)
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    // Advance past maturity
    let market_data = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Get lender position
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);

    // Withdraw with absurdly high min_payout — should trigger error 42
    let withdraw_ix = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance,
        u64::MAX, // impossibly high min_payout
    );
    send_expect_error(&mut ctx, withdraw_ix, &[&s.lender], 42).await;
}

// ===========================================================================
// Test: Error 33 — SettlementNotComplete (withdraw_excess before settlement)
// withdraw_excess checks: (1) past maturity, (2) scaled_total_supply == 0,
// (3) settlement_factor_wad != 0. To hit check 3, we need a market where
// no deposits were made (supply == 0) but settlement never triggered (factor == 0).
// ===========================================================================
#[tokio::test]
async fn test_error_33_settlement_not_complete() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // DON'T deposit — scaled_total_supply stays 0

    // Advance past maturity + grace period
    let market_data = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Try withdraw_excess: scaled_total_supply == 0 (passes check 2),
    // settlement_factor_wad == 0 → error 33 (SettlementNotComplete)
    let we_ix = build_withdraw_excess(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
    );
    send_expect_error(&mut ctx, we_ix, &[&s.borrower], 33).await;
}

// ===========================================================================
// Test: Error 39 — FeesNotCollected (withdraw_excess before fee collection)
// To reach this check, we need settlement_factor == WAD (fully solvent).
// Interest accrues on deposits, so we must use repay_interest to bring the
// vault balance up to cover the normalized total (principal + interest).
// ===========================================================================
#[tokio::test]
async fn test_error_39_fees_not_collected() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit
    let dep_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep_ix, &[&s.lender]).await;

    // Borrow and repay principal
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    let repay_ix = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        50_000_000,
    );
    send_ok(&mut ctx, repay_ix, &[&s.borrower]).await;

    // Use repay_interest to add enough to cover accrued interest + fees
    // so settlement_factor will be WAD. 50M is generous for ~10% annual.
    let ri_ix = build_repay_interest_with_amount(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        50_000_000,
    );
    send_ok(&mut ctx, ri_ix, &[&s.borrower]).await;

    // Advance past maturity + grace period
    let market_data = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Withdraw all (triggers settlement — should get factor == WAD)
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw_ix = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    send_ok(&mut ctx, withdraw_ix, &[&s.lender]).await;

    // Verify settlement_factor == WAD (market is solvent)
    let mdata_after = get_account_data(&mut ctx, &s.market).await;
    let parsed_after = parse_market(&mdata_after);
    assert_eq!(
        parsed_after.settlement_factor_wad, 1_000_000_000_000_000_000u128,
        "settlement factor should be WAD after repay_interest"
    );

    // Try withdraw_excess BEFORE collecting fees → FeesNotCollected (39)
    let we_ix = build_withdraw_excess(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
    );
    send_expect_error(&mut ctx, we_ix, &[&s.borrower], 39).await;
}

// ===========================================================================
// Test: Error 40 — NoExcessToWithdraw
// Use a 0% interest market so no interest accrues, no fees accrue.
// After withdrawal, vault is empty → NoExcessToWithdraw (40).
// ===========================================================================
#[tokio::test]
async fn test_error_40_no_excess_to_withdraw() {
    let mut ctx = common::start_context().await;

    // Custom setup with 0% interest rate so vault == normalized after maturity
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &wm, &fa],
        10_000_000_000,
    )
    .await;
    let mint = create_mint(&mut ctx, &ma, 6).await;
    let lta = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let bta = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fta = create_token_account(&mut ctx, &mint, &fa.pubkey()).await;

    mint_to_account(&mut ctx, &mint, &lta.pubkey(), &ma, 10_000_000_000).await;

    // 0% fee rate so no protocol fees accrue
    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 0).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + MATURITY_OFFSET;

    // 0% annual interest → scale_factor stays at WAD, no fees accrue
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        0,
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    // Deposit 100M
    let dep_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lta.pubkey(),
        &mint,
        &bp,
        100_000_000,
    );
    send_ok(&mut ctx, dep_ix, &[&lender]).await;

    // Advance past maturity + grace period
    advance_clock_past(&mut ctx, maturity + 301).await;

    // Withdraw all (settlement_factor == WAD since 0% interest, vault == normalized)
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&market, &lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lta.pubkey(),
        &bp,
        pos.scaled_balance,
        0,
    );
    send_ok(&mut ctx, withdraw_ix, &[&lender]).await;

    // No fees to collect (0% fee rate → accrued_protocol_fees == 0)
    // withdraw_excess — vault is empty → NoExcessToWithdraw (40)
    let we_ix = build_withdraw_excess(&market, &borrower.pubkey(), &bta.pubkey(), &bp);
    send_expect_error(&mut ctx, we_ix, &[&borrower], 40).await;
}

// ===========================================================================
// Test: Error 35 — RepaymentExceedsDebt
// ===========================================================================
#[tokio::test]
async fn test_error_35_repayment_exceeds_debt() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit and borrow
    let dep_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep_ix, &[&s.lender]).await;

    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        10_000_000, // borrow 10 USDC
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    // Try to repay more than borrowed
    let repay_ix = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        20_000_000, // repay 20 > borrowed 10
    );
    send_expect_error(&mut ctx, repay_ix, &[&s.borrower], 35).await;
}

// ===========================================================================
// Test: Error 32 — SettlementGracePeriod
// Withdraw after maturity but before maturity + SETTLEMENT_GRACE_PERIOD (300s)
// should fail with error 32.
// ===========================================================================
#[tokio::test]
async fn test_error_32_settlement_grace_period() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit and borrow
    let dep_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep_ix, &[&s.lender]).await;

    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    // Advance to just past maturity but BEFORE grace period ends (maturity + 10s)
    let market_data = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 10).await;

    // Try to withdraw — should fail with SettlementGracePeriod (32)
    // because current_ts < maturity + 300s and settlement_factor_wad == 0
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw_ix = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance / 4,
        0,
    );
    send_expect_error(&mut ctx, withdraw_ix, &[&s.lender], 32).await;
}

// ===========================================================================
// Test: Error 9 — BorrowerHasActiveDebt
// De-whitelist borrower with outstanding debt.
// ===========================================================================
#[tokio::test]
async fn test_error_9_borrower_has_active_debt() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit and borrow
    let dep_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep_ix, &[&s.lender]).await;

    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    // Try to de-whitelist borrower who has active debt
    let dw_ix = build_set_borrower_whitelist(
        &s.whitelist_manager.pubkey(),
        &s.borrower.pubkey(),
        0, // is_whitelisted = false
        0,
    );
    send_expect_error(&mut ctx, dw_ix, &[&s.whitelist_manager], 9).await;
}
