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
// 1. test_deposit_rejects_wrong_vault
//    Set up a market. Build deposit instruction normally, then replace the
//    vault account (index 3) with a different Pubkey. Should fail with
//    InvalidVault (9).
// ===========================================================================
#[tokio::test]
async fn test_deposit_rejects_wrong_vault() {
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

    let market = common::setup_market_full(
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

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Build deposit instruction normally
    let mut deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    let (vault, _) = common::get_vault_pda(&market);
    assert_eq!(
        deposit_ix.accounts[3].pubkey, vault,
        "deposit vault account index must point to the canonical vault PDA"
    );
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::try_get_account_data(&mut ctx, &lender_position).await;
    assert!(
        lender_position_before.is_none(),
        "deposit failure-path precondition: lender position should not exist before first deposit"
    );
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // Replace vault (index 3) with a wrong Pubkey, preserving writable/signer flags
    let wrong_vault = Pubkey::new_unique();
    deposit_ix.accounts[3] = AccountMeta::new(wrong_vault, false);

    // Snapshot state before failed transaction
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
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
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on rejected wrong-vault deposit"
    );
    assert_eq!(
        common::try_get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position lifecycle changed on rejected wrong-vault deposit"
    );
}

// ===========================================================================
// 2. test_deposit_rejects_wrong_mint
//    Build deposit instruction, replace the mint (index 7) with a different
//    Pubkey. Should fail with InvalidMint (7).
// ===========================================================================
#[tokio::test]
async fn test_deposit_rejects_wrong_mint() {
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

    let market = common::setup_market_full(
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

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Build deposit instruction normally
    let mut deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    assert_eq!(
        deposit_ix.accounts[7].pubkey, mint,
        "deposit mint account index must point to the market mint"
    );
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::try_get_account_data(&mut ctx, &lender_position).await;
    assert!(
        lender_position_before.is_none(),
        "deposit failure-path precondition: lender position should not exist before first deposit"
    );
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // Replace mint (index 7) with a wrong Pubkey, preserving read-only/non-signer flags
    let wrong_mint = Pubkey::new_unique();
    deposit_ix.accounts[7] = AccountMeta::new_readonly(wrong_mint, false);

    // Snapshot state before failed transaction
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
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
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on rejected wrong-mint deposit"
    );
    assert_eq!(
        common::try_get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position lifecycle changed on rejected wrong-mint deposit"
    );
}

// ===========================================================================
// 3. test_borrow_rejects_wrong_vault
//    Set up market with deposit. Build borrow instruction, replace vault
//    (index 3) with wrong Pubkey. Should fail with InvalidVault (9).
// ===========================================================================
#[tokio::test]
async fn test_borrow_rejects_wrong_vault() {
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

    let market = common::setup_market_full(
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
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Build borrow instruction normally
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let mut borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    let (vault, _) = common::get_vault_pda(&market);
    assert_eq!(
        borrow_ix.accounts[3].pubkey, vault,
        "borrow vault account index must point to the canonical vault PDA"
    );
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;

    // Replace vault (index 3) with a wrong Pubkey, preserving writable/non-signer flags
    let wrong_vault = Pubkey::new_unique();
    borrow_ix.accounts[3] = AccountMeta::new(wrong_vault, false);

    // Snapshot state before failed transaction
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
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
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected wrong-vault borrow"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &borrower_whitelist).await,
        borrower_whitelist_before,
        "borrower whitelist bytes changed on rejected wrong-vault borrow"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position bytes changed on rejected wrong-vault borrow"
    );
}

// ===========================================================================
// 4. test_withdraw_rejects_wrong_vault
//    Set up market, deposit, advance past maturity. Build withdraw
//    instruction, replace vault (index 3) with wrong Pubkey. Should fail
//    with InvalidVault (9).
// ===========================================================================
#[tokio::test]
async fn test_withdraw_rejects_wrong_vault() {
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

    // Deposit
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
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Advance clock past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Build withdraw instruction normally
    let mut withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full withdrawal
    );

    let (vault, _) = common::get_vault_pda(&market);
    assert_eq!(
        withdraw_ix.accounts[3].pubkey, vault,
        "withdraw vault account index must point to the canonical vault PDA"
    );
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // Replace vault (index 3) with a wrong Pubkey, preserving writable/non-signer flags
    let wrong_vault = Pubkey::new_unique();
    withdraw_ix.accounts[3] = AccountMeta::new(wrong_vault, false);

    // Snapshot state before failed transaction
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
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
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on rejected wrong-vault withdraw"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position bytes changed on rejected wrong-vault withdraw"
    );
}

// ===========================================================================
// 5. test_repay_rejects_wrong_vault
//    Set up market, deposit, borrow. Build repay instruction, replace vault
//    (index 3) with wrong Pubkey. Should fail with InvalidVault (9).
// ===========================================================================
#[tokio::test]
async fn test_repay_rejects_wrong_vault() {
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

    let market = common::setup_market_full(
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
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrow
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

    // Build repay instruction normally
    let mut repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        100 * USDC,
    );

    let (vault, _) = common::get_vault_pda(&market);
    assert_eq!(
        repay_ix.accounts[3].pubkey, vault,
        "repay vault account index must point to the canonical vault PDA"
    );
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;

    // Replace vault (index 3) with a wrong Pubkey, preserving writable/non-signer flags
    let wrong_vault = Pubkey::new_unique();
    repay_ix.accounts[3] = AccountMeta::new(wrong_vault, false);

    // Snapshot state before failed transaction
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
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
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected wrong-vault repay"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &borrower_whitelist).await,
        borrower_whitelist_before,
        "borrower whitelist bytes changed on rejected wrong-vault repay"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position bytes changed on rejected wrong-vault repay"
    );
}

// ===========================================================================
// 6. test_repay_rejects_wrong_mint
//    Build repay instruction, replace mint (index 5) with wrong Pubkey.
//    Should fail with InvalidMint (7).
// ===========================================================================
#[tokio::test]
async fn test_repay_rejects_wrong_mint() {
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

    let market = common::setup_market_full(
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
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrow
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

    // Build repay instruction normally
    let mut repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        100 * USDC,
    );

    assert_eq!(
        repay_ix.accounts[5].pubkey, mint,
        "repay mint account index must point to the market mint"
    );
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let borrower_token_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;

    // Replace mint (index 5) with a wrong Pubkey, preserving read-only/non-signer flags
    // Note: index changed from 4 to 5 after protocol_config was added at index 4
    let wrong_mint = Pubkey::new_unique();
    repay_ix.accounts[5] = AccountMeta::new_readonly(wrong_mint, false);

    // Snapshot state before failed transaction
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
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
    let borrower_token_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected wrong-mint repay"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &borrower_whitelist).await,
        borrower_whitelist_before,
        "borrower whitelist bytes changed on rejected wrong-mint repay"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position bytes changed on rejected wrong-mint repay"
    );
}

// ===========================================================================
// 7. test_close_lender_position_rejects_wrong_pda
//    Create a lender position, withdraw fully. Build close instruction,
//    replace lender_position (index 2) with wrong Pubkey. Should fail with
//    InvalidPDA (24).
// ===========================================================================
#[tokio::test]
async fn test_close_lender_position_rejects_wrong_pda() {
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

    // Deposit to create a lender position
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
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Advance clock past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw all (scaled_amount=0 means full withdrawal)
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("withdraw should succeed");

    // Build close_lender_position instruction normally
    let mut close_ix = common::build_close_lender_position(&market, &lender.pubkey());

    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    assert_eq!(
        close_ix.accounts[2].pubkey, lender_position,
        "close_lender_position account index must point to canonical lender-position PDA"
    );
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // Replace lender_position (index 2) with a wrong Pubkey, preserving writable/non-signer flags
    let wrong_pda = Pubkey::new_unique();
    close_ix.accounts[2] = AccountMeta::new(wrong_pda, false);

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
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
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on rejected wrong-PDA close"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position bytes changed on rejected wrong-PDA close"
    );
}

// ===========================================================================
// 8. test_collect_fees_rejects_wrong_vault
//    Set up market with fees. Build collect_fees instruction, replace vault
//    (index 4) with wrong Pubkey. Should fail with InvalidVault (9).
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_rejects_wrong_vault() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
    ] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Initialize protocol with fee_authority as the real fee authority
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

    // Create mint
    let mint_authority = Keypair::new();
    {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &mint_authority.pubkey(),
                1_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    // Create token accounts
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    // Mint tokens to lender
    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    // Setup market — short maturity so minimal interest accrues.
    // This test is about wrong vault PDA, not interest/settlement mechanics.
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
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow
    let borrow_amount = 500 * USDC;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Repay principal (can only repay up to borrowed amount due to SR-116)
    let repay_amount = 500 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        repay_amount,
    )
    .await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity + grace period
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Fund vault with interest for settlement
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        100 * USDC,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        100 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // SR-113: Lender must withdraw before fee collection
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0, // 0 = full withdrawal
        0, // no minimum payout
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Build collect_fees instruction normally
    let mut collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());

    let (vault, _) = common::get_vault_pda(&market);
    assert_eq!(
        collect_ix.accounts[4].pubkey, vault,
        "collect_fees vault account index must point to canonical vault PDA"
    );
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::try_get_account_data(&mut ctx, &lender_position).await;
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;

    // Replace vault (index 4) with a wrong Pubkey, preserving writable/non-signer flags
    let wrong_vault = Pubkey::new_unique();
    collect_ix.accounts[4] = AccountMeta::new(wrong_vault, false);

    // Snapshot state before failed transaction
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
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
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(
        fee_dest_after, fee_dest_before,
        "fee destination balance changed on rejected wrong-vault collect_fees"
    );
    assert_eq!(
        common::try_get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender position lifecycle changed on rejected wrong-vault collect_fees"
    );
}
