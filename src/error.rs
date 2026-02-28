use num_derive::FromPrimitive;
use pinocchio::error::ProgramError;

/// All error codes for the CoalesceFi lending protocol.
///
/// Error codes are organized by category for easier debugging and documentation.
/// Custom error offset is 0-based (no 6000 offset).
///
/// # Categories
///
/// | Range | Category           | Description                              |
/// |-------|--------------------|-----------------------------------------|
/// | 0-4   | Initialization     | Protocol/market setup errors            |
/// | 5-9   | Authorization      | Permission and access control errors    |
/// | 10-16 | Account Validation | PDA, owner, and account type errors     |
/// | 17-20 | Input Validation   | Invalid amounts, timestamps, etc.       |
/// | 21-27 | Balance/Capacity   | Insufficient funds or exceeded limits   |
/// | 28-35 | Market State       | Maturity, settlement, position errors   |
/// | 36-40 | Fee/Withdrawal     | Fee collection and excess withdrawal    |
/// | 41-42 | Operational        | Math overflow, slippage protection      |
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive)]
#[repr(u32)]
pub enum LendingError {
    // ═══════════════════════════════════════════════════════════════════════
    // INITIALIZATION ERRORS (0-4)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-001: ProtocolConfig already exists.
    AlreadyInitialized = 0,

    /// ERR-002: Fee rate exceeds maximum (10,000 bps = 100%).
    InvalidFeeRate = 1,

    /// ERR-003: max_total_supply is 0.
    InvalidCapacity = 2,

    /// ERR-004: Maturity timestamp not in future.
    InvalidMaturity = 3,

    /// ERR-005: Market PDA already initialized.
    MarketAlreadyExists = 4,

    // ═══════════════════════════════════════════════════════════════════════
    // AUTHORIZATION ERRORS (5-9)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-006: Signer does not have required authority.
    Unauthorized = 5,

    /// ERR-007: Borrower not in protocol BorrowerWhitelist.
    NotWhitelisted = 6,

    /// ERR-008: Address is on global blacklist.
    Blacklisted = 7,

    /// ERR-009: Protocol is paused; no operations allowed.
    ProtocolPaused = 8,

    /// ERR-010: Cannot blacklist borrower with outstanding debt.
    BorrowerHasActiveDebt = 9,

    // ═══════════════════════════════════════════════════════════════════════
    // ACCOUNT VALIDATION ERRORS (10-16)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-011: Required address is zero pubkey.
    InvalidAddress = 10,

    /// ERR-012: Mint validation failed (wrong mint or wrong decimals).
    InvalidMint = 11,

    /// ERR-013: Vault account doesn't match market or has wrong owner/mint.
    InvalidVault = 12,

    /// ERR-014: Account does not match expected PDA derivation.
    InvalidPDA = 13,

    /// ERR-015: Account not owned by expected program.
    InvalidAccountOwner = 14,

    /// ERR-016: Wrong token program passed.
    InvalidTokenProgram = 15,

    /// ERR-017: Token account owner does not match expected authority.
    InvalidTokenAccountOwner = 16,

    // ═══════════════════════════════════════════════════════════════════════
    // INPUT VALIDATION ERRORS (17-20)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-018: Amount parameter is 0.
    ZeroAmount = 17,

    /// ERR-019: Scaled amount rounds to zero.
    ZeroScaledAmount = 18,

    /// ERR-020: Scale factor is zero (invalid market state).
    InvalidScaleFactor = 19,

    /// ERR-021: Timestamp is invalid (effective_now < last_accrual).
    InvalidTimestamp = 20,

    // ═══════════════════════════════════════════════════════════════════════
    // BALANCE/CAPACITY ERRORS (21-27)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-022: Token account has insufficient balance.
    InsufficientBalance = 21,

    /// ERR-023: Requested scaled amount exceeds balance.
    InsufficientScaledBalance = 22,

    /// ERR-024: Lender has no scaled balance.
    NoBalance = 23,

    /// ERR-025: Nothing available to withdraw (vault empty).
    ZeroPayout = 24,

    /// ERR-026: Deposit would exceed max_total_supply.
    CapExceeded = 25,

    /// ERR-027: Borrow exceeds available vault funds (net of fee reservation).
    BorrowAmountTooHigh = 26,

    /// ERR-028: Borrow would exceed borrower's global max_borrow_capacity.
    GlobalCapacityExceeded = 27,

    // ═══════════════════════════════════════════════════════════════════════
    // MARKET STATE ERRORS (28-35)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-029: Operation not allowed after maturity.
    MarketMatured = 28,

    /// ERR-030: Withdrawal before maturity.
    NotMatured = 29,

    /// ERR-031: settlement_factor_wad == 0; market has not been settled yet.
    NotSettled = 30,

    /// ERR-032: New settlement factor is not strictly greater than current.
    SettlementNotImproved = 31,

    /// ERR-033: Settlement grace period has not elapsed yet.
    SettlementGracePeriod = 32,

    /// ERR-034: Settlement has not occurred (settlement_factor == 0).
    SettlementNotComplete = 33,

    /// ERR-035: scaled_balance != 0; lender position cannot be closed.
    PositionNotEmpty = 34,

    /// ERR-036: Repayment exceeds current borrowed amount.
    RepaymentExceedsDebt = 35,

    // ═══════════════════════════════════════════════════════════════════════
    // FEE/WITHDRAWAL ERRORS (36-40)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-037: No accrued protocol fees.
    NoFeesToCollect = 36,

    /// ERR-038: Fee collection blocked during market distress (settlement < 100%).
    FeeCollectionDuringDistress = 37,

    /// ERR-039: Fee collection blocked while lenders have pending withdrawals.
    LendersPendingWithdrawals = 38,

    /// ERR-040: Protocol fees have not been collected yet.
    FeesNotCollected = 39,

    /// ERR-041: No excess funds in vault to withdraw.
    NoExcessToWithdraw = 40,

    // ═══════════════════════════════════════════════════════════════════════
    // OPERATIONAL ERRORS (41-42)
    // ═══════════════════════════════════════════════════════════════════════
    /// ERR-042: Arithmetic overflow/underflow.
    MathOverflow = 41,

    /// ERR-043: Payout is below minimum specified by caller (slippage protection).
    PayoutBelowMinimum = 42,
}

impl From<LendingError> for ProgramError {
    fn from(e: LendingError) -> ProgramError {
        ProgramError::Custom(e as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensures the u32 discriminant of every error variant matches the spec.
    /// This prevents accidental reordering from breaking clients.
    #[test]
    fn error_code_stability() {
        // INITIALIZATION ERRORS (0-4)
        assert_eq!(LendingError::AlreadyInitialized as u32, 0);
        assert_eq!(LendingError::InvalidFeeRate as u32, 1);
        assert_eq!(LendingError::InvalidCapacity as u32, 2);
        assert_eq!(LendingError::InvalidMaturity as u32, 3);
        assert_eq!(LendingError::MarketAlreadyExists as u32, 4);

        // AUTHORIZATION ERRORS (5-9)
        assert_eq!(LendingError::Unauthorized as u32, 5);
        assert_eq!(LendingError::NotWhitelisted as u32, 6);
        assert_eq!(LendingError::Blacklisted as u32, 7);
        assert_eq!(LendingError::ProtocolPaused as u32, 8);
        assert_eq!(LendingError::BorrowerHasActiveDebt as u32, 9);

        // ACCOUNT VALIDATION ERRORS (10-16)
        assert_eq!(LendingError::InvalidAddress as u32, 10);
        assert_eq!(LendingError::InvalidMint as u32, 11);
        assert_eq!(LendingError::InvalidVault as u32, 12);
        assert_eq!(LendingError::InvalidPDA as u32, 13);
        assert_eq!(LendingError::InvalidAccountOwner as u32, 14);
        assert_eq!(LendingError::InvalidTokenProgram as u32, 15);
        assert_eq!(LendingError::InvalidTokenAccountOwner as u32, 16);

        // INPUT VALIDATION ERRORS (17-20)
        assert_eq!(LendingError::ZeroAmount as u32, 17);
        assert_eq!(LendingError::ZeroScaledAmount as u32, 18);
        assert_eq!(LendingError::InvalidScaleFactor as u32, 19);
        assert_eq!(LendingError::InvalidTimestamp as u32, 20);

        // BALANCE/CAPACITY ERRORS (21-27)
        assert_eq!(LendingError::InsufficientBalance as u32, 21);
        assert_eq!(LendingError::InsufficientScaledBalance as u32, 22);
        assert_eq!(LendingError::NoBalance as u32, 23);
        assert_eq!(LendingError::ZeroPayout as u32, 24);
        assert_eq!(LendingError::CapExceeded as u32, 25);
        assert_eq!(LendingError::BorrowAmountTooHigh as u32, 26);
        assert_eq!(LendingError::GlobalCapacityExceeded as u32, 27);

        // MARKET STATE ERRORS (28-35)
        assert_eq!(LendingError::MarketMatured as u32, 28);
        assert_eq!(LendingError::NotMatured as u32, 29);
        assert_eq!(LendingError::NotSettled as u32, 30);
        assert_eq!(LendingError::SettlementNotImproved as u32, 31);
        assert_eq!(LendingError::SettlementGracePeriod as u32, 32);
        assert_eq!(LendingError::SettlementNotComplete as u32, 33);
        assert_eq!(LendingError::PositionNotEmpty as u32, 34);
        assert_eq!(LendingError::RepaymentExceedsDebt as u32, 35);

        // FEE/WITHDRAWAL ERRORS (36-40)
        assert_eq!(LendingError::NoFeesToCollect as u32, 36);
        assert_eq!(LendingError::FeeCollectionDuringDistress as u32, 37);
        assert_eq!(LendingError::LendersPendingWithdrawals as u32, 38);
        assert_eq!(LendingError::FeesNotCollected as u32, 39);
        assert_eq!(LendingError::NoExcessToWithdraw as u32, 40);

        // OPERATIONAL ERRORS (41-42)
        assert_eq!(LendingError::MathOverflow as u32, 41);
        assert_eq!(LendingError::PayoutBelowMinimum as u32, 42);
    }

    /// Ensure total number of variants is 43 (0-42).
    #[test]
    fn error_variant_count() {
        // PayoutBelowMinimum is the last variant at index 42
        assert_eq!(LendingError::PayoutBelowMinimum as u32, 42);
    }

    /// Ensure From<LendingError> for ProgramError produces Custom(N).
    #[test]
    fn error_into_program_error() {
        let pe: ProgramError = LendingError::Unauthorized.into();
        assert_eq!(pe, ProgramError::Custom(5));
    }

    #[test]
    fn error_debug_and_clone() {
        let e = LendingError::MathOverflow;
        let e2 = e;
        assert_eq!(e, e2);
        // Debug trait works
        let _ = format!("{:?}", e);
    }
}
