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

use solana_program_test::{BanksClientError, ProgramTestContext};
use solana_sdk::{
    instruction::InstructionError,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

fn assert_custom_error(result: Result<(), BanksClientError>, expected_code: u32) {
    match result {
        Err(BanksClientError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(code),
        ))) => {
            assert_eq!(
                code, expected_code,
                "expected Custom({expected_code}), got Custom({code})"
            );
        },
        Err(other) => panic!("expected Custom({expected_code}), got {other:?}"),
        Ok(()) => panic!("expected Custom({expected_code}), but transaction succeeded"),
    }
}

// ---------------------------------------------------------------------------
// Test 1: Non-existent blacklist account (PDA has 0 lamports) => deposit OK
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blacklist_nonexistent_account_ok() {
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
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create lender and fund with tokens
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

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // No blacklist account injected: PDA has 0 lamports and should pass in fail-open mode.
    // Neighbor amounts `x` and `x+1` must both succeed.
    for amount in [500 * USDC, 1] {
        let deposit_ix = common::build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token_kp.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
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
            .expect("deposit should succeed when blacklist account does not exist");
    }

    let expected_total = 500 * USDC + 1;
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, expected_total);

    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_after, lender_before - expected_total);

    let market_data = common::get_account_data(&mut ctx, &market).await;
    let market_state = common::parse_market(&market_data);
    assert_eq!(market_state.total_deposited, expected_total);
    assert!(market_state.scaled_total_supply > 0);

    let pos_data = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    let pos = common::parse_lender_position(&pos_data);
    assert_eq!(pos.scaled_balance, market_state.scaled_total_supply);
}

// ---------------------------------------------------------------------------
// Test 2: Blacklist status byte = 1 => deposit blocked with Custom(7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blacklist_status_one_blocked() {
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
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create lender and fund with tokens
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

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // x = 1 status byte blocks.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 1);
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 7);
    assert!(
        ctx.banks_client
            .get_account(lender_pos_pda)
            .await
            .unwrap()
            .is_none(),
        "lender position must not be created on blacklisted deposit",
    );
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_after, lender_before);

    // x-1 = 0 status byte allows the same path for an otherwise equivalent lender.
    let lender_ok = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender_ok], 5_000_000_000).await;
    let lender_ok_token_kp =
        common::create_token_account(&mut ctx, &mint, &lender_ok.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_ok_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;
    common::setup_blacklist_account(
        &mut ctx,
        &blacklist_program.pubkey(),
        &lender_ok.pubkey(),
        0,
    );
    let deposit_ix = common::build_deposit(
        &market,
        &lender_ok.pubkey(),
        &lender_ok_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_ok],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed for status=0 lender");

    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 500 * USDC);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let market_state = common::parse_market(&market_data);
    assert_eq!(market_state.total_deposited, 500 * USDC);
}

// ---------------------------------------------------------------------------
// Test 3: Blacklist status byte = 0 => deposit succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blacklist_status_zero_ok() {
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
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create lender and fund with tokens
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

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // x-1 = 0 status byte should allow deposit.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
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
        .expect("deposit should succeed when blacklist status byte is 0");

    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 500 * USDC);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let market_state = common::parse_market(&market_data);
    assert_eq!(market_state.total_deposited, 500 * USDC);

    // x+1 = 2 is invalid blacklist status and should hard-fail (InvalidAccountOwner=14).
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 2);
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 14);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_after, lender_before);

    // status=1 behavior is asserted in dedicated blacklist-blocking tests.
}

// ---------------------------------------------------------------------------
// Test 4: Blacklisted lender cannot deposit => Custom(7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blacklist_blocks_deposit() {
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
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create lender and fund with tokens
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

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // Boundary setup: x-1 deposit succeeds with status=0.
    let base_amount = 500 * USDC - 1;
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        base_amount,
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
        .expect("x-1 deposit should succeed when not blacklisted");
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        base_amount
    );

    // x deposit is blocked once status flips to 1.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 1);
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 7);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_after, lender_before);

    // x+1 boundary: an equivalent status=0 lender should succeed with the same deposit amount.
    let lender_ok = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender_ok], 5_000_000_000).await;
    let lender_ok_token_kp =
        common::create_token_account(&mut ctx, &mint, &lender_ok.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_ok_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;
    common::setup_blacklist_account(
        &mut ctx,
        &blacklist_program.pubkey(),
        &lender_ok.pubkey(),
        0,
    );
    let deposit_ix = common::build_deposit(
        &market,
        &lender_ok.pubkey(),
        &lender_ok_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_ok],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed for non-blacklisted lender");

    let market_data = common::get_account_data(&mut ctx, &market).await;
    let market_state = common::parse_market(&market_data);
    assert_eq!(market_state.total_deposited, 500 * USDC);
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        500 * USDC
    );
}

// ---------------------------------------------------------------------------
// Test 5: Blacklisted borrower cannot borrow => Custom(7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blacklist_blocks_borrow() {
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
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Have a clean lender deposit first so the vault has funds to borrow
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
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("lender deposit should succeed");

    // Now inject blacklist for the borrower
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 1);

    // Create borrower token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let borrow_amount = 500 * USDC;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (whitelist_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let whitelist_before = common::get_account_data(&mut ctx, &whitelist_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
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
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 7);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let whitelist_after = common::get_account_data(&mut ctx, &whitelist_pda).await;
    assert_eq!(whitelist_before, whitelist_after);
    let borrower_after_fail =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_after_fail, borrower_before);

    // Boundary-neighbor control: status=0 borrower in an equivalent market can borrow.
    let borrower_ok = Keypair::new();
    let lender_ok = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&borrower_ok, &lender_ok], 5_000_000_000).await;

    let maturity_ok = common::FAR_FUTURE_MATURITY;
    let market_ok = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower_ok,
        &mint,
        &blacklist_program.pubkey(),
        2,
        500,
        maturity_ok,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_ok_token_kp =
        common::create_token_account(&mut ctx, &mint, &lender_ok.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_ok_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;
    let deposit_ix = common::build_deposit(
        &market_ok,
        &lender_ok.pubkey(),
        &lender_ok_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_ok],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("control lender deposit should succeed");

    let borrower_ok_token_kp =
        common::create_token_account(&mut ctx, &mint, &borrower_ok.pubkey()).await;
    common::setup_blacklist_account(
        &mut ctx,
        &blacklist_program.pubkey(),
        &borrower_ok.pubkey(),
        0,
    );
    let borrow_ix = common::build_borrow(
        &market_ok,
        &borrower_ok.pubkey(),
        &borrower_ok_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower_ok],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow should succeed for non-blacklisted borrower");

    let borrower_ok_after =
        common::get_token_balance(&mut ctx, &borrower_ok_token_kp.pubkey()).await;
    assert_eq!(borrower_ok_after, borrow_amount);
    let (vault_ok, _) = common::get_vault_pda(&market_ok);
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault_ok).await,
        1_000 * USDC - borrow_amount
    );
    let market_ok_data = common::get_account_data(&mut ctx, &market_ok).await;
    let market_ok_state = common::parse_market(&market_ok_data);
    assert_eq!(market_ok_state.total_borrowed, borrow_amount);
    let (whitelist_ok_pda, _) = common::get_borrower_whitelist_pda(&borrower_ok.pubkey());
    let whitelist_ok_data = common::get_account_data(&mut ctx, &whitelist_ok_pda).await;
    let whitelist_ok_state = common::parse_borrower_whitelist(&whitelist_ok_data);
    assert_eq!(whitelist_ok_state.current_borrowed, borrow_amount);
}

// ---------------------------------------------------------------------------
// Test 6: Blacklisted lender cannot withdraw => Custom(7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blacklist_blocks_withdraw() {
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
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create two lenders and deposit before maturity.
    let lender = Keypair::new();
    let lender_ok = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender, &lender_ok], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let lender_ok_token_kp =
        common::create_token_account(&mut ctx, &mint, &lender_ok.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_ok_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    for (who, token, amount) in [
        (&lender, &lender_token_kp, 500 * USDC),
        (&lender_ok, &lender_ok_token_kp, 300 * USDC),
    ] {
        let deposit_ix = common::build_deposit(
            &market,
            &who.pubkey(),
            &token.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
        );
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, who],
            recent,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("deposit should succeed before blacklisting");
    }

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (lender_ok_pos_pda, _) = common::get_lender_position_pda(&market, &lender_ok.pubkey());

    // Blacklist only lender #1 and advance to maturity.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 1);
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    let snapshot_before = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market,
        &vault,
        &[lender_pos_pda, lender_ok_pos_pda],
    )
    .await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

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
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 7);
    let snapshot_after = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market,
        &vault,
        &[lender_pos_pda, lender_ok_pos_pda],
    )
    .await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after_fail = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_after_fail, lender_before);

    // Boundary neighbor: non-blacklisted lender #2 can withdraw at the same time.
    let lender_ok_before = common::get_token_balance(&mut ctx, &lender_ok_token_kp.pubkey()).await;
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender_ok.pubkey(),
        &lender_ok_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_ok],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("withdraw should succeed for non-blacklisted lender");

    let lender_ok_after = common::get_token_balance(&mut ctx, &lender_ok_token_kp.pubkey()).await;
    assert!(
        lender_ok_after > lender_ok_before,
        "successful withdraw should transfer funds to non-blacklisted lender"
    );
    assert!(
        common::get_token_balance(&mut ctx, &vault).await < 800 * USDC,
        "vault should decrease after successful withdraw",
    );

    let lender_ok_pos_data = common::get_account_data(&mut ctx, &lender_ok_pos_pda).await;
    let lender_ok_pos = common::parse_lender_position(&lender_ok_pos_data);
    assert_eq!(lender_ok_pos.scaled_balance, 0);
}

// ---------------------------------------------------------------------------
// Test 7: Blacklisted borrower cannot create market => Custom(7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blacklist_blocks_create_market() {
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

    // Whitelist the borrower manually (since we cannot use setup_market_full
    // which would also call create_market)
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("whitelist should succeed");

    let (whitelist_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let whitelist_before = common::get_account_data(&mut ctx, &whitelist_pda).await;
    let (market_pda, _) = common::get_market_pda(&borrower.pubkey(), 1);
    let maturity = common::FAR_FUTURE_MATURITY;

    // x = 1 status byte blocks create_market.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 1);
    let create_ix = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 7);
    assert!(
        ctx.banks_client
            .get_account(market_pda)
            .await
            .unwrap()
            .is_none(),
        "market PDA must not be created when borrower is blacklisted",
    );
    let whitelist_after = common::get_account_data(&mut ctx, &whitelist_pda).await;
    assert_eq!(whitelist_before, whitelist_after);

    // x-1 = 0 status byte allows create_market for an equivalent borrower.
    let borrower_ok = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&borrower_ok], 5_000_000_000).await;
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower_ok.pubkey(),
        1,
        50_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("whitelist for non-blacklisted borrower should succeed");

    common::setup_blacklist_account(
        &mut ctx,
        &blacklist_program.pubkey(),
        &borrower_ok.pubkey(),
        0,
    );
    let (market_ok_pda, _) = common::get_market_pda(&borrower_ok.pubkey(), 1);
    let create_ix = common::build_create_market(
        &borrower_ok.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower_ok],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("create_market should succeed for status=0 borrower");

    let market_data = common::get_account_data(&mut ctx, &market_ok_pda).await;
    let market_state = common::parse_market(&market_data);
    assert_eq!(market_state.borrower, borrower_ok.pubkey().to_bytes());
    assert_eq!(market_state.total_deposited, 0);
    assert_eq!(market_state.total_borrowed, 0);

    let (vault, _) = common::get_vault_pda(&market_ok_pda);
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 0);
}
