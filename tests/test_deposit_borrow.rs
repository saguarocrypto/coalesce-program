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
    compute_budget::ComputeBudgetInstruction,
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

// ==========================================================================
// DEPOSIT TESTS
// ==========================================================================

// ---------------------------------------------------------------------------
// 1. Blacklisted lender cannot deposit => Custom(7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_blacklisted_lender() {
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

    // Inject blacklist PDA for the lender with status=1 (blacklisted)
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 1);

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

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
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 7);
    assert!(
        ctx.banks_client
            .get_account(lender_pos_pda)
            .await
            .unwrap()
            .is_none(),
        "Lender position must not be created for blacklisted lender"
    );
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_before, lender_after);

    // Boundary neighbor: clearing blacklist should allow the same deposit.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
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
        .expect("deposit should succeed once lender is unblacklisted");
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_deposited, 100 * USDC);
    let lender_balance = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_balance, lender_before - 100 * USDC);
    let lender_pos_data = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    let lender_pos = common::parse_lender_position(&lender_pos_data);
    assert!(lender_pos.scaled_balance > 0);
}

// ---------------------------------------------------------------------------
// 2. Deposit exactly at cap succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_exactly_at_cap() {
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
        0, // zero fees for boundary test
    )
    .await;
    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    // Market with max_total_supply = 100 USDC
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let max_supply = 100 * USDC;
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // zero interest for boundary test
        maturity_timestamp,
        max_supply,
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
        200 * USDC,
    )
    .await;

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // x-1 boundary first.
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        max_supply - 1,
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
        .expect("deposit at cap-1 should succeed");

    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_deposited, max_supply - 1);

    // x boundary: second deposit reaches cap exactly.
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
        .expect("deposit exactly at cap should succeed");

    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_deposited, max_supply);
    assert_eq!(parsed.scaled_total_supply, u128::from(max_supply));
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, max_supply);
    let lender_balance = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_balance, lender_before - max_supply);
    let lender_pos_data = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    let lender_pos = common::parse_lender_position(&lender_pos_data);
    assert_eq!(lender_pos.scaled_balance, u128::from(max_supply));

    // x+1 boundary: one more should fail atomically.
    // Use ComputeBudget to differentiate tx signature from the x-boundary deposit
    // (same instruction, same signers, same amount — would be deduplicated otherwise).
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
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            deposit_ix,
        ],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
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
// 3. Deposit one over cap fails => Custom(25)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_one_over_cap() {
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

    // Market with max_total_supply = 100 USDC
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
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
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        200 * USDC,
    )
    .await;

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let lender_before = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;

    // x+1 boundary: one over cap must fail atomically.
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
        "Position must not be created when cap check fails"
    );
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_after = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_before, lender_after);

    // Boundary neighbor x: exact cap should succeed.
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        max_supply,
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
        .expect("deposit at cap should succeed");
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_deposited, max_supply);
}

// ---------------------------------------------------------------------------
// 4. Two cumulative deposits to same position succeed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_twice_cumulative() {
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

    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    // First deposit: 500 USDC
    let deposit_ix_1 = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let tx1 = Transaction::new_signed_with_payer(
        &[deposit_ix_1],
        Some(&lender.pubkey()),
        &[&lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx1)
        .await
        .expect("first deposit should succeed");

    // Read lender position and market after first deposit
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let market_data_1 = common::get_account_data(&mut ctx, &market).await;
    let market_1 = common::parse_market(&market_data_1);
    assert_eq!(market_1.total_deposited, 500 * USDC);
    assert_eq!(market_1.scaled_total_supply, u128::from(500 * USDC));
    let pos_data_1 = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    let pos_1 = common::parse_lender_position(&pos_data_1);
    let scaled_balance_after_first = pos_1.scaled_balance;
    assert_eq!(scaled_balance_after_first, u128::from(500 * USDC));
    let lender_balance_1 = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_balance_1, 1_500 * USDC);

    // Second deposit: 500 USDC (need fresh blockhash)
    let new_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let deposit_ix_2 = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let tx2 = Transaction::new_signed_with_payer(
        &[deposit_ix_2],
        Some(&lender.pubkey()),
        &[&lender],
        new_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx2)
        .await
        .expect("second deposit should succeed");

    // Verify exact cumulative accounting after second deposit.
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed_market = common::parse_market(&market_data);
    assert_eq!(parsed_market.total_deposited, 1_000 * USDC);
    assert_eq!(parsed_market.scaled_total_supply, u128::from(1_000 * USDC));
    let (vault, _) = common::get_vault_pda(&market);
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 1_000 * USDC);
    let lender_balance_2 = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    assert_eq!(lender_balance_2, 1_000 * USDC);

    // Verify scaled_balance increased exactly.
    let pos_data_2 = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    let pos_2 = common::parse_lender_position(&pos_data_2);
    assert_eq!(pos_2.scaled_balance, u128::from(1_000 * USDC));
    assert!(pos_2.scaled_balance > scaled_balance_after_first);

    // Boundary-neighbor increment: +1 deposit updates all counters by exactly one.
    let deposit_ix_3 = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let new_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx3 = Transaction::new_signed_with_payer(
        &[deposit_ix_3],
        Some(&lender.pubkey()),
        &[&lender],
        new_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx3)
        .await
        .expect("neighbor deposit should succeed");
    let market_data_3 = common::get_account_data(&mut ctx, &market).await;
    let market_3 = common::parse_market(&market_data_3);
    assert_eq!(market_3.total_deposited, 1_000 * USDC + 1);
    let pos_data_3 = common::get_account_data(&mut ctx, &lender_pos_pda).await;
    let pos_3 = common::parse_lender_position(&pos_data_3);
    assert_eq!(pos_3.scaled_balance, u128::from(1_000 * USDC + 1));
}

// ---------------------------------------------------------------------------
// 5. Multiple lenders deposit to same market
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deposit_multiple_lenders() {
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

    // Lender A deposits 300 USDC
    let lender_a = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender_a], 5_000_000_000).await;
    let lender_a_token = common::create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &admin,
        500 * USDC,
    )
    .await;

    let deposit_a_ix = common::build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        300 * USDC,
    );
    let tx_a = Transaction::new_signed_with_payer(
        &[deposit_a_ix],
        Some(&lender_a.pubkey()),
        &[&lender_a],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx_a)
        .await
        .expect("lender A deposit should succeed");

    let (pos_a_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_a_data = common::get_account_data(&mut ctx, &pos_a_pda).await;
    let pos_a = common::parse_lender_position(&pos_a_data);
    assert_eq!(pos_a.scaled_balance, u128::from(300 * USDC));

    // Lender B deposits 700 USDC
    let lender_b = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender_b], 5_000_000_000).await;
    let lender_b_token = common::create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let new_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let deposit_b_ix = common::build_deposit(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        700 * USDC,
    );
    let tx_b = Transaction::new_signed_with_payer(
        &[deposit_b_ix],
        Some(&lender_b.pubkey()),
        &[&lender_b],
        new_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx_b)
        .await
        .expect("lender B deposit should succeed");

    // Verify exact aggregate and per-lender accounting.
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed_market = common::parse_market(&market_data);
    assert_eq!(parsed_market.total_deposited, 1_000 * USDC);
    assert_eq!(parsed_market.scaled_total_supply, u128::from(1_000 * USDC));

    let (pos_b_pda, _) = common::get_lender_position_pda(&market, &lender_b.pubkey());
    let pos_b_data = common::get_account_data(&mut ctx, &pos_b_pda).await;
    let pos_b = common::parse_lender_position(&pos_b_data);
    assert_eq!(pos_b.scaled_balance, u128::from(700 * USDC));
    let pos_a_data = common::get_account_data(&mut ctx, &pos_a_pda).await;
    let pos_a_after_b = common::parse_lender_position(&pos_a_data);
    assert_eq!(pos_a_after_b.scaled_balance, u128::from(300 * USDC));
    assert_eq!(
        pos_a_after_b.scaled_balance + pos_b.scaled_balance,
        parsed_market.scaled_total_supply
    );
    let (vault, _) = common::get_vault_pda(&market);
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 1_000 * USDC);
    let lender_a_balance = common::get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let lender_b_balance = common::get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;
    assert_eq!(lender_a_balance, 200 * USDC);
    assert_eq!(lender_b_balance, 300 * USDC);

    // Boundary-neighbor increment for lender A only.
    let deposit_a_ix = common::build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_a_ix],
        Some(&lender_a.pubkey()),
        &[&lender_a],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("neighbor deposit by lender A should succeed");
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let market_after = common::parse_market(&market_data);
    assert_eq!(market_after.total_deposited, 1_000 * USDC + 1);
    let pos_a_data = common::get_account_data(&mut ctx, &pos_a_pda).await;
    let pos_a_after = common::parse_lender_position(&pos_a_data);
    assert_eq!(pos_a_after.scaled_balance, u128::from(300 * USDC + 1));
    let pos_b_data = common::get_account_data(&mut ctx, &pos_b_pda).await;
    let pos_b_after = common::parse_lender_position(&pos_b_data);
    assert_eq!(pos_b_after.scaled_balance, u128::from(700 * USDC));
}

// ==========================================================================
// BORROW TESTS
// ==========================================================================

// ---------------------------------------------------------------------------
// 6. Borrow zero amount fails => Custom(17)
// ---------------------------------------------------------------------------

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

    // Deposit first so vault has funds
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

    // Create borrower token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;

    // Try borrow 0
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0,
    );

    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 17);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);

    // Boundary neighbor: borrow(1) should succeed and mutate state exactly.
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
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow(1) should succeed");
    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance, 1);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl = common::parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 1);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_borrowed, 1);
}

// ---------------------------------------------------------------------------
// 7. Wrong borrower (not market.borrower) fails => Custom(5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_wrong_borrower() {
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

    // A different keypair tries to borrow (not the market's borrower)
    let fake_borrower = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&fake_borrower], 5_000_000_000).await;
    let fake_borrower_token_kp =
        common::create_token_account(&mut ctx, &mint, &fake_borrower.pubkey()).await;

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let fake_before = common::get_token_balance(&mut ctx, &fake_borrower_token_kp.pubkey()).await;

    // Build borrow with fake borrower.
    let borrow_ix = common::build_borrow(
        &market,
        &fake_borrower.pubkey(),
        &fake_borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&fake_borrower.pubkey()),
        &[&fake_borrower],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 5);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let fake_after = common::get_token_balance(&mut ctx, &fake_borrower_token_kp.pubkey()).await;
    assert_eq!(fake_before, fake_after);

    // Boundary neighbor: the real borrower should succeed with the same amount.
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
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
        .expect("real borrower should succeed");
    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance, 100 * USDC);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl = common::parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 100 * USDC);
}

// ---------------------------------------------------------------------------
// 8. Blacklisted borrower cannot borrow => Custom(7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_blacklisted_borrower() {
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

    // Create borrower token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Inject blacklist PDA for the borrower with status=1 (blacklisted)
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 1);

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 7);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);

    // Boundary neighbor: clearing blacklist allows borrow.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
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
        .expect("borrow should succeed when borrower is unblacklisted");
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl = common::parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 100 * USDC);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let market_state = common::parse_market(&market_data);
    assert_eq!(market_state.total_borrowed, 100 * USDC);
}

// ---------------------------------------------------------------------------
// 9. Borrow after maturity fails => Custom(28)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_after_maturity() {
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

    // Deposit so vault has funds (before maturity)
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

    // Create borrower token account
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());

    // x-1 boundary: one second before maturity should allow borrow.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp - 1).await;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
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
        .expect("borrow before maturity should succeed");
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl = common::parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 100 * USDC);

    // x boundary: at maturity borrow should fail atomically.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp).await;
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
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 28);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);

    // x+1 boundary: after maturity should also fail atomically.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 1).await;
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
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_custom_error(result, 28);
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wl_after = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_before, wl_after);
    let borrower_after = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_before, borrower_after);
}

// ---------------------------------------------------------------------------
// 10. Borrow exactly to limit (vault balance) succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_exactly_to_limit() {
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
        0, // zero fees for boundary test
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
        0, // zero interest for boundary test
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

    // Borrow boundary checks around full vault balance.
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());

    // x-1 boundary
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        1_000 * USDC - 1,
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
        .expect("borrow at x-1 should succeed");
    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance, 1_000 * USDC - 1);
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 1);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl = common::parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 1_000 * USDC - 1);

    // x boundary: consume final token unit.
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
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow at x should succeed");
    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance, 1_000 * USDC);
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 0);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_borrowed, 1_000 * USDC);
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl = common::parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.current_borrowed, 1_000 * USDC);

    // x+1 boundary: one more should fail atomically.
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
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            borrow_ix,
        ],
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
// 11. Borrow one over limit fails => Custom(26)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_borrow_one_over_limit() {
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

    // Deposit 100 USDC
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;
    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        200 * USDC,
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
        .expect("deposit should succeed");

    // Try to borrow 101 USDC (one over the 100 USDC in vault)
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let wl_before = common::get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        101 * USDC,
    );

    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
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

    // Boundary neighbor x: borrow exactly available amount.
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
        .expect("borrow exactly at available limit should succeed");
    let borrower_balance = common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    assert_eq!(borrower_balance, 100 * USDC);
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(parsed.total_borrowed, 100 * USDC);
}
