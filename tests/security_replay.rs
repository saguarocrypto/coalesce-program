//! P1-6: Replay and instruction reordering tests.
//!
//! These tests verify that:
//! 1. Replaying a deposit after draining the lender's balance fails
//! 2. Borrow + repay in the same transaction produces correct state
//! 3. Double-deposit in one transaction respects caps

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

use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

const USDC: u64 = 1_000_000;

/// Deposit all tokens, then try to replay the same deposit instruction.
/// Should fail because the lender has no more tokens.
#[tokio::test]
async fn test_deposit_replay_fails() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // First deposit: 1000 USDC — should succeed
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (protocol_config, _) = common::get_protocol_config_pda();
    assert_eq!(
        deposit_ix.accounts[3].pubkey, vault,
        "deposit vault account index must point to canonical vault PDA"
    );
    assert_eq!(
        deposit_ix.accounts[4].pubkey, lender_position,
        "deposit lender_position account index must point to canonical lender-position PDA"
    );
    assert_eq!(
        deposit_ix.accounts[6].pubkey, protocol_config,
        "deposit protocol_config account index must point to canonical protocol config PDA"
    );
    assert_eq!(
        deposit_ix.accounts[7].pubkey, mint,
        "deposit mint account index must point to market mint"
    );
    assert_eq!(
        deposit_ix.accounts[8].pubkey,
        spl_token::id(),
        "deposit token_program account index must point to SPL Token program"
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("first deposit should succeed");

    // Verify lender has 0 tokens left
    let balance = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(balance, 0, "lender should have 0 tokens after deposit");

    // Snapshot state after successful deposit
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Replay: same deposit instruction with fresh blockhash
    let deposit_ix_replay = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    assert_eq!(
        deposit_ix_replay.accounts[3].pubkey, vault,
        "replay deposit vault account index must point to canonical vault PDA"
    );
    assert_eq!(
        deposit_ix_replay.accounts[4].pubkey, lender_position,
        "replay deposit lender_position account index must point to canonical lender-position PDA"
    );
    assert_eq!(
        deposit_ix_replay.accounts[6].pubkey, protocol_config,
        "replay deposit protocol_config account index must point to canonical protocol config PDA"
    );
    assert_eq!(
        deposit_ix_replay.accounts[7].pubkey, mint,
        "replay deposit mint account index must point to market mint"
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx2 = Transaction::new_signed_with_payer(
        &[deposit_ix_replay],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx2)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 1); // SPL Token: InsufficientFunds

    // State must not change
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after, balance,
        "lender token balance changed on rejected replayed deposit"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender-position bytes changed on rejected replayed deposit"
    );
}

/// Borrow and repay in the same transaction. Both instructions should execute,
/// and the market state should reflect both operations.
#[tokio::test]
async fn test_borrow_repay_same_transaction() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit funds first
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
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
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create borrower token account
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Build borrow(500) and repay(500) in the SAME transaction
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );

    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC,
    );
    let (vault, _) = common::get_vault_pda(&market);
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    assert_eq!(
        borrow_ix.accounts[3].pubkey, vault,
        "borrow vault account index must point to canonical vault PDA"
    );
    assert_eq!(
        borrow_ix.accounts[5].pubkey, borrower_whitelist,
        "borrow whitelist account index must point to canonical whitelist PDA"
    );
    assert_eq!(
        borrow_ix.accounts[8].pubkey,
        spl_token::id(),
        "borrow token_program account index must point to SPL Token program"
    );
    assert_eq!(
        repay_ix.accounts[3].pubkey, vault,
        "repay vault account index must point to canonical vault PDA"
    );
    assert_eq!(
        repay_ix.accounts[5].pubkey, mint,
        "repay mint account index must point to market mint"
    );
    assert_eq!(
        repay_ix.accounts[6].pubkey, borrower_whitelist,
        "repay whitelist account index must point to canonical whitelist PDA"
    );
    assert_eq!(
        repay_ix.accounts[7].pubkey,
        spl_token::id(),
        "repay token_program account index must point to SPL Token program"
    );
    let borrower_token_before = common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let whitelist_before_data = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let whitelist_before = common::parse_borrower_whitelist(&whitelist_before_data);
    let market_before_data = common::get_account_data(&mut ctx, &market).await;
    let market_before = common::parse_market(&market_before_data);

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix, repay_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow+repay in same tx should succeed");

    // Verify vault balance is unchanged (borrow then repay = net zero)
    let vault_balance = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_balance,
        1_000 * USDC,
        "vault should be back to original balance after borrow+repay"
    );

    // Verify market state reflects both operations
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&market_data);
    assert_eq!(
        parsed.total_borrowed,
        market_before.total_borrowed + 500 * USDC,
        "total_borrowed should reflect the borrow"
    );
    assert_eq!(
        parsed.total_repaid,
        market_before.total_repaid + 500 * USDC,
        "total_repaid should reflect the repay"
    );
    assert_eq!(
        parsed.total_deposited, market_before.total_deposited,
        "borrow+repay should not change total_deposited"
    );
    assert_eq!(
        parsed.scaled_total_supply, market_before.scaled_total_supply,
        "borrow+repay should not change scaled_total_supply"
    );
    let borrower_token_after = common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance should return to the pre-transaction value"
    );
    let lender_position_after = common::get_account_data(&mut ctx, &lender_position).await;
    assert_eq!(
        lender_position_after, lender_position_before,
        "lender-position bytes changed across borrow+repay same transaction"
    );
    let whitelist_after_data = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let whitelist_after = common::parse_borrower_whitelist(&whitelist_after_data);
    assert_eq!(
        whitelist_after.current_borrowed, whitelist_before.current_borrowed,
        "borrower whitelist current_borrowed should net to zero after borrow+repay same tx"
    );
}

/// Two deposits in the same transaction. If total exceeds the cap, the second
/// instruction should fail and the entire transaction rolls back.
#[tokio::test]
async fn test_double_deposit_exceeds_cap_rolls_back() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        5_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        100,
    )
    .await;

    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    // Market with a tight cap of 1000 USDC
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        common::FAR_FUTURE_MATURITY,
        1_000 * USDC, // max_total_supply = 1000 USDC
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    // Snapshot before
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_before = common::try_get_account_data(&mut ctx, &lender_position).await;
    assert!(
        lender_position_before.is_none(),
        "double-deposit rollback precondition: lender-position PDA should not exist yet"
    );
    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Two deposit instructions in the same transaction: 600 + 600 = 1200 > 1000 cap
    let dep1 = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        600 * USDC,
    );
    let dep2 = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        600 * USDC,
    );
    assert_eq!(
        dep1.accounts[3].pubkey, vault,
        "first deposit vault account index must point to canonical vault PDA"
    );
    assert_eq!(
        dep2.accounts[3].pubkey, vault,
        "second deposit vault account index must point to canonical vault PDA"
    );
    assert_eq!(
        dep1.accounts[8].pubkey,
        spl_token::id(),
        "first deposit token_program account index must point to SPL Token program"
    );
    assert_eq!(
        dep2.accounts[8].pubkey,
        spl_token::id(),
        "second deposit token_program account index must point to SPL Token program"
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep1, dep2],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 25); // CapExceeded

    // Entire transaction should roll back — first deposit reverted too
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);

    // Lender tokens should be unchanged
    let balance = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        balance, lender_balance_before,
        "lender tokens should be unchanged after rollback"
    );
    assert_eq!(
        common::try_get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender-position lifecycle changed despite full rollback"
    );
}
