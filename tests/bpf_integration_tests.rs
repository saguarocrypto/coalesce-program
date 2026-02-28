//! End-to-end BPF integration tests for the CoalesceFi Pinocchio program.
//!
//! These tests exercise the full instruction lifecycle through the Solana BPF
//! runtime, focusing on aspects invisible to native math tests:
//!
//! 1. Account ownership validation
//! 2. PDA derivation correctness
//! 3. Signer validation
//! 4. Instruction data deserialization
//! 5. Rent-exempt account handling
//! 6. Token transfer integration
//! 7. Full lifecycle through BPF
//! 8. CPI guard behavior

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
use solana_sdk::{
    account::{AccountSharedData, WritableAccount},
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program,
    transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

/// WAD = 1e18, the fixed-point precision constant.
const WAD: u128 = 1_000_000_000_000_000_000;

fn assert_banks_custom_error(
    err: &solana_program_test::BanksClientError,
    expected: u32,
    context: &str,
) {
    assert_eq!(
        extract_custom_error(err),
        Some(expected),
        "Expected Custom({expected}) for {context}, got: {err:?}"
    );
}

fn assert_invalid_instruction_data(err: &solana_program_test::BanksClientError, context: &str) {
    match err {
        solana_program_test::BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(
                _,
                solana_sdk::instruction::InstructionError::InvalidInstructionData,
            ),
        ) => {},
        other => panic!("Expected InvalidInstructionData for {context}, got: {other:?}"),
    }
}

// ===========================================================================
// 1. Account Ownership Validation (3 tests)
// ===========================================================================

/// 1a. Deposit with market account owned by a different program should fail
/// with InvalidAccountOwner (Custom(14)).
#[tokio::test]
async fn test_ownership_wrong_program_owned_account() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
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

    // Overwrite the market account so it is owned by a DIFFERENT program
    let fake_owner = Pubkey::new_unique();
    let market_data = get_account_data(&mut ctx, &market).await;
    let mut fake_account = AccountSharedData::new(1_000_000_000, market_data.len(), &fake_owner);
    fake_account
        .data_as_mut_slice()
        .copy_from_slice(&market_data);
    ctx.set_account(&market, &fake_account);

    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let lender_tokens_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // x/x+1 neighbor amounts should both fail before touching state.
    for amount in [1u64, 100 * USDC] {
        let deposit_ix = build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
        );

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 14, "deposit with wrong-owner market account");

        let snap_after =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &lender_token.pubkey()).await,
            lender_tokens_before
        );
        assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);
    }
    assert!(
        ctx.banks_client
            .get_account(lender_position)
            .await
            .unwrap()
            .is_none(),
        "lender position should not be created on ownership failure"
    );
}

/// 1b. Borrow with market account owned by system program should fail
/// with InvalidAccountOwner (Custom(14)).
#[tokio::test]
async fn test_ownership_system_owned_where_program_expected() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit first so vault has funds
    let lender = Keypair::new();
    airdrop_multiple(&mut ctx, &[&lender], 10_000_000_000).await;
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

    // Overwrite the market account to be owned by system_program
    let market_data = get_account_data(&mut ctx, &market).await;
    let mut system_owned_account =
        AccountSharedData::new(1_000_000_000, market_data.len(), &system_program::id());
    system_owned_account
        .data_as_mut_slice()
        .copy_from_slice(&market_data);
    ctx.set_account(&market, &system_owned_account);

    // Attempt borrow with system-owned market account.
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = get_vault_pda(&market);
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let borrower_tokens_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let whitelist_before = get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    // x/x+1 neighbor amounts should both fail before any side effects.
    for amount in [1u64, 100 * USDC] {
        let borrow_ix = build_borrow(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &blacklist_program.pubkey(),
            amount,
        );

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[borrow_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 14, "borrow with system-owned market account");

        let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_account_data(&mut ctx, &borrower_whitelist).await,
            whitelist_before
        );
        assert_eq!(
            get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
            borrower_tokens_before
        );
    }
}

/// 1c. Deposit with correctly-owned market account succeeds.
#[tokio::test]
async fn test_ownership_correct_owner_succeeds() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
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
        2_000 * USDC,
    )
    .await;

    // x-1/x/x+1 neighbor deposits with properly-owned accounts all succeed.
    let mut deposited_total = 0u64;
    for amount in [499u64 * USDC, 500u64 * USDC, 501u64 * USDC] {
        let deposit_ix = build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
        );

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );

        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("deposit with correct ownership should succeed");
        deposited_total = deposited_total.checked_add(amount).unwrap();
    }

    let (vault, _) = get_vault_pda(&market);
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, deposited_total);
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        2_000 * USDC - deposited_total
    );

    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert_eq!(parsed.total_deposited, deposited_total);
}

// ===========================================================================
// 2. PDA Derivation Correctness (3 tests)
// ===========================================================================

/// 2a. Market PDA derived with correct seeds succeeds (CreateMarket).
#[tokio::test]
async fn test_pda_market_correct_seeds_succeeds() {
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

    // x-1/x/x+1 nonce neighbors all derive and initialize the expected PDA.
    let mut markets = Vec::new();
    for nonce in [41u64, 42u64, 43u64] {
        let market = setup_market_full(
            &mut ctx,
            &admin,
            &borrower,
            &mint,
            &blacklist_program.pubkey(),
            nonce,
            500,
            common::FAR_FUTURE_MATURITY,
            10_000 * USDC,
            &whitelist_manager,
            10_000 * USDC,
        )
        .await;

        let (expected_market_pda, _) = get_market_pda(&borrower.pubkey(), nonce);
        assert_eq!(
            market, expected_market_pda,
            "market PDA should match expected derivation for nonce={nonce}"
        );

        let market_data = get_account_data(&mut ctx, &market).await;
        let parsed = parse_market(&market_data);
        assert_eq!(parsed.market_nonce, nonce);
        assert_eq!(&parsed.borrower, borrower.pubkey().as_ref());

        markets.push(market);
    }
    assert_ne!(markets[0], markets[1]);
    assert_ne!(markets[1], markets[2]);
    assert_ne!(markets[0], markets[2]);
}

/// 2b. LenderPosition PDA with wrong lender key should fail on deposit
/// by a different lender trying to use a pre-existing position.
/// We construct a deposit instruction that points to the wrong position PDA.
#[tokio::test]
async fn test_pda_lender_position_wrong_lender_fails() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender_b_token = create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_b_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Manually construct a deposit instruction for lender_b but with lender_a's position PDA
    let (vault, _) = get_vault_pda(&market);
    let (wrong_position, _) = get_lender_position_pda(&market, &lender_a.pubkey()); // wrong lender
    let (blacklist_check, _) = get_blacklist_pda(&blacklist_program.pubkey(), &lender_b.pubkey());
    let (protocol_config, _) = get_protocol_config_pda();

    let lender_b_tokens_before = get_token_balance(&mut ctx, &lender_b_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let snap_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[wrong_position]).await;

    // x/x+1 neighbor amounts should both fail with InvalidPDA and no mutation.
    for amount in [1u64, 500u64 * USDC] {
        let mut data = vec![3u8]; // deposit discriminator
        data.extend_from_slice(&amount.to_le_bytes());

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(market, false),
                AccountMeta::new(lender_b.pubkey(), true),
                AccountMeta::new(lender_b_token.pubkey(), false),
                AccountMeta::new(vault, false),
                AccountMeta::new(wrong_position, false), // wrong PDA for lender_b
                AccountMeta::new_readonly(blacklist_check, false),
                AccountMeta::new_readonly(protocol_config, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender_b],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 13, "deposit with wrong lender-position PDA");

        let snap_after =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[wrong_position]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &lender_b_token.pubkey()).await,
            lender_b_tokens_before
        );
        assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);
    }
    assert!(
        ctx.banks_client
            .get_account(wrong_position)
            .await
            .unwrap()
            .is_none(),
        "wrong-position account should not be created on InvalidPDA"
    );
}

/// 2c. ProtocolConfig PDA validation -- passing a wrong account address
/// for protocol_config in deposit should fail with InvalidPDA (Custom(13)).
#[tokio::test]
async fn test_pda_protocol_config_validation() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
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

    // Manually build a deposit instruction with a WRONG protocol_config address
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let (blacklist_check, _) = get_blacklist_pda(&blacklist_program.pubkey(), &lender.pubkey());
    let wrong_protocol_config = Pubkey::new_unique(); // definitely not the PDA

    let lender_tokens_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // x/x+1 neighbor amounts should both fail with InvalidPDA and no mutation.
    for amount in [1u64, 100u64 * USDC] {
        let mut data = vec![3u8]; // deposit discriminator
        data.extend_from_slice(&amount.to_le_bytes());

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(market, false),
                AccountMeta::new(lender.pubkey(), true),
                AccountMeta::new(lender_token.pubkey(), false),
                AccountMeta::new(vault, false),
                AccountMeta::new(lender_position, false),
                AccountMeta::new_readonly(blacklist_check, false),
                AccountMeta::new_readonly(wrong_protocol_config, false), // wrong
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 13, "deposit with wrong protocol_config PDA");

        let snap_after =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &lender_token.pubkey()).await,
            lender_tokens_before
        );
        assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);
    }
}

// ===========================================================================
// 3. Signer Validation (3 tests)
// ===========================================================================

/// 3a. Deposit without lender signature should fail with Unauthorized (Custom(5)).
#[tokio::test]
async fn test_signer_deposit_without_lender_signature() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
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

    // Manually build deposit with lender NOT marked as signer
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let (blacklist_check, _) = get_blacklist_pda(&blacklist_program.pubkey(), &lender.pubkey());
    let (protocol_config, _) = get_protocol_config_pda();

    let lender_tokens_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // x/x+1 neighbor amounts should fail when lender is not marked signer.
    for amount in [1u64, 100u64 * USDC] {
        let mut data = vec![3u8]; // deposit discriminator
        data.extend_from_slice(&amount.to_le_bytes());

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(market, false),
                AccountMeta::new(lender.pubkey(), false), // NOT a signer
                AccountMeta::new(lender_token.pubkey(), false),
                AccountMeta::new(vault, false),
                AccountMeta::new(lender_position, false),
                AccountMeta::new_readonly(blacklist_check, false),
                AccountMeta::new_readonly(protocol_config, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 5, "deposit without lender signature");

        let snap_after =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &lender_token.pubkey()).await,
            lender_tokens_before
        );
        assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);
    }

    // Neighbor success control: the same flow succeeds with lender marked as signer.
    let ok_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let ok_tx = Transaction::new_signed_with_payer(
        &[ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client.process_transaction(ok_tx).await.unwrap();
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_tokens_before - 1
    );
    assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before + 1);
}

/// 3b. Borrow without borrower signature should fail with Unauthorized (Custom(5)).
#[tokio::test]
async fn test_signer_borrow_without_borrower_signature() {
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
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit so vault has funds
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

    // Manually build borrow instruction with borrower NOT marked as signer
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = get_vault_pda(&market);
    let (market_authority, _) = get_market_authority_pda(&market);
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let (blacklist_check, _) = get_blacklist_pda(&blacklist_program.pubkey(), &borrower.pubkey());
    let (protocol_config, _) = get_protocol_config_pda();

    let borrower_tokens_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let whitelist_before = get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    // x/x+1 neighbor amounts fail when borrower is not marked signer.
    for amount in [1u64, 100u64 * USDC] {
        let mut data = vec![4u8]; // borrow discriminator
        data.extend_from_slice(&amount.to_le_bytes());

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(market, false),
                AccountMeta::new_readonly(borrower.pubkey(), false), // NOT a signer
                AccountMeta::new(borrower_token.pubkey(), false),
                AccountMeta::new(vault, false),
                AccountMeta::new_readonly(market_authority, false),
                AccountMeta::new(borrower_whitelist, false),
                AccountMeta::new_readonly(blacklist_check, false),
                AccountMeta::new_readonly(protocol_config, false),
                AccountMeta::new_readonly(spl_token::id(), false),
            ],
            data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 5, "borrow without borrower signature");

        let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
            borrower_tokens_before
        );
        assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);
        assert_eq!(
            get_account_data(&mut ctx, &borrower_whitelist).await,
            whitelist_before
        );
    }

    // Neighbor success control: same borrow succeeds when borrower signs.
    let ok_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );
    let ok_tx = Transaction::new_signed_with_payer(
        &[ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client.process_transaction(ok_tx).await.unwrap();
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_tokens_before + 1
    );
    assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before - 1);
}

/// 3c. Admin operation (SetFeeConfig) without admin signature should fail
/// with Unauthorized (Custom(5)).
#[tokio::test]
async fn test_signer_admin_operation_without_admin_signature() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Build SetFeeConfig manually with admin NOT marked as signer
    let (protocol_config, _) = get_protocol_config_pda();
    let new_fee_authority = Keypair::new();

    let config_before = get_account_data(&mut ctx, &protocol_config).await;

    // x-1/x/x+1 fee neighbors all fail when admin is not marked signer.
    for fee_bps in [999u16, 1000u16, 1001u16] {
        let mut data = vec![1u8]; // SetFeeConfig discriminator
        data.extend_from_slice(&fee_bps.to_le_bytes());

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(protocol_config, false),
                AccountMeta::new_readonly(admin.pubkey(), false), // NOT a signer
                AccountMeta::new_readonly(new_fee_authority.pubkey(), false),
            ],
            data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 5, "set_fee_config without admin signature");
        assert_eq!(
            get_account_data(&mut ctx, &protocol_config).await,
            config_before,
            "protocol config mutated on unauthorized set_fee_config"
        );
    }

    // Neighbor success control: same operation succeeds with admin signer.
    let ok_ix = build_set_fee_config(&admin.pubkey(), &new_fee_authority.pubkey(), 1001);
    let ok_tx = Transaction::new_signed_with_payer(
        &[ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client.process_transaction(ok_tx).await.unwrap();
    let config_after = get_account_data(&mut ctx, &protocol_config).await;
    let parsed_after = parse_protocol_config(&config_after);
    assert_eq!(parsed_after.fee_rate_bps, 1001);
    assert_eq!(
        parsed_after.fee_authority,
        *new_fee_authority.pubkey().as_ref()
    );
}

// ===========================================================================
// 4. Instruction Data Deserialization (3 tests)
// ===========================================================================

/// 4a. Valid instruction data for each core instruction type succeeds.
/// We test InitializeProtocol, CreateMarket, Deposit as representative.
#[tokio::test]
async fn test_deser_valid_instruction_data() {
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

    // Inject fake program_data account for upgrade authority verification
    let (program_data_pda, _) = Pubkey::find_program_address(
        &[program_id().as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    let mut program_data = vec![0u8; 45];
    program_data[0..4].copy_from_slice(&3u32.to_le_bytes()); // type = ProgramData
    program_data[4..12].copy_from_slice(&0u64.to_le_bytes()); // slot
    program_data[12] = 1; // option = Some
    program_data[13..45].copy_from_slice(admin.pubkey().as_ref()); // upgrade authority
    let mut program_data_account = AccountSharedData::new(
        1_000_000_000,
        program_data.len(),
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    program_data_account
        .data_as_mut_slice()
        .copy_from_slice(&program_data);
    ctx.set_account(&program_data_pda, &program_data_account);

    // InitializeProtocol with valid data
    let ix = build_initialize_protocol(
        &admin.pubkey(),
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("InitializeProtocol with valid data should succeed");

    // CreateMarket with valid data
    let mint = create_mint(&mut ctx, &admin, 6).await;
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

    // Deposit with valid data
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    let mut deposited_total = 0u64;
    for amount in [499u64 * USDC, 500u64 * USDC, 501u64 * USDC] {
        let deposit_ix = build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
        );
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("Deposit with valid data should succeed");
        deposited_total = deposited_total.checked_add(amount).unwrap();
    }

    // Verify state updated correctly
    let (vault, _) = get_vault_pda(&market);
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, deposited_total);
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        2_000 * USDC - deposited_total
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert_eq!(parsed_market.total_deposited, deposited_total);
}

/// 4b. Truncated instruction data should fail gracefully
/// (InvalidInstructionData from the runtime).
#[tokio::test]
async fn test_deser_truncated_instruction_data() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
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

    // Build deposit instruction with truncated data (only discriminator, missing amount bytes)
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let (blacklist_check, _) = get_blacklist_pda(&blacklist_program.pubkey(), &lender.pubkey());
    let (protocol_config, _) = get_protocol_config_pda();

    let lender_tokens_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // x-1 and x-2 payload-length neighbors (len=8, len=1) should fail.
    for payload_len in [8usize, 1usize] {
        let mut truncated_data = vec![3u8];
        truncated_data.extend(vec![0u8; payload_len.saturating_sub(1)]);

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(market, false),
                AccountMeta::new(lender.pubkey(), true),
                AccountMeta::new(lender_token.pubkey(), false),
                AccountMeta::new(vault, false),
                AccountMeta::new(lender_position, false),
                AccountMeta::new_readonly(blacklist_check, false),
                AccountMeta::new_readonly(protocol_config, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: truncated_data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_invalid_instruction_data(&err, "truncated deposit instruction data");

        let snap_after =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &lender_token.pubkey()).await,
            lender_tokens_before
        );
        assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);
    }

    // x+1 control: valid 9-byte payload succeeds.
    let ok_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let ok_tx = Transaction::new_signed_with_payer(
        &[ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client.process_transaction(ok_tx).await.unwrap();
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_tokens_before - 1
    );
    assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before + 1);
}

/// 4c. Extra trailing bytes in instruction data should be ignored or fail.
/// The deposit processor reads exactly 8 bytes for the amount. Extra bytes
/// after the valid amount should be silently ignored.
#[tokio::test]
async fn test_deser_extra_trailing_bytes() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
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

    // Build deposit instruction with valid amount + extra trailing bytes
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let (blacklist_check, _) = get_blacklist_pda(&blacklist_program.pubkey(), &lender.pubkey());
    let (protocol_config, _) = get_protocol_config_pda();

    let lender_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let mut deposited_total = 0u64;

    // x-1/x/x+1 trailing-byte neighbors should all succeed and ignore trailing bytes.
    for (amount, extra_len) in [
        (200u64 * USDC, 0usize),
        (201u64 * USDC, 1usize),
        (202u64 * USDC, 8usize),
    ] {
        let mut data_with_extra = vec![3u8]; // deposit discriminator
        data_with_extra.extend_from_slice(&amount.to_le_bytes());
        data_with_extra.extend(vec![0xAB; extra_len]);

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(market, false),
                AccountMeta::new(lender.pubkey(), true),
                AccountMeta::new(lender_token.pubkey(), false),
                AccountMeta::new(vault, false),
                AccountMeta::new(lender_position, false),
                AccountMeta::new_readonly(blacklist_check, false),
                AccountMeta::new_readonly(protocol_config, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: data_with_extra,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );

        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("deposit with extra trailing bytes should succeed");
        deposited_total = deposited_total.checked_add(amount).unwrap();
    }

    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, deposited_total);
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_before - deposited_total
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert_eq!(parsed_market.total_deposited, deposited_total);
}

// ===========================================================================
// 5. Rent-Exempt Account Handling (2 tests)
// ===========================================================================

/// 5a. Creating accounts (market, lender position) with exact minimum
/// rent-exempt balance. Verify they are rent-exempt.
#[tokio::test]
async fn test_rent_exact_minimum_balance() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let assert_rent_boundary = |name: &str, lamports: u64, data_len: usize| {
        let min = rent.minimum_balance(data_len);
        assert!(
            !rent.is_exempt(min.saturating_sub(1), data_len),
            "{name}: x-1 should not be rent exempt"
        );
        assert!(
            rent.is_exempt(min, data_len),
            "{name}: x should be rent exempt"
        );
        assert!(
            rent.is_exempt(min.saturating_add(1), data_len),
            "{name}: x+1 should be rent exempt"
        );
        assert!(
            lamports >= min,
            "{name}: actual lamports ({lamports}) should be >= minimum ({min})"
        );
    };

    // Verify market account rent boundaries.
    let market_account = ctx
        .banks_client
        .get_account(market)
        .await
        .unwrap()
        .expect("market account should exist");
    assert_rent_boundary("market", market_account.lamports, market_account.data.len());

    // Deposit to create lender position
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;
    let (vault, _) = get_vault_pda(&market);
    let lender_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_before - 500 * USDC
    );
    assert_eq!(
        get_token_balance(&mut ctx, &vault).await,
        vault_before + 500 * USDC
    );

    // Verify lender position account is rent-exempt
    let (lender_pos_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let pos_account = ctx
        .banks_client
        .get_account(lender_pos_pda)
        .await
        .unwrap()
        .expect("lender position should exist");
    assert_rent_boundary(
        "lender position",
        pos_account.lamports,
        pos_account.data.len(),
    );

    // Verify protocol config is rent-exempt
    let (config_pda, _) = get_protocol_config_pda();
    let config_account = ctx
        .banks_client
        .get_account(config_pda)
        .await
        .unwrap()
        .expect("protocol config should exist");
    assert_rent_boundary(
        "protocol config",
        config_account.lamports,
        config_account.data.len(),
    );
}

/// 5b. Verify accounts remain rent-exempt after state updates (deposit, borrow).
#[tokio::test]
async fn test_rent_remains_exempt_after_state_updates() {
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
        100,
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
        500,
        common::FAR_FUTURE_MATURITY,
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
        2_000 * USDC,
    )
    .await;

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let (lender_pos_pda, _) = get_lender_position_pda(&market, &lender.pubkey());

    // x-1 deposit neighbor
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        499 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let market_after_first = ctx.banks_client.get_account(market).await.unwrap().unwrap();
    assert!(
        rent.is_exempt(market_after_first.lamports, market_after_first.data.len()),
        "market should remain rent-exempt after first deposit"
    );
    let pos_after_first = ctx
        .banks_client
        .get_account(lender_pos_pda)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rent.is_exempt(pos_after_first.lamports, pos_after_first.data.len()),
        "lender position should remain rent-exempt after first deposit"
    );

    // x deposit neighbor
    let deposit_ix_2 = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let market_after_second = ctx.banks_client.get_account(market).await.unwrap().unwrap();
    assert!(
        rent.is_exempt(market_after_second.lamports, market_after_second.data.len()),
        "market should remain rent-exempt after second deposit"
    );
    let pos_after_second = ctx
        .banks_client
        .get_account(lender_pos_pda)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rent.is_exempt(pos_after_second.lamports, pos_after_second.data.len()),
        "lender position should remain rent-exempt after second deposit"
    );

    // x+1 deposit neighbor
    let deposit_ix_3 = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        501 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix_3],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let market_after_third = ctx.banks_client.get_account(market).await.unwrap().unwrap();
    assert!(
        rent.is_exempt(market_after_third.lamports, market_after_third.data.len()),
        "market should remain rent-exempt after third deposit"
    );
    let pos_after_third = ctx
        .banks_client
        .get_account(lender_pos_pda)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rent.is_exempt(pos_after_third.lamports, pos_after_third.data.len()),
        "lender position should remain rent-exempt after third deposit"
    );

    // Borrow (further updates market state)
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = get_vault_pda(&market);
    let vault_before_borrow = get_token_balance(&mut ctx, &vault).await;
    let borrower_before_borrow = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        300 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    assert_eq!(
        get_token_balance(&mut ctx, &vault).await,
        vault_before_borrow - 300 * USDC
    );
    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        borrower_before_borrow + 300 * USDC
    );

    let market_account = ctx
        .banks_client
        .get_account(market)
        .await
        .unwrap()
        .expect("market should exist");
    assert!(
        rent.is_exempt(market_account.lamports, market_account.data.len()),
        "market should remain rent-exempt after state updates"
    );

    let pos_account = ctx
        .banks_client
        .get_account(lender_pos_pda)
        .await
        .unwrap()
        .expect("lender position should exist");
    assert!(
        rent.is_exempt(pos_account.lamports, pos_account.data.len()),
        "lender position should remain rent-exempt after state updates"
    );

    let (config_pda, _) = get_protocol_config_pda();
    let config_account = ctx
        .banks_client
        .get_account(config_pda)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rent.is_exempt(config_account.lamports, config_account.data.len()),
        "protocol config should remain rent-exempt after state updates"
    );
}

// ===========================================================================
// 6. Token Transfer Integration (3 tests)
// ===========================================================================

/// 6a. Deposit transfers tokens from lender to vault.
#[tokio::test]
async fn test_token_transfer_deposit() {
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
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let (vault, _) = get_vault_pda(&market);

    // Verify initial balances
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_before = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(lender_balance_before, 1_000 * USDC);
    assert_eq!(vault_balance_before, 0);

    // Deposit 750 USDC
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        750 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify token transfer
    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_after = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(lender_balance_after, 1_000 * USDC - 750 * USDC);
    assert_eq!(vault_balance_after, 750 * USDC);
}

/// 6b. Withdraw transfers tokens from vault to lender.
#[tokio::test]
async fn test_token_transfer_withdraw() {
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

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity_timestamp,
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

    // Deposit 1000 USDC
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
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    let (vault, _) = get_vault_pda(&market);
    let lender_balance_before_withdraw = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_before_withdraw = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(lender_balance_before_withdraw, 0);
    assert_eq!(vault_balance_before_withdraw, 1_000 * USDC);

    // Full withdrawal (scaled_amount = 0 means full)
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

    // Verify tokens moved from vault to lender
    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let vault_balance_after = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(lender_balance_after, 1_000 * USDC);
    assert_eq!(vault_balance_after, 0);
}

/// 6c. Borrow transfers tokens from vault to borrower.
#[tokio::test]
async fn test_token_transfer_borrow() {
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

    // Deposit funds into vault
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

    // Borrow 600 USDC
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = get_vault_pda(&market);

    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let borrower_before = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(vault_before, 1_000 * USDC);
    assert_eq!(borrower_before, 0);

    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        600 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify token transfer
    let vault_after = get_token_balance(&mut ctx, &vault).await;
    let borrower_after = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(vault_after, 1_000 * USDC - 600 * USDC);
    assert_eq!(borrower_after, 600 * USDC);
}

// ===========================================================================
// 7. Full Lifecycle Through BPF (2 tests)
// ===========================================================================

/// 7a. Complete happy path:
/// init protocol -> create market -> deposit -> borrow -> repay -> withdraw -> close position
#[tokio::test]
async fn test_lifecycle_happy_path() {
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

    // Step 1: Initialize protocol (zero fees for simplicity)
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0,
    )
    .await;

    // Verify protocol config was created
    let (config_pda, _) = get_protocol_config_pda();
    let config_data = get_account_data(&mut ctx, &config_pda).await;
    let config = parse_protocol_config(&config_data);
    assert_eq!(config.is_initialized, 1);

    // Step 2: Create market
    let mint = create_mint(&mut ctx, &admin, 6).await;
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert_eq!(parsed_market.maturity_timestamp, maturity_timestamp);
    assert_eq!(parsed_market.market_nonce, 1);

    // Step 3: Deposit 1000 USDC
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
    assert_eq!(get_token_balance(&mut ctx, &vault).await, 1_000 * USDC);

    // Step 4: Borrow 500 USDC
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

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

    assert_eq!(
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
        500 * USDC
    );
    assert_eq!(get_token_balance(&mut ctx, &vault).await, 500 * USDC);

    // Step 5: Repay 500 USDC
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

    assert_eq!(get_token_balance(&mut ctx, &vault).await, 1_000 * USDC);

    // Step 6: Advance past maturity and withdraw
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full withdrawal
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Full repayment with 0% interest means lender gets back exactly 1000 USDC
    let lender_balance = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(lender_balance, 1_000 * USDC);

    // Step 7: Close lender position
    let close_ix = build_close_lender_position(&market, &lender.pubkey());
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify position is closed (zeroed or gone)
    let (lender_pos_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let pos_account = ctx.banks_client.get_account(lender_pos_pda).await.unwrap();
    match pos_account {
        None => {}, // garbage-collected -- correct
        Some(acct) => {
            assert_eq!(acct.lamports, 0, "position lamports should be 0");
            assert!(
                acct.data.iter().all(|&b| b == 0),
                "position data should be zeroed"
            );
        },
    }
}

/// 7b. Partial default path:
/// init -> create -> deposit -> borrow -> partial repay -> settle (via withdraw) -> withdraw with loss
#[tokio::test]
async fn test_lifecycle_partial_default() {
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

    // Initialize with 0% fee for clarity
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
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
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

    // Borrow 1000 USDC (all of it)
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

    // Partial repay: only 600 USDC out of 1000 (40% default)
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

    // Verify vault has only 600 USDC
    let (vault, _) = get_vault_pda(&market);
    assert_eq!(get_token_balance(&mut ctx, &vault).await, 600 * USDC);

    // Advance past maturity
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Withdraw -- triggers settlement. Lender gets proportional share (600/1000 = 60%)
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

    // Verify lender received 600 USDC (loss of 400)
    let lender_balance = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance,
        600 * USDC,
        "lender should receive proportional share (600 USDC with 40% default)"
    );

    // Verify settlement factor is < WAD (partial default)
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    assert!(
        parsed.settlement_factor_wad > 0 && parsed.settlement_factor_wad < WAD,
        "settlement factor ({}) should be between 0 and WAD ({}) for partial default",
        parsed.settlement_factor_wad,
        WAD,
    );

    // Verify re-settle after additional repayment improves the factor
    // Borrower repays 200 more USDC
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        200 * USDC,
    )
    .await;
    let repay_ix_2 = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        200 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Re-settle should improve the settlement factor
    let re_settle_ix = build_re_settle(&market, &vault);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[re_settle_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_after = parse_market(&market_data);
    assert!(
        parsed_after.settlement_factor_wad > parsed.settlement_factor_wad,
        "settlement factor should improve after re-settle ({} > {})",
        parsed_after.settlement_factor_wad,
        parsed.settlement_factor_wad,
    );
}

// ===========================================================================
// 8. CPI Guard Behavior (2 tests)
// ===========================================================================

/// 8a. Verify SPL token CPI calls succeed with correct authority (deposit).
/// The deposit instruction performs a Transfer CPI from lender to vault.
#[tokio::test]
async fn test_cpi_correct_authority_succeeds() {
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
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        2_000 * USDC,
    )
    .await;

    // x-1/x/x+1 deposits with correct authority all succeed.
    let mut deposited_total = 0u64;
    for amount in [499u64 * USDC, 500u64 * USDC, 501u64 * USDC] {
        let deposit_ix = build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
        );
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[deposit_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );

        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("CPI transfer with correct authority should succeed");
        deposited_total = deposited_total.checked_add(amount).unwrap();
    }

    let (vault, _) = get_vault_pda(&market);
    let vault_balance = get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_balance, deposited_total);
    assert_eq!(
        get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        2_000 * USDC - deposited_total
    );
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    assert_eq!(parsed_market.total_deposited, deposited_total);
}

/// 8b. Verify SPL token CPI calls fail with wrong authority.
/// Attempt to deposit using a token account whose owner is different from
/// the signer. The SPL token program should reject the transfer.
#[tokio::test]
async fn test_cpi_wrong_authority_fails() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();
    let other_user = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender, &other_user],
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

    // Create a token account owned by other_user (NOT the lender)
    let other_token = create_token_account(&mut ctx, &mint, &other_user.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &other_token.pubkey(), &admin, 500 * USDC).await;

    // Build deposit instruction that passes the lender as signer but uses
    // other_user's token account (whose authority is other_user).
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let (blacklist_check, _) = get_blacklist_pda(&blacklist_program.pubkey(), &lender.pubkey());
    let (protocol_config, _) = get_protocol_config_pda();

    let other_before = get_token_balance(&mut ctx, &other_token.pubkey()).await;
    let vault_before = get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // x/x+1 neighbor amounts both fail with InvalidTokenAccountOwner.
    for amount in [1u64, 500u64 * USDC] {
        let mut data = vec![3u8]; // deposit discriminator
        data.extend_from_slice(&amount.to_le_bytes());

        let ix = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(market, false),
                AccountMeta::new(lender.pubkey(), true), // lender signs
                AccountMeta::new(other_token.pubkey(), false), // owned by other_user
                AccountMeta::new(vault, false),
                AccountMeta::new(lender_position, false),
                AccountMeta::new_readonly(blacklist_check, false),
                AccountMeta::new_readonly(protocol_config, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(spl_token::id(), false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_banks_custom_error(&err, 16, "deposit with token-account owner mismatch");

        let snap_after =
            ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
        snap_before.assert_unchanged(&snap_after);
        assert_eq!(
            get_token_balance(&mut ctx, &other_token.pubkey()).await,
            other_before
        );
        assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before);
    }
    assert!(
        ctx.banks_client
            .get_account(lender_position)
            .await
            .unwrap()
            .is_none(),
        "lender position should not be created on owner mismatch"
    );

    // Neighbor success control: lender-owned token account succeeds.
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 1).await;
    let ok_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let ok_tx = Transaction::new_signed_with_payer(
        &[ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client.process_transaction(ok_tx).await.unwrap();
    assert_eq!(get_token_balance(&mut ctx, &lender_token.pubkey()).await, 0);
    assert_eq!(get_token_balance(&mut ctx, &vault).await, vault_before + 1);
}
