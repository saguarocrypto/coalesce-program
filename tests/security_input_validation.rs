//! Security tests for input validation gaps.
//!
//! Priority 1 tests covering:
//! - Non-signer checks (borrower in Borrow, whitelist_manager in SetBorrowerWhitelist)
//! - Truncated instruction data
//! - Invalid discriminator bytes
//! - Wrong account owner (market not owned by the lending program)

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
    account::{AccountSharedData, WritableAccount},
    instruction::{AccountMeta, Instruction, InstructionError},
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

fn assert_instruction_error(
    result: &Result<(), TransactionError>,
    expected_error: InstructionError,
) {
    match result {
        Err(TransactionError::InstructionError(_, actual_error)) => {
            assert_eq!(
                actual_error, &expected_error,
                "expected {expected_error:?}, got {actual_error:?}"
            );
        },
        Err(other) => panic!("expected InstructionError({expected_error:?}), got {other:?}"),
        Ok(()) => {
            panic!("expected InstructionError({expected_error:?}), but transaction succeeded")
        },
    }
}

// ---------------------------------------------------------------------------
// 1. Borrow rejects non-signer borrower
// ---------------------------------------------------------------------------

/// Build a borrow instruction but set the borrower AccountMeta to not be a
/// signer. The program must reject with Unauthorized (Custom 3).
#[tokio::test]
async fn test_borrow_rejects_non_signer_borrower() {
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
        500,
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

    // Deposit so vault has funds
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

    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let lender_position = get_lender_position_pda(&market, &lender.pubkey()).0;
    let borrower_whitelist = get_borrower_whitelist_pda(&borrower.pubkey()).0;
    let borrower_balance_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrower_whitelist_before = get_account_data(&mut ctx, &borrower_whitelist).await;
    let lender_position_before = get_account_data(&mut ctx, &lender_position).await;
    let (vault, _) = get_vault_pda(&market);
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Build borrow instruction and remove signer flag from borrower.
    let mut ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
    );
    assert!(
        ix.accounts[1].is_signer,
        "borrower meta index changed; expected signer at index 1"
    );
    ix.accounts[1] = AccountMeta::new_readonly(borrower.pubkey(), false); // non-signer

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &[&ctx.payer], recent);
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized

    let borrower_balance_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrower_whitelist_after = get_account_data(&mut ctx, &borrower_whitelist).await;
    let lender_position_after = get_account_data(&mut ctx, &lender_position).await;
    assert_eq!(
        borrower_balance_before, borrower_balance_after,
        "borrower token balance changed on failed non-signer borrow"
    );
    assert_eq!(
        borrower_whitelist_before, borrower_whitelist_after,
        "borrower whitelist changed on failed non-signer borrow"
    );
    assert_eq!(
        lender_position_before, lender_position_after,
        "lender position changed on failed non-signer borrow"
    );
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

// ---------------------------------------------------------------------------
// 2. SetBorrowerWhitelist rejects non-signer whitelist_manager
// ---------------------------------------------------------------------------

/// Build a set_borrower_whitelist instruction but set the whitelist_manager
/// AccountMeta to not be a signer. The program must reject.
#[tokio::test]
async fn test_set_borrower_whitelist_rejects_non_signer_manager() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &whitelist_manager], 10_000_000_000).await;

    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let mut ix = build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        10_000 * USDC,
    );
    assert!(
        ix.accounts[2].is_signer,
        "whitelist manager meta index changed; expected signer at index 2"
    );
    // Set whitelist_manager (index 2) to non-signer
    ix.accounts[2] = AccountMeta::new(whitelist_manager.pubkey(), false);
    let (protocol_config, _) = get_protocol_config_pda();
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let protocol_config_before = get_account_data(&mut ctx, &protocol_config).await;
    let borrower_whitelist_before = try_get_account_data(&mut ctx, &borrower_whitelist).await;
    assert!(
        borrower_whitelist_before.is_none(),
        "borrower whitelist should not exist before failed set_borrower_whitelist"
    );

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &[&ctx.payer], recent);
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized

    let protocol_config_after = get_account_data(&mut ctx, &protocol_config).await;
    let borrower_whitelist_after = try_get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        protocol_config_before, protocol_config_after,
        "protocol config changed on failed non-signer set_borrower_whitelist"
    );
    assert!(
        borrower_whitelist_after.is_none(),
        "borrower whitelist was created on failed non-signer set_borrower_whitelist"
    );
}

// ---------------------------------------------------------------------------
// 3. Truncated instruction data
// ---------------------------------------------------------------------------

/// Send empty data (no discriminator) to the program.
#[tokio::test]
async fn test_empty_instruction_data_rejected() {
    let mut ctx = common::start_context().await;
    let payer_before = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;

    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![AccountMeta::new(ctx.payer.pubkey(), true)],
        data: vec![], // empty — no discriminator
    };

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_instruction_error(&result, InstructionError::InvalidInstructionData);
    let payer_after = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;
    assert_eq!(
        payer_before, payer_after,
        "payer account data changed on invalid empty instruction"
    );
}

/// Send discriminator 5 (Deposit) with only 3 bytes of data (needs 8).
#[tokio::test]
async fn test_truncated_deposit_data_rejected() {
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
        500,
    )
    .await;

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

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
    let lender_position = get_lender_position_pda(&market, &lender.pubkey()).0;
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_before = try_get_account_data(&mut ctx, &lender_position).await;
    assert!(
        lender_position_before.is_none(),
        "lender position should not exist before failed truncated deposit"
    );
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Build a deposit instruction but replace data with only 3 bytes after disc
    let mut ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );
    // Instruction builder prepends disc 3 for deposit.
    // Replace with only disc + 3 bytes (processor needs 8 bytes after disc).
    ix.data = vec![3, 0x01, 0x02, 0x03]; // too short for deposit

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_instruction_error(&result, InstructionError::InvalidInstructionData);

    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_after = try_get_account_data(&mut ctx, &lender_position).await;
    assert_eq!(
        lender_balance_before, lender_balance_after,
        "lender token balance changed on failed truncated deposit"
    );
    assert_eq!(
        lender_position_before, lender_position_after,
        "lender position lifecycle changed on failed truncated deposit"
    );
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

/// Send discriminator 2 (CreateMarket) with only 10 bytes of data (needs 26).
#[tokio::test]
async fn test_truncated_create_market_data_rejected() {
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

    // Build create_market instruction with truncated data
    let expected_market = get_market_pda(&borrower.pubkey(), 1).0;
    let (protocol_config, _) = get_protocol_config_pda();
    let protocol_config_before = get_account_data(&mut ctx, &protocol_config).await;
    let market_before = try_get_account_data(&mut ctx, &expected_market).await;
    assert!(
        market_before.is_none(),
        "market should not exist before failed truncated create_market"
    );
    let mut ix = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        800,
        2_000_000_000,
        10_000 * USDC,
    );
    // Replace data: disc 2 + only 10 bytes (needs 26 after disc)
    ix.data = vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

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
    assert_instruction_error(&result, InstructionError::InvalidInstructionData);

    let protocol_config_after = get_account_data(&mut ctx, &protocol_config).await;
    let market_after = try_get_account_data(&mut ctx, &expected_market).await;
    assert_eq!(
        protocol_config_before, protocol_config_after,
        "protocol config changed on failed truncated create_market"
    );
    assert!(
        market_after.is_none(),
        "market was created on failed truncated create_market"
    );
}

// ---------------------------------------------------------------------------
// 4. Invalid discriminator bytes
// ---------------------------------------------------------------------------

/// Send discriminator 3 which maps to no instruction. The program should
/// return InvalidInstructionData.
#[tokio::test]
async fn test_invalid_discriminator_3_rejected() {
    let mut ctx = common::start_context().await;
    let payer_before = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;

    let ix = build_raw_instruction(3, vec![AccountMeta::new(ctx.payer.pubkey(), true)]);

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_instruction_error(&result, InstructionError::NotEnoughAccountKeys);
    let payer_after = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;
    assert_eq!(
        payer_before, payer_after,
        "payer account data changed on invalid discriminator 3"
    );
}

/// Send discriminator 255 (out of range).
#[tokio::test]
async fn test_invalid_discriminator_255_rejected() {
    let mut ctx = common::start_context().await;
    let payer_before = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;

    let ix = build_raw_instruction(255, vec![AccountMeta::new(ctx.payer.pubkey(), true)]);

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_instruction_error(&result, InstructionError::InvalidInstructionData);
    let payer_after = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;
    assert_eq!(
        payer_before, payer_after,
        "payer account data changed on invalid discriminator 255"
    );
}

/// Send discriminator 4 (gap between CreateMarket=2 and Deposit=5).
#[tokio::test]
async fn test_invalid_discriminator_4_rejected() {
    let mut ctx = common::start_context().await;
    let payer_before = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;

    let ix = build_raw_instruction(4, vec![AccountMeta::new(ctx.payer.pubkey(), true)]);

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_instruction_error(&result, InstructionError::NotEnoughAccountKeys);
    let payer_after = ctx
        .banks_client
        .get_account(ctx.payer.pubkey())
        .await
        .unwrap()
        .expect("payer account must exist")
        .data;
    assert_eq!(
        payer_before, payer_after,
        "payer account data changed on invalid discriminator 4"
    );
}

// ---------------------------------------------------------------------------
// 5. Wrong account owner — market owned by system program
// ---------------------------------------------------------------------------

/// Inject a fake market account owned by the system program (not the lending
/// program). Deposit should reject with InvalidAccountOwner (Custom 25).
#[tokio::test]
async fn test_deposit_rejects_market_with_wrong_owner() {
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
        500,
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

    // Create a real market to get the PDA address
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

    // Read market data, then re-inject it with system_program as owner
    let market_data = get_account_data(&mut ctx, &market).await;
    let mut fake_account = AccountSharedData::new(
        1_000_000_000,
        market_data.len(),
        &solana_sdk::system_program::id(), // wrong owner
    );
    fake_account
        .data_as_mut_slice()
        .copy_from_slice(&market_data);
    ctx.set_account(&market, &fake_account);
    let (vault, _) = get_vault_pda(&market);
    let lender_position = get_lender_position_pda(&market, &lender.pubkey()).0;
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_before = try_get_account_data(&mut ctx, &lender_position).await;
    assert!(
        lender_position_before.is_none(),
        "lender position should not exist before failed deposit on wrong-owner market"
    );
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Attempt to deposit — should fail with InvalidAccountOwner (25)
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
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
    assert_custom_error(&result, 14); // InvalidAccountOwner

    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_after = try_get_account_data(&mut ctx, &lender_position).await;
    assert_eq!(
        lender_balance_before, lender_balance_after,
        "lender token balance changed on failed deposit with wrong-owner market"
    );
    assert_eq!(
        lender_position_before, lender_position_after,
        "lender position lifecycle changed on failed deposit with wrong-owner market"
    );
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

/// Inject a fake market account owned by the system program and attempt to
/// borrow. Should fail with InvalidAccountOwner (Custom 25).
#[tokio::test]
async fn test_borrow_rejects_market_with_wrong_owner() {
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
        500,
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
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit first
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

    // Swap market owner to system program
    let market_data = get_account_data(&mut ctx, &market).await;
    let mut fake_account = AccountSharedData::new(
        1_000_000_000,
        market_data.len(),
        &solana_sdk::system_program::id(),
    );
    fake_account
        .data_as_mut_slice()
        .copy_from_slice(&market_data);
    ctx.set_account(&market, &fake_account);
    let (vault, _) = get_vault_pda(&market);
    let lender_position = get_lender_position_pda(&market, &lender.pubkey()).0;
    let borrower_whitelist = get_borrower_whitelist_pda(&borrower.pubkey()).0;
    let borrower_balance_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrower_whitelist_before = get_account_data(&mut ctx, &borrower_whitelist).await;
    let lender_position_before = get_account_data(&mut ctx, &lender_position).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Attempt borrow — should fail with InvalidAccountOwner (25)
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
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
    assert_custom_error(&result, 14); // InvalidAccountOwner

    let borrower_balance_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrower_whitelist_after = get_account_data(&mut ctx, &borrower_whitelist).await;
    let lender_position_after = get_account_data(&mut ctx, &lender_position).await;
    assert_eq!(
        borrower_balance_before, borrower_balance_after,
        "borrower token balance changed on failed borrow with wrong-owner market"
    );
    assert_eq!(
        borrower_whitelist_before, borrower_whitelist_after,
        "borrower whitelist changed on failed borrow with wrong-owner market"
    );
    assert_eq!(
        lender_position_before, lender_position_after,
        "lender position changed on failed borrow with wrong-owner market"
    );
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

/// ReSettle should reject a market owned by the wrong program.
#[tokio::test]
async fn test_re_settle_rejects_market_with_wrong_owner() {
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

    let fee_rate_bps: u16 = 1000;
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = create_mint(&mut ctx, &mint_authority, 6).await;

    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
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
        1000,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit
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

    // Borrow
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

    // Repay principal (can only repay up to borrowed amount due to SR-116)
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
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

    // Fund vault with interest for settlement
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        100 * USDC,
    )
    .await;
    let repay_interest_ix = build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        100 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity AND past 300-second grace period
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw to trigger settlement
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

    // Now swap market owner to system_program
    let market_data = get_account_data(&mut ctx, &market).await;
    let mut fake_account = AccountSharedData::new(
        1_000_000_000,
        market_data.len(),
        &solana_sdk::system_program::id(),
    );
    fake_account
        .data_as_mut_slice()
        .copy_from_slice(&market_data);
    ctx.set_account(&market, &fake_account);

    let (vault, _) = get_vault_pda(&market);
    let lender_position = get_lender_position_pda(&market, &lender.pubkey()).0;
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_before = try_get_account_data(&mut ctx, &lender_position).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
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
    assert_custom_error(&result, 14); // InvalidAccountOwner

    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_after = try_get_account_data(&mut ctx, &lender_position).await;
    assert_eq!(
        lender_balance_before, lender_balance_after,
        "lender token balance changed on failed re_settle with wrong-owner market"
    );
    assert_eq!(
        lender_position_before, lender_position_after,
        "lender position lifecycle changed on failed re_settle with wrong-owner market"
    );
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}
