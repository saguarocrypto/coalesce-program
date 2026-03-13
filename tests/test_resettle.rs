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
    instruction::InstructionError, signature::Keypair, signer::Signer, transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

/// WAD = 1e18 (used for settlement factor math).
const WAD: u128 = 1_000_000_000_000_000_000;

fn expected_settlement_factor(
    vault_balance: u64,
    scaled_total_supply: u128,
    scale_factor: u128,
    haircut_accumulator: u64,
) -> u128 {
    let vault_bal_128 = u128::from(vault_balance);
    // COAL-C01: No fee reservation.
    // COAL-H01: Subtract haircut accumulator to prevent recycled inflation.
    let haircut_128 = u128::from(haircut_accumulator);
    let available_for_lenders = vault_bal_128.saturating_sub(haircut_128);
    let total_normalized = scaled_total_supply
        .checked_mul(scale_factor)
        .unwrap()
        .checked_div(WAD)
        .unwrap();

    if total_normalized == 0 {
        WAD
    } else {
        let raw = available_for_lenders
            .checked_mul(WAD)
            .unwrap()
            .checked_div(total_normalized)
            .unwrap();
        let capped = if raw > WAD { WAD } else { raw };
        if capped < 1 {
            1
        } else {
            capped
        }
    }
}

// ===========================================================================
// 1. test_re_settle_success
//    Full lifecycle: deposit, borrow, partial repay, advance past maturity,
//    withdraw (sets settlement_factor_wad > 0), repay more, re_settle => OK.
//    Verify settlement_factor_wad increased.
// ===========================================================================
#[tokio::test]
async fn test_re_settle_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();

    let airdrop = 10_000_000_000u64;
    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
        ],
        airdrop,
    )
    .await;

    // Use fee_rate=0 to simplify math (no fees complicate settlement factor)
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

    let maturity_ts = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender deposits 1000 USDC
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;

    let dep_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrower borrows 800 USDC
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        800 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Partial repay: 200 USDC
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        200 * USDC,
    )
    .await;
    let rep_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        200 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Withdraw (lender) -- sets settlement_factor_wad > 0 (underfunded)
    let wdr_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read old settlement factor
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    let old_factor = parsed.settlement_factor_wad;
    assert!(
        old_factor > 0,
        "settlement_factor should be set after withdrawal"
    );

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // Repay 300 more (total repaid = 500, vault now has more tokens).
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        300 * USDC,
    )
    .await;
    let rep_ix2 = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        300 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Compute exact expected factor from on-chain pre-state.
    let md_pre = common::get_account_data(&mut ctx, &market).await;
    let parsed_pre = common::parse_market(&md_pre);
    let vault_balance_pre = common::get_token_balance(&mut ctx, &vault).await;
    let expected_factor = expected_settlement_factor(
        vault_balance_pre,
        parsed_pre.scaled_total_supply,
        parsed_pre.scale_factor,
        parsed_pre.haircut_accumulator,
    );
    assert!(
        expected_factor > old_factor,
        "post-repay expected factor must improve: expected={}, old={}",
        expected_factor,
        old_factor
    );

    // Call re_settle and verify exact factor update.
    let rs_ix = common::build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify exact settlement_factor_wad result.
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    let new_factor = parsed2.settlement_factor_wad;
    assert_eq!(
        new_factor, expected_factor,
        "settlement_factor_wad should match formula: expected={}, got={}",
        expected_factor, new_factor
    );

    // Determinism boundary: a second re_settle without new repayment must fail atomically.
    let snapshot_before_repeat =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let rs_ix = common::build_re_settle(&market, &vault);
    // Add a compute-budget instruction so this transaction is distinct from the
    // first re_settle.  Without it, both transactions can share the same
    // blockhash + instructions + signers, causing the runtime to silently
    // deduplicate and return Ok(()) without re-execution.
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            InstructionError::Custom(31)
        ),
        "Expected SettlementNotImproved error (Custom(31)) on repeated re_settle, got {:?}",
        tx_err
    );
    let snapshot_after_repeat =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before_repeat.assert_unchanged(&snapshot_after_repeat);
}

// ===========================================================================
// 2. test_re_settle_not_settled
//    Deposit but don't withdraw. Market settlement_factor = 0.
//    Call re_settle => Custom(30) NotSettled.
// ===========================================================================
#[tokio::test]
async fn test_re_settle_not_settled() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();

    let airdrop = 10_000_000_000u64;
    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
        ],
        airdrop,
    )
    .await;

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

    let maturity_ts = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC (no borrow, no withdraw)
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;

    let dep_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify settlement_factor_wad is 0 (market not yet settled)
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert_eq!(
        parsed.settlement_factor_wad, 0,
        "settlement_factor_wad should be 0 before any withdrawal"
    );

    // Call re_settle before maturity => Custom(30) NotSettled and no state mutation.
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;
    let rs_ix = common::build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(30)
        ),
        "Expected NotSettled error (Custom(30)), got {:?}",
        tx_err
    );

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let vault_after = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_after, vault_before,
        "vault balance changed on rejected re_settle in not-settled path"
    );

    // Boundary neighbor at maturity+1: still NotSettled until first settlement occurs.
    common::get_blockhash_pinned(&mut ctx, maturity_ts + 1).await;
    let snapshot_before_matured =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let rs_ix = common::build_re_settle(&market, &vault);
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(30)
        ),
        "Expected NotSettled error (Custom(30)) at maturity+1, got {:?}",
        tx_err
    );
    let snapshot_after_matured =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before_matured.assert_unchanged(&snapshot_after_matured);
}

// ===========================================================================
// 3. test_re_settle_not_improved
//    Deposit, borrow, partial repay, advance past maturity, withdraw (sets
//    factor).  Call re_settle WITHOUT additional repayment =>
//    Custom(31) SettlementNotImproved.
//
//    Uses two lenders so scaled_total_supply stays > 0 after the first
//    lender's withdrawal, and annual_interest_bps = 0 so scale_factor
//    stays exactly at WAD, avoiding integer-division rounding that could
//    make the recomputed factor trivially larger.
// ===========================================================================
#[tokio::test]
async fn test_re_settle_not_improved() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();
    let lender2 = Keypair::new();

    let airdrop = 10_000_000_000u64;
    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
            &lender2,
        ],
        airdrop,
    )
    .await;

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

    let maturity_ts = common::FAR_FUTURE_MATURITY;

    // annual_interest_bps = 0 keeps scale_factor at WAD, avoiding rounding artefacts.
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // annual_interest_bps = 0
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender 1 deposits 500 USDC
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;

    let dep_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender 2 deposits 500 USDC (keeps scaled_total_supply > 0 after lender1 withdraws)
    let lender2_token = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender2_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;

    let dep_ix2 = common::build_deposit(
        &market,
        &lender2.pubkey(),
        &lender2_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender2],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow 800 USDC
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        800 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Partial repay: 200 USDC
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        200 * USDC,
    )
    .await;
    let rep_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        200 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Lender 1 withdraws (sets settlement factor); lender2 remains
    let wdr_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Confirm settlement factor was set
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert!(
        parsed.settlement_factor_wad > 0,
        "settlement_factor should be set after withdrawal"
    );

    // Call re_settle WITHOUT any additional repayment => should fail atomically.
    let (vault, _) = common::get_vault_pda(&market);
    let (lender1_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (lender2_pos_pda, _) = common::get_lender_position_pda(&market, &lender2.pubkey());
    let snapshot_before = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market,
        &vault,
        &[lender1_pos_pda, lender2_pos_pda],
    )
    .await;
    let lender1_token_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender2_token_before = common::get_token_balance(&mut ctx, &lender2_token.pubkey()).await;
    let rs_ix = common::build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(31)
        ),
        "Expected SettlementNotImproved error (Custom(31)), got {:?}",
        tx_err
    );

    let snapshot_after = common::ProtocolSnapshot::capture(
        &mut ctx,
        &market,
        &vault,
        &[lender1_pos_pda, lender2_pos_pda],
    )
    .await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender1_token_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender2_token_after = common::get_token_balance(&mut ctx, &lender2_token.pubkey()).await;
    assert_eq!(
        lender1_token_after, lender1_token_before,
        "lender1 token balance changed on rejected re_settle"
    );
    assert_eq!(
        lender2_token_after, lender2_token_before,
        "lender2 token balance changed on rejected re_settle"
    );

    // Boundary neighbor: additional repayment exceeding the haircut accumulator
    // should make the settlement factor improvable.
    // With haircut_acc ≈ 300 USDC (lender1's 500 entitled − 200 payout) and
    // remaining vault ≈ 200 USDC, we need vault > 500 USDC for improvement.
    // Repaying 301 USDC puts vault at ~501 USDC, crossing the threshold.
    let boundary_repay = 301 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        boundary_repay,
    )
    .await;
    let rep_ix2 = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        boundary_repay,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let md_pre = common::get_account_data(&mut ctx, &market).await;
    let parsed_pre = common::parse_market(&md_pre);
    let vault_balance_pre = common::get_token_balance(&mut ctx, &vault).await;
    let expected_factor = expected_settlement_factor(
        vault_balance_pre,
        parsed_pre.scaled_total_supply,
        parsed_pre.scale_factor,
        parsed_pre.haircut_accumulator,
    );
    assert!(
        expected_factor > parsed.settlement_factor_wad,
        "boundary repayment exceeding haircut should improve settlement factor"
    );

    let rs_ix = common::build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let md_post = common::get_account_data(&mut ctx, &market).await;
    let parsed_post = common::parse_market(&md_post);
    assert_eq!(
        parsed_post.settlement_factor_wad, expected_factor,
        "re_settle should set exact improved settlement factor after +1 repayment"
    );
}

// ===========================================================================
// 4. test_re_settle_permissionless
//    Verify a random keypair (not borrower, lender, or admin) can call
//    re_settle successfully after proper setup.
// ===========================================================================
#[tokio::test]
async fn test_re_settle_permissionless() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();

    let airdrop = 10_000_000_000u64;
    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
        ],
        airdrop,
    )
    .await;

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

    let maturity_ts = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;

    let dep_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow 800 USDC
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        800 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Partial repay: 200 USDC
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        200 * USDC,
    )
    .await;
    let rep_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        200 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Withdraw (sets settlement factor)
    let wdr_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Confirm settlement factor was set
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert!(
        parsed.settlement_factor_wad > 0,
        "settlement_factor should be set after withdrawal"
    );

    // Create a completely random keypair (not admin, borrower, lender, or fee_authority).
    let random_caller = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&random_caller], 1_000_000_000).await;

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // Additional repay to increase vault balance.
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        300 * USDC,
    )
    .await;
    let rep_ix2 = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        300 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let md_pre = common::get_account_data(&mut ctx, &market).await;
    let parsed_pre = common::parse_market(&md_pre);
    let vault_balance_pre = common::get_token_balance(&mut ctx, &vault).await;
    let expected_factor = expected_settlement_factor(
        vault_balance_pre,
        parsed_pre.scaled_total_supply,
        parsed_pre.scale_factor,
        parsed_pre.haircut_accumulator,
    );
    assert!(
        expected_factor > parsed.settlement_factor_wad,
        "expected post-repay factor must improve for permissionless re_settle"
    );

    // Random keypair sends re_settle => should succeed (permissionless).
    let rs_ix = common::build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&random_caller.pubkey()),
        &[&random_caller],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("re_settle should succeed when called by a random keypair (permissionless)");

    // Verify exact settlement factor update.
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert_eq!(
        parsed2.settlement_factor_wad, expected_factor,
        "permissionless re_settle must produce exact expected settlement factor"
    );

    // Determinism boundary: repeated permissionless re_settle without new funds must fail atomically.
    let snapshot_before_repeat =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    let rs_ix = common::build_re_settle(&market, &vault);
    // Add a compute-budget instruction so this transaction is distinct from the
    // first re_settle (prevents silent runtime deduplication).
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, rs_ix],
        Some(&random_caller.pubkey()),
        &[&random_caller],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            1,
            InstructionError::Custom(31)
        ),
        "Expected SettlementNotImproved error (Custom(31)) on repeated permissionless re_settle, got {:?}",
        tx_err
    );
    let snapshot_after_repeat =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos_pda]).await;
    snapshot_before_repeat.assert_unchanged(&snapshot_after_repeat);
}

// ===========================================================================
// 5. test_re_settle_after_additional_repayment
//    Underfunded market, settle (via withdraw), repay more, re_settle.
//    Verify new factor > old factor.
//    Verify new factor calculation is correct.
// ===========================================================================
#[tokio::test]
async fn test_re_settle_after_additional_repayment() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();

    let airdrop = 10_000_000_000u64;
    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
        ],
        airdrop,
    )
    .await;

    // fee_rate=0 to simplify settlement factor calculation (no fees reserved)
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

    let maturity_ts = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;

    let dep_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow 800 USDC
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        800 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Partial repay: 200 USDC (vault now has 200 + 200 = 400 USDC)
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        200 * USDC,
    )
    .await;
    let rep_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        200 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Withdraw (sets settlement factor for underfunded market)
    let wdr_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read the old settlement factor and market state
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    let old_factor = parsed.settlement_factor_wad;
    assert!(old_factor > 0, "old factor should be > 0");
    // Since underfunded, factor should be less than WAD
    assert!(
        old_factor < WAD,
        "underfunded market should have factor < WAD, got {}",
        old_factor
    );

    // Repay 300 more to increase vault balance
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        300 * USDC,
    )
    .await;
    let rep_ix2 = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        300 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rep_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read vault balance and market state before re_settle for calculation verification
    let (vault, _) = common::get_vault_pda(&market);
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    let md_pre = common::get_account_data(&mut ctx, &market).await;
    let parsed_pre = common::parse_market(&md_pre);
    let scaled_total = parsed_pre.scaled_total_supply;
    let scale_factor = parsed_pre.scale_factor;

    // Call re_settle
    let rs_ix = common::build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read new factor
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    let new_factor = parsed2.settlement_factor_wad;

    // Verify new factor > old factor
    assert!(
        new_factor > old_factor,
        "new factor ({}) should be greater than old factor ({})",
        new_factor,
        old_factor
    );

    // Verify new factor via the shared helper (COAL-C01 + COAL-H01 formula)
    let expected_factor = expected_settlement_factor(
        vault_balance,
        scaled_total,
        scale_factor,
        parsed_pre.haircut_accumulator,
    );

    assert_eq!(
        new_factor, expected_factor,
        "new factor should match expected calculation: new_factor={}, expected={}",
        new_factor, expected_factor
    );
}
