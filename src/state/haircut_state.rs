use bytemuck::{Pod, Zeroable};

/// Per-market aggregate haircut state for the conservative `re_settle` solver (88 bytes).
///
/// Stores the sum of linearised `(weight, offset)` contributions across all
/// positions that have a pending haircut claim.  `re_settle` uses these to
/// compute the maximum settlement factor that keeps total obligations ≤ vault:
///
/// ```text
/// new_sf = WAD * (vault + offset_sum) / (remaining_normalized + weight_sum)
/// ```
///
/// Derived via `[SEED_HAIRCUT_STATE, market_pubkey]`.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct HaircutState {
    /// 8-byte account discriminator (must be DISC_HAIRCUT_STATE).
    pub discriminator: [u8; 8],
    /// Account schema version (1 byte).
    pub version: u8,
    /// Market this state belongs to (32 bytes).
    pub market: [u8; 32],
    /// Sum of per-position `weight = ceil(owed * WAD / (WAD - anchor))` (16 bytes, LE u128).
    pub claim_weight_sum: [u8; 16],
    /// Sum of per-position `offset = floor(weight * anchor / WAD)` (16 bytes, LE u128).
    pub claim_offset_sum: [u8; 16],
    /// PDA bump.
    pub bump: u8,
    /// Reserved.
    pub padding: [u8; 14],
}

impl HaircutState {
    pub fn claim_weight_sum(&self) -> u128 {
        u128::from_le_bytes(self.claim_weight_sum)
    }
    pub fn set_claim_weight_sum(&mut self, val: u128) {
        self.claim_weight_sum = val.to_le_bytes();
    }

    pub fn claim_offset_sum(&self) -> u128 {
        u128::from_le_bytes(self.claim_offset_sum)
    }
    pub fn set_claim_offset_sum(&mut self, val: u128) {
        self.claim_offset_sum = val.to_le_bytes();
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use core::mem::size_of;

    #[test]
    fn haircut_state_size() {
        assert_eq!(size_of::<HaircutState>(), 88);
    }

    #[test]
    fn haircut_state_zeroed() {
        let h = HaircutState::zeroed();
        assert_eq!(h.claim_weight_sum(), 0);
        assert_eq!(h.claim_offset_sum(), 0);
        assert_eq!(h.market, [0u8; 32]);
        assert_eq!(h.bump, 0);
    }

    #[test]
    fn haircut_state_weight_sum_roundtrip() {
        let mut h = HaircutState::zeroed();
        for val in [0u128, 1, 1_000_000_000_000_000_000u128, u128::MAX] {
            h.set_claim_weight_sum(val);
            assert_eq!(h.claim_weight_sum(), val);
        }
    }

    #[test]
    fn haircut_state_offset_sum_roundtrip() {
        let mut h = HaircutState::zeroed();
        for val in [0u128, 1, 1_000_000_000_000_000_000u128, u128::MAX] {
            h.set_claim_offset_sum(val);
            assert_eq!(h.claim_offset_sum(), val);
        }
    }

    #[test]
    fn haircut_state_fields_independent() {
        let mut h = HaircutState::zeroed();
        h.set_claim_weight_sum(500_000);
        h.set_claim_offset_sum(250_000);
        h.market = [0xAA; 32];
        h.bump = 42;
        assert_eq!(h.claim_weight_sum(), 500_000);
        assert_eq!(h.claim_offset_sum(), 250_000);
        assert_eq!(h.market, [0xAA; 32]);
        assert_eq!(h.bump, 42);
    }

    #[test]
    fn haircut_state_padding_zeroed() {
        let h = HaircutState::zeroed();
        assert_eq!(h.padding, [0u8; 14]);
    }

    #[test]
    fn haircut_state_bytemuck_cast_roundtrip() {
        let mut h = HaircutState::zeroed();
        h.discriminator = *b"COALHCST";
        h.version = 1;
        h.market = [0xBB; 32];
        h.set_claim_weight_sum(999_999);
        h.set_claim_offset_sum(123_456);
        h.bump = 254;

        let bytes: &[u8; 88] = bytemuck::bytes_of(&h).try_into().unwrap();
        let h2: &HaircutState = bytemuck::from_bytes(bytes);

        assert_eq!(h2.discriminator, *b"COALHCST");
        assert_eq!(h2.version, 1);
        assert_eq!(h2.market, [0xBB; 32]);
        assert_eq!(h2.claim_weight_sum(), 999_999);
        assert_eq!(h2.claim_offset_sum(), 123_456);
        assert_eq!(h2.bump, 254);
    }
}
