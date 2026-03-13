use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    BORROWER_WHITELIST_SIZE, DISC_BORROWER_WL, DISC_PROTOCOL_CONFIG, SEED_BORROWER_WHITELIST,
    SEED_PROTOCOL_CONFIG,
};
use crate::error::LendingError;
use crate::state::{BorrowerWhitelist, ProtocolConfig};

/// SetBorrowerWhitelist (disc 12)
/// Add or update a borrower's whitelist status and global borrow capacity.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let borrower_whitelist_account = &accounts[0];
    let protocol_config_account = &accounts[1];
    let whitelist_manager = &accounts[2];
    let borrower_address = &accounts[3];
    // accounts[4] = system_program

    // Parse instruction data: is_whitelisted (1 byte) + max_borrow_capacity (8 bytes)
    if data.len() < 9 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let is_whitelisted = data[0];
    let max_borrow_capacity = u64::from_le_bytes(
        data[1..9]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

    // Verify protocol_config PDA
    let (expected_config_pda, _) =
        Address::find_program_address(&[SEED_PROTOCOL_CONFIG], program_id);
    if protocol_config_account.address() != &expected_config_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    // Verify ownership before deserializing
    if !protocol_config_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Read protocol config
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let config_data = unsafe { protocol_config_account.borrow_unchecked() };
    let config: &ProtocolConfig =
        bytemuck::try_from_bytes(config_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for ProtocolConfig
    if config.discriminator != DISC_PROTOCOL_CONFIG {
        return Err(ProgramError::InvalidAccountData);
    }

    // SR-107: whitelist_manager must match and be signer
    if config.whitelist_manager != *whitelist_manager.address().as_ref() {
        return Err(LendingError::Unauthorized.into());
    }
    if !whitelist_manager.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // SR-108: max_borrow_capacity > 0 when whitelisting
    if is_whitelisted == 1 && max_borrow_capacity == 0 {
        return Err(LendingError::InvalidCapacity.into());
    }

    // Verify borrower_whitelist PDA
    let borrower_key = borrower_address.address();
    let (expected_wl_pda, wl_bump) = Address::find_program_address(
        &[SEED_BORROWER_WHITELIST, borrower_key.as_ref()],
        program_id,
    );
    if borrower_whitelist_account.address() != &expected_wl_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Use ownership check instead of lamports to determine if account exists.
    // Lamports can be donated to any PDA, so lamports > 0 is not proof of initialization.
    let account_exists = borrower_whitelist_account.owned_by(program_id);

    if !account_exists && is_whitelisted == 1 {
        // Defense-in-depth: reject if whitelist account already has data (fail closed).
        // Finding 5: Previously used `if let Ok(...)` which silently skipped
        // malformed accounts. Now check discriminator bytes directly.
        if borrower_whitelist_account.data_len() > 0 {
            let existing_data = unsafe { borrower_whitelist_account.borrow_unchecked() };
            if existing_data.len() >= 8 && existing_data[..8] == DISC_BORROWER_WL {
                return Err(LendingError::AlreadyInitialized.into());
            }
            if borrower_whitelist_account.owned_by(program_id) {
                return Err(ProgramError::InvalidAccountData);
            }
        }

        // Create the account
        let wl_bump_ref = [wl_bump];
        let signer_seeds = [
            pinocchio::cpi::Seed::from(SEED_BORROWER_WHITELIST),
            pinocchio::cpi::Seed::from(borrower_key.as_ref()),
            pinocchio::cpi::Seed::from(&wl_bump_ref),
        ];
        pinocchio_system::create_account_with_minimum_balance_signed(
            borrower_whitelist_account,
            BORROWER_WHITELIST_SIZE,
            program_id,
            whitelist_manager,
            None,
            &[pinocchio::cpi::Signer::from(&signer_seeds)],
        )?;

        // SAFETY: This is the only mutable borrow of this account in this instruction.
        // Account data length is verified by bytemuck::try_from_bytes_mut.
        let wl_data = unsafe { borrower_whitelist_account.borrow_unchecked_mut() };
        let wl: &mut BorrowerWhitelist =
            bytemuck::try_from_bytes_mut(wl_data).map_err(|_| ProgramError::InvalidAccountData)?;

        wl.discriminator.copy_from_slice(&DISC_BORROWER_WL);
        wl.version = 1;
        wl.borrower.copy_from_slice(borrower_key.as_ref());
        wl.is_whitelisted = 1;
        wl.set_max_borrow_capacity(max_borrow_capacity);
        // current_borrowed stays 0 (zeroed by create)
        wl.bump = wl_bump;
    } else if account_exists {
        // Verify ownership before deserializing existing account
        if !borrower_whitelist_account.owned_by(program_id) {
            return Err(LendingError::InvalidAccountOwner.into());
        }
        // Update existing account
        // SAFETY: This is the only mutable borrow of this account in this instruction.
        // Account data length is verified by bytemuck::try_from_bytes_mut.
        let wl_data = unsafe { borrower_whitelist_account.borrow_unchecked_mut() };
        let wl: &mut BorrowerWhitelist =
            bytemuck::try_from_bytes_mut(wl_data).map_err(|_| ProgramError::InvalidAccountData)?;

        // Discriminator check for BorrowerWhitelist
        if wl.discriminator != DISC_BORROWER_WL {
            return Err(ProgramError::InvalidAccountData);
        }

        // SR-137: Prevent de-whitelisting borrowers with outstanding debt
        // Removing a borrower from the whitelist while they have active debt
        // could prevent proper debt collection and accounting
        if is_whitelisted == 0 && wl.current_borrowed() > 0 {
            return Err(LendingError::BorrowerHasActiveDebt.into());
        }

        wl.is_whitelisted = is_whitelisted;
        wl.set_max_borrow_capacity(max_borrow_capacity);
        // current_borrowed is NOT modified
    } else {
        // !account_exists && is_whitelisted == 0: de-whitelisting a borrower
        // whose whitelist PDA was never created is an error.
        return Err(LendingError::NotWhitelisted.into());
    }

    log!(
        "evt:set_whitelist borrower={} whitelisted={}",
        crate::logic::events::short_hex(borrower_key.as_ref()),
        is_whitelisted
    );

    Ok(())
}
