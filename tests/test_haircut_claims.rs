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
    clippy::int_plus_one
)]

mod common;

use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;

/// WAD = 1e18.
const WAD: u128 = 1_000_000_000_000_000_000;

// ===========================================================================
// Shared setup: creates a distressed market with two lenders.
//
// Returns: (market, vault, borrower, lender_a, lender_a_token, lender_b,
//           lender_b_token, mint, mint_authority, blacklist_program)
//
// State after setup:
//   - Lender A deposited 500 USDC
//   - Lender B deposited 500 USDC
//   - Borrower borrowed 500 USDC (vault has 500 of 1000 deposited)
//   - Clock advanced past maturity + grace period
//   - Settlement NOT yet triggered (settlement_factor_wad == 0)
// ===========================================================================
async fn setup_distressed_market() -> (
    solana_sdk::pubkey::Pubkey, // market
    solana_sdk::pubkey::Pubkey, // vault
    Keypair,                    // borrower
    Keypair,                    // lender_a
    solana_sdk::pubkey::Pubkey, // lender_a_token
    Keypair,                    // lender_b
    solana_sdk::pubkey::Pubkey, // lender_b_token
    solana_sdk::pubkey::Pubkey, // mint
    Keypair,                    // mint_authority
    Keypair,                    // blacklist_program
    solana_program_test::ProgramTestContext,
) {
    let mut ctx = common::start_context().await;
    let admin = Keypair::new();
    let borrower = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_authority = Keypair::new();
    let lender_a = Keypair::new();
    let lender_b = Keypair::new();

    common::airdrop_multiple(
        &mut ctx,
        &[
            &admin,
            &borrower,
            &whitelist_manager,
            &fee_authority,
            &lender_a,
            &lender_b,
        ],
        10_000_000_000,
    )
    .await;

    // Protocol with zero fees to simplify math
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

    let maturity_ts = common::SHORT_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        0, // 0% APR — keeps scale_factor == WAD for clean math
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let (vault, _) = common::get_vault_pda(&market);

    // Lender A deposits 500 USDC
    let la_token = common::create_token_account(&mut ctx, &mint, &lender_a.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &la_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;
    let dep_a = common::build_deposit(
        &market,
        &lender_a.pubkey(),
        &la_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, dep_a, &[&lender_a]).await;

    // Lender B deposits 500 USDC
    let lb_token = common::create_token_account(&mut ctx, &mint, &lender_b.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lb_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;
    let dep_b = common::build_deposit(
        &market,
        &lender_b.pubkey(),
        &lb_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, dep_b, &[&lender_b]).await;

    // Borrower borrows 500 USDC (vault goes from 1000 to 500)
    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, brw, &[&borrower]).await;

    // Advance past maturity + grace period
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    (
        market,
        vault,
        borrower,
        lender_a,
        la_token.pubkey(),
        lender_b,
        lb_token.pubkey(),
        mint,
        mint_authority,
        blacklist_program,
        ctx,
    )
}

// ===========================================================================
// 1. Claim haircut — full recovery to WAD
// ===========================================================================

#[tokio::test]
async fn test_claim_haircut_full_recovery() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Lender A withdraws — triggers settlement at SF = 500/1000 = 0.5
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    // Verify distress + haircut recorded on position
    let (la_pos_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &la_pos_pda).await;
    let pos = common::parse_lender_position(&pos_data);
    assert!(
        pos.haircut_owed > 0,
        "haircut_owed should be nonzero after distressed withdrawal"
    );
    assert!(
        pos.withdrawal_sf > 0 && pos.withdrawal_sf < WAD,
        "withdrawal_sf should be distressed"
    );

    let la_balance_after_withdraw = common::get_token_balance(&mut ctx, &la_token).await;

    // Borrower repays enough to bring SF to WAD (repay the full 500 borrowed)
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 500 * USDC).await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    // Re-settle — should improve SF
    let rs = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs, &[]).await;

    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert!(
        parsed.settlement_factor_wad > pos.withdrawal_sf,
        "SF should have improved"
    );

    // Lender A claims haircut
    let claim = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
    common::send_ok(&mut ctx, claim, &[&lender_a]).await;

    // Verify: position haircut_owed should be 0 or near-zero
    let pos_data2 = common::get_account_data(&mut ctx, &la_pos_pda).await;
    let pos2 = common::parse_lender_position(&pos_data2);

    // Verify: lender A received additional tokens
    let la_balance_after_claim = common::get_token_balance(&mut ctx, &la_token).await;
    assert!(
        la_balance_after_claim > la_balance_after_withdraw,
        "lender A should have received claim tokens: before={la_balance_after_withdraw} after={la_balance_after_claim}"
    );

    // Verify: market accumulator decreased
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert!(
        parsed2.haircut_accumulator < parsed.haircut_accumulator,
        "accumulator should have decreased after claim"
    );

    // If SF reached WAD, full claim should have zeroed haircut_owed
    if parsed.settlement_factor_wad == WAD {
        assert_eq!(
            pos2.haircut_owed, 0,
            "full recovery should zero haircut_owed"
        );
    }
}

// ===========================================================================
// 2. Claim haircut — no improvement reverts
// ===========================================================================

#[tokio::test]
async fn test_claim_haircut_no_improvement_reverts() {
    let (
        market,
        _vault,
        _borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        _mint,
        _mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    // Lender A withdraws — triggers settlement
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    // Try to claim without any repayment / re_settle → SF hasn't improved
    // Error 31 = SettlementNotImproved
    let claim = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
    common::send_expect_error(&mut ctx, claim, &[&lender_a], 31).await;
}

// ===========================================================================
// 3. Claim haircut — no owed reverts
// ===========================================================================

#[tokio::test]
async fn test_claim_haircut_no_owed_reverts() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Lender A withdraws — triggers settlement at SF ~0.5
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    // Verify haircut_owed > 0
    let (la_pos_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &la_pos_pda).await;
    let pos = common::parse_lender_position(&pos_data);
    assert!(pos.haircut_owed > 0, "setup: haircut_owed should be > 0");

    // Borrower repays full 500 USDC to push SF toward WAD
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 500 * USDC).await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    // Re-settle to improve SF
    let rs = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs, &[]).await;

    // Claim iteratively until haircut_owed reaches 0.
    // Each claim rebases the position to current_sf, so subsequent claims at the
    // same SF will revert with SettlementNotImproved (31). But the conservative
    // solver may allow re_settle to improve SF further, enabling more claims.
    // After enough iterations, haircut_owed reaches 0.
    for nonce in 0u32..5 {
        let pos_data_loop = common::get_account_data(&mut ctx, &la_pos_pda).await;
        let pos_loop = common::parse_lender_position(&pos_data_loop);
        if pos_loop.haircut_owed == 0 {
            break;
        }
        // Use ComputeBudget nonce to ensure unique tx signatures per iteration
        let budget_ix =
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
                200_000 + nonce,
            );

        // Try re_settle to push SF higher (may fail if already at max)
        let rs_loop = common::build_re_settle(&market, &vault);
        let _ = ctx
            .banks_client
            .process_transaction(solana_sdk::transaction::Transaction::new_signed_with_payer(
                &[budget_ix.clone(), rs_loop],
                Some(&ctx.payer.pubkey()),
                &[&ctx.payer],
                ctx.banks_client.get_latest_blockhash().await.unwrap(),
            ))
            .await;

        let budget_ix2 =
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
                300_000 + nonce,
            );

        // Try claim
        let claim_loop = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
        let _ = ctx
            .banks_client
            .process_transaction(solana_sdk::transaction::Transaction::new_signed_with_payer(
                &[budget_ix2, claim_loop],
                Some(&ctx.payer.pubkey()),
                &[&ctx.payer, &lender_a],
                ctx.banks_client.get_latest_blockhash().await.unwrap(),
            ))
            .await;
    }

    // After claiming everything, haircut_owed should be 0
    let pos_final =
        common::parse_lender_position(&common::get_account_data(&mut ctx, &la_pos_pda).await);
    assert_eq!(
        pos_final.haircut_owed, 0,
        "haircut_owed should reach 0 after full claim cycle"
    );

    // Now the second claim must fail with NoHaircutToClaim (43)
    let claim_final = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
    common::send_expect_error(&mut ctx, claim_final, &[&lender_a], 43).await;
}

// ===========================================================================
// 4. Re-settle idempotence — second call without new funds reverts
// ===========================================================================

#[tokio::test]
async fn test_re_settle_idempotent_with_haircuts() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Lender A withdraws — triggers settlement
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    // Borrower repays 250 USDC
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 250 * USDC).await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        250 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    // First re_settle — should succeed
    let rs1 = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs1, &[]).await;

    // Second re_settle — same state, should revert SettlementNotImproved (31)
    let rs2 = common::build_re_settle(&market, &vault);
    common::send_expect_error_same_bank(&mut ctx, rs2, &[], 31).await;
}

// ===========================================================================
// 5. Force claim haircut — borrower clears abandoned position
// ===========================================================================

#[tokio::test]
async fn test_force_claim_haircut_by_borrower() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Lender A withdraws — triggers settlement
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    let (la_pos_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());

    // Borrower repays full 500
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 500 * USDC).await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    // Re-settle
    let rs = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs, &[]).await;

    // Snapshot pre-claim state for precise assertions
    let la_balance_before = common::get_token_balance(&mut ctx, &la_token).await;
    let pos_data_before = common::get_account_data(&mut ctx, &la_pos_pda).await;
    let pos_before = common::parse_lender_position(&pos_data_before);
    let md_before = common::get_account_data(&mut ctx, &market).await;
    let acc_before = common::parse_market(&md_before).haircut_accumulator;
    let owed_before = pos_before.haircut_owed;
    assert!(
        owed_before > 0,
        "setup: haircut_owed must be > 0 before force_claim"
    );

    // Borrower force-claims on behalf of lender A
    let fc = common::build_force_claim_haircut(&market, &borrower.pubkey(), &la_pos_pda, &la_token);
    common::send_ok(&mut ctx, fc, &[&borrower]).await;

    // Read post-claim state
    let la_balance_after = common::get_token_balance(&mut ctx, &la_token).await;
    let pos_data_after = common::get_account_data(&mut ctx, &la_pos_pda).await;
    let pos_after = common::parse_lender_position(&pos_data_after);
    let md_after = common::get_account_data(&mut ctx, &market).await;
    let acc_after = common::parse_market(&md_after).haircut_accumulator;

    // Token balance increased by the claimed amount
    let claimed = la_balance_after - la_balance_before;
    assert!(
        claimed > 0,
        "force_claim should have transferred tokens: before={la_balance_before} after={la_balance_after}"
    );

    // Position haircut_owed decreased by exactly the claimed amount
    assert_eq!(
        pos_after.haircut_owed,
        owed_before - claimed,
        "haircut_owed should decrease by claimed amount: before={owed_before} claimed={claimed} after={}",
        pos_after.haircut_owed
    );

    // Market accumulator decreased by exactly the claimed amount
    assert_eq!(
        acc_after,
        acc_before - claimed,
        "accumulator should decrease by claimed amount: before={acc_before} claimed={claimed} after={acc_after}"
    );
}

// ===========================================================================
// 6. Force claim — non-borrower rejected
// ===========================================================================

#[tokio::test]
async fn test_force_claim_haircut_non_borrower_rejected() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Lender A withdraws
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    let (la_pos_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());

    // Repay + re_settle
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 500 * USDC).await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    let rs = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs, &[]).await;

    // Random signer tries force_claim — should fail
    let random = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&random], 1_000_000_000).await;
    let fc = common::build_force_claim_haircut(&market, &random.pubkey(), &la_pos_pda, &la_token);
    // Error: MissingRequiredSignature or Unauthorized
    // force_claim checks borrower.is_signer then market.borrower match
    // With random as signer, market.borrower won't match → Unauthorized (5)
    common::send_expect_error(&mut ctx, fc, &[&random], 5).await;
}

// ===========================================================================
// 7. Close position blocked while haircut_owed > 0
// ===========================================================================

#[tokio::test]
async fn test_close_position_blocked_with_haircut() {
    let (
        market,
        _vault,
        _borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        _mint,
        _mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    // Lender A withdraws — triggers settlement, gets haircut
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    // Position has scaled_balance = 0 but haircut_owed > 0
    let (la_pos_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &la_pos_pda).await;
    let pos = common::parse_lender_position(&pos_data);
    assert_eq!(
        pos.scaled_balance, 0,
        "scaled_balance should be 0 after full withdrawal"
    );
    assert!(pos.haircut_owed > 0, "haircut_owed should be > 0");

    // Try to close — should fail with PositionNotEmpty (34)
    let close = common::build_close_lender_position(&market, &lender_a.pubkey());
    common::send_expect_error(&mut ctx, close, &[&lender_a], 34).await;
}

// ===========================================================================
// 8. Collect fees respects haircut accumulator
// ===========================================================================

#[tokio::test]
async fn test_collect_fees_respects_haircut_reserve() {
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

    // Protocol with 5% fee rate (to accrue fees)
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500, // 5% fee
    )
    .await;

    let mint_authority = Keypair::new();
    common::airdrop_multiple(&mut ctx, &[&mint_authority], 1_000_000_000).await;
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    let maturity_ts = common::SHORT_MATURITY;
    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000, // 10% APR to generate meaningful fees
        maturity_ts,
        10_000 * USDC,
        &whitelist_manager,
        10_000 * USDC,
    )
    .await;

    let (vault, _) = common::get_vault_pda(&market);

    // Lender deposits 1000 USDC
    let l_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &l_token.pubkey(),
        &mint_authority,
        1000 * USDC,
    )
    .await;
    let dep = common::build_deposit(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        1000 * USDC,
    );
    common::send_ok(&mut ctx, dep, &[&lender]).await;

    // Borrower borrows 500
    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;
    let brw = common::build_borrow(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &blacklist_program.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, brw, &[&borrower]).await;

    // Advance past maturity
    common::advance_clock_past(&mut ctx, maturity_ts + 301).await;

    // Lender withdraws — triggers distressed settlement
    let wdr = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &l_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender]).await;

    // Verify haircut accumulator is set
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert!(
        parsed.haircut_accumulator > 0,
        "accumulator should be > 0 after distressed withdrawal"
    );

    // Borrower repays full 500 to bring SF to WAD for remaining lenders
    common::mint_to_account(
        &mut ctx,
        &mint,
        &brw_token.pubkey(),
        &mint_authority,
        500 * USDC,
    )
    .await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    // Re-settle
    let rs = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs, &[]).await;

    // Read post-resettle state
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;
    let acc_before = parsed2.haircut_accumulator;

    // Note: vault may be < accumulator when interest accrual inflates the
    // entitlement beyond the vault's actual tokens. That's correct — the
    // accumulator tracks the full gap including unfunded interest.
    // The invariant we test: fee collection must not REDUCE vault below
    // what it was, net of any legitimate fee payment.

    // Attempt fee collection — it may succeed (capped) or fail (distress guard,
    // no fees, or no surplus). Either way, vault must not decrease by more than
    // the fees that were actually collected.
    let fee_dest = common::create_token_account(&mut ctx, &mint, &fee_authority.pubkey()).await;
    let cf = common::build_collect_fees(&market, &fee_authority.pubkey(), &fee_dest.pubkey());
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[cf],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &fee_authority],
        bh,
    );
    let result = ctx.banks_client.process_transaction(tx).await;

    let vault_after = common::get_token_balance(&mut ctx, &vault).await;
    let md3 = common::get_account_data(&mut ctx, &market).await;
    let parsed3 = common::parse_market(&md3);

    if result.is_ok() {
        let fee_collected = common::get_token_balance(&mut ctx, &fee_dest.pubkey()).await;
        assert!(
            fee_collected > 0,
            "fee collection succeeded but transferred 0 tokens"
        );
        // Vault decreased by exactly the fee amount
        assert_eq!(
            vault_after,
            vault_before - fee_collected,
            "vault change must equal fee collected"
        );
        // Accumulator must not have changed (collect_fees doesn't modify it)
        assert_eq!(
            parsed3.haircut_accumulator, acc_before,
            "accumulator must not change during fee collection"
        );
        // If vault was >= accumulator before, it should still be after
        // (collect_fees subtracts accumulator from safe_max, preventing drain)
        if vault_before >= acc_before {
            assert!(
                vault_after >= parsed3.haircut_accumulator,
                "fees drained below haircut reserve: vault={vault_after} acc={}",
                parsed3.haircut_accumulator
            );
        }
    } else {
        // Fee collection was blocked — vault unchanged
        assert_eq!(
            vault_after, vault_before,
            "failed fee collection must not change vault"
        );
    }
}

// ===========================================================================
// 9. Withdraw stores exact haircut state on position
// ===========================================================================

#[tokio::test]
async fn test_withdraw_stores_haircut_on_position() {
    let (
        market,
        _vault,
        _borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        _mint,
        _mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    // Lender A withdraws — triggers settlement
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    // Verify position haircut fields
    let (la_pos_pda, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &la_pos_pda).await;
    let pos = common::parse_lender_position(&pos_data);

    // Read market SF
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);

    assert!(
        pos.haircut_owed > 0,
        "haircut_owed must be > 0 after distressed withdrawal"
    );
    assert_eq!(
        pos.withdrawal_sf, parsed.settlement_factor_wad,
        "withdrawal_sf must match market settlement factor"
    );

    // haircut_accumulator on market should match position haircut_owed
    // (single withdrawal, so they should be equal)
    assert_eq!(
        parsed.haircut_accumulator, pos.haircut_owed,
        "market accumulator should equal position haircut_owed for first withdrawal"
    );
}

// ===========================================================================
// 10. Force close preserves haircut claim
// ===========================================================================

#[tokio::test]
async fn test_force_close_preserves_haircut() {
    let (
        market,
        _vault,
        borrower,
        lender_a,
        la_token,
        _lender_b,
        _lb_token,
        _mint,
        _mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    // Lender A withdraws — triggers settlement
    let wdr = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr, &[&lender_a]).await;

    // Borrower force-closes lender B (who hasn't withdrawn)
    let lb_escrow = common::create_token_account(&mut ctx, &_mint, &_lender_b.pubkey()).await;
    let (lb_pos_pda, _) = common::get_lender_position_pda(&market, &_lender_b.pubkey());
    let fc = common::build_force_close_position(
        &market,
        &borrower.pubkey(),
        &_lender_b.pubkey(),
        &lb_escrow.pubkey(),
    );
    common::send_ok(&mut ctx, fc, &[&borrower]).await;

    // Lender B's position should have haircut data
    let pos_data = common::get_account_data(&mut ctx, &lb_pos_pda).await;
    let pos = common::parse_lender_position(&pos_data);

    // Read market SF
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);

    // Only if SF < WAD should there be a haircut
    if parsed.settlement_factor_wad < WAD {
        assert!(
            pos.haircut_owed > 0,
            "force-closed position should retain haircut_owed when SF < WAD"
        );
        assert_eq!(
            pos.scaled_balance, 0,
            "scaled_balance should be 0 after force_close"
        );
    }
}

// ===========================================================================
// COMPREHENSIVE SCENARIO TESTS
//
// These are the "walk through the balances by hand" tests for the new haircut
// flow.
//
// Important scaling note:
// - the comments below use the same ratios as a 1,000,000 / 500,000 / 250,000
//   USDC example,
// - the actual test values are scaled down to 1000 / 500 / 250 USDC so the
//   arithmetic stays easy to inspect in account data and logs,
// - every ratio is identical, so the conclusions are the same.
//
// What these scenarios are proving:
// 1. A distressed withdrawal pays `entitlement * settlement_factor` immediately.
// 2. The unpaid portion is not lost. It is recorded on the lender position as:
//    - `haircut_owed`: exact amount still owed,
//    - `withdrawal_sf`: the settlement factor at which that debt was anchored.
// 3. The market tracks that same unpaid amount in two ways:
//    - `haircut_accumulator`: exact reserve that sweep paths must leave alone,
//    - `HaircutState`: conservative aggregate that lets `re_settle` see later
//      repayments and improve SF without double-counting prior withdrawals.
// 4. When SF improves, prior withdrawers can claim the proportional recovery
//    implied by the improvement from `withdrawal_sf` toward WAD.
// 5. As claims are paid, both the exact reserve and the conservative aggregate
//    shrink in lockstep.
//
// Baseline setup for every scenario in this section:
// - 2 lenders x 500 USDC deposit = 1000 USDC total supply,
// - borrower draws 500 USDC, leaving 500 in the vault,
// - 0% APR keeps `scale_factor == WAD`, so all changes come from settlement and
//   haircut math rather than interest growth.
// ===========================================================================

// ---------------------------------------------------------------------------
// Scenario 1: No repayment.
//
// The first post-maturity withdrawal locks SF at exactly 0.5 because the vault
// only has 500 against 1000 total lender entitlement. Each withdrawing lender
// receives half immediately and the other half becomes a stored haircut claim.
// With no new funds entering the vault, neither `re_settle` nor `claim_haircut`
// can make progress.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_no_repayment() {
    let (
        market,
        _vault,
        _borrower,
        lender_a,
        la_token,
        lender_b,
        lb_token,
        _mint,
        _mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    // Lender A withdraws (triggers settlement at SF = 500 / 1000 = 0.5).
    let wdr_a = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_a, &[&lender_a]).await;

    // Verify SF = 0.5
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert_eq!(
        parsed.settlement_factor_wad,
        WAD / 2,
        "SF should be exactly 0.5"
    );

    // Lender A got 250 (500 x 0.5).
    let la_bal = common::get_token_balance(&mut ctx, &la_token).await;
    assert_eq!(la_bal, 250 * USDC, "Lender A should receive 250K");

    // Lender A's missing 250 is preserved on-position for later recovery.
    let (la_pos, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_a = common::parse_lender_position(&common::get_account_data(&mut ctx, &la_pos).await);
    assert_eq!(
        pos_a.haircut_owed,
        250 * USDC,
        "Lender A haircut_owed should be 250K"
    );
    assert_eq!(pos_a.withdrawal_sf, WAD / 2, "withdrawal_sf should be 0.5");

    // Lender B withdraws at the same SF and gets the same treatment.
    let wdr_b = common::build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lb_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_b, &[&lender_b]).await;

    let lb_bal = common::get_token_balance(&mut ctx, &lb_token).await;
    assert_eq!(lb_bal, 250 * USDC, "Lender B should receive 250K");

    // Vault should be empty
    let (vault, _) = common::get_vault_pda(&market);
    let vault_bal = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_bal, 0, "Vault should be empty after both withdrawals");

    // HaircutState stores the conservative linearised form of both claims.
    // Each 250 haircut anchored at 0.5 contributes:
    // - weight = 500
    // - offset = 250
    // So the aggregate is (1000, 500).
    let (hs_pda, _) = common::get_haircut_state_pda(&market);
    let hs = common::parse_haircut_state(&common::get_account_data(&mut ctx, &hs_pda).await);
    assert_eq!(
        hs.claim_weight_sum,
        1_000 * (USDC as u128),
        "weight_sum should be 1000 USDC"
    );
    assert_eq!(
        hs.claim_offset_sum,
        500 * (USDC as u128),
        "offset_sum should be 500 USDC"
    );

    // The exact reserve is also 500 (= 250 + 250). This is what sweep paths
    // must continue to protect.
    let md2 = common::get_account_data(&mut ctx, &market).await;
    let parsed2 = common::parse_market(&md2);
    assert_eq!(
        parsed2.haircut_accumulator,
        500 * USDC,
        "accumulator should be 500K"
    );

    // No repayment means no new value. The conservative solver therefore sees no
    // improvement and `re_settle` must fail.
    let rs = common::build_re_settle(&market, &vault);
    common::send_expect_error(&mut ctx, rs, &[], 31).await; // SettlementNotImproved

    // Claims also fail because the market SF is still anchored at 0.5.
    let claim_a = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
    common::send_expect_error(&mut ctx, claim_a, &[&lender_a], 31).await;
}

// ---------------------------------------------------------------------------
// Scenario 2: Partial repayment (250).
//
// One lender exits early at SF = 0.5. Later, the borrower repays exactly 250,
// which is enough to raise the shared market SF to 0.75. A lender who stayed in
// the market receives 375 on withdrawal. The early lender can then claim 125,
// bringing their total to the same 375. This demonstrates that early
// withdrawers and late withdrawers converge to equal economic treatment once the
// market improves.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_partial_repayment_equal_treatment() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        lender_b,
        lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Lender A withdraws at SF = 0.5, gets 250 immediately and leaves a 250
    // haircut claim behind.
    let wdr_a = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_a, &[&lender_a]).await;

    let la_after_withdraw = common::get_token_balance(&mut ctx, &la_token).await;
    assert_eq!(
        la_after_withdraw,
        250 * USDC,
        "Lender A initial payout should be 250K"
    );

    // Verify haircut state
    let (la_pos, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_a = common::parse_lender_position(&common::get_account_data(&mut ctx, &la_pos).await);
    assert_eq!(pos_a.haircut_owed, 250 * USDC);
    assert_eq!(pos_a.withdrawal_sf, WAD / 2);

    // Borrower repays 250. Those new tokens stay visible to `re_settle`; they
    // are not hidden behind the haircut reserve.
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 250 * USDC).await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        250 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    // Re-settle: SF improves to 0.75.
    // Formula: WAD * (V + O) / (R + W)
    //   V = 750 vault balance after repayment,
    //   O = 250 offset from lender A's stored haircut,
    //   R = 500 remaining lender entitlement,
    //   W = 500 weight from lender A's stored haircut.
    // So: WAD * (750 + 250) / (500 + 500) = 0.75 * WAD.
    let rs = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs, &[]).await;

    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert_eq!(
        parsed.settlement_factor_wad,
        WAD * 3 / 4,
        "SF should be exactly 0.75"
    );

    // Lender B stayed in the market, so withdrawing now pays 500 x 0.75 = 375.
    let wdr_b = common::build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lb_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_b, &[&lender_b]).await;

    let lb_bal = common::get_token_balance(&mut ctx, &lb_token).await;
    assert_eq!(
        lb_bal,
        375 * USDC,
        "Lender B should receive 375K at SF 0.75"
    );

    // Lender A claims the exact proportional improvement:
    //   250 * (0.75 - 0.5) / (1.0 - 0.5) = 125.
    let claim_a = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
    common::send_ok(&mut ctx, claim_a, &[&lender_a]).await;

    let la_final = common::get_token_balance(&mut ctx, &la_token).await;
    assert_eq!(
        la_final,
        375 * USDC,
        "Lender A total (250K + 125K claim) should be 375K"
    );

    // Equal treatment: the early withdrawer plus later claim equals the amount
    // the patient lender receives by waiting.
    assert_eq!(
        la_final, lb_bal,
        "Both lenders should receive equal total payouts"
    );

    // Vault should be empty: the remaining 500 in the vault was split between
    // lender B's 375 withdrawal and lender A's 125 claim.
    let vault_bal = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(vault_bal, 0, "Vault should be empty after all payouts");
}

// ---------------------------------------------------------------------------
// Scenario 3: Partial repayment now, full repayment later.
//
// This is the staged-recovery case:
// - first SF improvement unlocks only part of the early lender's haircut,
// - the claim path rebases the remaining unpaid amount to the new SF anchor,
// - a later full repayment can keep improving SF until the haircut is fully
//   cleared,
// - both lenders still converge to approximately full recovery, with only small
//   rounding dust from conservative math.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_partial_then_full_repayment() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        lender_b,
        lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Lender A exits at SF = 0.5, receiving 250 and leaving 250 still owed.
    let wdr_a = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_a, &[&lender_a]).await;
    assert_eq!(
        common::get_token_balance(&mut ctx, &la_token).await,
        250 * USDC
    );

    // --- Phase 1: Borrower repays 249 ---
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 249 * USDC).await;
    let rep1 = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        249 * USDC,
    );
    common::send_ok(&mut ctx, rep1, &[&borrower]).await;

    let rs1 = common::build_re_settle(&market, &vault);
    common::send_ok(&mut ctx, rs1, &[]).await;

    // SF improved, so lender A can claim only the portion unlocked by that
    // first improvement. The remainder stays on-position and the anchor moves
    // forward to the post-claim SF.
    let claim1 = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
    common::send_ok(&mut ctx, claim1, &[&lender_a]).await;

    let la_after_phase1 = common::get_token_balance(&mut ctx, &la_token).await;
    assert!(
        la_after_phase1 > 250 * USDC,
        "After phase 1: A should have more than initial 250K"
    );

    // Remaining haircut_owed should have decreased
    let (la_pos, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_a = common::parse_lender_position(&common::get_account_data(&mut ctx, &la_pos).await);
    assert!(
        pos_a.haircut_owed < 250 * USDC,
        "Remaining haircut should be less than 250K"
    );
    assert!(
        pos_a.withdrawal_sf > WAD / 2,
        "withdrawal_sf should be rebased above 0.5"
    );

    // --- Phase 2: Borrower repays the remaining 251 ---
    // Use a different amount from phase 1 so the transaction signature is
    // unique even though the account list is the same.
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 251 * USDC).await;
    let rep2 = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        251 * USDC,
    );
    common::send_ok(&mut ctx, rep2, &[&borrower]).await;

    // Re-settle + claim cycle until fully recovered.
    // Once SF reaches WAD, the remaining haircut becomes fully claimable and the
    // later `claim_haircut` call clears it completely.
    for nonce in 0u32..5 {
        let budget = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
            200_000 + nonce,
        );
        let rs = common::build_re_settle(&market, &vault);
        let _ = ctx
            .banks_client
            .process_transaction(Transaction::new_signed_with_payer(
                &[budget.clone(), rs],
                Some(&ctx.payer.pubkey()),
                &[&ctx.payer],
                ctx.banks_client.get_latest_blockhash().await.unwrap(),
            ))
            .await;

        let budget2 = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
            300_000 + nonce,
        );
        let claim = common::build_claim_haircut(&market, &lender_a.pubkey(), &la_token);
        let _ = ctx
            .banks_client
            .process_transaction(Transaction::new_signed_with_payer(
                &[budget2, claim],
                Some(&ctx.payer.pubkey()),
                &[&ctx.payer, &lender_a],
                ctx.banks_client.get_latest_blockhash().await.unwrap(),
            ))
            .await;
    }

    // Lender A should recover effectively the full 500, allowing for at most
    // 1 USDC of conservative rounding dust.
    let la_final = common::get_token_balance(&mut ctx, &la_token).await;
    assert!(
        la_final >= 499 * USDC,
        "Lender A should recover ~500K after full repayment: got {}",
        la_final
    );

    // Lender B waited until the market was fully repaired, so its withdrawal
    // should also land at essentially full value.
    let wdr_b = common::build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lb_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_b, &[&lender_b]).await;

    let lb_final = common::get_token_balance(&mut ctx, &lb_token).await;
    assert!(
        lb_final >= 499 * USDC,
        "Lender B should receive ~500K after full repayment: got {}",
        lb_final
    );

    // Both lenders should end up approximately equal despite one exiting early
    // and recovering in multiple phases.
    let diff = if la_final > lb_final {
        la_final - lb_final
    } else {
        lb_final - la_final
    };
    assert!(
        diff <= 1 * USDC,
        "Lenders should receive approximately equal amounts: A={la_final} B={lb_final} diff={diff}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: Full repayment before anyone withdraws.
//
// If the borrower repairs the vault before the first post-maturity withdrawal,
// settlement starts directly at WAD. No haircut state is created at all:
// positions stay clean, `haircut_accumulator` remains zero, and `HaircutState`
// stays zeroed.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_full_repayment_no_haircuts() {
    let (
        market,
        vault,
        borrower,
        lender_a,
        la_token,
        lender_b,
        lb_token,
        mint,
        mint_auth,
        blp,
        mut ctx,
    ) = setup_distressed_market().await;

    let brw_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    // Borrower repays the full 500 before anyone withdraws.
    common::mint_to_account(&mut ctx, &mint, &brw_token.pubkey(), &mint_auth, 500 * USDC).await;
    let rep = common::build_repay(
        &market,
        &borrower.pubkey(),
        &brw_token.pubkey(),
        &mint,
        &borrower.pubkey(),
        500 * USDC,
    );
    common::send_ok(&mut ctx, rep, &[&borrower]).await;

    // Vault now holds the full 1000 again (500 still in-vault deposits + 500
    // repaid by the borrower).
    let vault_bal = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_bal,
        1_000 * USDC,
        "Vault should have 1M after full repayment"
    );

    // Lender A's withdrawal is the first post-maturity settlement action, so it
    // locks SF at 1000 / 1000 = WAD.
    let wdr_a = common::build_withdraw(
        &market,
        &lender_a.pubkey(),
        &la_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_a, &[&lender_a]).await;

    // SF should be WAD
    let md = common::get_account_data(&mut ctx, &market).await;
    let parsed = common::parse_market(&md);
    assert_eq!(
        parsed.settlement_factor_wad, WAD,
        "SF should be WAD (1.0) after full repayment"
    );

    // Full payout means no haircut bookkeeping is created.
    let la_bal = common::get_token_balance(&mut ctx, &la_token).await;
    assert_eq!(la_bal, 500 * USDC, "Lender A should receive full 500K");

    // No haircut recorded
    let (la_pos, _) = common::get_lender_position_pda(&market, &lender_a.pubkey());
    let pos_a = common::parse_lender_position(&common::get_account_data(&mut ctx, &la_pos).await);
    assert_eq!(pos_a.haircut_owed, 0, "No haircut when SF = WAD");
    assert_eq!(
        pos_a.withdrawal_sf, 0,
        "withdrawal_sf should be 0 (no haircut)"
    );

    // Lender B also withdraws at full value.
    let wdr_b = common::build_withdraw(
        &market,
        &lender_b.pubkey(),
        &lb_token,
        &blp.pubkey(),
        0u128,
        0,
    );
    common::send_ok(&mut ctx, wdr_b, &[&lender_b]).await;

    let lb_bal = common::get_token_balance(&mut ctx, &lb_token).await;
    assert_eq!(lb_bal, 500 * USDC, "Lender B should receive full 500K");

    // Because no distressed withdrawal ever happened, the conservative
    // settlement aggregate stays zeroed.
    let (hs_pda, _) = common::get_haircut_state_pda(&market);
    let hs = common::parse_haircut_state(&common::get_account_data(&mut ctx, &hs_pda).await);
    assert_eq!(hs.claim_weight_sum, 0, "weight_sum should be 0");
    assert_eq!(hs.claim_offset_sum, 0, "offset_sum should be 0");

    // The exact reserve also stays zero.
    assert_eq!(parsed.haircut_accumulator, 0, "accumulator should be 0");

    // Vault should be empty
    let vault_final = common::get_token_balance(&mut ctx, &vault).await;
    assert_eq!(
        vault_final, 0,
        "Vault should be empty after both full withdrawals"
    );
}
