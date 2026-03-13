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

use solana_program_test::*;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};

// ---------------------------------------------------------------------------
// Constants mirroring the on-chain state layout sizes and the WAD constant
// ---------------------------------------------------------------------------

/// Fixed-point precision constant = 1e18 (must match src/constants.rs).
const WAD: u128 = 1_000_000_000_000_000_000;

/// BorrowerWhitelist account size (96 bytes, matches BORROWER_WHITELIST_SIZE).
const BORROWER_WHITELIST_SIZE: usize = 96;

/// Market account size (250 bytes, matches MARKET_SIZE).
const MARKET_SIZE: usize = 250;

// ---------------------------------------------------------------------------
// Helper: extract Custom(N) from a failed transaction result
// ---------------------------------------------------------------------------

fn assert_custom_error(result: Result<(), TransactionError>, expected_code: u32) {
    match result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(code))) => {
            assert_eq!(
                code, expected_code,
                "expected Custom({expected_code}), got Custom({code})"
            );
        },
        Err(other) => panic!("expected Custom({expected_code}), got {other:?}"),
        Ok(()) => panic!("expected Custom({expected_code}), but transaction succeeded"),
    }
}

// ===========================================================================
// SetBorrowerWhitelist tests
// ===========================================================================

#[tokio::test]
async fn test_set_borrower_whitelist_success() {
    // 1. Boot test validator with BPF program loaded
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fee_rate_bps: u16 = 500;

    // Airdrop SOL to admin & whitelist_manager so they can pay for txns
    let airdrop_amount = 10_000_000_000; // 10 SOL
    {
        let admin_tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &admin.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client
            .process_transaction(admin_tx)
            .await
            .unwrap();

        let wm_tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &whitelist_manager.pubkey(),
                airdrop_amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(wm_tx).await.unwrap();
    }

    // 2. Initialize protocol
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        fee_rate_bps,
    )
    .await;

    // 3. Whitelist a borrower
    let borrower = Keypair::new();
    let max_borrow_capacity: u64 = 1_000_000_000_000; // 1M USDC (6 dec)

    let ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1, // is_whitelisted
        max_borrow_capacity,
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&whitelist_manager.pubkey()),
        &[&whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // 4. Read back the BorrowerWhitelist account and verify fields
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;

    assert_eq!(wl_data.len(), BORROWER_WHITELIST_SIZE);

    // Layout of BorrowerWhitelist (96 bytes, repr(C)):
    // Note: All offsets add 9 to skip discriminator (8 bytes) + version (1 byte)
    //   [9..41)   borrower:            [u8; 32]
    //   [41]      is_whitelisted:      u8
    //   [42..50)  max_borrow_capacity: [u8; 8] (LE u64)
    //   [50..58)  total_borrowed:      [u8; 8] (LE u64)
    //   [58]      bump:                u8
    //   [59..96)  _padding:            [u8; 37]
    let stored_borrower = &wl_data[9..41];
    assert_eq!(stored_borrower, borrower.pubkey().as_ref());

    let stored_is_whitelisted = wl_data[41];
    assert_eq!(stored_is_whitelisted, 1);

    let stored_capacity = u64::from_le_bytes(wl_data[42..50].try_into().unwrap());
    assert_eq!(stored_capacity, max_borrow_capacity);

    let stored_total_borrowed = u64::from_le_bytes(wl_data[50..58].try_into().unwrap());
    assert_eq!(stored_total_borrowed, 0);
}

#[tokio::test]
async fn test_set_borrower_whitelist_wrong_manager() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let fake_manager = Keypair::new(); // NOT the real whitelist_manager

    // Fund accounts
    let airdrop_amount = 10_000_000_000;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &fake_manager.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Initialize protocol with the REAL whitelist_manager
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Attempt to whitelist using the FAKE manager
    let borrower = Keypair::new();
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    let protocol_before = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    let ix = common::build_set_borrower_whitelist(
        &fake_manager.pubkey(),
        &borrower.pubkey(),
        1,
        1_000_000_000_000,
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&fake_manager.pubkey()),
        &[&fake_manager],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Expect Custom(5) = Unauthorized
    assert_custom_error(result, 5);
    assert!(
        ctx.banks_client
            .get_account(wl_pda)
            .await
            .unwrap()
            .is_none(),
        "Whitelist account must not be created on Unauthorized failure"
    );
    let protocol_after = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(
        protocol_before, protocol_after,
        "Protocol config must remain unchanged on Unauthorized failure"
    );

    // Boundary neighbor: real whitelist manager should succeed for the same borrower.
    let ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        1_000_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&whitelist_manager.pubkey()),
        &[&whitelist_manager],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_data.len(), BORROWER_WHITELIST_SIZE);
    assert_eq!(&wl_data[9..41], borrower.pubkey().as_ref());
    assert_eq!(wl_data[41], 1);
    assert_eq!(
        u64::from_le_bytes(wl_data[42..50].try_into().unwrap()),
        1_000_000_000_000
    );
    assert_eq!(u64::from_le_bytes(wl_data[50..58].try_into().unwrap()), 0);
}

#[tokio::test]
async fn test_set_borrower_whitelist_zero_capacity() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();

    // Fund
    let airdrop_amount = 10_000_000_000;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Try whitelisting with max_borrow_capacity=0
    let borrower = Keypair::new();
    let (wl_pda, _) = common::get_borrower_whitelist_pda(&borrower.pubkey());
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    let protocol_before = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    let ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1, // is_whitelisted = true
        0, // max_borrow_capacity = 0  <-- invalid
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&whitelist_manager.pubkey()),
        &[&whitelist_manager],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Expect Custom(2) = InvalidCapacity
    assert_custom_error(result, 2);
    assert!(
        ctx.banks_client
            .get_account(wl_pda)
            .await
            .unwrap()
            .is_none(),
        "Whitelist account must not be created on InvalidCapacity failure"
    );
    let protocol_after = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(
        protocol_before, protocol_after,
        "Protocol config must remain unchanged on InvalidCapacity failure"
    );

    // Boundary neighbor: capacity=1 should succeed for the same borrower.
    let ix =
        common::build_set_borrower_whitelist(&whitelist_manager.pubkey(), &borrower.pubkey(), 1, 1);
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&whitelist_manager.pubkey()),
        &[&whitelist_manager],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let wl_data = common::get_account_data(&mut ctx, &wl_pda).await;
    assert_eq!(wl_data.len(), BORROWER_WHITELIST_SIZE);
    assert_eq!(&wl_data[9..41], borrower.pubkey().as_ref());
    assert_eq!(wl_data[41], 1);
    assert_eq!(u64::from_le_bytes(wl_data[42..50].try_into().unwrap()), 1);
    assert_eq!(u64::from_le_bytes(wl_data[50..58].try_into().unwrap()), 0);
}

// ===========================================================================
// CreateMarket tests
// ===========================================================================

#[tokio::test]
async fn test_create_market_success() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    // Fund all needed accounts
    let airdrop_amount = 10_000_000_000;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &borrower.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Initialize protocol first
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Create a 6-decimal mint (mimics USDC)
    let mint_pubkey = common::create_mint(&mut ctx, &admin, 6).await;

    let nonce: u64 = 1;
    let annual_interest_bps: u16 = 800; // 8%
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let max_total_supply: u64 = 10_000_000_000; // 10,000 USDC (6 decimals)
    let max_borrow_capacity: u64 = 50_000_000_000; // 50,000 USDC

    // Whitelist borrower and create market
    let market_pda = common::setup_market_full(
        &mut ctx,
        &admin,
        &borrower,
        &mint_pubkey,
        &blacklist_program.pubkey(),
        nonce,
        annual_interest_bps,
        maturity_timestamp,
        max_total_supply,
        &whitelist_manager,
        max_borrow_capacity,
    )
    .await;

    // --- Verify market account ---
    let market_data = common::get_account_data(&mut ctx, &market_pda).await;
    assert_eq!(market_data.len(), MARKET_SIZE);

    // Market layout (250 bytes, repr(C)):
    // Note: All offsets add 9 to skip discriminator (8 bytes) + version (1 byte)
    //   [9..41)     borrower                [u8; 32]
    //   [41..73)    mint                    [u8; 32]
    //   [73..105)   vault                   [u8; 32]
    //   [105]       market_authority_bump   u8
    //   [106..108)  annual_interest_bps     [u8; 2]  LE u16
    //   [108..116)  maturity_timestamp      [u8; 8]  LE i64
    //   [116..124)  max_total_supply        [u8; 8]  LE u64
    //   [124..132)  market_nonce            [u8; 8]  LE u64
    //   [132..148)  scaled_total_supply     [u8; 16] LE u128
    //   [148..164)  scale_factor            [u8; 16] LE u128
    //   [164..172)  accrued_protocol_fees   [u8; 8]  LE u64
    //   [172..180)  total_deposited         [u8; 8]  LE u64
    //   [180..188)  total_borrowed          [u8; 8]  LE u64
    //   [188..196)  total_repaid            [u8; 8]  LE u64
    //   [196..204)  total_interest_repaid   [u8; 8]  LE u64
    //   [204..212)  last_accrual_timestamp  [u8; 8]  LE i64
    //   [212..228)  settlement_factor_wad   [u8; 16] LE u128
    //   [228]       bump                    u8
    //   [229..250)  _padding                [u8; 21]

    // borrower
    assert_eq!(&market_data[9..41], borrower.pubkey().as_ref());

    // mint
    assert_eq!(&market_data[41..73], mint_pubkey.as_ref());

    // vault — should be the vault PDA
    let (vault_pda, _) = common::get_vault_pda(&market_pda);
    assert_eq!(&market_data[73..105], vault_pda.as_ref());

    // annual_interest_bps
    let stored_interest = u16::from_le_bytes(market_data[106..108].try_into().unwrap());
    assert_eq!(stored_interest, annual_interest_bps);

    // maturity_timestamp
    let stored_maturity = i64::from_le_bytes(market_data[108..116].try_into().unwrap());
    assert_eq!(stored_maturity, maturity_timestamp);

    // max_total_supply
    let stored_supply = u64::from_le_bytes(market_data[116..124].try_into().unwrap());
    assert_eq!(stored_supply, max_total_supply);

    // market_nonce
    let stored_nonce = u64::from_le_bytes(market_data[124..132].try_into().unwrap());
    assert_eq!(stored_nonce, nonce);

    // scale_factor must be WAD at creation
    let stored_scale = u128::from_le_bytes(market_data[148..164].try_into().unwrap());
    assert_eq!(stored_scale, WAD);

    // scaled_total_supply starts at 0
    let stored_scaled = u128::from_le_bytes(market_data[132..148].try_into().unwrap());
    assert_eq!(stored_scaled, 0);

    // settlement_factor_wad starts at 0
    let stored_settlement = u128::from_le_bytes(market_data[212..228].try_into().unwrap());
    assert_eq!(stored_settlement, 0);

    // --- Verify vault token account ---
    let vault_account = ctx
        .banks_client
        .get_account(vault_pda)
        .await
        .unwrap()
        .expect("vault account should exist");

    // Vault must be owned by the SPL Token program
    assert_eq!(vault_account.owner, spl_token::id());

    // Verify the vault token account's owner is the market authority PDA
    // SPL token account layout: mint (32) + owner (32) at offset 32
    let vault_data = &vault_account.data;
    let (market_authority_pda, _) = common::get_market_authority_pda(&market_pda);
    assert_eq!(&vault_data[32..64], market_authority_pda.as_ref());

    // Verify the vault's mint matches
    assert_eq!(&vault_data[0..32], mint_pubkey.as_ref());
}

#[tokio::test]
async fn test_create_market_not_whitelisted() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    // Fund
    let airdrop_amount = 10_000_000_000;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &borrower.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Initialize protocol (but do NOT whitelist the borrower)
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    let mint_pubkey = common::create_mint(&mut ctx, &admin, 6).await;
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let nonce: u64 = 1;
    let (market_pda, _) = common::get_market_pda(&borrower.pubkey(), nonce);
    let (vault_pda, _) = common::get_vault_pda(&market_pda);
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    let protocol_before = common::get_account_data(&mut ctx, &protocol_config_pda).await;

    // Attempt to create market without whitelisting borrower
    let ix = common::build_create_market(
        &borrower.pubkey(),
        &mint_pubkey,
        &blacklist_program.pubkey(),
        nonce, // nonce
        800,   // annual_interest_bps
        maturity_timestamp,
        10_000_000_000, // max_total_supply
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // The borrower_whitelist PDA does not exist, so it is owned by the system
    // program. The program detects this and returns InvalidAccountOwner (14).
    assert_custom_error(result, 14);
    assert!(
        ctx.banks_client
            .get_account(market_pda)
            .await
            .unwrap()
            .is_none(),
        "Market account must not be created on not-whitelisted failure"
    );
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_none(),
        "Vault account must not be created on not-whitelisted failure"
    );
    let protocol_after = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(
        protocol_before, protocol_after,
        "Protocol config must remain unchanged on create-market failure"
    );

    // Boundary neighbor: once borrower is whitelisted, create_market succeeds.
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&whitelist_manager.pubkey()),
        &[&whitelist_manager],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let create_ix = common::build_create_market(
        &borrower.pubkey(),
        &mint_pubkey,
        &blacklist_program.pubkey(),
        nonce,
        800,
        maturity_timestamp,
        10_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[create_ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        recent,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let market_data = common::get_account_data(&mut ctx, &market_pda).await;
    assert_eq!(market_data.len(), MARKET_SIZE);
    assert_eq!(&market_data[41..73], mint_pubkey.as_ref());
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_some(),
        "Vault account should exist after successful create_market"
    );
}

#[tokio::test]
async fn test_create_market_invalid_maturity() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    // Fund
    let airdrop_amount = 10_000_000_000;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &borrower.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Init protocol + whitelist borrower
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
    {
        let ix = common::build_set_borrower_whitelist(
            &whitelist_manager.pubkey(),
            &borrower.pubkey(),
            1,
            50_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&whitelist_manager.pubkey()),
            &[&whitelist_manager],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let mint_pubkey = common::create_mint(&mut ctx, &admin, 6).await;

    let nonce: u64 = 1;
    let (market_pda, _) = common::get_market_pda(&borrower.pubkey(), nonce);
    let (vault_pda, _) = common::get_vault_pda(&market_pda);
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    let protocol_before = common::get_account_data(&mut ctx, &protocol_config_pda).await;

    // Boundary x-1 relative to MIN_MATURITY_DELTA(60): invalid at +59s.
    let ix = common::build_create_market(
        &borrower.pubkey(),
        &mint_pubkey,
        &blacklist_program.pubkey(),
        nonce,
        800,
        common::PINNED_EPOCH + 59,
        10_000_000_000,
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Expect Custom(3) = InvalidMaturity
    assert_custom_error(result, 3);
    assert!(
        ctx.banks_client
            .get_account(market_pda)
            .await
            .unwrap()
            .is_none(),
        "Market account must not be created on InvalidMaturity failure"
    );
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_none(),
        "Vault account must not be created on InvalidMaturity failure"
    );
    let protocol_after = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(
        protocol_before, protocol_after,
        "Protocol config must remain unchanged on InvalidMaturity failure"
    );

    // Boundary neighbor x+1: +61s should succeed.
    let ix = common::build_create_market(
        &borrower.pubkey(),
        &mint_pubkey,
        &blacklist_program.pubkey(),
        nonce,
        800,
        common::PINNED_EPOCH + 61,
        10_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&borrower.pubkey()), &[&borrower], recent);
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let market_data = common::get_account_data(&mut ctx, &market_pda).await;
    assert_eq!(market_data.len(), MARKET_SIZE);
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_some(),
        "Vault account should exist after valid maturity create_market"
    );
}

#[tokio::test]
async fn test_create_market_wrong_mint_decimals() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    // Fund
    let airdrop_amount = 10_000_000_000;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &borrower.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Init protocol + whitelist borrower
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    {
        let ix = common::build_set_borrower_whitelist(
            &whitelist_manager.pubkey(),
            &borrower.pubkey(),
            1,
            50_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&whitelist_manager.pubkey()),
            &[&whitelist_manager],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Create a mint with 9 decimals (wrong -- should be 6)
    let bad_mint = common::create_random_mint(&mut ctx, &admin, 9).await;
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let nonce: u64 = 1;
    let (market_pda, _) = common::get_market_pda(&borrower.pubkey(), nonce);
    let (vault_pda, _) = common::get_vault_pda(&market_pda);
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    let protocol_before = common::get_account_data(&mut ctx, &protocol_config_pda).await;

    let ix = common::build_create_market(
        &borrower.pubkey(),
        &bad_mint,
        &blacklist_program.pubkey(),
        nonce, // nonce
        800,   // annual_interest_bps
        maturity_timestamp,
        10_000_000_000, // max_total_supply
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Expect Custom(11) = InvalidMint
    assert_custom_error(result, 11);
    assert!(
        ctx.banks_client
            .get_account(market_pda)
            .await
            .unwrap()
            .is_none(),
        "Market account must not be created on InvalidMint failure"
    );
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_none(),
        "Vault account must not be created on InvalidMint failure"
    );
    let protocol_after = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(
        protocol_before, protocol_after,
        "Protocol config must remain unchanged on InvalidMint failure"
    );

    // Boundary neighbor: same request with a 6-decimal mint should succeed.
    let good_mint = common::create_mint(&mut ctx, &admin, 6).await;
    let ix = common::build_create_market(
        &borrower.pubkey(),
        &good_mint,
        &blacklist_program.pubkey(),
        nonce,
        800,
        maturity_timestamp,
        10_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&borrower.pubkey()), &[&borrower], recent);
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let market_data = common::get_account_data(&mut ctx, &market_pda).await;
    assert_eq!(market_data.len(), MARKET_SIZE);
    assert_eq!(&market_data[41..73], good_mint.as_ref());
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_some(),
        "Vault account should exist after valid-mint create_market"
    );
}

#[tokio::test]
async fn test_create_market_zero_capacity() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    // Fund
    let airdrop_amount = 10_000_000_000;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &borrower.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Init protocol + whitelist borrower
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    {
        let ix = common::build_set_borrower_whitelist(
            &whitelist_manager.pubkey(),
            &borrower.pubkey(),
            1,
            50_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&whitelist_manager.pubkey()),
            &[&whitelist_manager],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let mint_pubkey = common::create_mint(&mut ctx, &admin, 6).await;
    let maturity_timestamp = common::FAR_FUTURE_MATURITY;
    let nonce: u64 = 1;
    let (market_pda, _) = common::get_market_pda(&borrower.pubkey(), nonce);
    let (vault_pda, _) = common::get_vault_pda(&market_pda);
    let (protocol_config_pda, _) = common::get_protocol_config_pda();
    let protocol_before = common::get_account_data(&mut ctx, &protocol_config_pda).await;

    // Create market with max_total_supply = 0
    let ix = common::build_create_market(
        &borrower.pubkey(),
        &mint_pubkey,
        &blacklist_program.pubkey(),
        nonce, // nonce
        800,   // annual_interest_bps
        maturity_timestamp,
        0, // max_total_supply = 0  <-- invalid
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&borrower.pubkey()),
        &[&borrower],
        ctx.last_blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .map_err(|e| e.unwrap());

    // Expect Custom(2) = InvalidCapacity
    assert_custom_error(result, 2);
    assert!(
        ctx.banks_client
            .get_account(market_pda)
            .await
            .unwrap()
            .is_none(),
        "Market account must not be created on InvalidCapacity failure"
    );
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_none(),
        "Vault account must not be created on InvalidCapacity failure"
    );
    let protocol_after = common::get_account_data(&mut ctx, &protocol_config_pda).await;
    assert_eq!(
        protocol_before, protocol_after,
        "Protocol config must remain unchanged on InvalidCapacity failure"
    );

    // Boundary neighbor: max_total_supply=1 should succeed.
    let ix = common::build_create_market(
        &borrower.pubkey(),
        &mint_pubkey,
        &blacklist_program.pubkey(),
        nonce,
        800,
        maturity_timestamp,
        1,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&borrower.pubkey()), &[&borrower], recent);
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let market_data = common::get_account_data(&mut ctx, &market_pda).await;
    assert_eq!(market_data.len(), MARKET_SIZE);
    assert_eq!(
        u64::from_le_bytes(market_data[116..124].try_into().unwrap()),
        1
    );
    assert!(
        ctx.banks_client
            .get_account(vault_pda)
            .await
            .unwrap()
            .is_some(),
        "Vault account should exist after valid-capacity create_market"
    );
}

// -----------------------------------------------------------------------
// COAL-L01: USDC_MINT enforcement in create_market
// -----------------------------------------------------------------------

/// Verify that create_market rejects a non-USDC mint (hardcoded USDC_MINT enforced).
#[tokio::test]
async fn test_create_market_usdc_mint_enforced() {
    let mut ctx = common::start_context().await;

    let admin = Keypair::new();
    let fee_authority = Keypair::new();
    let whitelist_manager = Keypair::new();
    let blacklist_program = Keypair::new();
    let borrower = Keypair::new();

    let airdrop_amount = 10_000_000_000u64;
    {
        let tx = Transaction::new_signed_with_payer(
            &[
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &admin.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &whitelist_manager.pubkey(),
                    airdrop_amount,
                ),
                solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &borrower.pubkey(),
                    airdrop_amount,
                ),
            ],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Initialize protocol
    common::setup_protocol(
        &mut ctx,
        &admin,
        &fee_authority.pubkey(),
        &whitelist_manager.pubkey(),
        &blacklist_program.pubkey(),
        500,
    )
    .await;

    // Whitelist borrower
    let wl_ix = common::build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1,
        50_000_000_000,
    );
    let recent = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let wl_tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &whitelist_manager],
        recent,
    );
    ctx.banks_client.process_transaction(wl_tx).await.unwrap();

    // Create a random 6-decimal mint (not the hardcoded USDC_MINT)
    let non_usdc_mint = common::create_random_mint(&mut ctx, &admin, 6).await;

    // Try to create a market with the non-USDC mint — should fail
    let create_ix = common::build_create_market(
        &borrower.pubkey(),
        &non_usdc_mint,
        &blacklist_program.pubkey(),
        1,
        800,
        common::FAR_FUTURE_MATURITY,
        10_000_000_000,
    );
    let recent2 = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let create_tx = Transaction::new_signed_with_payer(
        &[create_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &borrower],
        recent2,
    );

    let result = ctx.banks_client.process_transaction(create_tx).await;
    assert!(
        result.is_err(),
        "create_market should fail when mint is not the hardcoded USDC_MINT"
    );
}
