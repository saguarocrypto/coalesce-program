use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_LENDER_POSITION, DISC_MARKET, DISC_PROTOCOL_CONFIG, SEED_LENDER, SEED_MARKET,
    SEED_PROTOCOL_CONFIG,
};
use crate::error::LendingError;
use crate::state::{LenderPosition, Market, ProtocolConfig};
use pinocchio_system;

/// CloseLenderPosition (disc 10)
/// Close an empty lender position account and return rent to the lender.
pub fn process(program_id: &Address, accounts: &[AccountView], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let lender = &accounts[1];
    let lender_position_account = &accounts[2];
    // accounts[3] = system_program
    let protocol_config_account = &accounts[4];

    // SR-095: lender must be signer
    if !lender.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // Verify protocol_config PDA and read it
    let (expected_config_pda, _) =
        Address::find_program_address(&[SEED_PROTOCOL_CONFIG], program_id);
    if protocol_config_account.address() != &expected_config_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    // Verify ownership before deserializing
    if !protocol_config_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let config_data = unsafe { protocol_config_account.borrow_unchecked() };
    let config: &ProtocolConfig =
        bytemuck::try_from_bytes(config_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for ProtocolConfig
    if config.discriminator != DISC_PROTOCOL_CONFIG {
        return Err(ProgramError::InvalidAccountData);
    }

    // Emergency pause check
    if config.is_paused() {
        return Err(LendingError::ProtocolPaused.into());
    }

    // SR-115: Verify market account is owned by our program
    if !market_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Read market and verify discriminator + PDA
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let market_data = unsafe { market_account.borrow_unchecked() };
    let market: &Market =
        bytemuck::try_from_bytes(market_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // SR-122: Discriminator check for Market
    if market.discriminator != DISC_MARKET {
        return Err(ProgramError::InvalidAccountData);
    }

    // SR-121: Verify market PDA derivation
    let nonce_bytes = market.market_nonce().to_le_bytes();
    let (expected_market_pda, _) =
        Address::find_program_address(&[SEED_MARKET, &market.borrower, &nonce_bytes], program_id);
    if market_account.address() != &expected_market_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Verify lender_position PDA
    let (expected_pos_pda, _) = Address::find_program_address(
        &[
            SEED_LENDER,
            market_account.address().as_ref(),
            lender.address().as_ref(),
        ],
        program_id,
    );
    if lender_position_account.address() != &expected_pos_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Verify position owned by our program
    if !lender_position_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Read position data and verify before zeroing
    {
        // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
        let pos_data = unsafe { lender_position_account.borrow_unchecked() };
        let position: &LenderPosition =
            bytemuck::try_from_bytes(pos_data).map_err(|_| ProgramError::InvalidAccountData)?;

        // Discriminator check for LenderPosition
        if position.discriminator != DISC_LENDER_POSITION {
            return Err(ProgramError::InvalidAccountData);
        }

        // SR-097: lender must match position.lender
        if position.lender != *lender.address().as_ref() {
            return Err(LendingError::Unauthorized.into());
        }

        // Step 1: Verify market matches
        if position.market != *market_account.address().as_ref() {
            return Err(LendingError::InvalidPDA.into());
        }

        // SR-096: scaled_balance must be 0
        if position.scaled_balance() != 0 {
            return Err(LendingError::PositionNotEmpty.into());
        }

        // COAL-H01: Cannot close position with pending haircut claim.
        // Lender must call claim_haircut first to recover owed funds.
        if position.haircut_owed() != 0 {
            return Err(LendingError::PositionNotEmpty.into());
        }
    }

    // Step 2: Zero account data
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by the read-only borrow above.
    let pos_data_mut = unsafe { lender_position_account.borrow_unchecked_mut() };
    for byte in pos_data_mut.iter_mut() {
        *byte = 0;
    }

    // Step 3: Transfer all lamports from position to lender.
    // The runtime reclaims zero-lamport accounts at end-of-transaction.
    // Step 4 below reassigns ownership to make closure explicit.
    let position_lamports = lender_position_account.lamports();
    lender_position_account.set_lamports(0);
    let new_lender_lamports = lender
        .lamports()
        .checked_add(position_lamports)
        .ok_or(LendingError::MathOverflow)?;
    lender.set_lamports(new_lender_lamports);

    // Step 4: Reassign ownership to the system program so the account is
    // fully closed within this instruction rather than waiting for
    // end-of-transaction cleanup of the zero-lamport account.
    // SAFETY: No active reference to the owner exists; the read-only borrows above are
    // scoped and dropped. The program owns this account so the runtime permits reassignment.
    unsafe {
        lender_position_account.assign(&pinocchio_system::ID);
    }

    log!(
        "evt:close_position market={} lender={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(lender.address().as_ref())
    );

    Ok(())
}
