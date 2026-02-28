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
    instruction::AccountMeta, signature::Keypair, signer::Signer, transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// 1. SetFeeConfig rejects wrong admin
// ---------------------------------------------------------------------------

/// Initialize the protocol with admin A, then attempt to call set_fee_config
/// from admin B (a different keypair). The program must reject this with
/// Unauthorized (Custom error code 3).
#[tokio::test]
async fn test_set_fee_config_rejects_wrong_admin() {
    let mut ctx = common::start_context().await;

    let admin_a = Keypair::new();
    let admin_b = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    // Fund both admins
    airdrop_multiple(&mut ctx, &[&admin_a, &admin_b], 10_000_000_000).await;

    // Initialize protocol with admin A as the real admin
    setup_protocol(
        &mut ctx,
        &admin_a,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500, // 5% fee rate
    )
    .await;

    // Attempt to set fee config from admin B (not the real admin)
    let new_fee_authority = Keypair::new();
    let ix = build_set_fee_config(
        &admin_b.pubkey(),
        &new_fee_authority.pubkey(),
        1000, // 10%
    );

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &admin_b],
        recent_blockhash,
    );

    // Snapshot protocol config before failed transaction
    let (protocol_config, _) = get_protocol_config_pda();
    let new_fee_authority_before =
        try_get_account_data(&mut ctx, &new_fee_authority.pubkey()).await;
    assert!(
        new_fee_authority_before.is_none(),
        "new fee authority account should not exist before failed unauthorized set_fee_config"
    );
    let config_before = get_account_data(&mut ctx, &protocol_config).await;

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Unauthorized = Custom(5)
    assert_eq!(
        extract_custom_error(&err),
        Some(5),
        "Expected Custom(5) Unauthorized, got: {:?}",
        err
    );

    // Verify state immutability after failed tx
    let config_after = get_account_data(&mut ctx, &protocol_config).await;
    let new_fee_authority_after = try_get_account_data(&mut ctx, &new_fee_authority.pubkey()).await;
    assert_eq!(
        config_before, config_after,
        "Protocol config changed on failed tx"
    );
    assert!(
        new_fee_authority_after.is_none(),
        "new fee authority account should not be created by failed unauthorized set_fee_config"
    );
}

// ---------------------------------------------------------------------------
// 2. SetBorrowerWhitelist rejects wrong manager
// ---------------------------------------------------------------------------

/// Initialize the protocol with whitelist_manager A, then attempt to call
/// set_borrower_whitelist from manager B. The program must reject this with
/// Unauthorized (Custom error code 3).
#[tokio::test]
async fn test_set_borrower_whitelist_rejects_wrong_manager() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager_a = Keypair::new();
    let whitelist_manager_b = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    // Fund all required keypairs
    airdrop_multiple(
        &mut ctx,
        &[&admin, &whitelist_manager_a, &whitelist_manager_b],
        10_000_000_000,
    )
    .await;

    // Initialize protocol with whitelist_manager_a as the real manager
    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager_a.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Attempt to whitelist a borrower using manager B (not the real manager)
    let ix = build_set_borrower_whitelist(
        &whitelist_manager_b.pubkey(),
        &borrower.pubkey(),
        1, // is_whitelisted = true
        10_000 * USDC,
    );

    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager_b],
        recent_blockhash,
    );

    // Snapshot protocol config before failed transaction
    let (protocol_config, _) = get_protocol_config_pda();
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    assert!(
        try_get_account_data(&mut ctx, &borrower_whitelist)
            .await
            .is_none(),
        "borrower whitelist account should not exist before unauthorized whitelist write"
    );
    let config_before = get_account_data(&mut ctx, &protocol_config).await;

    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();

    // Unauthorized = Custom(5)
    assert_eq!(
        extract_custom_error(&err),
        Some(5),
        "Expected Custom(5) Unauthorized, got: {:?}",
        err
    );

    // Verify state immutability after failed tx
    let config_after = get_account_data(&mut ctx, &protocol_config).await;
    let borrower_whitelist_after = try_get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        config_before, config_after,
        "Protocol config changed on failed tx"
    );
    assert!(
        borrower_whitelist_after.is_none(),
        "borrower whitelist account should not be created by unauthorized manager"
    );
}

// ---------------------------------------------------------------------------
// 3. CollectFees rejects wrong fee authority
// ---------------------------------------------------------------------------

/// Set up a full market with deposits, borrowing, and repayment so that fees
/// accrue. Then attempt to collect fees with a wrong fee_authority keypair.
/// The program must reject this with Unauthorized (Custom error code 3).
#[tokio::test]
async fn test_collect_fees_rejects_wrong_fee_authority() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let wrong_fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    // Fund all keypairs
    airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &fee_authority,
            &wrong_fee_authority,
            &whitelist_manager,
            &borrower,
            &lender,
        ],
        10_000_000_000,
    )
    .await;

    // Initialize protocol with fee_authority as the real fee authority
    let fee_rate_bps: u16 = 1000; // 10%
    setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    // Create mint
    let mint_authority = Keypair::new();
    airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = create_mint(&mut ctx, &mint_authority, 6).await;

    // Create token accounts
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fee_dest = create_token_account(&mut ctx, &mint, &wrong_fee_authority.pubkey()).await;

    // Mint tokens to lender
    let deposit_amount = 1_000 * USDC;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    // Get current clock to set maturity
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Setup market
    let market = setup_market_full(
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
    let deposit_ix = build_deposit(
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

    // Borrow
    let borrow_amount = 500 * USDC;
    let borrow_ix = build_borrow(
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

    // Repay principal (can only repay up to borrowed amount due to SR-116)
    let repay_amount = 500 * USDC;
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        repay_amount,
    )
    .await;
    let repay_ix = build_repay(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        repay_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[repay_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance clock past maturity + grace period
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // SR-113: Lender must withdraw before fee collection
    // First, repay interest to ensure vault has enough for full settlement
    let interest_amount = 10 * USDC;
    mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        interest_amount,
    )
    .await;
    let repay_interest_ix = build_repay_interest_with_amount(
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

    // Lender withdraws full balance
    let withdraw_ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0, // 0 = full withdrawal
        0, // no minimum payout
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Attempt to collect fees with wrong_fee_authority (not the real one)
    let collect_ix = build_collect_fees(&market, &wrong_fee_authority.pubkey(), &fee_dest.pubkey());

    // Snapshot state before failed transaction
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(&borrower.pubkey());
    let fee_dest_before = get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let borrower_whitelist_before = get_account_data(&mut ctx, &borrower_whitelist).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[collect_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &wrong_fee_authority],
        recent,
    );

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized

    // Verify state immutability after failed tx
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let fee_dest_after = get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
    let borrower_whitelist_after = get_account_data(&mut ctx, &borrower_whitelist).await;
    assert_eq!(
        fee_dest_after, fee_dest_before,
        "fee destination balance changed on unauthorized collect_fees"
    );
    assert_eq!(
        borrower_whitelist_after, borrower_whitelist_before,
        "borrower whitelist changed on unauthorized collect_fees"
    );
}

// ---------------------------------------------------------------------------
// 4. CloseLenderPosition rejects wrong lender
// ---------------------------------------------------------------------------

/// Set up a market, deposit as lender A, withdraw fully, then try to close
/// lender A's position from lender B. The program must reject this with
/// Unauthorized (Custom error code 3) because the PDA derived from lender B
/// does not match lender A's position account.
#[tokio::test]
async fn test_close_lender_position_rejects_wrong_lender() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender_a = Keypair::new();
    let lender_b = Keypair::new();

    // Fund all keypairs
    airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &fee_authority,
            &whitelist_manager,
            &borrower,
            &lender_a,
            &lender_b,
        ],
        10_000_000_000,
    )
    .await;

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

    // Create mint
    let mint_authority = Keypair::new();
    airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = create_mint(&mut ctx, &mint_authority, 6).await;

    // Create token account for lender A
    let lender_a_token = create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;

    // Mint tokens to lender A
    let deposit_amount = 1_000 * USDC;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_a_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    // Get current clock for maturity
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Setup market
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

    // Lender A deposits
    let deposit_ix = build_deposit(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity AND past 300-second grace period
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Lender A withdraws all (scaled_amount = 0 means full withdrawal)
    let withdraw_ix = build_withdraw(
        &market,
        &lender_a.pubkey(),
        &lender_a_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_a],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender B attempts to close lender A's position.
    // Build the instruction with lender B as the signer but pass
    // lender A's position PDA. The PDA derivation from lender B will
    // not match lender A's position account.
    let (lender_a_position, _) = get_lender_position_pda(&market, &lender_a.pubkey());
    let (protocol_config, _) = get_protocol_config_pda();
    let ix = solana_sdk::instruction::Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(market, false),
            AccountMeta::new(lender_b.pubkey(), true), // lender B signs
            AccountMeta::new(lender_a_position, false), // but this is lender A's position
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            AccountMeta::new_readonly(protocol_config, false),
        ],
        data: vec![10u8],
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_b],
        blockhash,
    );

    // Snapshot state before failed transaction
    let (vault, _) = get_vault_pda(&market);
    let lender_a_balance_before = get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let lender_a_position_before = get_account_data(&mut ctx, &lender_a_position).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_a_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 13); // InvalidPDA

    // Verify state immutability after failed tx
    let snap_after =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_a_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_a_balance_after = get_token_balance(&mut ctx, &lender_a_token.pubkey()).await;
    let lender_a_position_after = get_account_data(&mut ctx, &lender_a_position).await;
    assert_eq!(
        lender_a_balance_after, lender_a_balance_before,
        "lender A token balance changed on rejected close by wrong lender"
    );
    assert_eq!(
        lender_a_position_after, lender_a_position_before,
        "lender A position bytes changed on rejected close by wrong lender"
    );
}

// ---------------------------------------------------------------------------
// 5. Deposit rejects non-signer lender
// ---------------------------------------------------------------------------

/// Build a deposit instruction but modify the lender AccountMeta so it is not
/// marked as a signer. The program must reject the transaction because the
/// lender must sign deposit instructions.
#[tokio::test]
async fn test_deposit_rejects_non_signer() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    // Fund all keypairs
    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
    )
    .await;

    // Initialize protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Create mint
    let mint = create_mint(&mut ctx, &admin, 6).await;

    // Setup market
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

    // Create lender token account and mint tokens
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Build a deposit instruction normally
    let mut ix = build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    // Modify the lender AccountMeta (index 1) to NOT be a signer
    ix.accounts[1] = AccountMeta::new(lender.pubkey(), false); // is_signer = false

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Only sign with ctx.payer (lender does NOT sign)
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &[&ctx.payer], recent);

    // Snapshot state before failed transaction
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    assert!(
        try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not exist before failed non-signer deposit"
    );
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized

    // Verify state immutability after failed tx
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on rejected non-signer deposit"
    );
    assert!(
        try_get_account_data(&mut ctx, &lender_position)
            .await
            .is_none(),
        "lender position should not be created by rejected non-signer deposit"
    );
}

// ---------------------------------------------------------------------------
// 6. Borrow rejects wrong borrower
// ---------------------------------------------------------------------------

/// Create a market for borrower A, deposit funds, then attempt to borrow with
/// borrower B. The program must reject this with Unauthorized (Custom error
/// code 3) because borrower B is not the market's designated borrower.
#[tokio::test]
async fn test_borrow_rejects_wrong_borrower() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower_a = Keypair::new();
    let borrower_b = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    // Fund all keypairs
    airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower_a,
            &borrower_b,
            &whitelist_manager,
            &lender,
        ],
        10_000_000_000,
    )
    .await;

    // Initialize protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Create mint
    let mint = create_mint(&mut ctx, &admin, 6).await;

    // Setup market for borrower A
    let market = setup_market_full(
        &mut ctx,
        &admin,
        &borrower_a,
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

    // Deposit funds as lender so the vault has tokens
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
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Borrower B attempts to borrow from the market that belongs to borrower A
    let borrower_b_token = create_token_account(&mut ctx, &mint, &borrower_b.pubkey()).await;

    let borrow_ix = build_borrow(
        &market,
        &borrower_b.pubkey(),
        &borrower_b_token.pubkey(),
        &blacklist_program.pubkey(),
        100 * USDC,
    );

    // Snapshot state before failed transaction
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let (borrower_a_whitelist, _) = get_borrower_whitelist_pda(&borrower_a.pubkey());
    let (borrower_b_whitelist, _) = get_borrower_whitelist_pda(&borrower_b.pubkey());
    let borrower_b_balance_before = get_token_balance(&mut ctx, &borrower_b_token.pubkey()).await;
    let borrower_a_whitelist_before = get_account_data(&mut ctx, &borrower_a_whitelist).await;
    let borrower_b_whitelist_before = try_get_account_data(&mut ctx, &borrower_b_whitelist).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[borrow_ix],
        Some(&borrower_b.pubkey()),
        &[&borrower_b],
        recent,
    );

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized

    // Verify state immutability after failed tx
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let borrower_b_balance_after = get_token_balance(&mut ctx, &borrower_b_token.pubkey()).await;
    let borrower_a_whitelist_after = get_account_data(&mut ctx, &borrower_a_whitelist).await;
    let borrower_b_whitelist_after = try_get_account_data(&mut ctx, &borrower_b_whitelist).await;
    assert_eq!(
        borrower_b_balance_after, borrower_b_balance_before,
        "borrower B balance changed on rejected wrong-borrower borrow"
    );
    assert_eq!(
        borrower_a_whitelist_after, borrower_a_whitelist_before,
        "borrower A whitelist changed on rejected wrong-borrower borrow"
    );
    assert_eq!(
        borrower_b_whitelist_after, borrower_b_whitelist_before,
        "borrower B whitelist changed on rejected wrong-borrower borrow"
    );
}

// ---------------------------------------------------------------------------
// 7. Withdraw rejects non-signer lender
// ---------------------------------------------------------------------------

/// Build a withdraw instruction but modify the lender AccountMeta so it is not
/// marked as a signer. The program must reject the transaction because the
/// lender must sign withdraw instructions.
#[tokio::test]
async fn test_withdraw_rejects_non_signer() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let lender = Keypair::new();

    // Fund all keypairs
    airdrop_multiple(
        &mut ctx,
        &[&admin, &borrower, &whitelist_manager, &lender],
        10_000_000_000,
    )
    .await;

    // Initialize protocol
    setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Create mint
    let mint = create_mint(&mut ctx, &admin, 6).await;

    // Get current clock for maturity
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    // Setup market
    let market = setup_market_full(
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

    // Create lender token account and mint tokens
    let lender_token = create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000 * USDC,
    )
    .await;

    // Deposit as the lender (with proper signing)
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
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed");

    // Advance past maturity so withdrawal is allowed
    advance_clock_past(&mut ctx, maturity_timestamp + 301).await;

    // Build a withdraw instruction normally
    let mut ix = build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0, // full withdrawal
    );

    // Modify the lender AccountMeta (index 1) to NOT be a signer
    ix.accounts[1] = AccountMeta::new_readonly(lender.pubkey(), false); // is_signer = false

    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Only sign with ctx.payer (lender does NOT sign)
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &[&ctx.payer], recent);

    // Snapshot state before failed transaction
    let (vault, _) = get_vault_pda(&market);
    let (lender_position, _) = get_lender_position_pda(&market, &lender.pubkey());
    let lender_balance_before = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_before = get_account_data(&mut ctx, &lender_position).await;
    let snap_before =
        ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;

    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    assert_custom_error(&result, 5); // Unauthorized

    // Verify state immutability after failed tx
    let snap_after = ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before.assert_unchanged(&snap_after);
    let lender_balance_after = get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_after = get_account_data(&mut ctx, &lender_position).await;
    assert_eq!(
        lender_balance_after, lender_balance_before,
        "lender token balance changed on rejected non-signer withdraw"
    );
    assert_eq!(
        lender_position_after, lender_position_before,
        "lender position changed on rejected non-signer withdraw"
    );
}
