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

use solana_sdk::{signature::Keypair, signer::Signer, transaction::Transaction};

/// Test that set_sysvar on the Clock sysvar works for BPF programs
/// (ProgramTestContext::set_sysvar updates the sysvar cache that backs
/// the sol_get_clock_sysvar syscall).
#[tokio::test]
async fn test_clock_override_affects_bpf() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    for kp in [&admin, &whitelist_manager, &borrower] {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                10_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    common::setup_protocol(
        &mut ctx,
        &admin,
        &admin.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let mint = common::create_mint(&mut ctx, &admin, 6).await;

    let maturity_ts = common::FAR_FUTURE_MATURITY;

    let market = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint,
        &blacklist_program.pubkey(),
        1,
        1000,
        maturity_ts,
        10_000_000_000,
        &whitelist_manager,
        10_000_000_000,
    )
    .await;

    let lender = Keypair::new();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &lender.pubkey(),
            5_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();
    let lender_token = common::create_token_account(&mut ctx, &mint, &lender.pubkey()).await;
    common::mint_to_account(
        &mut ctx,
        &mint,
        &lender_token.pubkey(),
        &admin,
        1_000_000_000,
    )
    .await;

    let (vault, _) = common::get_vault_pda(&market);
    let (lender_position, _) = common::get_lender_position_pda(&market, &lender.pubkey());

    // x-1 boundary: just before maturity must allow deposit.
    let mut clock_before = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock_before.unix_timestamp = maturity_ts - 1;
    ctx.set_sysvar(&clock_before);

    let deposit_before_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        100_000_000,
    );
    assert_eq!(
        deposit_before_ix.accounts[3].pubkey, vault,
        "deposit vault account index must point to canonical vault PDA"
    );
    let lender_balance_before_success =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_before_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit should succeed at maturity-1 boundary");
    let lender_balance_after_success =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after_success,
        lender_balance_before_success - 100_000_000,
        "deposit at maturity-1 should debit lender token balance by exact amount"
    );
    let market_after_success =
        common::parse_market(&common::get_account_data(&mut ctx, &market).await);
    assert_eq!(
        market_after_success.total_deposited, 100_000_000,
        "market total_deposited should increase after maturity-1 deposit"
    );

    // x boundary: at maturity must reject with MarketMatured and not mutate state.
    let mut clock_at = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock_at.unix_timestamp = maturity_ts;
    ctx.set_sysvar(&clock_at);

    let lender_balance_before_at =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_before_at = common::get_account_data(&mut ctx, &lender_position).await;
    let snap_before_at =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let deposit_at_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        50_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_at_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 28); // MarketMatured
    let snap_after_at =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before_at.assert_unchanged(&snap_after_at);
    let lender_balance_after_at = common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after_at, lender_balance_before_at,
        "lender token balance changed on rejected maturity-boundary deposit"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before_at,
        "lender-position bytes changed on rejected maturity-boundary deposit"
    );

    // x+1 boundary: after maturity must also reject with MarketMatured and no mutation.
    let mut clock_after = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock_after.unix_timestamp = maturity_ts + 1;
    ctx.set_sysvar(&clock_after);

    let lender_balance_before_after =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    let lender_position_before_after = common::get_account_data(&mut ctx, &lender_position).await;
    let snap_before_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    let deposit_after_ix = common::build_deposit(
        &market,
        &lender.pubkey(),
        &lender_token.pubkey(),
        &mint,
        &blacklist_program.pubkey(),
        50_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[deposit_after_ix],
        Some(&lender.pubkey()),
        &[&lender],
        recent,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());
    common::assert_custom_error(&result, 28); // MarketMatured
    let snap_after_after =
        common::ProtocolSnapshot::capture(&mut ctx, &market, &vault, &[lender_position]).await;
    snap_before_after.assert_unchanged(&snap_after_after);
    let lender_balance_after_after =
        common::get_token_balance(&mut ctx, &lender_token.pubkey()).await;
    assert_eq!(
        lender_balance_after_after, lender_balance_before_after,
        "lender token balance changed on rejected maturity+1 deposit"
    );
    assert_eq!(
        common::get_account_data(&mut ctx, &lender_position).await,
        lender_position_before_after,
        "lender-position bytes changed on rejected maturity+1 deposit"
    );
}
