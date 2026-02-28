use bytemuck::{Pod, Zeroable};

/// Protocol-level borrower whitelist entry (96 bytes).
/// All multi-byte fields stored as `[u8; N]` for bytemuck alignment safety.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct BorrowerWhitelist {
    /// 8-byte account discriminator (must be DISC_BORROWER_WL).
    pub discriminator: [u8; 8],
    /// Account schema version (1 byte).
    pub version: u8,
    /// Borrower's wallet address (32 bytes).
    pub borrower: [u8; 32],
    /// 1 = whitelisted, 0 = removed.
    pub is_whitelisted: u8,
    /// Maximum USDC that can be outstanding at any time (8 bytes, little-endian u64).
    /// This is NOT a lifetime cap - borrower can re-borrow after repaying.
    pub max_borrow_capacity: [u8; 8],
    /// Current outstanding USDC debt across all markets (8 bytes, little-endian u64).
    /// Incremented on borrow, decremented on repay.
    pub current_borrowed: [u8; 8],
    /// PDA bump.
    pub bump: u8,
    /// Reserved.
    pub padding: [u8; 37],
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

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use core::mem::size_of;

    #[test]
    fn borrower_whitelist_size() {
        assert_eq!(size_of::<BorrowerWhitelist>(), 96);
    }

    #[test]
    fn borrower_whitelist_zeroed() {
        let w = BorrowerWhitelist::zeroed();
        assert_eq!(w.max_borrow_capacity(), 0);
        assert_eq!(w.current_borrowed(), 0);
        assert_eq!(w.is_whitelisted, 0);
        assert_eq!(w.borrower, [0u8; 32]);
        assert_eq!(w.bump, 0);
    }

    #[test]
    fn borrower_whitelist_max_borrow_capacity_roundtrip() {
        let mut w = BorrowerWhitelist::zeroed();
        for val in [0u64, 1, 1_000_000, u64::MAX] {
            w.set_max_borrow_capacity(val);
            assert_eq!(w.max_borrow_capacity(), val);
        }
    }

    #[test]
    fn borrower_whitelist_current_borrowed_roundtrip() {
        let mut w = BorrowerWhitelist::zeroed();
        for val in [0u64, 1, u64::MAX] {
            w.set_current_borrowed(val);
            assert_eq!(w.current_borrowed(), val);
        }
    }

    #[test]
    fn borrower_whitelist_fields_independent() {
        let mut w = BorrowerWhitelist::zeroed();
        w.borrower = [0xCC; 32];
        w.is_whitelisted = 1;
        w.set_max_borrow_capacity(5_000_000);
        w.set_current_borrowed(1_000_000);
        assert_eq!(w.borrower, [0xCC; 32]);
        assert_eq!(w.is_whitelisted, 1);
        assert_eq!(w.max_borrow_capacity(), 5_000_000);
        assert_eq!(w.current_borrowed(), 1_000_000);
    }
}
