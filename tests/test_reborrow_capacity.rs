//! Tests for non-accumulative borrowing model (re-borrow capacity).
//!
//! This module tests the functionality where:
//! - `current_borrowed` tracks outstanding debt (not lifetime total)
//! - On repay, `current_borrowed` is decremented
//! - Borrowers can re-borrow up to `max_borrow_capacity` after repaying
//!
//! Key scenarios tested:
//! 1. Basic borrow → repay → re-borrow cycle
//! 2. Partial repayments
//! 3. Overpayment handling (saturating_sub prevents underflow)
//! 4. Multiple borrow/repay cycles
//! 5. Third-party repayments
//! 6. Capacity tracking with multiple partial borrows

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
use solana_program_test::tokio;
use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

/// Test: Basic borrow → full repay → re-borrow cycle
///
/// Verifies that after full repayment, a borrower can borrow again
/// up to their max_borrow_capacity without admin intervention.
#[tokio::test]
async fn test_borrow_repay_reborrow_full_cycle() {
    let mut ctx = common::start_context().await;

    // Setup accounts
    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower, &lender],
        10_000_000_000,
    )
    .await;

    // Create mint and token accounts
    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    // Fund lender with tokens (need enough for $2M deposit with 6 decimals)
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000_000,
    )
    .await;

    // Initialize protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100, // 1% fee
    )
    .await;

    // Setup blacklist (not blacklisted)
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    // Create market with 1M capacity
    let max_borrow_capacity = 1_000_000_000_000u64; // $1M USDC (6 decimals)
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,   // nonce
        500, // 5% annual interest
        maturity_timestamp,
        10_000_000_000_000, // $10M max supply
        &whitelist_manager,
        max_borrow_capacity,
    )
    .await;

    // Lender deposits
    let deposit_amount = 2_000_000_000_000u64; // $2M
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );

    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // === FIRST BORROW: Borrow full capacity ===
    let borrow_amount = 500_000_000_000u64; // $500K
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify current_borrowed after first borrow
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, borrow_amount,
        "current_borrowed should equal borrow amount"
    );

    // === FULL REPAY ===
    // Fund borrower to repay (they have tokens from borrowing)
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        borrow_amount,
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify current_borrowed is now 0
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 0,
        "current_borrowed should be 0 after full repay"
    );

    // === RE-BORROW: Should succeed since capacity is restored ===
    let reborrow_amount = 400_000_000_000u64; // $400K
    let reborrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        reborrow_amount,
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[reborrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify current_borrowed after re-borrow
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, reborrow_amount,
        "current_borrowed should equal reborrow amount"
    );

    // Verify borrower received the tokens
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, reborrow_amount,
        "borrower should have reborrowed tokens"
    );
}

/// Test: Partial repayments correctly decrement current_borrowed
#[tokio::test]
async fn test_partial_repayments() {
    let mut ctx = common::start_context().await;

    // Setup (abbreviated for brevity)
    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower, &lender],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let max_capacity = 1_000_000_000u64; // $1K
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        2,
        500,
        maturity_timestamp,
        10_000_000_000,
        &whitelist_manager,
        max_capacity,
    )
    .await;

    // Lender deposits
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        5_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow $1K
    let borrow_amount = 1_000_000_000u64;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (vault_pda, _) = get_vault_pda(&market);
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, borrow_amount,
        "Borrower should hold full borrowed amount before repayments"
    );

    // Partial repay 1: $300
    let repay1 = 300_000_000u64;
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay1,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 700_000_000,
        "After $300 repay, should have $700 debt"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 700_000_000,
        "Borrower token balance should decrease by first partial repay"
    );

    // Partial repay 2: $500
    let repay2 = 500_000_000u64;
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay2,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 200_000_000,
        "After $500 more repay, should have $200 debt"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 200_000_000,
        "Borrower token balance should decrease by second partial repay"
    );

    // Now should be able to borrow $800 more (up to $1K capacity)
    let reborrow = 800_000_000u64;
    // Fund borrower to have enough tokens
    mint_to_account(&mut ctx, &mint, &borrower_token.pubkey(), &admin, reborrow).await;
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 1_000_000_000,
        "Borrower should hold debt remainder + minted buffer before reborrow boundary checks"
    );

    // Boundary x+1: borrowing one unit above available capacity must fail atomically.
    let snapshot_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    let borrower_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        reborrow + 1,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("Borrowing above remaining capacity should fail");
    assert_eq!(
        extract_custom_error(&err),
        Some(27),
        "Expected GlobalCapacityExceeded for x+1 over-borrow"
    );
    let snapshot_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let borrower_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_before, borrower_after,
        "Borrower token balance must not change on rejected over-borrow"
    );

    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        reborrow,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 1_000_000_000,
        "Should be at max capacity after reborrow"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 1_800_000_000,
        "Borrower balance should include pre-existing tokens plus successful reborrow"
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed, 1_800_000_000,
        "total_borrowed should be cumulative across initial borrow and reborrow"
    );
    assert_eq!(
        market_state.total_repaid, 800_000_000,
        "total_repaid should match cumulative partial repayments"
    );
}

/// Test: Repayment clears debt, extra funds via repay_interest
///
/// SR-116 now rejects repayment exceeding borrowed amount. This test
/// verifies proper repayment flow: repay borrowed amount, then use
/// repay_interest for additional funds.
#[tokio::test]
async fn test_overpayment_saturating_sub() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower, &lender],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        3,
        500,
        maturity_timestamp,
        10_000_000_000,
        &whitelist_manager,
        1_000_000_000,
    )
    .await;

    // Lender deposits
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        5_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow $500
    let borrow_amount = 500_000_000u64;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (vault_pda, _) = get_vault_pda(&market);
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, borrow_amount,
        "Borrow should increase outstanding debt by exact amount"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, borrow_amount,
        "Borrower should receive borrowed principal"
    );

    // Give borrower extra tokens
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 1_500_000_000,
        "Borrower should hold principal plus extra minted buffer"
    );

    // Boundary x+1: repay one unit above debt must fail atomically.
    let snapshot_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    let borrower_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let over_repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        borrow_amount + 1,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[over_repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("Repaying above outstanding debt should fail");
    assert_eq!(
        extract_custom_error(&err),
        Some(35),
        "Expected RepaymentExceedsDebt for over-repay"
    );
    let snapshot_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let borrower_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_before, borrower_after,
        "Borrower token balance must not change on rejected over-repay"
    );

    // Repay only borrowed amount (per SR-116)
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        borrow_amount, // Only repay what was borrowed
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 0, "Exact repay should clear debt");
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 1_000_000_000,
        "Borrower balance should decrease by exact principal repayment"
    );

    // Use repay_interest for extra funds
    let extra_amount = 300_000_000u64;
    let repay_interest_ix = build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        extra_amount,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify debt remains clear and interest repayment paths account correctly.
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 0, "Repayment should keep debt at zero");
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 700_000_000,
        "Borrower balance should decrease by interest repayment amount"
    );
    let vault_balance = get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_balance, 5_300_000_000,
        "Vault should retain deposit principal plus repaid principal and interest"
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_repaid,
        borrow_amount + extra_amount,
        "total_repaid should include principal and repay_interest inflows"
    );
    assert_eq!(
        market_state.total_interest_repaid, extra_amount,
        "total_interest_repaid should track repay_interest amount"
    );
}

/// Test: Third-party repayment decrements borrower's current_borrowed
///
/// Anyone can repay on behalf of the borrower, and it should decrement
/// the borrower's current_borrowed (not the payer's).
#[tokio::test]
async fn test_third_party_repayment() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let third_party = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower, &lender, &third_party],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let third_party_token = create_token_account(&mut ctx, &mint, &third_party.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000,
    )
    .await;
    mint_to_account(
        &mut ctx,
        &mint,
        &third_party_token.pubkey(),
        &admin,
        5_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);
    setup_blacklist_account(
        &mut ctx,
        &blacklist_program.pubkey(),
        &third_party.pubkey(),
        0,
    );

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        4,
        500,
        maturity_timestamp,
        10_000_000_000,
        &whitelist_manager,
        1_000_000_000,
    )
    .await;

    // Lender deposits
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        5_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrower borrows
    let borrow_amount = 800_000_000u64;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, borrow_amount,
        "Borrower should receive borrowed tokens before third-party repayments"
    );

    // Third party repays on behalf of borrower
    let repay_amount = 400_000_000u64;
    let repay_ix = build_repay(
        &market,
        &third_party.pubkey(), // payer is third party
        &third_party_token.pubkey(),
        &mint,
        &borrower.pubkey(), // but borrower's whitelist is updated
        repay_amount,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &third_party],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify borrower's current_borrowed decreased
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (vault_pda, _) = get_vault_pda(&market);
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 400_000_000,
        "Third-party repay should decrement borrower's debt"
    );

    // Verify third party's tokens were transferred
    let third_party_balance = get_token_balance(&mut ctx, &third_party_token.pubkey()).await;
    assert_eq!(
        third_party_balance, 4_600_000_000,
        "Third party should have paid 400M tokens"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, borrow_amount,
        "Third-party repayment must not change borrower token balance"
    );

    // Boundary x+1: one unit above remaining debt must fail atomically.
    let snapshot_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    let third_party_before = get_token_balance(&mut ctx, &third_party_token.pubkey()).await;
    let over_repay_ix = build_repay(
        &market,
        &third_party.pubkey(),
        &third_party_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount + 1,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[over_repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &third_party],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("Over-repay above remaining debt should fail");
    assert_eq!(
        extract_custom_error(&err),
        Some(35),
        "Expected RepaymentExceedsDebt for over-repay boundary"
    );
    let snapshot_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let third_party_after = get_token_balance(&mut ctx, &third_party_token.pubkey()).await;
    assert_eq!(
        third_party_before, third_party_after,
        "Third-party token balance must not change on rejected repay"
    );

    // Boundary neighbor: repay exact remaining debt succeeds and clears balance.
    let settle_ix = build_repay(
        &market,
        &third_party.pubkey(),
        &third_party_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[settle_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &third_party],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 0,
        "Exact remaining repay should clear debt"
    );
    let third_party_balance = get_token_balance(&mut ctx, &third_party_token.pubkey()).await;
    assert_eq!(
        third_party_balance, 4_200_000_000,
        "Third party should pay exact cumulative repay amount"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, borrow_amount,
        "Borrower token balance remains unchanged when third party repays"
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed, borrow_amount,
        "total_borrowed should track principal borrowed"
    );
    assert_eq!(
        market_state.total_repaid, borrow_amount,
        "total_repaid should track cumulative third-party repayments"
    );
}

/// Test: Multiple complete borrow/repay cycles
///
/// Verifies the system works correctly over many cycles without accumulation.
#[tokio::test]
async fn test_multiple_borrow_repay_cycles() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower, &lender],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        100_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let max_capacity = 1_000_000_000u64; // $1K
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        5,
        500,
        maturity_timestamp,
        100_000_000_000,
        &whitelist_manager,
        max_capacity,
    )
    .await;

    // Lender deposits large amount
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        50_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (vault_pda, _) = get_vault_pda(&market);
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());

    // Run 5 complete cycles with strict cumulative accounting checks.
    for cycle in 1..=5 {
        // Borrow full capacity
        let borrow_ix = build_borrow(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &blacklist_program.pubkey(),
            max_capacity,
        );
        ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[borrow_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let wl_data = get_account_data(&mut ctx, &wl_pda).await;
        let wl = parse_borrower_whitelist(&wl_data);
        assert_eq!(
            wl.current_borrowed, max_capacity,
            "Cycle {cycle}: debt should equal full capacity after borrow"
        );
        let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
        assert_eq!(
            borrower_balance, max_capacity,
            "Cycle {cycle}: borrower token balance should equal borrowed amount"
        );

        // Repay full amount
        let repay_ix = build_repay(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &mint,
            &borrower.pubkey(),
            max_capacity,
        );
        ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[repay_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let wl_data = get_account_data(&mut ctx, &wl_pda).await;
        let wl = parse_borrower_whitelist(&wl_data);
        assert_eq!(
            wl.current_borrowed, 0,
            "Cycle {cycle}: debt should be zero after full repay"
        );
        let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
        assert_eq!(
            borrower_balance, 0,
            "Cycle {cycle}: borrower token balance should return to zero after full repay"
        );
        let market_data = get_account_data(&mut ctx, &market).await;
        let market_state = parse_market(&market_data);
        assert_eq!(
            market_state.total_borrowed,
            cycle as u64 * max_capacity,
            "Cycle {cycle}: total_borrowed should accumulate exactly"
        );
        assert_eq!(
            market_state.total_repaid,
            cycle as u64 * max_capacity,
            "Cycle {cycle}: total_repaid should accumulate exactly"
        );
    }

    // Boundary x+1: one unit above capacity must fail atomically.
    let snapshot_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    let borrower_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let over_borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        max_capacity + 1,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[over_borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("Borrowing one unit above capacity should fail");
    assert_eq!(
        extract_custom_error(&err),
        Some(27),
        "Expected GlobalCapacityExceeded for over-capacity borrow"
    );
    let snapshot_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let borrower_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_before, borrower_after,
        "Borrower tokens must not change on rejected over-capacity borrow"
    );

    // Boundary neighbor x: exact-capacity borrow/repay remains valid.
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        max_capacity,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, max_capacity,
        "Exact-capacity borrow should succeed after prior cycles"
    );

    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        max_capacity,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 0, "Final repay should clear debt");
    assert_eq!(
        wl.max_borrow_capacity, max_capacity,
        "max_borrow_capacity should remain unchanged"
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed,
        6 * max_capacity,
        "Cumulative borrowed should include all six successful cycles"
    );
    assert_eq!(
        market_state.total_repaid,
        6 * max_capacity,
        "Cumulative repaid should include all six successful cycles"
    );
}

/// Test: Cannot borrow beyond capacity even after partial repay
///
/// If borrower has $800 debt with $1000 capacity, they can only borrow $200 more.
#[tokio::test]
async fn test_cannot_exceed_capacity_after_partial_repay() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower, &lender],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        100_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let max_capacity = 1_000_000_000u64; // $1K
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        6,
        500,
        maturity_timestamp,
        100_000_000_000,
        &whitelist_manager,
        max_capacity,
    )
    .await;

    // Lender deposits
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        50_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow $800
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        800_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Repay $300 (now have $500 debt, $500 available)
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        300_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 500_000_000, "Should have $500 debt");

    // Try to borrow $600 (should fail - would exceed capacity)
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        600_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "Should fail when trying to exceed capacity"
    );

    // Error code 27 = GlobalCapacityExceeded (reorganized error codes)
    let err = result.unwrap_err();
    let code = extract_custom_error(&err);
    assert_eq!(code, Some(27), "Should be GlobalCapacityExceeded error");

    // Borrow $500 (should succeed - exactly at capacity)
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        500_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 1_000_000_000,
        "Should be at full capacity"
    );
}

/// Test: Verify market.total_borrowed is cumulative while whitelist.current_borrowed is not
///
/// Market tracks lifetime totals, whitelist tracks current outstanding.
#[tokio::test]
async fn test_market_vs_whitelist_tracking() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower, &lender],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        100_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let capacity = 10_000_000_000u64; // $10K capacity
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        7,
        500,
        maturity_timestamp,
        100_000_000_000,
        &whitelist_manager,
        capacity,
    )
    .await;

    // Lender deposits
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        50_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (vault_pda, _) = get_vault_pda(&market);
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());

    // Cycle 1: Borrow $1K, repay $1K
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1_000_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed, 1_000_000_000,
        "After cycle 1 borrow, total_borrowed should be 1K"
    );
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 1_000_000_000,
        "After cycle 1 borrow, outstanding debt should be 1K"
    );

    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        1_000_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_repaid, 1_000_000_000,
        "After cycle 1 repay, total_repaid should be 1K"
    );
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 0,
        "After cycle 1 repay, outstanding debt should be zero"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 0,
        "Borrower token balance should net to zero after cycle 1"
    );

    // Cycle 2: Borrow $2K, repay $2K
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        2_000_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed, 3_000_000_000,
        "After cycle 2 borrow, total_borrowed should be cumulative 3K"
    );
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 2_000_000_000,
        "After cycle 2 borrow, outstanding debt should be 2K"
    );

    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        2_000_000_000,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed, 3_000_000_000,
        "total_borrowed should remain cumulative after second repay"
    );
    assert_eq!(
        market_state.total_repaid, 3_000_000_000,
        "total_repaid should be cumulative 3K after two cycles"
    );
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 0,
        "Outstanding debt should return to zero after second repay"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 0,
        "Borrower token balance should net to zero after cycle 2"
    );

    // Borrow full whitelist capacity and assert divergent cumulative/current tracking.
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        capacity,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed, 13_000_000_000,
        "total_borrowed should increase cumulatively to 13K"
    );
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, capacity,
        "current_borrowed should reflect only current outstanding debt"
    );

    // Boundary x+1 on remaining headroom (0): any additional borrow must fail atomically.
    let snapshot_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    let borrower_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let over_borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[over_borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("Borrowing above remaining headroom should fail");
    assert_eq!(
        extract_custom_error(&err),
        Some(27),
        "Expected GlobalCapacityExceeded at zero remaining headroom"
    );
    let snapshot_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let borrower_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_before, borrower_after,
        "Borrower token balance must not change on rejected over-capacity borrow"
    );

    // Repay full capacity and confirm final cumulative/current separation.
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        capacity,
    );
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    assert_eq!(
        market_state.total_borrowed, 13_000_000_000,
        "total_borrowed remains cumulative after final repay"
    );
    assert_eq!(
        market_state.total_repaid, 13_000_000_000,
        "total_repaid should catch up cumulatively after final repay"
    );
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 0,
        "current_borrowed should return to zero after all repayments"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 0,
        "Borrower token balance should net to zero after final repay"
    );
}
