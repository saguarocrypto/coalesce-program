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

const USDC: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000;

// ===========================================================================
// 1. test_force_close_success — Borrower force-closes a lender position
//    after settlement. Payout goes to escrow, position zeroed.
// ===========================================================================
#[tokio::test]
async fn test_force_close_success() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender1 = Keypair::new();
    let lender2 = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender1,
            &lender2,
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

    // Lender1 deposits 1000 USDC
    let l1_token = common::create_token_account(&mut ctx, &mint, &lender1.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l1_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep1 = common::build_deposit(
        &market,
        &lender1.pubkey(),
        &l1_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender1],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Lender2 deposits 500 USDC
    let l2_token = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l2_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;
    let dep2 = common::build_deposit(
        &market,
        &lender2.pubkey(),
        &l2_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender2],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow 800 USDC
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        800 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity + grace
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Lender1 withdraws → triggers settlement (distressed)
    let wdr = common::build_withdraw(
        &market,
        &lender1.pubkey(),
        &l1_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender1],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Confirm settlement and that lender2's position remains
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert!(parsed.settlement_factor_wad > 0);
    assert!(
        parsed.scaled_total_supply > 0,
        "lender2 position should remain"
    );

    // Force-close lender2's position
    let escrow = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;

    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender2.pubkey(),
        &escrow.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify position zeroed
    let (l2_pos, _) = common::get_lender_position_pda(&market, &lender2.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &l2_pos).await;
    let parsed_pos = common::parse_lender_position(&pos_data);
    assert_eq!(parsed_pos.scaled_balance, 0, "position should be zeroed");

    // Verify scaled_total_supply = 0
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert_eq!(parsed2.scaled_total_supply, 0);

    // Verify payout transferred
    let vault_after = common::get_token_balance(&mut ctx, &vault).await;
    let escrow_bal = common::get_token_balance(&mut ctx, &escrow.pubkey()).await;
    assert!(escrow_bal > 0, "escrow should receive payout");
    assert_eq!(vault_before - vault_after, escrow_bal);
}

// ===========================================================================
// 2. test_force_close_non_borrower_rejected
// ===========================================================================
#[tokio::test]
async fn test_force_close_non_borrower_rejected() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender = Keypair::new();
    let random_caller = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender,
            &random_caller,
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

    let l_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep = common::build_deposit(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow and advance
    let b_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &b_token.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // We need to trigger settlement first (force-close requires settlement_factor > 0)
    // Use a second lender to withdraw and trigger settlement, keeping lender's position
    let lender2 = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&lender2], 10_000_000_000).await;
    // Can't deposit after maturity — so use the existing lender as target.
    // Trigger settlement: the lender withdraws partially (half)
    let (lender_pos, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &lender_pos).await;
    let parsed_pos = common::parse_lender_position(&pos_data);
    let half_scaled = parsed_pos.scaled_balance / 2;

    let wdr = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &blacklist_program.pubkey(),
        half_scaled,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Now random_caller tries to force-close lender's remaining position
    let escrow = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    // Build instruction manually with random_caller as signer
    let (vault, _) = common::get_vault_pda(&market);
    let (market_authority, _) = common::get_market_authority_pda(&market);
    let (protocol_config, _) = common::get_protocol_config_pda();

    let (haircut_state, _) = common::get_haircut_state_pda(&market);

    let fc_ix = solana_sdk::instruction::Instruction {
        program_id: common::program_id(),
        accounts: vec![
            solana_sdk::instruction::AccountMeta::new(market, false),
            solana_sdk::instruction::AccountMeta::new_readonly(random_caller.pubkey(), true),
            solana_sdk::instruction::AccountMeta::new(lender_pos, false),
            solana_sdk::instruction::AccountMeta::new(vault, false),
            solana_sdk::instruction::AccountMeta::new(escrow.pubkey(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(market_authority, false),
            solana_sdk::instruction::AccountMeta::new_readonly(protocol_config, false),
            solana_sdk::instruction::AccountMeta::new_readonly(spl_token::id(), false),
            solana_sdk::instruction::AccountMeta::new(haircut_state, false),
        ],
        data: vec![18u8],
    };
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &random_caller],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_eq!(
        err.unwrap(),
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(5) // Unauthorized
        ),
    );
}

// ===========================================================================
// 3. test_force_close_triggers_settlement — Force-close computes the
//    settlement factor when no lender has withdrawn yet (factor == 0).
// ===========================================================================
#[tokio::test]
async fn test_force_close_triggers_settlement() {
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

    let l_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep = common::build_deposit(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity + grace — settlement_factor is still 0
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert_eq!(
        parsed.settlement_factor_wad, 0,
        "settlement should not have happened yet"
    );

    // Force-close should compute settlement factor and succeed
    let escrow = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender.pubkey(),
        &escrow.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify settlement factor was computed
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert!(
        parsed2.settlement_factor_wad > 0,
        "settlement factor should be set"
    );

    // Verify position zeroed and supply cleared
    let (pos, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &pos).await;
    let parsed_pos = common::parse_lender_position(&pos_data);
    assert_eq!(parsed_pos.scaled_balance, 0, "position should be zeroed");
    assert_eq!(parsed2.scaled_total_supply, 0, "supply should be 0");

    // Verify payout transferred to escrow
    let escrow_bal = common::get_token_balance(&mut ctx, &escrow.pubkey()).await;
    assert!(escrow_bal > 0, "escrow should receive payout");
}

// ===========================================================================
// 3b. test_force_close_fund_redirection_rejected — Borrower cannot redirect
//     lender payout to their own token account.
// ===========================================================================
#[tokio::test]
async fn test_force_close_fund_redirection_rejected() {
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

    let l_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep = common::build_deposit(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Borrower tries to redirect payout to their own token account
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender.pubkey(),
        &borrower_token.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_eq!(
        err.unwrap(),
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(16) // InvalidTokenAccountOwner
        ),
    );
}

// ===========================================================================
// 4. test_force_close_clears_supply — Force-close clears scaled_total_supply
//    to 0, unblocking withdraw_excess (prerequisite: supply must be 0).
// ===========================================================================
#[tokio::test]
async fn test_force_close_clears_supply() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender1 = Keypair::new();
    let lender2 = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender1,
            &lender2,
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

    // 0% interest so settlement_factor = WAD when fully funded
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender1: 500 USDC, Lender2 (dust): 1 unit
    let l1_token = common::create_token_account(&mut ctx, &mint, &lender1.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l1_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;
    let dep1 = common::build_deposit(
        &market,
        &lender1.pubkey(),
        &l1_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender1],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let l2_token = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    common::mint_to_account(&mut ctx, &mint, &l2_token.pubkey(), &mint_authority, 1).await;
    let dep2 = common::build_deposit(
        &market,
        &lender2.pubkey(),
        &l2_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender2],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Lender1 withdraws → triggers settlement (fully funded, 0% APR → factor = WAD)
    let wdr = common::build_withdraw(
        &market,
        &lender1.pubkey(),
        &l1_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender1],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify settlement_factor = WAD and dust position remains
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert_eq!(parsed.settlement_factor_wad, WAD);
    assert!(
        parsed.scaled_total_supply > 0,
        "dust position should remain"
    );

    // withdraw_excess BLOCKED: scaled_total_supply > 0
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let we_ix = common::build_withdraw_excess(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[we_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_eq!(
        err.unwrap(),
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(38) // LendersPendingWithdrawals
        ),
    );

    // Force-close dust position → clears supply to 0
    let escrow = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender2.pubkey(),
        &escrow.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: scaled_total_supply = 0, settlement_factor = WAD
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert_eq!(
        parsed2.scaled_total_supply, 0,
        "supply should be 0 after force-close"
    );
    assert_eq!(
        parsed2.settlement_factor_wad, WAD,
        "factor should remain WAD"
    );
}

// ===========================================================================
// 5. test_force_close_dust_payout_zero — Dust position with low settlement
//    factor → payout rounds to 0, position cleared without transfer.
// ===========================================================================
#[tokio::test]
async fn test_force_close_dust_payout_zero() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender_big = Keypair::new();
    let lender_dust = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender_big,
            &lender_dust,
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

    // Big lender: 1000 USDC
    let big_token = common::create_token_account(&mut ctx, &mint, &lender_big.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &big_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep_big = common::build_deposit(
        &market,
        &lender_big.pubkey(),
        &big_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_big],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_big],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Dust lender: 1 unit
    let dust_token = common::create_token_account(&mut ctx, &mint, &lender_dust.pubkey()).await;
    common::mint_to_account(&mut ctx, &mint, &dust_token.pubkey(), &mint_authority, 1).await;
    let dep_dust = common::build_deposit(
        &market,
        &lender_dust.pubkey(),
        &dust_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep_dust],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_dust],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow almost everything
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        999 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Big lender withdraws → settlement at low factor
    let wdr = common::build_withdraw(
        &market,
        &lender_big.pubkey(),
        &big_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender_big],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert!(parsed.settlement_factor_wad > 0);
    assert!(
        parsed.scaled_total_supply > 0,
        "dust position should remain"
    );

    // Force-close dust position (payout = 0 due to rounding)
    let escrow = common::create_token_account(&mut ctx, &mint, &lender_dust.pubkey()).await;
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender_dust.pubkey(),
        &escrow.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify position zeroed and supply decremented
    let (dust_pos, _) = common::get_lender_position_pda(&market, &lender_dust.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &dust_pos).await;
    let parsed_pos = common::parse_lender_position(&pos_data);
    assert_eq!(parsed_pos.scaled_balance, 0);

    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert_eq!(parsed2.scaled_total_supply, 0);
}

// ===========================================================================
// 6. test_force_close_haircut_accumulation — Distressed force-close
//    correctly accumulates the haircut gap.
// ===========================================================================
#[tokio::test]
async fn test_force_close_haircut_accumulation() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender1 = Keypair::new();
    let lender2 = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender1,
            &lender2,
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

    // Lender1: 1000, Lender2: 500
    let l1_token = common::create_token_account(&mut ctx, &mint, &lender1.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l1_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep1 = common::build_deposit(
        &market,
        &lender1.pubkey(),
        &l1_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender1],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let l2_token = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l2_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;
    let dep2 = common::build_deposit(
        &market,
        &lender2.pubkey(),
        &l2_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender2],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow 800 → vault = 700
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        800 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Lender1 withdraws → settlement (distressed)
    let wdr = common::build_withdraw(
        &market,
        &lender1.pubkey(),
        &l1_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wdr],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender1],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    let haircut_before = parsed.haircut_accumulator;
    assert!(parsed.settlement_factor_wad > 0);
    assert!(parsed.settlement_factor_wad < WAD, "should be distressed");

    // Force-close lender2's position
    let escrow = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender2.pubkey(),
        &escrow.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert!(
        parsed2.haircut_accumulator > haircut_before,
        "haircut accumulator should increase: before={}, after={}",
        haircut_before,
        parsed2.haircut_accumulator
    );
}

// ===========================================================================
// 7. test_force_close_before_maturity_rejected — Force-close before maturity
//    returns NotMatured (error 29).
// ===========================================================================
#[tokio::test]
async fn test_force_close_before_maturity_rejected() {
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
        0,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender deposits
    let l_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep = common::build_deposit(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Do NOT advance clock — still before maturity
    let escrow = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender.pubkey(),
        &escrow.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_eq!(
        err.unwrap(),
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(29) // NotMatured
        ),
    );
}

// ===========================================================================
// 8. test_force_close_during_grace_period_rejected — Force-close after
//    maturity but within grace period returns SettlementGracePeriod (error 32).
// ===========================================================================
#[tokio::test]
async fn test_force_close_during_grace_period_rejected() {
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
        0,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender deposits
    let l_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l_token.pubkey(),
        &mint_authority,
        1_000 * USDC,
    )
    .await;
    let dep = common::build_deposit(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1_000 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity but within grace period (300s)
    common::advance_clock_past(&mut ctx, maturity_ts + 150).await;

    let escrow = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender.pubkey(),
        &escrow.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    let err = ctx.banks_client.process_transaction(tx).await.unwrap_err();
    assert_eq!(
        err.unwrap(),
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            InstructionError::Custom(32) // SettlementGracePeriod
        ),
    );
}

// ===========================================================================
// 9. test_force_close_then_withdraw_excess — End-to-end lifecycle:
//    force-close all remaining positions, then borrower calls withdraw_excess.
//    Uses borrow + repay_interest to create vault surplus above lender claims.
// ===========================================================================
#[tokio::test]
async fn test_force_close_then_withdraw_excess() {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender1 = Keypair::new();
    let lender2 = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender1,
            &lender2,
        ],
        10_000_000_000,
    )
    .await;

    // 0% fee rate → all vault surplus goes to borrower
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

    // 0% APR → scale_factor stays at WAD, lender claims = deposits exactly
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0,
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    // Lender1: 600, Lender2: 400 (total deposits = 1000)
    let l1_token = common::create_token_account(&mut ctx, &mint, &lender1.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l1_token.pubkey(),
        &mint_authority,
        600 * USDC,
    )
    .await;
    let dep1 = common::build_deposit(
        &market,
        &lender1.pubkey(),
        &l1_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        600 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender1],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let l2_token = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l2_token.pubkey(),
        &mint_authority,
        400 * USDC,
    )
    .await;
    let dep2 = common::build_deposit(
        &market,
        &lender2.pubkey(),
        &l2_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        400 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[dep2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender2],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrow 500 (vault = 500, total_borrowed = 500)
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[brw],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Borrower repays principal 500 (vault = 1000)
    let repay_ix = common::build_repay(
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

    // Borrower pays 100 USDC "interest" into the vault → vault = 1100
    // This creates the surplus that withdraw_excess will recover
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        100 * USDC,
    )
    .await;
    let ri = common::build_repay_interest_with_amount(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        100 * USDC,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ri],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance past maturity + grace
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Vault = 1100, lender claims = 1000 (0% APR) → sf = WAD (capped)
    // Force-close both positions (borrower-driven, no voluntary withdrawal)
    let escrow1 = common::create_token_account(&mut ctx, &mint, &lender1.pubkey()).await;
    let fc1 = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender1.pubkey(),
        &escrow1.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let escrow2 = common::create_token_account(&mut ctx, &mint, &lender2.pubkey()).await;
    let fc2 = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &lender2.pubkey(),
        &escrow2.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[fc2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify supply cleared, lenders fully paid
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert_eq!(parsed.scaled_total_supply, 0, "supply should be cleared");
    assert_eq!(parsed.settlement_factor_wad, WAD);

    let e1_bal = common::get_token_balance(&mut ctx, &escrow1.pubkey()).await;
    let e2_bal = common::get_token_balance(&mut ctx, &escrow2.pubkey()).await;
    assert_eq!(e1_bal, 600 * USDC, "lender1 should receive full 600 USDC");
    assert_eq!(e2_bal, 400 * USDC, "lender2 should receive full 400 USDC");

    // Vault should have 100 USDC surplus (1100 - 1000 paid to lenders)
    let (vault, _) = common::get_vault_pda(&market);
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_before, 100 * USDC, "vault surplus should be 100 USDC");

    // Borrower calls withdraw_excess — should succeed (supply=0, sf=WAD, fees=0)
    let we_ix = common::build_withdraw_excess(
        &market,
        &borrower.pubkey(),
        &borrower_token.pubkey(),
        &blacklist_program.pubkey(),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[we_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Vault should be empty after full lifecycle
    let vault_after = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_after, 0, "vault should be empty after full lifecycle");
}
