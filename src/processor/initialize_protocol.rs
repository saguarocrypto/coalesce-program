use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    BPF_LOADER_UPGRADEABLE_ID, DISC_PROTOCOL_CONFIG, MAX_FEE_RATE_BPS, PROTOCOL_CONFIG_SIZE,
    SEED_PROTOCOL_CONFIG, ZERO_ADDRESS,
};
use crate::error::LendingError;
use crate::state::ProtocolConfig;

/// InitializeProtocol (disc 0)
/// Creates the singleton ProtocolConfig account.
/// Only the program's upgrade authority can initialize the protocol.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    // --- Parse accounts ---
    if accounts.len() < 7 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let protocol_config_account = &accounts[0];
    let admin = &accounts[1];
    let fee_authority = &accounts[2];
    let whitelist_manager = &accounts[3];
    let blacklist_program = &accounts[4];
    // accounts[5] = system_program
    let program_data_account = &accounts[6];

    // --- Parse instruction data ---
    if data.len() < 2 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let fee_rate_bps = u16::from_le_bytes([data[0], data[1]]);

    // --- Validate ---
    // SR-012: admin must be signer
    if !admin.is_signer() {
        return Err(LendingError::Unauthorized.into());
    }

    // Verify admin is the program's upgrade authority
    // Derive expected program data address
    let bpf_loader = Address::new_from_array(BPF_LOADER_UPGRADEABLE_ID);
    let (expected_program_data, _) =
        Address::find_program_address(&[program_id.as_ref()], &bpf_loader);
    if program_data_account.address() != &expected_program_data {
        log!("Invalid program data account");
        return Err(LendingError::InvalidPDA.into());
    }
    // Verify program_data account is owned by BPF Loader Upgradeable
    if unsafe { program_data_account.owner() } != &bpf_loader {
        log!("Program data not owned by BPF loader");
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Read upgrade authority from program data account
    // Layout: [4 bytes type][8 bytes slot][1 byte option][32 bytes authority]
    // SAFETY: Read-only access to verify upgrade authority
    let program_data = unsafe { program_data_account.borrow_unchecked() };
    if program_data.len() < 45 {
        log!("Program data account too small");
        return Err(ProgramError::InvalidAccountData);
    }

    // Check account type is ProgramData (type = 3)
    let account_type = u32::from_le_bytes([
        program_data[0],
        program_data[1],
        program_data[2],
        program_data[3],
    ]);
    if account_type != 3 {
        log!("Not a program data account");
        return Err(ProgramError::InvalidAccountData);
    }

    // Check if upgrade authority is present (byte 12 = 1 means Some)
    if program_data[12] != 1 {
        log!("Program has no upgrade authority (immutable)");
        return Err(LendingError::Unauthorized.into());
    }

    // Extract upgrade authority (bytes 13-44)
    let upgrade_authority = &program_data[13..45];
    if admin.address().as_ref() != upgrade_authority {
        log!("Admin is not the upgrade authority");
        return Err(LendingError::Unauthorized.into());
    }

    // SR-011: fee_rate_bps <= 10,000
    if fee_rate_bps > MAX_FEE_RATE_BPS {
        return Err(LendingError::InvalidFeeRate.into());
    }

    // SR-013: fee_authority must not be zero
    if fee_authority.address().as_ref() == ZERO_ADDRESS {
        return Err(LendingError::InvalidAddress.into());
    }

    // SR-103: whitelist_manager must not be zero
    if whitelist_manager.address().as_ref() == ZERO_ADDRESS {
        return Err(LendingError::InvalidAddress.into());
    }

    // SR-104: blacklist_program must not be zero
    if blacklist_program.address().as_ref() == ZERO_ADDRESS {
        return Err(LendingError::InvalidAddress.into());
    }

    // Derive PDA and verify
    let (expected_pda, bump) = Address::find_program_address(&[SEED_PROTOCOL_CONFIG], program_id);
    if protocol_config_account.address() != &expected_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Defense-in-depth: reject if account already has data (fail closed).
    // Finding 5: Previously used `if let Ok(...)` which silently skipped
    // malformed accounts. Now check discriminator bytes directly.
    if protocol_config_account.data_len() > 0 {
        let data = unsafe { protocol_config_account.borrow_unchecked() };
        if data.len() >= 8 && data[..8] == DISC_PROTOCOL_CONFIG {
            return Err(LendingError::AlreadyInitialized.into());
        }
        // Fail closed: if account has data but doesn't match our discriminator,
        // it may be corrupted or a different account type — still reject.
        if protocol_config_account.owned_by(program_id) {
            return Err(ProgramError::InvalidAccountData);
        }
    }

    // SR-010: Create the PDA account via CPI
    let bump_ref = [bump];
    let signer_seeds = [
        pinocchio::cpi::Seed::from(SEED_PROTOCOL_CONFIG),
        pinocchio::cpi::Seed::from(&bump_ref),
    ];
    pinocchio_system::create_account_with_minimum_balance_signed(
        protocol_config_account,
        PROTOCOL_CONFIG_SIZE,
        program_id,
        admin,
        None,
        &[pinocchio::cpi::Signer::from(&signer_seeds)],
    )?;

    // Write data
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let data = unsafe { protocol_config_account.borrow_unchecked_mut() };
    let config: &mut ProtocolConfig =
        bytemuck::try_from_bytes_mut(data).map_err(|_| ProgramError::InvalidAccountData)?;

    config.discriminator.copy_from_slice(&DISC_PROTOCOL_CONFIG);
    config.version = 1;
    config.admin.copy_from_slice(admin.address().as_ref());
    config.set_fee_rate_bps(fee_rate_bps);
    config
        .fee_authority
        .copy_from_slice(fee_authority.address().as_ref());
    config
        .whitelist_manager
        .copy_from_slice(whitelist_manager.address().as_ref());
    config
        .blacklist_program
        .copy_from_slice(blacklist_program.address().as_ref());
    config.is_initialized = 1;
    config.bump = bump;

    log!(
        "evt:initialize_protocol admin={} fee_bps={}",
        crate::logic::events::short_hex(&config.admin),
        fee_rate_bps
    );

    Ok(())
}
