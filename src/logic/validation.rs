use crate::constants::{
    SEED_BLACKLIST, SEED_MARKET, SEED_MARKET_AUTHORITY, SEED_PROTOCOL_CONFIG, ZERO_ADDRESS,
};
use crate::error::LendingError;
use crate::state::{Market, ProtocolConfig};
use pinocchio::error::ProgramError;
use pinocchio::sysvars::clock::Clock;
use pinocchio::sysvars::Sysvar;
use pinocchio::{AccountView, Address, ProgramResult};

/// Read `unix_timestamp` from the Clock sysvar via the `sol_get_clock_sysvar` syscall.
///
/// This replaces the previous approach of passing the Clock sysvar as an account
/// parameter. Using the syscall is the recommended Solana best practice — it saves
/// one account slot per transaction, reduces transaction size, and eliminates the
/// (theoretical) clock account spoofing attack surface.
pub fn get_unix_timestamp() -> Result<i64, ProgramError> {
    let clock = Clock::get()?;
    Ok(clock.unix_timestamp)
}

/// Returns true if the given 32-byte key is the zero address.
pub fn is_zero_address(key: &[u8; 32]) -> bool {
    *key == ZERO_ADDRESS
}

/// Validate that market.scale_factor is non-zero (defense-in-depth).
/// A zero scale_factor would cause division-by-zero in deposit/withdraw math.
pub fn validate_market_state(market: &Market) -> Result<(), ProgramError> {
    if market.scale_factor() == 0 {
        return Err(LendingError::InvalidScaleFactor.into());
    }
    Ok(())
}

/// Validate market PDA derivation: `[SEED_MARKET, &market.borrower, &nonce_bytes]`.
pub fn validate_market_pda(
    market_account: &AccountView,
    market: &Market,
    program_id: &Address,
) -> Result<(), ProgramError> {
    let nonce_bytes = market.market_nonce().to_le_bytes();
    let (expected_market_pda, _) =
        Address::find_program_address(&[SEED_MARKET, &market.borrower, &nonce_bytes], program_id);
    if market_account.address() != &expected_market_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    Ok(())
}

/// Validate protocol config PDA derivation: `[SEED_PROTOCOL_CONFIG]`.
pub fn validate_protocol_config_pda(
    config_account: &AccountView,
    program_id: &Address,
) -> Result<(), ProgramError> {
    let (expected_config_pda, _) =
        Address::find_program_address(&[SEED_PROTOCOL_CONFIG], program_id);
    if config_account.address() != &expected_config_pda {
        return Err(LendingError::InvalidPDA.into());
    }
    Ok(())
}

/// Validate market authority PDA and re-derive bump for CPI signing safety.
/// Returns `Ok(())` if both the authority address and stored bump match the
/// on-chain derivation.
pub fn validate_market_authority(
    market_authority: &AccountView,
    market_account: &AccountView,
    market: &Market,
    program_id: &Address,
) -> Result<(), ProgramError> {
    let (expected_authority, expected_bump) = Address::find_program_address(
        &[SEED_MARKET_AUTHORITY, market_account.address().as_ref()],
        program_id,
    );
    if expected_bump != market.market_authority_bump {
        return Err(LendingError::InvalidPDA.into());
    }
    if market_authority.address() != &expected_authority {
        return Err(LendingError::InvalidPDA.into());
    }
    Ok(())
}

// H-3: Compile-time assertion that [u8; 32] and pinocchio::Address have the same size.
// This guarantees the pointer cast in check_blacklist is sound.
const _: () = assert!(core::mem::size_of::<[u8; 32]>() == core::mem::size_of::<Address>());

/// Check external blacklist for a given address (§8.3).
///
/// `blacklist_check` — the account the caller claims is the blacklist PDA.
/// `protocol_config` — to read `blacklist_program`.
/// `address` — the address being checked.
///
/// # Trust Model
/// The blacklist program address is stored in ProtocolConfig, which is only
/// writable by the protocol admin. This makes the blacklist program admin-trusted;
/// the protocol admin is responsible for setting it to a legitimate program.
pub fn check_blacklist(
    blacklist_check: &AccountView,
    protocol_config: &ProtocolConfig,
    address: &Address,
) -> ProgramResult {
    // SAFETY: blacklist_program is a [u8; 32] field which has the same layout as Address.
    let blacklist_program: &Address =
        unsafe { &*(protocol_config.blacklist_program.as_ptr().cast::<Address>()) };

    // Step 2: Derive expected PDA from external program
    let (expected_pda, _bump) =
        Address::find_program_address(&[SEED_BLACKLIST, address.as_ref()], blacklist_program);

    // Step 3: Provided account must match derived PDA
    if blacklist_check.address() != &expected_pda {
        return Err(LendingError::InvalidPDA.into());
    }

    // Step 4: Non-existent account (0 lamports)
    // Behavior depends on blacklist_mode:
    //   - fail-open (default): non-existent = not blacklisted
    //   - fail-closed: non-existent = blacklisted
    if blacklist_check.lamports() == 0 {
        if protocol_config.is_blacklist_fail_closed() {
            return Err(LendingError::Blacklisted.into());
        }
        return Ok(());
    }

    // Step 5: Owner must be the blacklist program
    // SAFETY: Read-only access to account owner field.
    if unsafe { blacklist_check.owner() } != blacklist_program {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Step 6: Data must have >= 1 byte
    // SAFETY: Read-only borrow. Data length is checked immediately after.
    let data = unsafe { blacklist_check.borrow_unchecked() };
    if data.is_empty() {
        return Err(LendingError::InvalidAccountOwner.into());
    }

    // Step 7: Status byte check
    match data[0] {
        1 => Err(LendingError::Blacklisted.into()),
        0 => Ok(()),
        _ => Err(LendingError::InvalidAccountOwner.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_zero_address_true() {
        let zero = [0u8; 32];
        assert!(is_zero_address(&zero));
    }

    #[test]
    fn test_is_zero_address_false_first_byte() {
        let mut addr = [0u8; 32];
        addr[0] = 1;
        assert!(!is_zero_address(&addr));
    }

    #[test]
    fn test_is_zero_address_false_last_byte() {
        let mut addr = [0u8; 32];
        addr[31] = 1;
        assert!(!is_zero_address(&addr));
    }

    #[test]
    fn test_is_zero_address_false_middle_byte() {
        let mut addr = [0u8; 32];
        addr[16] = 0xFF;
        assert!(!is_zero_address(&addr));
    }

    #[test]
    fn test_is_zero_address_all_ones() {
        let addr = [0xFF; 32];
        assert!(!is_zero_address(&addr));
    }

    #[test]
    fn test_is_zero_address_all_zeros_matches_constant() {
        assert_eq!(ZERO_ADDRESS, [0u8; 32]);
        assert!(is_zero_address(&ZERO_ADDRESS));
    }

    // Test each of 32 byte positions with single non-zero byte
    #[test]
    fn test_is_zero_address_single_nonzero_byte() {
        for i in 0..32 {
            let mut addr = [0u8; 32];
            addr[i] = 1;
            assert!(
                !is_zero_address(&addr),
                "byte position {i} with value 1 should not be zero address"
            );
        }
    }

    // Alternating patterns
    #[test]
    fn test_is_zero_address_alternating_pattern() {
        let addr_aa = [0xAA; 32];
        assert!(!is_zero_address(&addr_aa));
        let addr_55 = [0x55; 32];
        assert!(!is_zero_address(&addr_55));
    }
}
