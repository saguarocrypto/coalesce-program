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

use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

// ==========================================================================
// 1. test_deposit_u64_max_amount
//    Try to deposit u64::MAX USDC. Should fail (either InsufficientBalance
//    from SPL token, CapExceeded, or MathOverflow). Must not panic or
//    succeed unexpectedly.
// ==========================================================================

#[tokio::test]
async fn test_deposit_u64_max_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    // Mint a small amount -- the lender does NOT have u64::MAX tokens
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Try to deposit u64::MAX
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        u64::MAX,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not exist before failed deposit"
    );
    let lender_balance_before =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // The deposit processor checks CapExceeded BEFORE calling SPL Token transfer,
    // so this error comes from program logic, not SPL Token's InsufficientFunds.
    common::assert_custom_error(&result, 25); // CapExceeded

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on failed max deposit"
    );
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not be created on failed deposit"
    );
}

// ==========================================================================
// 2. test_borrow_u64_max_amount
//    Set up market with a small deposit (1000 USDC), then try to borrow
//    u64::MAX. Should fail with BorrowAmountTooHigh (17) or similar.
// ==========================================================================

#[tokio::test]
async fn test_borrow_u64_max_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC so the vault has some funds
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Create borrower token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Try to borrow u64::MAX
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        u64::MAX,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // The borrow processor checks BorrowAmountTooHigh BEFORE calling SPL Token transfer,
    // so this error comes from program logic, not SPL Token's InsufficientFunds.
    common::assert_custom_error(&result, 26); // BorrowAmountTooHigh

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected max borrow"
    );
    assert_eq!(
        borrower_whitelist_after, borrower_whitelist_before,
        "borrower whitelist state changed on rejected max borrow"
    );
}

// ==========================================================================
// 3. test_repay_u64_max_amount
//    Try to repay u64::MAX. Should fail due to insufficient token balance.
// ==========================================================================

#[tokio::test]
async fn test_repay_u64_max_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrow 500 USDC so there is an outstanding loan to repay
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow should succeed");

    // Now try to repay u64::MAX -- borrower only has 500 USDC worth of tokens
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        u64::MAX,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // SPL Token `InsufficientFunds` maps to token custom error 1.
    common::assert_custom_error(&result, 1);

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on failed max repay"
    );
    assert_eq!(
        borrower_whitelist_after, borrower_whitelist_before,
        "borrower whitelist state changed on failed max repay"
    );
}

// ==========================================================================
// 4. test_withdraw_u128_max_scaled_amount
//    Set up market, deposit, advance past maturity. Try to withdraw with
//    u128::MAX as scaled_amount. Should fail with InsufficientScaledBalance (20).
// ==========================================================================

#[tokio::test]
async fn test_withdraw_u128_max_scaled_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Advance clock past maturity AND past 300-second grace period
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Try to withdraw with u128::MAX as scaled_amount
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        u128::MAX,
        0,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Should fail with InsufficientScaledBalance (22)
    common::assert_custom_error(&result, 22);

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on failed max withdraw"
    );
}

// ==========================================================================
// 5. test_deposit_exact_cap_succeeds
//    Create a market with max_total_supply = 1,000,000 (1 USDC). Deposit
//    exactly 1,000,000. Should succeed. Then try depositing 1 more.
//    Should fail with CapExceeded (14).
// ==========================================================================

#[tokio::test]
async fn test_deposit_exact_cap_succeeds() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    // Market with max_total_supply = 1,000,000 (1 USDC)
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        1_000_000, // exactly 1 USDC
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        10 * USDC,
    )
    .await;

    // Deposit exactly 1,000,000 (1 USDC = the cap)
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000_000,
    );

    // Reuse last_blockhash — no bank fork needed for sequential deposits.
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit exactly at cap should succeed");

    // Verify market state
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(
        parsed.total_deposited, 1_000_000,
        "total_deposited should be exactly 1,000,000"
    );

    // Try depositing 1 more token -- should fail with CapExceeded (14)
    let deposit_ix_2 = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );

    // No new blockhash — amount (1) differs from prior (1,000,000), so
    // the transaction signature is unique within the same bank.
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_2],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_balance_before =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    common::assert_custom_error(&result, 25); // CapExceeded
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on failed over-cap deposit"
    );
}

// ==========================================================================
// 6. test_borrow_exact_vault_balance
//    Deposit 1000 USDC (no fees), then borrow exactly 1000 USDC (the full
//    vault). Should succeed.
// ==========================================================================

#[tokio::test]
async fn test_borrow_exact_vault_balance() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrow exactly 1000 USDC (the full vault balance)
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow exactly at vault balance should succeed");

    // Verify borrower received the full amount
    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(
        borrower_balance,
        1_000 * USDC,
        "borrower should have received exactly 1000 USDC"
    );

    // Verify vault is now empty
    let (vault, _) = common::get_vault_pda(&market);
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 0, "vault should be empty after full borrow");

    // Verify market state
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_borrowed, 1_000 * USDC);
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let whitelist_data = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let whitelist = common::parse_borrower_whitelist(&whitelist_data);
    assert_eq!(
        whitelist.current_borrowed,
        1_000 * USDC,
        "whitelist current_borrowed should track exact full-vault borrow"
    );

    // Tight boundary neighbor: +1 after full-vault borrow must fail exactly.
    let borrow_ix_over = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix_over],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 26); // BorrowAmountTooHigh

    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_balance_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_balance_after, borrower_balance_before,
        "borrower token balance changed on rejected +1 post-full-vault borrow"
    );
    assert_eq!(
        whitelist_after, whitelist_before,
        "borrower whitelist changed on rejected +1 post-full-vault borrow"
    );
}

// ==========================================================================
// 7. test_borrow_exceeds_global_capacity
//    Whitelist borrower with max_borrow_capacity of 500 USDC. Deposit
//    1000 USDC. Try to borrow 501 USDC. Should fail with
//    GlobalCapacityExceeded (30).
// ==========================================================================

#[tokio::test]
async fn test_borrow_exceeds_global_capacity() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    // Set max_borrow_capacity = 500 USDC for the borrower
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        500 * USDC, // max_borrow_capacity = 500 USDC
    )
    .await;

    // Deposit 1000 USDC so the vault has plenty of funds
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Create borrower token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Borrow exactly the capacity first (tight boundary success).
    let borrow_ix_at_cap = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix_at_cap],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow exactly at max_borrow_capacity should succeed");

    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(
        borrower_balance,
        500 * USDC,
        "borrower should receive exactly max_borrow_capacity on boundary borrow"
    );
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(
        parsed.total_borrowed,
        500 * USDC,
        "market total_borrowed should reflect exact boundary borrow"
    );
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let whitelist_data = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let whitelist = common::parse_borrower_whitelist(&whitelist_data);
    assert_eq!(
        whitelist.current_borrowed,
        500 * USDC,
        "whitelist current_borrowed should track boundary borrow"
    );

    // Then try to exceed capacity by 1 unit.
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Should fail with GlobalCapacityExceeded (27)
    common::assert_custom_error(&result, 27);

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected over-capacity borrow"
    );
    assert_eq!(
        whitelist_after, whitelist_before,
        "borrower whitelist changed on rejected over-capacity borrow"
    );
}

// ==========================================================================
// 8. test_deposit_zero_amount
//    Try to deposit 0. Should fail with ZeroAmount (11).
// ==========================================================================

#[tokio::test]
async fn test_deposit_zero_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Try to deposit 0
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        0,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not exist before zero-amount deposit"
    );
    let lender_balance_before =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Should fail with ZeroAmount (17)
    common::assert_custom_error(&result, 17);

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on rejected zero deposit"
    );
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not be created on rejected zero deposit"
    );
}

// ==========================================================================
// 9. test_borrow_zero_amount
//    Try to borrow 0. Should fail with ZeroAmount (11).
// ==========================================================================

#[tokio::test]
async fn test_borrow_zero_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit so vault has funds
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Create borrower token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Try to borrow 0
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Should fail with ZeroAmount (17)
    common::assert_custom_error(&result, 17);

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected zero borrow"
    );
    assert_eq!(
        whitelist_after, whitelist_before,
        "borrower whitelist changed on rejected zero borrow"
    );
}

// ==========================================================================
// 10. test_repay_zero_amount
//     Try to repay 0. Should fail with ZeroAmount (11).
// ==========================================================================

#[tokio::test]
async fn test_repay_zero_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit and borrow so there is something to repay
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrow 500 USDC
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow should succeed");

    // Try to repay 0
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        0,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Should fail with ZeroAmount (17)
    common::assert_custom_error(&result, 17);

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected zero repay"
    );
    assert_eq!(
        whitelist_after, whitelist_before,
        "borrower whitelist changed on rejected zero repay"
    );
}
// ==========================================================================
// 11. test_repay_exceeds_outstanding_debt
//     Borrow 500 USDC, give borrower enough tokens, then try to repay more
//     than owed. Exercises the program's RepaymentExceedsDebt logic (the
//     repay processor's SPL transfer CPI succeeds because borrower has
//     sufficient balance, but the debt check rejects it and the transaction
//     rolls back).
// ==========================================================================

#[tokio::test]
async fn test_repay_exceeds_outstanding_debt() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrow 500 USDC
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow should succeed");

    // Mint extra tokens so borrower has enough for the repay to pass SPL transfer.
    // Borrower already has 500 USDC from borrow; mint 600 more = 1100 total.
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token_kp.pubkey(),
        &admin,
        600 * USDC,
    )
    .await;

    // Try to repay 1000 USDC when only 500 USDC was borrowed.
    // The SPL transfer will succeed (borrower has 1100 USDC), but the program
    // detects RepaymentExceedsDebt and rolls back the entire transaction.
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        1_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert!(
        borrower_token_before >= 1_000 * USDC,
        "precondition: borrower must have enough balance for SPL transfer path"
    );
    let whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // RepaymentExceedsDebt = Custom(35) — exercises program logic, not SPL Token
    common::assert_custom_error(&result, 35);

    // Verify state immutability after failed tx (SPL transfer rolled back)
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected over-repay (rollback failure)"
    );
    assert_eq!(
        whitelist_after, whitelist_before,
        "borrower whitelist changed on rejected over-repay"
    );
}
