use bytemuck::{Pod, Zeroable};

/// Singleton global configuration for the protocol (194 bytes).
/// All multi-byte fields stored as `[u8; N]` for bytemuck alignment safety.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct ProtocolConfig {
    /// 8-byte account discriminator (must be DISC_PROTOCOL_CONFIG).
    pub discriminator: [u8; 8],
    /// Account schema version (1 byte).
    pub version: u8,
    /// Protocol Admin pubkey (32 bytes).
    pub admin: [u8; 32],
    /// Protocol fee as bps of base interest (2 bytes, little-endian).
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
    /// Fail-open: non-existent blacklist accounts are treated as not blacklisted.
    /// Fail-closed: non-existent blacklist accounts are treated as blacklisted.
    pub blacklist_mode: u8,
    /// Reserved for future use.
    pub padding: [u8; 51],
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

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = u8::from(paused);
    }

    /// Returns true if blacklist mode is fail-closed (non-existent = blacklisted).
    pub fn is_blacklist_fail_closed(&self) -> bool {
        self.blacklist_mode != 0
    }

    /// Set blacklist mode: true = fail-closed, false = fail-open (default).
    pub fn set_blacklist_mode(&mut self, fail_closed: bool) {
        self.blacklist_mode = u8::from(fail_closed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use core::mem::size_of;

    #[test]
    fn protocol_config_size() {
        assert_eq!(size_of::<ProtocolConfig>(), 194);
    }

    #[test]
    fn protocol_config_zeroed() {
        let c = ProtocolConfig::zeroed();
        assert_eq!(c.fee_rate_bps(), 0);
        assert_eq!(c.is_initialized, 0);
        assert_eq!(c.bump, 0);
        assert_eq!(c.paused, 0);
        assert!(!c.is_paused());
        assert_eq!(c.admin, [0u8; 32]);
        assert_eq!(c.fee_authority, [0u8; 32]);
        assert_eq!(c.whitelist_manager, [0u8; 32]);
        assert_eq!(c.blacklist_program, [0u8; 32]);
    }

    #[test]
    fn protocol_config_paused_roundtrip() {
        let mut c = ProtocolConfig::zeroed();
        assert!(!c.is_paused());
        c.set_paused(true);
        assert!(c.is_paused());
        assert_eq!(c.paused, 1);
        c.set_paused(false);
        assert!(!c.is_paused());
        assert_eq!(c.paused, 0);
    }

    #[test]
    fn protocol_config_blacklist_mode_roundtrip() {
        let mut c = ProtocolConfig::zeroed();
        assert!(!c.is_blacklist_fail_closed());
        c.set_blacklist_mode(true);
        assert!(c.is_blacklist_fail_closed());
        assert_eq!(c.blacklist_mode, 1);
        c.set_blacklist_mode(false);
        assert!(!c.is_blacklist_fail_closed());
        assert_eq!(c.blacklist_mode, 0);
    }

    #[test]
    fn protocol_config_fee_rate_roundtrip() {
        let mut c = ProtocolConfig::zeroed();
        for val in [0u16, 1, 500, 5000, 10_000, u16::MAX] {
            c.set_fee_rate_bps(val);
            assert_eq!(c.fee_rate_bps(), val);
        }
    }

    #[test]
    fn protocol_config_fee_rate_does_not_corrupt_admin() {
        let mut c = ProtocolConfig::zeroed();
        c.admin = [0xAA; 32];
        c.set_fee_rate_bps(9999);
        assert_eq!(c.admin, [0xAA; 32]);
        assert_eq!(c.fee_rate_bps(), 9999);
    }

    // Non-zero paused values are all truthy
    #[test]
    fn protocol_config_paused_nonzero_values_truthy() {
        let mut c = ProtocolConfig::zeroed();
        c.paused = 2;
        assert!(c.is_paused());
        c.paused = 255;
        assert!(c.is_paused());
    }

    // Toggle pause does not corrupt fee_rate
    #[test]
    fn protocol_config_set_paused_does_not_corrupt_fee_rate() {
        let mut c = ProtocolConfig::zeroed();
        c.set_fee_rate_bps(5000);
        c.set_paused(true);
        assert_eq!(c.fee_rate_bps(), 5000);
        c.set_paused(false);
        assert_eq!(c.fee_rate_bps(), 5000);
    }

    // Toggle pause does not corrupt blacklist_mode
    #[test]
    fn protocol_config_set_paused_does_not_corrupt_blacklist_mode() {
        let mut c = ProtocolConfig::zeroed();
        c.set_blacklist_mode(true);
        c.set_paused(true);
        assert!(c.is_blacklist_fail_closed());
        c.set_paused(false);
        assert!(c.is_blacklist_fail_closed());
    }
}
