use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_PROTOCOL_CONFIG, MAX_FEE_RATE_BPS, SEED_PROTOCOL_CONFIG, ZERO_ADDRESS,
};
use crate::error::LendingError;
use crate::state::ProtocolConfig;

/// SetFeeConfig (disc 1)
/// Update protocol fee rate and fee authority.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let protocol_config_account = &accounts[0];
    let admin = &accounts[1];
    let new_fee_authority = &accounts[2];

    // Parse instruction data
    if data.len() < 2 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let new_fee_rate_bps = u16::from_le_bytes([data[0], data[1]]);

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
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let config_data = unsafe { protocol_config_account.borrow_unchecked() };
    let config_ref: &ProtocolConfig =
        bytemuck::try_from_bytes(config_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for ProtocolConfig
    if config_ref.discriminator != DISC_PROTOCOL_CONFIG {
        return Err(ProgramError::InvalidAccountData);
    }

    // SR-014: admin must match
    if config_ref.admin != *admin.address().as_ref() {
        return Err(LendingError::Unauthorized.into());
    }
    // admin must be signer
    if !admin.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // SR-015: fee rate <= 10,000
    if new_fee_rate_bps > MAX_FEE_RATE_BPS {
        return Err(LendingError::InvalidFeeRate.into());
    }

    // SR-016: new_fee_authority must not be zero
    if new_fee_authority.address().as_ref() == ZERO_ADDRESS {
        return Err(LendingError::InvalidAddress.into());
    }

    // Mutably update config
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let config_data_mut = unsafe { protocol_config_account.borrow_unchecked_mut() };
    let config: &mut ProtocolConfig = bytemuck::try_from_bytes_mut(config_data_mut)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    config.set_fee_rate_bps(new_fee_rate_bps);
    config
        .fee_authority
        .copy_from_slice(new_fee_authority.address().as_ref());

    log!("evt:set_fee_config fee_bps={}", new_fee_rate_bps);

    Ok(())
}
