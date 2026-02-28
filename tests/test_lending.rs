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

use solana_program_test::*;
use solana_sdk::{
    instruction::InstructionError,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// Helper: parse Market from raw account data
// All offsets add 9 to skip discriminator (8 bytes) + version (1 byte)
// ---------------------------------------------------------------------------

fn parse_total_deposited(data: &[u8]) -> u64 {
    u64::from_le_bytes(data[172..180].try_into().expect("slice"))
}

fn parse_total_borrowed(data: &[u8]) -> u64 {
    u64::from_le_bytes(data[180..188].try_into().expect("slice"))
}

fn parse_total_repaid(data: &[u8]) -> u64 {
    u64::from_le_bytes(data[188..196].try_into().expect("slice"))
}

fn parse_scaled_total_supply(data: &[u8]) -> u128 {
    u128::from_le_bytes(data[132..148].try_into().expect("slice"))
}

fn parse_lender_scaled_balance(data: &[u8]) -> u128 {
    u128::from_le_bytes(data[73..89].try_into().expect("slice"))
}

fn parse_whitelist_current_borrowed(data: &[u8]) -> u64 {
    u64::from_le_bytes(data[50..58].try_into().expect("slice"))
}

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
// Test 1: Deposit succeeds and updates all state correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 100; // 1%

    // Airdrop SOL to admin, borrower, whitelist_manager
    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000, // 5 SOL
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

    // Initialize protocol
    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(), // fee_authority
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    // Create mint (6 decimals)
    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    // Create market
    let nonce: u64 = 1;
    let annual_interest_bps: u16 = 500; // 5%
    let maturity_timestamp: i64 = common::FAR_FUTURE_MATURITY; // far future
    let max_total_supply: u64 = 10_000 * USDC; // 10,000 USDC

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        nonce,
        annual_interest_bps,
        maturity_timestamp,
        max_total_supply,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create lender and fund with 1000 USDC
    let lender = Keypair::new();
    let fund_lender_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000, // 2 SOL
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(fund_lender_tx)
        .await
        .expect("fund lender");

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Get vault PDA
    let (vault, _) = common::get_vault_pda(&market);

    // Verify initial balances
    let lender_balance_before =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_balance_before, 1_000 * USDC);

    let vault_balance_before = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance_before, 0);

    // Build and send deposit instruction (500 USDC)
    let deposit_amount = 500 * USDC;
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
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

    // Verify vault balance increased by 500 USDC
    let vault_balance_after = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance_after, deposit_amount);

    // Verify lender balance decreased by 500 USDC
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_balance_after, 1_000 * USDC - deposit_amount);

    // Verify market state
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let total_deposited = parse_total_deposited(&market_data);
    assert_eq!(total_deposited, deposit_amount);

    let scaled_total_supply = parse_scaled_total_supply(&market_data);
    assert!(scaled_total_supply > 0, "scaled_total_supply should be > 0");

    // Verify LenderPosition exists with scaled_balance > 0
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_pos_data = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    let scaled_balance = parse_lender_scaled_balance(&lender_pos_data);
    assert!(scaled_balance > 0, "lender scaled_balance should be > 0");
}

// ---------------------------------------------------------------------------
// Test 2: Deposit with zero amount fails with Custom(11)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_zero_amount() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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
    let market_before = common::get_account_data(&mut ctx, &market).await;
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // x boundary: zero deposit must fail with exact code and no side effects.
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 17);
    assert!(
        ctx.banks_client
            .get_account(lender_pos_pda)
            .await
            .unwrap()
            .is_none(),
        "Lender position must not be created on zero-amount deposit"
    );
    let market_after_fail = common::get_account_data(&mut ctx, &market).await;
    let vault_after_fail = common::get_token_balance(&mut ctx, &vault).await;
    let lender_after_fail = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(
        market_before, market_after_fail,
        "Market must stay unchanged on failure"
    );
    assert_eq!(
        vault_before, vault_after_fail,
        "Vault balance must stay unchanged on failure"
    );
    assert_eq!(
        lender_before, lender_after_fail,
        "Lender token balance must stay unchanged on failure"
    );

    // x+1 boundary neighbor: deposit of 1 should succeed.
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
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit(1) should succeed");

    let vault_after_success = common::get_token_balance(&mut ctx, &vault).await;
    let lender_after_success = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(vault_after_success, vault_before + 1);
    assert_eq!(lender_after_success, lender_before - 1);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(parse_total_deposited(&market_data), 1);
    let lender_pos_data = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    assert!(
        parse_lender_scaled_balance(&lender_pos_data) > 0,
        "Lender position must exist after successful deposit"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Deposit exceeding cap fails with Custom(14)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_cap_exceeded() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Market with max_total_supply = 100 USDC.
    let max_supply = 100 * USDC;
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_timestamp,
        max_supply,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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
    let market_before = common::get_account_data(&mut ctx, &market).await;
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // x+1 boundary: cap+1 fails exactly with CapExceeded.
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        max_supply + 1,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 25);
    assert!(
        ctx.banks_client
            .get_account(lender_pos_pda)
            .await
            .unwrap()
            .is_none(),
        "Lender position must not be created on cap-exceeded deposit"
    );
    let market_after_fail = common::get_account_data(&mut ctx, &market).await;
    let vault_after_fail = common::get_token_balance(&mut ctx, &vault).await;
    let lender_after_fail = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(market_before, market_after_fail);
    assert_eq!(vault_before, vault_after_fail);
    assert_eq!(lender_before, lender_after_fail);

    // x boundary: deposit exactly to cap succeeds.
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        max_supply,
    );
    // Reuse last_blockhash — amount (max_supply) differs from prior
    // (max_supply + 1), so the transaction signature is unique.
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit at cap should succeed");
    let market_data = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(parse_total_deposited(&market_data), max_supply);

    // Additional +1 once at cap still fails and is atomic.
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
    // No new blockhash — amount (1) differs from prior (max_supply), so
    // the transaction signature is unique within the same bank.
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 25);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_before, lender_after);
}

// ---------------------------------------------------------------------------
// Test 4: Deposit after maturity fails with Custom(28)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_after_maturity() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Set maturity to PINNED_EPOCH + 61 (just above MIN_MATURITY_DELTA of 60)
    let maturity_timestamp = common::PINNED_EPOCH + 61;

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

    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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

    // x-1 boundary: one second before maturity should still allow deposit.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp - 1).await;
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
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
        .expect("deposit before maturity should succeed");
    let market_after_pre = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(parse_total_deposited(&market_after_pre), 100 * USDC);

    // x boundary: exactly at maturity should fail with MarketMatured.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp).await;
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
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 28);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_before, lender_after);

    // x+1 boundary: after maturity must continue to fail atomically.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 1).await;
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
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 28);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_before, lender_after);
}

// ---------------------------------------------------------------------------
// Test 5: Borrow succeeds and updates all state correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Deposit 1000 USDC from a lender first
    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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

    // Create borrower's token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Get vault balance before borrow
    let (vault, _) = common::get_vault_pda(&market);
    let vault_balance_before = common::get_token_balance(&mut ctx, &vault).await;
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance_before, 0);

    // Borrow 500 USDC
    let borrow_amount = 500 * USDC;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );

    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow should succeed");

    // Verify borrower token account increased
    let borrower_balance_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance_after, borrow_amount);

    // Verify vault decreased
    let vault_balance_after = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance_after, vault_balance_before - borrow_amount);

    // Verify market.total_borrowed
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let total_borrowed = parse_total_borrowed(&market_data);
    assert_eq!(total_borrowed, borrow_amount);

    // Verify whitelist.current_borrowed updated
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl_current_borrowed = parse_whitelist_current_borrowed(&wl_data);
    assert_eq!(wl_current_borrowed, borrow_amount);
}

// ---------------------------------------------------------------------------
// Test 6: Borrow exceeding available vault balance fails with Custom(26)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_exceeds_available() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Deposit only 100 USDC
    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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
        100 * USDC,
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
        .expect("deposit");

    // Try to borrow above available vault balance.
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());

    // x+1 boundary: 101 USDC on 100 USDC available must fail atomically.
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC + 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 26);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after, "Whitelist state must remain unchanged");
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);

    // x boundary: exact available amount must succeed.
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
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
        .expect("borrow at available boundary should succeed");
    let market_data = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(parse_total_borrowed(&market_data), 100 * USDC);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(parse_whitelist_current_borrowed(&wl_data), 100 * USDC);
    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance, 100 * USDC);

    // After draining borrowable funds, +1 should fail again and stay atomic.
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
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
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 26);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);
}

// ---------------------------------------------------------------------------
// Test 7: Borrow exceeding global capacity fails with Custom(27)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_global_capacity_exceeded() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Set whitelist max_borrow_capacity to only 100 USDC
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
        100 * USDC, // max_borrow_capacity = 100 USDC
    )
    .await;

    // Deposit enough to cover the borrow request
    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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
        .expect("deposit");

    // Try to borrow above global whitelist capacity.
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());

    // x+1 boundary: 101 USDC on capacity 100 must fail atomically.
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC + 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 27);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);

    // x boundary: borrow exactly to capacity should succeed.
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
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
        .expect("borrow at global capacity should succeed");
    let market_data = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(parse_total_borrowed(&market_data), 100 * USDC);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(parse_whitelist_current_borrowed(&wl_data), 100 * USDC);

    // At full capacity, +1 should fail and remain atomic.
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
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
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 27);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);
}

// ---------------------------------------------------------------------------
// Test 8: Repay succeeds and updates state correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_repay_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Deposit 1000 USDC
    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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
        .expect("deposit");

    // Borrow 500 USDC
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow");

    // Repay boundaries around outstanding debt.
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let vault_balance_before = common::get_token_balance(&mut ctx, &vault).await;
    // Add one token-unit so x+1 exercises debt-bound check, not token-balance check.
    common::mint_to_account(&mut ctx, &mint, &borrower_token_kp.pubkey(), &admin, 1).await;
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance_before, 500 * USDC + 1);

    // x+1 boundary: over-repay should fail with RepaymentExceedsDebt.
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC + 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 35);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after_fail =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_after_fail, borrower_balance_before);

    // x-1 boundary: partial repay should succeed.
    let repay_amount = 500 * USDC - 1;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("partial repay should succeed");

    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(parse_whitelist_current_borrowed(&wl_data), 1);

    // x boundary: repay remaining debt exactly.
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("final repay should succeed");

    // Verify final effects and exact accounting.
    let vault_balance_after = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance_after, vault_balance_before + 500 * USDC);

    let borrower_balance_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance_after, 1);

    let market_data = common::get_account_data(&mut ctx, &market).await;
    let total_repaid = parse_total_repaid(&market_data);
    assert_eq!(total_repaid, 500 * USDC);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(parse_whitelist_current_borrowed(&wl_data), 0);
}

// ---------------------------------------------------------------------------
// Test 9: Repay with zero amount fails with Custom(17)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_repay_zero_amount() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Setup debt so boundary neighbor repay(1) is meaningful.
    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");
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
        .expect("deposit");

    let payer_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &payer_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        5 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow");

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());

    // x boundary: repay(0) must fail atomically.
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let payer_before = common::get_token_balance(&mut ctx, &payer_token_kp.pubkey()).await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &payer_token_kp.pubkey(),
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
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 17);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let payer_after = common::get_token_balance(&mut ctx, &payer_token_kp.pubkey()).await;
    assert_eq!(payer_before, payer_after);

    // x+1 boundary neighbor: repay(1) should succeed and decrement debt exactly.
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &payer_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("repay(1) should succeed");
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(parse_whitelist_current_borrowed(&wl_data), 5 * USDC - 1);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(parse_total_repaid(&market_data), 1);
}

// ---------------------------------------------------------------------------
// Test 10: Third party can repay on behalf of the borrower
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_repay_by_third_party() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    for kp in [&admin, &borrower, &whitelist_manager] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                5_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("airdrop");
    }

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

    // Deposit 1000 USDC from a lender
    let lender = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund lender");

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
        .expect("deposit");

    // Borrow 500 USDC as the borrower
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow");

    // Create third-party payer
    let third_party = Keypair::new();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &third_party.pubkey(),
            2_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("fund third party");

    let third_party_token_kp =
        common::create_token_account(&mut ctx, &mint, &third_party.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &third_party_token_kp.pubkey(),
        &admin,
        600 * USDC,
    )
    .await;

    // Third-party repayment boundaries.
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let vault_balance_before = common::get_token_balance(&mut ctx, &vault).await;
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let third_party_balance_before =
        common::get_token_balance(&mut ctx, &third_party_token_kp.pubkey()).await;
    assert_eq!(third_party_balance_before, 600 * USDC);

    // x+1 boundary: over-repay should fail atomically.
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let repay_ix = common::build_repay(
        &market,
        &third_party.pubkey(),
        &third_party_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC + 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&third_party.pubkey()),
        &[&third_party],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 35);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let third_party_after_fail =
        common::get_token_balance(&mut ctx, &third_party_token_kp.pubkey()).await;
    let borrower_after_fail =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(third_party_after_fail, third_party_balance_before);
    assert_eq!(borrower_after_fail, borrower_balance_before);

    // x-1 boundary: repay 499 should succeed.
    let repay_ix = common::build_repay(
        &market,
        &third_party.pubkey(),
        &third_party_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC - 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&third_party.pubkey()),
        &[&third_party],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("third-party partial repay should succeed");
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(parse_whitelist_current_borrowed(&wl_data), 1);

    // x boundary: repay remaining 1 should succeed.
    let repay_ix = common::build_repay(
        &market,
        &third_party.pubkey(),
        &third_party_token_kp.pubkey(),
        &mint,
        &borrower.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&third_party.pubkey()),
        &[&third_party],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("third-party final repay should succeed");

    // Verify accounting and side-effects.
    let vault_balance_after = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance_after, vault_balance_before + 500 * USDC);
    let third_party_balance_after =
        common::get_token_balance(&mut ctx, &third_party_token_kp.pubkey()).await;
    assert_eq!(
        third_party_balance_after,
        third_party_balance_before - 500 * USDC
    );
    let borrower_balance_after =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance_after, borrower_balance_before);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let total_repaid = parse_total_repaid(&market_data);
    assert_eq!(total_repaid, 500 * USDC);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(parse_whitelist_current_borrowed(&wl_data), 0);
}
