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

use solana_program_test::ProgramTestContext;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;
const MIN_MATURITY_DELTA: i64 = 60;
const WAD: u128 = 1_000_000_000_000_000_000;

fn assert_custom_code(err: &solana_program_test::BanksClientError, expected: u32, context: &str) {
    assert_eq!(
        common::extract_custom_error(err),
        Some(expected),
        "Expected Custom({expected}) for {context}, got: {err:?}"
    );
}

async fn current_unix_timestamp(ctx: &mut ProgramTestContext) -> i64 {
    ctx.banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap()
        .unix_timestamp
}

// ==========================================================================
// Error 0: AlreadyInitialized
// Initialize protocol, then try to initialize again.
//
// NOTE: The on-chain processor does not explicitly check `is_initialized`
// before calling create_account_with_minimum_balance_signed. Some runtime
// implementations treat that CPI as idempotent for already-existing accounts,
// which means the second initialization may succeed (overwriting the config).
// This test verifies the actual behaviour: when the CPI is idempotent, the
// config retains its `is_initialized = 1` flag and remains usable.
//
// A defence-in-depth improvement would be to add an explicit
// `is_initialized != 0` guard in the processor before the CPI.
// ==========================================================================

#[tokio::test]
async fn test_err_000_already_initialized() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;

    // First initialization -- should succeed
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let (config_pda, _) = common::get_protocol_config_pda();
    let config_before = common::get_account_data(&mut ctx, &config_pda).await;
    let parsed_before = common::parse_protocol_config(&config_before);
    assert_eq!(parsed_before.is_initialized, 1);
    assert_eq!(parsed_before.fee_rate_bps, 500);

    // Re-initialization should deterministically fail regardless of fee boundary
    // neighbors, and must not mutate already-initialized config fields.
    for (idx, retry_fee_bps) in [499u16, 500u16, 501u16].into_iter().enumerate() {
        let attacker_fee_authority = Keypair::new();
        let attacker_whitelist_manager = Keypair::new();
        let attacker_blacklist_program = Keypair::new();
        let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

        let ix = common::build_initialize_protocol(
            &admin.pubkey(),
            &attacker_fee_authority.pubkey(),
            &attacker_whitelist_manager.pubkey(),
            &attacker_blacklist_program.pubkey(),
            retry_fee_bps,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&admin.pubkey()),
            &[&admin],
            recent_blockhash,
        );

        let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
        assert_custom_code(&err, 0, "re-initialize");

        let config_after = common::get_account_data(&mut ctx, &config_pda).await;
        let parsed_after = common::parse_protocol_config(&config_after);
        assert_eq!(
            config_before, config_after,
            "protocol config mutated on re-initialize attempt {idx}"
        );
        assert_eq!(
            parsed_after.fee_authority, parsed_before.fee_authority,
            "fee authority changed on re-initialize attempt {idx}"
        );
        assert_eq!(
            parsed_after.whitelist_manager, parsed_before.whitelist_manager,
            "whitelist manager changed on re-initialize attempt {idx}"
        );
        assert_eq!(
            parsed_after.blacklist_program, parsed_before.blacklist_program,
            "blacklist program changed on re-initialize attempt {idx}"
        );
        assert_eq!(
            parsed_after.fee_rate_bps, parsed_before.fee_rate_bps,
            "fee rate changed on re-initialize attempt {idx}"
        );
    }
}

// ==========================================================================
// Error 1: InvalidFeeRate
// Initialize protocol with fee_rate_bps = 10001 (exceeds max 10000).
// ==========================================================================

#[tokio::test]
async fn test_err_001_invalid_fee_rate() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;

    common::setup_program_data_account(&mut ctx, &admin.pubkey());

    let ix = common::build_initialize_protocol(
        &admin.pubkey(),
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        10_001, // exceeds MAX_FEE_RATE_BPS (10,000)
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.last_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 1, "initialize with fee_rate_bps=10001");

    // Failure should not create protocol config.
    let (protocol_config, _) = common::get_protocol_config_pda();
    assert!(
        ctx.banks_client
            .get_account(protocol_config)
            .await
            .unwrap()
            .is_none(),
        "Protocol config should not exist after invalid fee-rate initialize attempt"
    );

    // Boundary neighbors around MAX_FEE_RATE_BPS: 9999 and 10000 both succeed.
    for fee_rate_bps in [9_999u16, 10_000u16] {
        let mut ok_ctx = common::start_context().await;
        let ok_admin = Keypair::new();
        let ok_fee_authority = Keypair::new();
        let ok_whitelist_manager = Keypair::new();
        let ok_blacklist_program = Keypair::new();

        common::airdrop_multiple(&mut ok_ctx, &[&ok_admin], 10_000_000_000).await;
        common::setup_program_data_account(&mut ok_ctx, &ok_admin.pubkey());

        let ok_ix = common::build_initialize_protocol(
            &ok_admin.pubkey(),
            &ok_fee_authority.pubkey(),
            &ok_whitelist_manager.pubkey(),
            &ok_blacklist_program.pubkey(),
            fee_rate_bps,
        );
        let ok_tx = Transaction::new_signed_with_payer(
            &[ok_ix],
            Some(&ok_admin.pubkey()),
            &[&ok_admin],
            ok_ctx.last_blockhash,
        );
        ok_ctx
            .banks_client
            .process_transaction(ok_tx)
            .await
            .unwrap();

        let (ok_config_pda, _) = common::get_protocol_config_pda();
        let ok_config = common::get_account_data(&mut ok_ctx, &ok_config_pda).await;
        let parsed = common::parse_protocol_config(&ok_config);
        assert_eq!(
            parsed.fee_rate_bps, fee_rate_bps,
            "fee rate mismatch for valid boundary fee {fee_rate_bps}"
        );
        assert_eq!(parsed.is_initialized, 1);
    }
}

// ==========================================================================
// Error 2: InvalidAddress
// Initialize protocol with fee_authority = Pubkey::default() (zero address).
// ==========================================================================

#[tokio::test]
async fn test_err_002_invalid_address() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
    common::setup_program_data_account(&mut ctx, &admin.pubkey());

    let ix = common::build_initialize_protocol(
        &admin.pubkey(),
        &Pubkey::default(), // zero address
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

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 10, "initialize with zero fee_authority");

    let (protocol_config, _) = common::get_protocol_config_pda();
    assert!(
        ctx.banks_client
            .get_account(protocol_config)
            .await
            .unwrap()
            .is_none(),
        "Protocol config should not be created for zero fee_authority"
    );

    // Neighbor boundary: zero whitelist_manager also fails with InvalidAddress.
    let good_fee_authority = Keypair::new();
    let ix_zero_wl = common::build_initialize_protocol(
        &admin.pubkey(),
        &good_fee_authority.pubkey(),
        &Pubkey::default(),
        &blacklist_program.pubkey(),
        500,
    );
    let tx_zero_wl = Transaction::new_signed_with_payer(
        &[ix_zero_wl],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    let err_zero_wl = ctx
        .banks_client
        .process_transaction(tx_zero_wl)
        .await
        .unwrap_err();
    assert_custom_code(&err_zero_wl, 10, "initialize with zero whitelist_manager");
    assert!(
        ctx.banks_client
            .get_account(protocol_config)
            .await
            .unwrap()
            .is_none(),
        "Protocol config should not be created for zero whitelist_manager"
    );

    // Valid non-zero addresses succeed.
    let valid_fee_authority = Keypair::new();
    let valid_whitelist_manager = Keypair::new();
    let valid_blacklist_program = Keypair::new();
    let ix_ok = common::build_initialize_protocol(
        &admin.pubkey(),
        &valid_fee_authority.pubkey(),
        &valid_whitelist_manager.pubkey(),
        &valid_blacklist_program.pubkey(),
        500,
    );
    let tx_ok = Transaction::new_signed_with_payer(
        &[ix_ok],
        Some(&admin.pubkey()),
        &[&admin],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client.process_transaction(tx_ok).await.unwrap();

    let config_data = common::get_account_data(&mut ctx, &protocol_config).await;
    let config = common::parse_protocol_config(&config_data);
    assert_eq!(config.is_initialized, 1);
    assert_eq!(config.fee_rate_bps, 500);
    assert_eq!(
        config.fee_authority,
        valid_fee_authority.pubkey().to_bytes()
    );
    assert_eq!(
        config.whitelist_manager,
        valid_whitelist_manager.pubkey().to_bytes()
    );
    assert_eq!(
        config.blacklist_program,
        valid_blacklist_program.pubkey().to_bytes()
    );
}

// ==========================================================================
// Error 3: Unauthorized
// Call set_fee_config with wrong admin.
// ==========================================================================

#[tokio::test]
async fn test_err_003_unauthorized() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let wrong_admin = Keypair::new();

    common::airdrop_multiple(&mut ctx, &[&admin, &wrong_admin], 10_000_000_000).await;

    // Initialize protocol with the real admin
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Try to set fee config with a DIFFERENT keypair as admin
    let new_fee_authority = Keypair::new();
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let set_ix =
        common::build_set_fee_config(&wrong_admin.pubkey(), &new_fee_authority.pubkey(), 1000);
    let tx = Transaction::new_signed_with_payer(
        &[set_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_admin],
        recent_blockhash,
    );

    // Capture config before failed tx
    let (protocol_config, _) = common::get_protocol_config_pda();
    let config_before = common::get_account_data(&mut ctx, &protocol_config).await;
    let parsed_before = common::parse_protocol_config(&config_before);

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 5, "set_fee_config by non-admin signer");

    // Verify state immutability after failed tx
    let config_after = common::get_account_data(&mut ctx, &protocol_config).await;
    let parsed_after = common::parse_protocol_config(&config_after);
    assert_eq!(
        config_before, config_after,
        "Protocol config changed on failed tx"
    );
    assert_eq!(parsed_before.admin, parsed_after.admin);
    assert_eq!(parsed_before.fee_authority, parsed_after.fee_authority);
    assert_eq!(parsed_before.fee_rate_bps, parsed_after.fee_rate_bps);

    // Neighbor control: same update succeeds with the real admin signer.
    let authorized_ix =
        common::build_set_fee_config(&admin.pubkey(), &new_fee_authority.pubkey(), 1000);
    let authorized_tx = Transaction::new_signed_with_payer(
        &[authorized_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(authorized_tx)
        .await
        .unwrap();

    let config_updated = common::get_account_data(&mut ctx, &protocol_config).await;
    let parsed_updated = common::parse_protocol_config(&config_updated);
    assert_eq!(parsed_updated.admin, admin.pubkey().to_bytes());
    assert_eq!(
        parsed_updated.fee_authority,
        new_fee_authority.pubkey().to_bytes()
    );
    assert_eq!(parsed_updated.fee_rate_bps, 1000);
}

// ==========================================================================
// Error 5: InvalidMaturity
// Create market with maturity_timestamp in the past.
// ==========================================================================

#[tokio::test]
async fn test_err_005_invalid_maturity() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower],
        10_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Whitelist the borrower
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let (protocol_config, _) = common::get_protocol_config_pda();
    let config_before = common::get_account_data(&mut ctx, &protocol_config).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;

    let now = common::PINNED_EPOCH;
    let min_maturity = now + MIN_MATURITY_DELTA;

    // x-1/x/x+1 maturity boundary around the minimum allowed maturity.
    for (idx, (maturity, should_succeed)) in [
        (min_maturity - 1, false),
        (min_maturity, false),
        (min_maturity + 1, true),
    ]
    .into_iter()
    .enumerate()
    {
        let nonce = (idx + 1) as u64;
        let ix = common::build_create_market(
            &borrower.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            nonce,
            800,
            maturity,
            10_000 * USDC,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            ctx.banks_client.get_latest_blockhash().await.unwrap(),
        );

        let (market, _) = common::get_market_pda(&borrower.pubkey(), nonce);
        if should_succeed {
            ctx.banks_client.process_transaction(tx).await.unwrap();
            let market_data = common::get_account_data(&mut ctx, &market).await;
            let parsed_market = common::parse_market(&market_data);
            assert_eq!(
                parsed_market.maturity_timestamp, maturity,
                "created market maturity mismatch at boundary case {idx}"
            );
        } else {
            let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
            assert_custom_code(&err, 3, "create_market with invalid maturity");
            assert!(
                ctx.banks_client
                    .get_account(market)
                    .await
                    .unwrap()
                    .is_none(),
                "market PDA should not be created for invalid maturity case {idx}"
            );
            let config_after = common::get_account_data(&mut ctx, &protocol_config).await;
            let wl_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
            assert_eq!(
                config_before, config_after,
                "protocol config mutated on invalid maturity"
            );
            assert_eq!(wl_before, wl_after, "whitelist mutated on invalid maturity");
        }
    }
}

// ==========================================================================
// Error 6: InvalidCapacity
// Create market with max_total_supply = 0.
// ==========================================================================

#[tokio::test]
async fn test_err_006_invalid_capacity() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower],
        10_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Whitelist the borrower
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let (protocol_config, _) = common::get_protocol_config_pda();
    let config_before = common::get_account_data(&mut ctx, &protocol_config).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let maturity = common::PINNED_EPOCH + MIN_MATURITY_DELTA + 1;

    // x-1/x/x+1 around the minimum valid capacity threshold (1 unit).
    for (idx, (capacity, should_succeed)) in [(0u64, false), (1u64, true), (2u64, true)]
        .into_iter()
        .enumerate()
    {
        let nonce = (idx + 1) as u64;
        let ix = common::build_create_market(
            &borrower.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            nonce,
            800,
            maturity,
            capacity,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            ctx.banks_client.get_latest_blockhash().await.unwrap(),
        );

        let (market, _) = common::get_market_pda(&borrower.pubkey(), nonce);
        if should_succeed {
            ctx.banks_client.process_transaction(tx).await.unwrap();
            let market_data = common::get_account_data(&mut ctx, &market).await;
            let parsed_market = common::parse_market(&market_data);
            assert_eq!(
                parsed_market.max_total_supply, capacity,
                "market max_total_supply mismatch for capacity boundary case {idx}"
            );
        } else {
            let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
            assert_custom_code(&err, 2, "create_market with zero capacity");
            assert!(
                ctx.banks_client
                    .get_account(market)
                    .await
                    .unwrap()
                    .is_none(),
                "market PDA should not be created for invalid capacity case {idx}"
            );
            let config_after = common::get_account_data(&mut ctx, &protocol_config).await;
            let wl_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
            assert_eq!(
                config_before, config_after,
                "protocol config mutated on invalid capacity"
            );
            assert_eq!(wl_before, wl_after, "whitelist mutated on invalid capacity");
        }
    }
}

// ==========================================================================
// Error 7: InvalidMint
// Create market with a mint that has 9 decimals (not 6).
// ==========================================================================

#[tokio::test]
async fn test_err_007_invalid_mint() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower],
        10_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Whitelist the borrower
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (protocol_config, _) = common::get_protocol_config_pda();
    let config_before = common::get_account_data(&mut ctx, &protocol_config).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let maturity = common::PINNED_EPOCH + MIN_MATURITY_DELTA + 1;

    let mint_5 = common::create_random_mint(&mut ctx, &admin, 5).await;
    let mint_6 = common::create_mint(&mut ctx, &admin, 6).await;
    let mint_7 = common::create_random_mint(&mut ctx, &admin, 7).await;

    for (idx, (mint, should_succeed, expected_error)) in [
        (mint_5, false, Some(11u32)),
        (mint_6, true, None),
        (mint_7, false, Some(11u32)),
    ]
    .into_iter()
    .enumerate()
    {
        let nonce = (idx + 1) as u64;
        let ix = common::build_create_market(
            &borrower.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            nonce,
            800,
            maturity,
            10_000 * USDC,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            ctx.banks_client.get_latest_blockhash().await.unwrap(),
        );

        let (market, _) = common::get_market_pda(&borrower.pubkey(), nonce);
        if should_succeed {
            ctx.banks_client.process_transaction(tx).await.unwrap();
            let market_data = common::get_account_data(&mut ctx, &market).await;
            let parsed_market = common::parse_market(&market_data);
            assert_eq!(parsed_market.mint, mint.to_bytes());
        } else {
            let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
            assert_custom_code(
                &err,
                expected_error.expect("error code must be present"),
                "create_market with non-USDC mint decimals",
            );
            assert!(
                ctx.banks_client
                    .get_account(market)
                    .await
                    .unwrap()
                    .is_none(),
                "market PDA should not be created for invalid mint case {idx}"
            );
            let config_after = common::get_account_data(&mut ctx, &protocol_config).await;
            let wl_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
            assert_eq!(
                config_before, config_after,
                "protocol config mutated on invalid mint"
            );
            assert_eq!(wl_before, wl_after, "whitelist mutated on invalid mint");
        }
    }
}

// ==========================================================================
// Error 8: Blacklisted
// Setup blacklist account with status=1 for borrower, then try to create market.
// ==========================================================================

#[tokio::test]
async fn test_err_008_blacklisted() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager, &borrower],
        10_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Whitelist the borrower
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + MIN_MATURITY_DELTA + 1;

    // Boundary neighbor around blacklist status bit:
    // status=0 succeeds, status=1 fails and is atomic.
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    let allow_ix = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
    );
    let allow_tx = Transaction::new_signed_with_payer(
        &[allow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(allow_tx)
        .await
        .unwrap();

    let (protocol_config, _) = common::get_protocol_config_pda();
    let config_before = common::get_account_data(&mut ctx, &protocol_config).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let (market_one, _) = common::get_market_pda(&borrower.pubkey(), 1);
    let market_one_before = common::get_account_data(&mut ctx, &market_one).await;

    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 1);
    let deny_ix = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2,
        500,
        maturity + 1,
        10_000 * USDC,
    );
    let deny_tx = Transaction::new_signed_with_payer(
        &[deny_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );

    let err = ctx
        .banks_client
        .process_transaction(deny_tx)
        .await
        .unwrap_err();
    assert_custom_code(&err, 7, "create_market for blacklisted borrower");

    let (market_two, _) = common::get_market_pda(&borrower.pubkey(), 2);
    assert!(
        ctx.banks_client
            .get_account(market_two)
            .await
            .unwrap()
            .is_none(),
        "market should not be created for blacklisted borrower"
    );
    let config_after = common::get_account_data(&mut ctx, &protocol_config).await;
    let wl_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let market_one_after = common::get_account_data(&mut ctx, &market_one).await;
    assert_eq!(
        config_before, config_after,
        "Protocol config changed on failed tx"
    );
    assert_eq!(
        wl_before, wl_after,
        "whitelist mutated on blacklist rejection"
    );
    assert_eq!(
        market_one_before, market_one_after,
        "existing market mutated by rejected blacklisted create_market"
    );
}

// ==========================================================================
// Error 11: ZeroAmount
// Call deposit with amount=0.
// ==========================================================================

#[tokio::test]
async fn test_err_011_zero_amount() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
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

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 86400 * 365;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed_market = common::parse_market(&market_data);
    assert_eq!(parsed_market.maturity_timestamp, maturity);

    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Deposit 0 amount
    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        0, // zero amount
    );

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let lender_token_before_fail =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let vault_before_fail = common::get_token_balance(&mut ctx, &vault).await;
    let snap_before = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 17, "deposit amount=0");

    // Verify state immutability after failed tx
    let snap_after = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await,
        lender_token_before_fail
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        vault_before_fail
    );

    // x/x+1 neighbors around zero: deposits of 1 and 2 base units both succeed.
    for amount in [1u64, 2u64] {
        let ok_ix = common::build_deposit(
            &market,
            &lender.pubkey(),
            &lender_token_kp.pubkey(),
            &mint,
            &blacklist_program.pubkey(),
            amount,
        );
        let ok_tx = Transaction::new_signed_with_payer(
            &[ok_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            ctx.banks_client.get_latest_blockhash().await.unwrap(),
        );
        ctx.banks_client.process_transaction(ok_tx).await.unwrap();
    }

    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let position_data = common::get_account_data(&mut ctx, &lender_position).await;
    let parsed_position = common::parse_lender_position(&position_data);
    assert_eq!(parsed_position.scaled_balance, 3u128);

    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed_market = common::parse_market(&market_data);
    assert_eq!(parsed_market.scaled_total_supply, 3u128);
    assert_eq!(parsed_market.total_deposited, 3u64);
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        vault_before_fail + 3
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await,
        lender_token_before_fail - 3
    );
}

// ==========================================================================
// Error 12: MarketMatured
// Create market, advance clock past maturity, try to deposit.
// ==========================================================================

#[tokio::test]
async fn test_err_012_market_matured() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
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

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY; // just above MIN_MATURITY_DELTA

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;
    let market_data = common::get_account_data(&mut ctx, &market).await;
    let parsed_market = common::parse_market(&market_data);
    assert_eq!(parsed_market.maturity_timestamp, maturity_timestamp);

    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let (vault, _) = common::get_vault_pda(&market);
    let deposit_ix_pre = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        10 * USDC,
    );

    // x-1 boundary: deposit succeeds one second before maturity.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp - 1).await;
    let ok_tx = Transaction::new_signed_with_payer(
        &[deposit_ix_pre],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(ok_tx).await.unwrap();

    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_before_fail = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let vault_before_fail = common::get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // x boundary: deposit at maturity fails with MarketMatured and is atomic.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp).await;
    // Use a distinct amount from the x-1 tx so the signature cannot be replay-cached.
    let deposit_ix_at_maturity = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        11 * USDC,
    );
    let at_maturity_tx = Transaction::new_signed_with_payer(
        &[deposit_ix_at_maturity],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let at_maturity_err = ctx
        .banks_client
        .process_transaction(at_maturity_tx)
        .await
        .unwrap_err();
    assert_custom_code(&at_maturity_err, 28, "deposit at maturity timestamp");
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await,
        lender_before_fail
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        vault_before_fail
    );

    // x+1 boundary: deposit one second after maturity also fails and is atomic.
    common::get_blockhash_pinned(&mut ctx, maturity_timestamp + 1).await;
    let deposit_ix_after_maturity = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        12 * USDC,
    );
    let after_maturity_tx = Transaction::new_signed_with_payer(
        &[deposit_ix_after_maturity],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let after_maturity_err = ctx
        .banks_client
        .process_transaction(after_maturity_tx)
        .await
        .unwrap_err();
    assert_custom_code(&after_maturity_err, 28, "deposit after maturity timestamp");
    let snap_after_plus_one =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after_plus_one);
}

// ==========================================================================
// Error 13: NotWhitelisted
// Try to create market for a borrower that is NOT whitelisted.
// ==========================================================================

#[tokio::test]
async fn test_err_013_not_whitelisted() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    // Create whitelist account, then disable it (is_whitelisted=0).
    let wl_create_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000 * USDC,
    );
    let wl_create_tx = Transaction::new_signed_with_payer(
        &[wl_create_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(wl_create_tx)
        .await
        .unwrap();

    let wl_disabled_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        0,
        50_000 * USDC,
    );
    let wl_disabled_tx = Transaction::new_signed_with_payer(
        &[wl_disabled_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(wl_disabled_tx)
        .await
        .unwrap();

    let (protocol_config, _) = common::get_protocol_config_pda();
    let config_before = common::get_account_data(&mut ctx, &protocol_config).await;
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let wl_parsed_before = common::parse_borrower_whitelist(&wl_before);
    assert_eq!(wl_parsed_before.is_whitelisted, 0);

    let maturity = common::PINNED_EPOCH + MIN_MATURITY_DELTA + 1;
    let ix = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        800,
        maturity,
        10_000 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 6, "create_market with whitelist flag disabled");

    let (market_disabled, _) = common::get_market_pda(&borrower.pubkey(), 1);
    assert!(
        ctx.banks_client
            .get_account(market_disabled)
            .await
            .unwrap()
            .is_none(),
        "market should not be created when borrower whitelist flag is 0"
    );

    // Verify state immutability after failed tx.
    let config_after = common::get_account_data(&mut ctx, &protocol_config).await;
    let wl_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        config_before, config_after,
        "protocol config mutated on NotWhitelisted"
    );
    assert_eq!(wl_before, wl_after, "whitelist mutated on NotWhitelisted");

    // Neighbor boundary: flip whitelist to 1 and the same create_market path succeeds.
    let wl_enabled_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000 * USDC,
    );
    let wl_enabled_tx = Transaction::new_signed_with_payer(
        &[wl_enabled_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(wl_enabled_tx)
        .await
        .unwrap();

    let ix_ok = common::build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        2,
        800,
        maturity + 1,
        10_000 * USDC,
    );
    let tx_ok = Transaction::new_signed_with_payer(
        &[ix_ok],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client.process_transaction(tx_ok).await.unwrap();

    let (market_enabled, _) = common::get_market_pda(&borrower.pubkey(), 2);
    let market_data = common::get_account_data(&mut ctx, &market_enabled).await;
    let parsed_market = common::parse_market(&market_data);
    assert_eq!(parsed_market.borrower, borrower.pubkey().to_bytes());
}

// ==========================================================================
// Error 14: CapExceeded
// Create market with small cap, deposit exceeding cap.
// ==========================================================================

#[tokio::test]
async fn test_err_014_cap_exceeded() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0, // zero fees to prevent fee accrual from shifting cap boundary
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    // Market with max_total_supply = 100 USDC
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // zero interest to prevent accrual from shifting cap boundary
        common::FAR_FUTURE_MATURITY,
        100 * USDC, // small cap
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;
    common::setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // x-1 boundary: deposit to cap-1 succeeds.
    // Use a single deposit of cap-1 to avoid multiple blockhash fetches.
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let deposit_cap_minus_one_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC - 1,
    );
    let deposit_cap_minus_one_tx = Transaction::new_signed_with_payer(
        &[deposit_cap_minus_one_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(deposit_cap_minus_one_tx)
        .await
        .unwrap();

    // x boundary: deposit exactly 1 more unit reaches cap and succeeds.
    // No new blockhash needed — amount (1) differs from prior (cap-1), so
    // the transaction signature is unique within the same bank.
    let deposit_to_exact_cap_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let deposit_to_exact_cap_tx = Transaction::new_signed_with_payer(
        &[deposit_to_exact_cap_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(deposit_to_exact_cap_tx)
        .await
        .unwrap();

    let market_data_at_cap = common::get_account_data(&mut ctx, &market).await;
    let parsed_market_at_cap = common::parse_market(&market_data_at_cap);
    assert_eq!(parsed_market_at_cap.total_deposited, 100 * USDC);

    // x+1 boundary: one more unit exceeds cap and must fail atomically.
    // Build the transaction and snapshot BEFORE fetching a new blockhash
    // to minimize the window for clock drift.
    let deposit_over_cap_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_before_fail = common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let vault_before_fail = common::get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // Verify the market is truly at cap before the over-cap attempt.
    let market_data_before_fail = common::get_account_data(&mut ctx, &market).await;
    let parsed_before_fail = common::parse_market(&market_data_before_fail);
    assert_eq!(
        parsed_before_fail.total_deposited,
        100 * USDC,
        "total_deposited should be exactly at cap before over-cap attempt"
    );
    assert_eq!(
        parsed_before_fail.scale_factor, 1_000_000_000_000_000_000u128,
        "scale_factor should be WAD (no interest accrued)"
    );

    // Same bank as prior deposit(1) — prepend ComputeBudget to differentiate
    // the signature (same amount + same signers would otherwise deduplicate).
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, deposit_over_cap_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 25, "deposit above market cap");

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await,
        lender_before_fail
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        vault_before_fail
    );
}

// ==========================================================================
// Error 17: BorrowAmountTooHigh
// Deposit small amount, try to borrow more than vault balance.
// ==========================================================================

#[tokio::test]
async fn test_err_017_borrow_amount_too_high() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
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

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 86400 * 365;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit only 100 USDC
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // x+1 boundary: try to borrow 101 USDC when only 100 USDC is available.
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let borrow_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        101 * USDC,
    );

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let borrower_tokens_before_fail =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let snap_before = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 26, "borrow amount above available vault liquidity");

    // Verify state immutability after failed tx
    let snap_after = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snap_before.assert_unchanged(&snap_after);
    let wl_after = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        wl_before, wl_after,
        "whitelist mutated on failed over-borrow"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await,
        borrower_tokens_before_fail
    );

    // x boundary: borrowing exactly available liquidity succeeds.
    let borrow_exact_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
    );
    let borrow_exact_tx = Transaction::new_signed_with_payer(
        &[borrow_exact_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(borrow_exact_tx)
        .await
        .unwrap();

    let wl_success = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let parsed_wl = common::parse_borrower_whitelist(&wl_success);
    assert_eq!(parsed_wl.current_borrowed, 100 * USDC);
    assert_eq!(common::get_token_balance(&mut ctx, &vault).await, 0);
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await,
        borrower_tokens_before_fail + 100 * USDC
    );
}

// ==========================================================================
// Error 18: NotMatured
// Try to withdraw before maturity.
// ==========================================================================

#[tokio::test]
async fn test_err_018_not_matured() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
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

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 86400 * 365; // far in the future

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_tokens_before_fail =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let vault_before_fail = common::get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    // x-1 boundary: one second before maturity fails with NotMatured.
    common::advance_clock_past(&mut ctx, maturity - 1).await;
    let withdraw_pre_maturity_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full withdrawal
    );
    let pre_maturity_tx = Transaction::new_signed_with_payer(
        &[withdraw_pre_maturity_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    let pre_maturity_err = ctx
        .banks_client
        .process_transaction(pre_maturity_tx)
        .await
        .unwrap_err();
    assert_custom_code(&pre_maturity_err, 29, "withdraw before maturity");

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await,
        lender_tokens_before_fail
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        vault_before_fail
    );

    // x boundary: at exact maturity, withdraw reaches settlement grace gate.
    common::advance_clock_past(&mut ctx, maturity).await;
    // Use a distinct min_payout so this tx cannot be replay-cached from x-1.
    let at_maturity_tx = Transaction::new_signed_with_payer(
        &[common::build_withdraw(
            &market,
            &lender.pubkey(),
            &lender_token_kp.pubkey(),
            &blacklist_program.pubkey(),
            0u128,
            1,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    let at_maturity_err = ctx
        .banks_client
        .process_transaction(at_maturity_tx)
        .await
        .unwrap_err();
    assert_custom_code(
        &at_maturity_err,
        32,
        "withdraw at maturity before settlement grace",
    );
    let snap_at_maturity =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_at_maturity);

    // x+1 boundary after grace window: full withdraw succeeds.
    common::advance_clock_past(&mut ctx, maturity + 301).await;
    let post_grace_tx = Transaction::new_signed_with_payer(
        &[common::build_withdraw(
            &market,
            &lender.pubkey(),
            &lender_token_kp.pubkey(),
            &blacklist_program.pubkey(),
            0u128,
            2,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(post_grace_tx)
        .await
        .unwrap();

    let lender_position_data = common::get_account_data(&mut ctx, &lender_position).await;
    let parsed_position = common::parse_lender_position(&lender_position_data);
    assert_eq!(parsed_position.scaled_balance, 0u128);
}

// ==========================================================================
// Error 19: NoBalance
// Deposit, advance past maturity, withdraw all, then try to withdraw again.
// ==========================================================================

#[tokio::test]
async fn test_err_019_no_balance() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
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

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance clock past maturity
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // First withdraw -- full withdrawal (scaled_amount=0)
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("first full withdrawal should succeed");

    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let lender_position_after_full = common::get_account_data(&mut ctx, &lender_position).await;
    let parsed_after_full = common::parse_lender_position(&lender_position_after_full);
    assert_eq!(parsed_after_full.scaled_balance, 0u128);

    // Second withdraw (scaled_amount=0 full) -- should fail with NoBalance.
    let withdraw_ix_2 = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let lender_tokens_before_fail =
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await;
    let vault_before_fail = common::get_token_balance(&mut ctx, &vault).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 23, "second full withdraw with zero balance");

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token_kp.pubkey()).await,
        lender_tokens_before_fail
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &vault).await,
        vault_before_fail
    );

    // Neighbor check: explicit non-zero scaled amount also fails with NoBalance.
    let withdraw_ix_3 = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        1u128,
        0,
    );
    let tx_3 = Transaction::new_signed_with_payer(
        &[withdraw_ix_3],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    let err_3 = ctx
        .banks_client
        .process_transaction(tx_3)
        .await
        .unwrap_err();
    assert_custom_code(&err_3, 23, "withdraw scaled_amount=1 with zero balance");
    let snap_after_third =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after_third);
}

// ==========================================================================
// Error 22: NoFeesToCollect
// Create market with fee_rate=0, no fees accrued, try to collect.
// ==========================================================================

#[tokio::test]
async fn test_err_022_no_fees_to_collect() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &fee_authority,
            &borrower,
            &whitelist_manager,
            &lender,
        ],
        10_000_000_000,
    )
    .await;

    // Initialize with fee_rate = 0 so no fees accrue
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        0, // fee_rate_bps = 0
    )
    .await;

    let mint_authority = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    // Short maturity so minimal interest accrues — this test is about the
    // NoFeesToCollect error path, not interest/settlement mechanics.
    let maturity_timestamp = common::PINNED_EPOCH + 86_400;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_timestamp,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit so the market has some activity
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
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
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // SR-113: Lender must withdraw before fee collection
    // Advance past maturity + grace period (300s)
    common::advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Fund vault with interest so settlement_factor = WAD (no distress)
    // Even though fee_rate=0, interest still accrues on the scale factor
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        10 * USDC,
    )
    .await;
    let repay_interest_ix = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        10 * USDC,
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender withdraws full balance
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0, // 0 = full withdrawal
        0, // no minimum payout
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create fee destination token account
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;

    // Collect fees should fail with NoFeesToCollect since fee_rate=0.
    let collect_ix =
        common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let market_before = common::get_account_data(&mut ctx, &market).await;
    let parsed_market_before = common::parse_market(&market_before);
    assert_eq!(parsed_market_before.accrued_protocol_fees, 0);
    let fee_dest_before = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let tx = Transaction::new_signed_with_payer(
        &[collect_ix.clone()],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 36, "collect_fees with zero accrued fees");

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await,
        fee_dest_before
    );
    let market_after = common::get_account_data(&mut ctx, &market).await;
    let parsed_market_after = common::parse_market(&market_after);
    assert_eq!(parsed_market_after.accrued_protocol_fees, 0);

    // Deterministic repeated failure (no hidden state drift).
    let tx_repeat = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    let err_repeat = ctx
        .banks_client
        .process_transaction(tx_repeat)
        .await
        .unwrap_err();
    assert_custom_code(
        &err_repeat,
        36,
        "collect_fees repeated with zero accrued fees",
    );
    let snap_repeat =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_repeat);
}

// ==========================================================================
// Error 27: PositionNotEmpty
// Deposit, then try to close position without withdrawing.
// ==========================================================================

#[tokio::test]
async fn test_err_027_position_not_empty() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
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

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 86400 * 365;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to close position WITHOUT withdrawing first
    let close_ix = common::build_close_lender_position(&market, &lender.pubkey());

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 34, "close non-empty lender position");

    // Verify state immutability after failed tx
    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);

    // Neighbor boundary: after full withdrawal, close succeeds and account is deleted.
    common::advance_clock_past(&mut ctx, maturity + 301).await;
    let withdraw_full_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let withdraw_full_tx = Transaction::new_signed_with_payer(
        &[withdraw_full_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(withdraw_full_tx)
        .await
        .unwrap();

    let close_ok_ix = common::build_close_lender_position(&market, &lender.pubkey());
    let close_ok_tx = Transaction::new_signed_with_payer(
        &[close_ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(close_ok_tx)
        .await
        .unwrap();

    assert!(
        ctx.banks_client
            .get_account(lender_position)
            .await
            .unwrap()
            .is_none(),
        "lender position account should be closed after zero-balance close"
    );
}

// ==========================================================================
// Error 29: NotSettled
// Create market, try to call re_settle before any withdrawal
// (settlement_factor=0).
// ==========================================================================

#[tokio::test]
async fn test_err_029_not_settled() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
        ],
        10_000_000_000,
    )
    .await;

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
    common::airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    let maturity_ts = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Deposit (no borrow, no withdraw)
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;

    let dep_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Call re_settle without any withdrawal (settlement_factor_wad = 0)
    let (vault, _) = common::get_vault_pda(&market);
    let rs_ix = common::build_re_settle(&market, &vault);

    // Snapshot state before failed transaction
    let snap_before = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[rs_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        recent_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 30, "re_settle before first settlement");

    // Verify state immutability after failed tx
    let snap_after = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snap_before.assert_unchanged(&snap_after);

    // Boundary neighbor: after first settlement is established, re_settle fails
    // with SettlementNotImproved when there is no improvement.
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;
    let withdraw_full_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let withdraw_full_tx = Transaction::new_signed_with_payer(
        &[withdraw_full_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(withdraw_full_tx)
        .await
        .unwrap();

    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let mut last_factor = common::parse_market(&common::get_account_data(&mut ctx, &market).await)
        .settlement_factor_wad;
    let mut observed_not_improved = false;

    // ReSettle may improve multiple times depending on accrued-interest rounding;
    // require monotonic strict improvement until it deterministically reaches
    // SettlementNotImproved.
    for _attempt in 0..8 {
        let snap_before_attempt =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
        let budget_ix =
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
                250_000 + _attempt * 1_000,
            );
        let rs_attempt_tx = Transaction::new_signed_with_payer(
            &[budget_ix, common::build_re_settle(&market, &vault)],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.banks_client.get_latest_blockhash().await.unwrap(),
        );

        match ctx.banks_client.process_transaction(rs_attempt_tx).await {
            Ok(()) => {
                let market_after = common::get_account_data(&mut ctx, &market).await;
                let parsed_after = common::parse_market(&market_after);
                assert!(
                    parsed_after.settlement_factor_wad > last_factor,
                    "successful re_settle must strictly improve settlement factor"
                );
                assert!(
                    parsed_after.settlement_factor_wad <= WAD,
                    "settlement factor must remain capped at WAD"
                );
                last_factor = parsed_after.settlement_factor_wad;
            },
            Err(err_again) => {
                assert_custom_code(&err_again, 31, "re_settle without settlement improvement");
                let snap_after_attempt = common::ProtocolSnapshot::capture(
                    &mut ctx,
                    &market,
                    &vault,
                    &[lender_position],
                )
                .await;
                snap_before_attempt.assert_unchanged(&snap_after_attempt);
                observed_not_improved = true;
                break;
            },
        }
    }

    assert!(
        observed_not_improved,
        "expected to observe SettlementNotImproved after repeated re_settle attempts"
    );
    // One more deterministic check: once NotImproved is reached, it remains NotImproved.
    let budget_ix_final =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(260_000);
    let final_err_tx = Transaction::new_signed_with_payer(
        &[budget_ix_final, common::build_re_settle(&market, &vault)],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    if let Err(final_err) = ctx.banks_client.process_transaction(final_err_tx).await {
        assert_custom_code(&final_err, 31, "re_settle remains not-improved");
    } else {
        // If one more improvement still exists, it must remain bounded by WAD.
        let market_after_extra = common::get_account_data(&mut ctx, &market).await;
        let parsed_after_extra = common::parse_market(&market_after_extra);
        assert!(parsed_after_extra.settlement_factor_wad <= WAD);
    }
}

// ==========================================================================
// Error 30: GlobalCapacityExceeded
// Whitelist with max_borrow_capacity=500 USDC. Deposit 1000 USDC.
// Borrow 501 USDC -- exceeds global capacity.
// ==========================================================================

#[tokio::test]
async fn test_err_030_global_capacity_exceeded() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager],
        10_000_000_000,
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

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity = common::PINNED_EPOCH + 86400 * 365;

    // Set max_borrow_capacity = 500 USDC (500_000_000 in 6-decimal units)
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        500,
        maturity,
        10_000 * USDC,
        &whitelist_manager,
        500 * USDC, // max_borrow_capacity = 500 USDC
    )
    .await;

    // Deposit 1000 USDC
    let lender = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender], 5_000_000_000).await;

    let lender_token_kp = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token_kp.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    let deposit_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token_kp.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // x boundary: borrow exactly to global capacity succeeds.
    let borrower_token_kp = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_exact_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let borrow_exact_tx = Transaction::new_signed_with_payer(
        &[borrow_exact_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.banks_client.get_latest_blockhash().await.unwrap(),
    );
    ctx.banks_client
        .process_transaction(borrow_exact_tx)
        .await
        .unwrap();

    let (borrower_whitelist, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_at_capacity = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    let parsed_wl_at_capacity = common::parse_borrower_whitelist(&wl_at_capacity);
    assert_eq!(parsed_wl_at_capacity.current_borrowed, 500 * USDC);

    // x+1 boundary: one additional unit exceeds capacity and must fail atomically.
    let borrow_over_ix = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token_kp.pubkey(),
        &blacklist_program.pubkey(),
        1,
    );

    // Snapshot state before failed transaction
    let (vault, _) = common::get_vault_pda(&market);
    let borrower_tokens_before_fail =
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await;
    let snap_before = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_over_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent_blockhash,
    );

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_custom_code(&err, 27, "borrow beyond whitelist global capacity");

    // Verify state immutability after failed tx
    let snap_after = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[]).await;
    snap_before.assert_unchanged(&snap_after);
    let wl_after_fail = common::get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        wl_at_capacity, wl_after_fail,
        "borrower whitelist mutated on failed capacity-exceeded borrow"
    );
    assert_eq!(
        common::get_token_balance(&mut ctx, &borrower_token_kp.pubkey()).await,
        borrower_tokens_before_fail
    );
}
