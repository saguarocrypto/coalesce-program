//! Comprehensive pause tests for the CoalesceFi protocol.
//!
//! Tests all 6 paused instructions (deposit, borrow, repay, repay_interest,
//! withdraw, collect_fees), pause/unpause roundtrips, admin bypass, and edge
//! cases like double-pause/unpause idempotency.

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
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};

// ---------------------------------------------------------------------------
// Helper: full setup returning all relevant keypairs and addresses
// ---------------------------------------------------------------------------
struct TestSetup {
    admin: Keypair,
    borrower: Keypair,
    lender: Keypair,
    whitelist_manager: Keypair,
    fee_authority: Keypair,
    blacklist_program: Pubkey,
    mint: Pubkey,
    mint_authority: Keypair,
    market: Pubkey,
    lender_token_account: Pubkey,
    borrower_token_account: Pubkey,
    fee_token_account: Pubkey,
}

const MATURITY_OFFSET: i64 = 365 * 24 * 60 * 60; // 1 year from now

async fn full_setup(ctx: &mut solana_program_test::ProgramTestContext) -> TestSetup {
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let fee_authority = Keypair::new();
    let blacklist_program = Pubkey::new_unique();
    let mint_authority = Keypair::new();

    // Airdrop SOL
    airdrop_multiple(
        ctx,
        &[
            &admin,
            &borrower,
            &lender,
            &whitelist_manager,
            &fee_authority,
        ],
        10_000_000_000,
    )
    .await;

    // Create mint
    let mint = create_mint(ctx, &mint_authority, 6).await;

    // Create token accounts
    let lender_ta = create_token_account(ctx, &mint, &lender.pubkey()).await;
    let borrower_ta = create_token_account(ctx, &mint, &borrower.pubkey()).await;
    let fee_ta = create_token_account(ctx, &mint, &fee_authority.pubkey()).await;

    // Mint tokens to lender
    mint_to_account(
        ctx,
        &mint,
        &lender_ta.pubkey(),
        &mint_authority,
        10_000_000_000,
    )
    .await;
    // Mint tokens to borrower (for repayment)
    mint_to_account(
        ctx,
        &mint,
        &borrower_ta.pubkey(),
        &mint_authority,
        10_000_000_000,
    )
    .await;

    // Setup protocol
    setup_protocol(
        ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program,
        500, // 5% fee
    )
    .await;

    // Setup blacklist (not blacklisted)
    setup_blacklist_account(ctx, &blacklist_program, &borrower.pubkey(), 0);
    setup_blacklist_account(ctx, &blacklist_program, &lender.pubkey(), 0);

    // Get clock for maturity
    let maturity = common::PINNED_EPOCH + MATURITY_OFFSET;

    // Setup market
    let market = setup_market_full(
        ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program,
        0,    // nonce
        1000, // 10% annual interest
        maturity,
        1_000_000_000, // max supply 1000 USDC
        &whitelist_manager,
        1_000_000_000, // max borrow capacity
    )
    .await;

    TestSetup {
        admin,
        borrower,
        lender,
        whitelist_manager,
        fee_authority,
        blacklist_program,
        mint,
        mint_authority,
        market,
        lender_token_account: lender_ta.pubkey(),
        borrower_token_account: borrower_ta.pubkey(),
        fee_token_account: fee_ta.pubkey(),
    }
}

// ===========================================================================
// Test 1: All 6 paused instructions return error 8 (ProtocolPaused)
// Checks: deposit, borrow, repay, repay_interest, withdraw, collect_fees
// ===========================================================================
#[tokio::test]
async fn test_all_paused_instructions_return_error_8() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit first (before pausing) so we have state to work with
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;

    // Borrow some amount
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    // Now pause the protocol
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // 1. Deposit (disc 3)
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    send_expect_error(&mut ctx, deposit_ix, &[&s.lender], 8).await;

    // 2. Borrow (disc 4)
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        1_000_000,
    );
    send_expect_error(&mut ctx, borrow_ix, &[&s.borrower], 8).await;

    // 3. Repay (disc 5)
    let repay_ix = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.mint,
        &s.borrower.pubkey(),
        1_000_000,
    );
    send_expect_error(&mut ctx, repay_ix, &[&s.borrower], 8).await;

    // 4. RepayInterest (disc 6)
    let repay_interest_ix = build_repay_interest_with_amount(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        1_000_000,
    );
    send_expect_error(&mut ctx, repay_interest_ix, &[&s.borrower], 8).await;

    // 5. Withdraw (disc 7)
    let lender_pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let parsed_pos = parse_lender_position(&lender_pos_data);
    let withdraw_ix = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.blacklist_program,
        parsed_pos.scaled_balance,
        0,
    );
    send_expect_error(&mut ctx, withdraw_ix, &[&s.lender], 8).await;

    // 6. CollectFees (disc 8)
    let collect_fees_ix =
        build_collect_fees(&s.market, &s.fee_authority.pubkey(), &s.fee_token_account);
    send_expect_error(&mut ctx, collect_fees_ix, &[&s.fee_authority], 8).await;
}

// ===========================================================================
// Test 2: repay_interest fails when paused
// ===========================================================================
#[tokio::test]
async fn test_repay_interest_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit & borrow
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // repay_interest should fail
    let ri_ix = build_repay_interest_with_amount(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        1_000,
    );
    send_expect_error(&mut ctx, ri_ix, &[&s.borrower], 8).await;
}

// ===========================================================================
// Test 3: collect_fees fails when paused
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    let cf_ix = build_collect_fees(&s.market, &s.fee_authority.pubkey(), &s.fee_token_account);
    send_expect_error(&mut ctx, cf_ix, &[&s.fee_authority], 8).await;
}

// ===========================================================================
// Test 4: Admin instructions work when paused
// ===========================================================================
#[tokio::test]
async fn test_admin_instructions_work_when_paused() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // SetFeeConfig should still work
    let sfc_ix = build_set_fee_config(&s.admin.pubkey(), &s.fee_authority.pubkey(), 600);
    send_ok(&mut ctx, sfc_ix, &[&s.admin]).await;

    // SetBorrowerWhitelist should still work
    let new_borrower = Keypair::new();
    let sbw_ix = build_set_borrower_whitelist(
        &s.whitelist_manager.pubkey(),
        &new_borrower.pubkey(),
        1,
        1_000_000,
    );
    send_ok(&mut ctx, sbw_ix, &[&s.whitelist_manager]).await;

    // SetAdmin should work
    let new_admin = Keypair::new();
    let sa_ix = build_set_admin(&s.admin.pubkey(), &new_admin.pubkey());
    send_ok(&mut ctx, sa_ix, &[&s.admin]).await;

    // SetWhitelistManager should work (with new admin)
    airdrop_multiple(&mut ctx, &[&new_admin], 1_000_000_000).await;
    let new_wm = Keypair::new();
    let swm_ix = build_set_whitelist_manager(&new_admin.pubkey(), &new_wm.pubkey());
    send_ok(&mut ctx, swm_ix, &[&new_admin]).await;

    // SetBlacklistMode should work
    let sbm_ix = build_set_blacklist_mode(&new_admin.pubkey(), true);
    send_ok(&mut ctx, sbm_ix, &[&new_admin]).await;
}

// ===========================================================================
// Test 5: Pause -> deposit fails -> unpause -> deposit succeeds -> pause -> fails
// ===========================================================================
#[tokio::test]
async fn test_pause_unpause_roundtrip_deposit() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Deposit should fail
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    send_expect_error(&mut ctx, deposit_ix, &[&s.lender], 8).await;

    // Unpause
    let unpause_ix = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix, &[&s.admin]).await;

    // Deposit should succeed (ComputeBudget differentiates from the paused attempt)
    let deposit_ix2 = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            deposit_ix2,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &s.lender],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Pause again
    let pause_ix2 = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix2, &[&s.admin]).await;

    // Deposit should fail again (ComputeBudget differentiates from previous attempts)
    let deposit_ix3 = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(250_000),
            deposit_ix3,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &s.lender],
        bh,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 8);
}

// ===========================================================================
// Test 6: Pause -> borrow fails -> unpause -> borrow succeeds
// ===========================================================================
#[tokio::test]
async fn test_pause_unpause_roundtrip_borrow() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit first
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Borrow should fail
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        10_000_000,
    );
    send_expect_error(&mut ctx, borrow_ix, &[&s.borrower], 8).await;

    // Unpause
    let unpause_ix = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix, &[&s.admin]).await;

    // Borrow should succeed (ComputeBudget differentiates from the paused attempt)
    let borrow_ix2 = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        10_000_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            borrow_ix2,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &s.borrower],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// ===========================================================================
// Test 7: Pause -> repay fails -> unpause -> repay succeeds
// ===========================================================================
#[tokio::test]
async fn test_pause_unpause_roundtrip_repay() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit & borrow
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow_ix, &[&s.borrower]).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Repay should fail
    let repay_ix = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.mint,
        &s.borrower.pubkey(),
        10_000_000,
    );
    send_expect_error(&mut ctx, repay_ix, &[&s.borrower], 8).await;

    // Unpause
    let unpause_ix = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix, &[&s.admin]).await;

    // Repay should succeed (ComputeBudget differentiates from the paused attempt)
    let repay_ix2 = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.mint,
        &s.borrower.pubkey(),
        10_000_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            repay_ix2,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &s.borrower],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// ===========================================================================
// Test 8: Pause -> withdraw fails -> unpause -> withdraw succeeds
// ===========================================================================
#[tokio::test]
async fn test_pause_unpause_roundtrip_withdraw() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;

    // Advance past maturity + grace period
    let market_data = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Withdraw should fail
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw_ix = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    send_expect_error(&mut ctx, withdraw_ix, &[&s.lender], 8).await;

    // Unpause
    let unpause_ix = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix, &[&s.admin]).await;

    // Withdraw should succeed (ComputeBudget differentiates from the paused attempt)
    let withdraw_ix2 = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            withdraw_ix2,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &s.lender],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// ===========================================================================
// Test 9: Pause state preserved across transactions
// ===========================================================================
#[tokio::test]
async fn test_pause_state_preserved_across_transactions() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Send multiple failing deposit transactions
    for _ in 0..3 {
        let deposit_ix = build_deposit(
            &s.market,
            &s.lender.pubkey(),
            &s.lender_token_account,
            &s.mint,
            &s.blacklist_program,
            1_000_000,
        );
        send_expect_error(&mut ctx, deposit_ix, &[&s.lender], 8).await;
    }

    // Verify protocol is still paused by checking one more instruction
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        1_000_000,
    );
    send_expect_error(&mut ctx, borrow_ix, &[&s.borrower], 8).await;
}

// ===========================================================================
// Test 10: Double pause is idempotent
// ===========================================================================
#[tokio::test]
async fn test_double_pause_idempotent() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Pause twice
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    let pause_ix2 = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix2, &[&s.admin]).await;

    // Still paused
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    send_expect_error(&mut ctx, deposit_ix, &[&s.lender], 8).await;
}

// ===========================================================================
// Test 11: Double unpause is idempotent
// ===========================================================================
#[tokio::test]
async fn test_double_unpause_idempotent() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Unpause twice (already unpaused)
    let unpause_ix = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix, &[&s.admin]).await;

    let unpause_ix2 = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix2, &[&s.admin]).await;

    // Still functional
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;
}

// ===========================================================================
// Test 12: Pause check fires before input validation
// ===========================================================================
#[tokio::test]
async fn test_pause_check_before_other_validation() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit first so market has state
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Try borrow with excessive amount — would normally be BorrowAmountTooHigh (26),
    // but pause check fires before business-logic validation.
    // Note: ZeroAmount(17) fires before pause because instruction data is parsed first.
    let borrow_ix = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        &s.blacklist_program,
        999_999_999,
    );
    send_expect_error(&mut ctx, borrow_ix, &[&s.borrower], 8).await;
}

// ===========================================================================
// Test 13: After pause, account data is still readable
// ===========================================================================
#[tokio::test]
async fn test_pause_does_not_affect_state_reads() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit some funds
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;

    // Read state before pause
    let market_data_before = get_account_data(&mut ctx, &s.market).await;
    let parsed_before = parse_market(&market_data_before);

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Read state after pause — market data should be unchanged
    let market_data_after = get_account_data(&mut ctx, &s.market).await;
    let parsed_after = parse_market(&market_data_after);

    assert_eq!(parsed_before.total_deposited, parsed_after.total_deposited);
    assert_eq!(parsed_before.scale_factor, parsed_after.scale_factor);
    assert_eq!(parsed_before.borrower, parsed_after.borrower);
}

// ===========================================================================
// Test 14: Admin can call set_pause even while paused
// ===========================================================================
#[tokio::test]
async fn test_set_pause_itself_works_when_paused() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // set_pause(false) should still work
    let unpause_ix = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix, &[&s.admin]).await;

    // Verify it's unpaused
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;
}

// ===========================================================================
// Test 15: Pause -> collect_fees fails -> unpause -> collect_fees works
// ===========================================================================
#[tokio::test]
async fn test_pause_unpause_roundtrip_collect_fees() {
    let mut ctx = common::start_context().await;
    let s = full_setup(&mut ctx).await;

    // Deposit and add interest so vault is solvent
    let deposit_ix = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, deposit_ix, &[&s.lender]).await;

    // Use repay_interest to make vault solvent (covers accrued interest + fees)
    let ri_ix = build_repay_interest_with_amount(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_token_account,
        50_000_000,
    );
    send_ok(&mut ctx, ri_ix, &[&s.borrower]).await;

    // Advance past maturity + grace period
    let market_data = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Withdraw to settle (settlement_factor == WAD since vault is solvent)
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw_ix = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_token_account,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    send_ok(&mut ctx, withdraw_ix, &[&s.lender]).await;

    // Pause
    let pause_ix = build_set_pause(&s.admin.pubkey(), true);
    send_ok(&mut ctx, pause_ix, &[&s.admin]).await;

    // Collect fees should fail
    let cf_ix = build_collect_fees(&s.market, &s.fee_authority.pubkey(), &s.fee_token_account);
    send_expect_error(&mut ctx, cf_ix, &[&s.fee_authority], 8).await;

    // Unpause
    let unpause_ix = build_set_pause(&s.admin.pubkey(), false);
    send_ok(&mut ctx, unpause_ix, &[&s.admin]).await;

    // Collect fees should succeed (ComputeBudget differentiates from the paused attempt)
    let cf_ix2 = build_collect_fees(&s.market, &s.fee_authority.pubkey(), &s.fee_token_account);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            cf_ix2,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &s.fee_authority],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}
