//! Boundary condition tests for the CoalesceFi protocol.
//!
//! Tests extreme/boundary values: minimum deposit, full vault borrow,
//! exact repayment, and maturity delta boundaries.

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

// MIN_MATURITY_DELTA = 60 seconds, MAX_MATURITY_DELTA = 5 years
const MIN_MATURITY_DELTA: i64 = 60;
const MAX_MATURITY_DELTA: i64 = 5 * 365 * 24 * 60 * 60;

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
}

async fn setup_with_maturity(
    ctx: &mut solana_program_test::ProgramTestContext,
    maturity_delta: i64,
) -> Setup {
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

    mint_to_account(ctx, &mint, &lta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(ctx, &mint, &bta.pubkey(), &ma, 10_000_000_000).await;

    setup_protocol(ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(ctx, &bp, &lender.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + maturity_delta;

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
    }
}

// ===========================================================================
// Test 1: Deposit exactly 1 lamport
// ===========================================================================
#[tokio::test]
async fn test_deposit_one_lamport() {
    let mut ctx = common::start_context().await;
    let s = setup_with_maturity(&mut ctx, 365 * 86400).await;

    // Deposit exactly 1 token unit
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        1,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    // Verify non-zero scaled balance
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    assert!(
        pos.scaled_balance > 0,
        "deposit of 1 should yield non-zero scaled balance"
    );

    // Verify market total_deposited = 1
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    assert_eq!(parsed.total_deposited, 1);
}

// ===========================================================================
// Test 2: Borrow entire vault balance
// ===========================================================================
#[tokio::test]
async fn test_borrow_entire_vault() {
    let mut ctx = common::start_context().await;
    let s = setup_with_maturity(&mut ctx, 365 * 86400).await;

    // Deposit 100 USDC
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    // Borrow the full deposit amount.
    // With 0 accrued fees, borrowable = vault_balance - min(vault, fees) = 100M - 0 = 100M.
    // So borrowing exactly 100M must succeed.
    let borrow = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, borrow, &[&s.borrower]).await;

    // Verify total_borrowed
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    assert_eq!(parsed.total_borrowed, 100_000_000);
}

// ===========================================================================
// Test 3: Repay exact debt — total_borrowed goes to 0
// ===========================================================================
#[tokio::test]
async fn test_repay_exact_debt() {
    let mut ctx = common::start_context().await;
    let s = setup_with_maturity(&mut ctx, 365 * 86400).await;

    // Deposit and borrow
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    let borrow_amount = 50_000_000u64;
    let borrow = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        borrow_amount,
    );
    send_ok(&mut ctx, borrow, &[&s.borrower]).await;

    // Repay exactly the borrowed amount
    let repay = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        borrow_amount,
    );
    send_ok(&mut ctx, repay, &[&s.borrower]).await;

    // Verify repayment tracked correctly
    // Note: total_borrowed is cumulative (not decremented on repay).
    // Net outstanding debt = total_borrowed - total_repaid
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    assert_eq!(
        parsed.total_borrowed, borrow_amount,
        "total_borrowed is cumulative"
    );
    assert_eq!(
        parsed.total_repaid, borrow_amount,
        "total_repaid should match"
    );
    assert_eq!(
        parsed.total_borrowed - parsed.total_repaid,
        0,
        "net outstanding debt should be 0 after exact repay"
    );
}

// ===========================================================================
// Test 4: Create market at MIN and MAX maturity delta boundaries
// ===========================================================================
#[tokio::test]
async fn test_create_market_min_max_maturity_delta() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &borrower, &wm, &fa], 10_000_000_000).await;
    let mint = create_mint(&mut ctx, &ma, 6).await;

    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);

    // Whitelist borrower
    let wl_ix = build_set_borrower_whitelist(&wm.pubkey(), &borrower.pubkey(), 1, 1_000_000_000);
    send_ok(&mut ctx, wl_ix, &[&wm]).await;

    // Create market at MIN_MATURITY_DELTA + 1
    // The program uses strict inequality: maturity_timestamp > current_ts + MIN_MATURITY_DELTA
    let create_min = build_create_market(
        &borrower.pubkey(),
        &mint,
        &bp,
        1,
        1000,
        common::PINNED_EPOCH + MIN_MATURITY_DELTA + 1,
        1_000_000_000,
    );
    send_ok(&mut ctx, create_min, &[&borrower]).await;

    // Verify market was created
    let (market_min, _) = get_market_pda(&borrower.pubkey(), 1);
    let mdata = get_account_data(&mut ctx, &market_min).await;
    let parsed = parse_market(&mdata);
    assert_eq!(parsed.annual_interest_bps, 1000);

    // Create market at MAX_MATURITY_DELTA
    let create_max = build_create_market(
        &borrower.pubkey(),
        &mint,
        &bp,
        2,
        1000,
        common::PINNED_EPOCH + MAX_MATURITY_DELTA,
        1_000_000_000,
    );
    send_ok(&mut ctx, create_max, &[&borrower]).await;

    // Verify market was created
    let (market_max, _) = get_market_pda(&borrower.pubkey(), 2);
    let mdata2 = get_account_data(&mut ctx, &market_max).await;
    let parsed2 = parse_market(&mdata2);
    assert_eq!(parsed2.annual_interest_bps, 1000);
}
