use pinocchio::error::ProgramError;
use pinocchio::{AccountView, Address, ProgramResult};
use solana_program_log::log;

use crate::constants::{
    DISC_BORROWER_WL, DISC_MARKET, DISC_PROTOCOL_CONFIG, MARKET_SIZE, MAX_ANNUAL_INTEREST_BPS,
    MAX_MATURITY_DELTA, MIN_MATURITY_DELTA, SEED_BORROWER_WHITELIST, SEED_MARKET,
    SEED_MARKET_AUTHORITY, SEED_PROTOCOL_CONFIG, SEED_VAULT, SETTLEMENT_GRACE_PERIOD,
    SPL_TOKEN_ACCOUNT_SIZE, USDC_DECIMALS, WAD,
};
use crate::error::LendingError;
use crate::logic::validation::{check_blacklist, get_unix_timestamp};
use crate::state::{BorrowerWhitelist, Market, ProtocolConfig};

/// CreateMarket (disc 2)
/// Create a new lending market with fixed terms.
pub fn process(program_id: &Address, accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 10 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let market_account = &accounts[0];
    let borrower = &accounts[1];
    let mint_account = &accounts[2];
    let vault_account = &accounts[3];
    let market_authority_account = &accounts[4];
    let protocol_config_account = &accounts[5];
    let borrower_whitelist_account = &accounts[6];
    let blacklist_check = &accounts[7];
    // accounts[8] = system_program
    // accounts[9] = token_program

    // Parse instruction data: nonce(8) + annual_interest_bps(2) + maturity_timestamp(8) + max_total_supply(8)
    if data.len() < 26 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let market_nonce = u64::from_le_bytes(
        data[0..8]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let annual_interest_bps = u16::from_le_bytes(
        data[8..10]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let maturity_timestamp = i64::from_le_bytes(
        data[10..18]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let max_total_supply = u64::from_le_bytes(
        data[18..26]
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );

    // --- Validations ---

    // SR-017: borrower must be signer
    if !borrower.is_signer() {
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

    // SR-018: protocol must be initialized
    if config.is_initialized != 1 {
        return Err(LendingError::AlreadyInitialized.into());
    }

    // SR-019: annual_interest_bps <= 10,000
    if annual_interest_bps > MAX_ANNUAL_INTEREST_BPS {
        return Err(LendingError::InvalidFeeRate.into());
    }

    // SR-020/SR-085: maturity must be > current time + MIN_MATURITY_DELTA
    let current_ts = get_unix_timestamp()?;
    let min_maturity = current_ts
        .checked_add(MIN_MATURITY_DELTA)
        .ok_or(LendingError::MathOverflow)?;
    if maturity_timestamp <= min_maturity {
        return Err(LendingError::InvalidMaturity.into());
    }

    // H-13: Enforce maximum maturity (5 years) to bound interest accumulation
    let max_maturity = current_ts
        .checked_add(MAX_MATURITY_DELTA)
        .ok_or(LendingError::MathOverflow)?;
    if maturity_timestamp > max_maturity {
        return Err(LendingError::InvalidMaturity.into());
    }
    // Verify that maturity + grace period does not overflow
    maturity_timestamp
        .checked_add(SETTLEMENT_GRACE_PERIOD)
        .ok_or(LendingError::MathOverflow)?;

    // SR-021: max_total_supply > 0
    if max_total_supply == 0 {
        return Err(LendingError::InvalidCapacity.into());
    }

    // SR-022/SR-063: Validate mint has USDC_DECIMALS
    // SAFETY: Token account data is validated by the SPL Token program which owns it.
    let mint_ref =
        unsafe { pinocchio_token::state::Mint::from_account_view_unchecked(mint_account)? };
    if mint_ref.decimals() != USDC_DECIMALS {
        return Err(LendingError::InvalidMint.into());
    }

    // COAL-L01: Enforce USDC-only markets.
    if *mint_account.address().as_ref() != crate::constants::USDC_MINT {
        return Err(LendingError::InvalidMint.into());
    }

    // SR-025: borrower must be whitelisted
    let (expected_wl_pda, _) = Address::find_program_address(
        &[SEED_BORROWER_WHITELIST, borrower.address().as_ref()],
        program_id,
    );
    if borrower_whitelist_account.address() != &expected_wl_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    // Verify ownership before deserializing
    if !borrower_whitelist_account.owned_by(program_id) {
        return Err(LendingError::InvalidAccountOwner.into());
    }
    // SAFETY: Read-only borrow. Account data length is verified by bytemuck::try_from_bytes.
    let wl_data = unsafe { borrower_whitelist_account.borrow_unchecked() };
    let wl: &BorrowerWhitelist =
        bytemuck::try_from_bytes(wl_data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Discriminator check for BorrowerWhitelist
    if wl.discriminator != DISC_BORROWER_WL {
        return Err(ProgramError::InvalidAccountData);
    }

    if wl.is_whitelisted != 1 {
        return Err(LendingError::NotWhitelisted.into());
    }

    // SR-105: Blacklist check for borrower
    check_blacklist(blacklist_check, config, borrower.address())?;

    // Derive market PDA
    let nonce_bytes = market_nonce.to_le_bytes();
    let (expected_market_pda, market_bump) = Address::find_program_address(
        &[SEED_MARKET, borrower.address().as_ref(), &nonce_bytes],
        program_id,
    );
    if market_account.address() != &expected_market_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Derive vault PDA
    let (expected_vault_pda, vault_bump) =
        Address::find_program_address(&[SEED_VAULT, market_account.address().as_ref()], program_id);
    if vault_account.address() != &expected_vault_pda {
        return Err(LendingError::InvalidVault.into());
    }

    // Derive market_authority PDA
    let (expected_auth_pda, auth_bump) = Address::find_program_address(
        &[SEED_MARKET_AUTHORITY, market_account.address().as_ref()],
        program_id,
    );
    if market_authority_account.address() != &expected_auth_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Defense-in-depth: reject if market account already has data (fail closed).
    // Finding 5: Previously used `if let Ok(...)` which silently skipped
    // malformed accounts. Now check discriminator bytes directly.
    if market_account.data_len() > 0 {
        let existing_data = unsafe { market_account.borrow_unchecked() };
        if existing_data.len() >= 8 && existing_data[..8] == DISC_MARKET {
            return Err(LendingError::MarketAlreadyExists.into());
        }
        if market_account.owned_by(program_id) {
            return Err(ProgramError::InvalidAccountData);
        }
    }

    // --- Create market account ---
    let market_bump_ref = [market_bump];
    let market_signer_seeds = [
        pinocchio::cpi::Seed::from(SEED_MARKET),
        pinocchio::cpi::Seed::from(borrower.address().as_ref()),
        pinocchio::cpi::Seed::from(nonce_bytes.as_ref()),
        pinocchio::cpi::Seed::from(&market_bump_ref),
    ];
    pinocchio_system::create_account_with_minimum_balance_signed(
        market_account,
        MARKET_SIZE,
        program_id,
        borrower,
        None,
        &[pinocchio::cpi::Signer::from(&market_signer_seeds)],
    )?;

    // --- Create vault token account ---
    // Step 1: Create account owned by Token Program
    let vault_bump_ref = [vault_bump];
    let vault_signer_seeds = [
        pinocchio::cpi::Seed::from(SEED_VAULT),
        pinocchio::cpi::Seed::from(market_account.address().as_ref()),
        pinocchio::cpi::Seed::from(&vault_bump_ref),
    ];
    pinocchio_system::create_account_with_minimum_balance_signed(
        vault_account,
        SPL_TOKEN_ACCOUNT_SIZE as usize,
        &pinocchio_token::ID,
        borrower,
        None,
        &[pinocchio::cpi::Signer::from(&vault_signer_seeds)],
    )?;

    // Step 2: Initialize token account with InitializeAccount3
    pinocchio_token::instructions::InitializeAccount3 {
        account: vault_account,
        mint: mint_account,
        owner: &expected_auth_pda,
    }
    .invoke()?;

    // --- Write market data ---
    // SAFETY: This is the only mutable borrow of this account in this instruction.
    // Account data length is verified by bytemuck::try_from_bytes_mut.
    let market_data = unsafe { market_account.borrow_unchecked_mut() };
    let market: &mut Market =
        bytemuck::try_from_bytes_mut(market_data).map_err(|_| ProgramError::InvalidAccountData)?;

    market.discriminator.copy_from_slice(&DISC_MARKET);
    market.version = 1;
    market.borrower.copy_from_slice(borrower.address().as_ref());
    market.mint.copy_from_slice(mint_account.address().as_ref());
    market
        .vault
        .copy_from_slice(vault_account.address().as_ref());
    market.market_authority_bump = auth_bump;
    market.set_annual_interest_bps(annual_interest_bps);
    market.set_maturity_timestamp(maturity_timestamp);
    market.set_max_total_supply(max_total_supply);
    market.set_market_nonce(market_nonce);
    market.set_scale_factor(WAD);
    market.set_last_accrual_timestamp(current_ts);
    // All other fields are 0 from create (scaled_total_supply, accrued_protocol_fees, etc.)
    market.set_settlement_factor_wad(0);
    market.bump = market_bump;

    log!(
        "evt:create_market market={} borrower={} nonce={}",
        crate::logic::events::short_hex(market_account.address().as_ref()),
        crate::logic::events::short_hex(borrower.address().as_ref()),
        market_nonce
    );

    Ok(())
}
