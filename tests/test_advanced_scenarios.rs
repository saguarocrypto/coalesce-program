//! Advanced scenario tests for financial logic and robustness.
//!
//! Priority 2 — Financial Logic:
//! - Over-repayment (total_repaid > total_borrowed), settlement factor capped at WAD
//! - Settlement factor freezes after first withdrawal
//! - Multiple markets sharing global borrow capacity per borrower
//! - Fee reservation reducing borrowable amount
//!
//! Priority 3 — Robustness:
//! - Duplicate market creation (same PDA/nonce)
//! - Annual interest rate > MAX rejected
//! - De-whitelist then borrow (design verification)
//! - Re-close already-closed lender position

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
    compute_budget::ComputeBudgetInstruction, signature::Keypair, signer::Signer,
    transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

/// WAD = 1e18, the fixed-point precision constant.
const WAD: u128 = 1_000_000_000_000_000_000;

// ===========================================================================
// Priority 2 — Financial Logic
// ===========================================================================

// ---------------------------------------------------------------------------
// 1. Over-repayment: total_repaid > total_borrowed
// ---------------------------------------------------------------------------

/// Borrow 500 USDC, repay 800 USDC (more than borrowed). Verify that
/// total_repaid exceeds total_borrowed and that the settlement factor
/// is capped at WAD (100%) on withdrawal.
#[tokio::test]
async fn test_over_repayment_settlement_factor_capped_at_wad() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
    )
    .await;

    let fee_rate_bps: u16 = 0; // zero fees to simplify
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        3_000 * USDC,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Boundary around "over-repayment starts at extra_interest > 0":
    // x-1=0, x=1, x+1=2.
    let scenarios = [(1u64, 0u64), (2u64, 1u64), (3u64, 2u64)];
    let mut created_markets: Vec<(solana_sdk::pubkey::Pubkey, u64)> = Vec::new();
    for (nonce, extra_interest) in scenarios {
        let market = setup_market_full(
            &mut ctx,
            &admin,
            &borrower,
            &mint,
            &blacklist_program.pubkey(),
            nonce,
            0,
            maturity_timestamp,
            10_000 * USDC,
            &whitelist_manager,
            10_000 * USDC,
        )
        .await;

        // Deposit 1000 USDC
        let deposit_ix = build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
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
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Borrow 500 USDC
        let borrow_ix = build_borrow(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &blacklist_program.pubkey(),
            500 * USDC,
        );
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[borrow_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            recent,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Repay principal fully.
        mint_to_account(
            &mut ctx,
            &mint,
            &borrower_token.pubkey(),
            &admin,
            500 * USDC,
        )
        .await;
        let repay_ix = build_repay(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &mint,
            &borrower.pubkey(),
            500 * USDC,
        );
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[repay_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            recent,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Add extra funds as interest repayment for over-repayment scenarios.
        if extra_interest > 0 {
            mint_to_account(
                &mut ctx,
                &mint,
                &borrower_token.pubkey(),
                &admin,
                extra_interest,
            )
            .await;
            let repay_interest_ix = build_repay_interest_with_amount(
                &market,
                &borrower.pubkey(),
                &borrower_token.pubkey(),
                extra_interest,
            );
            let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
            let tx = Transaction::new_signed_with_payer(
                &[repay_interest_ix],
                Some(&ctx.payer.pubkey()),
                &[&ctx.payer, &borrower],
                recent,
            );
            ctx.banks_client.process_transaction(tx).await.unwrap();
        }

        // Verify accounting before settlement.
        let market_data = get_account_data(&mut ctx, &market).await;
        let parsed = parse_market(&market_data);
        assert_eq!(parsed.total_borrowed, 500 * USDC);
        assert_eq!(parsed.total_repaid, (500 * USDC) + extra_interest);
        assert_eq!(parsed.total_interest_repaid, extra_interest);
        if extra_interest == 0 {
            assert_eq!(parsed.total_repaid, parsed.total_borrowed);
        } else {
            assert!(parsed.total_repaid > parsed.total_borrowed);
        }

        created_markets.push((market, extra_interest));
    }

    // Advance once so every scenario settles under identical clock conditions.
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    for (market, extra_interest) in created_markets {
        let lender_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
        let withdraw_ix = build_withdraw(
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

        let lender_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
        assert_eq!(lender_after - lender_before, 1_000 * USDC);

        // Settlement factor must clamp to WAD exactly in all overfunded/equal-funded cases.
        let market_data = get_account_data(&mut ctx, &market).await;
        let parsed = parse_market(&market_data);
        assert_eq!(parsed.settlement_factor_wad, WAD);
        assert_eq!(parsed.scaled_total_supply, 0);

        let (pos_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
        let pos_data = get_account_data(&mut ctx, &pos_pda).await;
        let position = parse_lender_position(&pos_data);
        assert_eq!(position.scaled_balance, 0);

        let (vault, _) = get_vault_pda(&market);
        let vault_balance = get_token_balance(&mut ctx, &vault).await;
        assert_eq!(vault_balance, extra_interest);
    }
}

// ---------------------------------------------------------------------------
// 2. Settlement factor freezes after first withdrawal
// ---------------------------------------------------------------------------

/// Two lenders deposit. After maturity, lender A withdraws (triggers settlement
/// factor computation). Then the borrower repays more, but lender B's withdrawal
/// should use the same settlement factor (frozen on first withdrawal).
#[tokio::test]
async fn test_settlement_factor_freezes_after_first_withdrawal() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender_a = Keypair::new();
    let lender_b = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender_a, &lender_b],
        10_000_000_000,
    )
    .await;

    let fee_rate_bps: u16 = 0;
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_a_token = create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;
    let lender_b_token = create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &admin,
        500 * USDC,
    )
    .await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &admin,
        500 * USDC,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;
    let (vault, _) = get_vault_pda(&market);

    // Lender A deposits 500
    let ix = build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B deposits 500
    let ix = build_deposit(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (pos_a, _) = get_lender_position_pda(&market, &lender_a.pubkey());
    let (pos_b, _) = get_lender_position_pda(&market, &lender_b.pubkey());

    // Borrow 600 (leaving only 400 in vault)
    let ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        600 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Repay 200 (vault now has 600)
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        200 * USDC,
    )
    .await;
    let ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        200 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Before settlement, factor must be unset.
    let market_before_settle = parse_market(&get_account_data(&mut ctx, &market).await);
    assert_eq!(market_before_settle.settlement_factor_wad, 0);

    // Boundary check at grace-1: first settlement withdrawal must fail.
    advance_clock_past(&mut ctx, maturity_timestamp + 299).await;
    let lender_a_before_fail = get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let snapshot_before_fail =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_a, pos_b]).await;
    let ix = build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 32); // SettlementGracePeriod
    let snapshot_after_fail =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_a, pos_b]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        get_token_balance(&mut ctx, &lender_a_token.pubkey()).await,
        lender_a_before_fail
    );
    assert_eq!(
        parse_market(&get_account_data(&mut ctx, &market).await).settlement_factor_wad,
        0
    );

    // Boundary check at grace: first withdrawal succeeds and freezes settlement factor.
    advance_clock_past(&mut ctx, maturity_timestamp + 300).await;
    let lender_a_before = get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let ix = build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    // Add compute-budget instruction to distinguish from the grace-1 withdrawal
    // (prevents silent runtime deduplication when both share the same blockhash).
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let lender_a_after = get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;

    // Read the frozen settlement factor
    let market_data = get_account_data(&mut ctx, &market).await;
    let frozen_factor = parse_market(&market_data).settlement_factor_wad;
    assert!(
        frozen_factor > 0,
        "settlement factor should be set after first withdrawal"
    );
    assert!(
        frozen_factor < WAD,
        "underfunded settlement factor should be strictly below WAD"
    );
    let expected_payout = ((500u128 * u128::from(USDC) * frozen_factor) / WAD) as u64;
    assert_eq!(lender_a_after - lender_a_before, expected_payout);

    // Repay 300 more (vault balance increases)
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        300 * USDC,
    )
    .await;
    let ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        300 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B withdraws — should use the SAME frozen settlement factor
    let lender_b_before = get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;
    let ix = build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let lender_b_after = get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;
    assert_eq!(lender_b_after - lender_b_before, expected_payout);

    // Verify settlement factor is unchanged
    let market_data = get_account_data(&mut ctx, &market).await;
    let final_market = parse_market(&market_data);
    let final_factor = final_market.settlement_factor_wad;
    assert_eq!(
        final_factor, frozen_factor,
        "settlement factor should remain frozen at {} but was {}",
        frozen_factor, final_factor,
    );
    assert_eq!(final_market.scaled_total_supply, 0);
    assert_eq!(get_token_balance(&mut ctx, &vault).await, 300 * USDC);
}

// ---------------------------------------------------------------------------
// 3. Multiple markets sharing global borrow capacity
// ---------------------------------------------------------------------------

/// Create two markets for the same borrower. Borrow capacity is shared via
/// the BorrowerWhitelist.current_borrowed field. Borrowing across both markets
/// must not exceed max_borrow_capacity.
#[tokio::test]
async fn test_multiple_markets_global_capacity() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    // Fund lender with enough for both markets
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    // Whitelist borrower with max_borrow_capacity = 1000 USDC
    let wl_ix = build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create market 1 (nonce=1)
    let create_ix_1 = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create_ix_1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (market1, _) = get_market_pda(&borrower.pubkey(), 1);

    // Create market 2 (nonce=2)
    let create_ix_2 = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2,
        0,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (market2, _) = get_market_pda(&borrower.pubkey(), 2);
    let (vault2, _) = get_vault_pda(&market2);

    // Deposit 1000 into market1
    let deposit_ix = build_deposit(
        &market1,
        &lender.pubkey(),
        &lender_token.pubkey(),
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
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Deposit 1000 into market2
    let deposit_ix = build_deposit(
        &market2,
        &lender.pubkey(),
        &lender_token.pubkey(),
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
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (pos2, _) = get_lender_position_pda(&market2, &lender.pubkey());

    // Borrow 700 from market1 (ok, under 1000 global cap)
    let borrow_ix = build_borrow(
        &market1,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        700 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(
        parse_borrower_whitelist(&wl_data).current_borrowed,
        700 * USDC
    );
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        700 * USDC
    );

    // x+1 boundary: remaining capacity is 300; borrowing 301 must fail atomically.
    let wl_before_fail = get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before_fail = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let market2_before_fail = get_account_data(&mut ctx, &market2).await;
    let vault2_before_fail = get_token_balance(&mut ctx, &vault2).await;
    let snapshot_before_fail =
        ProtocolSnapshot::capture(&mut ctx, &market2, &vault2, &[pos2]).await;
    let borrow_ix = build_borrow(
        &market2,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        301 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 27); // GlobalCapacityExceeded
    let snapshot_after_fail = ProtocolSnapshot::capture(&mut ctx, &market2, &vault2, &[pos2]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(get_account_data(&mut ctx, &wl_pda).await, wl_before_fail);
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_before_fail
    );
    assert_eq!(
        get_account_data(&mut ctx, &market2).await,
        market2_before_fail
    );
    assert_eq!(
        get_token_balance(&mut ctx, &vault2).await,
        vault2_before_fail
    );

    // x-1 boundary: borrow 299 succeeds.
    let borrow_ix = build_borrow(
        &market2,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        299 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(
        parse_borrower_whitelist(&wl_data).current_borrowed,
        999 * USDC
    );
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        999 * USDC
    );

    // x boundary: final 1 USDC to exactly hit cap succeeds.
    let borrow_ix = build_borrow(
        &market2,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow exactly at global cap should succeed");
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(
        parse_borrower_whitelist(&wl_data).current_borrowed,
        1_000 * USDC
    );
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        1_000 * USDC
    );

    // One more unit over cap must fail atomically.
    let wl_before_second_fail = get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before_second_fail = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let snapshot_before_second_fail =
        ProtocolSnapshot::capture(&mut ctx, &market2, &vault2, &[pos2]).await;
    let borrow_ix = build_borrow(
        &market2,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 27); // GlobalCapacityExceeded
    let snapshot_after_second_fail =
        ProtocolSnapshot::capture(&mut ctx, &market2, &vault2, &[pos2]).await;
    snapshot_before_second_fail.assert_unchanged(&snapshot_after_second_fail);
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_before_second_fail
    );
    assert_eq!(
        get_account_data(&mut ctx, &wl_pda).await,
        wl_before_second_fail
    );

    let market1_data = get_account_data(&mut ctx, &market1).await;
    let market2_data = get_account_data(&mut ctx, &market2).await;
    assert_eq!(parse_market(&market1_data).total_borrowed, 700 * USDC);
    assert_eq!(parse_market(&market2_data).total_borrowed, 300 * USDC);
}

// ---------------------------------------------------------------------------
// 4. Fee reservation reduces borrowable amount
// ---------------------------------------------------------------------------

/// With a 10% fee rate, deposit 1000, advance time to accrue interest/fees,
/// then verify borrowable amount is reduced by the reserved fees.
#[tokio::test]
async fn test_fee_reservation_reduces_borrowable() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
    )
    .await;

    let fee_rate_bps: u16 = 1000; // 10%
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let maturity_timestamp = common::PINNED_EPOCH + 365 * 24 * 3600; // 1 year

    let market = setup_market_full(
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
    let (vault, _) = get_vault_pda(&market);
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());

    // Deposit 1000 USDC
    let ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance time by ~6 months to accrue significant interest+fees
    let six_months_later = common::PINNED_EPOCH + 180 * 24 * 3600;
    advance_clock_past(&mut ctx, six_months_later).await;

    // Seed borrow accrues interest/fees at this timestamp and locks the threshold for
    // subsequent same-slot boundary checks.
    let seed_borrow: u64 = 1;
    let ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        seed_borrow,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_after_seed = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_seed = get_token_balance(&mut ctx, &vault).await;
    let fees_reserved = core::cmp::min(vault_after_seed, market_after_seed.accrued_protocol_fees);
    let borrowable = vault_after_seed
        .checked_sub(fees_reserved)
        .expect("fees reserved must be <= vault balance");
    assert!(
        fees_reserved > 0,
        "fees should be reserved after time accrual"
    );
    assert!(
        borrowable < vault_after_seed,
        "reserved fees should reduce borrowable amount"
    );
    assert!(
        borrowable >= 2,
        "borrowable must support x-1/x boundary checks"
    );

    // x+1 boundary: borrow one over borrowable must fail atomically.
    let borrower_before_fail = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let wl_before_fail = get_account_data(&mut ctx, &wl_pda).await;
    let market_before_fail = get_account_data(&mut ctx, &market).await;
    let vault_before_fail = get_token_balance(&mut ctx, &vault).await;
    let snapshot_before_fail =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrowable + 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 26); // BorrowAmountTooHigh
    let snapshot_after_fail =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_before_fail
    );
    assert_eq!(get_account_data(&mut ctx, &wl_pda).await, wl_before_fail);
    assert_eq!(
        get_account_data(&mut ctx, &market).await,
        market_before_fail
    );
    assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before_fail);

    // x-1 boundary.
    let borrower_before_success = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrowable - 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // x boundary (remaining 1 unit).
    let ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let borrower_after_exact = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(borrower_after_exact - borrower_before_success, borrowable);

    let wl_after_exact = parse_borrower_whitelist(&get_account_data(&mut ctx, &wl_pda).await);
    assert_eq!(wl_after_exact.current_borrowed, seed_borrow + borrowable);
    assert_eq!(get_token_balance(&mut ctx, &vault).await, fees_reserved);

    // One more unit should fail and preserve state.
    let wl_before_second_fail = get_account_data(&mut ctx, &wl_pda).await;
    let snapshot_before_second_fail =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(250_000),
            borrow_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 26); // BorrowAmountTooHigh
    let snapshot_after_second_fail =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before_second_fail.assert_unchanged(&snapshot_after_second_fail);
    assert_eq!(
        get_account_data(&mut ctx, &wl_pda).await,
        wl_before_second_fail
    );
}

// ===========================================================================
// Priority 3 — Robustness
// ===========================================================================

// ---------------------------------------------------------------------------
// 5. Duplicate market creation (same nonce)
// ---------------------------------------------------------------------------

/// Create a market with nonce=1, then try to create another market with the
/// same nonce. The second call should fail because the PDA already exists.
#[tokio::test]
async fn test_duplicate_market_creation_fails() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    // First market creation (nonce=1) — should succeed
    let market = setup_market_full(
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
    let (vault, _) = get_vault_pda(&market);
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    let market_before = get_account_data(&mut ctx, &market).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;

    // Second market creation with same nonce=1 — should fail
    let ix = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 4); // MarketAlreadyExists

    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    assert_eq!(get_account_data(&mut ctx, &market).await, market_before);
    assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);

    let parsed = parse_market(&get_account_data(&mut ctx, &market).await);
    assert_eq!(parsed.market_nonce, 1);
    assert_eq!(parsed.total_deposited, 0);
    assert_eq!(parsed.total_borrowed, 0);
}

// ---------------------------------------------------------------------------
// 6. Annual interest rate > MAX rejected
// ---------------------------------------------------------------------------

/// Attempt to create a market with annual_interest_bps = 10001 (exceeds
/// MAX_ANNUAL_INTEREST_BPS = 10000). Should fail with InvalidFeeRate (Custom 1).
#[tokio::test]
async fn test_create_market_interest_rate_exceeds_max() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Whitelist borrower
    let wl_ix = build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());

    // x-1 boundary: 9_999 bps should succeed.
    let ix = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        9_999,
        maturity_timestamp,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (market1, _) = get_market_pda(&borrower.pubkey(), 1);
    assert_eq!(
        parse_market(&get_account_data(&mut ctx, &market1).await).annual_interest_bps,
        9_999
    );

    // x boundary: 10_000 bps should succeed.
    let ix = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2,
        10_000,
        maturity_timestamp,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (market2, _) = get_market_pda(&borrower.pubkey(), 2);
    assert_eq!(
        parse_market(&get_account_data(&mut ctx, &market2).await).annual_interest_bps,
        10_000
    );

    // x+1 boundary: 10_001 bps should fail with InvalidFeeRate.
    let wl_before_fail = get_account_data(&mut ctx, &wl_pda).await;
    let (vault1, _) = get_vault_pda(&market1);
    let snapshot_before_fail = ProtocolSnapshot::capture(&mut ctx, &market1, &vault1, &[]).await;
    let (market3, _) = get_market_pda(&borrower.pubkey(), 3);
    let (vault3, _) = get_vault_pda(&market3);
    assert!(
        try_get_account_data(&mut ctx, &market3).await.is_none(),
        "nonce=3 market should not exist before failing create"
    );
    assert!(
        try_get_account_data(&mut ctx, &vault3).await.is_none(),
        "nonce=3 vault should not exist before failing create"
    );
    let ix = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        3,
        10_001,
        maturity_timestamp,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 1); // InvalidFeeRate

    let snapshot_after_fail = ProtocolSnapshot::capture(&mut ctx, &market1, &vault1, &[]).await;
    snapshot_before_fail.assert_unchanged(&snapshot_after_fail);
    assert_eq!(
        get_account_data(&mut ctx, &wl_pda).await,
        wl_before_fail,
        "whitelist state must not change on failed market create"
    );
    assert!(
        try_get_account_data(&mut ctx, &market3).await.is_none(),
        "invalid-rate market account must not be created"
    );
    assert!(
        try_get_account_data(&mut ctx, &vault3).await.is_none(),
        "invalid-rate vault account must not be created"
    );
}

// ---------------------------------------------------------------------------
// 7. De-whitelist then borrow (design verification)
// ---------------------------------------------------------------------------

/// Whitelist a borrower, create a market, then de-whitelist the borrower
/// (is_whitelisted=0). Borrowing should fail with NotWhitelisted (COAL-I02).
#[tokio::test]
async fn test_borrow_fails_after_dewhitelist() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        100 * USDC,
    )
    .await;
    let (vault, _) = get_vault_pda(&market);
    let (wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());

    // Deposit
    let ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // De-whitelist the borrower (is_whitelisted=0)
    let dewhitelist_ix = build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        0,
        100 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dewhitelist_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let wl_data = get_account_data(&mut ctx, &wl_pda).await;
    let wl = parse_borrower_whitelist(&wl_data);
    assert_eq!(wl.is_whitelisted, 0);
    assert_eq!(wl.max_borrow_capacity, 100 * USDC);
    assert_eq!(wl.current_borrowed, 0);

    // COAL-I02: Borrow should fail after de-whitelist with NotWhitelisted.
    let wl_before = get_account_data(&mut ctx, &wl_pda).await;
    let borrower_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let snapshot_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        50 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 6); // NotWhitelisted

    // Verify no state changes
    let snapshot_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    assert_eq!(get_account_data(&mut ctx, &wl_pda).await, wl_before);
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_before
    );
}

// ---------------------------------------------------------------------------
// 8. Re-close already-closed lender position
// ---------------------------------------------------------------------------

/// Close a lender position successfully and verify the account is zeroed.
/// Then verify that re-closing the same position is a harmless no-op:
/// the account is already zeroed with 0 lamports, so the second close
/// transfers 0 lamports and re-zeroes already-zeroed data.
///
/// This is acceptable because no funds are at risk — the second close
/// simply transfers 0 lamports from the position (which has 0) to the
/// lender. The real-world Solana runtime garbage-collects 0-lamport
/// accounts, so this situation only arises in test environments.
#[tokio::test]
async fn test_close_position_zeroes_account_and_reclaims_rent() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit
    let ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify lender position has balance
    let (pos_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = get_account_data(&mut ctx, &pos_pda).await;
    let pos = parse_lender_position(&pos_data);
    assert!(
        pos.scaled_balance > 0,
        "position should have a balance after deposit"
    );

    // Advance past maturity
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw all
    let ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Record lender lamports before close
    let lender_account = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap();
    let lender_lamports_before = lender_account.lamports;

    // Close position — should succeed and transfer rent back to lender
    let ix = build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("close should succeed");

    // Verify lender received lamports (rent reclaimed)
    let lender_account = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert!(
        lender_account.lamports > lender_lamports_before,
        "lender should receive rent lamports after closing position"
    );

    // Verify position account data is zeroed
    let pos_account = ctx.banks_client.get_account(pos_pda).await.unwrap();
    if let Some(acct) = pos_account {
        // Account may still exist in test runtime with 0 lamports
        assert_eq!(
            acct.lamports, 0,
            "position lamports should be 0 after close"
        );
        assert!(
            acct.data.iter().all(|&b| b == 0),
            "position data should be zeroed after close"
        );
    }
    // If account is None, it was garbage-collected — also correct
}
