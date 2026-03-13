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
    compute_budget::ComputeBudgetInstruction,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

/// 1 USDC with 6 decimals.
const USDC: u64 = 1_000_000;
const WAD: u128 = 1_000_000_000_000_000_000u128;
const BPS: u128 = 10_000;
const SECONDS_PER_YEAR: u128 = 31_536_000;
const ERR_BLACKLISTED: u32 = 7;
const ERR_NO_BALANCE: u32 = 23;
const ERR_PAYOUT_BELOW_MINIMUM: u32 = 42;

// ---------------------------------------------------------------------------
// Helper: read u128 from account data at a given byte offset (little-endian)
// ---------------------------------------------------------------------------
fn read_u128(data: &[u8], offset: usize) -> u128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&data[offset..offset + 16]);
    u128::from_le_bytes(buf)
}

// ---------------------------------------------------------------------------
// Helper: read u64 from account data at a given byte offset (little-endian)
// ---------------------------------------------------------------------------
fn read_u64(data: &[u8], offset: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(buf)
}

fn compute_settlement_factor_for_tests(
    available_for_lenders: u128,
    total_normalized: u128,
) -> u128 {
    if total_normalized == 0 {
        return WAD;
    }
    let raw = available_for_lenders
        .checked_mul(WAD)
        .expect("available * WAD overflow")
        / total_normalized;
    if raw > WAD {
        WAD
    } else if raw < 1 {
        1
    } else {
        raw
    }
}

fn compute_payout_for_tests(
    scaled_amount: u128,
    scale_factor: u128,
    settlement_factor: u128,
) -> u64 {
    let normalized_amount = scaled_amount
        .checked_mul(scale_factor)
        .expect("scaled * scale_factor overflow")
        / WAD;
    let payout_u128 = normalized_amount
        .checked_mul(settlement_factor)
        .expect("normalized * settlement_factor overflow")
        / WAD;
    u64::try_from(payout_u128).expect("payout must fit u64")
}

const SECONDS_PER_DAY: i64 = 86_400;
const DAYS_PER_YEAR: u128 = 365;

fn mul_wad_test(a: u128, b: u128) -> u128 {
    a.checked_mul(b)
        .expect("mul_wad overflow")
        .checked_div(WAD)
        .expect("mul_wad div zero")
}

fn pow_wad_test(base: u128, exp: u32) -> u128 {
    let mut result = WAD;
    let mut b = base;
    let mut e = exp;
    while e > 0 {
        if e & 1 == 1 {
            result = mul_wad_test(result, b);
        }
        e >>= 1;
        if e > 0 {
            b = mul_wad_test(b, b);
        }
    }
    result
}

fn project_scale_and_fees_for_withdraw(
    market: &common::ParsedMarket,
    fee_rate_bps: u16,
    current_ts: i64,
) -> (u128, u64) {
    let effective_now = core::cmp::min(current_ts, market.maturity_timestamp);
    if effective_now <= market.last_accrual_timestamp {
        return (market.scale_factor, market.accrued_protocol_fees);
    }

    let time_elapsed = effective_now - market.last_accrual_timestamp;
    let annual_bps = u128::from(market.annual_interest_bps);

    let days_elapsed = time_elapsed / SECONDS_PER_DAY;
    let remaining_seconds = time_elapsed % SECONDS_PER_DAY;
    let days_elapsed_u32 = u32::try_from(days_elapsed).expect("days fit u32");
    let remaining_seconds_u128 = u128::try_from(remaining_seconds).expect("remaining fit u128");

    // daily_rate_wad = annual_bps * WAD / (365 * BPS)
    let daily_rate_wad = annual_bps
        .checked_mul(WAD)
        .expect("daily_rate overflow")
        .checked_div(DAYS_PER_YEAR.checked_mul(BPS).expect("365*BPS"))
        .expect("daily_rate div");

    // Compound full days: (1 + daily_rate) ^ whole_days
    let days_growth = pow_wad_test(WAD + daily_rate_wad, days_elapsed_u32);

    // Linear remaining seconds: 1 + annual_bps * remaining * WAD / (SECONDS_PER_YEAR * BPS)
    let remaining_delta_wad = annual_bps
        .checked_mul(remaining_seconds_u128)
        .expect("remaining * bps overflow")
        .checked_mul(WAD)
        .expect("remaining * WAD overflow")
        .checked_div(SECONDS_PER_YEAR.checked_mul(BPS).expect("spy*bps"))
        .expect("remaining div");
    let remaining_growth = WAD + remaining_delta_wad;

    let total_growth = mul_wad_test(days_growth, remaining_growth);
    let new_scale_factor = mul_wad_test(market.scale_factor, total_growth);
    let interest_delta_wad = total_growth - WAD;

    let mut new_fees = market.accrued_protocol_fees;
    if fee_rate_bps > 0 {
        let fee_delta_wad = interest_delta_wad
            .checked_mul(u128::from(fee_rate_bps))
            .expect("interest_delta * fee_rate overflow")
            .checked_div(BPS)
            .expect("division by zero in fee delta");
        // Use pre-accrual scale_factor (matches on-chain Finding 10 fix)
        let fee_normalized = market
            .scaled_total_supply
            .checked_mul(market.scale_factor)
            .expect("scaled_total_supply * scale_factor overflow")
            .checked_div(WAD)
            .expect("division by zero in fee normalized (1)")
            .checked_mul(fee_delta_wad)
            .expect("normalized * fee_delta overflow")
            .checked_div(WAD)
            .expect("division by zero in fee normalized (2)");
        new_fees = new_fees
            .checked_add(u64::try_from(fee_normalized).expect("fee_normalized must fit u64"))
            .expect("accrued_protocol_fees overflow");
    }

    (new_scale_factor, new_fees)
}

// State offset constants (from spec) - add 9 for discriminator (8) + version (1)
const MARKET_TOTAL_REPAID_OFFSET: usize = 188; // u64 at [188..196]
const POSITION_SCALED_BALANCE_OFFSET: usize = 73; // u128 at [73..89]

// ===========================================================================
// 1. test_repay_after_maturity_succeeds
//    Full lifecycle: deposit, borrow, advance clock past maturity, repay.
//    Repay should succeed even after maturity (it is permissionless).
//    Verify total_repaid increased.
// ===========================================================================
#[tokio::test]
async fn test_repay_after_maturity_succeeds() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
    ] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let fee_rate_bps: u16 = 500;
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &mint_authority.pubkey(),
                1_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
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
    let deposit_ix = common::build_deposit(
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
    let borrow_ix = common::build_borrow(
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

    // Mint extra tokens for repayment so we can exercise x-1/x/x+1 maturity boundaries.
    let repay_amount = 500 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &borrower_token.pubkey(),
        &mint_authority,
        repay_amount,
    )
    .await;

    let mut borrower_balance = common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance,
        borrow_amount + repay_amount,
        "borrower should hold borrowed amount + minted top-up before repayments"
    );
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());

    // Repay around maturity boundary: x-1, x, x+1 (permissionless and always allowed).
    let boundary_repays = [
        (maturity_timestamp - 1, 100 * USDC, 100 * USDC),
        (maturity_timestamp, 200 * USDC, 300 * USDC),
        (maturity_timestamp + 301, 200 * USDC, 500 * USDC),
    ];

    for (i, (when, repay_chunk, expected_total_repaid)) in boundary_repays.iter().enumerate() {
        common::get_blockhash_pinned(&mut ctx, *when).await;
        let repay_ix = common::build_repay(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &mint,
            &borrower.pubkey(),
            *repay_chunk,
        );
        let budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(300_000 - (i as u32) * 10_000);
        let tx = Transaction::new_signed_with_payer(
            &[budget_ix, repay_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("repay around maturity boundary should succeed");

        borrower_balance -= *repay_chunk;
        assert_eq!(
            common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
            borrower_balance,
            "borrower token balance should decrease by repay chunk at timestamp {}",
            when
        );

        let market_data = common::get_account_data(&mut ctx, &market).await;
        let total_repaid = read_u64(&market_data, MARKET_TOTAL_REPAID_OFFSET);
        assert_eq!(
            total_repaid, *expected_total_repaid,
            "total_repaid should track cumulative repays at timestamp {}",
            when
        );
    }

    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    let wl = common::parse_borrower_whitelist(&wl_data);
    assert_eq!(
        wl.current_borrowed, 0,
        "borrower debt should be fully cleared after cumulative boundary repays"
    );
}

// ===========================================================================
// 2. test_repay_cumulative
//    Deposit, borrow, repay 100, repay 200. Verify total_repaid = 300.
// ===========================================================================
#[tokio::test]
async fn test_repay_cumulative() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
    ] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let fee_rate_bps: u16 = 500;
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &mint_authority.pubkey(),
                1_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
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
    let deposit_ix = common::build_deposit(
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

    // Borrow 500 USDC
    let borrow_amount = 500 * USDC;
    let borrow_ix = common::build_borrow(
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

    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let mut borrower_balance = common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await;
    assert_eq!(
        borrower_balance, borrow_amount,
        "borrower should initially hold exactly borrowed amount"
    );

    // x-1/x/x+1 around 100 USDC: 99, 100, 101.
    let repay_cases = [
        (99 * USDC, 99 * USDC, 401 * USDC),
        (100 * USDC, 199 * USDC, 301 * USDC),
        (101 * USDC, 300 * USDC, 200 * USDC),
    ];

    for (repay_chunk, expected_total_repaid, expected_debt) in repay_cases {
        let repay_ix = common::build_repay(
            &market,
            &borrower.pubkey(),
            &borrower_token.pubkey(),
            &mint,
            &borrower.pubkey(),
            repay_chunk,
        );
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[repay_ix],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &borrower],
            recent,
        );
        ctx.banks_client
            .process_transaction(tx)
            .await
            .expect("repay chunk should succeed");

        borrower_balance -= repay_chunk;
        assert_eq!(
            common::get_token_balance(&mut ctx, &borrower_token.pubkey()).await,
            borrower_balance,
            "borrower balance should decrease exactly by repay chunk"
        );

        let market_data = common::get_account_data(&mut ctx, &market).await;
        let total_repaid = read_u64(&market_data, MARKET_TOTAL_REPAID_OFFSET);
        assert_eq!(
            total_repaid, expected_total_repaid,
            "total_repaid must be cumulative across boundary-neighbor repay chunks"
        );

        let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
        let wl = common::parse_borrower_whitelist(&wl_data);
        assert_eq!(
            wl.current_borrowed, expected_debt,
            "borrower debt should decrease exactly with each repay chunk"
        );
    }
}

// ===========================================================================
// 3. test_withdraw_blacklisted_lender
//    Setup blacklist for lender with status=1 before attempting withdraw.
//    Deposit first, advance past maturity, try withdraw => Custom(7).
// ===========================================================================
#[tokio::test]
async fn test_withdraw_blacklisted_lender() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();
    let peer_lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
        &peer_lender,
    ] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let fee_rate_bps: u16 = 500;
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &mint_authority.pubkey(),
                1_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let peer_lender_token =
        common::create_token_account(&mut ctx, &mint, &peer_lender.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    let peer_deposit_amount = 200 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &peer_lender_token.pubkey(),
        &mint_authority,
        peer_deposit_amount,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
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
    let deposit_ix = common::build_deposit(
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

    // Peer lender deposit (not blacklisted) for boundary-neighbor success path.
    let peer_deposit_ix = common::build_deposit(
        &market,
        &peer_lender.pubkey(),
        &peer_lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        peer_deposit_amount,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[peer_deposit_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &peer_lender],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance clock past maturity
    let withdraw_ts = maturity_timestamp + 301;
    common::advance_clock_past(&mut ctx, withdraw_ts).await;

    // Blacklist the lender (status=1)
    common::setup_blacklist_account(
        &mut ctx,
        &blacklist_program.pubkey(),
        &lender.pubkey(),
        1, // blacklisted
    );

    // Try to withdraw while blacklisted -- must fail with Blacklisted and no mutation.
    let (vault, _) = common::get_vault_pda(&market);
    let (pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let (peer_pos_pda, _) = common::get_lender_position_pda(&market, &peer_lender.pubkey());
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda, peer_pos_pda])
            .await;
    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    let withdraw_ix = common::build_withdraw(
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
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, ERR_BLACKLISTED);

    let snap_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda, peer_pos_pda])
            .await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_balance_before,
        "blacklisted withdraw must not transfer lender tokens"
    );

    // Boundary-neighbor success: another non-blacklisted lender can withdraw.
    let peer_balance_before =
        common::get_token_balance(&mut ctx, &peer_lender_token.pubkey()).await;
    let withdraw_ix = common::build_withdraw(
        &market,
        &peer_lender.pubkey(),
        &peer_lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &peer_lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("non-blacklisted peer lender should be able to withdraw");
    assert!(
        common::get_token_balance(&mut ctx, &peer_lender_token.pubkey()).await
            > peer_balance_before,
        "successful peer withdrawal should increase peer lender token balance"
    );

    let pos_data = common::get_account_data(&mut ctx, &peer_pos_pda).await;
    let scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_balance, 0,
        "full withdraw should empty non-blacklisted peer lender position"
    );
}

// ===========================================================================
// 4. test_withdraw_no_balance
//    Deposit, advance past maturity, withdraw all (scaled_amount=0 for full),
//    then try to withdraw again => Custom(23) NoBalance.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_no_balance() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
    ] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let fee_rate_bps: u16 = 500;
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &mint_authority.pubkey(),
                1_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
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
    let deposit_ix = common::build_deposit(
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

    // Advance clock past maturity
    let withdraw_ts = maturity_timestamp + 301;
    common::advance_clock_past(&mut ctx, withdraw_ts).await;

    // Compute exact expected full-withdraw payout from on-chain state.
    let (vault, _) = common::get_vault_pda(&market);
    let (pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let market_before = common::parse_market(&common::get_account_data(&mut ctx, &market).await);
    let pos_before =
        common::parse_lender_position(&common::get_account_data(&mut ctx, &pos_pda).await);
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;
    let (projected_scale_factor, _projected_fees) =
        project_scale_and_fees_for_withdraw(&market_before, fee_rate_bps, withdraw_ts);
    // COAL-C01: Settlement factor uses full vault balance, no fee reservation.
    let available_for_lenders = u128::from(vault_before);
    let total_normalized = market_before
        .scaled_total_supply
        .checked_mul(projected_scale_factor)
        .expect("scaled_total_supply * scale_factor overflow")
        / WAD;
    let settlement_factor = if market_before.settlement_factor_wad == 0 {
        compute_settlement_factor_for_tests(available_for_lenders, total_normalized)
    } else {
        market_before.settlement_factor_wad
    };
    let expected_payout = compute_payout_for_tests(
        pos_before.scaled_balance,
        projected_scale_factor,
        settlement_factor,
    );
    assert!(
        expected_payout > 0,
        "full withdrawal should produce non-zero payout in funded market"
    );

    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;

    // x+1 min_payout boundary should fail atomically.
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda]).await;
    let withdraw_fail_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        expected_payout.saturating_add(1),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_fail_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, ERR_PAYOUT_BELOW_MINIMUM);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_balance_before,
        "failed min_payout check must not change lender balance"
    );
    let snap_after = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda]).await;
    snap_before.assert_unchanged(&snap_after);

    // x boundary: exact payout minimum should succeed.
    let withdraw_ok_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        expected_payout,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ok_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("full withdrawal with exact minimum payout should succeed");

    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after - lender_balance_before,
        expected_payout,
        "full withdrawal payout should match exact formula"
    );
    let pos_after =
        common::parse_lender_position(&common::get_account_data(&mut ctx, &pos_pda).await);
    assert_eq!(
        pos_after.scaled_balance, 0,
        "position should be empty after full withdrawal"
    );

    // After balance reaches zero, both full (0) and explicit (1) withdrawals must fail with NoBalance.
    for scaled_amount in [0u128, 1u128] {
        let snap_before =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda]).await;
        let lender_balance_before_fail =
            common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
        let withdraw_fail_no_balance = common::build_withdraw(
            &market,
            &lender.pubkey(),
            &lender_token.pubkey(),
            &blacklist_program.pubkey(),
            scaled_amount,
            0,
        );
        let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[withdraw_fail_no_balance],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &lender],
            recent,
        );
        let result = ctx
            .banks_client
            .process_transaction(tx)
            .await
            .map_err(|e| e.unwrap());
        common::assert_custom_error(&result, ERR_NO_BALANCE);
        assert_eq!(
            common::get_token_balance(&mut ctx, &lender_token.pubkey()).await,
            lender_balance_before_fail,
            "failed no-balance withdrawal (scaled={}) must not transfer tokens",
            scaled_amount
        );
        let snap_after =
            common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda]).await;
        snap_before.assert_unchanged(&snap_after);
    }
}

// ===========================================================================
// 5. test_withdraw_multiple_partials
//    Deposit, advance past maturity, read scaled_balance from position,
//    then withdraw 1/3, then 1/3, then remaining 1/3. All succeed.
//    After last, position has 0 balance.
// ===========================================================================
#[tokio::test]
async fn test_withdraw_multiple_partials() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
    ] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let fee_rate_bps: u16 = 500;
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &mint_authority.pubkey(),
                1_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
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
    let deposit_ix = common::build_deposit(
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

    // Read scaled_balance after deposit
    let (pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let pos_data = common::get_account_data(&mut ctx, &pos_pda).await;
    let full_scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert!(
        full_scaled_balance > 0,
        "Scaled balance should be > 0 after deposit"
    );

    // Compute thirds
    let one_third = full_scaled_balance / 3;
    let remaining_third = full_scaled_balance - one_third - one_third; // handles rounding

    // Advance clock past maturity
    let withdraw_ts = maturity_timestamp + 301;
    common::advance_clock_past(&mut ctx, withdraw_ts).await;

    // Partial withdraw 1/3
    let withdraw_ix_1 = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        one_third,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix_1],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("first partial withdrawal should succeed");

    // Verify remaining = 2/3
    let pos_data = common::get_account_data(&mut ctx, &pos_pda).await;
    let scaled_after_first = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_after_first,
        full_scaled_balance - one_third,
        "Scaled balance should be 2/3 after first withdrawal"
    );

    // Partial withdraw another 1/3
    let withdraw_ix_2 = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        one_third,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix_2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("second partial withdrawal should succeed");

    // Verify remaining = 1/3 (the remaining third)
    let pos_data = common::get_account_data(&mut ctx, &pos_pda).await;
    let scaled_after_second = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_after_second, remaining_third,
        "Scaled balance should be the remaining 1/3 after second withdrawal"
    );

    // Withdraw the remaining 1/3
    let withdraw_ix_3 = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        remaining_third,
        0,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix_3],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("third partial withdrawal should succeed");

    // Verify position has 0 balance
    let pos_data = common::get_account_data(&mut ctx, &pos_pda).await;
    let scaled_after_third = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_after_third, 0,
        "Position should have 0 balance after all three partial withdrawals"
    );
}

// ===========================================================================
// 6. test_withdraw_underfunded_vault
//    Deposit 1000, borrow 800, no repay, advance past maturity. Withdraw.
//    The settlement factor < WAD because vault has only 200.
//    Lender payout should be < 1000 (specifically ~200 minus fees).
// ===========================================================================
#[tokio::test]
async fn test_withdraw_underfunded_vault() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();
    let lender = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    for kp in [
        &admin,
        &fee_authority,
        &whitelist_manager,
        &borrower,
        &lender,
    ] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let fee_rate_bps: u16 = 500; // 5%
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    let mint_authority = Keypair::new();
    {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &mint_authority.pubkey(),
                1_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
    let mint = common::create_mint(&mut ctx, &mint_authority, 6).await;

    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    let borrower_token = common::create_token_account(&mut ctx, &mint, &borrower.pubkey()).await;

    let deposit_amount = 1_000 * USDC;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let maturity_timestamp = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
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

    // Deposit 1000 USDC
    let deposit_ix = common::build_deposit(
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

    // Borrow 800 USDC (leaving only 200 in vault)
    let borrow_amount = 800 * USDC;
    let borrow_ix = common::build_borrow(
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

    // NO repay -- vault only has ~200 USDC

    // Advance clock past maturity
    let withdraw_ts = maturity_timestamp + 301;
    common::advance_clock_past(&mut ctx, withdraw_ts).await;

    // Compute exact expected payout for full withdrawal from on-chain pre-settlement state.
    let lender_balance_before = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let (vault, _) = common::get_vault_pda(&market);
    let (pos_pda, _) = common::get_lender_position_pda(&market, &lender.pubkey());
    let vault_before = common::get_token_balance(&mut ctx, &vault).await;
    let market_before = common::parse_market(&common::get_account_data(&mut ctx, &market).await);
    let (projected_scale_factor, _projected_fees) =
        project_scale_and_fees_for_withdraw(&market_before, fee_rate_bps, withdraw_ts);
    // COAL-C01: Settlement factor uses full vault balance, no fee reservation.
    let available_for_lenders = u128::from(vault_before);
    let total_normalized = market_before
        .scaled_total_supply
        .checked_mul(projected_scale_factor)
        .expect("scaled_total_supply * scale_factor overflow")
        / WAD;
    let settlement_factor = if market_before.settlement_factor_wad == 0 {
        compute_settlement_factor_for_tests(available_for_lenders, total_normalized)
    } else {
        market_before.settlement_factor_wad
    };
    let pos_before =
        common::parse_lender_position(&common::get_account_data(&mut ctx, &pos_pda).await);
    let expected_payout = compute_payout_for_tests(
        pos_before.scaled_balance,
        projected_scale_factor,
        settlement_factor,
    );
    assert!(
        available_for_lenders > 0 && available_for_lenders < u128::from(deposit_amount),
        "underfunded path expects non-zero but haircuted lender availability"
    );
    assert!(
        expected_payout > 0 && expected_payout < deposit_amount,
        "underfunded full withdrawal payout should be non-zero and haircuted"
    );

    // x+1 min_payout boundary should fail atomically.
    let snap_before =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda]).await;
    let withdraw_fail_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        expected_payout.saturating_add(1),
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_fail_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, ERR_PAYOUT_BELOW_MINIMUM);
    let snap_after = common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[pos_pda]).await;
    snap_before.assert_unchanged(&snap_after);
    assert_eq!(
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await,
        lender_balance_before,
        "failed min_payout check must not transfer lender tokens"
    );

    // x boundary: exact payout minimum should succeed.
    let withdraw_ix = common::build_withdraw(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &blacklist_program.pubkey(),
        0u128,
        expected_payout,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("withdraw from underfunded vault should succeed");

    // Verify: lender received some tokens but less than the deposited amount
    let lender_balance_after = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let payout = lender_balance_after - lender_balance_before;

    assert!(
        payout > 0 && payout < deposit_amount,
        "Lender payout ({}) should be less than deposit amount ({}) due to underfunded vault",
        payout,
        deposit_amount
    );
    assert_eq!(
        payout, expected_payout,
        "underfunded withdrawal payout should match exact formula"
    );

    // Verify position is emptied
    let pos_data = common::get_account_data(&mut ctx, &pos_pda).await;
    let scaled_balance = read_u128(&pos_data, POSITION_SCALED_BALANCE_OFFSET);
    assert_eq!(
        scaled_balance, 0,
        "Position should have 0 balance after full withdrawal"
    );

    let market_after = common::parse_market(&common::get_account_data(&mut ctx, &market).await);
    assert!(
        market_after.settlement_factor_wad == settlement_factor,
        "settlement factor should lock to the precomputed value"
    );
    assert!(
        market_after.settlement_factor_wad > 0 && market_after.settlement_factor_wad < WAD,
        "underfunded market should settle to a strict haircut factor in (0, WAD)"
    );
}
