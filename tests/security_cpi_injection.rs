//! P1-7: CPI injection tests — fake token program.
//!
//! These tests verify that every instruction performing SPL Token CPI
//! rejects a fake token program ID with InvalidTokenProgram (Custom(15)).
//! State must remain unchanged after each rejection.

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
    instruction::AccountMeta, pubkey::Pubkey, signature::Keypair, signer::Signer,
    transaction::Transaction,
};

const USDC: u64 = 1_000_000;

/// Helper: set up a market with a deposit and borrow already executed,
/// returning everything needed to test CPI injection on all four instructions.
struct CpiTestCtx {
    ctx: solana_program_test::ProgramTestContext,
    _admin: Keypair,
    borrower: Keypair,
    lender: Keypair,
    mint: Pubkey,
    market: Pubkey,
    lender_token: Pubkey,
    borrower_token: Pubkey,
    blacklist_program: Pubkey,
}

async fn setup_cpi_test() -> CpiTestCtx {
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
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);

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

    // Fund lender and deposit
    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        5_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Fund borrower and borrow
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    CpiTestCtx {
        ctx,
        _admin: admin,
        borrower,
        lender,
        mint,
        market,
        lender_token: lender_token_kp.pubkey(),
        borrower_token: borrower_token_kp.pubkey(),
        blacklist_program: blacklist_program.pubkey(),
    }
}

/// Deposit with a fake token program should fail with InvalidTokenProgram (15).
#[tokio::test]
async fn test_deposit_fake_token_program() {
    let mut t = setup_cpi_test().await;

    let (vault, _) = common::get_vault_pda(&t.market);
    let lender_position = common::get_lender_position_pda(&t.market, &t.lender.pubkey()).0;
    let lender_token_before = common::get_token_balance(&mut t.ctx, &t.lender_token).await;
    let lender_position_before = common::get_account_data(&mut t.ctx, &lender_position).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;

    let mut ix = common::build_deposit(
        &t.market,
        &t.lender.pubkey(),
        &t.lender_token,
        &t.mint,
        &t.blacklist_program,
        100 * USDC,
    );
    // Replace token_program (index 8) with a fake pubkey
    assert_eq!(
        ix.accounts[8].pubkey,
        spl_token::id(),
        "deposit token-program account index changed"
    );
    let fake_program = Pubkey::new_unique();
    ix.accounts[8] = AccountMeta::new_readonly(fake_program, false);

    let recent = t.ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&t.lender.pubkey()), &[&t.lender], recent);
    let result = t
        .ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 15); // InvalidTokenProgram

    let lender_token_after = common::get_token_balance(&mut t.ctx, &t.lender_token).await;
    let lender_position_after = common::get_account_data(&mut t.ctx, &lender_position).await;
    assert_eq!(
        lender_token_before, lender_token_after,
        "lender token balance changed on failed deposit with fake token program"
    );
    assert_eq!(
        lender_position_before, lender_position_after,
        "lender position changed on failed deposit with fake token program"
    );

    let snap_after =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

/// Borrow with a fake token program should fail with InvalidTokenProgram (15).
#[tokio::test]
async fn test_borrow_fake_token_program() {
    let mut t = setup_cpi_test().await;

    let (vault, _) = common::get_vault_pda(&t.market);
    let lender_position = common::get_lender_position_pda(&t.market, &t.lender.pubkey()).0;
    let borrower_whitelist = common::get_borrower_whitelist_pda(&t.borrower.pubkey()).0;
    let borrower_token_before = common::get_token_balance(&mut t.ctx, &t.borrower_token).await;
    let borrower_whitelist_before = common::get_account_data(&mut t.ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;

    let mut ix = common::build_borrow(
        &t.market,
        &t.borrower.pubkey(),
        &t.borrower_token,
        &t.blacklist_program,
        100 * USDC,
    );
    // Replace token_program (index 8) with a fake pubkey
    assert_eq!(
        ix.accounts[8].pubkey,
        spl_token::id(),
        "borrow token-program account index changed"
    );
    let fake_program = Pubkey::new_unique();
    ix.accounts[8] = AccountMeta::new_readonly(fake_program, false);

    let recent = t.ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&t.borrower.pubkey()),
        &[&t.borrower],
        recent,
    );
    let result = t
        .ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 15); // InvalidTokenProgram

    let borrower_token_after = common::get_token_balance(&mut t.ctx, &t.borrower_token).await;
    let borrower_whitelist_after = common::get_account_data(&mut t.ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_before, borrower_token_after,
        "borrower token balance changed on failed borrow with fake token program"
    );
    assert_eq!(
        borrower_whitelist_before, borrower_whitelist_after,
        "borrower whitelist changed on failed borrow with fake token program"
    );

    let snap_after =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

/// Repay with a fake token program should fail with InvalidTokenProgram (15).
#[tokio::test]
async fn test_repay_fake_token_program() {
    let mut t = setup_cpi_test().await;

    let (vault, _) = common::get_vault_pda(&t.market);
    let lender_position = common::get_lender_position_pda(&t.market, &t.lender.pubkey()).0;
    let borrower_whitelist = common::get_borrower_whitelist_pda(&t.borrower.pubkey()).0;
    let borrower_token_before = common::get_token_balance(&mut t.ctx, &t.borrower_token).await;
    let borrower_whitelist_before = common::get_account_data(&mut t.ctx, &borrower_whitelist).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;

    let mut ix = common::build_repay(
        &t.market,
        &t.borrower.pubkey(),
        &t.borrower_token,
        &t.mint,
        &t.borrower.pubkey(),
        100 * USDC,
    );
    // Replace token_program (index 7) with a fake pubkey
    assert_eq!(
        ix.accounts[7].pubkey,
        spl_token::id(),
        "repay token-program account index changed"
    );
    let fake_program = Pubkey::new_unique();
    ix.accounts[7] = AccountMeta::new_readonly(fake_program, false);

    let recent = t.ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&t.borrower.pubkey()),
        &[&t.borrower],
        recent,
    );
    let result = t
        .ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 15); // InvalidTokenProgram

    let borrower_token_after = common::get_token_balance(&mut t.ctx, &t.borrower_token).await;
    let borrower_whitelist_after = common::get_account_data(&mut t.ctx, &borrower_whitelist).await;
    assert_eq!(
        borrower_token_before, borrower_token_after,
        "borrower token balance changed on failed repay with fake token program"
    );
    assert_eq!(
        borrower_whitelist_before, borrower_whitelist_after,
        "borrower whitelist changed on failed repay with fake token program"
    );

    let snap_after =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

/// Withdraw with a fake token program should fail with InvalidTokenProgram (15).
/// Requires advancing past maturity + grace period first.
#[tokio::test]
async fn test_withdraw_fake_token_program() {
    let mut t = setup_cpi_test().await;

    // Advance clock past maturity + grace period to enable withdrawal
    common::advance_clock_past(&mut t.ctx, common::FAR_FUTURE_MATURITY + 86_400 + 10).await;

    let (vault, _) = common::get_vault_pda(&t.market);
    let lender_position = common::get_lender_position_pda(&t.market, &t.lender.pubkey()).0;
    let lender_token_before = common::get_token_balance(&mut t.ctx, &t.lender_token).await;
    let lender_position_before = common::get_account_data(&mut t.ctx, &lender_position).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;

    // Get lender's scaled balance for withdraw amount
    let position_data = common::get_account_data(&mut t.ctx, &lender_position).await;
    let scaled_balance = u128::from_le_bytes(position_data[41..57].try_into().unwrap());

    let mut ix = common::build_withdraw(
        &t.market,
        &t.lender.pubkey(),
        &t.lender_token,
        &t.blacklist_program,
        scaled_balance,
        0, // min_payout
    );
    // Replace token_program (index 8) with a fake pubkey
    assert_eq!(
        ix.accounts[8].pubkey,
        spl_token::id(),
        "withdraw token-program account index changed"
    );
    let fake_program = Pubkey::new_unique();
    ix.accounts[8] = AccountMeta::new_readonly(fake_program, false);

    let recent = t.ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&t.lender.pubkey()), &[&t.lender], recent);
    let result = t
        .ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 15); // InvalidTokenProgram

    let lender_token_after = common::get_token_balance(&mut t.ctx, &t.lender_token).await;
    let lender_position_after = common::get_account_data(&mut t.ctx, &lender_position).await;
    assert_eq!(
        lender_token_before, lender_token_after,
        "lender token balance changed on failed withdraw with fake token program"
    );
    assert_eq!(
        lender_position_before, lender_position_after,
        "lender position changed on failed withdraw with fake token program"
    );

    let snap_after =
        common::ProtocolSnapshot::capture(&mut t.ctx, &t.market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}
