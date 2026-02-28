//! CU Benchmark Tests
//!
//! Measures compute unit usage for key protocol instructions by executing
//! them through the BPF VM and verifying they complete within budget.
//! Run with `--nocapture` to see CU estimates printed.

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
    compute_budget::ComputeBudgetInstruction, pubkey::Pubkey, signature::Keypair, signer::Signer,
    transaction::Transaction,
};

const MATURITY_OFFSET: i64 = 365 * 24 * 60 * 60;
const CU_LIMIT: u64 = 200_000;

/// Send an instruction with a CU limit and verify it succeeds within budget.
async fn send_with_cu_limit(
    ctx: &mut solana_program_test::ProgramTestContext,
    ix: solana_sdk::instruction::Instruction,
    signers: &[&Keypair],
    cu_limit: u32,
    label: &str,
) {
    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(cu_limit);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(&[cu_ix, ix], Some(&ctx.payer.pubkey()), &all, bh);
    let result = ctx.banks_client.process_transaction(tx).await;
    match result {
        Ok(()) => {
            println!("[CU OK] {label}: completed within {cu_limit} CU");
        },
        Err(err) => {
            panic!("[CU FAIL] {label}: failed with CU limit {cu_limit}: {err:?}");
        },
    }
}

struct BenchSetup {
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

async fn bench_setup(ctx: &mut solana_program_test::ProgramTestContext) -> BenchSetup {
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

    BenchSetup {
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
// Benchmark 1: Initialize Protocol
// ===========================================================================
#[tokio::test]
async fn bench_initialize_protocol() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fa = Keypair::new();
    let wm = Keypair::new();
    let bp = Pubkey::new_unique();

    airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
    setup_program_data_account(&mut ctx, &admin.pubkey());

    let ix = build_initialize_protocol(&admin.pubkey(), &fa.pubkey(), &wm.pubkey(), &bp, 500);
    // InitializeProtocol estimated at ~25,000 CU
    send_with_cu_limit(&mut ctx, ix, &[&admin], 50_000, "InitializeProtocol").await;
}

// ===========================================================================
// Benchmark 2: Deposit with accrual
// ===========================================================================
#[tokio::test]
async fn bench_deposit_with_accrual() {
    let mut ctx = common::start_context().await;
    let s = bench_setup(&mut ctx).await;

    // First deposit to initialize lender position
    let dep1 = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        50_000_000,
    );
    send_ok(&mut ctx, dep1, &[&s.lender]).await;

    // Advance time to trigger interest accrual
    advance_clock_past(&mut ctx, common::PINNED_EPOCH + 86400).await;

    // Second deposit triggers accrual
    let dep2 = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        50_000_000,
    );
    // Deposit estimated at ~55,000 CU
    send_with_cu_limit(&mut ctx, dep2, &[&s.lender], 100_000, "Deposit+Accrual").await;
}

// ===========================================================================
// Benchmark 3: Borrow
// ===========================================================================
#[tokio::test]
async fn bench_borrow() {
    let mut ctx = common::start_context().await;
    let s = bench_setup(&mut ctx).await;

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
        50_000_000,
    );
    // Borrow estimated at ~50,000 CU
    send_with_cu_limit(&mut ctx, borrow, &[&s.borrower], 100_000, "Borrow").await;
}

// ===========================================================================
// Benchmark 4: Repay
// ===========================================================================
#[tokio::test]
async fn bench_repay() {
    let mut ctx = common::start_context().await;
    let s = bench_setup(&mut ctx).await;

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
        50_000_000,
    );
    send_ok(&mut ctx, borrow, &[&s.borrower]).await;

    let repay = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        50_000_000,
    );
    // Repay estimated at ~30,000 CU
    send_with_cu_limit(&mut ctx, repay, &[&s.borrower], 80_000, "Repay").await;
}

// ===========================================================================
// Benchmark 5: Withdraw (first settlement)
// ===========================================================================
#[tokio::test]
async fn bench_withdraw_first_settlement() {
    let mut ctx = common::start_context().await;
    let s = bench_setup(&mut ctx).await;

    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    // Advance past maturity + grace period
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

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
    // Withdraw estimated at ~60,000 CU (includes settlement factor computation)
    send_with_cu_limit(
        &mut ctx,
        withdraw,
        &[&s.lender],
        120_000,
        "Withdraw+Settlement",
    )
    .await;
}

// ===========================================================================
// Benchmark 6: Collect Fees
// ===========================================================================
#[tokio::test]
async fn bench_collect_fees() {
    let mut ctx = common::start_context().await;
    let s = bench_setup(&mut ctx).await;

    let dep = build_deposit(
        &s.market,
        &s.lender.pubkey(),
        &s.lender_ta,
        &s.mint,
        &s.blacklist_program,
        100_000_000,
    );
    send_ok(&mut ctx, dep, &[&s.lender]).await;

    // Advance past maturity + grace period
    let mdata = get_account_data(&mut ctx, &s.market).await;
    let parsed = parse_market(&mdata);
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 301).await;

    // Use repay_interest to make vault solvent (covers accrued interest + fees)
    let ri =
        build_repay_interest_with_amount(&s.market, &s.lender.pubkey(), &s.lender_ta, 50_000_000);
    send_ok(&mut ctx, ri, &[&s.lender]).await;

    // Withdraw to trigger settlement (settlement_factor should be WAD)
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

    let cf = build_collect_fees(&s.market, &s.fee_authority.pubkey(), &s.fee_ta);
    // CollectFees estimated at ~45,000 CU
    send_with_cu_limit(&mut ctx, cf, &[&s.fee_authority], 100_000, "CollectFees").await;
}

// ===========================================================================
// Benchmark 7: Re-Settle
// ===========================================================================
#[tokio::test]
async fn bench_re_settle() {
    let mut ctx = common::start_context().await;
    let s = bench_setup(&mut ctx).await;

    // Deposit, borrow, partial repay
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

    // Trigger initial settlement (use meaningful amount to avoid ZeroPayout)
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

    // Repay more
    let repay2 = build_repay(
        &s.market,
        &s.borrower.pubkey(),
        &s.borrower_ta,
        &s.mint,
        &s.borrower.pubkey(),
        20_000_000,
    );
    send_ok(&mut ctx, repay2, &[&s.borrower]).await;

    // Advance further past grace period (already past maturity+301 from above)
    advance_clock_past(&mut ctx, parsed.maturity_timestamp + 602).await;

    let (vault, _) = get_vault_pda(&s.market);
    let rs = build_re_settle(&s.market, &vault);
    // ReSettle estimated at ~35,000 CU
    send_with_cu_limit(&mut ctx, rs, &[], 80_000, "ReSettle").await;
}

// ===========================================================================
// Benchmark 8: Worst-case Withdraw (max rate, 1-year accrual)
// ===========================================================================
#[tokio::test]
async fn bench_worst_case_withdraw() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &wm, &fa],
        10_000_000_000,
    )
    .await;
    let mint = create_mint(&mut ctx, &ma, 6).await;
    let lta = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let bta = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fta = create_token_account(&mut ctx, &mint, &fa.pubkey()).await;

    mint_to_account(&mut ctx, &mint, &lta.pubkey(), &ma, 10_000_000_000).await;
    mint_to_account(&mut ctx, &mint, &bta.pubkey(), &ma, 10_000_000_000).await;

    // Use maximum interest rate (100%) with moderate fee rate (5%)
    // 100% fee rate would reserve all vault funds for fees, leaving 0 for lenders
    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + MATURITY_OFFSET;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        10_000, // 100% annual interest
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    // Deposit large amount
    let dep = build_deposit(
        &market,
        &lender.pubkey(),
        &lta.pubkey(),
        &mint,
        &bp,
        500_000_000,
    );
    send_ok(&mut ctx, dep, &[&lender]).await;

    // Use repay_interest to cover the accrued interest (~100% of 500M = 500M)
    // so vault is solvent when settlement computes
    let ri =
        build_repay_interest_with_amount(&market, &borrower.pubkey(), &bta.pubkey(), 600_000_000);
    send_ok(&mut ctx, ri, &[&borrower]).await;

    // Advance full year to maturity
    advance_clock_past(&mut ctx, maturity + 301).await;

    // This withdraw triggers worst-case: full year accrual at max rate + settlement
    let pos_data = get_account_data(
        &mut ctx,
        &get_lender_position_pda(&market, &lender.pubkey()).0,
    )
    .await;
    let pos = parse_lender_position(&pos_data);
    let withdraw = build_withdraw(
        &market,
        &lender.pubkey(),
        &lta.pubkey(),
        &bp,
        pos.scaled_balance,
        0,
    );
    send_with_cu_limit(&mut ctx, withdraw, &[&lender], 150_000, "WorstCaseWithdraw").await;
}
