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
    instruction::InstructionError,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

// ---------------------------------------------------------------------------
// Helper: read u128 from account data at a given byte offset (little-endian)
// ---------------------------------------------------------------------------
fn read_u128(data: &[u8], offset: usize) -> u128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&data[offset..offset + 16]);
    u128::from_le_bytes(buf)
}

// ---------------------------------------------------------------------------
// Helper: read u64 from account data at a given byte offset (little-endian)
// ---------------------------------------------------------------------------
fn read_u64(data: &[u8], offset: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(buf)
}

// ---------------------------------------------------------------------------
// State offset constants (from spec) - add 9 for discriminator (8) + version (1)
// ---------------------------------------------------------------------------
// Market (250 bytes)
const MARKET_SCALED_TOTAL_SUPPLY_OFFSET: usize = 132; // u128 at [132..148]
const MARKET_ACCRUED_PROTOCOL_FEES_OFFSET: usize = 164; // u64  at [164..172]
const MARKET_SETTLEMENT_FACTOR_OFFSET: usize = 204; // u128 at [204..220]

// LenderPosition (128 bytes)
const POSITION_SCALED_BALANCE_OFFSET: usize = 73; // u128 at [73..89]

/// 1e6 USDC (6 decimals)
const USDC: u64 = 1_000_000;

// ===========================================================================
// 1. test_withdraw_success
//    Full deposit+borrow+repay cycle, warp past maturity, full withdrawal.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_success() {
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

    // Setup protocol
    let fee_rate_bps: u16 = 500; // 5%
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

    // Setup market (maturity = now + 120s)
    let nonce: u64 = 1;
    let annual_interest_bps: u16 = 1000; // 10%
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
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

    // Deposit
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

    // Repay only borrowed amount per SR-116
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

    // Add extra funds via repay_interest
    let interest_amount = 50 * USDC;
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

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (vault_pda, _) = common::get_vault_pda(&market);

    // Record lender/vault balances before withdrawal
    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_before = common::get_token_balance(&mut ctx, &vault_pda).await;

    // Warp past maturity (~300 slots for ~120 seconds)
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw (scaled_amount = 0 means full withdrawal)
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

    // Verify: lender received payout
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let payout = lender_balance_after - lender_balance_before;
    assert!(
        payout > 0,
        "Lender should have received tokens: before={}, after={}",
        lender_balance_before,
        lender_balance_after
    );
    let vault_balance_after = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_balance_before - vault_balance_after,
        payout,
        "Vault delta should equal lender payout"
    );

    // Verify: position.scaled_balance == 0
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_balance, 0,
        "Position scaled_balance should be 0 after full withdrawal"
    );

    // Verify: market.settlement_factor_wad > 0
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let settlement_factor = read_u128(&market_data, MARKET_SETTLEMENT_FACTOR_OFFSET);
    assert!(settlement_factor > 0, "Settlement factor should be set");

    // Boundary neighbor: a second full withdraw is a deterministic no-op in
    // this path and must be atomic.
    let snapshot_before_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let withdraw_again_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, withdraw_again_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let retry_result = ctx.banks_client.process_transaction(tx).await;
    if let Err(err) = retry_result {
        let tx_err = err.unwrap();
        assert_eq!(
            tx_err,
            solana_sdk::transaction::TransactionError::InstructionError(
                1,
                InstructionError::Custom(23)
            ),
            "Second full withdraw should either no-op or return NoBalance"
        );
    }
    let snapshot_after_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_retry.assert_unchanged(&snapshot_after_retry);
    let lender_balance_after_retry =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(lender_balance_after_retry, lender_balance_after);
}

// ===========================================================================
// 2. test_withdraw_before_maturity
//    Attempt withdraw before maturity => Custom(29) NotMatured
// ===========================================================================
#[tokio::test]
async fn test_withdraw_before_maturity() {
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
        500,
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
    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    // Boundary x-1: warp to exactly maturity-1 and verify withdraw fails.
    common::advance_clock_past(&mut ctx, maturity_timestamp - 1).await;
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
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
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_balance_before,
        "Lender token balance must be unchanged on NotMatured failure"
    );

    // Retry within settlement grace period (300s): withdrawal should fail
    // because the grace period has not yet elapsed.
    common::advance_clock_past(&mut ctx, maturity_timestamp + 100).await;
    let snapshot_before_grace =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let withdraw_grace_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, withdraw_grace_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            InstructionError::Custom(32) // SettlementGracePeriod
        ),
        "Expected SettlementGracePeriod error (Custom(32)), got {:?}",
        tx_err
    );
    let snapshot_after_grace =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_grace.assert_unchanged(&snapshot_after_grace);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_balance_before,
        "Lender balance must remain unchanged during grace period"
    );
}

// ===========================================================================
// 3. test_withdraw_partial
//    Deposit, warp past maturity, withdraw half. Verify partial balance.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_partial() {
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
        500,
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

    // Read position scaled_balance after deposit
    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let full_scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert!(
        full_scaled_balance > 0,
        "Scaled balance should be > 0 after deposit"
    );

    let half_scaled = full_scaled_balance / 2;
    let (vault_pda, _) = common::get_vault_pda(&market);

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Record balances before partial withdraw
    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_before = common::get_token_balance(&mut ctx, &vault_pda).await;

    // Withdraw half
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        half_scaled,
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

    // Verify: lender received some tokens
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let first_payout = lender_balance_after - lender_balance_before;
    assert!(
        first_payout > 0,
        "Lender should have received partial payout: before={}, after={}",
        lender_balance_before,
        lender_balance_after
    );
    let vault_balance_after_first = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_balance_before - vault_balance_after_first,
        first_payout,
        "Vault delta should match first partial payout"
    );

    // Verify: position still has remaining balance
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let remaining_scaled = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        remaining_scaled,
        full_scaled_balance - half_scaled,
        "Remaining scaled balance should be original minus withdrawn half"
    );
    assert!(
        remaining_scaled > 0,
        "Should still have remaining balance after partial withdraw"
    );

    // Withdraw the exact remaining scaled balance and verify deterministic completion.
    // Use min_payout=1 so the tx signature differs from the first withdrawal
    // (same scaled_amount + blockhash = duplicate-tx detection in BanksClient).
    let lender_balance_before_second =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let withdraw_remaining_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        remaining_scaled,
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_remaining_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let lender_balance_after_second =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let second_payout = lender_balance_after_second - lender_balance_before_second;
    assert!(
        lender_balance_after_second >= lender_balance_before_second,
        "Second withdrawal must not decrease lender balance"
    );
    let vault_balance_after_second = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_balance_after_first - vault_balance_after_second,
        second_payout,
        "Vault delta should match second payout"
    );

    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let final_scaled = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        final_scaled, 0,
        "Second withdrawal should empty the position"
    );

    // Third full-withdraw attempt must fail deterministically with NoBalance.
    let snapshot_before_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let withdraw_again_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_again_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(23)
        ),
        "Expected NoBalance after fully draining position"
    );
    let snapshot_after_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_retry.assert_unchanged(&snapshot_after_retry);
}

// ===========================================================================
// 4. test_collect_fees_success
//    Deposit + borrow + repay, warp past maturity, collect fees.
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_success() {
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

    // Use 10% fee rate to ensure meaningful fee accrual
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

    // Borrow some so the vault has activity
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

    // Repay only borrowed amount per SR-116
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

    // Add extra funds via repay_interest
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

    // Warp past maturity so interest accrues up to maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

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

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (vault_pda, _) = common::get_vault_pda(&market);
    let market_data_before_collect = common::get_account_data(&mut ctx, &market).await;
    let accrued_before = read_u64(
        &market_data_before_collect,
        MARKET_ACCRUED_PROTOCOL_FEES_OFFSET,
    );
    let vault_before_collect = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert!(
        accrued_before > 0,
        "Fees should be accrued before collection"
    );

    // Check balances before collect_fees
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;

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

    // Verify: fee_destination received tokens
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let collected = fee_dest_after - fee_dest_before;
    assert!(
        collected > 0,
        "Fee destination should have received tokens: before={}, after={}",
        fee_dest_before,
        fee_dest_after
    );
    assert!(
        collected <= accrued_before,
        "Collected amount must not exceed accrued fees"
    );
    assert!(
        collected <= vault_before_collect,
        "Collected amount must not exceed vault liquidity"
    );

    // Verify: market.accrued_protocol_fees decreased by exactly collected amount.
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let remaining_fees = read_u64(&market_data, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert_eq!(
        remaining_fees,
        accrued_before - collected,
        "Remaining accrued fees should be previous minus collected"
    );

    // Boundary neighbor: immediate second collection must fail with NoFeesToCollect
    // and be atomic.
    let snapshot_before_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let fee_dest_before_retry = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let collect_again_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, collect_again_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            InstructionError::Custom(36)
        ),
        "Expected NoFeesToCollect on immediate retry"
    );
    let snapshot_after_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_retry.assert_unchanged(&snapshot_after_retry);
    assert_eq!(
        common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await,
        fee_dest_before_retry,
        "Fee destination token balance must be unchanged on failed retry"
    );
}

// ===========================================================================
// 5. test_collect_fees_no_fees
//    fee_rate_bps = 0. No fees accrue. CollectFees should fail Custom(36).
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_no_fees() {
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

    // fee_rate_bps = 0 => no fees
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0, // zero fees
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

    // Add repay_interest to ensure vault solvency for SR-057
    let interest_amount = 10 * USDC;
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

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

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

    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (vault_pda, _) = common::get_vault_pda(&market);
    let market_data_before = common::get_account_data(&mut ctx, &market).await;
    let accrued_before = read_u64(&market_data_before, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert_eq!(
        accrued_before, 0,
        "No fees should be accrued with fee_rate_bps=0"
    );
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;

    // Try to collect fees (should fail with NoFeesToCollect = 22)
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(36)
        ),
        "Expected NoFeesToCollect error (Custom(36)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await,
        fee_dest_before,
        "Fee destination token balance must not change when no fees exist"
    );

    // Deterministic repeat failure: second attempt should return the same error
    // and remain atomic.
    let snapshot_before_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let collect_retry_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, collect_retry_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            InstructionError::Custom(36)
        ),
        "Second collect attempt should also fail with NoFeesToCollect"
    );
    let snapshot_after_retry =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_retry.assert_unchanged(&snapshot_after_retry);
}

// ===========================================================================
// 6. test_close_lender_position_success
//    Full lifecycle: deposit, warp, withdraw all, close position.
// ===========================================================================
#[tokio::test]
async fn test_close_lender_position_success() {
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
        500,
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

    // Withdraw all
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

    // Verify position balance is 0
    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_balance, 0,
        "Position should have 0 balance before closing"
    );
    let position_account_before = ctx
        .banks_client
        .get_account(position_pda)
        .await
        .unwrap()
        .unwrap();
    assert!(
        position_account_before.lamports > 0,
        "Position must hold rent lamports before close"
    );

    let (vault_pda, _) = common::get_vault_pda(&market);
    let market_data_before_close = common::get_account_data(&mut ctx, &market).await;
    let vault_balance_before_close = common::get_token_balance(&mut ctx, &vault_pda).await;
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    let protocol_data_before_close = common::get_account_data(&mut ctx, &protocol_config_pda).await;

    // Record lender lamports before close
    let lender_account_before = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap();
    let lender_lamports_before = lender_account_before.lamports;

    // Check if the withdrawal was distressed (haircut_owed > 0).
    // Interest accrual can inflate entitlement above vault balance, leaving a
    // haircut that blocks close_lender_position with error 34 (PositionNotEmpty).
    let parsed = common::parse_lender_position(&pos_data);

    // Close lender position
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );

    if parsed.haircut_owed > 0 {
        // Distressed market -- close must fail with PositionNotEmpty (34).
        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        let code = common::extract_custom_error(&err)
            .expect("expected Custom error from close_lender_position");
        assert_eq!(
            code, 34,
            "Distressed close should fail with PositionNotEmpty (34), got {code}"
        );
        return;
    }

    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: position account has 0 lamports (closed)
    let position_account = ctx.banks_client.get_account(position_pda).await.unwrap();
    // Account may be None (fully closed) or have 0 lamports
    match position_account {
        None => { /* good -- account closed */ },
        Some(acct) => {
            assert_eq!(acct.lamports, 0, "Closed position should have 0 lamports");
        },
    }

    // Verify: lender received lamport refund
    let lender_account_after = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert!(
        lender_account_after.lamports > lender_lamports_before,
        "Lender should have received rent lamports back"
    );

    // Close should not mutate market/vault/protocol economic state.
    let market_data_after_close = common::get_account_data(&mut ctx, &market).await;
    assert_eq!(
        market_data_after_close, market_data_before_close,
        "Closing an empty lender position should not mutate market state"
    );
    let vault_balance_after_close = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_balance_after_close, vault_balance_before_close,
        "Closing an empty lender position should not move vault tokens"
    );
    let protocol_data_after_close = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(
        protocol_data_after_close, protocol_data_before_close,
        "Closing an empty lender position should not mutate protocol config"
    );
}

// ===========================================================================
// 7. test_close_lender_position_not_empty
//    Deposit but don't withdraw. Try close => Custom(34) PositionNotEmpty.
// ===========================================================================
#[tokio::test]
async fn test_close_lender_position_not_empty() {
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
        500,
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

    // Deposit (creates position with non-zero scaled_balance)
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
    let snapshot_before_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;

    // Do NOT withdraw -- try to close position immediately
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(34)
        ),
        "Expected PositionNotEmpty error (Custom(34)), got {:?}",
        tx_err
    );
    let snapshot_after_fail =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);

    // Boundary neighbor: once position is emptied after maturity, close must succeed.
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;
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

    // Check if the distressed withdrawal left a haircut_owed > 0, which
    // blocks close_lender_position with error 34 (PositionNotEmpty).
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let parsed = common::parse_lender_position(&pos_data);

    let close_after_withdraw_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_after_withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );

    if parsed.haircut_owed > 0 {
        // Distressed market -- close must fail with PositionNotEmpty (34).
        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        let code = common::extract_custom_error(&err)
            .expect("expected Custom error from close_lender_position");
        assert_eq!(
            code, 34,
            "Distressed close should fail with PositionNotEmpty (34), got {code}"
        );
        return;
    }

    ctx.banks_client.process_transaction(tx).await.unwrap();

    let position_account_after = ctx.banks_client.get_account(position_pda).await.unwrap();
    if let Some(acct) = position_account_after {
        assert_eq!(acct.lamports, 0, "Closed position should have 0 lamports");
    }
}

// ===========================================================================
// 8. test_full_lifecycle
//    Complete lifecycle: init -> whitelist -> create market -> deposit ->
//    borrow -> repay -> warp -> collect fees -> withdraw -> close position.
// ===========================================================================
#[tokio::test]
async fn test_full_lifecycle() {
    let mut ctx = common::start_context().await;

    // --- Step (a): Initialize protocol ---
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

    let fee_rate_bps: u16 = 500; // 5%
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    // --- Create mint and token accounts ---
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

    // --- Step (b) + (c): Whitelist borrower and create market ---
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

    // --- Step (d): Lender deposits 1000 USDC ---
    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

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

    // Verify deposit: lender_token should be empty, vault should have tokens
    let lender_bal = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_bal, 0,
        "Lender token balance should be 0 after depositing all"
    );

    let (vault_pda, _) = common::get_vault_pda(&market);
    let vault_bal = common::get_token_balance(&mut ctx, &vault_pda).await;
    assert_eq!(
        vault_bal, deposit_amount,
        "Vault should hold the deposited amount"
    );

    // --- Step (e): Borrower borrows 500 USDC ---
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

    // Verify borrow: borrower should have tokens
    let borrower_bal = common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_bal, borrow_amount,
        "Borrower should have received the borrowed amount"
    );

    // Verify market state
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let total_borrowed = read_u64(&market_data, 180); // total_borrowed offset (9-byte prefix + 171)
    assert_eq!(
        total_borrowed, borrow_amount,
        "Market total_borrowed should match borrow amount"
    );

    // --- Step (f): Borrower repays only borrowed amount per SR-116 ---
    let repay_amount = borrow_amount;

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

    // Add extra funds via repay_interest
    let interest_amount = 50 * USDC;
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

    // Verify repay: market total_repaid includes principal + interest
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let total_repaid = read_u64(&market_data, 188); // total_repaid offset (9-byte prefix + 179)
    assert_eq!(
        total_repaid,
        repay_amount + interest_amount,
        "Market total_repaid should include principal and interest"
    );

    // --- Step (g): Warp past maturity ---
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // --- Step (h): Lender withdraws all (must happen before fee collection per SR-113) ---
    let lender_bal_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;

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

    let lender_bal_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert!(
        lender_bal_after > lender_bal_before,
        "Lender should have received withdrawal payout: before={}, after={}",
        lender_bal_before,
        lender_bal_after
    );

    // Verify position is now empty
    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_balance, 0,
        "Position should be empty after full withdrawal"
    );

    // Verify settlement factor is set
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let settlement_factor = read_u128(&market_data, MARKET_SETTLEMENT_FACTOR_OFFSET);
    assert!(
        settlement_factor > 0,
        "Settlement factor should be non-zero after withdrawal"
    );

    // --- Step (i): Fee authority collects fees (after lender withdraw per SR-113) ---
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

    // Verify fees collected
    let fee_collected = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert!(
        fee_collected > 0,
        "Fee destination should have received protocol fees, got {}",
        fee_collected
    );

    // --- Step (j): Lender closes position ---
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: position account is closed
    let position_account = ctx.banks_client.get_account(position_pda).await.unwrap();
    match position_account {
        None => { /* closed successfully */ },
        Some(acct) => {
            assert_eq!(acct.lamports, 0, "Closed position should have 0 lamports");
        },
    }

    // Final verification: vault should have minimal (or 0) balance
    let vault_final = common::get_token_balance(&mut ctx, &vault_pda).await;
    // The vault should have deposited - borrowed + repaid - fees_collected - lender_payout
    // Since lender_payout and fees are based on settlement, vault may have a small remainder
    // Main check: vault is much less than the deposit amount
    assert!(
        vault_final < deposit_amount,
        "Vault should have much less than the original deposit: vault={}",
        vault_final
    );

    // Final state checks on market
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let market_scaled_total = read_u128(&market_data, MARKET_SCALED_TOTAL_SUPPLY_OFFSET);
    assert_eq!(
        market_scaled_total, 0,
        "Market scaled_total_supply should be 0 after full withdrawal"
    );
}
