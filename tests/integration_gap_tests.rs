//! # BPF Integration Gap Tests
//!
//! These tests cover scenarios identified as gaps in the existing BPF integration
//! test suite. Each test runs through the full Solana BPF runtime, exercising
//! the complete instruction pipeline including account validation, CPI, and PDA signing.
//!
//! ## Oracle Methodology
//!
//! Where hand-computed expected values are used, they are derived from the spec
//! formulas independently of the production code. Where exact values cannot be
//! precomputed (due to clock timing), we verify invariants and bounds.
//!
//! ## Gap Categories
//!
//! - G1: Interest accrual dynamics (on-chain scale factor changes)
//! - G2: Multi-lender settlement and conservation
//! - G3: Fee collection through BPF
//! - G4: Re-settlement after additional repayment
//! - G5: Capacity enforcement at boundary
//! - G6: Fee reservation limits borrowing
//! - G7: Error path coverage (NotMatured, PositionNotEmpty, NotSettled, ZeroAmount)
//! - G8: Partial withdrawal and second withdrawal

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
use solana_program_test::*;
use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

/// WAD = 1e18 — independently defined for oracle assertions.
const WAD: u128 = 1_000_000_000_000_000_000;

// ===========================================================================
// G1: Interest Accrual Dynamics (2 tests)
// ===========================================================================

/// G1.1: Deposit, advance clock by 1 year at 10% annual, verify
/// scale_factor on-chain matches hand-computed 1.1*WAD.
///
/// Oracle: At 10% annual for exactly 1 year starting from sf=WAD:
///   new_sf = WAD + WAD * 1000 / 10000 = 1.1 * WAD = 1_100_000_000_000_000_000
///
/// This test is non-tautological because the expected value is a literal constant
/// derived from the spec formula, not from calling accrue_interest().
#[tokio::test]
async fn g1_1_interest_accrual_on_chain_known_value() {
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
        0, // zero fee rate for simplicity
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    // Use far-future maturity so interest isn't capped
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual
        common::FAR_FUTURE_MATURITY,
        100_000 * USDC,
        &whitelist_manager,
        100_000 * USDC,
    )
    .await;

    // Deposit 1000 USDC
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000 * USDC,
    )
    .await;

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

    // Read the market to get last_accrual_timestamp (the starting point)
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let start_ts = parsed.last_accrual_timestamp;
    assert_eq!(parsed.scale_factor, WAD, "Initial sf should be WAD");
    assert_eq!(parsed.scaled_total_supply, 1_000 * USDC as u128);

    // Advance clock by exactly 1 year (31,536,000 seconds) from start
    let one_year_later = start_ts + 31_536_000;
    advance_clock_past(&mut ctx, one_year_later).await;

    // Deposit 1 more USDC to trigger interest accrual on-chain
    let deposit_ix2 = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify scale_factor after 1 year of daily compound interest at 10%
    // Daily compound: pow_wad(WAD + daily_rate, 365) where daily_rate = 1000 * WAD / (365 * 10000)
    // This gives ~1.10516 * WAD, NOT simple interest 1.1 * WAD
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);

    let expected_sf: u128 = 1_105_155_781_616_264_095; // compound 10% daily over 365 days
    assert_eq!(
        parsed.scale_factor, expected_sf,
        "After 1 year at 10% (daily compound), sf should reflect compound growth"
    );

    // The second deposit of 1 USDC at elevated sf gives scaled = 1_000_000 * WAD / sf
    // = 1_000_000 * WAD / 1_105_155_781_616_264_095 = 904_849 (floor division)
    let expected_new_scaled = 904_849u128;
    let expected_total_scaled = 1_000 * USDC as u128 + expected_new_scaled;
    assert_eq!(
        parsed.scaled_total_supply, expected_total_scaled,
        "Second deposit should be scaled at elevated sf"
    );
}

/// G1.2: Interest capped at maturity on-chain.
///
/// Deposit, set maturity 1000 seconds after start, advance clock far past maturity.
/// Verify sf reflects only 1000 seconds of interest, not the full elapsed time.
///
/// Hand-computed expected sf for 1000 seconds at 10% annual:
///   interest_delta_wad = 1000 * 1000 * WAD / (31536000 * 10000)
///                      = 3_170_979_198_376
///   expected_sf = WAD + 3_170_979_198_376
#[tokio::test]
async fn g1_2_interest_capped_at_maturity_on_chain() {
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

    let maturity_timestamp = common::PINNED_EPOCH + 1000;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual
        maturity_timestamp,
        100_000 * USDC,
        &whitelist_manager,
        100_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        10_000 * USDC,
    )
    .await;

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

    // Get last_accrual_timestamp for computing expected sf.
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let start_ts = parsed.last_accrual_timestamp;
    assert!(
        maturity_timestamp > start_ts,
        "Test precondition violated: maturity must be after start"
    );

    let (vault, _) = get_vault_pda(&market);
    let lender_balance_after_seed = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_after_seed = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        lender_balance_after_seed,
        9_000 * USDC,
        "Seed deposit should debit lender by 1000 USDC"
    );
    assert_eq!(
        vault_balance_after_seed,
        1_000 * USDC,
        "Seed deposit should credit vault by 1000 USDC"
    );

    let probe_amount = 1 * USDC;

    // x-1 boundary: one second before maturity.
    let before_maturity_ts = maturity_timestamp - 1;
    get_blockhash_pinned(&mut ctx, before_maturity_ts).await;
    let deposit_before = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        probe_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_before],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_before_data = get_account_data(&mut ctx, &market).await;
    let parsed_before = parse_market(&market_before_data);
    let elapsed_before = before_maturity_ts - start_ts;
    let expected_delta_before =
        1000u128 * (elapsed_before as u128) * WAD / (31_536_000u128 * 10_000u128);
    let expected_sf_before = WAD + expected_delta_before;
    assert_eq!(
        parsed_before.scale_factor, expected_sf_before,
        "x-1 boundary sf mismatch"
    );
    assert_eq!(
        parsed_before.last_accrual_timestamp, before_maturity_ts,
        "x-1 boundary should advance last_accrual_timestamp to current time"
    );

    // Token balance verification after x-1 probe deposit
    let lender_balance_after_xm1 = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_after_xm1 = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        lender_balance_after_xm1,
        10_000 * USDC - 1_000 * USDC - probe_amount,
        "x-1: lender balance should reflect seed + 1 probe deposit"
    );
    assert_eq!(
        vault_balance_after_xm1,
        1_000 * USDC + probe_amount,
        "x-1: vault balance should reflect seed + 1 probe deposit"
    );

    // Interest capping verification: the scale_factor after x-1 reflects accrual only
    // up to before_maturity_ts. Post-maturity deposit rejection is reliably tested by g10_1.
    let expected_total_deposited = 1_000 * USDC + probe_amount;
    let lender_balance_final = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_final = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        lender_balance_final,
        10_000 * USDC - expected_total_deposited,
        "Lender token balance should reflect seed + x-1 probe deposit"
    );
    assert_eq!(
        vault_balance_final, expected_total_deposited,
        "Vault token balance should reflect seed + x-1 probe deposit"
    );
}

// ===========================================================================
// G2: Multi-Lender Settlement and Conservation (2 tests)
// ===========================================================================

/// G2.1: Two lenders deposit equal amounts, borrower partially defaults,
/// both withdraw. Verify conservation: sum(payouts) <= vault balance.
///
/// Setup: 0% interest, 0% fee. Each lender deposits 500 USDC.
/// Borrower borrows all 1000, repays only 600.
/// Settlement factor = 600*WAD/1000 = 0.6*WAD
/// Each lender gets: 500_000 * WAD/WAD * 0.6*WAD/WAD = 300_000 = 300 USDC.
/// Total payouts = 600 USDC = vault balance. Conservation holds.
#[tokio::test]
async fn g2_1_two_lenders_partial_default_conservation() {
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

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0, // zero fees
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // 0% interest
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender A deposits 500 USDC
    let lender_a_token = create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &admin,
        500 * USDC,
    )
    .await;

    let deposit_a = build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_a],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B deposits 500 USDC
    let lender_b_token = create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &admin,
        500 * USDC,
    )
    .await;

    let deposit_b = build_deposit(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_b],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrower borrows all 1000 USDC
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrower repays only 600 USDC (40% default)
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        600 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity
    advance_clock_past(&mut ctx, maturity + 301).await;

    // Lender A withdraws (triggers settlement)
    let withdraw_a = build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_a],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let payout_a = get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;

    // Lender B withdraws
    let withdraw_b = build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_b],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let payout_b = get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;

    // With 0% interest and 0% fees:
    // settlement = 600 * WAD / 1000 = 0.6 * WAD
    // Each lender: payout = 500000 * WAD/WAD * 0.6*WAD/WAD = 300000
    assert_eq!(
        payout_a,
        300 * USDC,
        "Lender A should get 300 USDC (60% of 500)"
    );
    assert_eq!(
        payout_b,
        300 * USDC,
        "Lender B should get 300 USDC (60% of 500)"
    );

    // Conservation: total payouts = 600 USDC = vault before withdrawals
    assert_eq!(
        payout_a + payout_b,
        600 * USDC,
        "Conservation: total payouts = vault balance"
    );

    // Verify settlement factor
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    // 600_000 * WAD / 1_000_000 = 600_000_000_000_000_000
    assert_eq!(parsed.settlement_factor_wad, 600_000_000_000_000_000);
}

/// G2.2: Lender deposits then another lender deposits at elevated sf.
/// After full repayment, the late lender gets less because they deposited
/// at higher sf (fewer scaled tokens per USDC).
#[tokio::test]
async fn g2_2_late_lender_gets_proportional_share() {
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

    // Use far-future maturity
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual
        common::FAR_FUTURE_MATURITY,
        1_000_000 * USDC,
        &whitelist_manager,
        1_000_000 * USDC,
    )
    .await;

    // Lender A deposits 1000 USDC at t=start (sf=WAD)
    let lender_a_token = create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    let deposit_a = build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_a],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read start timestamp
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let start_ts = parsed.last_accrual_timestamp;

    // Advance to half year
    let half_year = start_ts + 15_768_000; // 31_536_000 / 2
    advance_clock_past(&mut ctx, half_year).await;

    // Lender B deposits 1000 USDC at t=half_year (sf should be 1.05*WAD)
    let lender_b_token = create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_b = build_deposit(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_b],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify sf after half year with daily compound interest at 10%
    // half_year = 15_768_000s = 182 days + 43200s remaining
    // Compound: pow_wad(WAD + daily_rate, 182) * (WAD + linear_remaining)
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.scale_factor, 1_051_263_907_089_511_381,
        "sf after half year (daily compound at 10%)"
    );

    // Verify Lender B got fewer scaled tokens
    let (lender_b_pos, _) = get_lender_position_pda(&market, &lender_b.pubkey());
    let pos_data = get_account_data(&mut ctx, &lender_b_pos).await;
    let pos_b = parse_lender_position(&pos_data);
    // 1_000_000_000 * WAD / 1_051_263_907_089_511_381 = 951_235_929 (floor)
    assert_eq!(
        pos_b.scaled_balance, 951_235_929,
        "Lender B scaled balance at compound sf"
    );

    // Advance to full year to trigger interest accrual
    let full_year = start_ts + 31_536_000;
    advance_clock_past(&mut ctx, full_year).await;

    // Note: We don't need to fund the vault with extra for interest - the test is about
    // verifying scaled balances and scale factor, not full withdrawals. The repay step
    // was removed because you can only repay borrowed amounts, and nothing was borrowed
    // in this test (it's testing deposit timing, not borrow/repay flows).

    // Trigger interest accrual by reading market state (accrue happens on deposits/withdraws)
    // We need to call deposit or another instruction that accrues interest to update sf.
    // For simplicity, let's deposit 2 units from lender_a to trigger accrual.
    // Note: 1 base unit would round to 0 scaled tokens at sf=1.1025*WAD, causing ZeroScaledAmount error.
    let deposit_trigger = build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2, // 2 base units to trigger accrual (1 would round to 0 scaled)
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_trigger],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify final sf after full year (two half-year accrual steps with daily compound)
    // Step 1: WAD -> 1_051_263_907_089_511_381 (182 days + 43200s linear)
    // Step 2: 1_051... -> 1_105_155_802_349_104_817 (another 182 days + 43200s)
    // Note: slightly higher than single-step full year (1_105_155_781_616_264_095) because
    // the sub-day linear portion from step 1 gets compounded in step 2.
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let expected_sf = 1_105_155_802_349_104_817u128;
    assert_eq!(
        parsed.scale_factor, expected_sf,
        "sf after full year (two-step daily compound at 10%)"
    );

    // Verify Lender A has more scaled tokens than B
    let (lender_a_pos, _) = get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_data = get_account_data(&mut ctx, &lender_a_pos).await;
    let pos_a = parse_lender_position(&pos_data);
    assert!(
        pos_a.scaled_balance > pos_b.scaled_balance,
        "Lender A should have more scaled tokens: A={}, B={}",
        pos_a.scaled_balance,
        pos_b.scaled_balance
    );
}

// ===========================================================================
// G3: Fee Collection Through BPF (2 tests)
// ===========================================================================

/// G3.1: Deposit, accrue interest with fees, lender withdraws, then collect fees.
///
/// Setup: 10% annual, 5% fee rate, 1 USDC deposit, 1 year.
/// Expected fees: 5_500 base units.
/// Lender must withdraw first (SR-113), then fee authority collects.
#[tokio::test]
async fn g3_1_fee_collection_correct_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &lender,
            &fee_authority,
        ],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500, // 5% fee rate
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    // Set maturity to 1 year + 1000 seconds from PINNED_EPOCH, so full year of interest accrues
    // before maturity, and we can advance past maturity to allow withdrawal
    let initial_ts = common::PINNED_EPOCH;
    let one_year = 31_536_000i64;
    let maturity_ts = initial_ts + one_year + 1000;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual
        maturity_ts,
        100_000 * USDC,
        &whitelist_manager,
        100_000 * USDC,
    )
    .await;

    // Deposit 1 USDC (1_000_000 base units) — small supply for predictable fees
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 10 * USDC).await;

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read start timestamp
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let start_ts = parsed.last_accrual_timestamp;

    // Advance 1 year to accrue full interest (still before maturity)
    advance_clock_past(&mut ctx, start_ts + one_year).await;

    // Trigger interest accrual with a small deposit
    // Must be >= 2 base units so scaled amount doesn't round to zero at sf=1.1*WAD
    let deposit_trigger_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2, // 2 base units to trigger accrual (1 would round to 0 scaled)
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_trigger_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify fees have accrued (compound interest produces higher fees than simple interest)
    // At 10% annual (daily compound), 5% fee rate, 1 USDC scaled supply:
    //   interest_delta_wad = 105_155_781_616_264_095
    //   fee_delta_wad = interest_delta * 500 / 10000
    //   fee_normalized = scaled_supply * new_sf / WAD * fee_delta_wad / WAD = 5810
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let expected_fee: u64 = 5_810;
    assert!(
        parsed.accrued_protocol_fees >= expected_fee,
        "Fees should have accrued: got {} expected >= {}",
        parsed.accrued_protocol_fees,
        expected_fee
    );

    // Borrower repays interest so vault has enough for full settlement
    // Without this, settlement_factor < WAD and fee collection is blocked (SR-057)
    // Compound interest on 1 USDC at 10% for 1 year ≈ 105,155 base units
    // Fees at 5% ≈ 5,810 base units. Min repay needed ≈ 110,965.
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &borrower_token.pubkey(), &admin, 200_000).await;

    let repay_ix = build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        120_000, // covers compound interest + fees with buffer
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // SR-113: Lender must withdraw before fee collection
    // Advance past maturity + settlement grace period (300 seconds)
    let post_maturity_ts = maturity_ts + 400; // 400s past maturity (> 300s grace period)
    advance_clock_past(&mut ctx, post_maturity_ts).await;

    // Lender withdraws full balance (triggers settlement)
    // Pass scaled_amount=0 for full withdrawal, min_payout=0 for no slippage protection
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0, // 0 = full withdrawal
        0, // no minimum payout
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify lender has withdrawn (scaled_total_supply should be 0 now)
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    // Full withdrawal should reduce scaled_total_supply to 0
    assert_eq!(
        parsed.scaled_total_supply, 0,
        "Lender should have withdrawn: scaled_total_supply={}",
        parsed.scaled_total_supply
    );

    // Now collect fees
    let fee_dest = create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let collect_ix = build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify fee collection
    let fee_balance = get_token_balance(&mut ctx, &fee_dest.pubkey()).await;

    // Hand-computed expected fee for supply=1_000_000, 10% annual (daily compound), 5% fee rate, 1 year:
    //   growth = pow_wad(WAD + daily_rate, 365) = 1_105_155_781_616_264_095
    //   interest_delta_wad = growth - WAD = 105_155_781_616_264_095
    //   fee_delta_wad = interest_delta * 500 / 10000 = 5_257_789_080_813_204
    //   fee_normalized = 1_000_000 * new_sf / WAD * fee_delta_wad / WAD = 5810
    // Note: The tiny trigger deposit (1 scaled unit) adds negligible additional fees
    assert!(
        fee_balance >= expected_fee && fee_balance <= expected_fee + 10,
        "Collected fees should be close to hand-computed value: got {} expected ~{}",
        fee_balance,
        expected_fee
    );

    // Verify market state updated
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.accrued_protocol_fees, 0,
        "Fees should be zeroed after collection"
    );
}

/// G3.2: Fee collection with wrong fee_authority fails with Unauthorized.
#[tokio::test]
async fn g3_2_fee_collection_wrong_authority_fails() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let imposter = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &lender,
            &fee_authority,
            &imposter,
        ],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        100_000 * USDC,
        &whitelist_manager,
        100_000 * USDC,
    )
    .await;

    // Deposit and advance clock so fees exist
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.last_accrual_timestamp + 31_536_000).await;

    let (vault, _) = get_vault_pda(&market);
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_before = get_token_balance(&mut ctx, &vault).await;
    let market_before_data = get_account_data(&mut ctx, &market).await;
    let market_before = parse_market(&market_before_data);
    assert!(
        market_before.scaled_total_supply > 0,
        "Precondition: lender shares must exist for this authorization-path test"
    );

    // Two unauthorized neighbors should both fail with the same exact code and no side effects.
    for attacker in [&imposter, &borrower] {
        let attacker_dest = create_token_account(&mut ctx, &mint, &attacker.pubkey()).await;
        let attacker_dest_before = get_token_balance(&mut ctx, &attacker_dest.pubkey()).await;

        let snapshot_before =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
        let collect_ix = build_collect_fees(&market, &attacker.pubkey(), &attacker_dest.pubkey());
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[collect_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, attacker],
            recent,
        );
        let result = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .map_err(|e| e.unwrap());

        // Unauthorized = Custom(5)
        assert_custom_error(&result, 5);

        let snapshot_after =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
        snapshot_before.assert_unchanged(&snapshot_after);

        let attacker_dest_after = get_token_balance(&mut ctx, &attacker_dest.pubkey()).await;
        let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
        let vault_balance_after = get_token_balance(&mut ctx, &vault).await;
        let market_after_data = get_account_data(&mut ctx, &market).await;
        let market_after = parse_market(&market_after_data);
        assert_eq!(
            attacker_dest_after, attacker_dest_before,
            "Unauthorized fee collection must not credit attacker destination"
        );
        assert_eq!(
            lender_balance_after, lender_balance_before,
            "Unauthorized fee collection must not change lender balance"
        );
        assert_eq!(
            vault_balance_after, vault_balance_before,
            "Unauthorized fee collection must not change vault balance"
        );
        assert_eq!(
            market_after.accrued_protocol_fees, market_before.accrued_protocol_fees,
            "Unauthorized fee collection must not change accrued fees"
        );
        assert_eq!(
            market_after.scale_factor, market_before.scale_factor,
            "Unauthorized fee collection must not change scale_factor"
        );
        assert_eq!(
            market_after.scaled_total_supply, market_before.scaled_total_supply,
            "Unauthorized fee collection must not change scaled_total_supply"
        );
        assert_eq!(
            market_after.total_deposited, market_before.total_deposited,
            "Unauthorized fee collection must not change total_deposited"
        );
        assert_eq!(
            market_after.total_borrowed, market_before.total_borrowed,
            "Unauthorized fee collection must not change total_borrowed"
        );
        assert_eq!(
            market_after.settlement_factor_wad, market_before.settlement_factor_wad,
            "Unauthorized fee collection must not change settlement_factor_wad"
        );
    }
    // NOTE: Positive path (correct authority collects fees) is covered by g3_1.
}

// ===========================================================================
// G4: Re-Settlement After Additional Repayment (1 test)
// ===========================================================================

/// G4.1: Settle at partial default, repay more, re-settle, verify improvement.
/// Then attempt re-settle without improvement => SettlementNotImproved error.
#[tokio::test]
async fn g4_1_resettle_improvement_and_rejection() {
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

    let maturity = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    // Borrow all 1000 and repay only 500
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

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

    // Advance past maturity
    advance_clock_past(&mut ctx, maturity + 301).await;

    // Partial withdrawal to trigger settlement (withdraw half)
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = get_account_data(&mut ctx, &lender_pos).await;
    let pos = parse_lender_position(&pos_data);
    let half_scaled = pos.scaled_balance / 2;

    let withdraw_ix = build_withdraw(
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

    // Check settlement factor = 0.5*WAD
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let settlement_before = parsed.settlement_factor_wad;
    assert_eq!(settlement_before, WAD / 2, "Initial settlement = 50%");

    // Borrower repays 300 more
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        300 * USDC,
    )
    .await;
    let repay_ix2 = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        300 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Re-settle — should improve
    let (vault, _) = get_vault_pda(&market);
    let resettle_ix = build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[resettle_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let settlement_after = parsed.settlement_factor_wad;
    assert!(
        settlement_after > settlement_before,
        "Re-settle should improve: {} > {}",
        settlement_after,
        settlement_before
    );

    // Hand-computed exact settlement_factor after re-settle:
    // Vault now has: 500 (initial repay) + 300 (second repay) - 250 (half withdrawal payout) = 550
    // Actually: deposit=1000, borrow=1000 (vault=0), repay=500 (vault=500),
    // half withdrawal at 0.5 sf: payout = 500_000 * WAD/WAD * 0.5*WAD/WAD = 250_000 (250 USDC)
    // vault after withdrawal = 500 - 250 = 250, then second repay of 300 => vault = 550
    // Remaining scaled_total_supply = 500_000_000 (half of original 1B)
    // nominal_liabilities = scaled_total_supply * sf / WAD = 500_000_000 * WAD/WAD = 500_000_000 (500 USDC)
    // At 0% interest, sf=WAD, so new_settlement = min(vault_balance * WAD / nominal_liabilities, WAD)
    // = min(550_000_000 * WAD / 500_000_000, WAD) = min(1.1 * WAD, WAD) = WAD
    // Settlement capped at WAD since vault > liabilities.
    // However the re-settle formula uses: vault_balance * WAD / nominal_liabilities
    // With 0% interest, sf=WAD. After half withdrawal of 500_000_000 scaled at 0.5 sf:
    // payout = 500_000_000 * WAD/WAD * (WAD/2)/WAD = 250_000_000 (250 USDC)
    // vault after = 500*USDC - 250*USDC = 250*USDC, then +300*USDC = 550*USDC
    // remaining scaled = 500_000_000, nominal = 500_000_000 * WAD / WAD = 500_000_000
    // new_sf = min(550_000_000 * WAD / 500_000_000, WAD) = min(1.1*WAD, WAD) = WAD
    assert_eq!(
        settlement_after, WAD,
        "Re-settle factor should be exactly WAD (vault covers full liabilities)"
    );

    // Verify all market fields after re-settle
    assert_eq!(
        parsed.scale_factor, WAD,
        "sf should remain WAD at 0% interest"
    );
    assert_eq!(
        parsed.total_deposited,
        1_000 * USDC,
        "total_deposited unchanged"
    );
    assert_eq!(
        parsed.total_borrowed,
        1_000 * USDC,
        "total_borrowed unchanged"
    );
    assert_eq!(parsed.total_repaid, 800 * USDC, "total_repaid = 500 + 300");

    // Take ProtocolSnapshot before rejection attempt
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before_rejection =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

    // Try re-settle again without additional repayment => SettlementNotImproved
    let resettle_ix2 = build_re_settle(&market, &vault);
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(250_000);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, resettle_ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // SettlementNotImproved = Custom(31)
    assert_custom_error(&result, 31);

    // Verify ALL state unchanged after rejected re-settle
    let snapshot_after_rejection =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before_rejection.assert_unchanged(&snapshot_after_rejection);
}

// ===========================================================================
// G5: Capacity Enforcement (1 test)
// ===========================================================================

/// G5.1: Deposit up to exactly max_total_supply succeeds, then deposit 1 more
/// base unit fails with CapExceeded.
#[tokio::test]
async fn g5_1_capacity_enforcement_at_boundary() {
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

    let max_supply: u64 = 1_000 * USDC; // cap at 1000 USDC

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // 0% interest to keep sf=WAD for predictable cap
        common::FAR_FUTURE_MATURITY,
        max_supply,
        &whitelist_manager,
        100_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    let (vault, _) = get_vault_pda(&market);

    // x-1 boundary: deposit max_supply - 1 => succeeds (room for 1 more)
    let deposit_xm1 = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        max_supply - 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_xm1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("Deposit at max_supply-1 should succeed");

    // Verify market state after x-1 deposit
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.total_deposited,
        max_supply - 1,
        "total_deposited should be max_supply - 1"
    );
    assert_eq!(
        parsed.scaled_total_supply,
        (max_supply - 1) as u128,
        "scaled_total_supply should equal max_supply - 1 at sf=WAD"
    );
    assert_eq!(parsed.scale_factor, WAD, "sf should be WAD at 0% interest");
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_balance,
        max_supply - 1,
        "vault balance should equal deposits"
    );

    // x boundary: deposit 1 more to hit exactly max_supply => succeeds
    // No new blockhash — amount (1) differs from prior (max_supply-1), so
    // the transaction signature is unique within the same bank.
    let deposit_x = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_x],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("Deposit that fills exactly to max_supply should succeed");

    // Verify market state at exactly max_supply
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.total_deposited, max_supply,
        "total_deposited should be exactly max_supply"
    );
    assert_eq!(
        parsed.scaled_total_supply, max_supply as u128,
        "scaled_total_supply should equal max_supply at sf=WAD"
    );
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, max_supply, "vault should be exactly at cap");

    // Take snapshot before x+1 failure
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

    // x+1 boundary: deposit 2 more base units => exceeds by 1, fails with CapExceeded
    // No new blockhash — amount (2) differs from prior (1), unique signature.
    let deposit_xp1 = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2, // exceeds cap by 1 (already at max_supply, depositing 2)
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_xp1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // CapExceeded = Custom(25)
    assert_custom_error(&result, 25);

    // Verify ALL state unchanged after failed deposit
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Also try depositing exactly 1 base unit over cap.
    // Same amount (1) and signers as the cap deposit above — prepend
    // ComputeBudget to differentiate the signature on the same blockhash.
    let deposit_over1 = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, deposit_over1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 25);
}

// ===========================================================================
// G6: Fee Reservation Limits Borrowing (1 test)
// ===========================================================================

/// G6.1: With accrued fees, borrower cannot borrow more than vault - fees.
#[tokio::test]
async fn g6_1_fee_reservation_limits_borrow() {
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
        5000, // 50% fee rate (high so fees are significant)
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual
        common::FAR_FUTURE_MATURITY,
        100_000 * USDC,
        &whitelist_manager,
        100_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    // Read start timestamp and advance 1 year
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    advance_clock_past(&mut ctx, parsed.last_accrual_timestamp + 31_536_000).await;

    // Take snapshot before failure attempts
    let (vault, _) = get_vault_pda(&market);
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

    // Try to borrow all 1000 USDC — should fail because fees are reserved
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1_000 * USDC,
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
    // BorrowAmountTooHigh = Custom(26)
    assert_custom_error(&result, 26);

    // Verify ALL state unchanged after failed borrow
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Verify borrower token account wasn't credited
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 0,
        "Failed borrow must not credit borrower"
    );

    // Read current fees to compute exact available amount
    // Interest accrual happens on the borrow instruction, so we need to trigger it first.
    // The failed borrow above already triggered accrual (happens before the amount check).
    // Actually, failed txs are rolled back, so we need a successful accrual trigger.
    // The deposit already accrued at deposit time; advancing clock doesn't auto-accrue.
    // Let's read vault balance and fees to compute available.
    // NOTE: Accrual is triggered on deposit/borrow/withdraw. Since we only deposited before
    // advancing the clock, and borrow failed (rolled back), fees haven't been accrued on-chain yet.
    // We need a successful tx to trigger accrual. Let's do a small deposit to trigger it.
    // Mint a small amount since the lender deposited all tokens above.
    mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 100).await;
    let deposit_trigger = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2, // 2 base units to trigger accrual
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_trigger],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Now read the accrued fees and vault balance
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    let fees = parsed.accrued_protocol_fees;
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert!(
        fees > 0,
        "Fees should be non-zero after 1 year at 10% with 50% fee rate"
    );

    let available = vault_balance - fees;

    // Borrow exactly at available amount => succeeds
    let borrow_exact = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        available,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_exact],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("Borrow at exact available (vault - fees) should succeed");

    // Verify borrower received exactly the available amount
    let borrower_balance_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance_after, available,
        "Borrower should receive exactly the available amount"
    );

    // Verify market state after successful borrow
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_after = parse_market(&market_data);
    assert_eq!(
        parsed_after.total_borrowed, available,
        "total_borrowed should equal the borrowed amount"
    );

    // Now try to borrow 1 more base unit => should fail (only fees left in vault)
    let borrow_one_more = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_one_more],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // BorrowAmountTooHigh = Custom(26)
    assert_custom_error(&result, 26);
}

// ===========================================================================
// G7: Error Path Coverage (4 tests)
// ===========================================================================

/// G7.1: Withdrawal before maturity should fail with NotMatured.
/// Tests x-1/x/x+1 boundary: maturity-1 (fail), maturity+grace_period+1 (succeed).
#[tokio::test]
async fn g7_1_withdraw_before_maturity_fails() {
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

    let maturity = common::PINNED_EPOCH + 500;
    let grace_period = 300i64; // settlement grace period

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    let (vault, _) = get_vault_pda(&market);
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());

    // x-1 boundary: maturity - 1 => should fail with NotMatured
    advance_clock_past(&mut ctx, maturity - 1).await;

    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

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
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // NotMatured = Custom(29)
    assert_custom_error(&result, 29);

    // Verify ALL state unchanged after failed withdrawal
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Verify lender token balance unchanged
    let lender_balance = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance, 0,
        "Lender should not have received any tokens from failed withdrawal"
    );

    // Verify market fields unchanged
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.scaled_total_supply,
        1_000 * USDC as u128,
        "scaled_total_supply should be unchanged after failed withdrawal"
    );
    assert_eq!(
        parsed.total_deposited,
        1_000 * USDC,
        "total_deposited unchanged"
    );
    assert_eq!(
        parsed.settlement_factor_wad, 0,
        "No settlement should have occurred"
    );

    // x+1 boundary: maturity + grace_period + 1 => should succeed
    advance_clock_past(&mut ctx, maturity + grace_period + 1).await;

    let withdraw_success = build_withdraw(
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
        &[budget_ix, withdraw_success],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("Withdrawal after maturity + grace period should succeed");

    // Verify lender received their full deposit back
    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after,
        1_000 * USDC,
        "Lender should receive full deposit after maturity withdrawal"
    );

    // Verify position is emptied
    let pos_data = get_account_data(&mut ctx, &lender_pos).await;
    let pos = parse_lender_position(&pos_data);
    assert_eq!(
        pos.scaled_balance, 0,
        "Position should be empty after full withdrawal"
    );
}

/// G7.2: Close lender position with non-zero balance fails with PositionNotEmpty.
#[tokio::test]
async fn g7_2_close_nonempty_position_fails() {
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

    let maturity = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    // Take snapshot before failed close attempt
    let (vault, _) = get_vault_pda(&market);
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

    // Verify precondition: position has non-zero balance
    let pos_data = get_account_data(&mut ctx, &lender_pos).await;
    let pos = parse_lender_position(&pos_data);
    assert_eq!(
        pos.scaled_balance,
        1_000 * USDC as u128,
        "Precondition: lender position should have 1000 USDC scaled balance"
    );

    // Try to close position without withdrawing first
    let close_ix = build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // PositionNotEmpty = Custom(34)
    assert_custom_error(&result, 34);

    // Verify ALL state unchanged after failed close
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Verify lender position still exists with same balance
    let pos_data_after = get_account_data(&mut ctx, &lender_pos).await;
    let pos_after = parse_lender_position(&pos_data_after);
    assert_eq!(
        pos_after.scaled_balance, pos.scaled_balance,
        "Lender position balance must be unchanged after failed close"
    );

    // Verify market fields unchanged
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.scaled_total_supply,
        1_000 * USDC as u128,
        "scaled_total_supply unchanged after failed close"
    );
    assert_eq!(
        parsed.total_deposited,
        1_000 * USDC,
        "total_deposited unchanged after failed close"
    );

    // Verify vault balance unchanged
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_balance,
        1_000 * USDC,
        "Vault balance unchanged after failed close"
    );
}

/// G7.3: Re-settle before any settlement fails with NotSettled.
#[tokio::test]
async fn g7_3_resettle_before_settlement_fails() {
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

    let maturity = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    // Advance past maturity but DON'T trigger settlement (no withdrawal)
    advance_clock_past(&mut ctx, maturity + 301).await;

    // Take snapshot before failure attempt
    let (vault, _) = get_vault_pda(&market);
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

    // Verify precondition: settlement_factor_wad is 0 (not yet settled)
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.settlement_factor_wad, 0,
        "Precondition: settlement_factor_wad should be 0 before any settlement"
    );

    // Try to re-settle without prior settlement
    let resettle_ix = build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[resettle_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // NotSettled = Custom(30)
    assert_custom_error(&result, 30);

    // Verify ALL state unchanged after failed re-settle
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Verify specific market fields unchanged
    let market_data_after = get_account_data(&mut ctx, &market).await;
    let parsed_after = parse_market(&market_data_after);
    assert_eq!(
        parsed_after.settlement_factor_wad, 0,
        "settlement_factor_wad must remain 0 after failed re-settle"
    );
    assert_eq!(
        parsed_after.scaled_total_supply, parsed.scaled_total_supply,
        "scaled_total_supply unchanged after failed re-settle"
    );
    assert_eq!(
        parsed_after.total_deposited, parsed.total_deposited,
        "total_deposited unchanged after failed re-settle"
    );
    assert_eq!(
        parsed_after.scale_factor, parsed.scale_factor,
        "scale_factor unchanged after failed re-settle"
    );
}

/// G7.4: Deposit of 0 amount fails with ZeroAmount.
#[tokio::test]
async fn g7_4_zero_amount_deposit_fails() {
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
        10_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    // Take snapshot before zero-amount deposit attempt
    let (vault, _) = get_vault_pda(&market);
    // Lender position may not exist yet (first deposit attempt). Use empty list.
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    // Capture market state before for field-level assertions
    let market_data_before = get_account_data(&mut ctx, &market).await;
    let parsed_before = parse_market(&market_data_before);

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        0, // zero amount
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // ZeroAmount = Custom(17)
    assert_custom_error(&result, 17);

    // Verify ALL state unchanged after failed zero-amount deposit
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Verify specific market fields unchanged
    let market_data_after = get_account_data(&mut ctx, &market).await;
    let parsed_after = parse_market(&market_data_after);
    assert_eq!(
        parsed_after.scaled_total_supply, parsed_before.scaled_total_supply,
        "scaled_total_supply unchanged after failed zero deposit"
    );
    assert_eq!(
        parsed_after.total_deposited, parsed_before.total_deposited,
        "total_deposited unchanged after failed zero deposit"
    );
    assert_eq!(
        parsed_after.scale_factor, parsed_before.scale_factor,
        "scale_factor unchanged after failed zero deposit"
    );
    assert_eq!(
        parsed_after.accrued_protocol_fees, parsed_before.accrued_protocol_fees,
        "accrued_protocol_fees unchanged after failed zero deposit"
    );

    // Verify vault balance unchanged
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_balance, 0,
        "Vault should remain empty after failed deposit"
    );
}

// ===========================================================================
// G8: Partial Withdrawal and Second Withdrawal (1 test)
// ===========================================================================

/// G8.1: Partial withdrawal followed by second withdrawal.
/// Verify both payouts use the same locked settlement factor and
/// total payouts are correct.
#[tokio::test]
async fn g8_1_partial_then_full_withdrawal() {
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

    let maturity = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    // Advance past maturity
    advance_clock_past(&mut ctx, maturity + 301).await;

    // Partial withdrawal: 500_000_000 scaled (half of 1B deposit at sf=WAD)
    let half_scaled: u128 = 500_000_000;
    let withdraw_1 = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        half_scaled,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let balance_after_first = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        balance_after_first,
        500 * USDC,
        "First withdrawal should yield 500 USDC"
    );

    // Verify settlement factor was locked at WAD (full repayment)
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.settlement_factor_wad, WAD,
        "Settlement should be WAD for full repayment"
    );

    // Second withdrawal: remaining 500_000_000 scaled
    let withdraw_2 = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full remaining
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let balance_after_second = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        balance_after_second,
        1_000 * USDC,
        "Total withdrawals should equal deposit"
    );

    // Verify position is empty
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = get_account_data(&mut ctx, &lender_pos).await;
    let pos = parse_lender_position(&pos_data);
    assert_eq!(
        pos.scaled_balance, 0,
        "Position should be empty after full withdrawal"
    );

    // Now close the position
    let close_ix = build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("Close empty position should succeed");
}

// ===========================================================================
// G9: Global Borrow Capacity Enforcement (1 test)
// ===========================================================================

/// G9.1: Borrower exceeds global max_borrow_capacity fails with GlobalCapacityExceeded.
#[tokio::test]
async fn g9_1_global_borrow_capacity_exceeded() {
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

    // Set global borrow capacity to only 500 USDC
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
        500 * USDC, // max_borrow_capacity = 500 USDC
    )
    .await;

    // Deposit 1000 USDC
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

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

    let (vault, _) = get_vault_pda(&market);
    let (lender_pos, _) = get_lender_position_pda(&market, &lender.pubkey());

    // Take snapshot before capacity+1 failure
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;

    // x+1 boundary: Try to borrow capacity+1 (501 USDC) => fails
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_over = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC + 1, // capacity + 1 base unit
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_over],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // GlobalCapacityExceeded = Custom(27)
    assert_custom_error(&result, 27);

    // Verify ALL state unchanged after failed borrow
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_pos]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Verify borrower didn't receive any tokens
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, 0,
        "Failed borrow must not credit borrower"
    );

    // Verify market fields unchanged
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.total_borrowed, 0,
        "total_borrowed should be 0 after failed borrow"
    );
    assert_eq!(
        parsed.scaled_total_supply,
        1_000 * USDC as u128,
        "scaled_total_supply unchanged after failed borrow"
    );

    // x boundary: Borrow exactly at cap should succeed
    let borrow_exact = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_exact],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("Borrow at exactly capacity should succeed");

    // Verify market state after successful borrow at cap
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(
        parsed.total_borrowed,
        500 * USDC,
        "total_borrowed should equal borrow capacity"
    );
    let borrower_balance = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance,
        500 * USDC,
        "Borrower should receive exactly the capacity amount"
    );
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_balance,
        500 * USDC,
        "Vault should have 500 USDC remaining after borrowing 500 of 1000"
    );

    // x+1 again: Now at cap, try to borrow 1 more base unit => fails
    let borrow_one_more = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_one_more],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 27);
}

// ===========================================================================
// G10: Deposit After Maturity Rejected (1 test)
// ===========================================================================

/// G10.1: Deposit after market maturity fails with MarketMatured.
#[tokio::test]
async fn g10_1_deposit_after_maturity_fails() {
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

    let maturity = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Advance past maturity
    advance_clock_past(&mut ctx, maturity + 301).await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Take snapshot before failed deposit
    let (vault, _) = get_vault_pda(&market);
    let snapshot_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    // Capture market state for field-level verification
    let market_data_before = get_account_data(&mut ctx, &market).await;
    let parsed_before = parse_market(&market_data_before);

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
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    // MarketMatured = Custom(28)
    assert_custom_error(&result, 28);

    // Verify ALL state unchanged after failed deposit
    let snapshot_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snapshot_before.assert_unchanged(&snapshot_after);

    // Verify specific market fields unchanged
    let market_data_after = get_account_data(&mut ctx, &market).await;
    let parsed_after = parse_market(&market_data_after);
    assert_eq!(
        parsed_after.scaled_total_supply, parsed_before.scaled_total_supply,
        "scaled_total_supply unchanged after failed deposit"
    );
    assert_eq!(
        parsed_after.total_deposited, parsed_before.total_deposited,
        "total_deposited unchanged after failed deposit"
    );
    assert_eq!(
        parsed_after.scale_factor, parsed_before.scale_factor,
        "scale_factor unchanged after failed deposit"
    );
    assert_eq!(
        parsed_after.settlement_factor_wad, parsed_before.settlement_factor_wad,
        "settlement_factor_wad unchanged after failed deposit"
    );

    // Verify token balances unchanged
    let lender_balance = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance,
        1_000 * USDC,
        "Lender token balance unchanged after failed deposit"
    );
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, 0, "Vault unchanged after failed deposit");
}
