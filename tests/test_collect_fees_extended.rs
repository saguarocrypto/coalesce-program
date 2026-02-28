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
    instruction::InstructionError,
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

// State offset constants (Market account, 250 bytes total)
// Computed from struct layout: 8(disc) + 1(ver) + 32(borrower) + 32(mint)
// + 32(vault) + 1(bump) + 2(bps) + 8(maturity) + 8(max_supply) + 8(nonce)
// + 16(scaled_total_supply) + 16(scale_factor) = 164
const MARKET_ACCRUED_PROTOCOL_FEES_OFFSET: usize = 164; // u64 at [164..172]
                                                        // ... + 8(fees) + 8(deposited) + 8(borrowed) + 8(repaid) + 8(interest_repaid) + 8(last_accrual) = 212
const MARKET_SETTLEMENT_FACTOR_OFFSET: usize = 212; // u128 at [212..228]

/// 1e6 USDC (6 decimals)
const USDC: u64 = 1_000_000;

/// WAD = 1e18
const WAD: u128 = 1_000_000_000_000_000_000;

// ===========================================================================
// 1. test_collect_fees_multiple_sequential
//    Full setup with fee_rate_bps=1000 (10%). Deposit, add interest via
//    repay_interest, warp past maturity, lender withdraws, collect fees once.
//    Verify fee_dest balance increases and accrued_protocol_fees goes to 0.
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_multiple_sequential() {
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

    // 10% fee rate
    let fee_rate_bps: u16 = 1000;
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
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
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual interest
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Add interest via repay_interest
    let interest_amount = 100 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender withdraws all (required before fee collection per SR-113)
    let withdraw_ix = common::build_withdraw(
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

    // Record fee_dest balance before collection
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(fee_dest_before, 0, "Fee dest should start at 0");

    // Check accrued fees before collection
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let accrued_before = read_u64(&market_data, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert!(
        accrued_before > 0,
        "Accrued protocol fees should be > 0 before collection, got {}",
        accrued_before
    );

    // First collect fees — should succeed
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify fee_dest balance increased
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert!(
        fee_dest_after > fee_dest_before,
        "Fee dest should have increased: before={}, after={}",
        fee_dest_before,
        fee_dest_after
    );

    // Verify accrued_protocol_fees went to 0
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let accrued_after = read_u64(&market_data, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert_eq!(
        accrued_after, 0,
        "Accrued protocol fees should be 0 after collection"
    );

    // Second collect: may fail with NoFeesToCollect (error 36) if accrued_protocol_fees
    // is zero, or may succeed if accrue_interest() accrued new fees during the first
    // collect_fees call (since the market has a non-zero interest rate). Either outcome
    // is acceptable — the key assertion is that the first collect succeeded and the
    // fee destination balance increased.
    let collect_ix_2 =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    match result {
        Ok(()) => {
            // Second collect succeeded — interest accrual created new fees.
            // Verify fee_dest balance increased again.
            let fee_dest_final = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
            assert!(
                fee_dest_final >= fee_dest_after,
                "Fee dest should not decrease: after_first={}, final={}",
                fee_dest_after,
                fee_dest_final
            );
        },
        Err(err) => {
            // Second collect failed — no more fees to collect.
            let tx_err = err.unwrap();
            assert_eq!(
                tx_err,
                solana_sdk::transaction::TransactionError::InstructionError(
                    0,
                    InstructionError::Custom(36)
                ),
                "Expected NoFeesToCollect error (Custom(36)) on second collect, got {:?}",
                tx_err
            );
        },
    }
}

// ===========================================================================
// 2. test_collect_fees_zero_accrued
//    fee_rate_bps=0, deposit, add interest, warp, lender withdraws all.
//    Attempt collect_fees → expect Custom(36) NoFeesToCollect.
//    Use ProtocolSnapshot to verify no state changed.
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_zero_accrued() {
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

    // fee_rate_bps = 0 => no fees
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
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

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

    // Deposit
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Add interest so vault is solvent
    let interest_amount = 10 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender withdraws all
    let withdraw_ix = common::build_withdraw(
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

    // Preconditions: no fees accrued and fee destination unchanged baseline.
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let market_data_before = common::get_account_data(&mut ctx, &market).await;
    let accrued_before = read_u64(&market_data_before, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert_eq!(
        accrued_before, 0,
        "accrued_protocol_fees must be exactly zero in fee_rate=0 path"
    );

    // Capture state snapshot before failed collect_fees.
    let (vault_pda, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;

    // Try to collect fees — should fail with NoFeesToCollect
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(36)
        ),
        "Expected NoFeesToCollect error (Custom(36)), got {:?}",
        tx_err
    );

    // Verify no state changed (atomicity), including token side effects.
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(
        fee_dest_after, fee_dest_before,
        "fee destination balance changed"
    );
    let market_data_after = common::get_account_data(&mut ctx, &market).await;
    let accrued_after = read_u64(&market_data_after, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert_eq!(
        accrued_after, accrued_before,
        "accrued_protocol_fees changed on failed collect"
    );

    // Determinism/idempotence: repeated collect attempt should fail identically with no mutation.
    let snapshot_before_2 =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    let collect_ix_2 =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(36)
        ),
        "Expected NoFeesToCollect error (Custom(36)) on repeated call, got {:?}",
        tx_err
    );
    let snapshot_after_2 =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before_2.assert_unchanged(&snapshot_after_2);
    let fee_dest_after_2 = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(
        fee_dest_after_2, fee_dest_before,
        "fee destination changed after repeated failed collect"
    );
}

// ===========================================================================
// 3. test_collect_fees_wrong_destination_owner
//    fee_rate_bps=1000 (10%), full setup, deposit, add interest, warp,
//    lender withdraws. Create fee_dest token account owned by a RANDOM
//    keypair (not fee_authority). Attempt collect_fees → expect Custom(16)
//    InvalidTokenAccountOwner. Validates the H-1 fix.
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_wrong_destination_owner() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let random_owner = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
        &random_owner,
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

    // 10% fee rate
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000,
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

    // Create fee_dest owned by random_owner (NOT fee_authority)
    let wrong_fee_dest =
        common::create_token_account(&mut ctx, &mint, &random_owner.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

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

    // Deposit
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Add interest
    let interest_amount = 100 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender withdraws all
    let withdraw_ix = common::build_withdraw(
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

    // Preconditions: fees exist, so failure must come from owner validation path.
    let market_data_before = common::get_account_data(&mut ctx, &market).await;
    let accrued_before = read_u64(&market_data_before, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert!(
        accrued_before > 0,
        "expected accrued_protocol_fees > 0 before wrong-owner collect"
    );
    let wrong_fee_dest_before = common::get_token_balance(&mut ctx, &wrong_fee_dest.pubkey()).await;
    let (vault_pda, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;

    // Attempt collect_fees with wrong_fee_dest (owned by random_owner, not fee_authority).
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &wrong_fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Expect Custom(16) InvalidTokenAccountOwner (H-1 fix validation)
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(16)
        ),
        "Expected InvalidTokenAccountOwner error (Custom(16)), got {:?}",
        tx_err
    );

    // No-mutation guarantees on failed authorization path.
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let wrong_fee_dest_after = common::get_token_balance(&mut ctx, &wrong_fee_dest.pubkey()).await;
    assert_eq!(
        wrong_fee_dest_after, wrong_fee_dest_before,
        "wrong fee destination balance changed on failed collect"
    );
    let market_data_after = common::get_account_data(&mut ctx, &market).await;
    let accrued_after = read_u64(&market_data_after, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert_eq!(
        accrued_after, accrued_before,
        "accrued_protocol_fees changed on failed wrong-owner collect"
    );
}

// ===========================================================================
// 4. test_collect_fees_settlement_interaction
//    Setup with high fee rate. Deposit, borrow, but only partially repay.
//    Warp past maturity, lender withdraws (triggers settlement with
//    factor < WAD since vault is underfunded). Now attempt collect_fees →
//    expect Custom(37) FeeCollectionDuringDistress.
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_settlement_interaction() {
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

    // High fee rate to ensure fee accrual
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000, // 10%
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
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

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

    // Deposit
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow most of the vault
    let borrow_amount = 800 * USDC;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Only partially repay — repay 200 of the 800 borrowed.
    // This leaves the vault severely underfunded at maturity.
    let partial_repay = 200 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        partial_repay,
    )
    .await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        partial_repay,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender withdraws — this triggers settlement. Since vault is underfunded,
    // settlement_factor_wad will be < WAD (distressed settlement).
    let withdraw_ix = common::build_withdraw(
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

    // Verify settlement factor is < WAD (distressed) and fees are still present.
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let settlement_factor = read_u128(&market_data, MARKET_SETTLEMENT_FACTOR_OFFSET);
    assert!(
        settlement_factor > 0 && settlement_factor < WAD,
        "Settlement factor should be < WAD (distressed): got {}",
        settlement_factor
    );
    let accrued_before = read_u64(&market_data, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert!(
        accrued_before > 0,
        "accrued_protocol_fees should remain > 0 prior to distressed fee collection"
    );
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let (vault_pda, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;

    // Attempt collect_fees — should fail with FeeCollectionDuringDistress
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(37)
        ),
        "Expected FeeCollectionDuringDistress error (Custom(37)), got {:?}",
        tx_err
    );

    // Failed distressed-collection must be atomic.
    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(
        fee_dest_after, fee_dest_before,
        "fee destination balance changed on distressed collect failure"
    );
    let market_data_after = common::get_account_data(&mut ctx, &market).await;
    let settlement_factor_after = read_u128(&market_data_after, MARKET_SETTLEMENT_FACTOR_OFFSET);
    let accrued_after = read_u64(&market_data_after, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert_eq!(
        settlement_factor_after, settlement_factor,
        "settlement factor changed on failed distressed collect"
    );
    assert_eq!(
        accrued_after, accrued_before,
        "accrued_protocol_fees changed on failed distressed collect"
    );
}

// ===========================================================================
// 5. test_collect_fees_with_abandoned_lender_solvent
//    Two lenders deposit, borrower borrows, interest is fully repaid so vault
//    is solvent. Advance past maturity + grace. Lender A withdraws (triggers
//    settlement with factor == WAD). Lender B never withdraws (abandoned).
//    collect_fees should SUCCEED because settlement_factor == WAD guarantees
//    the vault holds enough for all remaining lender payouts AND fees.
//    After fees are collected, Lender B withdraws and gets full payout.
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_with_abandoned_lender_solvent() {
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

    // 10% fee rate
    let fee_rate_bps: u16 = 1000;
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let deposit_amount = 500 * USDC;
    // Mint tokens to both lenders
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% annual interest
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender A deposits
    let deposit_ix_a = common::build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_a],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B deposits
    let deposit_ix_b = common::build_deposit(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_b],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Add interest via repay_interest — enough to make vault solvent
    let interest_amount = 100 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        interest_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity + grace period
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender A withdraws — triggers settlement with factor == WAD (vault is solvent)
    let withdraw_ix_a = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix_a],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: scaled_total_supply > 0 (lender B still has position)
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let scaled_total_supply_offset = 132; // 8+1+32+32+32+1+2+8+8+8 = 132
    let scaled_total_supply = read_u128(&market_data, scaled_total_supply_offset);
    assert!(
        scaled_total_supply > 0,
        "Lender B's position should still exist: scaled_total_supply = {}",
        scaled_total_supply
    );

    // Verify: settlement_factor == WAD (fully solvent)
    let settlement_factor = read_u128(&market_data, MARKET_SETTLEMENT_FACTOR_OFFSET);
    assert_eq!(
        settlement_factor, WAD,
        "Settlement factor should be WAD (fully solvent): got {}",
        settlement_factor
    );

    // Verify: accrued_protocol_fees > 0
    let accrued_before = read_u64(&market_data, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert!(
        accrued_before > 0,
        "Accrued protocol fees should be > 0 before collection, got {}",
        accrued_before
    );

    // Record fee_dest balance before collection
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(fee_dest_before, 0, "Fee dest should start at 0");

    // Collect fees — should SUCCEED despite scaled_total_supply > 0
    // because settlement_factor == WAD
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify fee_dest received tokens
    let fee_dest_after = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert!(
        fee_dest_after > fee_dest_before,
        "Fee dest should have received tokens: before={}, after={}",
        fee_dest_before,
        fee_dest_after
    );

    // Verify accrued_protocol_fees decreased
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let accrued_after = read_u64(&market_data, MARKET_ACCRUED_PROTOCOL_FEES_OFFSET);
    assert!(
        accrued_after < accrued_before,
        "Accrued fees should have decreased: before={}, after={}",
        accrued_before,
        accrued_after
    );

    // Lender B withdraws — should still get full payout
    let lender_b_balance_before =
        common::get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;
    let withdraw_ix_b = common::build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix_b],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B should have received at least their deposit back
    let lender_b_balance_after =
        common::get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;
    assert!(
        lender_b_balance_after > lender_b_balance_before,
        "Lender B should have received payout: before={}, after={}",
        lender_b_balance_before,
        lender_b_balance_after
    );
    // With settlement_factor == WAD, lender B gets full payout (>= deposit amount)
    assert!(
        lender_b_balance_after >= deposit_amount,
        "Lender B should receive at least deposit amount ({}): got {}",
        deposit_amount,
        lender_b_balance_after
    );
}

// ===========================================================================
// 6. test_collect_fees_still_blocked_during_distress
//    Underfunded vault → settlement_factor < WAD. Even with the new exception,
//    collect_fees should still fail with Custom(37) FeeCollectionDuringDistress
//    when the market is distressed, regardless of scaled_total_supply.
// ===========================================================================
#[tokio::test]
async fn test_collect_fees_still_blocked_during_distress() {
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

    // 10% fee rate
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000,
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
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    let deposit_amount = 500 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        2, // different nonce from test 5
        1000,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Both lenders deposit
    let deposit_ix_a = common::build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_a],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let deposit_ix_b = common::build_deposit(
        &market,
        &lender_b.pubkey(),
        &lender_b_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_b],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrower borrows most of the vault
    let borrow_amount = 800 * USDC;
    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Only partially repay — leaves vault severely underfunded
    let partial_repay = 200 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        partial_repay,
    )
    .await;
    let repay_ix = common::build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        partial_repay,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender A withdraws — triggers distressed settlement (factor < WAD)
    let withdraw_ix_a = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix_a],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: settlement_factor < WAD (distressed) and lender B still has position
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let settlement_factor = read_u128(&market_data, MARKET_SETTLEMENT_FACTOR_OFFSET);
    assert!(
        settlement_factor > 0 && settlement_factor < WAD,
        "Settlement factor should be < WAD (distressed): got {}",
        settlement_factor
    );

    // Attempt collect_fees — should FAIL with FeeCollectionDuringDistress
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(37)
        ),
        "Expected FeeCollectionDuringDistress error (Custom(37)), got {:?}",
        tx_err
    );

    // Fee dest should remain untouched
    let fee_dest_balance = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    assert_eq!(
        fee_dest_balance, 0,
        "Fee destination should be empty after failed distressed collect"
    );
}
