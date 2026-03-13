use bytemuck::{Pod, Zeroable};

/// Per-market configuration and state (250 bytes).
/// All multi-byte fields stored as `[u8; N]` for bytemuck alignment safety.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Market {
    /// 8-byte account discriminator (must be DISC_MARKET).
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
    /// Fixed annual rate in bps (2 bytes, little-endian).
    pub annual_interest_bps: [u8; 2],
    /// Unix timestamp of loan maturity (8 bytes, little-endian i64).
    pub maturity_timestamp: [u8; 8],
    /// Borrow cap — normalized USDC, 6 decimals (8 bytes, little-endian u64).
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
    /// Cumulative haircut gap from early withdrawals in distressed markets (COAL-H01).
    /// Subtracted from available_for_lenders in re_settle to prevent recycled
    /// haircut tokens from inflating the settlement factor.
    pub haircut_accumulator: [u8; 8],
    /// Reserved.
    pub padding: [u8; 13],
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

    pub fn haircut_accumulator(&self) -> u64 {
        u64::from_le_bytes(self.haircut_accumulator)
    }
    pub fn set_haircut_accumulator(&mut self, val: u64) {
        self.haircut_accumulator = val.to_le_bytes();
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use core::mem::size_of;

    #[test]
    fn market_size() {
        assert_eq!(size_of::<Market>(), 250);
    }

    #[test]
    fn market_zeroed_all_fields_zero() {
        let m = Market::zeroed();
        assert_eq!(m.annual_interest_bps(), 0);
        assert_eq!(m.maturity_timestamp(), 0);
        assert_eq!(m.max_total_supply(), 0);
        assert_eq!(m.market_nonce(), 0);
        assert_eq!(m.scaled_total_supply(), 0);
        assert_eq!(m.scale_factor(), 0);
        assert_eq!(m.accrued_protocol_fees(), 0);
        assert_eq!(m.total_deposited(), 0);
        assert_eq!(m.total_borrowed(), 0);
        assert_eq!(m.total_repaid(), 0);
        assert_eq!(m.total_interest_repaid(), 0);
        assert_eq!(m.last_accrual_timestamp(), 0);
        assert_eq!(m.settlement_factor_wad(), 0);
        assert_eq!(m.haircut_accumulator(), 0);
    }

    #[test]
    fn market_annual_interest_bps_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u16, 1, 500, 10_000, u16::MAX] {
            m.set_annual_interest_bps(val);
            assert_eq!(m.annual_interest_bps(), val);
        }
    }

    #[test]
    fn market_maturity_timestamp_roundtrip() {
        let mut m = Market::zeroed();
        for val in [i64::MIN, -1, 0, 1, 1_000_000_000, i64::MAX] {
            m.set_maturity_timestamp(val);
            assert_eq!(m.maturity_timestamp(), val);
        }
    }

    #[test]
    fn market_max_total_supply_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, 1_000_000, u64::MAX] {
            m.set_max_total_supply(val);
            assert_eq!(m.max_total_supply(), val);
        }
    }

    #[test]
    fn market_nonce_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, u64::MAX] {
            m.set_market_nonce(val);
            assert_eq!(m.market_nonce(), val);
        }
    }

    #[test]
    fn market_scaled_total_supply_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u128, 1, u128::MAX] {
            m.set_scaled_total_supply(val);
            assert_eq!(m.scaled_total_supply(), val);
        }
    }

    #[test]
    fn market_scale_factor_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u128, 1, 1_000_000_000_000_000_000u128, u128::MAX] {
            m.set_scale_factor(val);
            assert_eq!(m.scale_factor(), val);
        }
    }

    #[test]
    fn market_accrued_protocol_fees_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, u64::MAX] {
            m.set_accrued_protocol_fees(val);
            assert_eq!(m.accrued_protocol_fees(), val);
        }
    }

    #[test]
    fn market_total_deposited_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, u64::MAX] {
            m.set_total_deposited(val);
            assert_eq!(m.total_deposited(), val);
        }
    }

    #[test]
    fn market_total_borrowed_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, u64::MAX] {
            m.set_total_borrowed(val);
            assert_eq!(m.total_borrowed(), val);
        }
    }

    #[test]
    fn market_total_repaid_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, u64::MAX] {
            m.set_total_repaid(val);
            assert_eq!(m.total_repaid(), val);
        }
    }

    #[test]
    fn market_total_interest_repaid_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, u64::MAX] {
            m.set_total_interest_repaid(val);
            assert_eq!(m.total_interest_repaid(), val);
        }
    }

    #[test]
    fn market_last_accrual_timestamp_roundtrip() {
        let mut m = Market::zeroed();
        for val in [i64::MIN, 0, i64::MAX] {
            m.set_last_accrual_timestamp(val);
            assert_eq!(m.last_accrual_timestamp(), val);
        }
    }

    #[test]
    fn market_settlement_factor_wad_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u128, 1, 1_000_000_000_000_000_000u128, u128::MAX] {
            m.set_settlement_factor_wad(val);
            assert_eq!(m.settlement_factor_wad(), val);
        }
    }

    #[test]
    fn market_fields_independent() {
        let mut m = Market::zeroed();
        m.set_annual_interest_bps(1000);
        m.set_maturity_timestamp(999);
        m.set_scale_factor(42);
        m.set_total_deposited(100);
        m.set_total_interest_repaid(500);
        // Verify setting one field doesn't corrupt others
        assert_eq!(m.annual_interest_bps(), 1000);
        assert_eq!(m.maturity_timestamp(), 999);
        assert_eq!(m.scale_factor(), 42);
        assert_eq!(m.total_deposited(), 100);
        assert_eq!(m.total_interest_repaid(), 500);
        assert_eq!(m.scaled_total_supply(), 0);
        assert_eq!(m.accrued_protocol_fees(), 0);
        assert_eq!(m.total_repaid(), 0);
    }

    // Discriminator set at correct offset (bytes 0..8)
    #[test]
    fn market_discriminator_at_correct_offset() {
        let mut m = Market::zeroed();
        m.discriminator = *b"COALMKT_";
        let bytes: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
        assert_eq!(&bytes[0..8], b"COALMKT_");
    }

    // Cast Market to [u8; 250] and back, all fields preserved
    #[test]
    fn market_bytemuck_cast_roundtrip() {
        let mut m = Market::zeroed();
        m.discriminator = *b"COALMKT_";
        m.version = 1;
        m.set_annual_interest_bps(1234);
        m.set_maturity_timestamp(999_999);
        m.set_scale_factor(42_000);
        m.set_total_borrowed(500_000);
        m.bump = 254;

        let bytes: &[u8; 250] = bytemuck::bytes_of(&m).try_into().unwrap();
        let m2: &Market = bytemuck::from_bytes(bytes);

        assert_eq!(m2.discriminator, *b"COALMKT_");
        assert_eq!(m2.version, 1);
        assert_eq!(m2.annual_interest_bps(), 1234);
        assert_eq!(m2.maturity_timestamp(), 999_999);
        assert_eq!(m2.scale_factor(), 42_000);
        assert_eq!(m2.total_borrowed(), 500_000);
        assert_eq!(m2.bump, 254);
    }

    // Zeroed Market has all padding bytes zero
    #[test]
    fn market_padding_zeroed() {
        let m = Market::zeroed();
        assert_eq!(m.haircut_accumulator, [0u8; 8]);
        assert_eq!(m.padding, [0u8; 13]);
    }

    #[test]
    fn market_haircut_accumulator_roundtrip() {
        let mut m = Market::zeroed();
        for val in [0u64, 1, 1_000_000, u64::MAX] {
            m.set_haircut_accumulator(val);
            assert_eq!(m.haircut_accumulator(), val);
        }
    }
}
