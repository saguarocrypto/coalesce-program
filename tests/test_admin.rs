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
    instruction::AccountMeta, pubkey::Pubkey, signature::Keypair, signer::Signer, system_program,
    transaction::Transaction,
};

// ---------------------------------------------------------------------------
// InitializeProtocol tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_initialize_protocol_success() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500; // 5%

    // Fund the admin so it can pay for the PDA account creation
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &admin.pubkey(),
        10_000_000_000, // 10 SOL
    );
    let fund_tx = Transaction::new_signed_with_payer(
        &[fund_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    // Build and send InitializeProtocol
    let ix = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read back the protocol config account and verify fields
    let (protocol_config_pda, expected_bump) = get_protocol_config_pda();
    let data = get_account_data(&mut ctx, &protocol_config_pda).await;

    assert_eq!(data.len(), PROTOCOL_CONFIG_SIZE);

    // Parse fields according to ProtocolConfig layout:
    // Account prefix: discriminator (8 bytes) + version (1 byte) = 9 bytes
    // admin: [u8; 32]         offset 9
    // fee_rate_bps: [u8; 2]   offset 41
    // fee_authority: [u8; 32] offset 43
    // whitelist_manager: [u8; 32] offset 75
    // blacklist_program: [u8; 32] offset 107
    // is_initialized: u8      offset 139
    // bump: u8                offset 140
    // _padding: [u8; 53]      offset 141

    assert_eq!(&data[9..41], admin.pubkey().as_ref(), "admin mismatch");
    assert_eq!(
        u16::from_le_bytes([data[41], data[42]]),
        fee_rate_bps,
        "fee_rate_bps mismatch"
    );
    assert_eq!(
        &data[43..75],
        fee_authority.pubkey().as_ref(),
        "fee_authority mismatch"
    );
    assert_eq!(
        &data[75..107],
        whitelist_manager.pubkey().as_ref(),
        "whitelist_manager mismatch"
    );
    assert_eq!(
        &data[107..139],
        blacklist_program.pubkey().as_ref(),
        "blacklist_program mismatch"
    );
    assert_eq!(data[139], 1, "is_initialized should be 1");
    assert_eq!(data[140], expected_bump, "bump mismatch");
}

#[tokio::test]
async fn test_initialize_protocol_invalid_fee_rate() {
    // x-1/x/x+1 boundary around MAX_FEE_RATE_BPS=10_000.
    for (fee_rate_bps, should_succeed) in [(9_999u16, true), (10_000u16, true), (10_001u16, false)]
    {
        let mut ctx = common::start_context().await;

        let admin = Keypair::new();
        let fee_authority = Keypair::new();
        let whitelist_manager = Keypair::new();
        let blacklist_program = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &fee_authority.pubkey(),
            &whitelist_manager.pubkey(),
            &blacklist_program.pubkey(),
            fee_rate_bps,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        let result = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .map_err(|e| e.unwrap());

        let (protocol_config_pda, _) = get_protocol_config_pda();
        if should_succeed {
            assert!(
                result.is_ok(),
                "fee_rate_bps={fee_rate_bps} should be accepted"
            );
            let parsed =
                parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
            assert_eq!(parsed.fee_rate_bps, fee_rate_bps);
            assert_eq!(parsed.is_initialized, 1);
        } else {
            assert_custom_error(&result, 1); // InvalidFeeRate
            assert!(
                try_get_account_data(&mut ctx, &protocol_config_pda)
                    .await
                    .is_none(),
                "protocol config must not be created on invalid fee rate"
            );
        }
    }
}

#[tokio::test]
async fn test_initialize_protocol_zero_fee_authority() {
    // Failure case: zero fee authority must be rejected.
    {
        let mut ctx = common::start_context().await;
        let admin = Keypair::new();
        let whitelist_manager = Keypair::new();
        let blacklist_program = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &Pubkey::default(),
            &whitelist_manager.pubkey(),
            &blacklist_program.pubkey(),
            500,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        let result = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .map_err(|e| e.unwrap());
        assert_custom_error(&result, 10); // InvalidAddress

        let (protocol_config_pda, _) = get_protocol_config_pda();
        assert!(
            try_get_account_data(&mut ctx, &protocol_config_pda)
                .await
                .is_none(),
            "protocol config must not be created when fee authority is zero"
        );
    }

    // Neighbor control: non-zero fee authority should succeed.
    {
        let mut ctx = common::start_context().await;
        let admin = Keypair::new();
        let fee_authority = Keypair::new();
        let whitelist_manager = Keypair::new();
        let blacklist_program = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &fee_authority.pubkey(),
            &whitelist_manager.pubkey(),
            &blacklist_program.pubkey(),
            500,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let (protocol_config_pda, _) = get_protocol_config_pda();
        let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
        assert_eq!(parsed.fee_rate_bps, 500);
        assert_eq!(&parsed.fee_authority, fee_authority.pubkey().as_ref());
    }
}

#[tokio::test]
async fn test_initialize_protocol_non_signer() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500;

    // Build the instruction manually with admin NOT marked as signer
    let (protocol_config, _) = get_protocol_config_pda();
    let (program_data, _) = Pubkey::find_program_address(
        &[program_id().as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    let mut data = vec![0u8]; // discriminator
    data.extend_from_slice(&fee_rate_bps.to_le_bytes());

    let ix = solana_sdk::instruction::Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false), // protocol_config PDA (writable)
            AccountMeta::new(admin.pubkey(), false),  // admin NOT a signer (writable for payer)
            AccountMeta::new_readonly(fee_authority.pubkey(), false), // fee_authority
            AccountMeta::new_readonly(whitelist_manager.pubkey(), false), // whitelist_manager
            AccountMeta::new_readonly(blacklist_program.pubkey(), false), // blacklist_program
            AccountMeta::new_readonly(system_program::id(), false), // system_program
            AccountMeta::new_readonly(program_data, false), // program_data
        ],
        data,
    };

    // Only sign with payer (not admin)
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
    assert_custom_error(&result, 5); // Unauthorized

    // Failed non-signer call must not create protocol config state.
    let (protocol_config_pda, _) = get_protocol_config_pda();
    assert!(
        try_get_account_data(&mut ctx, &protocol_config_pda)
            .await
            .is_none(),
        "protocol config must not be created when admin is not signer"
    );

    // Neighbor control: same operation with signer admin should succeed.
    airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
    let success_ix = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let success_tx = Transaction::new_signed_with_payer(
        &[success_ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client
        .process_transaction(success_tx)
        .await
        .unwrap();

    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, fee_rate_bps);
    assert_eq!(&parsed.admin, admin.pubkey().as_ref());
}

// ---------------------------------------------------------------------------
// SetFeeConfig tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_set_fee_config_success() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let initial_fee_rate_bps: u16 = 500;

    // Fund admin
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &admin.pubkey(),
        10_000_000_000,
    );
    let fund_tx = Transaction::new_signed_with_payer(
        &[fund_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    // Step 1: Initialize the protocol
    let init_ix = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        initial_fee_rate_bps,
    );
    let init_tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(init_tx).await.unwrap();

    let (protocol_config_pda, _) = get_protocol_config_pda();
    // x-1 boundary: 9_999 should succeed.
    let auth_x_minus_1 = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x_minus_1.pubkey(), 9_999);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, 9_999);
    assert_eq!(&parsed.fee_authority, auth_x_minus_1.pubkey().as_ref());
    assert_eq!(&parsed.admin, admin.pubkey().as_ref());

    // x boundary: 10_000 should succeed.
    let auth_x = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x.pubkey(), 10_000);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, 10_000);
    assert_eq!(&parsed.fee_authority, auth_x.pubkey().as_ref());

    // x+1 boundary: 10_001 must fail without mutating protocol config.
    let before_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    let auth_x_plus_1 = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x_plus_1.pubkey(), 10_001);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 1); // InvalidFeeRate
    let after_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(after_fail, before_fail);
}

#[tokio::test]
async fn test_set_fee_config_wrong_admin() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500;

    // Fund admin
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &admin.pubkey(),
        10_000_000_000,
    );
    let fund_tx = Transaction::new_signed_with_payer(
        &[fund_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    // Step 1: Initialize protocol with the real admin
    let init_ix = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    );
    let init_tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(init_tx).await.unwrap();

    // Step 2: Try to set fee config with a DIFFERENT keypair as admin
    let wrong_admin = Keypair::new();
    let new_fee_authority = Keypair::new();
    let (protocol_config_pda, _) = get_protocol_config_pda();
    let before_fail = get_account_data(&mut ctx, &protocol_config_pda).await;

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let set_ix = build_set_fee_config(&wrong_admin.pubkey(), &new_fee_authority.pubkey(), 1000);
    let set_tx = Transaction::new_signed_with_payer(
        &[set_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_admin],
        recent_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(set_tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized
    let after_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(after_fail, before_fail);

    // Control path: same update with the real admin should succeed.
    let real_admin_auth = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &real_admin_auth.pubkey(), 1000);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, 1000);
    assert_eq!(&parsed.fee_authority, real_admin_auth.pubkey().as_ref());
}

#[tokio::test]
async fn test_set_fee_config_invalid_fee_rate() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500;

    // Fund admin
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &admin.pubkey(),
        10_000_000_000,
    );
    let fund_tx = Transaction::new_signed_with_payer(
        &[fund_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    // Step 1: Initialize protocol
    let init_ix = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    );
    let init_tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(init_tx).await.unwrap();
    let (protocol_config_pda, _) = get_protocol_config_pda();

    // x-1 boundary success.
    let auth_x_minus_1 = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x_minus_1.pubkey(), 9_999);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, 9_999);
    assert_eq!(&parsed.fee_authority, auth_x_minus_1.pubkey().as_ref());

    // x+1 boundary failure.
    let before_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    let auth_x_plus_1 = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x_plus_1.pubkey(), 10_001);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 1); // InvalidFeeRate
    let after_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(after_fail, before_fail);

    // x boundary success after failed attempt.
    let auth_x = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x.pubkey(), 10_000);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, 10_000);
    assert_eq!(&parsed.fee_authority, auth_x.pubkey().as_ref());
}

// ---------------------------------------------------------------------------
// Additional InitializeProtocol tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_init_protocol_zero_whitelist_manager() {
    // Failure case: zero whitelist manager must be rejected.
    {
        let mut ctx = common::start_context().await;
        let admin = Keypair::new();
        let fee_authority = Keypair::new();
        let blacklist_program = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &fee_authority.pubkey(),
            &Pubkey::default(),
            &blacklist_program.pubkey(),
            500,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        let result = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .map_err(|e| e.unwrap());
        assert_custom_error(&result, 10); // InvalidAddress

        let (protocol_config_pda, _) = get_protocol_config_pda();
        assert!(
            try_get_account_data(&mut ctx, &protocol_config_pda)
                .await
                .is_none(),
            "protocol config must not be created when whitelist manager is zero"
        );
    }

    // Neighbor control: non-zero whitelist manager should succeed.
    {
        let mut ctx = common::start_context().await;
        let admin = Keypair::new();
        let fee_authority = Keypair::new();
        let whitelist_manager = Keypair::new();
        let blacklist_program = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &fee_authority.pubkey(),
            &whitelist_manager.pubkey(),
            &blacklist_program.pubkey(),
            500,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        let (protocol_config_pda, _) = get_protocol_config_pda();
        let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
        assert_eq!(
            &parsed.whitelist_manager,
            whitelist_manager.pubkey().as_ref()
        );
    }
}

#[tokio::test]
async fn test_init_protocol_zero_blacklist_program() {
    // Failure case: zero blacklist program must be rejected.
    {
        let mut ctx = common::start_context().await;
        let admin = Keypair::new();
        let fee_authority = Keypair::new();
        let whitelist_manager = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &fee_authority.pubkey(),
            &whitelist_manager.pubkey(),
            &Pubkey::default(),
            500,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        let result = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .map_err(|e| e.unwrap());
        assert_custom_error(&result, 10); // InvalidAddress

        let (protocol_config_pda, _) = get_protocol_config_pda();
        assert!(
            try_get_account_data(&mut ctx, &protocol_config_pda)
                .await
                .is_none(),
            "protocol config must not be created when blacklist program is zero"
        );
    }

    // Neighbor control: non-zero blacklist program should succeed.
    {
        let mut ctx = common::start_context().await;
        let admin = Keypair::new();
        let fee_authority = Keypair::new();
        let whitelist_manager = Keypair::new();
        let blacklist_program = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &fee_authority.pubkey(),
            &whitelist_manager.pubkey(),
            &blacklist_program.pubkey(),
            500,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        let (protocol_config_pda, _) = get_protocol_config_pda();
        let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
        assert_eq!(
            &parsed.blacklist_program,
            blacklist_program.pubkey().as_ref()
        );
    }
}

#[tokio::test]
async fn test_init_protocol_fee_rate_at_max() {
    // x-1/x/x+1 boundary around MAX_FEE_RATE_BPS=10_000.
    for (fee_rate_bps, should_succeed) in [(9_999u16, true), (10_000u16, true), (10_001u16, false)]
    {
        let mut ctx = common::start_context().await;
        let admin = Keypair::new();
        let fee_authority = Keypair::new();
        let whitelist_manager = Keypair::new();
        let blacklist_program = Keypair::new();

        airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
        setup_program_data_account(&mut ctx, &admin.pubkey());

        let ix = build_initialize_protocol(
            &admin.pubkey(),
            &fee_authority.pubkey(),
            &whitelist_manager.pubkey(),
            &blacklist_program.pubkey(),
            fee_rate_bps,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            ctx.last_blockhash,
        );
        let result = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .map_err(|e| e.unwrap());

        let (protocol_config_pda, _) = get_protocol_config_pda();
        if should_succeed {
            assert!(
                result.is_ok(),
                "fee_rate_bps={fee_rate_bps} should be accepted"
            );
            let parsed =
                parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
            assert_eq!(parsed.fee_rate_bps, fee_rate_bps);
        } else {
            assert_custom_error(&result, 1); // InvalidFeeRate
            assert!(
                try_get_account_data(&mut ctx, &protocol_config_pda)
                    .await
                    .is_none(),
                "protocol config must not be created on invalid fee rate"
            );
        }
    }
}

#[tokio::test]
async fn test_init_protocol_double_init() {
    // The program does not guard against double-init: create_account_with_minimum_balance_signed
    // succeeds on an already-existing PDA and the config data is overwritten.
    // Verify the second call succeeds and the config reflects the new values.
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500;

    // Fund admin
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &admin.pubkey(),
        10_000_000_000,
    );
    let fund_tx = Transaction::new_signed_with_payer(
        &[fund_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    // First init -- should succeed
    let ix1 = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    );
    let tx1 = Transaction::new_signed_with_payer(
        &[ix1],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx1)
        .await
        .expect("first init should succeed");

    // Second init with a different fee_rate must fail with AlreadyInitialized.
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let (protocol_config_pda, _) = get_protocol_config_pda();
    let before_second = get_account_data(&mut ctx, &protocol_config_pda).await;
    let ix2 = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        1000,
    );
    let tx2 = Transaction::new_signed_with_payer(
        &[ix2],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );

    // The program now has double-init protection (AccountAlreadyInitialized = 0)
    let result = ctx
        .banks_client
        .process_transaction(tx2)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 0); // AccountAlreadyInitialized
    let after_second = get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(after_second, before_second);

    // Determinism: repeated re-init attempts keep failing with no state mutation.
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let before_third = get_account_data(&mut ctx, &protocol_config_pda).await;
    let ix3 = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        9_999,
    );
    let tx3 = Transaction::new_signed_with_payer(
        &[ix3],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx3)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 0); // AccountAlreadyInitialized
    let after_third = get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(after_third, before_third);
}

// ---------------------------------------------------------------------------
// Additional SetFeeConfig tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_set_fee_config_non_signer_admin() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500;

    // Fund admin for protocol init
    airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;

    // Initialize the protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    // Build set_fee_config manually with admin NOT marked as signer
    let new_fee_authority = Keypair::new();
    let (protocol_config, _) = get_protocol_config_pda();
    let before_fail = get_account_data(&mut ctx, &protocol_config).await;

    let ix = solana_sdk::instruction::Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false),
            AccountMeta::new_readonly(admin.pubkey(), false), // NOT signer
            AccountMeta::new_readonly(new_fee_authority.pubkey(), false),
        ],
        data: {
            let mut d = vec![1u8];
            d.extend_from_slice(&500u16.to_le_bytes());
            d
        },
    };

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Only sign with payer (not admin)
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized
    let after_fail = get_account_data(&mut ctx, &protocol_config).await;
    assert_eq!(after_fail, before_fail);

    // Neighbor control: signer admin update should succeed.
    let authorized_fee_authority = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &authorized_fee_authority.pubkey(), 500);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config).await);
    assert_eq!(
        &parsed.fee_authority,
        authorized_fee_authority.pubkey().as_ref()
    );
}

#[tokio::test]
async fn test_set_fee_config_zero_new_fee_authority() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500;

    // Fund admin
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &admin.pubkey(),
        10_000_000_000,
    );
    let fund_tx = Transaction::new_signed_with_payer(
        &[fund_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    // Initialize the protocol
    let init_ix = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    );
    let init_tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(init_tx).await.unwrap();

    // Try to set fee config with Pubkey::default() as new fee authority
    let (protocol_config_pda, _) = get_protocol_config_pda();
    let before_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let set_ix = build_set_fee_config(&admin.pubkey(), &Pubkey::default(), 500);
    let set_tx = Transaction::new_signed_with_payer(
        &[set_ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(set_tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 10); // InvalidAddress
    let after_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(after_fail, before_fail);

    // Neighbor control: non-zero fee authority should succeed.
    let valid_fee_authority = Keypair::new();
    let set_ix = build_set_fee_config(&admin.pubkey(), &valid_fee_authority.pubkey(), 500);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let set_tx = Transaction::new_signed_with_payer(
        &[set_ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(set_tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(&parsed.fee_authority, valid_fee_authority.pubkey().as_ref());
}

#[tokio::test]
async fn test_set_fee_config_fee_rate_at_max() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let initial_fee_rate_bps: u16 = 500;

    // Fund admin
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &admin.pubkey(),
        10_000_000_000,
    );
    let fund_tx = Transaction::new_signed_with_payer(
        &[fund_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Inject fake program_data account for upgrade authority verification
    setup_program_data_account(&mut ctx, &admin.pubkey());

    // Initialize the protocol
    let init_ix = build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        initial_fee_rate_bps,
    );
    let init_tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(init_tx).await.unwrap();
    let (protocol_config_pda, _) = get_protocol_config_pda();

    // x-1 boundary: 9_999
    let auth_x_minus_1 = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x_minus_1.pubkey(), 9_999);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, 9_999);
    assert_eq!(&parsed.fee_authority, auth_x_minus_1.pubkey().as_ref());

    // x boundary: 10_000
    let auth_x = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x.pubkey(), 10_000);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let parsed = parse_protocol_config(&get_account_data(&mut ctx, &protocol_config_pda).await);
    assert_eq!(parsed.fee_rate_bps, 10_000);
    assert_eq!(&parsed.fee_authority, auth_x.pubkey().as_ref());

    // x+1 boundary: 10_001 fails with no mutation.
    let before_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    let auth_x_plus_1 = Keypair::new();
    let ix = build_set_fee_config(&admin.pubkey(), &auth_x_plus_1.pubkey(), 10_001);
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        recent_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 1); // InvalidFeeRate
    let after_fail = get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(after_fail, before_fail);
}
