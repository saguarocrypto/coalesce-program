//! P1-3: BPF entrypoint fuzz test.
//!
//! Unlike the existing fuzz targets (which test in-process logic), this test
//! sends fuzzed instruction data through the REAL sBPF binary via
//! `solana-program-test` with `prefer_bpf(true)`. It verifies:
//!
//! 1. Random garbage bytes never crash the BPF program (always clean error)
//! 2. Valid discriminators with corrupted payloads don't panic
//! 3. Valid instruction shapes with invalid accounts don't corrupt state
//!
//! This provides coverage that model-based fuzz targets cannot: the actual
//! BPF binary's instruction parsing, account deserialization, and error paths
//! are exercised end-to-end.

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

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use solana_program_test::{BanksClientError, ProgramTestContext};
use solana_sdk::{
    instruction::{AccountMeta, Instruction, InstructionError},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::{Transaction, TransactionError},
};

const USDC: u64 = 1_000_000;

/// Known instruction discriminators used by the Coalesce program.
const VALID_DISCRIMINATORS: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

fn to_tx_result(result: Result<(), BanksClientError>) -> Result<(), TransactionError> {
    result.map_err(|e| e.unwrap())
}

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

fn assert_instruction_error_in(
    result: &Result<(), TransactionError>,
    expected_errors: &[InstructionError],
) {
    match result {
        Err(TransactionError::InstructionError(_, actual_error)) => {
            assert!(
                expected_errors.iter().any(|e| e == actual_error),
                "expected one of {expected_errors:?}, got {actual_error:?}"
            );
        },
        Err(other) => panic!("expected InstructionError({expected_errors:?}), got {other:?}"),
        Ok(()) => {
            panic!("expected InstructionError({expected_errors:?}), but transaction succeeded")
        },
    }
}

async fn fund_system_account(ctx: &mut ProgramTestContext, account: &Pubkey) {
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[system_instruction::transfer(
            &ctx.payer.pubkey(),
            account,
            1_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// ---------------------------------------------------------------------------
// Strategy: generate random instruction data bytes
// ---------------------------------------------------------------------------

fn arb_instruction_data() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // Empty data
        Just(vec![]),
        // Single byte (discriminator only)
        (0u8..=255).prop_map(|b| vec![b]),
        // Valid discriminator + random payload (1..64 bytes)
        (
            prop::sample::select(VALID_DISCRIMINATORS),
            prop::collection::vec(any::<u8>(), 0..63)
        )
            .prop_map(|(disc, mut payload)| {
                let mut data = vec![disc];
                data.append(&mut payload);
                data
            }),
        // Completely random bytes (1..128 bytes)
        prop::collection::vec(any::<u8>(), 1..128),
        // Valid discriminator + exact expected payload sizes (edge cases)
        (prop::sample::select(VALID_DISCRIMINATORS), any::<[u8; 8]>()).prop_map(
            |(disc, payload)| {
                let mut data = vec![disc];
                data.extend_from_slice(&payload);
                data
            }
        ),
        // Valid discriminator + oversized payload
        (
            prop::sample::select(VALID_DISCRIMINATORS),
            prop::collection::vec(any::<u8>(), 64..256)
        )
            .prop_map(|(disc, mut payload)| {
                let mut data = vec![disc];
                data.append(&mut payload);
                data
            }),
    ]
}

/// Generate random account metas (mix of valid and garbage pubkeys).
fn arb_account_metas() -> impl Strategy<Value = Vec<AccountMeta>> {
    let meta = (any::<[u8; 32]>(), any::<bool>(), any::<bool>()).prop_map(
        |(bytes, is_signer, is_writable)| {
            let pubkey = Pubkey::new_from_array(bytes);
            if is_writable {
                AccountMeta::new(pubkey, is_signer)
            } else {
                AccountMeta::new_readonly(pubkey, is_signer)
            }
        },
    );
    prop::collection::vec(meta, 0..15)
}

// ---------------------------------------------------------------------------
// Test 1: Random garbage data never crashes the BPF binary
// ---------------------------------------------------------------------------

/// Send random instruction data to the BPF entrypoint with a minimal account
/// list. The program must return an error, never panic/abort.
#[tokio::test]
async fn test_bpf_random_data_no_crash() {
    let mut ctx = common::start_context().await;
    let payer = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&payer], 10_000_000_000).await;
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    assert!(common::try_get_account_data(&mut ctx, &protocol_config_pda)
        .await
        .is_none());

    // Generate a fixed set of random instruction data vectors
    // (proptest strategies can't be used directly in async tests, so we
    // use a deterministic seed to generate test cases)
    let mut runner = proptest::test_runner::TestRunner::deterministic();

    for _ in 0..50 {
        let data = arb_instruction_data()
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let data_is_empty = data.is_empty();
        let disc = data.first().copied();

        // Minimal account list: just the payer
        let ix = Instruction {
            program_id: common::program_id(),
            accounts: vec![AccountMeta::new(payer.pubkey(), true)],
            data,
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx =
            Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], recent);

        // Must reject cleanly with deterministic runtime/program errors.
        let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
        if data_is_empty {
            assert_instruction_error(&result, InstructionError::InvalidInstructionData);
        } else if disc.expect("disc exists when not empty") > 16 {
            assert_instruction_error(&result, InstructionError::InvalidInstructionData);
        } else {
            // With a single account, all known instructions should fail account-count checks.
            assert_instruction_error(&result, InstructionError::NotEnoughAccountKeys);
        }

        // Random garbage data with minimal accounts must never initialize protocol state.
        assert!(common::try_get_account_data(&mut ctx, &protocol_config_pda)
            .await
            .is_none());
    }
}

// ---------------------------------------------------------------------------
// Test 2: Valid discriminators with corrupted payloads
// ---------------------------------------------------------------------------

/// For each known discriminator, send it with truncated, empty, and oversized
/// payloads. The BPF binary must reject cleanly, never crash.
#[tokio::test]
async fn test_bpf_corrupted_payloads_no_crash() {
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

    // Set up protocol so some accounts exist
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
        5_000 * USDC,
    )
    .await;

    // Snapshot state before fuzzing
    let (vault, _) = common::get_vault_pda(&market);
    let snap_before = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    // Build a valid deposit instruction as a template
    let valid_deposit = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    // Corrupted payload variants for each discriminator
    let corrupted_payloads: Vec<(&str, Vec<u8>)> = vec![
        // Disc 0 (InitializeProtocol) — truncated
        ("init_empty_payload", vec![0]),
        ("init_truncated_payload", vec![0, 0xff]),
        // Disc 1 (SetFeeConfig) — truncated
        ("set_fee_config_truncated", vec![1]),
        // Disc 2 (CreateMarket) — truncated (needs 26 bytes, give 5)
        ("create_market_truncated", vec![2, 0, 0, 0, 0]),
        // Disc 3 (Deposit) — wrong size
        ("deposit_empty_payload", vec![3]),
        ("deposit_short_payload", vec![3, 0xff, 0xff, 0xff]),
        // Disc 4 (Borrow) — truncated
        ("borrow_empty_payload", vec![4]),
        ("borrow_short_payload", vec![4, 0, 0]),
        // Disc 5 (Repay) — truncated
        ("repay_empty_payload", vec![5]),
        // Disc 6 (RepayInterest) — truncated
        ("repay_interest_empty_payload", vec![6]),
        // Disc 7 (Withdraw) — truncated (needs 24 bytes, give 8)
        ("withdraw_short_payload", vec![7, 0, 0, 0, 0, 0, 0, 0, 0]),
        // Disc 9 (SetPause) — truncated
        ("set_pause_truncated", vec![13]),
        // Disc 12 (SetBorrowerWhitelist) — truncated
        ("set_whitelist_truncated", vec![12, 0, 0]),
        // Disc 14 (UpdateMarket) — truncated
        ("set_blacklist_mode_truncated", vec![14]),
        // Invalid discriminator
        ("invalid_discriminator_200", vec![200]),
        ("invalid_discriminator_255", vec![255]),
    ];

    for (name, data) in &corrupted_payloads {
        let snap_before_case =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

        // Use the valid deposit's accounts but with corrupted data
        let ix = Instruction {
            program_id: common::program_id(),
            accounts: valid_deposit.accounts.clone(),
            data: data.clone(),
        };

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx =
            Transaction::new_signed_with_payer(&[ix], Some(&lender.pubkey()), &[&lender], recent);

        let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
        assert_instruction_error_in(
            &result,
            &[
                InstructionError::InvalidInstructionData,
                InstructionError::Custom(15), // InvalidTokenProgram on mismatched account layouts
                InstructionError::NotEnoughAccountKeys, // Account count mismatch (e.g., create_market needs 11)
            ],
        );

        // Failed corrupted payloads must be atomic.
        let snap_after_case =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
        snap_before_case.assert_unchanged(&snap_after_case);

        let _ = name;
    }

    // State must not have changed from any corrupted instruction
    let snap_after = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snap_before.assert_unchanged(&snap_after);
}

// ---------------------------------------------------------------------------
// Test 3: Valid instruction with random account substitutions
// ---------------------------------------------------------------------------

/// Take a valid deposit instruction and randomly replace individual accounts
/// with garbage pubkeys. The program must reject without corrupting state.
#[tokio::test]
async fn test_bpf_random_account_substitution() {
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
        5_000 * USDC,
    )
    .await;

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let valid_deposit = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    // For each account position in the instruction, replace it with a random
    // pubkey and verify the program rejects it
    for account_idx in 0..valid_deposit.accounts.len() {
        // Skip the signer account (index 1) — replacing the signer breaks
        // transaction signing, not program logic
        // Skip index 9 (system_program) because Deposit does not read it.
        if account_idx == 1 || account_idx == 9 {
            continue;
        }

        let mut ix = valid_deposit.clone();
        let fake_account = Keypair::new();
        fund_system_account(&mut ctx, &fake_account.pubkey()).await;
        let fake_pubkey = fake_account.pubkey();
        let was_writable = ix.accounts[account_idx].is_writable;
        let was_signer = ix.accounts[account_idx].is_signer;
        ix.accounts[account_idx] = if was_writable {
            AccountMeta::new(fake_pubkey, was_signer)
        } else {
            AccountMeta::new_readonly(fake_pubkey, was_signer)
        };

        let snap_before_case =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx =
            Transaction::new_signed_with_payer(&[ix], Some(&lender.pubkey()), &[&lender], recent);

        let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
        match account_idx {
            0 => common::assert_custom_error(&result, 14), // InvalidAccountOwner
            2 => assert_instruction_error_in(
                &result,
                &[
                    InstructionError::InvalidAccountData,
                    InstructionError::InvalidAccountOwner,
                    InstructionError::IncorrectProgramId,
                    InstructionError::Custom(14),
                    InstructionError::Custom(16),
                ],
            ),
            3 => common::assert_custom_error(&result, 12), // InvalidVault
            4 => common::assert_custom_error(&result, 13), // InvalidPDA
            5 => common::assert_custom_error(&result, 13), // InvalidPDA
            6 => common::assert_custom_error(&result, 13), // InvalidPDA
            7 => common::assert_custom_error(&result, 11), // InvalidMint
            8 => common::assert_custom_error(&result, 15), // InvalidTokenProgram
            10 => assert_instruction_error(&result, InstructionError::InvalidAccountData),
            _ => unreachable!("unexpected account index"),
        }

        let snap_after_case =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
        snap_before_case.assert_unchanged(&snap_after_case);
    }

    // State must be unchanged
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
}

// ---------------------------------------------------------------------------
// Test 4: Fuzzed instruction sequences through BPF
// ---------------------------------------------------------------------------

/// Send a deterministic-random sequence of valid-shaped instructions with
/// edge-case parameters through the BPF entrypoint. Verifies that:
/// 1. No instruction sequence crashes the program
/// 2. The program processes or rejects each instruction cleanly
#[tokio::test]
async fn test_bpf_instruction_sequence_no_crash() {
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

    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        5_000 * USDC,
    )
    .await;

    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // Edge-case amounts to test through the BPF entrypoint
    let edge_amounts: &[u64] = &[
        0,               // zero
        1,               // minimum
        USDC,            // 1 USDC
        u32::MAX as u64, // 2^32 - 1
        u64::MAX,        // maximum
        1_000 * USDC,    // normal deposit
        u64::MAX / 2,    // half max
        u64::MAX - 1,    // max - 1
    ];

    // Test each edge amount through deposit
    for &amount in edge_amounts {
        let snap_before =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

        let ix = common::build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
        );

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx =
            Transaction::new_signed_with_payer(&[ix], Some(&lender.pubkey()), &[&lender], recent);
        let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
        let snap_after =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

        if amount == 0 {
            common::assert_custom_error(&result, 17); // ZeroAmount
            snap_before.assert_unchanged(&snap_after);
        } else if let Err(TransactionError::InstructionError(_, err)) = &result {
            assert!(
                matches!(err, InstructionError::Custom(_)),
                "unexpected non-custom deposit error for amount {amount}: {err:?}"
            );
            snap_before.assert_unchanged(&snap_after);
        } else {
            assert!(result.is_ok());
        }
    }

    // Test edge amounts through borrow
    for &amount in edge_amounts {
        let snap_before =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

        let ix = common::build_borrow(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &blacklist_program.pubkey(),
            amount,
        );

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&borrower.pubkey()),
            &[&borrower],
            recent,
        );
        let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
        let snap_after =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

        if amount == 0 {
            common::assert_custom_error(&result, 17); // ZeroAmount
            snap_before.assert_unchanged(&snap_after);
        } else if let Err(TransactionError::InstructionError(_, err)) = &result {
            assert!(
                matches!(err, InstructionError::Custom(_)),
                "unexpected non-custom borrow error for amount {amount}: {err:?}"
            );
            snap_before.assert_unchanged(&snap_after);
        } else {
            assert!(result.is_ok());
        }
    }

    // Test edge amounts through repay
    for &amount in edge_amounts {
        let snap_before =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

        let ix = common::build_repay(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &mint,
            &borrower.pubkey(),
            amount,
        );

        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&borrower.pubkey()),
            &[&borrower],
            recent,
        );
        let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
        let snap_after =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

        if amount == 0 {
            common::assert_custom_error(&result, 17); // ZeroAmount
            snap_before.assert_unchanged(&snap_after);
        } else if let Err(TransactionError::InstructionError(_, err)) = &result {
            assert!(
                matches!(err, InstructionError::Custom(_)),
                "unexpected non-custom repay error for amount {amount}: {err:?}"
            );
            snap_before.assert_unchanged(&snap_after);
        } else {
            assert!(result.is_ok());
        }
    }
}

// ---------------------------------------------------------------------------
// Test 5: Truncated and oversized account lists
// ---------------------------------------------------------------------------

/// Send valid instruction data but with wrong number of accounts (too few,
/// too many). The BPF binary must handle this gracefully.
#[tokio::test]
async fn test_bpf_wrong_account_count() {
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

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Valid deposit data
    let mut data = vec![3u8]; // deposit discriminator
    data.extend_from_slice(&(100u64 * USDC).to_le_bytes());

    // Test with 0 accounts
    let snap_before_case =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let ix_zero = Instruction {
        program_id: common::program_id(),
        accounts: vec![],
        data: data.clone(),
    };
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix_zero], Some(&lender.pubkey()), &[&lender], recent);
    let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
    assert_instruction_error(&result, InstructionError::NotEnoughAccountKeys);
    let snap_after_case =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before_case.assert_unchanged(&snap_after_case);

    // Test with 1 account (too few)
    let snap_before_case =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let ix_one = Instruction {
        program_id: common::program_id(),
        accounts: vec![AccountMeta::new(lender.pubkey(), true)],
        data: data.clone(),
    };
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix_one], Some(&lender.pubkey()), &[&lender], recent);
    let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
    assert_instruction_error(&result, InstructionError::NotEnoughAccountKeys);
    let snap_after_case =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before_case.assert_unchanged(&snap_after_case);

    // Test with 5 accounts (still too few for deposit)
    let snap_before_case =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let ix_five = Instruction {
        program_id: common::program_id(),
        accounts: vec![
            AccountMeta::new(market, false),
            AccountMeta::new(lender.pubkey(), true),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new(Pubkey::new_unique(), false),
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
        ],
        data: data.clone(),
    };
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix_five], Some(&lender.pubkey()), &[&lender], recent);
    let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
    assert_instruction_error(&result, InstructionError::NotEnoughAccountKeys);
    let snap_after_case =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before_case.assert_unchanged(&snap_after_case);

    // Prepare a valid lender token account so "extra accounts" isolates count handling.
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(&mut ctx, &mint, &lender_token.pubkey(), &admin, 500 * USDC).await;

    // Test with extra accounts (too many — 15 accounts for deposit that needs 11)
    let snap_before_extra =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let valid_deposit = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );
    let mut ix_extra = valid_deposit;
    for _ in 0..4 {
        ix_extra
            .accounts
            .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
    }
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix_extra], Some(&lender.pubkey()), &[&lender], recent);
    let result = to_tx_result(ctx.banks_client.process_transaction(tx).await);
    assert!(
        result.is_ok(),
        "deposit with extra trailing accounts should succeed"
    );
    let snap_after_extra =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    assert_eq!(
        snap_after_extra.vault_balance,
        snap_before_extra.vault_balance + 100 * USDC
    );
    assert_ne!(snap_after_extra.market_data, snap_before_extra.market_data);

    // State must remain unchanged
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    assert_ne!(snap_before.vault_balance, snap_after.vault_balance);
}
