use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{DISC_PROTOCOL_CONFIG, SEED_PROTOCOL_CONFIG, ZERO_ADDRESS};
use crate::error::LendingError;
use crate::state::ProtocolConfig;

/// SetWhitelistManager (disc 16)
/// Change the whitelist manager to a new address.
/// Only the current admin can call this.
pub fn process(program_id: &Address, accounts: &[AccountView], _data: &[u8]) -> ProgramResult {
    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let protocol_config_account = &accounts[0];
    let admin = &accounts[1];
    let new_whitelist_manager = &accounts[2];

    // Verify PDA
    let (expected_pda, _bump) = Address::find_program_address(&[SEED_PROTOCOL_CONFIG], program_id);
    if protocol_config_account.address() != &expected_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Verify ownership before deserializing
    if !protocol_config_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Read current config to validate admin
    {
        // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
        let config_data = unsafe { protocol_config_account.borrow_unchecked() };
        let config_ref: &ProtocolConfig =
            bytemuck::try_from_bytes(config_data).map_err(|_| ProgramError::InvalidAccountData)?;

        // Discriminator check for ProtocolConfig
        if config_ref.discriminator != DISC_PROTOCOL_CONFIG {
            return Err(ProgramError::InvalidAccountData);
        }

        // Admin must match and be signer
        if config_ref.admin != *admin.address().as_ref() {
            return Err(LendingError::Unauthorized.into());
        }
        if !admin.is_signer() {
            return Err(LendingError::Unauthorized.into());
        }

        // New whitelist manager must not be zero address
        if new_whitelist_manager.address().as_ref() == ZERO_ADDRESS {
            return Err(LendingError::InvalidAddress.into());
        }
    }

    // Mutably update config
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let config_data_mut = unsafe { protocol_config_account.borrow_unchecked_mut() };
    let config: &mut ProtocolConfig = bytemuck::try_from_bytes_mut(config_data_mut)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    config
        .whitelist_manager
        .copy_from_slice(new_whitelist_manager.address().as_ref());

    log!(
        "evt:set_whitelist_manager new_manager={}",
        crate::logic::events::short_hex(&config.whitelist_manager)
    );

    Ok(())
}
