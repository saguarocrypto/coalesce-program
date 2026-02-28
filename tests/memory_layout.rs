//! Memory layout tests for CoalesceFi account structs.
//!
//! Verifies exact sizes, byte offsets, discriminator placement, and
//! BPF roundtrip integrity for all account types.

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
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};

// Inline the struct definitions for size checks — we use bytemuck for raw inspection
// The actual struct definitions are in the on-chain program, but we verify against
// the sizes defined in common/mod.rs which mirror the on-chain constants.

// Note: Account sizes are validated via BPF roundtrip in test_bpf_roundtrip_market
// and test_bpf_roundtrip_protocol_config (assert data.len() == expected size).

// ===========================================================================
// Test 5: Market discriminator at bytes 0..8, version at byte 8
// ===========================================================================
#[tokio::test]
async fn test_market_discriminator_offset_0() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &borrower, &wm], 10_000_000_000).await;
    let mint = create_mint(&mut ctx, &ma, 6).await;

    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + 365 * 86400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        1000,
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    let data = get_account_data(&mut ctx, &market).await;

    // Discriminator at bytes 0..8
    assert_eq!(&data[0..8], b"COALMKT_", "market discriminator at offset 0");
    // Version at byte 8
    assert_eq!(data[8], 1, "market version at offset 8 should be 1");
}

// ===========================================================================
// Test 6: ProtocolConfig field offsets
// ===========================================================================
#[tokio::test]
async fn test_protocol_config_field_offsets() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fa = Keypair::new();
    let wm = Keypair::new();
    let bp = Pubkey::new_unique();

    airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 1234).await;

    let (config_pda, _) = get_protocol_config_pda();
    let data = get_account_data(&mut ctx, &config_pda).await;

    // Discriminator at 0..8
    assert_eq!(&data[0..8], b"COALPC__", "protocol config discriminator");
    // Version at byte 8
    assert_eq!(data[8], 1, "protocol config version");
    // Admin at 9..41
    assert_eq!(&data[9..41], admin.pubkey().as_ref(), "admin field");
    // fee_rate_bps at 41..43 (LE u16)
    let fee_rate = u16::from_le_bytes([data[41], data[42]]);
    assert_eq!(fee_rate, 1234, "fee_rate_bps field");
    // fee_authority at 43..75
    assert_eq!(&data[43..75], fa.pubkey().as_ref(), "fee_authority field");
    // whitelist_manager at 75..107
    assert_eq!(
        &data[75..107],
        wm.pubkey().as_ref(),
        "whitelist_manager field"
    );
    // blacklist_program at 107..139
    assert_eq!(&data[107..139], bp.as_ref(), "blacklist_program field");
    // is_initialized at 139
    assert_eq!(data[139], 1, "is_initialized field");
}

// ===========================================================================
// Test 7: Market field offsets
// ===========================================================================
#[tokio::test]
async fn test_market_field_offsets() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &borrower, &wm], 10_000_000_000).await;
    let mint = create_mint(&mut ctx, &ma, 6).await;

    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + 365 * 86400;
    let nonce = 0u64;
    let annual_bps = 1000u16;
    let max_supply = 1_000_000_000u64;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        nonce,
        annual_bps,
        maturity,
        max_supply,
        &wm,
        1_000_000_000,
    )
    .await;

    let data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&data);

    // Verify parsed fields match what we set
    assert_eq!(parsed.borrower, borrower.pubkey().to_bytes());
    assert_eq!(parsed.mint, mint.to_bytes());
    assert_eq!(parsed.annual_interest_bps, annual_bps);
    assert_eq!(parsed.maturity_timestamp, maturity);
    assert_eq!(parsed.max_total_supply, max_supply);
    assert_eq!(parsed.market_nonce, nonce);
    assert!(
        parsed.scale_factor > 0,
        "scale factor should be initialized"
    );
    assert_eq!(parsed.total_deposited, 0);
    assert_eq!(parsed.total_borrowed, 0);
}

// ===========================================================================
// Test 8: LenderPosition field offsets
// ===========================================================================
#[tokio::test]
async fn test_lender_position_field_offsets() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &borrower, &lender, &wm], 10_000_000_000).await;
    let mint = create_mint(&mut ctx, &ma, 6).await;
    let lta = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(&mut ctx, &mint, &lta.pubkey(), &ma, 10_000_000_000).await;

    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &bp, &lender.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + 365 * 86400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        0,
        1000,
        maturity,
        1_000_000_000,
        &wm,
        1_000_000_000,
    )
    .await;

    // Deposit to create lender position
    let dep = build_deposit(
        &market,
        &lender.pubkey(),
        &lta.pubkey(),
        &mint,
        &bp,
        50_000_000,
    );
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (lp_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let data = get_account_data(&mut ctx, &lp_pda).await;
    let parsed = parse_lender_position(&data);

    assert_eq!(parsed.market, market.to_bytes());
    assert_eq!(parsed.lender, lender.pubkey().to_bytes());
    assert!(
        parsed.scaled_balance > 0,
        "should have non-zero balance after deposit"
    );
    // Discriminator check
    assert_eq!(&data[0..8], b"COALLPOS", "lender position discriminator");
    assert_eq!(data[8], 1, "lender position version");
}

// ===========================================================================
// Test 9: BPF roundtrip — create market, read back, verify all fields
// ===========================================================================
#[tokio::test]
async fn test_bpf_roundtrip_market() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let wm = Keypair::new();
    let fa = Keypair::new();
    let bp = Pubkey::new_unique();
    let ma = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &borrower, &wm], 10_000_000_000).await;
    let mint = create_mint(&mut ctx, &ma, 6).await;

    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 500).await;
    setup_blacklist_account(&mut ctx, &bp, &borrower.pubkey(), 0);

    let maturity = common::PINNED_EPOCH + 365 * 86400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &bp,
        42,
        5000,
        maturity,
        999_000_000,
        &wm,
        999_000_000,
    )
    .await;

    let data = get_account_data(&mut ctx, &market).await;
    assert_eq!(data.len(), MARKET_SIZE);

    let parsed = parse_market(&data);
    assert_eq!(parsed.borrower, borrower.pubkey().to_bytes());
    assert_eq!(parsed.mint, mint.to_bytes());
    assert_eq!(parsed.annual_interest_bps, 5000);
    assert_eq!(parsed.maturity_timestamp, maturity);
    assert_eq!(parsed.max_total_supply, 999_000_000);
    assert_eq!(parsed.market_nonce, 42);
    assert_eq!(parsed.total_deposited, 0);
    assert_eq!(parsed.total_borrowed, 0);
    assert_eq!(parsed.total_repaid, 0);
    assert_eq!(parsed.total_interest_repaid, 0);
    assert_eq!(parsed.accrued_protocol_fees, 0);
    assert_eq!(parsed.settlement_factor_wad, 0);

    // Vault PDA should be in the market data
    let (expected_vault, _) = get_vault_pda(&market);
    assert_eq!(parsed.vault, expected_vault.to_bytes());
}

// ===========================================================================
// Test 10: BPF roundtrip — init protocol, read back, verify all fields
// ===========================================================================
#[tokio::test]
async fn test_bpf_roundtrip_protocol_config() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fa = Keypair::new();
    let wm = Keypair::new();
    let bp = Pubkey::new_unique();

    airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;
    setup_protocol(&mut ctx, &admin, &fa.pubkey(), &wm.pubkey(), &bp, 7777).await;

    let (config_pda, _) = get_protocol_config_pda();
    let data = get_account_data(&mut ctx, &config_pda).await;
    assert_eq!(data.len(), PROTOCOL_CONFIG_SIZE);

    let parsed = parse_protocol_config(&data);
    assert_eq!(parsed.admin, admin.pubkey().to_bytes());
    assert_eq!(parsed.fee_rate_bps, 7777);
    assert_eq!(parsed.fee_authority, fa.pubkey().to_bytes());
    assert_eq!(parsed.whitelist_manager, wm.pubkey().to_bytes());
    assert_eq!(parsed.blacklist_program, bp.to_bytes());
    assert_eq!(parsed.is_initialized, 1);
}
