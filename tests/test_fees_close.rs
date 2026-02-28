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
    instruction::{AccountMeta, Instruction},
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// COLLECT_FEES NEGATIVE TESTS
// ---------------------------------------------------------------------------

/// Test 1: collect_fees with wrong fee authority => Custom(5) Unauthorized.
///
/// Setup protocol with fee_authority = A. Perform a deposit + borrow + repay
/// cycle so fees accrue, advance past maturity, then attempt to collect fees
/// with a different keypair B. The program must reject the wrong authority.
#[tokio::test]
async fn test_collect_fees_wrong_fee_authority() {
    let mut ctx = common::start_context().await;

    // Keys
    let admin = Keypair::new();
    let fee_authority_kp = Keypair::new(); // the real fee authority
    let wrong_authority = Keypair::new(); // imposter
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    // Airdrop SOL
    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority_kp,
        &wrong_authority,
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

    // Initialize protocol with fee_authority_kp as the real fee authority
    let fee_rate_bps: u16 = 1000; // 10%
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority_kp.pubkey(),
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
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority_kp.pubkey()).await;

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

    // Setup market
    // Use short maturity (1 day) to keep interest accrual small and avoid distress
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

    // Repay only the borrowed amount (500 USDC) per SR-116
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

    // Add extra funds via repay_interest for vault solvency
    let interest_amount = 100 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance well past maturity + grace period and pin the clock.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 600).await;

    // Lender must withdraw before fee collection per SR-113
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
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
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Preconditions: market is otherwise fee-collectable.
    let market_before = common::parse_market(&common::get_account_data(&mut ctx, &market).await);
    assert!(
        market_before.accrued_protocol_fees > 0,
        "expected accrued fees > 0 before collect attempt"
    );
    assert_eq!(
        market_before.scaled_total_supply, 0,
        "all lenders must be withdrawn before collect_fees test"
    );

    // Try to collect fees with wrong_authority instead of fee_authority_kp.
    let (vault_pda, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let wrong_authority_lamports_before = ctx
        .banks_client
        .get_account(wrong_authority.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;

    let collect_ix =
        common::build_collect_fees(&market, &wrong_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_authority],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 5); // Unauthorized

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(
        fee_dest_after, fee_dest_before,
        "fee destination changed on unauthorized collect_fees"
    );
    let wrong_authority_lamports_after = ctx
        .banks_client
        .get_account(wrong_authority.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        wrong_authority_lamports_after, wrong_authority_lamports_before,
        "wrong authority lamports changed on unauthorized collect_fees"
    );

    // Determinism: repeated unauthorized attempt should fail identically and remain atomic.
    let snapshot_before_2 =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    let collect_ix =
        common::build_collect_fees(&market, &wrong_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_authority],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 5); // Unauthorized
    let snapshot_after_2 =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before_2.assert_unchanged(&snapshot_after_2);
}

/// Test 2: collect_fees with fee_authority AccountMeta set to is_signer=false.
///
/// Build the instruction manually so the fee_authority is NOT marked as a
/// signer. The runtime or program must reject this because the fee_authority
/// must sign the collect_fees instruction.
#[tokio::test]
async fn test_collect_fees_non_signer_fee_authority() {
    let mut ctx = common::start_context().await;

    // Keys
    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    // Airdrop SOL
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

    // Initialize protocol
    let fee_rate_bps: u16 = 1000;
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

    // Setup market
    // Use short maturity (1 day) to keep interest accrual small and avoid distress
    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

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

    // Repay only the borrowed amount (500 USDC) per SR-116
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

    // Add extra funds via repay_interest for vault solvency
    let interest_amount = 100 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance well past maturity + grace period and pin the clock.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 600).await;

    // Lender must withdraw before fee collection per SR-113
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
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
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Preconditions: market is otherwise fee-collectable.
    let market_before = common::parse_market(&common::get_account_data(&mut ctx, &market).await);
    assert!(
        market_before.accrued_protocol_fees > 0,
        "expected accrued fees > 0 before collect attempt"
    );
    assert_eq!(
        market_before.scaled_total_supply, 0,
        "all lenders must be withdrawn before collect_fees test"
    );

    // Build collect_fees instruction manually with fee_authority NOT as signer.
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (protocol_config, _) = common::get_protocol_config_pda();
    let (market_authority, _) = common::get_market_authority_pda(&market);
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let ix = Instruction {
        program_id: common::program_id(),
        accounts: vec![
            AccountMeta::new(market, false),
            AccountMeta::new_readonly(protocol_config, false),
            AccountMeta::new_readonly(fee_authority.pubkey(), false), // NOT signer!
            AccountMeta::new(fee_dest.pubkey(), false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(market_authority, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(solana_sdk::sysvar::clock::id(), false),
        ],
        data: vec![8u8], // CollectFees discriminator
    };

    // Sign only with ctx.payer (fee_authority does NOT sign)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 5); // Unauthorized

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(
        fee_dest_after, fee_dest_before,
        "fee destination changed on non-signer fee_authority collect_fees"
    );

    // Boundary neighbor: marking fee_authority as signer should permit collection.
    let collect_ok_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let fee_dest_after_success = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert!(
        fee_dest_after_success > fee_dest_before,
        "fee destination should increase when fee_authority signs collect_fees"
    );
    let market_after_success =
        common::parse_market(&common::get_account_data(&mut ctx, &market).await);
    assert!(
        market_after_success.accrued_protocol_fees < market_before.accrued_protocol_fees,
        "accrued_protocol_fees should decrease on successful collect_fees"
    );
}

// ---------------------------------------------------------------------------
// CLOSE_LENDER_POSITION NEGATIVE TESTS
// ---------------------------------------------------------------------------

/// Test 3: close_lender_position with lender AccountMeta is_signer=false.
///
/// Deposit so the position exists, advance past maturity, withdraw all, then
/// try to close the position with a manually crafted instruction where the
/// lender is NOT marked as signer. The program must reject this.
#[tokio::test]
async fn test_close_position_non_signer_lender() {
    let mut ctx = common::start_context().await;

    // Keys
    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    // Airdrop SOL
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

    // Initialize protocol
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
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
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

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

    // Setup market
    // Use short maturity (1 day) to keep interest accrual small and avoid distress
    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

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

    // Advance well past maturity + grace period and pin the clock.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 600).await;

    // Withdraw all (scaled_amount = 0 means full withdrawal)
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
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
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Preconditions: position exists and is empty (ready to close).
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_pos_before =
        common::parse_lender_position(&common::get_account_data(&mut ctx, &lender_position).await);
    assert_eq!(
        lender_pos_before.scaled_balance, 0,
        "position must be empty before close_lender_position test"
    );

    // Build close_lender_position manually with lender NOT as signer.
    let (vault_pda, _) = common::get_vault_pda(&market);
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position]).await;
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_lamports_before = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let (protocol_config, _) = common::get_protocol_config_pda();
    let ix = Instruction {
        program_id: common::program_id(),
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(lender.pubkey(), false), // NOT signer!
            AccountMeta::new(lender_position, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(protocol_config, false),
        ],
        data: vec![10u8],
    };

    // Sign only with ctx.payer (lender does NOT sign)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 5); // Unauthorized

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_position]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token changed on non-signer close_lender_position"
    );
    let lender_lamports_after = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        lender_lamports_after, lender_lamports_before,
        "lender lamports changed on non-signer close_lender_position"
    );

    // Boundary neighbor: correct signer should be able to close the now-empty position.
    let position_lamports_before_close = ctx
        .banks_client
        .get_account(lender_position)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let lender_lamports_before_close = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let close_ok_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let lender_lamports_after_close = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        lender_lamports_after_close,
        lender_lamports_before_close + position_lamports_before_close,
        "lender should receive all position lamports on successful close"
    );
    let position_after = ctx.banks_client.get_account(lender_position).await.unwrap();
    match position_after {
        None => { /* closed */ },
        Some(acct) => assert_eq!(acct.lamports, 0, "position should be closed"),
    }
}

/// Test 4: close_lender_position with wrong lender.
///
/// Create a position for lender A, withdraw all, then try to close from
/// lender B (who did not create the position). The instruction is built with
/// lender B's pubkey but passes lender A's position PDA. The program must
/// reject this because the PDA does not match lender B.
#[tokio::test]
async fn test_close_position_wrong_lender() {
    let mut ctx = common::start_context().await;

    // Keys
    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender_a = Keypair::new(); // the real position owner
    let lender_b = Keypair::new(); // the imposter

    // Airdrop SOL
    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender_a,
        &lender_b,
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

    // Initialize protocol
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
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
    let lender_a_token = common::create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;

    // Mint tokens to lender A
    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    // Setup market
    // Use short maturity (1 day) to keep interest accrual small and avoid distress
    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

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

    // Lender A deposits
    let deposit_ix = common::build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance well past maturity + grace period and pin the clock.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 600).await;

    // Lender A withdraws all
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Build close instruction with lender B as the signer but passing
    // lender A's position PDA. This should fail because the PDA derivation
    // from lender B will not match lender A's position account.
    let (lender_a_position, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let (protocol_config, _) = common::get_protocol_config_pda();
    let ix = Instruction {
        program_id: common::program_id(),
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(lender_b.pubkey(), true), // lender B signs
            AccountMeta::new(lender_a_position, false), // but this is lender A's position
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(protocol_config, false),
        ],
        data: vec![10u8],
    };

    // Preconditions: lender A position is empty and closeable.
    let lender_a_pos = common::parse_lender_position(
        &common::get_account_data(&mut ctx, &lender_a_position).await,
    );
    assert_eq!(
        lender_a_pos.scaled_balance, 0,
        "lender A position must be empty before close test"
    );

    let (vault_pda, _) = common::get_vault_pda(&market);
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_a_position])
            .await;
    let lender_a_token_before = common::get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let lender_b_lamports_before = ctx
        .banks_client
        .get_account(lender_b.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 13); // InvalidPDA

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_a_position])
            .await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_a_token_after = common::get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    assert_eq!(
        lender_a_token_after, lender_a_token_before,
        "lender A token balance changed on wrong-lender close attempt"
    );
    let lender_b_lamports_after = ctx
        .banks_client
        .get_account(lender_b.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        lender_b_lamports_after, lender_b_lamports_before,
        "lender B lamports changed on wrong-lender close attempt"
    );

    // Boundary neighbor: the true lender can close successfully.
    let position_lamports_before_close = ctx
        .banks_client
        .get_account(lender_a_position)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let lender_a_lamports_before_close = ctx
        .banks_client
        .get_account(lender_a.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let close_ok_ix = common::build_close_lender_position(&market, &lender_a.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let lender_a_lamports_after_close = ctx
        .banks_client
        .get_account(lender_a.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        lender_a_lamports_after_close,
        lender_a_lamports_before_close + position_lamports_before_close,
        "lender A should receive all position lamports on successful close"
    );
    let position_after = ctx
        .banks_client
        .get_account(lender_a_position)
        .await
        .unwrap();
    match position_after {
        None => { /* closed */ },
        Some(acct) => assert_eq!(acct.lamports, 0, "position should be closed"),
    }
}
