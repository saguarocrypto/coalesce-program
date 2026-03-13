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
    instruction::{AccountMeta, Instruction, InstructionError},
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// Helper: read u128 from account data at a given byte offset (little-endian)
// ---------------------------------------------------------------------------
fn read_u128(data: &[u8], offset: usize) -> u128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&data[offset..offset + 16]);
    u128::from_le_bytes(buf)
}

// LenderPosition (128 bytes)
const POSITION_SCALED_BALANCE_OFFSET: usize = 73; // u128 at [73..89]

// ===========================================================================
// 1. test_withdraw_excess_success - Happy path
//    Setup: deposit, borrow, repay principal, repay_interest (overpay), warp past maturity,
//    lender withdraw, collect fees, then withdraw_excess.
//    Verify: borrower receives vault balance, vault is empty.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_excess_success() {
    let mut ctx = common::start_context().await;

    // Keys
    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    // Airdrop lamports
    let airdrop_amount = 10_000_000_000u64; // 10 SOL
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

    // Setup protocol with 10% fee rate
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

    // Setup market (maturity = now + 30d)
    let nonce: u64 = 1;
    let annual_interest_bps: u16 = 1000; // 10%
    let maturity_timestamp = common::SHORT_MATURITY;
    let max_total_supply = 10_000 * USDC;
    let max_borrow_capacity = 10_000 * USDC;

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
        max_borrow_capacity,
    )
    .await;

    // Step 1: Deposit
    let deposit_ix = common::build_deposit(
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

    // Step 2: Borrow
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

    // Step 3: Repay principal
    let repay_amount = borrow_amount;
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

    // Step 4: Repay interest (OVERPAY - this creates excess)
    // Overpay significantly to ensure there's excess after settlement
    let interest_overpay = 200 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_overpay,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_overpay,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Step 5: Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 10_000).await;

    // Step 6: Lender withdraws all
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full withdrawal
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify position is now empty
    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_balance, 0,
        "Position should be empty after full withdrawal"
    );

    // Step 7: Collect fees
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Check vault balance before withdraw_excess
    let (vault_pda, _) = common::get_vault_pda(&market);
    let vault_balance_before = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert!(
        vault_balance_before > 0,
        "Vault should have excess after settlement, got {}",
        vault_balance_before
    );

    // Record borrower balance before withdraw_excess
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;

    // Step 8: Withdraw excess
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: borrower received vault balance
    let borrower_balance_after =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert!(
        borrower_balance_after > borrower_balance_before,
        "Borrower should have received excess: before={}, after={}",
        borrower_balance_before,
        borrower_balance_after
    );
    assert_eq!(
        borrower_balance_after - borrower_balance_before,
        vault_balance_before,
        "Borrower should have received the entire vault excess"
    );

    // Verify: vault is empty
    let vault_balance_after = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_balance_after, 0,
        "Vault should be empty after withdraw_excess"
    );
}

// ===========================================================================
// 2. test_withdraw_excess_before_maturity - Should fail with NotMatured (18)
// ===========================================================================
#[tokio::test]
async fn test_withdraw_excess_before_maturity() {
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

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000,
    )
    .await;

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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

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

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (vault_pda, _) = common::get_vault_pda(&market);
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    // Boundary x-1: withdraw_excess must fail right before maturity.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp - 1).await;
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Expect Custom(29) = NotMatured
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(29)
        ),
        "Expected NotMatured error (Custom(29)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before,
        "Borrower token balance must not change on NotMatured failure"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_balance_before,
        "Lender token balance must not change on NotMatured failure"
    );

    // Late retry: the failure must stay in the maturity/pending-withdrawals
    // family and remain fully atomic.
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;
    let snapshot_before_late_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let borrower_balance_before_late =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert!(
        tx_err
            == solana_sdk::transaction::TransactionError::InstructionError(
                1,
                InstructionError::Custom(29)
            )
            || tx_err
                == solana_sdk::transaction::TransactionError::InstructionError(
                    1,
                    InstructionError::Custom(38)
                ),
        "Expected NotMatured (Custom(29)) or LendersPendingWithdrawals (Custom(38)) on late retry, got {:?}",
        tx_err
    );
    let snapshot_after_late_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_late_fail.assert_unchanged(&snapshot_after_late_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before_late,
        "Borrower token balance must remain unchanged on mature failure"
    );
}

// ===========================================================================
// 3. test_withdraw_excess_lenders_pending - Should fail with LendersPendingWithdrawals (36)
//    Don't have lender withdraw before calling.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_excess_lenders_pending() {
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

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000,
    )
    .await;

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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

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

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (vault_pda, _) = common::get_vault_pda(&market);
    let borrower_balance_before_fail =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;

    // Do NOT withdraw - lenders have pending withdrawals (scaled_total_supply > 0)
    // Try withdraw_excess.
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Expect Custom(38) = LendersPendingWithdrawals
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(38)
        ),
        "Expected LendersPendingWithdrawals error (Custom(38)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before_fail,
        "Borrower token balance must not change on pending-withdrawals failure"
    );

    // Boundary neighbor: after lender drains their position, the pending-lender
    // gate clears and distress gating becomes the next deterministic failure.
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

    let snapshot_before_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let borrower_balance_before_retry =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            InstructionError::Custom(37)
        ),
        "Expected FeeCollectionDuringDistress (Custom(37)) once lender is drained"
    );
    let snapshot_after_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_retry.assert_unchanged(&snapshot_after_retry);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before_retry,
        "Borrower token balance must not change on no-excess failure"
    );
}

// ===========================================================================
// 4. test_withdraw_excess_settlement_not_complete - Should fail with SettlementNotComplete (41)
//    No withdrawals have occurred yet (settlement_factor_wad == 0).
// ===========================================================================
#[tokio::test]
async fn test_withdraw_excess_settlement_not_complete() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [&admin, &fee_authority, &whitelist_manager, &borrower] {
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

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000,
    )
    .await;

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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Create market with no deposits (scaled_total_supply == 0)
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

    let (vault_pda, _) = common::get_vault_pda(&market);
    let no_positions: [solana_sdk::pubkey::Pubkey; 0] = [];
    let borrower_balance_before =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;

    // Boundary x-1: before maturity this must fail as NotMatured.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp - 1).await;
    let snapshot_before_early_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &no_positions).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(29)
        ),
        "Expected NotMatured (Custom(29)) before maturity"
    );
    let snapshot_after_early_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &no_positions).await;
    snapshot_before_early_fail.assert_unchanged(&snapshot_after_early_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before,
        "Borrower token balance must not change on early failure"
    );

    // Boundary x+1: once mature, with no settlement progress this must fail as SettlementNotComplete.
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &no_positions).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Expect Custom(33) = SettlementNotComplete
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            InstructionError::Custom(33)
        ),
        "Expected SettlementNotComplete error (Custom(33)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &no_positions).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before,
        "Borrower token balance must not change on settlement-not-complete failure"
    );
}

// ===========================================================================
// 5. test_withdraw_excess_fees_not_collected - Should fail with FeesNotCollected (42)
//    Skip collect_fees step.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_excess_fees_not_collected() {
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

    // Use non-zero fee rate to ensure fees accrue
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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::SHORT_MATURITY;

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

    // Repay principal
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        borrow_amount,
    )
    .await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        borrow_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Repay interest (overpay)
    let interest_overpay = 200 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_overpay,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_overpay,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender withdraws all
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

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (vault_pda, _) = common::get_vault_pda(&market);

    // Skip collect_fees step - try withdraw_excess directly.
    let borrower_balance_before_fail =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Expect Custom(39) = FeesNotCollected
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(39)
        ),
        "Expected FeesNotCollected error (Custom(39)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before_fail,
        "Borrower token balance must not change on FeesNotCollected failure"
    );

    // Boundary neighbor: once fees are collected, withdraw_excess should succeed
    // and transfer exactly the vault remainder.
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let vault_before_success = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert!(
        vault_before_success > 0,
        "Vault should contain withdrawable excess after fee collection"
    );
    let borrower_balance_before_success =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let borrower_balance_after_success =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance_after_success - borrower_balance_before_success,
        vault_before_success,
        "Borrower should receive exactly the full vault excess after fee collection"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault_pda).await,
        0,
        "Vault should be empty after successful withdraw_excess"
    );
}

// ===========================================================================
// 6. test_withdraw_excess_no_excess - Should fail with NoExcessToWithdraw (43)
//    Don't overpay, so vault is empty after full settlement.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_excess_no_excess() {
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

    // Use zero fee rate so no fees accrue (and vault can be drained completely)
    let fee_rate_bps: u16 = 0;
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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Deposit exactly what will be withdrawn (no excess)
    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // 0% interest rate
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

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender withdraws all (should get exactly what they deposited)
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

    // Verify vault is empty (no excess)
    let (vault_pda, _) = common::get_vault_pda(&market);
    let vault_balance = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_balance, 0,
        "Vault should be empty after withdrawal with no excess"
    );

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // Try withdraw_excess when vault is empty.
    let borrower_balance_before_fail =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Expect Custom(40) = NoExcessToWithdraw
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(40)
        ),
        "Expected NoExcessToWithdraw error (Custom(40)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before_fail,
        "Borrower token balance must not change on NoExcessToWithdraw failure"
    );

    // Boundary neighbor: add exactly one unit of excess and verify exact transfer.
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        1,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let vault_before_success = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_before_success, 1,
        "One-unit repay_interest should create one-unit excess in the vault"
    );
    let borrower_balance_before_success =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let borrower_balance_after_success =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance_after_success - borrower_balance_before_success,
        1,
        "Borrower should receive exactly one unit of excess"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault_pda).await,
        0,
        "Vault should be empty after one-unit excess withdrawal"
    );
}

// ===========================================================================
// 7. test_withdraw_excess_wrong_borrower - Should fail with Unauthorized (3)
//    Try to call with different signer.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_excess_wrong_borrower() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let wrong_borrower = Keypair::new();
    let lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &wrong_borrower,
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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let wrong_borrower_token =
        common::create_token_account(&mut ctx, &mint, &wrong_borrower.pubkey()).await;
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::SHORT_MATURITY;

    // Create market with `borrower` as the borrower
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

    // Repay principal
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        borrow_amount,
    )
    .await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        borrow_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Repay interest (overpay)
    let interest_overpay = 200 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_overpay,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_overpay,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender withdraws all
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

    // Collect fees
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // Try withdraw_excess with wrong_borrower instead of borrower.
    // Build instruction manually to pass wrong_borrower as signer.
    let (vault, _) = common::get_vault_pda(&market);
    let (market_authority, _) = common::get_market_authority_pda(&market);
    let borrower_balance_before_fail =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let wrong_borrower_balance_before_fail =
        common::get_token_balance(&mut ctx, &wrong_borrower_token.pubkey()).await;
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[position_pda]).await;
    let (protocol_config, _) = common::get_protocol_config_pda();
    let (blacklist_check, _) = common::get_blacklist_pda(&blacklist_program.pubkey(), &wrong_borrower.pubkey());
    let ix = Instruction {
        program_id: common::program_id(),
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new_readonly(wrong_borrower.pubkey(), true), // wrong borrower signs
            AccountMeta::new(wrong_borrower_token.pubkey(), false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(market_authority, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new_readonly(protocol_config, false),
            AccountMeta::new_readonly(blacklist_check, false),
        ],
        data: vec![11u8], // WithdrawExcess discriminator
    };

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Expect Custom(5) = Unauthorized
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(0, InstructionError::Custom(5)),
        "Expected Unauthorized error (Custom(5)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[position_pda]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_balance_before_fail,
        "Borrower token balance must not change on Unauthorized failure"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &wrong_borrower_token.pubkey()).await,
        wrong_borrower_balance_before_fail,
        "Wrong borrower token balance must not change on Unauthorized failure"
    );

    // Boundary neighbor: with the correct borrower signer/account, the same
    // operation should succeed and drain the exact vault remainder.
    let vault_before_success = common::get_token_balance(&mut ctx, &vault).await;
    assert!(
        vault_before_success > 0,
        "Vault should contain excess before authorized withdraw_excess"
    );
    let borrower_balance_before_success =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let withdraw_excess_ix =
        common::build_withdraw_excess(&market, &borrower.pubkey(), &borrower_token.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let borrower_balance_after_success =
        common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance_after_success - borrower_balance_before_success,
        vault_before_success,
        "Authorized borrower should receive the full vault excess"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        0,
        "Vault should be empty after authorized withdraw_excess"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &wrong_borrower_token.pubkey()).await,
        wrong_borrower_balance_before_fail,
        "Wrong borrower token account must remain unchanged"
    );
}
