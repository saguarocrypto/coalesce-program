#![allow(dead_code)]
#![allow(deprecated)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::too_many_arguments)]

use solana_program_test::{BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::{AccountSharedData, WritableAccount},
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program,
    transaction::Transaction,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The on-chain program ID for the CoalesceFi Pinocchio program.
pub const PROGRAM_ID: Pubkey = solana_sdk::pubkey!("2xuc7ZLcVMWkVwVoVPkmeS6n3Picycyek4wqVVy2QbGy");

pub fn program_id() -> Pubkey {
    PROGRAM_ID
}

// Account sizes (mirrored from on-chain constants)
pub const PROTOCOL_CONFIG_SIZE: usize = 194;
pub const MARKET_SIZE: usize = 250;
pub const LENDER_POSITION_SIZE: usize = 128;
pub const BORROWER_WHITELIST_SIZE: usize = 96;
pub const HAIRCUT_STATE_SIZE: usize = 88;

// Discriminator constants (must match on-chain DISC_* values)
pub const DISC_PROTOCOL_CONFIG: [u8; 8] = *b"COALPC__";
pub const DISC_MARKET: [u8; 8] = *b"COALMKT_";
pub const DISC_LENDER_POSITION: [u8; 8] = *b"COALLPOS";
pub const DISC_BORROWER_WL: [u8; 8] = *b"COALBWL_";
pub const DISC_HAIRCUT_STATE: [u8; 8] = *b"COALHCST";

/// Deterministic epoch pinned by `start_context()` so that every test
/// begins at the same wall-clock time regardless of real elapsed time.
pub const PINNED_EPOCH: i64 = 1_700_000_000;

/// A maturity timestamp far enough in the future that no test will
/// accidentally cross it, but within the on-chain 5-year max-maturity
/// window relative to [`PINNED_EPOCH`].
/// Equals `PINNED_EPOCH + 4 years` ≈ 1,826,208,000.
pub const FAR_FUTURE_MATURITY: i64 = PINNED_EPOCH + 4 * 365 * 24 * 60 * 60;

/// A short maturity (30 days from epoch) for tests that need the vault to
/// remain solvent after compound interest accrual (e.g., fee collection).
pub const SHORT_MATURITY: i64 = PINNED_EPOCH + 30 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// ProgramTest builder
// ---------------------------------------------------------------------------

pub fn program_test() -> ProgramTest {
    let mut pt = ProgramTest::default();
    pt.prefer_bpf(true);
    pt.add_program("coalesce", program_id(), None);
    pt
}

/// Start a `ProgramTestContext` with the clock pinned to [`PINNED_EPOCH`].
///
/// This eliminates nondeterminism caused by `solana-program-test`
/// initializing the clock from wall-clock time.  We warp forward to a
/// high slot number so that subsequent slot advances (which add ~400 ms
/// each) only move the timestamp by fractions of a second — far too
/// small to affect any test.
pub async fn start_context() -> ProgramTestContext {
    let mut ctx = program_test().start_with_context().await;
    // Warp forward so the runtime's slot→timestamp mapping is
    // well past genesis.  Then override the timestamp to our
    // deterministic epoch.
    ctx.warp_to_slot(2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = PINNED_EPOCH;
    ctx.set_sysvar(&clock);
    ctx
}

// ---------------------------------------------------------------------------
// PDA helpers — each returns (Pubkey, bump)
// ---------------------------------------------------------------------------

pub fn get_protocol_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"protocol_config"], &program_id())
}

pub fn get_market_pda(borrower: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"market", borrower.as_ref(), &nonce.to_le_bytes()],
        &program_id(),
    )
}

pub fn get_market_authority_pda(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"market_authority", market.as_ref()], &program_id())
}

pub fn get_vault_pda(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"vault", market.as_ref()], &program_id())
}

pub fn get_lender_position_pda(market: &Pubkey, lender: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"lender", market.as_ref(), lender.as_ref()],
        &program_id(),
    )
}

pub fn get_borrower_whitelist_pda(borrower: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"borrower_whitelist", borrower.as_ref()], &program_id())
}

pub fn get_blacklist_pda(blacklist_program: &Pubkey, address: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"blacklist", address.as_ref()], blacklist_program)
}

pub fn get_haircut_state_pda(market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"haircut_state", market.as_ref()], &program_id())
}

// ---------------------------------------------------------------------------
// Instruction builders
// ---------------------------------------------------------------------------

/// InitializeProtocol (disc 0)
/// data = [0u8] ++ fee_rate_bps (2 bytes LE u16)
///
/// Accounts:
/// 0. protocol_config (writable) - PDA for protocol configuration
/// 1. admin (signer, writable) - Admin who pays for initialization
/// 2. fee_authority (readonly) - Address that can collect fees
/// 3. whitelist_manager (readonly) - Address that can manage borrower whitelist
/// 4. blacklist_program (readonly) - External program for blacklist checks
/// 5. system_program (readonly)
/// 6. program_data (readonly) - BPF upgradeable loader program data for upgrade authority verification
pub fn build_initialize_protocol(
    admin: &Pubkey,
    fee_authority: &Pubkey,
    whitelist_manager: &Pubkey,
    blacklist_program: &Pubkey,
    fee_rate_bps: u16,
) -> Instruction {
    let (protocol_config, _) = get_protocol_config_pda();

    // Derive the program_data PDA from the BPF upgradeable loader
    let (program_data, _) = Pubkey::find_program_address(
        &[program_id().as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );

    let mut data = vec![0u8]; // discriminator
    data.extend_from_slice(&fee_rate_bps.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false), // protocol_config PDA (writable)
            AccountMeta::new(*admin, true),           // admin (signer, writable — payer)
            AccountMeta::new_readonly(*fee_authority, false), // fee_authority
            AccountMeta::new_readonly(*whitelist_manager, false), // whitelist_manager
            AccountMeta::new_readonly(*blacklist_program, false), // blacklist_program
            AccountMeta::new_readonly(system_program::id(), false), // system_program
            AccountMeta::new_readonly(program_data, false), // program_data (upgrade authority verification)
        ],
        data,
    }
}

/// SetFeeConfig (disc 1)
/// data = [1u8] ++ new_fee_rate_bps (2 bytes LE u16)
pub fn build_set_fee_config(
    admin: &Pubkey,
    new_fee_authority: &Pubkey,
    new_fee_rate_bps: u16,
) -> Instruction {
    let (protocol_config, _) = get_protocol_config_pda();

    let mut data = vec![1u8]; // discriminator
    data.extend_from_slice(&new_fee_rate_bps.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(protocol_config, false), // protocol_config PDA (writable)
            AccountMeta::new_readonly(*admin, true),  // admin (signer)
            AccountMeta::new_readonly(*new_fee_authority, false), // new_fee_authority
        ],
        data,
    }
}

/// SetBorrowerWhitelist (disc 12)
/// data = [12u8] ++ is_whitelisted (1 byte) ++ max_borrow_capacity (8 bytes LE u64)
pub fn build_set_borrower_whitelist(
    whitelist_manager: &Pubkey,
    borrower: &Pubkey,
    is_whitelisted: u8,
    max_borrow_capacity: u64,
) -> Instruction {
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(borrower);
    let (protocol_config, _) = get_protocol_config_pda();

    let mut data = vec![12u8]; // discriminator
    data.push(is_whitelisted);
    data.extend_from_slice(&max_borrow_capacity.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(borrower_whitelist, false), // borrower_whitelist PDA (writable)
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new(*whitelist_manager, true), // whitelist_manager (signer, writable — payer)
            AccountMeta::new_readonly(*borrower, false), // borrower_address
            AccountMeta::new_readonly(system_program::id(), false), // system_program
        ],
        data,
    }
}

/// CreateMarket (disc 2)
/// data = [2u8] ++ nonce(8) ++ annual_interest_bps(2) ++ maturity_timestamp(8) ++ max_total_supply(8)
pub fn build_create_market(
    borrower: &Pubkey,
    mint: &Pubkey,
    blacklist_program_id: &Pubkey,
    nonce: u64,
    annual_interest_bps: u16,
    maturity_timestamp: i64,
    max_total_supply: u64,
) -> Instruction {
    let (market, _) = get_market_pda(borrower, nonce);
    let (vault, _) = get_vault_pda(&market);
    let (market_authority, _) = get_market_authority_pda(&market);
    let (protocol_config, _) = get_protocol_config_pda();
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(borrower);
    let (blacklist_check, _) = get_blacklist_pda(blacklist_program_id, borrower);

    let mut data = vec![2u8]; // discriminator
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(&annual_interest_bps.to_le_bytes());
    data.extend_from_slice(&maturity_timestamp.to_le_bytes());
    data.extend_from_slice(&max_total_supply.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(market, false),         // market PDA (writable)
            AccountMeta::new(*borrower, true),       // borrower (signer, writable — payer)
            AccountMeta::new_readonly(*mint, false), // mint
            AccountMeta::new(vault, false),          // vault PDA (writable)
            AccountMeta::new_readonly(market_authority, false), // market_authority PDA
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(borrower_whitelist, false), // borrower_whitelist PDA
            AccountMeta::new_readonly(blacklist_check, false), // blacklist_check PDA
            AccountMeta::new_readonly(system_program::id(), false), // system_program
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
            AccountMeta::new(get_haircut_state_pda(&market).0, false), // haircut_state_account
        ],
        data,
    }
}

/// Deposit (disc 3)
/// data = [3u8] ++ amount(8 LE u64)
/// Vault is derived from market automatically.
pub fn build_deposit(
    market: &Pubkey,
    lender: &Pubkey,
    lender_token_account: &Pubkey,
    mint: &Pubkey,
    blacklist_program_id: &Pubkey,
    amount: u64,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (lender_position, _) = get_lender_position_pda(market, lender);
    let (blacklist_check, _) = get_blacklist_pda(blacklist_program_id, lender);
    let (protocol_config, _) = get_protocol_config_pda();

    let mut data = vec![3u8]; // discriminator
    data.extend_from_slice(&amount.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),               // market (writable)
            AccountMeta::new(*lender, true),                // lender (signer, writable)
            AccountMeta::new(*lender_token_account, false), // lender_token_account (writable)
            AccountMeta::new(vault, false),                 // vault (writable)
            AccountMeta::new(lender_position, false),       // lender_position PDA (writable)
            AccountMeta::new_readonly(blacklist_check, false), // blacklist_check PDA
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(*mint, false),        // mint
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
            AccountMeta::new_readonly(system_program::id(), false), // system_program
        ],
        data,
    }
}

/// Borrow (disc 4)
/// data = [4u8] ++ amount(8 LE u64)
/// Vault is derived from market automatically.
pub fn build_borrow(
    market: &Pubkey,
    borrower: &Pubkey,
    borrower_token_account: &Pubkey,
    blacklist_program_id: &Pubkey,
    amount: u64,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (market_authority, _) = get_market_authority_pda(market);
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(borrower);
    let (blacklist_check, _) = get_blacklist_pda(blacklist_program_id, borrower);
    let (protocol_config, _) = get_protocol_config_pda();

    let mut data = vec![4u8]; // discriminator
    data.extend_from_slice(&amount.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),           // market (writable)
            AccountMeta::new_readonly(*borrower, true), // borrower (signer)
            AccountMeta::new(*borrower_token_account, false), // borrower_token_account (writable)
            AccountMeta::new(vault, false),             // vault (writable)
            AccountMeta::new_readonly(market_authority, false), // market_authority PDA
            AccountMeta::new(borrower_whitelist, false), // borrower_whitelist PDA (writable)
            AccountMeta::new_readonly(blacklist_check, false), // blacklist_check PDA
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
        ],
        data,
    }
}

/// Repay (disc 5)
/// data = [5u8] ++ amount(8 LE u64)
/// Vault is derived from market automatically.
///
/// UPDATED: Now includes borrower_whitelist account to decrement current_borrowed
/// on repay, enabling re-borrowing up to max_borrow_capacity.
/// NOTE: Use this for PRINCIPAL repayments. For interest-only payments, use build_repay_interest.
pub fn build_repay(
    market: &Pubkey,
    payer: &Pubkey,
    payer_token_account: &Pubkey,
    mint: &Pubkey,
    borrower: &Pubkey, // The market's borrower (for whitelist PDA derivation)
    amount: u64,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(borrower);

    let mut data = vec![5u8]; // discriminator
    data.extend_from_slice(&amount.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),              // market (writable)
            AccountMeta::new_readonly(*payer, true),       // payer (signer)
            AccountMeta::new(*payer_token_account, false), // payer_token_account (writable)
            AccountMeta::new(vault, false),                // vault (writable)
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(*mint, false),       // mint
            AccountMeta::new(borrower_whitelist, false),   // borrower_whitelist PDA (writable)
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
        ],
        data,
    }
}

/// RepayInterest (disc 6)
/// data = [6u8] ++ amount(8 LE u64)
/// Vault is derived from market automatically.
///
/// Use this for INTEREST-ONLY repayments. Unlike regular repay, this does NOT
/// decrement the borrower's current_borrowed, so it doesn't free up borrowing capacity.
/// This prevents the exploit where interest payments incorrectly increase available capacity.
pub fn build_repay_interest(
    market: &Pubkey,
    payer: &Pubkey,
    payer_token_account: &Pubkey,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();

    // Note: instruction data is just the discriminator + amount
    // Amount will be appended by caller or we can add a parameter
    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),              // market (writable)
            AccountMeta::new_readonly(*payer, true),       // payer (signer)
            AccountMeta::new(*payer_token_account, false), // payer_token_account (writable)
            AccountMeta::new(vault, false),                // vault (writable)
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
        ],
        data: vec![6u8], // discriminator only - amount must be appended
    }
}

/// RepayInterest (disc 6) with amount
/// Convenience function that includes the amount in the instruction data.
pub fn build_repay_interest_with_amount(
    market: &Pubkey,
    payer: &Pubkey,
    payer_token_account: &Pubkey,
    amount: u64,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();

    let mut data = vec![6u8]; // discriminator
    data.extend_from_slice(&amount.to_le_bytes());

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),              // market (writable)
            AccountMeta::new_readonly(*payer, true),       // payer (signer)
            AccountMeta::new(*payer_token_account, false), // payer_token_account (writable)
            AccountMeta::new(vault, false),                // vault (writable)
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
        ],
        data,
    }
}

/// Withdraw (disc 7)
/// data = [7u8] ++ scaled_amount(16 LE u128) ++ min_payout(8 LE u64)
/// Vault is derived from market automatically.
///
/// UPDATED: Now includes min_payout for slippage protection (SR-111)
/// Set min_payout to 0 to disable slippage protection.
pub fn build_withdraw(
    market: &Pubkey,
    lender: &Pubkey,
    lender_token_account: &Pubkey,
    blacklist_program_id: &Pubkey,
    scaled_amount: u128,
    min_payout: u64,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (lender_position, _) = get_lender_position_pda(market, lender);
    let (market_authority, _) = get_market_authority_pda(market);
    let (blacklist_check, _) = get_blacklist_pda(blacklist_program_id, lender);
    let (protocol_config, _) = get_protocol_config_pda();

    let mut data = vec![7u8]; // discriminator
    data.extend_from_slice(&scaled_amount.to_le_bytes());
    data.extend_from_slice(&min_payout.to_le_bytes()); // NEW: slippage protection

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),               // market (writable)
            AccountMeta::new_readonly(*lender, true),       // lender (signer)
            AccountMeta::new(*lender_token_account, false), // lender_token_account (writable)
            AccountMeta::new(vault, false),                 // vault (writable)
            AccountMeta::new(lender_position, false),       // lender_position PDA (writable)
            AccountMeta::new_readonly(market_authority, false), // market_authority PDA
            AccountMeta::new_readonly(blacklist_check, false), // blacklist_check PDA
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
            AccountMeta::new(get_haircut_state_pda(market).0, false), // haircut_state_account
        ],
        data,
    }
}

/// CollectFees (disc 8)
/// data = [8u8] (no additional data)
/// Vault is derived from market automatically.
pub fn build_collect_fees(
    market: &Pubkey,
    fee_authority: &Pubkey,
    fee_destination: &Pubkey,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();
    let (market_authority, _) = get_market_authority_pda(market);

    let data = vec![8u8]; // discriminator only

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false), // market (writable)
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(*fee_authority, true), // fee_authority (signer)
            AccountMeta::new(*fee_destination, false), // fee_destination (writable — token account)
            AccountMeta::new(vault, false),   // vault (writable)
            AccountMeta::new_readonly(market_authority, false), // market_authority PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
        ],
        data,
    }
}

/// CloseLenderPosition (disc 10)
/// data = [10u8] (no additional data)
pub fn build_close_lender_position(market: &Pubkey, lender: &Pubkey) -> Instruction {
    let (lender_position, _) = get_lender_position_pda(market, lender);
    let (protocol_config, _) = get_protocol_config_pda();

    let data = vec![10u8]; // discriminator only

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*market, false), // market (read-only)
            AccountMeta::new(*lender, true),           // lender (signer, writable)
            AccountMeta::new(lender_position, false),  // lender_position PDA (writable)
            AccountMeta::new_readonly(system_program::id(), false), // system_program
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
        ],
        data,
    }
}

/// ReSettle (disc 9)
/// data = [9u8] (no additional data)
///
/// UPDATED: Now requires protocol_config account for proper fee accrual (SR-109)
pub fn build_re_settle(market: &Pubkey, vault: &Pubkey) -> Instruction {
    let (protocol_config, _) = get_protocol_config_pda();
    let data = vec![9u8]; // discriminator only

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),         // market (writable)
            AccountMeta::new_readonly(*vault, false), // vault (read-only)
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA (NEW)
            AccountMeta::new_readonly(get_haircut_state_pda(market).0, false), // haircut_state_account
        ],
        data,
    }
}

/// ForceClosePosition (disc 18)
/// data = [18u8] (no additional data)
///
/// Borrower force-closes a lender position after maturity + grace period.
pub fn build_force_close_position(
    market: &Pubkey,
    borrower: &Pubkey,
    lender: &Pubkey,
    escrow_token_account: &Pubkey,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (market_authority, _) = get_market_authority_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();
    let (lender_position, _) = get_lender_position_pda(market, lender);

    let data = vec![18u8]; // discriminator only

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),               // market (writable)
            AccountMeta::new_readonly(*borrower, true),     // borrower (signer)
            AccountMeta::new(lender_position, false),       // lender_position (writable)
            AccountMeta::new(vault, false),                 // vault (writable)
            AccountMeta::new(*escrow_token_account, false), // escrow_token_account (writable)
            AccountMeta::new_readonly(market_authority, false), // market_authority PDA
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
            AccountMeta::new(get_haircut_state_pda(market).0, false), // haircut_state_account
        ],
        data,
    }
}

/// WithdrawExcess (disc 11)
/// data = [11u8] (no additional data)
///
/// Allows borrower to withdraw excess funds after full settlement.
pub fn build_withdraw_excess(
    market: &Pubkey,
    borrower: &Pubkey,
    borrower_token_account: &Pubkey,
    blacklist_program_id: &Pubkey,
) -> Instruction {
    let (vault, _) = get_vault_pda(market);
    let (market_authority, _) = get_market_authority_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();
    let (blacklist_check, _) = get_blacklist_pda(blacklist_program_id, borrower);
    let (borrower_whitelist, _) = get_borrower_whitelist_pda(borrower);

    let data = vec![11u8]; // discriminator only

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*market, false), // market (read-only)
            AccountMeta::new_readonly(*borrower, true), // borrower (signer)
            AccountMeta::new(*borrower_token_account, false), // borrower_token_account (writable)
            AccountMeta::new(vault, false),            // vault (writable)
            AccountMeta::new_readonly(market_authority, false), // market_authority PDA
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
            AccountMeta::new_readonly(protocol_config, false), // protocol_config PDA
            AccountMeta::new_readonly(blacklist_check, false), // blacklist_check
            AccountMeta::new_readonly(borrower_whitelist, false), // borrower_whitelist
        ],
        data,
    }
}

/// ClaimHaircut (disc 19)
/// data = [19u8] (no additional data)
pub fn build_claim_haircut(
    market: &Pubkey,
    lender: &Pubkey,
    lender_token_account: &Pubkey,
) -> Instruction {
    let (vault_pda, _) = get_vault_pda(market);
    let (market_authority, _) = get_market_authority_pda(market);
    let (haircut_state, _) = get_haircut_state_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();
    let (lender_position, _) = get_lender_position_pda(market, lender);

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*lender, true),
            AccountMeta::new(lender_position, false),
            AccountMeta::new(*lender_token_account, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new_readonly(market_authority, false),
            AccountMeta::new(haircut_state, false),
            AccountMeta::new_readonly(protocol_config, false),
            AccountMeta::new_readonly(spl_token::id(), false),
        ],
        data: vec![19u8],
    }
}

/// ForceClaimHaircut (disc 20)
/// data = [20u8] (no additional data)
pub fn build_force_claim_haircut(
    market: &Pubkey,
    borrower: &Pubkey,
    lender_position: &Pubkey,
    escrow_token_account: &Pubkey,
) -> Instruction {
    let (vault_pda, _) = get_vault_pda(market);
    let (market_authority, _) = get_market_authority_pda(market);
    let (haircut_state, _) = get_haircut_state_pda(market);
    let (protocol_config, _) = get_protocol_config_pda();

    Instruction {
        program_id: program_id(),
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*borrower, true),
            AccountMeta::new(*lender_position, false),
            AccountMeta::new(*escrow_token_account, false),
            AccountMeta::new(vault_pda, false),
            AccountMeta::new_readonly(market_authority, false),
            AccountMeta::new(haircut_state, false),
            AccountMeta::new_readonly(protocol_config, false),
            AccountMeta::new_readonly(spl_token::id(), false),
        ],
        data: vec![20u8],
    }
}

// ---------------------------------------------------------------------------
// SPL Token helper functions
// ---------------------------------------------------------------------------

/// Create a new SPL Token mint with the given authority and decimals.
/// Returns the mint Pubkey.
/// USDC mint address on mainnet (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v).
/// COAL-L01: The program enforces this as the only accepted mint.
pub const USDC_MINT_PUBKEY: Pubkey =
    solana_sdk::pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

/// Create an SPL Token mint at the hardcoded USDC address by injecting the
/// account directly into the test context. Returns the USDC mint pubkey.
pub async fn create_mint(
    ctx: &mut ProgramTestContext,
    authority: &Keypair,
    decimals: u8,
) -> Pubkey {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let mint_rent = rent.minimum_balance(spl_token::state::Mint::LEN);

    // Build SPL Token mint account data (82 bytes) as raw bytes.
    // Layout: [4 COption tag][32 mint_authority][8 supply][1 decimals][1 is_initialized][4 COption tag][32 freeze_authority]
    let mut data = vec![0u8; spl_token::state::Mint::LEN];
    // mint_authority = Some(authority)
    data[0..4].copy_from_slice(&1u32.to_le_bytes()); // COption::Some
    data[4..36].copy_from_slice(&authority.pubkey().to_bytes());
    // supply = 0 (already zeroed)
    // decimals
    data[44] = decimals;
    // is_initialized = true
    data[45] = 1;
    // freeze_authority = None (already zeroed)

    let mut account =
        AccountSharedData::new(mint_rent, spl_token::state::Mint::LEN, &spl_token::id());
    account.set_data_from_slice(&data);
    ctx.set_account(&USDC_MINT_PUBKEY, &account);

    USDC_MINT_PUBKEY
}

/// Create a new SPL Token mint with a random address (not USDC).
/// Used for negative tests that verify non-USDC mints are rejected.
pub async fn create_random_mint(
    ctx: &mut ProgramTestContext,
    authority: &Keypair,
    decimals: u8,
) -> Pubkey {
    let mint = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let mint_rent = rent.minimum_balance(spl_token::state::Mint::LEN);

    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::system_instruction::create_account(
                &ctx.payer.pubkey(),
                &mint.pubkey(),
                mint_rent,
                spl_token::state::Mint::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_mint(
                &spl_token::id(),
                &mint.pubkey(),
                &authority.pubkey(),
                None,
                decimals,
            )
            .unwrap(),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &mint],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    mint.pubkey()
}

/// Create a new SPL Token account for the given mint and owner.
/// Returns the token account Keypair.
pub async fn create_token_account(
    ctx: &mut ProgramTestContext,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Keypair {
    let account = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account_rent = rent.minimum_balance(spl_token::state::Account::LEN);

    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::system_instruction::create_account(
                &ctx.payer.pubkey(),
                &account.pubkey(),
                account_rent,
                spl_token::state::Account::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_account(
                &spl_token::id(),
                &account.pubkey(),
                mint,
                owner,
            )
            .unwrap(),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &account],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    account
}

/// Mint tokens to a destination token account.
pub async fn mint_to_account(
    ctx: &mut ProgramTestContext,
    mint: &Pubkey,
    dest: &Pubkey,
    authority: &Keypair,
    amount: u64,
) {
    let tx = Transaction::new_signed_with_payer(
        &[spl_token::instruction::mint_to(
            &spl_token::id(),
            mint,
            dest,
            &authority.pubkey(),
            &[],
            amount,
        )
        .unwrap()],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, authority],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

/// Get the token balance of a token account.
pub async fn get_token_balance(ctx: &mut ProgramTestContext, account: &Pubkey) -> u64 {
    let account_data = ctx
        .banks_client
        .get_account(*account)
        .await
        .unwrap()
        .unwrap();

    let token_account = spl_token::state::Account::unpack(&account_data.data).unwrap();
    token_account.amount
}

// ---------------------------------------------------------------------------
// High-level setup helpers
// ---------------------------------------------------------------------------

/// Initialize the protocol. Sends the InitializeProtocol instruction.
///
/// This function also injects a fake program_data account to satisfy the
/// upgrade authority verification in the processor.
pub async fn setup_protocol(
    ctx: &mut ProgramTestContext,
    admin: &Keypair,
    fee_authority: &Pubkey,
    whitelist_manager: &Pubkey,
    blacklist_program: &Pubkey,
    fee_rate_bps: u16,
) {
    // Inject fake program_data account with admin as upgrade authority
    // The program_data PDA is derived from [program_id] with BPF Upgradeable Loader
    let (program_data_pda, _) = Pubkey::find_program_address(
        &[program_id().as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );

    // Create program_data account structure:
    // [4 bytes type = 3 (ProgramData)][8 bytes slot][1 byte option = 1][32 bytes authority]
    let mut program_data = vec![0u8; 45];
    program_data[0..4].copy_from_slice(&3u32.to_le_bytes()); // type = ProgramData
    program_data[4..12].copy_from_slice(&0u64.to_le_bytes()); // slot
    program_data[12] = 1; // option = Some
    program_data[13..45].copy_from_slice(admin.pubkey().as_ref()); // upgrade authority

    let mut program_data_account = AccountSharedData::new(
        1_000_000_000,
        program_data.len(),
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    program_data_account
        .data_as_mut_slice()
        .copy_from_slice(&program_data);
    ctx.set_account(&program_data_pda, &program_data_account);

    let ix = build_initialize_protocol(
        &admin.pubkey(),
        fee_authority,
        whitelist_manager,
        blacklist_program,
        fee_rate_bps,
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, admin],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

/// Full market setup: whitelist the borrower, then create the market.
/// Returns the market Pubkey.
pub async fn setup_market_full(
    ctx: &mut ProgramTestContext,
    admin: &Keypair,
    borrower: &Keypair,
    mint: &Pubkey,
    blacklist_program: &Pubkey,
    nonce: u64,
    annual_interest_bps: u16,
    maturity_timestamp: i64,
    max_total_supply: u64,
    whitelist_manager: &Keypair,
    max_borrow_capacity: u64,
) -> Pubkey {
    // Step 1: Whitelist the borrower
    let wl_ix = build_set_borrower_whitelist(
        &whitelist_manager.pubkey(),
        &borrower.pubkey(),
        1, // is_whitelisted = true
        max_borrow_capacity,
    );

    let wl_tx = Transaction::new_signed_with_payer(
        &[wl_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, whitelist_manager],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(wl_tx).await.unwrap();

    // Need a fresh blockhash for the next transaction
    let recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Step 2: Create the market
    let create_ix = build_create_market(
        &borrower.pubkey(),
        mint,
        blacklist_program,
        nonce,
        annual_interest_bps,
        maturity_timestamp,
        max_total_supply,
    );

    let create_tx = Transaction::new_signed_with_payer(
        &[create_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, borrower],
        recent_blockhash,
    );
    ctx.banks_client
        .process_transaction(create_tx)
        .await
        .unwrap();

    let (market, _) = get_market_pda(&borrower.pubkey(), nonce);
    market
}

// ---------------------------------------------------------------------------
// Account data reader
// ---------------------------------------------------------------------------

/// Read the raw data bytes for a given account.
pub async fn get_account_data(ctx: &mut ProgramTestContext, pubkey: &Pubkey) -> Vec<u8> {
    let account = ctx
        .banks_client
        .get_account(*pubkey)
        .await
        .unwrap()
        .expect("account not found");
    account.data
}

// ---------------------------------------------------------------------------
// Error extraction helper
// ---------------------------------------------------------------------------

/// Extract a `Custom(N)` error code from a `BanksClientError`, if present.
pub fn extract_custom_error(err: &BanksClientError) -> Option<u32> {
    match err {
        BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(
                _,
                solana_sdk::instruction::InstructionError::Custom(code),
            ),
        ) => Some(*code),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Clock manipulation helper
// ---------------------------------------------------------------------------

/// Override the Clock sysvar's `unix_timestamp` to `target_timestamp`.
///
/// The on-chain processors read unix_timestamp via the `sol_get_clock_sysvar`
/// syscall, which reads from the bank's sysvar cache — NOT from account data.
/// `set_sysvar` updates that cache directly, so the BPF runtime sees the new
/// timestamp on the very next instruction.
pub async fn advance_clock_past(ctx: &mut ProgramTestContext, target_timestamp: i64) {
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = target_timestamp;
    ctx.set_sysvar(&clock);
}

/// Get a fresh blockhash and re-pin the clock to `target_timestamp`.
///
/// `get_latest_blockhash()` creates a new slot/bank which derives its clock
/// from the parent bank's slot time + ~400ms.  This means our `set_sysvar`
/// override gets overwritten.  This helper fetches the blockhash first (which
/// creates the new bank), then immediately re-sets the clock on the new bank.
///
/// Use this in tests that need precise clock control across multiple
/// transactions — especially timing-boundary tests (e.g., "deposit one
/// second before maturity").
pub async fn get_blockhash_pinned(ctx: &mut ProgramTestContext, target_timestamp: i64) {
    // Get a fresh blockhash (creates a new bank/slot), then override the clock
    // sysvar on that bank to our target timestamp.  The `set_sysvar` writes
    // into the working bank's sysvar cache so the BPF runtime sees the
    // overridden timestamp when the next transaction is processed.
    ctx.last_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = target_timestamp;
    ctx.set_sysvar(&clock);
}

// ---------------------------------------------------------------------------
// Blacklist setup helper
// ---------------------------------------------------------------------------

/// Inject a fake program_data account for upgrade authority verification.
/// This is needed for tests that call build_initialize_protocol directly
/// instead of using setup_protocol.
pub fn setup_program_data_account(ctx: &mut ProgramTestContext, admin: &Pubkey) {
    let (program_data_pda, _) = Pubkey::find_program_address(
        &[program_id().as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    let mut program_data = vec![0u8; 45];
    program_data[0..4].copy_from_slice(&3u32.to_le_bytes()); // type = ProgramData
    program_data[4..12].copy_from_slice(&0u64.to_le_bytes()); // slot
    program_data[12] = 1; // option = Some
    program_data[13..45].copy_from_slice(admin.as_ref()); // upgrade authority
    let mut program_data_account = AccountSharedData::new(
        1_000_000_000,
        program_data.len(),
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    program_data_account
        .data_as_mut_slice()
        .copy_from_slice(&program_data);
    ctx.set_account(&program_data_pda, &program_data_account);
}

/// Inject a fake blacklist PDA account via `ctx.set_account()`.
/// - `blacklist_program`: the external program that "owns" the blacklist PDA.
/// - `address`: the user address being blacklisted.
/// - `status_byte`: 1 = blacklisted, 0 = not blacklisted.
pub fn setup_blacklist_account(
    ctx: &mut ProgramTestContext,
    blacklist_program: &Pubkey,
    address: &Pubkey,
    status_byte: u8,
) {
    let (blacklist_pda, _) = get_blacklist_pda(blacklist_program, address);
    let mut account = AccountSharedData::new(1_000_000_000, 1, blacklist_program);
    account.data_as_mut_slice()[0] = status_byte;
    ctx.set_account(&blacklist_pda, &account);
}

/// Inject a blacklist PDA with specific raw data and owner.
/// Useful for testing wrong-owner and empty-data edge cases.
pub fn setup_blacklist_account_raw(
    ctx: &mut ProgramTestContext,
    blacklist_program: &Pubkey,
    address: &Pubkey,
    data: &[u8],
    owner: &Pubkey,
) {
    let (blacklist_pda, _) = get_blacklist_pda(blacklist_program, address);
    let mut account = AccountSharedData::new(1_000_000_000, data.len(), owner);
    account.data_as_mut_slice().copy_from_slice(data);
    ctx.set_account(&blacklist_pda, &account);
}

// ---------------------------------------------------------------------------
// Parse helpers — structured field extraction from raw account bytes
// ---------------------------------------------------------------------------

/// Parsed Market fields (from 250-byte account data).
pub struct ParsedMarket {
    pub borrower: [u8; 32],
    pub mint: [u8; 32],
    pub vault: [u8; 32],
    pub market_authority_bump: u8,
    pub annual_interest_bps: u16,
    pub maturity_timestamp: i64,
    pub max_total_supply: u64,
    pub market_nonce: u64,
    pub scaled_total_supply: u128,
    pub scale_factor: u128,
    pub accrued_protocol_fees: u64,
    pub total_deposited: u64,
    pub total_borrowed: u64,
    pub total_repaid: u64,
    pub total_interest_repaid: u64,
    pub last_accrual_timestamp: i64,
    pub settlement_factor_wad: u128,
    pub bump: u8,
    pub haircut_accumulator: u64,
}

pub fn parse_market(data: &[u8]) -> ParsedMarket {
    assert!(data.len() >= MARKET_SIZE, "Market data too short");
    // Validate discriminator before parsing
    assert_eq!(
        &data[0..8],
        &DISC_MARKET,
        "Market discriminator mismatch: expected {:?}, got {:?}",
        DISC_MARKET,
        &data[0..8]
    );
    // Skip the 9-byte prefix: discriminator (8 bytes) + version (1 byte)
    let offset = 9;
    let d = &data[offset..];
    let mut borrower = [0u8; 32];
    borrower.copy_from_slice(&d[0..32]);
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&d[32..64]);
    let mut vault = [0u8; 32];
    vault.copy_from_slice(&d[64..96]);
    ParsedMarket {
        borrower,
        mint,
        vault,
        market_authority_bump: d[96],
        annual_interest_bps: u16::from_le_bytes(d[97..99].try_into().unwrap()),
        maturity_timestamp: i64::from_le_bytes(d[99..107].try_into().unwrap()),
        max_total_supply: u64::from_le_bytes(d[107..115].try_into().unwrap()),
        market_nonce: u64::from_le_bytes(d[115..123].try_into().unwrap()),
        scaled_total_supply: u128::from_le_bytes(d[123..139].try_into().unwrap()),
        scale_factor: u128::from_le_bytes(d[139..155].try_into().unwrap()),
        accrued_protocol_fees: u64::from_le_bytes(d[155..163].try_into().unwrap()),
        total_deposited: u64::from_le_bytes(d[163..171].try_into().unwrap()),
        total_borrowed: u64::from_le_bytes(d[171..179].try_into().unwrap()),
        total_repaid: u64::from_le_bytes(d[179..187].try_into().unwrap()),
        total_interest_repaid: u64::from_le_bytes(d[187..195].try_into().unwrap()),
        last_accrual_timestamp: i64::from_le_bytes(d[195..203].try_into().unwrap()),
        settlement_factor_wad: u128::from_le_bytes(d[203..219].try_into().unwrap()),
        bump: d[219],
        haircut_accumulator: u64::from_le_bytes(d[220..228].try_into().unwrap()),
    }
}

/// Parsed LenderPosition fields (from 128-byte account data).
pub struct ParsedLenderPosition {
    pub market: [u8; 32],
    pub lender: [u8; 32],
    pub scaled_balance: u128,
    pub bump: u8,
    pub haircut_owed: u64,
    pub withdrawal_sf: u128,
}

pub fn parse_lender_position(data: &[u8]) -> ParsedLenderPosition {
    assert!(
        data.len() >= LENDER_POSITION_SIZE,
        "LenderPosition data too short"
    );
    // Validate discriminator before parsing
    assert_eq!(
        &data[0..8],
        &DISC_LENDER_POSITION,
        "LenderPosition discriminator mismatch: expected {:?}, got {:?}",
        DISC_LENDER_POSITION,
        &data[0..8]
    );
    // Skip the 9-byte prefix: discriminator (8 bytes) + version (1 byte)
    let offset = 9;
    let d = &data[offset..];
    let mut market = [0u8; 32];
    market.copy_from_slice(&d[0..32]);
    let mut lender = [0u8; 32];
    lender.copy_from_slice(&d[32..64]);
    // haircut_owed is at struct offset 90, minus 9-byte prefix = d[81..89]
    let haircut_owed = u64::from_le_bytes(d[81..89].try_into().unwrap());
    // withdrawal_sf is at struct offset 98, minus 9-byte prefix = d[89..105]
    let withdrawal_sf = u128::from_le_bytes(d[89..105].try_into().unwrap());
    ParsedLenderPosition {
        market,
        lender,
        scaled_balance: u128::from_le_bytes(d[64..80].try_into().unwrap()),
        bump: d[80],
        haircut_owed,
        withdrawal_sf,
    }
}

/// Parsed HaircutState fields (from 88-byte account data).
pub struct ParsedHaircutState {
    pub claim_weight_sum: u128,
    pub claim_offset_sum: u128,
}

pub fn parse_haircut_state(data: &[u8]) -> ParsedHaircutState {
    assert!(
        data.len() >= HAIRCUT_STATE_SIZE,
        "HaircutState data too short"
    );
    assert_eq!(
        &data[0..8],
        &DISC_HAIRCUT_STATE,
        "HaircutState discriminator mismatch"
    );
    // Skip 9-byte prefix: discriminator (8) + version (1)
    // market at offset 9 (32 bytes), claim_weight_sum at offset 41 (16 bytes),
    // claim_offset_sum at offset 57 (16 bytes)
    ParsedHaircutState {
        claim_weight_sum: u128::from_le_bytes(data[41..57].try_into().unwrap()),
        claim_offset_sum: u128::from_le_bytes(data[57..73].try_into().unwrap()),
    }
}

/// Parsed ProtocolConfig fields (from 194-byte account data).
pub struct ParsedProtocolConfig {
    pub admin: [u8; 32],
    pub fee_rate_bps: u16,
    pub fee_authority: [u8; 32],
    pub whitelist_manager: [u8; 32],
    pub blacklist_program: [u8; 32],
    pub is_initialized: u8,
    pub bump: u8,
}

pub fn parse_protocol_config(data: &[u8]) -> ParsedProtocolConfig {
    assert!(
        data.len() >= PROTOCOL_CONFIG_SIZE,
        "ProtocolConfig data too short"
    );
    // Validate discriminator before parsing
    assert_eq!(
        &data[0..8],
        &DISC_PROTOCOL_CONFIG,
        "ProtocolConfig discriminator mismatch: expected {:?}, got {:?}",
        DISC_PROTOCOL_CONFIG,
        &data[0..8]
    );
    // Skip the 9-byte prefix: discriminator (8 bytes) + version (1 byte)
    let offset = 9;
    let d = &data[offset..];
    let mut admin = [0u8; 32];
    admin.copy_from_slice(&d[0..32]);
    let mut fee_authority = [0u8; 32];
    fee_authority.copy_from_slice(&d[34..66]);
    let mut whitelist_manager = [0u8; 32];
    whitelist_manager.copy_from_slice(&d[66..98]);
    let mut blacklist_program = [0u8; 32];
    blacklist_program.copy_from_slice(&d[98..130]);
    ParsedProtocolConfig {
        admin,
        fee_rate_bps: u16::from_le_bytes(d[32..34].try_into().unwrap()),
        fee_authority,
        whitelist_manager,
        blacklist_program,
        is_initialized: d[130],
        bump: d[131],
    }
}

/// Parsed BorrowerWhitelist fields (from 96-byte account data).
///
/// Layout (after 9-byte prefix):
/// - borrower: [u8; 32]           (0-31)
/// - is_whitelisted: u8           (32)
/// - max_borrow_capacity: u64     (33-40)
/// - current_borrowed: u64        (41-48)
/// - bump: u8                     (49)
/// - _padding: [u8; 37]           (50-86)
pub struct ParsedBorrowerWhitelist {
    pub borrower: [u8; 32],
    pub is_whitelisted: u8,
    pub max_borrow_capacity: u64,
    pub current_borrowed: u64,
    pub bump: u8,
}

pub fn parse_borrower_whitelist(data: &[u8]) -> ParsedBorrowerWhitelist {
    assert!(
        data.len() >= BORROWER_WHITELIST_SIZE,
        "BorrowerWhitelist data too short"
    );
    // Validate discriminator before parsing
    assert_eq!(
        &data[0..8],
        &DISC_BORROWER_WL,
        "BorrowerWhitelist discriminator mismatch: expected {:?}, got {:?}",
        DISC_BORROWER_WL,
        &data[0..8]
    );
    // Skip the 9-byte prefix: discriminator (8 bytes) + version (1 byte)
    let offset = 9;
    let d = &data[offset..];
    let mut borrower = [0u8; 32];
    borrower.copy_from_slice(&d[0..32]);
    ParsedBorrowerWhitelist {
        borrower,
        is_whitelisted: d[32],
        max_borrow_capacity: u64::from_le_bytes(d[33..41].try_into().unwrap()),
        current_borrowed: u64::from_le_bytes(d[41..49].try_into().unwrap()),
        bump: d[49],
    }
}

// ---------------------------------------------------------------------------
// Common assertion helper
// ---------------------------------------------------------------------------

/// Assert that a transaction result contains a specific custom error code.
pub fn assert_custom_error(
    result: &Result<(), solana_sdk::transaction::TransactionError>,
    expected_code: u32,
) {
    match result {
        Err(solana_sdk::transaction::TransactionError::InstructionError(
            _,
            solana_sdk::instruction::InstructionError::Custom(code),
        )) => {
            assert_eq!(
                *code, expected_code,
                "expected Custom({expected_code}), got Custom({code})"
            );
        },
        Err(other) => panic!("expected Custom({expected_code}), got {other:?}"),
        Ok(()) => panic!("expected Custom({expected_code}), but transaction succeeded"),
    }
}

// ---------------------------------------------------------------------------
// Transaction helpers
// ---------------------------------------------------------------------------

/// Send an instruction and assert it succeeds.
pub async fn send_ok(
    ctx: &mut ProgramTestContext,
    ix: solana_sdk::instruction::Instruction,
    signers: &[&Keypair],
) {
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &all, bh);
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

/// Send an instruction and assert it fails with a specific custom error code.
pub async fn send_expect_error(
    ctx: &mut ProgramTestContext,
    ix: solana_sdk::instruction::Instruction,
    signers: &[&Keypair],
    expected: u32,
) {
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &all, bh);
    let result = ctx.banks_client.process_transaction(tx).await;
    match result {
        Err(err) => {
            let code = extract_custom_error(&err)
                .unwrap_or_else(|| panic!("expected Custom({expected}), got {err:?}"));
            assert_eq!(
                code, expected,
                "expected Custom({expected}), got Custom({code})"
            );
        },
        Ok(()) => panic!("expected Custom({expected}), but transaction succeeded"),
    }
}

/// Send an instruction expecting a specific error, WITHOUT fetching a new
/// blockhash.  A ComputeBudget instruction is prepended to guarantee a
/// distinct transaction signature (prevents deduplication when the instruction
/// and signers match a prior transaction on the same blockhash).
///
/// Use after a successful transaction whose state the error path depends on.
pub async fn send_expect_error_same_bank(
    ctx: &mut ProgramTestContext,
    ix: solana_sdk::instruction::Instruction,
    signers: &[&Keypair],
    expected: u32,
) {
    let budget_ix =
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(
        &[budget_ix, ix],
        Some(&ctx.payer.pubkey()),
        &all,
        ctx.last_blockhash,
    );
    let err = ctx
        .banks_client
        .process_transaction(tx)
        .await
        .expect_err(&format!(
            "expected Custom({expected}), but transaction succeeded"
        ));
    let code = extract_custom_error(&err)
        .unwrap_or_else(|| panic!("expected Custom({expected}), got {err:?}"));
    assert_eq!(
        code, expected,
        "expected Custom({expected}), got Custom({code})"
    );
}

/// Airdrop SOL to multiple keypairs.
pub async fn airdrop_multiple(ctx: &mut ProgramTestContext, keypairs: &[&Keypair], amount: u64) {
    for kp in keypairs {
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &kp.pubkey(),
                amount,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            ctx.last_blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }
}

/// SetPause (disc 13)
/// data = [13u8] ++ paused (1 byte, 0 = unpause, 1 = pause)
pub fn build_set_pause(admin: &Pubkey, paused: bool) -> Instruction {
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
pub fn build_set_blacklist_mode(admin: &Pubkey, fail_closed: bool) -> Instruction {
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
pub fn build_set_admin(current_admin: &Pubkey, new_admin: &Pubkey) -> Instruction {
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
pub fn build_set_whitelist_manager(admin: &Pubkey, new_manager: &Pubkey) -> Instruction {
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

/// Build a raw instruction with a specific discriminator and no additional data.
/// Useful for testing invalid discriminators.
pub fn build_raw_instruction(disc: u8, accounts: Vec<AccountMeta>) -> Instruction {
    Instruction {
        program_id: program_id(),
        accounts,
        data: vec![disc],
    }
}

// ---------------------------------------------------------------------------
// Account data reader (fallible)
// ---------------------------------------------------------------------------

/// Try to read the raw data bytes for a given account. Returns None if account
/// does not exist.
pub async fn try_get_account_data(
    ctx: &mut ProgramTestContext,
    pubkey: &Pubkey,
) -> Option<Vec<u8>> {
    ctx.banks_client
        .get_account(*pubkey)
        .await
        .unwrap()
        .map(|a| a.data)
}

// ---------------------------------------------------------------------------
// State snapshot for atomicity verification (P1-1)
// ---------------------------------------------------------------------------

/// Snapshot of all security-relevant state for verifying that failed
/// transactions don't mutate any on-chain accounts.
#[derive(Debug)]
pub struct ProtocolSnapshot {
    pub vault_balance: u64,
    pub market_data: Vec<u8>,
    pub lender_positions: Vec<(Pubkey, Vec<u8>)>,
    pub protocol_config_data: Vec<u8>,
}

impl ProtocolSnapshot {
    /// Capture current state of market, vault, protocol config, and lender positions.
    pub async fn capture(
        ctx: &mut ProgramTestContext,
        market: &Pubkey,
        vault: &Pubkey,
        lender_positions: &[Pubkey],
    ) -> Self {
        let vault_balance = get_token_balance(ctx, vault).await;
        let market_data = get_account_data(ctx, market).await;
        let (protocol_config, _) = get_protocol_config_pda();
        let protocol_config_data = get_account_data(ctx, &protocol_config).await;

        let mut positions = Vec::new();
        for pos in lender_positions {
            if let Some(data) = try_get_account_data(ctx, pos).await {
                positions.push((*pos, data));
            }
        }

        Self {
            vault_balance,
            market_data,
            lender_positions: positions,
            protocol_config_data,
        }
    }

    /// Assert ALL state is unchanged (failed transaction was atomic).
    pub fn assert_unchanged(&self, after: &Self) {
        assert_eq!(
            self.vault_balance, after.vault_balance,
            "Vault balance changed on failed tx"
        );
        assert_eq!(
            self.market_data, after.market_data,
            "Market data changed on failed tx"
        );
        assert_eq!(
            self.protocol_config_data, after.protocol_config_data,
            "Protocol config changed on failed tx"
        );
        assert_eq!(
            self.lender_positions.len(),
            after.lender_positions.len(),
            "Lender position count changed on failed tx"
        );
        for (a, b) in self
            .lender_positions
            .iter()
            .zip(after.lender_positions.iter())
        {
            assert_eq!(a.0, b.0, "Lender position key mismatch");
            assert_eq!(
                a.1, b.1,
                "Lender position {:?} data changed on failed tx",
                a.0
            );
        }
    }
}
