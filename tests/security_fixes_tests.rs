//! Security Fixes Tests
//!
//! Tests for the security fixes applied to the CoalesceFi protocol:
//! - SR-110: Token account ownership validation in repay
//! - SR-111: Slippage protection (min_payout) in withdraw
//! - SR-112: Settlement grace period in withdraw
//! - SR-109: Protocol config requirement in re_settle
//! - SR-113: Lender priority check in collect_fees

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

use coalesce::constants::WAD;
use common::*;
use solana_program_test::tokio;
use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

/// Error codes from the protocol (updated for category-based reorganization)
const ERR_FEE_COLLECTION_DURING_DISTRESS: u32 = 37;
const ERR_NO_BALANCE: u32 = 23;
const ERR_PAYOUT_BELOW_MINIMUM: u32 = 42;
const ERR_SETTLEMENT_GRACE_PERIOD: u32 = 32;
#[allow(dead_code)]
const ERR_LENDERS_PENDING_WITHDRAWALS: u32 = 38;

/// Settlement grace period constant (must match Rust constant)
const SETTLEMENT_GRACE_PERIOD: i64 = 300; // 5 minutes

// =============================================================================
// SR-111: Slippage Protection Tests
// =============================================================================

/// Test that withdraw fails when payout is below min_payout
#[tokio::test]
async fn test_withdraw_slippage_protection_rejects_low_payout() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    // Setup protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0, // Keep payout deterministic for min_payout boundary checks
    )
    .await;

    // Create mint and market
    let mint = create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 1000;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // No interest so expected payout equals deposit amount
        maturity,
        1_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    // Lender deposits
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 1_000_000).await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let deposit_amount = 1_000_000u64;
    let deposit_ix = build_deposit(
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

    let (vault, _) = get_vault_pda(&market);
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        0,
        "lender token balance should be 0 after depositing full amount"
    );

    // Advance past maturity + grace period.
    advance_clock_past(&mut ctx, maturity + SETTLEMENT_GRACE_PERIOD + 1).await;

    // x+1 and extreme boundary: both must fail with identical error and no mutation.
    for min_payout in [deposit_amount + 1, u64::MAX] {
        let withdraw_ix = build_withdraw(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &blacklist_program.pubkey(),
            0, // full withdrawal
            min_payout,
        );

        let snap_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
        let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[withdraw_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );
        let err = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .expect_err("withdraw should fail when min_payout exceeds deterministic payout");
        assert_eq!(
            extract_custom_error(&err),
            Some(ERR_PAYOUT_BELOW_MINIMUM),
            "expected PayoutBelowMinimum for min_payout={}",
            min_payout
        );

        let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &lender_token.pubkey()).await,
            lender_balance_before,
            "lender token balance must remain unchanged on failed withdraw"
        );
    }
}

/// Test that withdraw succeeds when payout meets min_payout
#[tokio::test]
async fn test_withdraw_slippage_protection_accepts_adequate_payout() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0, // 0% fee for simplicity
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 1000;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // 0% interest
        maturity,
        1_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let deposit_amount = 2_000_000u64;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        deposit_amount,
    )
    .await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let deposit_ix = build_deposit(
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

    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        0,
        "lender token balance should be 0 after deposit"
    );

    // Advance past maturity + grace period.
    advance_clock_past(&mut ctx, maturity + SETTLEMENT_GRACE_PERIOD + 1).await;

    let withdraw_amount = 1_000_000u64;
    let (vault, _) = get_vault_pda(&market);
    let (position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());

    // x-1 boundary: succeeds with exact payout.
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        u128::from(withdraw_amount),
        withdraw_amount - 1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );

    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        withdraw_amount,
        "first withdrawal should pay exact amount"
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert_eq!(
        parsed_market.settlement_factor_wad, WAD,
        "fully funded market should settle at WAD"
    );
    assert_eq!(
        parsed_market.scaled_total_supply,
        u128::from(withdraw_amount),
        "first withdrawal should reduce scaled_total_supply by withdrawn amount"
    );
    assert_eq!(
        get_token_balance(&mut ctx, &vault).await,
        withdraw_amount,
        "vault should contain the unwithdrawn half after first withdrawal"
    );
    let position_data = get_account_data(&mut ctx, &position_pda).await;
    let parsed_position = parse_lender_position(&position_data);
    assert_eq!(
        parsed_position.scaled_balance,
        u128::from(withdraw_amount),
        "lender position should retain remaining scaled balance"
    );

    // x boundary: succeeds when min_payout equals exact payout.
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        u128::from(withdraw_amount),
        withdraw_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        deposit_amount,
        "two successful withdrawals should fully restore lender token balance"
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert_eq!(parsed_market.scaled_total_supply, 0);
    assert_eq!(get_token_balance(&mut ctx, &vault).await, 0);
}

// =============================================================================
// SR-112: Settlement Grace Period Tests
// =============================================================================

/// Test that first withdrawal fails within grace period
#[tokio::test]
async fn test_withdraw_fails_during_grace_period() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 1000;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        1_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 1_000_000).await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // x-1 boundary: one second before grace end must fail.
    let grace_end = maturity + SETTLEMENT_GRACE_PERIOD;
    get_blockhash_pinned(&mut ctx, grace_end - 1).await;

    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0,
        0,
    );

    let (vault, _) = get_vault_pda(&market);
    let snap_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;

    let err = result.expect_err("withdraw should fail before grace period elapses");
    assert_eq!(
        extract_custom_error(&err),
        Some(ERR_SETTLEMENT_GRACE_PERIOD)
    );
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_balance_before,
        "failed withdraw during grace period must not transfer tokens"
    );

    // x boundary: exactly at grace end must succeed.
    get_blockhash_pinned(&mut ctx, grace_end).await;
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0,
        1_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        1_000_000,
        "withdraw at grace boundary should transfer full payout"
    );
}

/// Test that withdrawal succeeds after grace period
#[tokio::test]
async fn test_withdraw_succeeds_after_grace_period() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 1000;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity,
        1_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 1_000_000).await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // x+1 boundary: one second after grace end.
    let grace_end = maturity + SETTLEMENT_GRACE_PERIOD;
    get_blockhash_pinned(&mut ctx, grace_end + 1).await;

    // Withdraw should succeed now
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0,
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        1_000_000,
        "x+1 grace boundary should allow full withdrawal"
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert_eq!(parsed_market.settlement_factor_wad, WAD);
    assert_eq!(parsed_market.scaled_total_supply, 0);

    // Repeating withdraw with zero position should fail with NoBalance and no mutation.
    let (vault, _) = get_vault_pda(&market);
    get_blockhash_pinned(&mut ctx, grace_end + 2).await;
    let snap_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    // Use min_payout=1 to distinguish from the first withdraw tx and avoid tx-signature collision
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0,
        1,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err("second full withdraw should fail with no balance");
    assert_eq!(extract_custom_error(&err), Some(ERR_NO_BALANCE));
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snap_before.assert_unchanged(&snap_after);
}

// =============================================================================
// SR-057/SR-113: Fee Collection Priority Tests
// =============================================================================

/// Test that fee collection fails when market is in distress (settlement_factor < WAD)
/// This tests SR-057: Fee collection is blocked during market distress to prioritize
/// lender recovery. When settlement_factor < WAD (indicating losses), fees cannot be
/// collected until lenders have been made as whole as possible.
#[tokio::test]
async fn test_collect_fees_fails_during_distress() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
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

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000, // 10% fee
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 1000;
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual interest to generate fees
        maturity,
        1_000_000_000_000,
        &whitelist_manager,
        1_000_000_000_000,
    )
    .await;

    // Lender deposits
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 1_000_000).await;

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity + grace period and trigger settlement via withdraw
    advance_clock_past(&mut ctx, maturity + SETTLEMENT_GRACE_PERIOD + 1).await;

    // First withdrawal triggers settlement
    // Use a meaningful amount (100 tokens) that won't round to 0 payout
    // but leaves enough balance to verify the fee collection restriction
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        100_000, // withdraw 100 scaled units (enough to not round to 0)
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

    // Now market is settled but lender still has balance.
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert!(
        parsed_market.settlement_factor_wad > 0 && parsed_market.settlement_factor_wad < WAD,
        "distress path requires settlement in (0, WAD)"
    );
    let (position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let position_data = get_account_data(&mut ctx, &position_pda).await;
    let parsed_position = parse_lender_position(&position_data);
    assert!(
        parsed_position.scaled_balance > 0,
        "lender must still have balance for distress-fee block condition"
    );

    // Try to collect fees (must fail deterministically).
    let fee_token = create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;
    assert_eq!(get_token_balance(&mut ctx, &fee_token.pubkey()).await, 0);
    let (vault, _) = get_vault_pda(&market);

    for attempt in 0..2 {
        let collect_ix = build_collect_fees(&market, &fee_authority.pubkey(), &fee_token.pubkey());
        let snap_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
        let fee_balance_before = get_token_balance(&mut ctx, &fee_token.pubkey()).await;

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[collect_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &fee_authority],
            recent,
        );
        let err = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .expect_err("collect_fees should fail while settlement_factor < WAD");
        assert_eq!(
            extract_custom_error(&err),
            Some(ERR_FEE_COLLECTION_DURING_DISTRESS),
            "attempt {} should fail with FeeCollectionDuringDistress",
            attempt
        );

        let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &fee_token.pubkey()).await,
            fee_balance_before,
            "fee destination balance must not change on failed collect"
        );
    }
}
