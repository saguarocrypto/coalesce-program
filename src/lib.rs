//! # CoalesceFi: Fixed-Rate, Fixed-Term Unsecured Lending Protocol
//!
//! ## Overview
//!
//! CoalesceFi is a credit-based lending protocol for Solana where:
//! - **Whitelisted borrowers** create markets and borrow without collateral
//! - **Lenders** deposit funds and earn fixed interest rates
//! - **Settlement** at maturity distributes funds pro-rata based on vault balance
//!
//! ## Architecture
//!
//! The protocol uses four account types (see `state/` module):
//! - `ProtocolConfig`: Global settings, admin keys, fee configuration
//! - `Market`: Individual lending market with maturity date and interest rate
//! - `LenderPosition`: Tracks a lender's scaled balance in a market
//! - `BorrowerWhitelist`: Per-borrower whitelist status and capacity
//!
//! ## Instruction Categories
//!
//! | Range | Category       | Instructions                                                    |
//! |-------|----------------|-----------------------------------------------------------------|
//! | 0-2   | Admin/Setup    | initialize_protocol, set_fee_config, create_market              |
//! | 3-7   | Core Lending   | deposit, borrow, repay, repay_interest, withdraw                |
//! | 8-11  | Settlement     | collect_fees, re_settle, close_lender_position, withdraw_excess |
//! | 12-16 | Access Control | set_borrower_whitelist, set_pause, set_blacklist_mode, etc.     |
//!
//! ## Security Model
//!
//! - All accounts validated via PDA derivation (no raw address trust)
//! - Blacklist checked on all user-facing operations
//! - Interest accrued before balance-modifying operations
//! - Settlement factor monotonically increases (prevents gaming)
//!
//! ## Implementation Notes
//!
//! This program uses the Pinocchio framework for optimized Solana development:
//! - Zero-copy account access via `AccountView`
//! - No heap allocation (`no_allocator!()`)
//! - Minimal runtime overhead

#![cfg_attr(target_os = "solana", no_std)]

pub mod constants;
pub mod error;
pub mod logic;
pub mod processor;
pub mod state;

use pinocchio::error::ProgramError;
use pinocchio::{
    no_allocator, nostd_panic_handler, program_entrypoint, AccountView, Address, ProgramResult,
};

nostd_panic_handler!();
no_allocator!();

program_entrypoint!(process_instruction);

fn process_instruction(
    program_id: &Address,
    accounts: &[AccountView],
    data: &[u8],
) -> ProgramResult {
    let disc = data.first().ok_or(ProgramError::InvalidInstructionData)?;
    match disc {
        // ═══════════════════════════════════════════════════════════════
        // ADMIN/SETUP (0-2)
        // ═══════════════════════════════════════════════════════════════
        0 => processor::initialize_protocol(program_id, accounts, &data[1..]),
        1 => processor::set_fee_config(program_id, accounts, &data[1..]),
        2 => processor::create_market(program_id, accounts, &data[1..]),

        // ═══════════════════════════════════════════════════════════════
        // CORE LENDING (3-7)
        // ═══════════════════════════════════════════════════════════════
        3 => processor::deposit(program_id, accounts, &data[1..]),
        4 => processor::borrow(program_id, accounts, &data[1..]),
        5 => processor::repay(program_id, accounts, &data[1..]),
        6 => processor::repay_interest(program_id, accounts, &data[1..]),
        7 => processor::withdraw(program_id, accounts, &data[1..]),

        // ═══════════════════════════════════════════════════════════════
        // SETTLEMENT (8-11)
        // ═══════════════════════════════════════════════════════════════
        8 => processor::collect_fees(program_id, accounts, &data[1..]),
        9 => processor::re_settle(program_id, accounts, &data[1..]),
        10 => processor::close_lender_position(program_id, accounts, &data[1..]),
        11 => processor::withdraw_excess(program_id, accounts, &data[1..]),

        // ═══════════════════════════════════════════════════════════════
        // ACCESS CONTROL (12-16)
        // ═══════════════════════════════════════════════════════════════
        12 => processor::set_borrower_whitelist(program_id, accounts, &data[1..]),
        13 => processor::set_pause(program_id, accounts, &data[1..]),
        14 => processor::set_blacklist_mode(program_id, accounts, &data[1..]),
        15 => processor::set_admin(program_id, accounts, &data[1..]),
        16 => processor::set_whitelist_manager(program_id, accounts, &data[1..]),

        _ => Err(ProgramError::InvalidInstructionData),
    }
}
