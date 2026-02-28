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

use solana_sdk::{
    instruction::AccountMeta, pubkey::Pubkey, signature::Keypair, signer::Signer,
    transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

// ===========================================================================
// 1. test_deposit_wrong_mint
//    Create two different USDC mints (both 6 decimals). Create market with
//    mint A. Try to deposit but pass mint B in the instruction's mint account
//    slot (index 7). Should fail with InvalidMint (7).
// ===========================================================================
#[tokio::test]
async fn test_deposit_wrong_mint() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
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

    // Create two different mints (both 6 decimals)
    let mint_a = common::create_mint(&mut ctx, &admin, 6).await;
    let mint_b = common::create_mint(&mut ctx, &admin, 6).await;

    // Create market with mint A
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint_a,
        &blacklist_program.pubkey(),
        1,
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create lender token account for mint A (the actual token transfer source)
    let lender_token_kp = common::create_token_account(&mut ctx, &mint_a, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint_a,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Build deposit instruction normally (with mint_a), then swap mint to mint_b at index 7
    let mut deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint_a,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    // Index 7 is the mint account (read-only)
    deposit_ix.accounts[7] = AccountMeta::new_readonly(mint_b, false);

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not exist before failed wrong-mint deposit"
    );
    let lender_balance_before =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 11); // InvalidMint

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on rejected wrong-mint deposit"
    );
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not be created on rejected wrong-mint deposit"
    );
}

// ===========================================================================
// 2. test_deposit_lender_position_from_wrong_market
//    Create two markets (nonce 1 and nonce 2) with the same borrower. Deposit
//    into market 1 to create lender position. Then build a deposit instruction
//    for market 2, but swap in the lender_position PDA from market 1 (index 4).
//    Should fail with InvalidPDA (24).
// ===========================================================================
#[tokio::test]
async fn test_deposit_lender_position_from_wrong_market() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
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

    // Create market 1 (nonce 1)
    let market_1 = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create market 2 (nonce 2) -- need a fresh blockhash
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let create_ix_2 = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2,
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[create_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (market_2, _) = common::get_market_pda(&borrower.pubkey(), 2);

    // Fund lender
    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    // Deposit into market 1 to create lender position for market 1
    let deposit_ix_1 = common::build_deposit(
        &market_1,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_1],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit into market 1 should succeed");

    // Now build a deposit instruction for market 2 but swap in lender_position
    // PDA from market 1 at index 4
    let mut deposit_ix_2 = common::build_deposit(
        &market_2,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let (lender_position_market_1, _) =
        common::get_lender_position_pda(&market_1, &lender.pubkey());
    // Index 4 is the lender_position PDA (writable)
    deposit_ix_2.accounts[4] = AccountMeta::new(lender_position_market_1, false);

    // Snapshot state before failed transaction
    let (vault_1, _) = common::get_vault_pda(&market_1);
    let (vault_2, _) = common::get_vault_pda(&market_2);
    let (lender_position_market_2, _) =
        common::get_lender_position_pda(&market_2, &lender.pubkey());
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position_market_2)
            .await
            .is_none(),
        "market 2 lender position should not exist before failed wrong-PDA deposit"
    );
    let lender_balance_before =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let market_1_before = common::get_account_data(&mut ctx, &market_1).await;
    let vault_1_before = common::get_token_balance(&mut ctx, &vault_1).await;
    let snap_before = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market_2,
        &vault_2,
        &[lender_position_market_1, lender_position_market_2],
    )
    .await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_2],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 13); // InvalidPDA

    // Verify state immutability after failed tx
    let snap_after = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market_2,
        &vault_2,
        &[lender_position_market_1, lender_position_market_2],
    )
    .await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let market_1_after = common::get_account_data(&mut ctx, &market_1).await;
    let vault_1_after = common::get_token_balance(&mut ctx, &vault_1).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on rejected wrong-market lender-position deposit"
    );
    assert_eq!(
        market_1_after, market_1_before,
        "market 1 state changed when market 2 deposit used wrong lender-position PDA"
    );
    assert_eq!(
        vault_1_after, vault_1_before,
        "market 1 vault changed when market 2 deposit used wrong lender-position PDA"
    );
    assert!(
        common::try_get_account_data(&mut ctx, &lender_position_market_2)
            .await
            .is_none(),
        "market 2 lender position should not be created on rejected wrong-PDA deposit"
    );
}

// ===========================================================================
// 3. test_borrow_wrong_borrower
//    Create market for borrower A. Build borrow instruction but supply
//    borrower B (who is also whitelisted). The market.borrower check should
//    fail with Unauthorized (3).
// ===========================================================================
#[tokio::test]
async fn test_borrow_wrong_borrower() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower_a = Keypair::new();
    let borrower_b = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower_a,
            &borrower_b,
            &whitelist_manager,
            &lender,
        ],
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

    // Create market with borrower A
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower_a,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Also whitelist borrower B
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower_b.pubkey(),
        1,
        10_000 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Deposit so vault has funds
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
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Create borrower B's token account
    let borrower_b_token_kp =
        common::create_token_account(&mut ctx, &mint, &borrower_b.pubkey()).await;

    // Build borrow instruction with borrower B instead of borrower A
    // borrower B is whitelisted, but the market.borrower field stores borrower A
    let borrow_ix = common::build_borrow(
        &market,
        &borrower_b.pubkey(),
        &borrower_b_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_a_whitelist, _) = common::get_borrower_whitelist_pda(&borrower_a.pubkey());
    let (borrower_b_whitelist, _) = common::get_borrower_whitelist_pda(&borrower_b.pubkey());
    let borrower_b_balance_before =
        common::get_token_balance(&mut ctx, &borrower_b_token_kp.pubkey()).await;
    let borrower_a_whitelist_before =
        common::get_account_data(&mut ctx, &borrower_a_whitelist).await;
    let borrower_b_whitelist_before =
        common::get_account_data(&mut ctx, &borrower_b_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower_b.pubkey()),
        &[&borrower_b],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 5); // Unauthorized

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_b_balance_after =
        common::get_token_balance(&mut ctx, &borrower_b_token_kp.pubkey()).await;
    let borrower_a_whitelist_after =
        common::get_account_data(&mut ctx, &borrower_a_whitelist).await;
    let borrower_b_whitelist_after =
        common::get_account_data(&mut ctx, &borrower_b_whitelist).await;
    assert_eq!(
        borrower_b_balance_after, borrower_b_balance_before,
        "borrower B token balance changed on rejected wrong-borrower borrow"
    );
    assert_eq!(
        borrower_a_whitelist_after, borrower_a_whitelist_before,
        "borrower A whitelist changed on rejected wrong-borrower borrow"
    );
    assert_eq!(
        borrower_b_whitelist_after, borrower_b_whitelist_before,
        "borrower B whitelist changed on rejected wrong-borrower borrow"
    );
}

// ===========================================================================
// 4. test_repay_wrong_mint
//    Create market with mint A. Build repay instruction but pass mint B in
//    mint slot (index 4). Should fail with InvalidMint (7).
// ===========================================================================
#[tokio::test]
async fn test_repay_wrong_mint() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
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

    // Create two mints
    let mint_a = common::create_mint(&mut ctx, &admin, 6).await;
    let mint_b = common::create_mint(&mut ctx, &admin, 6).await;

    // Create market with mint A
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint_a,
        &blacklist_program.pubkey(),
        1,
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit so vault has funds
    let lender_token_kp = common::create_token_account(&mut ctx, &mint_a, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint_a,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint_a,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrow some amount so that repay makes sense
    let borrower_token_kp =
        common::create_token_account(&mut ctx, &mint_a, &borrower.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow should succeed");

    // Build repay instruction with mint_a, then swap mint at index 5 to mint_b
    let mut repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint_a,
        &borrower.pubkey(),
        100 * USDC,
    );
    // Index 5 is the mint account (read-only) - after protocol_config at index 4
    repay_ix.accounts[5] = AccountMeta::new_readonly(mint_b, false);

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 11); // InvalidMint

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_balance_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_balance_after, borrower_balance_before,
        "borrower token balance changed on rejected wrong-mint repay"
    );
    assert_eq!(
        borrower_whitelist_after, borrower_whitelist_before,
        "borrower whitelist changed on rejected wrong-mint repay"
    );
}

// ===========================================================================
// 5. test_withdraw_wrong_lender_position
//    Create two markets. Lender deposits into both. After maturity, build
//    withdraw for market 1 but swap in lender_position from market 2
//    (index 4). Should fail with InvalidPDA (24).
// ===========================================================================
#[tokio::test]
async fn test_withdraw_wrong_lender_position() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
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

    // Get clock for maturity timestamp
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Create market 1 (nonce 1)
    let market_1 = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create market 2 (nonce 2)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let create_ix_2 = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2,
        500,
        maturity_timestamp,
        10_000 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[create_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (market_2, _) = common::get_market_pda(&borrower.pubkey(), 2);

    // Fund lender
    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    // Deposit into market 1
    let deposit_ix_1 = common::build_deposit(
        &market_1,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_1],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit into market 1 should succeed");

    // Deposit into market 2
    let deposit_ix_2 = common::build_deposit(
        &market_2,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_2],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit into market 2 should succeed");

    // Advance clock past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Build withdraw for market 1 but swap in lender_position from market 2
    let mut withdraw_ix = common::build_withdraw(
        &market_1,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full withdrawal
    );
    let (lender_position_market_2, _) =
        common::get_lender_position_pda(&market_2, &lender.pubkey());
    // Index 4 is the lender_position PDA (writable)
    withdraw_ix.accounts[4] = AccountMeta::new(lender_position_market_2, false);

    // Snapshot state before failed transaction
    let (vault_1, _) = common::get_vault_pda(&market_1);
    let (vault_2, _) = common::get_vault_pda(&market_2);
    let (lender_position_market_1, _) =
        common::get_lender_position_pda(&market_1, &lender.pubkey());
    let lender_balance_before =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let market_2_before = common::get_account_data(&mut ctx, &market_2).await;
    let vault_2_before = common::get_token_balance(&mut ctx, &vault_2).await;
    let snap_before = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market_1,
        &vault_1,
        &[lender_position_market_1, lender_position_market_2],
    )
    .await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&lender.pubkey()),
        &[&lender],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 13); // InvalidPDA

    // Verify state immutability after failed tx
    let snap_after = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market_1,
        &vault_1,
        &[lender_position_market_1, lender_position_market_2],
    )
    .await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let market_2_after = common::get_account_data(&mut ctx, &market_2).await;
    let vault_2_after = common::get_token_balance(&mut ctx, &vault_2).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on rejected wrong-lender-position withdraw"
    );
    assert_eq!(
        market_2_after, market_2_before,
        "market 2 state changed when market 1 withdraw used wrong lender-position PDA"
    );
    assert_eq!(
        vault_2_after, vault_2_before,
        "market 2 vault changed when market 1 withdraw used wrong lender-position PDA"
    );
}

// ===========================================================================
// 6. test_collect_fees_wrong_market_authority
//    Set up market with accrued fees. Build collect_fees instruction but
//    replace market_authority (index 5) with a random Pubkey. Should fail
//    with InvalidPDA (24).
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_wrong_market_authority() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &fee_authority,
            &borrower,
            &whitelist_manager,
            &lender,
        ],
        10_000_000_000,
    )
    .await;

    // Initialize protocol with non-zero fee rate so fees accrue
    let fee_rate_bps: u16 = 1000; // 10%
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    // Short maturity so minimal interest accrues — this test is about wrong
    // market authority, not interest/settlement mechanics.
    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual interest
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit
    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &mint_authority,
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
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow to trigger interest accrual
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Repay principal (to put tokens back in vault for fee collection)
    // Note: Can only repay up to borrowed amount (500 USDC) due to SR-116 validation
    let repay_amount = 500 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token_kp.pubkey(),
        &mint_authority,
        repay_amount,
    )
    .await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity + grace period (300s) so we can withdraw
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // SR-113: Lender must withdraw before fee collection
    // First, repay a bit of interest to ensure vault has enough for full settlement
    let interest_amount = 10 * USDC; // Extra buffer for settlement
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token_kp.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        interest_amount,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender withdraws full balance
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0, // 0 = full withdrawal
        0, // no minimum payout
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create fee destination token account
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    // Build collect_fees instruction, then replace market_authority (index 5)
    // with a random Pubkey
    let random_key = Pubkey::new_unique();
    let mut collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    // Index 5 is market_authority (read-only)
    collect_ix.accounts[5] = AccountMeta::new_readonly(random_key, false);

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 13); // InvalidPDA

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let borrower_whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        fee_dest_after, fee_dest_before,
        "fee destination balance changed on rejected wrong-market-authority collect_fees"
    );
    assert_eq!(
        borrower_whitelist_after, borrower_whitelist_before,
        "borrower whitelist changed on rejected wrong-market-authority collect_fees"
    );
}

// ===========================================================================
// 7. test_re_settle_wrong_vault
//    Set up a settled market. Build re_settle instruction but replace vault
//    (index 1) with a random Pubkey. Should fail with InvalidVault (9).
// ===========================================================================
#[tokio::test]
async fn test_re_settle_wrong_vault() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
        ],
        10_000_000_000,
    )
    .await;

    // Use fee_rate=0 to simplify
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0,
    )
    .await;

    let mint_authority = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &mint_authority,
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
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow 800 USDC (underfund the vault)
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        800 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Partial repay 200 USDC
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token_kp.pubkey(),
        &mint_authority,
        200 * USDC,
    )
    .await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        200 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity AND past 300-second grace period
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw to set settlement factor (underfunded market)
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify settlement factor was set
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert!(
        parsed.settlement_factor_wad > 0,
        "settlement_factor should be set after withdrawal"
    );

    // Repay more so re_settle would normally succeed
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token_kp.pubkey(),
        &mint_authority,
        300 * USDC,
    )
    .await;
    let repay_ix2 = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        300 * USDC,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Build re_settle instruction with the correct vault, then swap it out
    let (vault, _) = common::get_vault_pda(&market);
    let mut re_settle_ix = common::build_re_settle(&market, &vault);

    // Replace vault (index 1) with a random Pubkey
    let random_vault = Pubkey::new_unique();
    re_settle_ix.accounts[1] = AccountMeta::new_readonly(random_vault, false);

    // Snapshot state before failed transaction
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let market_before = common::get_account_data(&mut ctx, &market).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[re_settle_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 12); // InvalidVault

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_balance_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrower_whitelist_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let market_after = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(
        borrower_balance_after, borrower_balance_before,
        "borrower token balance changed on rejected wrong-vault re_settle"
    );
    assert_eq!(
        borrower_whitelist_after, borrower_whitelist_before,
        "borrower whitelist changed on rejected wrong-vault re_settle"
    );
    assert_eq!(
        market_after, market_before,
        "market bytes changed on rejected wrong-vault re_settle"
    );
}
