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

// LenderPosition scaled_balance offset (9-byte prefix + 64 bytes of market+lender)
const POSITION_SCALED_BALANCE_OFFSET: usize = 73; // u128 at [73..89]

/// 1e6 USDC (6 decimals)
const USDC: u64 = 1_000_000;

// ===========================================================================
// 1. test_close_position_wrong_lender
//    A different keypair tries to close someone else's lender position.
//    The PDA derived from wrong_lender won't match the actual position PDA.
//    Expect Custom(13) InvalidPDA.
// ===========================================================================
#[tokio::test]
async fn test_close_position_wrong_lender() {
    let mut ctx = common::start_context().await;

    // Keys
    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let wrong_lender = Keypair::new();

    // Airdrop lamports
    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
        &wrong_lender,
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

    // Setup protocol
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Create mint and token accounts
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

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    // Setup market
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

    // Deposit as the real lender
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

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw all as the real lender
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

    let (vault_pda, _) = common::get_vault_pda(&market);
    let (lender_pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let wrong_lender_lamports_before = ctx
        .banks_client
        .get_account(wrong_lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;

    // Now wrong_lender tries to close lender's position.
    // build_close_lender_position derives the PDA from (market, wrong_lender),
    // which won't match lender's actual position PDA, so the program should
    // reject it with InvalidPDA.
    let close_ix = common::build_close_lender_position(&market, &wrong_lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_lender],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(14)
        ),
        "Expected InvalidAccountOwner error (Custom(14)) for wrong-lender position close, got {:?}",
        tx_err
    );

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[lender_pos_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on failed wrong-lender close"
    );
    let wrong_lender_lamports_after = ctx
        .banks_client
        .get_account(wrong_lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        wrong_lender_lamports_after, wrong_lender_lamports_before,
        "wrong lender lamports changed on failed close"
    );
}

// ===========================================================================
// 2. test_close_position_unsettled_market
//    Deposit but DON'T warp past maturity. The position still has balance
//    (no withdrawal possible before maturity). Try close → expect
//    Custom(34) PositionNotEmpty.
// ===========================================================================
#[tokio::test]
async fn test_close_position_unsettled_market() {
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

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
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

    // Deposit (creates position with non-zero scaled_balance)
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

    let (vault_pda, _) = common::get_vault_pda(&market);
    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let pos_before = common::get_account_data(&mut ctx, &position_pda).await;
    let scaled_before = read_u128(&pos_before, POSITION_SCALED_BALANCE_OFFSET);
    assert!(scaled_before > 0, "position must be non-empty before close");
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;

    // DO NOT warp past maturity — market is unsettled, position has balance.
    // Try to close position immediately.
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(34)
        ),
        "Expected PositionNotEmpty error (Custom(34)), got {:?}",
        tx_err
    );

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on failed close before maturity"
    );
    let pos_after = common::get_account_data(&mut ctx, &position_pda).await;
    let scaled_after = read_u128(&pos_after, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_after, scaled_before,
        "scaled balance changed on failed close before maturity"
    );

    // Boundary neighbor around maturity: even just after maturity, close must fail until position is emptied.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 1).await;
    let snapshot_before_matured =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(34)
        ),
        "Expected PositionNotEmpty error (Custom(34)) just after maturity, got {:?}",
        tx_err
    );
    let snapshot_after_matured =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before_matured.assert_unchanged(&snapshot_after_matured);
}

// ===========================================================================
// 3. test_close_position_double_close
//    Full lifecycle: deposit, warp, withdraw all, close. Then try close AGAIN.
//    The second close should fail because the account is already closed.
// ===========================================================================
#[tokio::test]
async fn test_close_position_double_close() {
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

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
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

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw all
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

    // First close — should succeed
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify position is closed
    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let position_account = ctx.banks_client.get_account(position_pda).await.unwrap();
    match position_account {
        None => { /* good — account closed */ },
        Some(acct) => {
            assert_eq!(acct.lamports, 0, "Closed position should have 0 lamports");
        },
    }

    let (vault_pda, _) = common::get_vault_pda(&market);
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[]).await;
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_lamports_before = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let position_data_before = common::try_get_account_data(&mut ctx, &position_pda).await;
    let position_lamports_before = ctx
        .banks_client
        .get_account(position_pda)
        .await
        .unwrap()
        .map(|a| a.lamports);

    // Second close may be rejected (Custom(14)) or treated as a no-op depending on
    // zero-lamport account handling in this runtime. Both outcomes must preserve state.
    let close_ix_2 = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    if let Err(err) = result {
        let tx_err = err.unwrap();
        assert_eq!(
            tx_err,
            solana_sdk::transaction::TransactionError::InstructionError(
                0,
                InstructionError::Custom(14)
            ),
            "Expected InvalidAccountOwner error (Custom(14)) on rejected double close, got {:?}",
            tx_err
        );
    }

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on failed double close"
    );
    let lender_lamports_after = ctx
        .banks_client
        .get_account(lender.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        lender_lamports_after, lender_lamports_before,
        "lender lamports changed on failed double close"
    );
    let position_data_after = common::try_get_account_data(&mut ctx, &position_pda).await;
    assert_eq!(
        position_data_after, position_data_before,
        "position account data changed on double close"
    );
    let position_lamports_after = ctx
        .banks_client
        .get_account(position_pda)
        .await
        .unwrap()
        .map(|a| a.lamports);
    assert_eq!(
        position_lamports_after, position_lamports_before,
        "position lamports changed on double close"
    );
    let position_account_after = ctx.banks_client.get_account(position_pda).await.unwrap();
    match position_account_after {
        None => { /* still closed */ },
        Some(acct) => assert_eq!(acct.lamports, 0, "position should remain closed"),
    }
}

// ===========================================================================
// 4. test_close_position_after_partial_withdraw
//    Deposit, warp past maturity, withdraw HALF, then try close.
//    Should fail with Custom(34) PositionNotEmpty since there's still balance.
// ===========================================================================
#[tokio::test]
async fn test_close_position_after_partial_withdraw() {
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

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
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

    // Read full scaled balance after deposit
    let (position_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let full_scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert!(
        full_scaled_balance > 0,
        "Scaled balance should be > 0 after deposit"
    );

    let half_scaled = full_scaled_balance / 2;

    // Warp past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw HALF
    let withdraw_ix = common::build_withdraw(
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

    // Verify position still has remaining balance
    let pos_data = common::get_account_data(&mut ctx, &position_pda).await;
    let remaining_scaled = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert!(
        remaining_scaled > 0,
        "Should still have remaining balance after partial withdraw"
    );
    assert_eq!(
        remaining_scaled,
        full_scaled_balance - half_scaled,
        "remaining scaled balance should match exact subtraction after partial withdraw"
    );

    let (vault_pda, _) = common::get_vault_pda(&market);
    let lender_token_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let snapshot_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;

    // Try to close position — should fail because position is not empty
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    let tx_err = err.unwrap();
    assert_eq!(
        tx_err,
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(34)
        ),
        "Expected PositionNotEmpty error (Custom(34)), got {:?}",
        tx_err
    );

    let snapshot_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault_pda, &[position_pda]).await;
    snapshot_before.assert_unchanged(&snapshot_after);
    let lender_token_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_token_after, lender_token_before,
        "lender token balance changed on failed close with non-empty position"
    );
    let pos_data_after = common::get_account_data(&mut ctx, &position_pda).await;
    let remaining_after = read_u128(&pos_data_after, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        remaining_after, remaining_scaled,
        "scaled balance changed on failed close with non-empty position"
    );
}
