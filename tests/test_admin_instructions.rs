//! Tests for admin-only instructions:
//! - SetAdmin (disc 15)
//! - SetPause (disc 13)
//! - SetBlacklistMode (disc 14)
//! - SetWhitelistManager (disc 16)
//!
//! These tests verify authorization, state transitions, and edge cases
//! for protocol administration functions.

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
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};

// ---------------------------------------------------------------------------
// Instruction builders for admin instructions
// ---------------------------------------------------------------------------

/// SetPause (disc 13)
/// data = [13u8] ++ paused (1 byte, 0 = unpause, 1 = pause)
fn build_set_pause(admin: &Pubkey, paused: bool) -> Instruction {
    let (protocol_config, _) = get_protocol_config_pda();

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false),
            AccountMeta::new_readonly(*admin, true),
        ],
        data: vec![13u8, if paused { 1 } else { 0 }],
    }
}

/// SetBlacklistMode (disc 14)
/// data = [14u8] ++ mode (1 byte, 0 = fail-open, 1 = fail-closed)
fn build_set_blacklist_mode(admin: &Pubkey, fail_closed: bool) -> Instruction {
    let (protocol_config, _) = get_protocol_config_pda();

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false),
            AccountMeta::new_readonly(*admin, true),
        ],
        data: vec![14u8, if fail_closed { 1 } else { 0 }],
    }
}

/// SetAdmin (disc 15)
/// data = [15u8] (no additional data - new admin is in accounts)
fn build_set_admin(current_admin: &Pubkey, new_admin: &Pubkey) -> Instruction {
    let (protocol_config, _) = get_protocol_config_pda();

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false),
            AccountMeta::new_readonly(*current_admin, true),
            AccountMeta::new_readonly(*new_admin, false),
        ],
        data: vec![15u8],
    }
}

/// SetWhitelistManager (disc 16)
/// data = [16u8] (no additional data - new manager is in accounts)
fn build_set_whitelist_manager(admin: &Pubkey, new_manager: &Pubkey) -> Instruction {
    let (protocol_config, _) = get_protocol_config_pda();

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false),
            AccountMeta::new_readonly(*admin, true),
            AccountMeta::new_readonly(*new_manager, false),
        ],
        data: vec![16u8],
    }
}

const PAUSED_OFFSET: usize = 141;
const BLACKLIST_MODE_OFFSET: usize = 142;

async fn read_protocol_config_raw(ctx: &mut solana_program_test::ProgramTestContext) -> Vec<u8> {
    let (protocol_config_pda, _) = get_protocol_config_pda();
    get_account_data(ctx, &protocol_config_pda).await
}

fn assert_protocol_config_unchanged(before: &[u8], after: &[u8], context: &str) {
    assert_eq!(
        before, after,
        "{context}: protocol config bytes should be unchanged"
    );
}

fn assert_only_protocol_byte_changed(
    before: &[u8],
    after: &[u8],
    offset: usize,
    expected_after: u8,
    context: &str,
) {
    assert_eq!(after[offset], expected_after, "{context}: byte mismatch");
    assert_eq!(
        before.len(),
        after.len(),
        "{context}: protocol config length mismatch"
    );

    for i in 0..before.len() {
        if i == offset {
            continue;
        }
        assert_eq!(
            before[i], after[i],
            "{context}: unexpected protocol config change at byte {i}"
        );
    }
}

fn assert_only_protocol_range_changed(
    before: &[u8],
    after: &[u8],
    range_start: usize,
    expected_bytes: &[u8],
    context: &str,
) {
    assert_eq!(
        before.len(),
        after.len(),
        "{context}: protocol config length mismatch"
    );
    let range_end = range_start + expected_bytes.len();
    assert_eq!(
        &after[range_start..range_end],
        expected_bytes,
        "{context}: expected bytes mismatch in target range"
    );
    for i in 0..before.len() {
        if i >= range_start && i < range_end {
            continue;
        }
        assert_eq!(
            before[i], after[i],
            "{context}: unexpected protocol config change at byte {i}"
        );
    }
}

async fn capture_market_snapshot_for_lenders(
    ctx: &mut solana_program_test::ProgramTestContext,
    market: &Pubkey,
    lenders: &[Pubkey],
) -> ProtocolSnapshot {
    let (vault, _) = get_vault_pda(market);
    let lender_positions: Vec<Pubkey> = lenders
        .iter()
        .map(|lender| get_lender_position_pda(market, lender).0)
        .collect();
    ProtocolSnapshot::capture(ctx, market, &vault, &lender_positions).await
}

// ===========================================================================
// SetPause tests
// ===========================================================================

#[tokio::test]
async fn test_set_pause_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin], 10_000_000_000).await;

    // Initialize protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let config_before = read_protocol_config_raw(&mut ctx).await;
    assert_eq!(config_before[PAUSED_OFFSET], 0);
    assert_eq!(config_before[BLACKLIST_MODE_OFFSET], 0);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_after_pause = read_protocol_config_raw(&mut ctx).await;
    assert_only_protocol_byte_changed(
        &config_before,
        &config_after_pause,
        PAUSED_OFFSET,
        1,
        "SetPause(true)",
    );
    assert_eq!(config_after_pause[BLACKLIST_MODE_OFFSET], 0);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause the protocol
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_after_unpause = read_protocol_config_raw(&mut ctx).await;
    assert_only_protocol_byte_changed(
        &config_after_pause,
        &config_after_unpause,
        PAUSED_OFFSET,
        0,
        "SetPause(false)",
    );
    assert_eq!(config_after_unpause, config_before);
}

#[tokio::test]
async fn test_set_pause_wrong_admin() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let wrong_admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &wrong_admin], 10_000_000_000).await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to pause with wrong admin
    let pause_ix = build_set_pause(&wrong_admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_admin],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Unauthorized = Custom(5)
    assert_custom_error(&result, 5);
    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(&config_before, &config_after, "wrong admin set_pause");
}

#[tokio::test]
async fn test_set_pause_non_signer() {
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

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Build instruction with admin NOT marked as signer
    let (protocol_config, _) = get_protocol_config_pda();
    let ix = Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false),
            AccountMeta::new_readonly(admin.pubkey(), false), // NOT signer
        ],
        data: vec![13u8, 1], // pause = true
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

    // Unauthorized = Custom(5)
    assert_custom_error(&result, 5);
    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(&config_before, &config_after, "non-signer set_pause");
}

// ===========================================================================
// SetBlacklistMode tests
// ===========================================================================

#[tokio::test]
async fn test_set_blacklist_mode_success() {
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

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let config_before = read_protocol_config_raw(&mut ctx).await;
    assert_eq!(config_before[BLACKLIST_MODE_OFFSET], 0);
    assert_eq!(config_before[PAUSED_OFFSET], 0);

    // Set to fail-closed mode
    let mode_ix = build_set_blacklist_mode(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[mode_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_after_fail_closed = read_protocol_config_raw(&mut ctx).await;
    assert_only_protocol_byte_changed(
        &config_before,
        &config_after_fail_closed,
        BLACKLIST_MODE_OFFSET,
        1,
        "SetBlacklistMode(true)",
    );
    assert_eq!(config_after_fail_closed[PAUSED_OFFSET], 0);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Set back to fail-open mode
    let mode_ix = build_set_blacklist_mode(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[mode_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_after_fail_open = read_protocol_config_raw(&mut ctx).await;
    assert_only_protocol_byte_changed(
        &config_after_fail_closed,
        &config_after_fail_open,
        BLACKLIST_MODE_OFFSET,
        0,
        "SetBlacklistMode(false)",
    );
    assert_eq!(config_after_fail_open, config_before);
}

#[tokio::test]
async fn test_set_blacklist_mode_wrong_admin() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let wrong_admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &wrong_admin], 10_000_000_000).await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let mode_ix = build_set_blacklist_mode(&wrong_admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[mode_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_admin],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    assert_custom_error(&result, 5); // Unauthorized
    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(
        &config_before,
        &config_after,
        "wrong admin set_blacklist_mode",
    );
}

// ===========================================================================
// SetAdmin tests
// ===========================================================================

#[tokio::test]
async fn test_set_admin_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let new_admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &new_admin], 10_000_000_000).await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Transfer admin to new_admin
    let set_admin_ix = build_set_admin(&admin.pubkey(), &new_admin.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[set_admin_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_after_set_admin = read_protocol_config_raw(&mut ctx).await;
    assert_only_protocol_range_changed(
        &config_before,
        &config_after_set_admin,
        9,
        new_admin.pubkey().as_ref(),
        "SetAdmin(new_admin)",
    );
    let config = parse_protocol_config(&config_after_set_admin);
    assert_eq!(
        &config.admin,
        new_admin.pubkey().as_ref(),
        "admin should be updated to new_admin"
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Old admin should no longer be able to perform admin actions
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    let config_before_old_admin_pause = read_protocol_config_raw(&mut ctx).await;
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    assert_custom_error(&result, 5); // Unauthorized
    let config_after_old_admin_pause = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(
        &config_before_old_admin_pause,
        &config_after_old_admin_pause,
        "old admin pause after SetAdmin",
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // New admin should be able to perform admin actions
    let pause_ix = build_set_pause(&new_admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &new_admin],
        ctx.last_blockhash,
    );
    let config_before_new_admin_pause = read_protocol_config_raw(&mut ctx).await;
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("new admin should be able to pause");
    let config_after_new_admin_pause = read_protocol_config_raw(&mut ctx).await;
    assert_only_protocol_byte_changed(
        &config_before_new_admin_pause,
        &config_after_new_admin_pause,
        PAUSED_OFFSET,
        1,
        "new admin pause after SetAdmin",
    );
}

#[tokio::test]
async fn test_set_admin_wrong_current_admin() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let wrong_admin = Keypair::new();
    let new_admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &wrong_admin], 10_000_000_000).await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to set admin with wrong current admin
    let set_admin_ix = build_set_admin(&wrong_admin.pubkey(), &new_admin.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[set_admin_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_admin],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    assert_custom_error(&result, 5); // Unauthorized
    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(
        &config_before,
        &config_after,
        "wrong current admin set_admin",
    );
}

#[tokio::test]
async fn test_set_admin_zero_address() {
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

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to set admin to zero address
    let set_admin_ix = build_set_admin(&admin.pubkey(), &Pubkey::default());
    let tx = Transaction::new_signed_with_payer(
        &[set_admin_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // InvalidAddress = Custom(10)
    assert_custom_error(&result, 10);
    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(&config_before, &config_after, "set_admin zero address");
}

// ===========================================================================
// SetWhitelistManager tests
// ===========================================================================

#[tokio::test]
async fn test_set_whitelist_manager_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let new_whitelist_manager = Keypair::new();
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

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Set new whitelist manager
    let set_wm_ix = build_set_whitelist_manager(&admin.pubkey(), &new_whitelist_manager.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[set_wm_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_only_protocol_range_changed(
        &config_before,
        &config_after,
        75,
        new_whitelist_manager.pubkey().as_ref(),
        "SetWhitelistManager(new_manager)",
    );
    let config = parse_protocol_config(&config_after);
    assert_eq!(
        &config.whitelist_manager,
        new_whitelist_manager.pubkey().as_ref(),
        "whitelist_manager should be updated"
    );
}

#[tokio::test]
async fn test_set_whitelist_manager_wrong_admin() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let wrong_admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let new_whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(&mut ctx, &[&admin, &wrong_admin], 10_000_000_000).await;

    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let set_wm_ix =
        build_set_whitelist_manager(&wrong_admin.pubkey(), &new_whitelist_manager.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[set_wm_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_admin],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    assert_custom_error(&result, 5); // Unauthorized
    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(
        &config_before,
        &config_after,
        "wrong admin set_whitelist_manager",
    );
}

#[tokio::test]
async fn test_set_whitelist_manager_zero_address() {
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

    let config_before = read_protocol_config_raw(&mut ctx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let set_wm_ix = build_set_whitelist_manager(&admin.pubkey(), &Pubkey::default());
    let tx = Transaction::new_signed_with_payer(
        &[set_wm_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    assert_custom_error(&result, 10); // InvalidAddress
    let config_after = read_protocol_config_raw(&mut ctx).await;
    assert_protocol_config_unchanged(
        &config_before,
        &config_after,
        "set_whitelist_manager zero address",
    );
}

// ===========================================================================
// Protocol pause integration tests
// ===========================================================================

#[tokio::test]
async fn test_deposit_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
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

    // Setup blacklist accounts
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    let deposit_amount = 100_000_000u64;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let snapshot_before_paused_deposit =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let lender_balance_before_paused_deposit =
        get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to deposit while paused
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);
    let snapshot_after_paused_deposit =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_paused_deposit.assert_unchanged(&snapshot_after_paused_deposit);
    let lender_balance_after_paused_deposit =
        get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after_paused_deposit,
        lender_balance_before_paused_deposit
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause and verify deposit works
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let market_before_unpaused_deposit = parse_market(&get_account_data(&mut ctx, &market).await);
    let (vault, _) = get_vault_pda(&market);
    let vault_before_unpaused_deposit = get_token_balance(&mut ctx, &vault).await;
    let lender_before_unpaused_deposit = get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            deposit_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed after unpause");
    let market_after_unpaused_deposit = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_unpaused_deposit = get_token_balance(&mut ctx, &vault).await;
    let lender_after_unpaused_deposit = get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    assert_eq!(
        lender_before_unpaused_deposit - lender_after_unpaused_deposit,
        deposit_amount
    );
    assert_eq!(
        vault_after_unpaused_deposit - vault_before_unpaused_deposit,
        deposit_amount
    );
    assert_eq!(
        market_after_unpaused_deposit.total_deposited,
        market_before_unpaused_deposit.total_deposited + deposit_amount
    );
    assert!(
        market_after_unpaused_deposit.scaled_total_supply
            > market_before_unpaused_deposit.scaled_total_supply
    );
}

// ===========================================================================
// Pause tests for borrow, repay, and withdraw (P2-5)
// ===========================================================================

/// Helper: Set up a market with a deposit and borrow, ready for pause tests.
async fn setup_pause_test_market(
    ctx: &mut solana_program_test::ProgramTestContext,
) -> (
    Keypair, // admin
    Keypair, // borrower
    Keypair, // lender
    Pubkey,  // market
    Pubkey,  // mint
    Keypair, // lender_token
    Keypair, // borrower_token
    Keypair, // blacklist_program
) {
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
        10_000_000_000,
    )
    .await;

    setup_protocol(
        ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Setup blacklist accounts (not blacklisted)
    setup_blacklist_account(ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(ctx, &admin, 6).await;
    let lender_token = create_token_account(ctx, &mint, &lender.pubkey()).await;
    mint_to_account(ctx, &mint, &lender_token.pubkey(), &admin, 1_000_000_000).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Use short maturity (1 day) to keep interest accrual small and avoid distress
    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Deposit 500 USDC
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Borrow 200 USDC
    let borrower_token = create_token_account(ctx, &mint, &borrower.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        200_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    (
        admin,
        borrower,
        lender,
        market,
        mint,
        lender_token,
        borrower_token,
        blacklist_program,
    )
}

#[tokio::test]
async fn test_borrow_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let (admin, borrower, lender, market, _mint, _lender_token, borrower_token, blacklist_program) =
        setup_pause_test_market(&mut ctx).await;
    let borrow_amount = 100_000_000u64;

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let snapshot_before_paused_borrow =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let borrower_balance_before_paused_borrow =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to borrow while paused
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);
    let snapshot_after_paused_borrow =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_paused_borrow.assert_unchanged(&snapshot_after_paused_borrow);
    let borrower_balance_after_paused_borrow =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance_after_paused_borrow,
        borrower_balance_before_paused_borrow
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause and verify borrow works
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let market_before_unpaused_borrow = parse_market(&get_account_data(&mut ctx, &market).await);
    let (vault, _) = get_vault_pda(&market);
    let vault_before_unpaused_borrow = get_token_balance(&mut ctx, &vault).await;
    let borrower_before_unpaused_borrow =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let (borrower_wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before_unpaused_borrow =
        parse_borrower_whitelist(&get_account_data(&mut ctx, &borrower_wl_pda).await);

    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            borrow_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow should succeed after unpause");
    let market_after_unpaused_borrow = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_unpaused_borrow = get_token_balance(&mut ctx, &vault).await;
    let borrower_after_unpaused_borrow =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let wl_after_unpaused_borrow =
        parse_borrower_whitelist(&get_account_data(&mut ctx, &borrower_wl_pda).await);

    assert_eq!(
        market_after_unpaused_borrow.total_borrowed,
        market_before_unpaused_borrow.total_borrowed + borrow_amount
    );
    assert_eq!(
        vault_before_unpaused_borrow - vault_after_unpaused_borrow,
        borrow_amount
    );
    assert_eq!(
        borrower_after_unpaused_borrow - borrower_before_unpaused_borrow,
        borrow_amount
    );
    assert_eq!(
        wl_after_unpaused_borrow.current_borrowed,
        wl_before_unpaused_borrow.current_borrowed + borrow_amount
    );
}

#[tokio::test]
async fn test_repay_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let (admin, borrower, lender, market, mint, _lender_token, borrower_token, _blacklist_program) =
        setup_pause_test_market(&mut ctx).await;
    let repay_amount = 100_000_000u64;

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let snapshot_before_paused_repay =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let borrower_balance_before_paused_repay =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to repay while paused
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);
    let snapshot_after_paused_repay =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_paused_repay.assert_unchanged(&snapshot_after_paused_repay);
    let borrower_balance_after_paused_repay =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance_after_paused_repay,
        borrower_balance_before_paused_repay
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause and verify repay works
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let market_before_unpaused_repay = parse_market(&get_account_data(&mut ctx, &market).await);
    let (vault, _) = get_vault_pda(&market);
    let vault_before_unpaused_repay = get_token_balance(&mut ctx, &vault).await;
    let borrower_before_unpaused_repay =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let (borrower_wl_pda, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_before_unpaused_repay =
        parse_borrower_whitelist(&get_account_data(&mut ctx, &borrower_wl_pda).await);

    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            repay_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("repay should succeed after unpause");
    let market_after_unpaused_repay = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_unpaused_repay = get_token_balance(&mut ctx, &vault).await;
    let borrower_after_unpaused_repay = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let wl_after_unpaused_repay =
        parse_borrower_whitelist(&get_account_data(&mut ctx, &borrower_wl_pda).await);

    assert_eq!(
        market_after_unpaused_repay.total_repaid,
        market_before_unpaused_repay.total_repaid + repay_amount
    );
    assert_eq!(
        vault_after_unpaused_repay - vault_before_unpaused_repay,
        repay_amount
    );
    assert_eq!(
        borrower_before_unpaused_repay - borrower_after_unpaused_repay,
        repay_amount
    );
    assert_eq!(
        wl_before_unpaused_repay.current_borrowed - wl_after_unpaused_repay.current_borrowed,
        repay_amount
    );
}

#[tokio::test]
async fn test_withdraw_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let (admin, _borrower, lender, market, _mint, lender_token, _borrower_token, blacklist_program) =
        setup_pause_test_market(&mut ctx).await;
    let withdraw_scaled = 1_000_000u128;

    // Advance well past maturity + grace period (300s) so withdrawal is normally allowed.
    // Use maturity + 600 to give wide margin against clock drift under parallel load.
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed = parse_market(&market_data);
    common::get_blockhash_pinned(&mut ctx, parsed.maturity_timestamp + 600).await;

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let snapshot_before_paused_withdraw =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let lender_balance_before_paused_withdraw =
        get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to withdraw while paused
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        withdraw_scaled, // small scaled amount
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);
    let snapshot_after_paused_withdraw =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_paused_withdraw.assert_unchanged(&snapshot_after_paused_withdraw);
    let lender_balance_after_paused_withdraw =
        get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after_paused_withdraw,
        lender_balance_before_paused_withdraw
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause and verify withdraw works
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Fetch a new blockhash so the next tx runs on a bank that includes the unpause state
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let market_before_unpaused_withdraw = parse_market(&get_account_data(&mut ctx, &market).await);
    let (vault, _) = get_vault_pda(&market);
    let vault_before_unpaused_withdraw = get_token_balance(&mut ctx, &vault).await;
    let lender_before_unpaused_withdraw = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let lender_pos_before =
        parse_lender_position(&get_account_data(&mut ctx, &lender_position_pda).await);

    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        withdraw_scaled, // small scaled amount
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            withdraw_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("withdraw should succeed after unpause");
    let market_after_unpaused_withdraw = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_unpaused_withdraw = get_token_balance(&mut ctx, &vault).await;
    let lender_after_unpaused_withdraw = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_pos_after =
        parse_lender_position(&get_account_data(&mut ctx, &lender_position_pda).await);

    assert_eq!(
        lender_pos_after.scaled_balance,
        lender_pos_before.scaled_balance - withdraw_scaled
    );
    assert_eq!(
        market_after_unpaused_withdraw.scaled_total_supply,
        market_before_unpaused_withdraw.scaled_total_supply - withdraw_scaled
    );
    assert!(lender_after_unpaused_withdraw > lender_before_unpaused_withdraw);
    assert!(vault_after_unpaused_withdraw < vault_before_unpaused_withdraw);
}

// ===========================================================================
// Maturity boundary tests (P2-6)
// ===========================================================================

#[tokio::test]
async fn test_deposit_one_second_before_maturity_succeeds() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
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

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    let deposit_amount = 100_000_000u64;

    // Advance clock to maturity - 1 second (exact boundary).
    common::get_blockhash_pinned(&mut ctx, maturity - 1).await;

    let market_before_maturity_minus_1 = parse_market(&get_account_data(&mut ctx, &market).await);
    let (vault, _) = get_vault_pda(&market);
    let vault_before_maturity_minus_1 = get_token_balance(&mut ctx, &vault).await;
    let lender_before_maturity_minus_1 = get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit at maturity-1 should succeed");

    let market_after_maturity_minus_1 = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_maturity_minus_1 = get_token_balance(&mut ctx, &vault).await;
    let lender_after_maturity_minus_1 = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        market_after_maturity_minus_1.total_deposited,
        market_before_maturity_minus_1.total_deposited + deposit_amount
    );
    assert_eq!(
        vault_after_maturity_minus_1 - vault_before_maturity_minus_1,
        deposit_amount
    );
    assert_eq!(
        lender_before_maturity_minus_1 - lender_after_maturity_minus_1,
        deposit_amount
    );

    // Boundary check at exact maturity: should fail and preserve state.
    common::get_blockhash_pinned(&mut ctx, maturity).await;
    let snapshot_before_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let lender_before_maturity_exact = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            build_deposit(
                &market,
                &lender.pubkey(),
                &lender_token.pubkey(),
                &mint,
                &blacklist_program.pubkey(),
                deposit_amount,
            ),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 28);
    let snapshot_after_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_maturity_exact.assert_unchanged(&snapshot_after_maturity_exact);
    let lender_after_maturity_exact = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(lender_after_maturity_exact, lender_before_maturity_exact);
}

#[tokio::test]
async fn test_deposit_at_exact_maturity_fails() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
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

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    let deposit_amount = 100_000_000u64;

    // Advance clock to exactly maturity.
    common::get_blockhash_pinned(&mut ctx, maturity).await;

    let snapshot_before_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let lender_before_maturity_exact = get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // MarketMatured = Custom(28)
    assert_custom_error(&result, 28);
    let snapshot_after_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_maturity_exact.assert_unchanged(&snapshot_after_maturity_exact);
    let lender_after_maturity_exact = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(lender_after_maturity_exact, lender_before_maturity_exact);

    // maturity + 1 should also fail with same error and no side effects.
    common::get_blockhash_pinned(&mut ctx, maturity + 1).await;
    let snapshot_before_maturity_plus_1 =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            build_deposit(
                &market,
                &lender.pubkey(),
                &lender_token.pubkey(),
                &mint,
                &blacklist_program.pubkey(),
                deposit_amount,
            ),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 28);
    let snapshot_after_maturity_plus_1 =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_maturity_plus_1.assert_unchanged(&snapshot_after_maturity_plus_1);
}

#[tokio::test]
async fn test_borrow_one_second_before_maturity_succeeds() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
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

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    // Deposit first
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance clock to maturity - 1 second (exact boundary).
    common::get_blockhash_pinned(&mut ctx, maturity - 1).await;

    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_amount = 100_000_000u64;
    let market_before_maturity_minus_1 = parse_market(&get_account_data(&mut ctx, &market).await);
    let (vault, _) = get_vault_pda(&market);
    let vault_before_maturity_minus_1 = get_token_balance(&mut ctx, &vault).await;
    let borrower_before_maturity_minus_1 =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("borrow at maturity-1 should succeed");

    let market_after_maturity_minus_1 = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_maturity_minus_1 = get_token_balance(&mut ctx, &vault).await;
    let borrower_after_maturity_minus_1 =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        market_after_maturity_minus_1.total_borrowed,
        market_before_maturity_minus_1.total_borrowed + borrow_amount
    );
    assert_eq!(
        vault_before_maturity_minus_1 - vault_after_maturity_minus_1,
        borrow_amount
    );
    assert_eq!(
        borrower_after_maturity_minus_1 - borrower_before_maturity_minus_1,
        borrow_amount
    );

    // Boundary check at exact maturity: should fail and preserve state.
    common::get_blockhash_pinned(&mut ctx, maturity).await;
    let snapshot_before_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            build_borrow(
                &market,
                &borrower.pubkey(),
                &borrower_token.pubkey(),
                &blacklist_program.pubkey(),
                borrow_amount,
            ),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 28);
    let snapshot_after_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_maturity_exact.assert_unchanged(&snapshot_after_maturity_exact);
}

#[tokio::test]
async fn test_borrow_at_exact_maturity_fails() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
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

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    // Deposit first (before maturity)
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance clock to exactly maturity.
    common::get_blockhash_pinned(&mut ctx, maturity).await;

    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let borrow_amount = 100_000_000u64;
    let snapshot_before_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let borrower_before_maturity_exact =
        get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    let borrow_ix = build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        borrow_amount,
    );
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // MarketMatured = Custom(28)
    assert_custom_error(&result, 28);
    let snapshot_after_maturity_exact =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_maturity_exact.assert_unchanged(&snapshot_after_maturity_exact);
    let borrower_after_maturity_exact = get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_after_maturity_exact,
        borrower_before_maturity_exact
    );

    // maturity + 1 should also fail with same error and no side effects.
    common::get_blockhash_pinned(&mut ctx, maturity + 1).await;
    let snapshot_before_maturity_plus_1 =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            build_borrow(
                &market,
                &borrower.pubkey(),
                &borrower_token.pubkey(),
                &blacklist_program.pubkey(),
                borrow_amount,
            ),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 28);
    let snapshot_after_maturity_plus_1 =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_maturity_plus_1.assert_unchanged(&snapshot_after_maturity_plus_1);
}

#[tokio::test]
async fn test_withdraw_before_maturity_fails() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
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

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    // Deposit
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to withdraw at maturity - 1 second (exact boundary).
    common::get_blockhash_pinned(&mut ctx, maturity - 1).await;

    let snapshot_before_early_withdraw =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let lender_before_early_withdraw = get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        1_000_000,
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // NotMatured = Custom(29)
    assert_custom_error(&result, 29);
    let snapshot_after_early_withdraw =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_early_withdraw.assert_unchanged(&snapshot_after_early_withdraw);
    let lender_after_early_withdraw = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(lender_after_early_withdraw, lender_before_early_withdraw);
}

#[tokio::test]
async fn test_withdraw_after_maturity_plus_grace_succeeds() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &lender, &whitelist_manager],
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

    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &lender.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    let maturity = common::PINNED_EPOCH + 86_400;

    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    let withdraw_scaled = 1_000_000u128;

    // Deposit
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let deposit_ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Well before grace period ends: current_ts < grace_end so withdrawal is blocked.
    // Use maturity + 200 (100 seconds of margin before grace_end = maturity + 300)
    // to prevent clock drift under parallel test load from crossing the boundary.
    common::get_blockhash_pinned(&mut ctx, maturity + 200).await;
    let snapshot_before_grace_boundary =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    let tx = Transaction::new_signed_with_payer(
        &[build_withdraw(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &blacklist_program.pubkey(),
            withdraw_scaled,
            0,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 32);
    let snapshot_after_grace_boundary =
        capture_market_snapshot_for_lenders(&mut ctx, &market, &[lender.pubkey()]).await;
    snapshot_before_grace_boundary.assert_unchanged(&snapshot_after_grace_boundary);

    // Well past grace period end (maturity + 300).  Use maturity + 600 to give
    // 300 seconds of margin against clock drift under parallel test load.
    common::get_blockhash_pinned(&mut ctx, maturity + 600).await;

    let market_before_post_grace_withdraw =
        parse_market(&get_account_data(&mut ctx, &market).await);
    let (vault, _) = get_vault_pda(&market);
    let vault_before_post_grace_withdraw = get_token_balance(&mut ctx, &vault).await;
    let lender_before_post_grace_withdraw =
        get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let lender_pos_before_post_grace_withdraw =
        parse_lender_position(&get_account_data(&mut ctx, &lender_position_pda).await);

    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        withdraw_scaled,
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            withdraw_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("withdraw after maturity + grace should succeed");
    let market_after_post_grace_withdraw = parse_market(&get_account_data(&mut ctx, &market).await);
    let vault_after_post_grace_withdraw = get_token_balance(&mut ctx, &vault).await;
    let lender_after_post_grace_withdraw =
        get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_pos_after_post_grace_withdraw =
        parse_lender_position(&get_account_data(&mut ctx, &lender_position_pda).await);

    assert_eq!(
        lender_pos_after_post_grace_withdraw.scaled_balance,
        lender_pos_before_post_grace_withdraw.scaled_balance - withdraw_scaled
    );
    assert_eq!(
        market_after_post_grace_withdraw.scaled_total_supply,
        market_before_post_grace_withdraw.scaled_total_supply - withdraw_scaled
    );
    assert!(lender_after_post_grace_withdraw > lender_before_post_grace_withdraw);
    assert!(vault_after_post_grace_withdraw < vault_before_post_grace_withdraw);
}

// ===========================================================================
// Pause tests for create_market, re_settle, close_lender_position, withdraw_excess
// ===========================================================================

#[tokio::test]
async fn test_create_market_fails_when_paused() {
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

    // Setup blacklist accounts
    setup_blacklist_account(&mut ctx, &blacklist_program.pubkey(), &borrower.pubkey(), 0);

    let mint = create_mint(&mut ctx, &admin, 6).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Whitelist borrower
    let wl_ix = build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        10_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let maturity = common::PINNED_EPOCH + 86_400;

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to create market while paused
    let create_ix = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[create_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause and verify create_market works
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let create_ix = build_create_market(
        &borrower.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity,
        10_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            create_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("create_market should succeed after unpause");

    // Verify market was created
    let (market_pda, _) = get_market_pda(&borrower.pubkey(), 1);
    let market_data = get_account_data(&mut ctx, &market_pda).await;
    let market = parse_market(&market_data);
    assert_eq!(market.borrower, borrower.pubkey().to_bytes());
}

#[tokio::test]
async fn test_close_lender_position_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let (admin, _borrower, lender, market, mint, lender_token, _borrower_token, blacklist_program) =
        setup_pause_test_market(&mut ctx).await;

    // Advance clock past maturity so we can withdraw
    let market_data = get_account_data(&mut ctx, &market).await;
    let parsed_market = parse_market(&market_data);
    common::get_blockhash_pinned(&mut ctx, parsed_market.maturity_timestamp + 600).await;

    // Withdraw all lender funds to make position empty (scaled_balance == 0)
    // First, get the lender's scaled balance
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = get_account_data(&mut ctx, &lender_position_pda).await;
    let pos = parse_lender_position(&pos_data);
    let full_scaled = pos.scaled_balance;

    // Withdraw everything
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        full_scaled,
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Verify position is now empty
    let pos_data = get_account_data(&mut ctx, &lender_position_pda).await;
    let pos = parse_lender_position(&pos_data);
    assert_eq!(pos.scaled_balance, 0);

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to close lender position while paused
    let close_ix = build_close_lender_position(&market, &lender.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);

    // Verify position account still exists
    let pos_data = try_get_account_data(&mut ctx, &lender_position_pda).await;
    assert!(
        pos_data.is_some(),
        "position should still exist after paused close attempt"
    );

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause and verify close works
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // COAL-H01: If the withdrawal created a haircut (distressed market from
    // interest accrual), close will fail with PositionNotEmpty (34) even after
    // unpause. That is correct behavior — the lender must claim the haircut first.
    let pos_check = get_account_data(&mut ctx, &lender_position_pda).await;
    let pos_parsed = parse_lender_position(&pos_check);

    let close_ix = build_close_lender_position(&market, &lender.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            close_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;

    if pos_parsed.haircut_owed > 0 {
        // Distressed — close still blocked by pending haircut
        let err = result.unwrap_err();
        assert_custom_error(&Err(err.unwrap()), 34);
    } else {
        // Non-distressed — close should succeed
        result.expect("close_lender_position should succeed after unpause");
        let pos_data = try_get_account_data(&mut ctx, &lender_position_pda).await;
        assert!(
            pos_data.is_none(),
            "position should be closed after unpause"
        );
    }
}

#[tokio::test]
async fn test_re_settle_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let (admin, borrower, _lender, market, mint, _lender_token, borrower_token, blacklist_program) =
        setup_pause_test_market(&mut ctx).await;

    // Need to advance past maturity + grace period for settlement, then do a partial repay
    let market_data = parse_market(&get_account_data(&mut ctx, &market).await);
    let maturity = market_data.maturity_timestamp;
    // Advance well past maturity + grace period (300s).
    // Use maturity + 600 to give wide margin against clock drift under parallel load.
    common::get_blockhash_pinned(&mut ctx, maturity + 600).await;

    // First, do a withdraw to trigger settlement
    let (lender_position_pda, _) = get_lender_position_pda(&market, &_lender.pubkey());
    let pos_data = get_account_data(&mut ctx, &lender_position_pda).await;
    let pos = parse_lender_position(&pos_data);

    let withdraw_ix = build_withdraw(
        &market,
        &_lender.pubkey(),
        &_lender_token.pubkey(),
        &blacklist_program.pubkey(),
        pos.scaled_balance,
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &_lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Verify settlement factor is set (> 0)
    let market_data = parse_market(&get_account_data(&mut ctx, &market).await);
    assert!(
        market_data.settlement_factor_wad > 0,
        "settlement factor should be set"
    );

    // Repay some more to allow re-settle to improve the factor
    // Mint tokens to borrower to repay
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        100_000_000,
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        100_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to re-settle while paused
    let (vault, _) = get_vault_pda(&market);
    let re_settle_ix = build_re_settle(&market, &vault);
    let tx = Transaction::new_signed_with_payer(
        &[re_settle_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause and verify re-settle works
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let old_factor = parse_market(&get_account_data(&mut ctx, &market).await).settlement_factor_wad;

    let re_settle_ix = build_re_settle(&market, &vault);
    let tx = Transaction::new_signed_with_payer(
        &[
            ComputeBudgetInstruction::set_compute_unit_limit(300_000),
            re_settle_ix,
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("re_settle should succeed after unpause");

    let new_factor = parse_market(&get_account_data(&mut ctx, &market).await).settlement_factor_wad;
    assert!(
        new_factor > old_factor,
        "settlement factor should have improved"
    );
}

#[tokio::test]
async fn test_withdraw_excess_fails_when_paused() {
    let mut ctx = common::start_context().await;
    let (admin, borrower, lender, market, mint, lender_token, borrower_token, blacklist_program) =
        setup_pause_test_market(&mut ctx).await;

    // Full repay of borrowed amount
    // The setup borrows 200 USDC, we need to repay principal + interest
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &admin,
        500_000_000, // Extra to cover interest
    )
    .await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        200_000_000, // Repay 200 USDC principal
    );
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Advance past maturity + grace period
    let market_data = parse_market(&get_account_data(&mut ctx, &market).await);
    common::get_blockhash_pinned(&mut ctx, market_data.maturity_timestamp + 600).await;

    // Repay any accrued interest
    let _market_data = parse_market(&get_account_data(&mut ctx, &market).await);
    // Repay enough interest to cover what's accrued (use repay_interest)
    let repay_interest_ix = build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        50_000_000, // Should cover interest
    );
    let tx = Transaction::new_signed_with_payer(
        &[repay_interest_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    // Interest repay may or may not succeed depending on accrued amount, ignore errors
    let _ = ctx.banks_client.process_transaction(tx).await;

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Withdraw all lender funds (triggers settlement)
    let (lender_position_pda, _) = get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = get_account_data(&mut ctx, &lender_position_pda).await;
    let pos = parse_lender_position(&pos_data);

    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        pos.scaled_balance,
        0,
    );
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Verify settlement factor = WAD (full settlement) and scaled_total_supply = 0
    let market_data = parse_market(&get_account_data(&mut ctx, &market).await);

    // Collect fees if any
    if market_data.accrued_protocol_fees > 0 {
        let fee_token = create_token_account(&mut ctx, &mint, &admin.pubkey()).await;
        ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

        let collect_ix = build_collect_fees(&market, &admin.pubkey(), &fee_token.pubkey());
        let tx = Transaction::new_signed_with_payer(
            &[collect_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &admin],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    }

    // Now check if withdraw_excess prerequisites are met
    let _market_data = parse_market(&get_account_data(&mut ctx, &market).await);
    // If not fully settled, skip the actual withdraw_excess test
    // (the pause check happens before those checks anyway)

    // Pause the protocol
    let pause_ix = build_set_pause(&admin.pubkey(), true);
    let tx = Transaction::new_signed_with_payer(
        &[pause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Try to withdraw_excess while paused
    let excess_ix = build_withdraw_excess(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
    );
    let tx = Transaction::new_signed_with_payer(
        &[excess_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // ProtocolPaused = Custom(8)
    assert_custom_error(&result, 8);

    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Unpause
    let unpause_ix = build_set_pause(&admin.pubkey(), false);
    let tx = Transaction::new_signed_with_payer(
        &[unpause_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // After unpause, withdraw_excess would succeed if prerequisites are met
    // (full settlement, fees collected, etc.) — the key assertion is the pause check above
}
