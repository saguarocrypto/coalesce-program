use bytemuck::{Pod, Zeroable};

/// Per-lender, per-market balance tracking (128 bytes).
/// All multi-byte fields stored as `[u8; N]` for bytemuck alignment safety.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct LenderPosition {
    /// 8-byte account discriminator (must be DISC_LENDER_POSITION).
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
    /// COAL-H01: Token amount the lender was shorted during distressed withdrawal.
    /// Zero means no pending haircut claim.
    pub haircut_owed: [u8; 8],
    /// COAL-H01: Settlement factor (WAD-scaled) at which the lender last withdrew
    /// or claimed. Used to compute proportional recovery on SF improvement.
    pub withdrawal_sf: [u8; 16],
    /// Reserved.
    pub padding: [u8; 14],
}

impl LenderPosition {
    pub fn scaled_balance(&self) -> u128 {
        u128::from_le_bytes(self.scaled_balance)
    }
    pub fn set_scaled_balance(&mut self, val: u128) {
        self.scaled_balance = val.to_le_bytes();
    }

    pub fn haircut_owed(&self) -> u64 {
        u64::from_le_bytes(self.haircut_owed)
    }
    pub fn set_haircut_owed(&mut self, val: u64) {
        self.haircut_owed = val.to_le_bytes();
    }

    pub fn withdrawal_sf(&self) -> u128 {
        u128::from_le_bytes(self.withdrawal_sf)
    }
    pub fn set_withdrawal_sf(&mut self, val: u128) {
        self.withdrawal_sf = val.to_le_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use core::mem::size_of;

    #[test]
    fn lender_position_size() {
        assert_eq!(size_of::<LenderPosition>(), 128);
    }

    #[test]
    fn lender_position_zeroed() {
        let p = LenderPosition::zeroed();
        assert_eq!(p.scaled_balance(), 0);
        assert_eq!(p.haircut_owed(), 0);
        assert_eq!(p.withdrawal_sf(), 0);
        assert_eq!(p.market, [0u8; 32]);
        assert_eq!(p.lender, [0u8; 32]);
        assert_eq!(p.bump, 0);
    }

    #[test]
    fn lender_position_scaled_balance_roundtrip() {
        let mut p = LenderPosition::zeroed();
        for val in [0u128, 1, 1_000_000_000_000_000_000u128, u128::MAX] {
            p.set_scaled_balance(val);
            assert_eq!(p.scaled_balance(), val);
        }
    }

    #[test]
    fn lender_position_balance_does_not_corrupt_lender() {
        let mut p = LenderPosition::zeroed();
        p.lender = [0xBB; 32];
        p.set_scaled_balance(999_999);
        assert_eq!(p.lender, [0xBB; 32]);
        assert_eq!(p.scaled_balance(), 999_999);
    }

    #[test]
    fn lender_position_haircut_owed_roundtrip() {
        let mut p = LenderPosition::zeroed();
        for val in [0u64, 1, 1_000_000, u64::MAX] {
            p.set_haircut_owed(val);
            assert_eq!(p.haircut_owed(), val);
        }
    }

    #[test]
    fn lender_position_withdrawal_sf_roundtrip() {
        let mut p = LenderPosition::zeroed();
        for val in [0u128, 1, 1_000_000_000_000_000_000u128, u128::MAX] {
            p.set_withdrawal_sf(val);
            assert_eq!(p.withdrawal_sf(), val);
        }
    }

    #[test]
    fn lender_position_haircut_fields_do_not_corrupt_existing() {
        let mut p = LenderPosition::zeroed();
        p.set_scaled_balance(999_999);
        p.bump = 42;
        p.lender = [0xBB; 32];
        p.set_haircut_owed(500_000);
        p.set_withdrawal_sf(500_000_000_000_000_000);
        assert_eq!(p.scaled_balance(), 999_999);
        assert_eq!(p.bump, 42);
        assert_eq!(p.lender, [0xBB; 32]);
        assert_eq!(p.haircut_owed(), 500_000);
        assert_eq!(p.withdrawal_sf(), 500_000_000_000_000_000);
    }
}
