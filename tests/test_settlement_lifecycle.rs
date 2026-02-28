//! Settlement lifecycle tests for the CoalesceFi protocol.
//!
//! Comprehensive end-to-end tests covering the full market lifecycle from
//! creation through settlement, including partial repayment, re-settlement,
//! multiple lenders, and maturity boundary conditions.

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
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};

const MATURITY_OFFSET: i64 = 365 * 24 * 60 * 60;
const WAD: u128 = 1_000_000_000_000_000_000;

struct Setup {
    admin: Keypair,
    borrower: Keypair,
    lender: Keypair,
    whitelist_manager: Keypair,
    fee_authority: Keypair,
    blacklist_program: Pubkey,
    mint: Pubkey,
    mint_authority: Keypair,
    market: Pubkey,
    lender_ta: Pubkey,
    borrower_ta: Pubkey,
    fee_ta: Pubkey,
}

async fn setup(ctx: &mut solana_program_test::ProgramTestContext) -> Setup {
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(ctx, &[&admin, &borrower, &lender, &wm, &fa], 10_000_000_000).await;
    let mint = create_mint(ctx, &ma, 6).await;
    let lta = create_token_account(ctx, &mint, &lender.pubkey()).await;
    let bta = create_token_account(ctx, &mint, &borrower.pubkey()).await;
    let fta = create_token_account(ctx, &mint, &fa.pubkey()).await;

    mint_to_account(ctx, &mint, &lta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(ctx, &mint, &bta.pubkey(), &ma, 10_000_000_000).await;

    setup_protocol(ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(ctx, &bp, &lender.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + MATURITY_OFFSET;

    let market = setup_market_full(
        ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        1000,
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    Setup {
        admin,
        borrower,
        lender,
        whitelist_manager: wm,
        fee_authority: fa,
        blacklist_program: bp,
        mint,
        mint_authority: ma,
        market,
        lender_ta: lta.pubkey(),
        borrower_ta: bta.pubkey(),
        fee_ta: fta.pubkey(),
    }
}

// ===========================================================================
// Test 1: Full lifecycle — creation to excess withdrawal
// ===========================================================================
#[tokio::test]
async fn test_full_lifecycle_creation_to_excess_withdrawal() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // 1. Deposit
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    // 2. Borrow
    let borrow = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, borrow, &[&s.borrower]).await;

    // 3. Repay full principal
    let repay = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        50_000_000,
    );
    send_ok(&mut ctx, repay, &[&s.borrower]).await;

    // 3b. Use repay_interest to cover accrued interest + fees so vault is fully solvent
    let ri = build_repay_interest_with_amount(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        50_000_000,
    );
    send_ok(&mut ctx, ri, &[&s.borrower]).await;

    // 4. Advance past maturity
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // 5. Withdraw (triggers settlement — factor should be WAD)
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    send_ok(&mut ctx, withdraw, &[&s.lender]).await;

    // 6. Collect fees (works because settlement_factor == WAD)
    let cf = build_collect_fees(&s.market, &s.fee_authority.pubkey(), &s.fee_ta);
    send_ok(&mut ctx, cf, &[&s.fee_authority]).await;

    // 7. Withdraw excess — must succeed because the borrower overpaid via repay_interest
    // (50M extra tokens in vault beyond what lender + fees consumed)
    let we = build_withdraw_excess(&s.market, &s.borrower.pubkey(), &s.borrower_ta);
    send_ok(&mut ctx, we, &[&s.borrower]).await;
}

// ===========================================================================
// Test 2: Partial repayment — settlement_factor < WAD
// ===========================================================================
#[tokio::test]
async fn test_partial_repayment_settlement_factor_below_wad() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit 100, borrow 80, repay 40 (50%)
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    let borrow = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        80_000_000,
    );
    send_ok(&mut ctx, borrow, &[&s.borrower]).await;

    // Only repay 40 out of 80 borrowed
    let repay = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        40_000_000,
    );
    send_ok(&mut ctx, repay, &[&s.borrower]).await;

    // Advance past maturity
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Withdraw triggers settlement
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);

    let withdraw = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    send_ok(&mut ctx, withdraw, &[&s.lender]).await;

    // Read settlement factor — should be less than WAD
    let mdata_after = get_account_data(&mut ctx, &s.market).await;
    let parsed_after = parse_market(&mdata_after);
    assert!(
        parsed_after.settlement_factor_wad > 0,
        "settlement should have been triggered"
    );
    assert!(
        parsed_after.settlement_factor_wad < WAD,
        "settlement factor should be < WAD with partial repayment"
    );
}

// ===========================================================================
// Test 3: Re-settle after additional repayment improves factor
// ===========================================================================
#[tokio::test]
async fn test_re_settle_after_additional_repayment() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit 100, borrow 80, repay 40
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    let borrow = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        80_000_000,
    );
    send_ok(&mut ctx, borrow, &[&s.borrower]).await;

    let repay = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        40_000_000,
    );
    send_ok(&mut ctx, repay, &[&s.borrower]).await;

    // Advance past maturity
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Trigger initial settlement via withdraw (use meaningful amount to avoid ZeroPayout)
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance / 10,
        0,
    );
    send_ok(&mut ctx, withdraw, &[&s.lender]).await;

    let mdata_1 = get_account_data(&mut ctx, &s.market).await;
    let sf_1 = parse_market(&mdata_1).settlement_factor_wad;
    assert!(sf_1 > 0 && sf_1 < WAD);

    // Borrower repays more
    let repay2 = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        20_000_000,
    );
    send_ok(&mut ctx, repay2, &[&s.borrower]).await;

    // Advance further past grace period for re_settle
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 602).await;

    // Re-settle
    let (vault, _) = get_vault_pda(&s.market);
    let rs = build_re_settle(&s.market, &vault);
    send_ok(&mut ctx, rs, &[]).await;

    let mdata_2 = get_account_data(&mut ctx, &s.market).await;
    let sf_2 = parse_market(&mdata_2).settlement_factor_wad;
    assert!(
        sf_2 > sf_1,
        "re_settle should improve factor: {sf_2} > {sf_1}"
    );
}

// ===========================================================================
// Test 4: Re-settle monotonically increases
// ===========================================================================
#[tokio::test]
async fn test_re_settle_monotonically_increases() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit 100, borrow 90, repay 30
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    let borrow = build_borrow(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.blacklist_program,
        90_000_000,
    );
    send_ok(&mut ctx, borrow, &[&s.borrower]).await;

    let repay = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        30_000_000,
    );
    send_ok(&mut ctx, repay, &[&s.borrower]).await;

    // Advance past maturity
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Trigger settlement (use meaningful amount to avoid ZeroPayout)
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance / 10,
        0,
    );
    send_ok(&mut ctx, withdraw, &[&s.lender]).await;

    let mdata_1 = get_account_data(&mut ctx, &s.market).await;
    let sf_1 = parse_market(&mdata_1).settlement_factor_wad;

    // Successive repayments and re-settles
    let mut prev_sf = sf_1;
    for repay_amount in [15_000_000u64, 15_000_000, 15_000_000] {
        let repay = build_repay(
            &s.market,
            &s.borrower.pubkey(),
            &s.borrower_ta,
            &s.mint,
            &s.borrower.pubkey(),
            repay_amount,
        );
        send_ok(&mut ctx, repay, &[&s.borrower]).await;

        // Advance further past grace period for re_settle
        advance_clock_past(&mut ctx, parsed.maturity_timestamp + 602).await;

        let (vault, _) = get_vault_pda(&s.market);
        let rs = build_re_settle(&s.market, &vault);
        send_ok(&mut ctx, rs, &[]).await;

        let mdata = get_account_data(&mut ctx, &s.market).await;
        let sf = parse_market(&mdata).settlement_factor_wad;
        assert!(
            sf > prev_sf,
            "sf should strictly increase after additional repayment: {sf} > {prev_sf}"
        );
        prev_sf = sf;
    }
}

// ===========================================================================
// Test 5: Multiple lenders, same deposit time, different amounts
// ===========================================================================
#[tokio::test]
async fn test_multiple_lenders_same_time_different_amounts() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender1 = Keypair::new();
    let lender2 = Keypair::new();
    let lender3 = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender1, &lender2, &lender3, &wm, &fa],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &ma, 6).await;
    let l1_ta = create_token_account(&mut ctx, &mint, &lender1.pubkey()).await;
    let l2_ta = create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    let l3_ta = create_token_account(&mut ctx, &mint, &lender3.pubkey()).await;
    let b_ta = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let f_ta = create_token_account(&mut ctx, &mint, &fa.pubkey()).await;

    mint_to_account(&mut ctx, &mint, &l1_ta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(&mut ctx, &mint, &l2_ta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(&mut ctx, &mint, &l3_ta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(&mut ctx, &mint, &b_ta.pubkey(), &ma, 10_000_000_000).await;

    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender1.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender2.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender3.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + MATURITY_OFFSET;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        1000,
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    // Three lenders deposit 100, 200, 300 USDC
    let dep1 = build_deposit(
        &market,
        &lender1.pubkey(),
        &l1_ta.pubkey(),
        &mint,
        &bp,
        100_000_000,
    );
    send_ok(&mut ctx, dep1, &[&lender1]).await;

    let dep2 = build_deposit(
        &market,
        &lender2.pubkey(),
        &l2_ta.pubkey(),
        &mint,
        &bp,
        200_000_000,
    );
    send_ok(&mut ctx, dep2, &[&lender2]).await;

    let dep3 = build_deposit(
        &market,
        &lender3.pubkey(),
        &l3_ta.pubkey(),
        &mint,
        &bp,
        300_000_000,
    );
    send_ok(&mut ctx, dep3, &[&lender3]).await;

    // Read scaled balances
    let p1_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&market, &lender1.pubkey()).0,
    )
    .await;
    let p2_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&market, &lender2.pubkey()).0,
    )
    .await;
    let p3_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&market, &lender3.pubkey()).0,
    )
    .await;

    let sb1 = parse_lender_position(&p1_data).scaled_balance;
    let sb2 = parse_lender_position(&p2_data).scaled_balance;
    let sb3 = parse_lender_position(&p3_data).scaled_balance;

    // Verify proportional: sb2 ≈ 2*sb1, sb3 ≈ 3*sb1
    // Allow some tolerance for rounding
    assert!(
        sb2 >= 2 * sb1 - 10 && sb2 <= 2 * sb1 + 10,
        "lender2 balance should be ~2x lender1: {sb2} vs 2*{sb1}"
    );
    assert!(
        sb3 >= 3 * sb1 - 10 && sb3 <= 3 * sb1 + 10,
        "lender3 balance should be ~3x lender1: {sb3} vs 3*{sb1}"
    );
}

// ===========================================================================
// Test 6: Full settlement — all lenders withdraw
// Interest accrues even without borrowing, making normalized_total > vault.
// Use repay_interest to bring vault to solvency before withdrawal.
// ===========================================================================
#[tokio::test]
async fn test_full_settlement_all_lenders_withdraw() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    // Deposit and let it mature without borrowing
    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    // Use repay_interest to cover accrued interest (~10% over 1 year ≈ 10M)
    // Add a generous buffer (50M) to ensure full solvency including fees
    let ri = build_repay_interest_with_amount(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        50_000_000,
    );
    send_ok(&mut ctx, ri, &[&s.borrower]).await;

    // Advance past maturity
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Withdraw all
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    send_ok(&mut ctx, withdraw, &[&s.lender]).await;

    // Verify settlement factor = WAD (full settlement, vault was made solvent)
    let mdata_after = get_account_data(&mut ctx, &s.market).await;
    let parsed_after = parse_market(&mdata_after);
    assert_eq!(parsed_after.settlement_factor_wad, WAD);

    // Close lender position
    let close = build_close_lender_position(&s.market, &s.lender.pubkey());
    send_ok(&mut ctx, close, &[&s.lender]).await;

    // Verify lender position account is closed
    let pos_account = ctx
        .banks_client
        .get_account(get_lender_position_pda(&s.market, &s.lender.pubkey()).0)
        .await
        .unwrap();
    assert!(pos_account.is_none(), "lender position should be closed");
}

// ===========================================================================
// Test 7: Exact maturity boundary operations
// ===========================================================================
#[tokio::test]
async fn test_exact_maturity_boundary_operations() {
    let mut ctx = common::start_context().await;
    let s = setup(&mut ctx).await;

    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    let mdata = get_account_data(&mut ctx, &s.market).await;
    let maturity = parse_market(&mdata).maturity_timestamp;

    // At maturity - 1: deposit should still work
    advance_clock_past(&mut ctx, maturity - 1).await;
    let dep2 = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    send_ok(&mut ctx, dep2, &[&s.lender]).await;

    // At maturity: deposit should fail with MarketMatured (28)
    get_blockhash_pinned(&mut ctx, maturity).await;
    let dep3 = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        1_000_000,
    );
    // Program uses `current_ts >= maturity` (SR-031), so deposit at exact maturity must fail
    let tx = Transaction::new_signed_with_payer(
        &[dep3],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &s.lender],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("deposit at exact maturity must fail (SR-031)");
    let code = extract_custom_error(&err).unwrap();
    assert_eq!(code, 28, "expected MarketMatured(28), got {code}");

    // At maturity + 301: withdraw should work (after grace period)
    advance_clock_past(&mut ctx, maturity + 301).await;
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&s.market, &s.lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw = build_withdraw(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.blacklist_program,
        pos.scaled_balance,
        0,
    );
    send_ok(&mut ctx, withdraw, &[&s.lender]).await;
}

// ===========================================================================
// Test 8: Multiple lenders deposit at different times
// ===========================================================================
#[tokio::test]
async fn test_multiple_lenders_different_deposit_times() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender_a = Keypair::new();
    let lender_b = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender_a, &lender_b, &wm, &fa],
        10_000_000_000,
    )
    .await;

    let mint = create_mint(&mut ctx, &ma, 6).await;
    let la_ta = create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;
    let lb_ta = create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;
    let b_ta = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let f_ta = create_token_account(&mut ctx, &mint, &fa.pubkey()).await;

    mint_to_account(&mut ctx, &mint, &la_ta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(&mut ctx, &mint, &lb_ta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(&mut ctx, &mint, &b_ta.pubkey(), &ma, 10_000_000_000).await;

    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender_a.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender_b.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + MATURITY_OFFSET;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        1000,
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    // Lender A deposits at t=0
    let dep_a = build_deposit(
        &market,
        &lender_a.pubkey(),
        &la_ta.pubkey(),
        &mint,
        &bp,
        100_000_000,
    );
    send_ok(&mut ctx, dep_a, &[&lender_a]).await;

    // Advance 30 days
    advance_clock_past(&mut ctx, common::PINNED_EPOCH + 30 * 86400).await;

    // Lender B deposits same amount at t=30d
    let dep_b = build_deposit(
        &market,
        &lender_b.pubkey(),
        &lb_ta.pubkey(),
        &mint,
        &bp,
        100_000_000,
    );
    send_ok(&mut ctx, dep_b, &[&lender_b]).await;

    // Read scaled balances
    let pa_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&market, &lender_a.pubkey()).0,
    )
    .await;
    let pb_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&market, &lender_b.pubkey()).0,
    )
    .await;
    let sba = parse_lender_position(&pa_data).scaled_balance;
    let sbb = parse_lender_position(&pb_data).scaled_balance;

    // Lender A should have MORE scaled tokens than B (same deposit, but A deposited
    // when scale_factor was lower, so got more scaled tokens)
    assert!(
        sba > sbb,
        "early depositor should have more scaled tokens: {sba} > {sbb}"
    );
}
