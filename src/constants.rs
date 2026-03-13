/// Fixed-point precision constant = 1e18.
pub const WAD: u128 = 1_000_000_000_000_000_000;

/// Basis points denominator = 10,000.
pub const BPS: u128 = 10_000;

/// Seconds in a 365-day year.
pub const SECONDS_PER_YEAR: u128 = 31_536_000;

/// Maximum annual interest rate in basis points (100%).
pub const MAX_ANNUAL_INTEREST_BPS: u16 = 10_000;

/// Maximum protocol fee rate in basis points (100%).
pub const MAX_FEE_RATE_BPS: u16 = 10_000;

/// Expected decimals for the USDC mint.
pub const USDC_DECIMALS: u8 = 6;

/// USDC mint address on mainnet (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v).
/// COAL-L01: Hardcoded to enforce USDC-only markets.
pub const USDC_MINT: [u8; 32] = [
    198, 250, 122, 243, 190, 219, 173, 58, 61, 101, 243, 106, 171, 201, 116, 49, 177, 187, 228,
    194, 210, 246, 224, 228, 124, 166, 2, 3, 69, 47, 93, 97,
];

/// Minimum seconds until maturity at market creation.
pub const MIN_MATURITY_DELTA: i64 = 60;

/// Maximum seconds until maturity at market creation (5 years).
pub const MAX_MATURITY_DELTA: i64 = 5 * 365 * 24 * 60 * 60;

/// Settlement grace period in seconds (prevents front-running settlement).
/// First withdrawal after maturity must wait this long before settlement factor is locked.
pub const SETTLEMENT_GRACE_PERIOD: i64 = 300; // 5 minutes

// --- Account discriminators ---

pub const DISC_PROTOCOL_CONFIG: [u8; 8] = *b"COALPC__";
pub const DISC_MARKET: [u8; 8] = *b"COALMKT_";
pub const DISC_LENDER_POSITION: [u8; 8] = *b"COALLPOS";
pub const DISC_BORROWER_WL: [u8; 8] = *b"COALBWL_";

// --- PDA seeds ---

pub const SEED_PROTOCOL_CONFIG: &[u8] = b"protocol_config";
pub const SEED_MARKET: &[u8] = b"market";
pub const SEED_MARKET_AUTHORITY: &[u8] = b"market_authority";
pub const SEED_LENDER: &[u8] = b"lender";
pub const SEED_VAULT: &[u8] = b"vault";
pub const SEED_BORROWER_WHITELIST: &[u8] = b"borrower_whitelist";
pub const SEED_BLACKLIST: &[u8] = b"blacklist";

// --- Account sizes ---

pub const PROTOCOL_CONFIG_SIZE: usize = 194;
pub const MARKET_SIZE: usize = 250;
pub const LENDER_POSITION_SIZE: usize = 128;
pub const BORROWER_WHITELIST_SIZE: usize = 96;

/// SPL Token account size (fixed).
pub const SPL_TOKEN_ACCOUNT_SIZE: u64 = 165;

/// The system program address (all zeros except last byte = 0).
pub const SYSTEM_PROGRAM_ID: [u8; 32] = [0u8; 32];

/// Zero address (all zeros).
pub const ZERO_ADDRESS: [u8; 32] = [0u8; 32];

/// BPF Loader Upgradeable program ID: BPFLoaderUpgradeab1e11111111111111111111111
pub const BPF_LOADER_UPGRADEABLE_ID: [u8; 32] = [
    2, 168, 246, 145, 78, 136, 161, 176, 226, 16, 21, 62, 247, 99, 174, 43, 0, 194, 185, 61, 22,
    193, 36, 210, 192, 83, 122, 16, 4, 128, 0, 0,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wad_value() {
        assert_eq!(WAD, 1_000_000_000_000_000_000u128);
        assert_eq!(WAD, 10u128.pow(18));
    }

    #[test]
    fn bps_value() {
        assert_eq!(BPS, 10_000u128);
    }

    #[test]
    fn seconds_per_year_value() {
        assert_eq!(SECONDS_PER_YEAR, 365 * 24 * 60 * 60);
    }

    #[test]
    fn max_annual_interest_bps_value() {
        assert_eq!(MAX_ANNUAL_INTEREST_BPS, 10_000u16);
    }

    #[test]
    fn max_fee_rate_bps_value() {
        assert_eq!(MAX_FEE_RATE_BPS, 10_000u16);
    }

    #[test]
    fn usdc_decimals_value() {
        assert_eq!(USDC_DECIMALS, 6u8);
    }

    #[test]
    fn usdc_mint_value() {
        // EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
        assert_eq!(USDC_MINT.len(), 32);
        assert_eq!(USDC_MINT[0], 198);
        assert_eq!(USDC_MINT[31], 97);
    }

    #[test]
    fn min_maturity_delta_value() {
        assert_eq!(MIN_MATURITY_DELTA, 60i64);
    }

    #[test]
    fn settlement_grace_period_value() {
        assert_eq!(SETTLEMENT_GRACE_PERIOD, 300i64); // 5 minutes
    }

    #[test]
    fn account_sizes() {
        assert_eq!(PROTOCOL_CONFIG_SIZE, 194);
        assert_eq!(MARKET_SIZE, 250);
        assert_eq!(LENDER_POSITION_SIZE, 128);
        assert_eq!(BORROWER_WHITELIST_SIZE, 96);
        assert_eq!(SPL_TOKEN_ACCOUNT_SIZE, 165);
    }

    #[test]
    fn zero_address_is_all_zeros() {
        assert_eq!(ZERO_ADDRESS, [0u8; 32]);
        assert_eq!(SYSTEM_PROGRAM_ID, [0u8; 32]);
    }

    #[test]
    fn pda_seeds_are_expected_values() {
        assert_eq!(SEED_PROTOCOL_CONFIG, b"protocol_config");
        assert_eq!(SEED_MARKET, b"market");
        assert_eq!(SEED_MARKET_AUTHORITY, b"market_authority");
        assert_eq!(SEED_LENDER, b"lender");
        assert_eq!(SEED_VAULT, b"vault");
        assert_eq!(SEED_BORROWER_WHITELIST, b"borrower_whitelist");
        assert_eq!(SEED_BLACKLIST, b"blacklist");
    }
}
