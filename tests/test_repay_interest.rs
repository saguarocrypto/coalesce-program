//! Tests for the repay_interest instruction (disc 17).
//!
//! This instruction allows repaying accrued interest WITHOUT affecting the
//! borrower's `current_borrowed` capacity. This prevents the exploit where
//! interest payments incorrectly free up borrowing capacity.

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
use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

/// Test that repay_interest works and doesn't affect current_borrowed
#[tokio::test]
async fn test_repay_interest_does_not_affect_capacity() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new().pubkey();

    // Fund accounts
    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    // Initialize protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program,
        500, // 5% fee
    )
    .await;

    // Setup blacklist (not blacklisted)
    setup_blacklist_account(&mut ctx, &blacklist_program, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program, &lender.pubkey(), 0);

    // Create mint and token accounts
    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    // Fund lender with tokens
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000_000,
    )
    .await;

    // Get fresh blockhash
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let maturity = common::PINNED_EPOCH + 86400 * 365; // 1 year ahead (within max maturity delta)
    let max_borrow_capacity = 1_000_000_000_000u64; // $1M capacity

    // Setup market with borrower whitelisted
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program,
        1,    // nonce
        1000, // 10% annual interest
        maturity,
        2_000_000_000_000, // $2M max supply
        &whitelist_manager,
        max_borrow_capacity,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Lender deposits $1M
    let deposit_amount = 1_000_000_000_000u64; // $1M
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program,
        deposit_amount,
    );
    let deposit_tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(deposit_tx)
        .await
        .unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Borrower borrows $500K (half of capacity)
    let borrow_amount = 500_000_000_000u64; // $500K
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program,
        borrow_amount,
    );
    let borrow_tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(borrow_tx)
        .await
        .unwrap();

    // Check current_borrowed is now $500K
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, borrow_amount,
        "current_borrowed should be $500K after borrowing"
    );

    // Check market state before interest repayment
    let market_data_before = get_account_data(&mut ctx, &market).await;
    let market_before = parse_market(&market_data_before);
    let total_repaid_before = market_before.total_repaid;
    let total_interest_repaid_before = market_before.total_interest_repaid;
    assert_eq!(
        total_interest_repaid_before, 0,
        "total_interest_repaid should start at 0"
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Repay interest (simulating $50K interest payment)
    let interest_amount = 50_000_000_000u64; // $50K
    let repay_interest_ix = build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let repay_interest_tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(repay_interest_tx)
        .await
        .unwrap();

    // Verify current_borrowed is UNCHANGED (key assertion!)
    let wl_data_after = get_account_data(&mut ctx, &wl_pda).await;
    let wl_after = parse_borrower_whitelist(&wl_data_after);
    assert_eq!(
        wl_after.current_borrowed, borrow_amount,
        "current_borrowed should be UNCHANGED after repay_interest"
    );

    // Verify market tracking is updated
    let market_data_after = get_account_data(&mut ctx, &market).await;
    let market_after = parse_market(&market_data_after);

    assert_eq!(
        market_after.total_repaid,
        total_repaid_before + interest_amount,
        "total_repaid should include interest payment"
    );
    assert_eq!(
        market_after.total_interest_repaid, interest_amount,
        "total_interest_repaid should track interest payment"
    );
}

/// Test that repay_interest fails with zero amount
#[tokio::test]
async fn test_repay_interest_zero_amount_fails() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new().pubkey();

    // Fund accounts
    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    // Initialize protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program,
        500,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program, &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let maturity = common::PINNED_EPOCH + 86400 * 365; // 1 year ahead (within max maturity delta)

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program,
        1,
        1000,
        maturity,
        2_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Deposit to have funds in vault
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program,
        1_000_000_000_000,
    );
    let deposit_tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(deposit_tx)
        .await
        .unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let (vault_pda, _) = get_vault_pda(&market);
    let (lender_pos_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    let borrower_balance_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let market_before_data = get_account_data(&mut ctx, &market).await;
    let market_before = parse_market(&market_before_data);

    // Try to repay interest with zero amount
    let repay_interest_ix = build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        0, // Zero amount should fail
    );
    let repay_interest_tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(repay_interest_tx)
        .await;

    // Should fail with ZeroAmount error (code 11)
    assert!(
        result.is_err(),
        "repay_interest with zero amount should fail"
    );
    let err = result.unwrap_err();
    let code = extract_custom_error(&err);
    assert_eq!(
        code,
        Some(17),
        "expected ZeroAmount error (17), got {:?}",
        code
    );

    let snapshot_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let borrower_balance_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance_after, borrower_balance_before,
        "borrower token balance changed on failed zero-amount repay_interest"
    );
    let market_after_data = get_account_data(&mut ctx, &market).await;
    let market_after = parse_market(&market_after_data);
    assert_eq!(
        market_after.total_repaid, market_before.total_repaid,
        "total_repaid changed on failed zero-amount repay_interest"
    );
    assert_eq!(
        market_after.total_interest_repaid, market_before.total_interest_repaid,
        "total_interest_repaid changed on failed zero-amount repay_interest"
    );

    // Boundary neighbor: amount=1 should succeed and update balances/market exactly.
    mint_to_account(&mut ctx, &mint, &borrower_token.pubkey(), &admin, 1).await;
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let borrower_balance_before_one = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let vault_balance_before_one = get_token_balance(&mut ctx, &vault_pda).await;
    let market_before_one_data = get_account_data(&mut ctx, &market).await;
    let market_before_one = parse_market(&market_before_one_data);

    let repay_interest_ix =
        build_repay_interest_with_amount(&market, &borrower.pubkey(), &borrower_token.pubkey(), 1);
    let repay_interest_tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(repay_interest_tx)
        .await
        .unwrap();

    let borrower_balance_after_one = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let vault_balance_after_one = get_token_balance(&mut ctx, &vault_pda).await;
    let market_after_one_data = get_account_data(&mut ctx, &market).await;
    let market_after_one = parse_market(&market_after_one_data);
    assert_eq!(
        borrower_balance_after_one,
        borrower_balance_before_one - 1,
        "borrower token balance should decrease by exactly 1 on successful repay_interest"
    );
    assert_eq!(
        vault_balance_after_one,
        vault_balance_before_one + 1,
        "vault balance should increase by exactly 1 on successful repay_interest"
    );
    assert_eq!(
        market_after_one.total_repaid,
        market_before_one.total_repaid + 1,
        "total_repaid should increase by exactly 1 on successful repay_interest"
    );
    assert_eq!(
        market_after_one.total_interest_repaid,
        market_before_one.total_interest_repaid + 1,
        "total_interest_repaid should increase by exactly 1 on successful repay_interest"
    );
}

/// Test that repay_interest works from a third party (anyone can pay interest)
#[tokio::test]
async fn test_repay_interest_by_third_party() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let third_party = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new().pubkey();

    // Fund accounts
    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &third_party, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program,
        500,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program, &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let third_party_token = create_token_account(&mut ctx, &mint, &third_party.pubkey()).await;

    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000_000,
    )
    .await;
    mint_to_account(
        &mut ctx,
        &mint,
        &third_party_token.pubkey(),
        &admin,
        1_000_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let maturity = common::PINNED_EPOCH + 86400 * 365; // 1 year ahead (within max maturity delta)

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program,
        1,
        1000,
        maturity,
        2_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program,
        1_000_000_000_000,
    );
    let deposit_tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(deposit_tx)
        .await
        .unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Borrow first so we can prove third-party interest repayment does not reduce debt capacity.
    let borrow_amount = 400_000_000_000u64;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program,
        borrow_amount,
    );
    let borrow_tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(borrow_tx)
        .await
        .unwrap();

    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl_before = parse_borrower_whitelist(&wl_before_data);
    assert_eq!(
        wl_before.current_borrowed, borrow_amount,
        "borrower debt should reflect initial borrow before third-party interest repayment"
    );

    let (vault_pda, _) = get_vault_pda(&market);
    let third_party_before = get_token_balance(&mut ctx, &third_party_token.pubkey()).await;
    let borrower_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault_pda).await;
    let market_before_data = get_account_data(&mut ctx, &market).await;
    let market_before = parse_market(&market_before_data);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Third party pays interest on behalf of borrower
    let interest_amount = 100_000_000_000u64; // $100K
    let repay_interest_ix = build_repay_interest_with_amount(
        &market,
        &third_party.pubkey(),
        &third_party_token.pubkey(),
        interest_amount,
    );
    let repay_interest_tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &third_party],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(repay_interest_tx)
        .await
        .unwrap();

    // Verify exact side effects and invariants.
    let third_party_after = get_token_balance(&mut ctx, &third_party_token.pubkey()).await;
    let borrower_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let vault_after = get_token_balance(&mut ctx, &vault_pda).await;
    let market_data = get_account_data(&mut ctx, &market).await;
    let market_state = parse_market(&market_data);
    let wl_after_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl_after = parse_borrower_whitelist(&wl_after_data);

    assert_eq!(
        third_party_after,
        third_party_before - interest_amount,
        "third-party payer token balance should decrease by interest amount"
    );
    assert_eq!(
        borrower_after, borrower_before,
        "borrower token balance should be unchanged by third-party interest repayment"
    );
    assert_eq!(
        vault_after,
        vault_before + interest_amount,
        "vault balance should increase by interest payment amount"
    );
    assert_eq!(
        wl_after.current_borrowed, borrow_amount,
        "third-party repay_interest must not change current_borrowed"
    );
    assert_eq!(
        market_state.total_interest_repaid,
        market_before.total_interest_repaid + interest_amount,
        "total_interest_repaid should track third party payment"
    );
    assert_eq!(
        market_state.total_repaid,
        market_before.total_repaid + interest_amount,
        "total_repaid should include third-party interest payment"
    );
}

/// Test that regular repay still affects current_borrowed (for comparison)
#[tokio::test]
async fn test_regular_repay_still_affects_capacity() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new().pubkey();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program,
        500,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program, &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let maturity = common::PINNED_EPOCH + 86400 * 365; // 1 year ahead (within max maturity delta)

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program,
        1,
        1000,
        maturity,
        2_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program,
        1_000_000_000_000,
    );
    let deposit_tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(deposit_tx)
        .await
        .unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let borrow_amount = 500_000_000_000u64;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program,
        borrow_amount,
    );
    let borrow_tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(borrow_tx)
        .await
        .unwrap();

    // Check current_borrowed
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, borrow_amount);

    let (vault_pda, _) = get_vault_pda(&market);
    let (lender_pos_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let borrower_balance_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let vault_balance_before = get_token_balance(&mut ctx, &vault_pda).await;
    let market_before_data = get_account_data(&mut ctx, &market).await;
    let market_before = parse_market(&market_before_data);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Regular repay (should reduce current_borrowed)
    let repay_amount = 200_000_000_000u64;
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );
    let repay_tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(repay_tx)
        .await
        .unwrap();

    // Verify current_borrowed WAS reduced (contrast with repay_interest)
    let wl_data_after = get_account_data(&mut ctx, &wl_pda).await;
    let wl_after = parse_borrower_whitelist(&wl_data_after);
    assert_eq!(
        wl_after.current_borrowed,
        borrow_amount - repay_amount,
        "regular repay should reduce current_borrowed"
    );
    let borrower_balance_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let vault_balance_after = get_token_balance(&mut ctx, &vault_pda).await;
    let market_after_data = get_account_data(&mut ctx, &market).await;
    let market_after = parse_market(&market_after_data);
    assert_eq!(
        borrower_balance_after,
        borrower_balance_before - repay_amount,
        "borrower token balance should decrease by repay amount"
    );
    assert_eq!(
        vault_balance_after,
        vault_balance_before + repay_amount,
        "vault balance should increase by repay amount"
    );
    assert_eq!(
        market_after.total_repaid,
        market_before.total_repaid + repay_amount,
        "total_repaid should increase by repay amount"
    );
    assert_eq!(
        market_after.total_interest_repaid, market_before.total_interest_repaid,
        "regular repay must not change total_interest_repaid"
    );

    // Boundary neighbor: repaying remaining debt + 1 must fail atomically.
    mint_to_account(&mut ctx, &mint, &borrower_token.pubkey(), &admin, 1).await;
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let overpay_amount = (borrow_amount - repay_amount) + 1;
    let snapshot_before_overpay =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    let borrower_before_overpay = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let wl_before_overpay_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl_before_overpay = parse_borrower_whitelist(&wl_before_overpay_data);
    let market_before_overpay_data = get_account_data(&mut ctx, &market).await;
    let market_before_overpay = parse_market(&market_before_overpay_data);

    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        overpay_amount,
    );
    let repay_tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(repay_tx)
        .await
        .unwrap_err();
    let code = extract_custom_error(&err);
    assert_eq!(
        code,
        Some(35),
        "expected RepaymentExceedsDebt error (35), got {:?}",
        code
    );

    let snapshot_after_overpay =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before_overpay.assert_unchanged(&snapshot_after_overpay);
    let borrower_after_overpay = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_after_overpay, borrower_before_overpay,
        "borrower token balance changed on failed over-repay"
    );
    let wl_after_overpay_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl_after_overpay = parse_borrower_whitelist(&wl_after_overpay_data);
    assert_eq!(
        wl_after_overpay.current_borrowed, wl_before_overpay.current_borrowed,
        "current_borrowed changed on failed over-repay"
    );
    let market_after_overpay_data = get_account_data(&mut ctx, &market).await;
    let market_after_overpay = parse_market(&market_after_overpay_data);
    assert_eq!(
        market_after_overpay.total_repaid, market_before_overpay.total_repaid,
        "total_repaid changed on failed over-repay"
    );
    assert_eq!(
        market_after_overpay.total_interest_repaid, market_before_overpay.total_interest_repaid,
        "total_interest_repaid changed on failed over-repay"
    );
}

/// Test the exploit scenario: interest payments should NOT free up capacity
#[tokio::test]
async fn test_interest_exploit_prevented() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new().pubkey();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program,
        500,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program, &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000_000_000_000,
    )
    .await;
    // Give borrower enough to pay interest
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        1_000_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let maturity = common::PINNED_EPOCH + 86400 * 365; // 1 year ahead (within max maturity delta)
    let max_borrow_capacity = 1_000_000_000_000u64; // $1M capacity

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program,
        1,
        1000,
        maturity,
        2_000_000_000_000,
        &whitelist_manager,
        max_borrow_capacity,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Lender deposits $2M
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program,
        2_000_000_000_000,
    );
    let deposit_tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(deposit_tx)
        .await
        .unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Borrower borrows MAX capacity ($1M)
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program,
        max_borrow_capacity,
    );
    let borrow_tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(borrow_tx)
        .await
        .unwrap();

    // Verify at max capacity
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, max_borrow_capacity,
        "should be at max capacity"
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Pay interest (should NOT free up capacity)
    let interest_amount = 100_000_000_000u64; // $100K interest
    let repay_interest_ix = build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let repay_interest_tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(repay_interest_tx)
        .await
        .unwrap();

    // Verify capacity is still maxed out
    let wl_data_after = get_account_data(&mut ctx, &wl_pda).await;
    let wl_after = parse_borrower_whitelist(&wl_data_after);
    assert_eq!(
        wl_after.current_borrowed, max_borrow_capacity,
        "interest payment should NOT have freed up capacity"
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Attempt to borrow more should fail (at capacity)
    let extra_borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program,
        1, // Even $0.000001 should fail
    );
    let extra_borrow_tx = Transaction::new_signed_with_payer(
        &[extra_borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(extra_borrow_tx).await;

    assert!(
        result.is_err(),
        "borrowing more when at capacity should fail"
    );
    let err = result.unwrap_err();
    let code = extract_custom_error(&err);
    // GlobalCapacityExceeded = 27
    assert_eq!(
        code,
        Some(27),
        "expected GlobalCapacityExceeded error (27), got {:?}",
        code
    );
}
