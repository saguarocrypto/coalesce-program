//! Off-chain mirror of on-chain CoalesceFi account structs.
//!
//! Layouts match `src/state/*.rs` exactly (same field order, sizes, and
//! byte-encoding conventions). All multi-byte numeric fields are stored as
//! little-endian `[u8; N]` arrays and exposed via accessor methods.

use bytemuck::{Pod, Zeroable};

// ---------------------------------------------------------------------------
// Constants (mirrors src/constants.rs)
// ---------------------------------------------------------------------------

/// Fixed-point precision constant = 1e18.
pub const WAD: u128 = 1_000_000_000_000_000_000;

/// Basis points denominator = 10,000.
pub const BPS: u128 = 10_000;

/// Seconds in a 365-day year.
pub const SECONDS_PER_YEAR: u128 = 31_536_000;

/// Account data sizes (raw account sizes, matching on-chain structs).
pub const MARKET_SIZE: usize = 250;
pub const PROTOCOL_CONFIG_SIZE: usize = 194;
pub const LENDER_POSITION_SIZE: usize = 128;
pub const BORROWER_WHITELIST_SIZE: usize = 96;

/// A 32-byte all-zeros address, used as the "zero / null" pubkey.
pub const ZERO_ADDRESS: [u8; 32] = [0u8; 32];

// ---------------------------------------------------------------------------
// Market (250 bytes)
// ---------------------------------------------------------------------------

/// Per-market configuration and state.
/// Layout mirrors `src/state/market.rs` exactly.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Market {
    /// 8-byte account discriminator.
    pub discriminator: [u8; 8],
    /// Account schema version (1 byte).
    pub version: u8,
    // --- Immutable after creation ---
    /// Borrower pubkey (32 bytes).
    pub borrower: [u8; 32],
    /// USDC mint pubkey (32 bytes).
    pub mint: [u8; 32],
    /// Vault PDA pubkey (32 bytes).
    pub vault: [u8; 32],
    /// PDA bump for market authority.
    pub market_authority_bump: u8,
    /// Fixed annual rate in bps (2 bytes, little-endian u16).
    pub annual_interest_bps: [u8; 2],
    /// Unix timestamp of loan maturity (8 bytes, little-endian i64).
    pub maturity_timestamp: [u8; 8],
    /// Borrow cap -- normalized USDC, 6 decimals (8 bytes, little-endian u64).
    pub max_total_supply: [u8; 8],
    /// Nonce for PDA derivation (8 bytes, little-endian u64).
    pub market_nonce: [u8; 8],

    // --- Mutable state ---
    /// Sum of all lender scaled balances (16 bytes, little-endian u128).
    pub scaled_total_supply: [u8; 16],
    /// WAD precision, monotonically increasing (16 bytes, little-endian u128).
    pub scale_factor: [u8; 16],
    /// Normalized USDC amount of uncollected fees (8 bytes, little-endian u64).
    pub accrued_protocol_fees: [u8; 8],
    /// Running total of normalized deposits (8 bytes, little-endian u64).
    pub total_deposited: [u8; 8],
    /// Running total borrowed (8 bytes, little-endian u64).
    pub total_borrowed: [u8; 8],
    /// Running total repaid (8 bytes, little-endian u64).
    pub total_repaid: [u8; 8],
    /// Running total interest repaid - does not affect borrower capacity (8 bytes, little-endian u64).
    pub total_interest_repaid: [u8; 8],
    /// Last interest accrual Unix timestamp (8 bytes, little-endian i64).
    pub last_accrual_timestamp: [u8; 8],
    /// 0 = unsettled; once set, payout ratio locked (16 bytes, little-endian u128).
    pub settlement_factor_wad: [u8; 16],
    /// Market PDA bump.
    pub bump: u8,
    /// Reserved.
    pub _padding: [u8; 21],
}

impl Market {
    pub fn annual_interest_bps(&self) -> u16 {
        u16::from_le_bytes(self.annual_interest_bps)
    }
    pub fn set_annual_interest_bps(&mut self, val: u16) {
        self.annual_interest_bps = val.to_le_bytes();
    }

    pub fn maturity_timestamp(&self) -> i64 {
        i64::from_le_bytes(self.maturity_timestamp)
    }
    pub fn set_maturity_timestamp(&mut self, val: i64) {
        self.maturity_timestamp = val.to_le_bytes();
    }

    pub fn max_total_supply(&self) -> u64 {
        u64::from_le_bytes(self.max_total_supply)
    }
    pub fn set_max_total_supply(&mut self, val: u64) {
        self.max_total_supply = val.to_le_bytes();
    }

    pub fn market_nonce(&self) -> u64 {
        u64::from_le_bytes(self.market_nonce)
    }
    pub fn set_market_nonce(&mut self, val: u64) {
        self.market_nonce = val.to_le_bytes();
    }

    pub fn scaled_total_supply(&self) -> u128 {
        u128::from_le_bytes(self.scaled_total_supply)
    }
    pub fn set_scaled_total_supply(&mut self, val: u128) {
        self.scaled_total_supply = val.to_le_bytes();
    }

    pub fn scale_factor(&self) -> u128 {
        u128::from_le_bytes(self.scale_factor)
    }
    pub fn set_scale_factor(&mut self, val: u128) {
        self.scale_factor = val.to_le_bytes();
    }

    pub fn accrued_protocol_fees(&self) -> u64 {
        u64::from_le_bytes(self.accrued_protocol_fees)
    }
    pub fn set_accrued_protocol_fees(&mut self, val: u64) {
        self.accrued_protocol_fees = val.to_le_bytes();
    }

    pub fn total_deposited(&self) -> u64 {
        u64::from_le_bytes(self.total_deposited)
    }
    pub fn set_total_deposited(&mut self, val: u64) {
        self.total_deposited = val.to_le_bytes();
    }

    pub fn total_borrowed(&self) -> u64 {
        u64::from_le_bytes(self.total_borrowed)
    }
    pub fn set_total_borrowed(&mut self, val: u64) {
        self.total_borrowed = val.to_le_bytes();
    }

    pub fn total_repaid(&self) -> u64 {
        u64::from_le_bytes(self.total_repaid)
    }
    pub fn set_total_repaid(&mut self, val: u64) {
        self.total_repaid = val.to_le_bytes();
    }

    pub fn total_interest_repaid(&self) -> u64 {
        u64::from_le_bytes(self.total_interest_repaid)
    }
    pub fn set_total_interest_repaid(&mut self, val: u64) {
        self.total_interest_repaid = val.to_le_bytes();
    }

    pub fn last_accrual_timestamp(&self) -> i64 {
        i64::from_le_bytes(self.last_accrual_timestamp)
    }
    pub fn set_last_accrual_timestamp(&mut self, val: i64) {
        self.last_accrual_timestamp = val.to_le_bytes();
    }

    pub fn settlement_factor_wad(&self) -> u128 {
        u128::from_le_bytes(self.settlement_factor_wad)
    }
    pub fn set_settlement_factor_wad(&mut self, val: u128) {
        self.settlement_factor_wad = val.to_le_bytes();
    }

    /// Returns `true` when the market has a non-zero scale factor, indicating
    /// it has been initialized and had at least one deposit.
    pub fn is_initialized(&self) -> bool {
        self.scale_factor() != 0
    }

    /// Returns `true` when settlement has occurred (factor > 0).
    pub fn is_settled(&self) -> bool {
        self.settlement_factor_wad() != 0
    }

    /// Returns `true` when there are outstanding borrows that have not been
    /// fully repaid.
    pub fn has_active_borrows(&self) -> bool {
        self.total_borrowed() > self.total_repaid()
    }
}

// ---------------------------------------------------------------------------
// ProtocolConfig (194 bytes)
// ---------------------------------------------------------------------------

/// Singleton global configuration for the protocol.
/// Layout mirrors `src/state/protocol_config.rs` exactly.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct ProtocolConfig {
    /// 8-byte account discriminator.
    pub discriminator: [u8; 8],
    /// Account schema version (1 byte).
    pub version: u8,
    /// Protocol Admin pubkey (32 bytes).
    pub admin: [u8; 32],
    /// Protocol fee as bps of base interest (2 bytes, little-endian u16).
    pub fee_rate_bps: [u8; 2],
    /// Wallet pubkey authorized to collect fees (32 bytes).
    pub fee_authority: [u8; 32],
    /// Keypair authorized to manage BorrowerWhitelist (32 bytes).
    pub whitelist_manager: [u8; 32],
    /// External program for blacklist lookups (32 bytes).
    pub blacklist_program: [u8; 32],
    /// Guard against double-init (1 = initialized).
    pub is_initialized: u8,
    /// PDA bump seed.
    pub bump: u8,
    /// Emergency pause flag (0 = active, 1 = paused).
    pub paused: u8,
    /// Blacklist mode (0 = fail-open, 1 = fail-closed).
    pub blacklist_mode: u8,
    /// Reserved for future use.
    pub _padding: [u8; 51],
}

impl ProtocolConfig {
    pub fn fee_rate_bps(&self) -> u16 {
        u16::from_le_bytes(self.fee_rate_bps)
    }
    pub fn set_fee_rate_bps(&mut self, val: u16) {
        self.fee_rate_bps = val.to_le_bytes();
    }

    pub fn is_paused(&self) -> bool {
        self.paused != 0
    }

    pub fn is_blacklist_fail_closed(&self) -> bool {
        self.blacklist_mode != 0
    }
}

// ---------------------------------------------------------------------------
// LenderPosition (128 bytes)
// ---------------------------------------------------------------------------

/// Per-lender, per-market balance tracking.
/// Layout mirrors `src/state/lender_position.rs` exactly.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct LenderPosition {
    /// 8-byte account discriminator.
    pub discriminator: [u8; 8],
    /// Account schema version (1 byte).
    pub version: u8,
    /// Market this position belongs to (32 bytes).
    pub market: [u8; 32],
    /// Lender's wallet address (32 bytes).
    pub lender: [u8; 32],
    /// Lender's scaled (share) balance (16 bytes, little-endian u128).
    pub scaled_balance: [u8; 16],
    /// PDA bump.
    pub bump: u8,
    /// Reserved.
    pub _padding: [u8; 38],
}

impl LenderPosition {
    pub fn scaled_balance(&self) -> u128 {
        u128::from_le_bytes(self.scaled_balance)
    }
    pub fn set_scaled_balance(&mut self, val: u128) {
        self.scaled_balance = val.to_le_bytes();
    }
}

// ---------------------------------------------------------------------------
// BorrowerWhitelist (96 bytes)
// ---------------------------------------------------------------------------

/// Protocol-level borrower whitelist entry.
/// Layout mirrors `src/state/borrower_whitelist.rs` exactly.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct BorrowerWhitelist {
    /// 8-byte account discriminator.
    pub discriminator: [u8; 8],
    /// Account schema version (1 byte).
    pub version: u8,
    /// Borrower's wallet address (32 bytes).
    pub borrower: [u8; 32],
    /// 1 = whitelisted, 0 = removed.
    pub is_whitelisted: u8,
    /// Maximum USDC that can be outstanding at any time (8 bytes, little-endian u64).
    pub max_borrow_capacity: [u8; 8],
    /// Current outstanding USDC debt across all markets (8 bytes, little-endian u64).
    /// Incremented on borrow, decremented on repay.
    pub current_borrowed: [u8; 8],
    /// PDA bump.
    pub bump: u8,
    /// Reserved.
    pub _padding: [u8; 37],
}

impl BorrowerWhitelist {
    pub fn max_borrow_capacity(&self) -> u64 {
        u64::from_le_bytes(self.max_borrow_capacity)
    }
    pub fn set_max_borrow_capacity(&mut self, val: u64) {
        self.max_borrow_capacity = val.to_le_bytes();
    }

    pub fn current_borrowed(&self) -> u64 {
        u64::from_le_bytes(self.current_borrowed)
    }
    pub fn set_current_borrowed(&mut self, val: u64) {
        self.current_borrowed = val.to_le_bytes();
    }
}

// ---------------------------------------------------------------------------
// Size assertions
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn struct_sizes_match_on_chain() {
        assert_eq!(size_of::<Market>(), MARKET_SIZE);
        assert_eq!(size_of::<ProtocolConfig>(), PROTOCOL_CONFIG_SIZE);
        assert_eq!(size_of::<LenderPosition>(), LENDER_POSITION_SIZE);
        assert_eq!(size_of::<BorrowerWhitelist>(), BORROWER_WHITELIST_SIZE);
    }

    #[test]
    fn market_roundtrip() {
        let mut m = Market::zeroed();
        m.set_scale_factor(WAD);
        m.set_scaled_total_supply(500_000_000_000_000_000_000);
        m.set_max_total_supply(1_000_000);
        m.set_accrued_protocol_fees(123);
        m.set_total_borrowed(500);
        m.set_total_repaid(200);
        m.set_total_interest_repaid(50);
        m.set_maturity_timestamp(1_700_000_000);
        m.set_last_accrual_timestamp(1_699_999_000);
        m.set_settlement_factor_wad(0);

        assert_eq!(m.scale_factor(), WAD);
        assert_eq!(m.scaled_total_supply(), 500_000_000_000_000_000_000);
        assert_eq!(m.max_total_supply(), 1_000_000);
        assert_eq!(m.accrued_protocol_fees(), 123);
        assert_eq!(m.total_borrowed(), 500);
        assert_eq!(m.total_repaid(), 200);
        assert_eq!(m.total_interest_repaid(), 50);
        assert!(m.is_initialized());
        assert!(!m.is_settled());
        assert!(m.has_active_borrows());
    }

    #[test]
    fn lender_position_roundtrip() {
        let mut lp = LenderPosition::zeroed();
        lp.set_scaled_balance(42_000_000_000_000_000_000);
        assert_eq!(lp.scaled_balance(), 42_000_000_000_000_000_000);
    }

    #[test]
    fn borrower_whitelist_roundtrip() {
        let mut bw = BorrowerWhitelist::zeroed();
        bw.set_max_borrow_capacity(1_000_000);
        bw.set_current_borrowed(500_000);
        assert_eq!(bw.max_borrow_capacity(), 1_000_000);
        assert_eq!(bw.current_borrowed(), 500_000);
    }
}
