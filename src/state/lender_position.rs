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
    /// Reserved.
    pub padding: [u8; 38],
}

impl LenderPosition {
    pub fn scaled_balance(&self) -> u128 {
        u128::from_le_bytes(self.scaled_balance)
    }
    pub fn set_scaled_balance(&mut self, val: u128) {
        self.scaled_balance = val.to_le_bytes();
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
}
