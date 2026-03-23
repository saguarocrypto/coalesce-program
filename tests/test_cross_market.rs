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
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
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

// LenderPosition scaled_balance offset (9-byte prefix + 64 bytes of market+lender)
const POSITION_SCALED_BALANCE_OFFSET: usize = 73; // u128 at [73..89]

/// 1e6 USDC (6 decimals)
const USDC: u64 = 1_000_000;

/// Build a borrow instruction that substitutes a different vault address.
/// This is used for testing PDA isolation between markets.
fn build_borrow_with_wrong_vault(
    market: &Pubkey,
    borrower: &Pubkey,
    borrower_token_account: &Pubkey,
    wrong_vault: &Pubkey,
    blacklist_program_id: &Pubkey,
    amount: u64,
) -> Instruction {
    let (market_authority, _) = common::get_market_authority_pda(market);
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(borrower);
    let (blacklist_check, _) = common::get_blacklist_pda(blacklist_program_id, borrower);
    let (protocol_config, _) = common::get_protocol_config_pda();

    let mut data = vec![4u8]; // borrow discriminator
    data.extend_from_slice(&amount.to_le_bytes());

    Instruction {
        program_id: common::program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),           // market (writable)
            AccountMeta::new_readonly(*borrower, true), // borrower (signer)
            AccountMeta::new(*borrower_token_account, false), // borrower_token_account (writable)
            AccountMeta::new(*wrong_vault, false),      // WRONG vault (writable)
            AccountMeta::new_readonly(market_authority, false), // market_authority PDA
            AccountMeta::new(borrower_whitelist, false), // borrower_whitelist PDA (writable)
            AccountMeta::new_readonly(blacklist_check, false), // blacklist_check PDA
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
            AccountMeta::new_readonly(solana_sdk::sysvar::clock::id(), false), // clock sysvar
        ],
        data,
    }
}

// ===========================================================================
// 1. test_multiple_lenders_same_market
//    Create one market. Two different lenders deposit into it.
//    Warp, both withdraw. Verify both get proportional payouts and
//    both positions can be closed.
// ===========================================================================
#[tokio::test]
async fn test_multiple_lenders_same_market() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender_a = Keypair::new();
    let lender_b = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender_a,
        &lender_b,
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
        500, // 5% fee
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
    let lender_a_token = common::create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;
    let lender_b_token = common::create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;

    // Lender A deposits 1000 USDC, lender B deposits 2000 USDC
    let deposit_a = 1_000 * USDC;
    let deposit_b = 2_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &mint_authority,
        deposit_a,
    )
    .await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &mint_authority,
        deposit_b,
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

    // Lender A deposits
    let deposit_a_ix = common::build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_a,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_a_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B deposits
    let deposit_b_ix = common::build_deposit(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_b,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_b_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read both scaled balances
    let (pos_a_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let (pos_b_pda, _) = common::get_lender_position_pda(&market, &lender_b.pubkey());
    let pos_a_data = common::get_account_data(&mut ctx, &pos_a_pda).await;
    let pos_b_data = common::get_account_data(&mut ctx, &pos_b_pda).await;
    let scaled_a = read_u128(&pos_a_data, POSITION_SCALED_BALANCE_OFFSET);
    let scaled_b = read_u128(&pos_b_data, POSITION_SCALED_BALANCE_OFFSET);
    assert!(scaled_a > 0, "Lender A should have positive scaled balance");
    assert!(scaled_b > 0, "Lender B should have positive scaled balance");

    // Lender B deposited 2x lender A, so their scaled balance should be ~2x
    // (allowing small rounding differences from interest accrual between deposits)
    let ratio = (scaled_b as f64) / (scaled_a as f64);
    assert!(
        ratio > 1.8 && ratio < 2.2,
        "Lender B scaled balance should be ~2x lender A: A={}, B={}, ratio={}",
        scaled_a,
        scaled_b,
        ratio
    );

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Record balances before withdrawal
    let balance_a_before = common::get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let balance_b_before = common::get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;

    // Lender A withdraws all
    let withdraw_a_ix = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_a_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B withdraws all
    let withdraw_b_ix = common::build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_b_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify both got payouts
    let balance_a_after = common::get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let balance_b_after = common::get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;
    let payout_a = balance_a_after - balance_a_before;
    let payout_b = balance_b_after - balance_b_before;
    assert!(payout_a > 0, "Lender A should have received payout");
    assert!(payout_b > 0, "Lender B should have received payout");

    // Payout B should be ~2x payout A (proportional to deposits)
    let payout_ratio = (payout_b as f64) / (payout_a as f64);
    assert!(
        payout_ratio > 1.8 && payout_ratio < 2.2,
        "Lender B payout should be ~2x lender A: A={}, B={}, ratio={}",
        payout_a,
        payout_b,
        payout_ratio
    );

    // Verify both positions have 0 balance
    let pos_a_data = common::get_account_data(&mut ctx, &pos_a_pda).await;
    let pos_b_data = common::get_account_data(&mut ctx, &pos_b_pda).await;
    let final_scaled_a = read_u128(&pos_a_data, POSITION_SCALED_BALANCE_OFFSET);
    let final_scaled_b = read_u128(&pos_b_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(final_scaled_a, 0, "Lender A position should be empty");
    assert_eq!(final_scaled_b, 0, "Lender B position should be empty");

    // COAL-H01: Close may fail if withdrawals created haircuts (distressed market).
    // Check haircut_owed before attempting close.
    let pos_a_parsed = common::parse_lender_position(&pos_a_data);
    let pos_b_parsed = common::parse_lender_position(&pos_b_data);

    if pos_a_parsed.haircut_owed == 0 && pos_b_parsed.haircut_owed == 0 {
        // Non-distressed — both can close
        let close_a_ix = common::build_close_lender_position(&market, &lender_a.pubkey());
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[close_a_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender_a],
            recent,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let close_b_ix = common::build_close_lender_position(&market, &lender_b.pubkey());
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[close_b_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender_b],
            recent,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    } else {
        // Distressed — close blocked by pending haircut claims, which is correct
        return;
    }

    // Verify both position accounts are closed
    let pos_a_account = ctx.banks_client.get_account(pos_a_pda).await.unwrap();
    let pos_b_account = ctx.banks_client.get_account(pos_b_pda).await.unwrap();
    match pos_a_account {
        None => { /* closed */ },
        Some(acct) => assert_eq!(acct.lamports, 0, "Lender A position should be closed"),
    }
    match pos_b_account {
        None => { /* closed */ },
        Some(acct) => assert_eq!(acct.lamports, 0, "Lender B position should be closed"),
    }
}

// ===========================================================================
// 2. test_same_borrower_different_markets
//    Create TWO markets for the SAME borrower using different nonces
//    (nonce=1 and nonce=2). Deposit into both. Borrow from market 1.
//    Verify market 2's vault is unaffected.
// ===========================================================================
#[tokio::test]
async fn test_same_borrower_different_markets() {
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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Create market 1 with nonce=1 (setup_market_full whitelists the borrower)
    let market_1 = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1, // nonce=1
        1000,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Create market 2 with nonce=2 — the borrower is already whitelisted from
    // the first setup_market_full call, so we call build_create_market directly.
    let create_market_2_ix = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2, // nonce=2
        1000,
        maturity_timestamp,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create_market_2_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (market_2, _) = common::get_market_pda(&borrower.pubkey(), 2);

    // Fund lender with enough for both deposits
    let total_deposit = 2_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        total_deposit,
    )
    .await;

    // Deposit 1000 USDC into market 1
    let deposit_1_ix = common::build_deposit(
        &market_1,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_1_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Deposit 1000 USDC into market 2
    let deposit_2_ix = common::build_deposit(
        &market_2,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_2_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Record vault 2 balance before borrow from market 1
    let (vault_2, _) = common::get_vault_pda(&market_2);
    let vault_2_before = common::get_token_balance(&mut ctx, &vault_2).await;
    assert_eq!(
        vault_2_before,
        1_000 * USDC,
        "Vault 2 should have the deposited amount"
    );

    // Borrow 500 USDC from market 1
    let borrow_ix = common::build_borrow(
        &market_1,
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

    // Verify market 1's vault decreased
    let (vault_1, _) = common::get_vault_pda(&market_1);
    let vault_1_after = common::get_token_balance(&mut ctx, &vault_1).await;
    assert_eq!(
        vault_1_after,
        500 * USDC,
        "Vault 1 should have 500 USDC after borrow of 500"
    );

    // Verify market 2's vault is UNAFFECTED
    let vault_2_after = common::get_token_balance(&mut ctx, &vault_2).await;
    assert_eq!(
        vault_2_after, vault_2_before,
        "Vault 2 should be unaffected by borrow from market 1: before={}, after={}",
        vault_2_before, vault_2_after
    );

    // Verify market 2's total_borrowed is still 0
    let market_2_data = common::get_account_data(&mut ctx, &market_2).await;
    let market_2_parsed = common::parse_market(&market_2_data);
    assert_eq!(
        market_2_parsed.total_borrowed, 0,
        "Market 2 total_borrowed should be 0"
    );
}

// ===========================================================================
// 3. test_pda_isolation_wrong_vault
//    Create two markets (nonce 1 and 2). Try to borrow from market 1 using
//    a manually crafted instruction that substitutes market 2's vault.
//    Expect Custom(12) InvalidVault.
// ===========================================================================
#[tokio::test]
async fn test_pda_isolation_wrong_vault() {
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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Create market 1 (nonce=1)
    let market_1 = common::setup_market_full(
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

    // Create market 2 (nonce=2) — borrower already whitelisted
    let create_market_2_ix = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2,
        1000,
        maturity_timestamp,
        10_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create_market_2_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (market_2, _) = common::get_market_pda(&borrower.pubkey(), 2);

    // Fund lender and deposit into market 1
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market_1,
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

    // Get market 2's vault PDA (the wrong vault for market 1)
    let (vault_2, _) = common::get_vault_pda(&market_2);

    // Try to borrow from market 1 but substitute market 2's vault
    let wrong_vault_ix = build_borrow_with_wrong_vault(
        &market_1,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &vault_2,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let (vault_1, _) = common::get_vault_pda(&market_1);
    assert_ne!(
        vault_1, vault_2,
        "cross-market isolation precondition: market 1 and market 2 must use distinct vault PDAs"
    );
    assert_eq!(
        wrong_vault_ix.accounts[3].pubkey, vault_2,
        "wrong-vault borrow must substitute market 2 vault at account index 3"
    );
    let (lender_position, _) = common::get_lender_position_pda(&market_1, &lender.pubkey());
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_token_before = common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrower_whitelist_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let lender_position_before = common::get_account_data(&mut ctx, &lender_position).await;
    let snap_market_1_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market_1, &vault_1, &[lender_position]).await;
    let snap_market_2_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market_2, &vault_2, &[]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wrong_vault_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 12); // InvalidVault

    let snap_market_1_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market_1, &vault_1, &[lender_position]).await;
    let snap_market_2_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market_2, &vault_2, &[]).await;
    snap_market_1_before.assert_unchanged(&snap_market_1_after);
    snap_market_2_before.assert_unchanged(&snap_market_2_after);
    let borrower_token_after = common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_token_after, borrower_token_before,
        "borrower token balance changed on rejected cross-market wrong-vault borrow"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &borrower_whitelist).await,
        borrower_whitelist_before,
        "borrower-whitelist bytes changed on rejected cross-market wrong-vault borrow"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before,
        "lender-position bytes changed on rejected cross-market wrong-vault borrow"
    );
}
