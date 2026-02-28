//! Invariant checking logic for off-chain monitoring of the CoalesceFi
//! lending protocol.
//!
//! Every public function in this module checks a single protocol invariant and
//! returns `Ok(())` when the invariant holds, or `Err(InvariantViolation)` with
//! details when it is broken.

use std::collections::HashMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::types::{BorrowerWhitelist, LenderPosition, Market, WAD};

// ---------------------------------------------------------------------------
// Alert / violation types
// ---------------------------------------------------------------------------

/// Severity of an invariant violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Protocol funds are at risk (solvency, balance mismatches).
    Critical,
    /// Something is unusual and should be investigated (stale accruals,
    /// suspicious values near `u64::MAX`).
    Warning,
    /// Informational heartbeat or status update.
    Info,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Critical => write!(f, "CRITICAL"),
            Severity::Warning => write!(f, "WARNING"),
            Severity::Info => write!(f, "INFO"),
        }
    }
}

/// The kind of invariant that was violated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ViolationType {
    VaultInsolvency,
    ScaleFactorBelowWad,
    ScaleFactorDecreased,
    SettlementFactorOutOfBounds,
    SettlementFactorDecreased,
    SuspiciousAccruedFees,
    SupplyCapExceeded,
    LenderBalanceInconsistency,
    WhitelistCapacityExceeded,
    StaleAccrual,
}

impl fmt::Display for ViolationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            ViolationType::VaultInsolvency => "VaultInsolvency",
            ViolationType::ScaleFactorBelowWad => "ScaleFactorBelowWad",
            ViolationType::ScaleFactorDecreased => "ScaleFactorDecreased",
            ViolationType::SettlementFactorOutOfBounds => "SettlementFactorOutOfBounds",
            ViolationType::SettlementFactorDecreased => "SettlementFactorDecreased",
            ViolationType::SuspiciousAccruedFees => "SuspiciousAccruedFees",
            ViolationType::SupplyCapExceeded => "SupplyCapExceeded",
            ViolationType::LenderBalanceInconsistency => "LenderBalanceInconsistency",
            ViolationType::WhitelistCapacityExceeded => "WhitelistCapacityExceeded",
            ViolationType::StaleAccrual => "StaleAccrual",
        };
        write!(f, "{}", label)
    }
}

/// Detailed description of a single invariant violation.
#[derive(Debug, Clone)]
pub struct InvariantViolation {
    /// Which market (or account) the violation pertains to, as a base58 pubkey
    /// or hex string.
    pub market_pubkey: String,
    /// The kind of invariant that was broken.
    pub violation_type: ViolationType,
    /// Human-readable description of the expected condition.
    pub expected: String,
    /// Human-readable description of the actual observed value.
    pub actual: String,
    /// Unix timestamp when the violation was detected.
    pub timestamp: u64,
    /// Severity level.
    pub severity: Severity,
}

impl fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} on market {}: expected {}, got {} (at {})",
            self.severity,
            self.violation_type,
            self.market_pubkey,
            self.expected,
            self.actual,
            self.timestamp,
        )
    }
}

// ---------------------------------------------------------------------------
// Historical tracking (for monotonicity checks)
// ---------------------------------------------------------------------------

/// Holds previously observed values for monotonicity invariants.
#[derive(Debug, Default, Clone)]
pub struct MonitorState {
    /// Previous scale_factor per market (keyed by market pubkey bytes).
    pub prev_scale_factors: HashMap<[u8; 32], u128>,
    /// Previous settlement_factor_wad per market.
    pub prev_settlement_factors: HashMap<[u8; 32], u128>,
}

// ---------------------------------------------------------------------------
// Helper: current unix timestamp
// ---------------------------------------------------------------------------

/// Returns the current Unix timestamp in seconds. Falls back to 0 on error.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Individual invariant checks
// ---------------------------------------------------------------------------

/// **Vault solvency**: when there are no active borrows the vault token
/// balance must be at least as large as accrued protocol fees.
///
/// `vault_balance` is the on-chain SPL token balance of the market vault.
pub fn check_vault_solvency(
    market_pubkey: &str,
    market: &Market,
    vault_balance: u64,
) -> Result<(), InvariantViolation> {
    if !market.has_active_borrows() && vault_balance < market.accrued_protocol_fees() {
        return Err(InvariantViolation {
            market_pubkey: market_pubkey.to_string(),
            violation_type: ViolationType::VaultInsolvency,
            expected: format!(
                "vault_balance >= accrued_protocol_fees ({})",
                market.accrued_protocol_fees()
            ),
            actual: format!("vault_balance = {}", vault_balance),
            timestamp: now_unix(),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

/// **Scale factor validity**: for any initialized market, `scale_factor >= WAD`.
pub fn check_scale_factor_validity(
    market_pubkey: &str,
    market: &Market,
) -> Result<(), InvariantViolation> {
    if market.is_initialized() && market.scale_factor() < WAD {
        return Err(InvariantViolation {
            market_pubkey: market_pubkey.to_string(),
            violation_type: ViolationType::ScaleFactorBelowWad,
            expected: format!("scale_factor >= WAD ({})", WAD),
            actual: format!("scale_factor = {}", market.scale_factor()),
            timestamp: now_unix(),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

/// **Scale factor monotonicity**: the scale factor must never decrease between
/// observations.
pub fn check_scale_factor_monotonicity(
    market_pubkey: &str,
    market_key: &[u8; 32],
    market: &Market,
    state: &mut MonitorState,
) -> Result<(), InvariantViolation> {
    let current = market.scale_factor();
    if let Some(&prev) = state.prev_scale_factors.get(market_key) {
        if current < prev {
            let violation = InvariantViolation {
                market_pubkey: market_pubkey.to_string(),
                violation_type: ViolationType::ScaleFactorDecreased,
                expected: format!("scale_factor >= previous ({})", prev),
                actual: format!("scale_factor = {}", current),
                timestamp: now_unix(),
                severity: Severity::Critical,
            };
            state.prev_scale_factors.insert(*market_key, current);
            return Err(violation);
        }
    }
    state.prev_scale_factors.insert(*market_key, current);
    Ok(())
}

/// **Settlement factor bounds**: when non-zero, the settlement factor must
/// satisfy `1 <= settlement_factor_wad <= WAD`.
pub fn check_settlement_factor_bounds(
    market_pubkey: &str,
    market: &Market,
) -> Result<(), InvariantViolation> {
    let sf = market.settlement_factor_wad();
    if sf != 0 && (sf < 1 || sf > WAD) {
        return Err(InvariantViolation {
            market_pubkey: market_pubkey.to_string(),
            violation_type: ViolationType::SettlementFactorOutOfBounds,
            expected: format!("1 <= settlement_factor_wad <= WAD ({})", WAD),
            actual: format!("settlement_factor_wad = {}", sf),
            timestamp: now_unix(),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

/// **Settlement factor monotonicity**: the settlement factor must never
/// decrease between observations.
pub fn check_settlement_factor_monotonicity(
    market_pubkey: &str,
    market_key: &[u8; 32],
    market: &Market,
    state: &mut MonitorState,
) -> Result<(), InvariantViolation> {
    let current = market.settlement_factor_wad();
    if let Some(&prev) = state.prev_settlement_factors.get(market_key) {
        if current < prev {
            let violation = InvariantViolation {
                market_pubkey: market_pubkey.to_string(),
                violation_type: ViolationType::SettlementFactorDecreased,
                expected: format!("settlement_factor_wad >= previous ({})", prev),
                actual: format!("settlement_factor_wad = {}", current),
                timestamp: now_unix(),
                severity: Severity::Critical,
            };
            state.prev_settlement_factors.insert(*market_key, current);
            return Err(violation);
        }
    }
    state.prev_settlement_factors.insert(*market_key, current);
    Ok(())
}

/// **Fee non-negativity**: accrued_protocol_fees is u64 so it can never be
/// negative, but values suspiciously close to `u64::MAX` likely indicate
/// corruption or overflow.
///
/// The threshold defaults to `u64::MAX - 1_000_000_000_000` (about 999 billion
/// USDC with 6 decimals -- far beyond any realistic protocol TVL).
pub fn check_fee_non_negativity(
    market_pubkey: &str,
    market: &Market,
) -> Result<(), InvariantViolation> {
    const SUSPICIOUS_THRESHOLD: u64 = u64::MAX - 1_000_000_000_000;
    let fees = market.accrued_protocol_fees();
    if fees > SUSPICIOUS_THRESHOLD {
        return Err(InvariantViolation {
            market_pubkey: market_pubkey.to_string(),
            violation_type: ViolationType::SuspiciousAccruedFees,
            expected: format!("accrued_protocol_fees < {}", SUSPICIOUS_THRESHOLD),
            actual: format!("accrued_protocol_fees = {}", fees),
            timestamp: now_unix(),
            severity: Severity::Warning,
        });
    }
    Ok(())
}

/// **Supply cap respected**: the real (unscaled) total supply must not exceed
/// the market's `max_total_supply`.
///
/// real_supply = scaled_total_supply * scale_factor / WAD
pub fn check_supply_cap(
    market_pubkey: &str,
    market: &Market,
) -> Result<(), InvariantViolation> {
    if !market.is_initialized() {
        return Ok(());
    }
    let scaled = market.scaled_total_supply();
    let sf = market.scale_factor();
    // Use u128 arithmetic; scale_factor and scaled_total_supply are already u128.
    // Saturating mul: if the product overflows u128 it is certainly > cap.
    let product = scaled.saturating_mul(sf);
    let real_supply = product / WAD;
    let cap = u128::from(market.max_total_supply());

    if real_supply > cap {
        return Err(InvariantViolation {
            market_pubkey: market_pubkey.to_string(),
            violation_type: ViolationType::SupplyCapExceeded,
            expected: format!("real_supply <= max_total_supply ({})", cap),
            actual: format!("real_supply = {}", real_supply),
            timestamp: now_unix(),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

/// **Lender balance consistency**: the sum of all lender positions'
/// `scaled_balance` for a given market must equal the market's
/// `scaled_total_supply`.
///
/// `positions` should contain only the lender positions belonging to this
/// market.
pub fn check_lender_balance_consistency(
    market_pubkey: &str,
    market: &Market,
    positions: &[LenderPosition],
) -> Result<(), InvariantViolation> {
    let sum: u128 = positions.iter().map(|p| p.scaled_balance()).sum();
    let expected = market.scaled_total_supply();
    if sum != expected {
        return Err(InvariantViolation {
            market_pubkey: market_pubkey.to_string(),
            violation_type: ViolationType::LenderBalanceInconsistency,
            expected: format!("sum(scaled_balance) == scaled_total_supply ({})", expected),
            actual: format!("sum(scaled_balance) = {}", sum),
            timestamp: now_unix(),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

/// **Whitelist capacity**: `current_borrowed <= max_borrow_capacity`.
pub fn check_whitelist_capacity(
    whitelist_pubkey: &str,
    wl: &BorrowerWhitelist,
) -> Result<(), InvariantViolation> {
    if wl.current_borrowed() > wl.max_borrow_capacity() {
        return Err(InvariantViolation {
            market_pubkey: whitelist_pubkey.to_string(),
            violation_type: ViolationType::WhitelistCapacityExceeded,
            expected: format!(
                "current_borrowed <= max_borrow_capacity ({})",
                wl.max_borrow_capacity()
            ),
            actual: format!("current_borrowed = {}", wl.current_borrowed()),
            timestamp: now_unix(),
            severity: Severity::Critical,
        });
    }
    Ok(())
}

/// **Stale accrual detection**: alerts if the time since the last accrual
/// exceeds `threshold_secs`.
///
/// `current_unix` is the current Unix timestamp (seconds).
pub fn check_stale_accrual(
    market_pubkey: &str,
    market: &Market,
    current_unix: i64,
    threshold_secs: i64,
) -> Result<(), InvariantViolation> {
    // Only applicable to initialized markets that haven't been settled.
    if !market.is_initialized() || market.is_settled() {
        return Ok(());
    }
    let last = market.last_accrual_timestamp();
    if last == 0 {
        // Never accrued -- skip (market may have just been created).
        return Ok(());
    }
    let elapsed = current_unix.saturating_sub(last);
    if elapsed > threshold_secs {
        return Err(InvariantViolation {
            market_pubkey: market_pubkey.to_string(),
            violation_type: ViolationType::StaleAccrual,
            expected: format!(
                "time since last accrual <= {} secs",
                threshold_secs
            ),
            actual: format!("elapsed = {} secs (last_accrual = {})", elapsed, last),
            timestamp: now_unix(),
            severity: Severity::Warning,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Aggregate checker
// ---------------------------------------------------------------------------

/// Runs all per-market invariant checks and returns a list of violations.
///
/// - `market_pubkey`: base58 string of the market account address.
/// - `market_key`: raw 32-byte pubkey of the market.
/// - `market`: deserialized market state.
/// - `vault_balance`: on-chain SPL token balance of the market vault.
/// - `positions`: lender positions that belong to this market.
/// - `state`: mutable monitor state for monotonicity tracking.
/// - `current_unix`: current Unix timestamp.
/// - `stale_threshold_secs`: threshold for stale accrual alerts.
pub fn check_all_market_invariants(
    market_pubkey: &str,
    market_key: &[u8; 32],
    market: &Market,
    vault_balance: u64,
    positions: &[LenderPosition],
    state: &mut MonitorState,
    current_unix: i64,
    stale_threshold_secs: i64,
) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();

    let checks: Vec<Result<(), InvariantViolation>> = vec![
        check_vault_solvency(market_pubkey, market, vault_balance),
        check_scale_factor_validity(market_pubkey, market),
        check_scale_factor_monotonicity(market_pubkey, market_key, market, state),
        check_settlement_factor_bounds(market_pubkey, market),
        check_settlement_factor_monotonicity(market_pubkey, market_key, market, state),
        check_fee_non_negativity(market_pubkey, market),
        check_supply_cap(market_pubkey, market),
        check_lender_balance_consistency(market_pubkey, market, positions),
        check_stale_accrual(market_pubkey, market, current_unix, stale_threshold_secs),
    ];

    for result in checks {
        if let Err(v) = result {
            violations.push(v);
        }
    }

    violations
}

/// Runs the whitelist capacity check for a single whitelist account.
pub fn check_all_whitelist_invariants(
    whitelist_pubkey: &str,
    wl: &BorrowerWhitelist,
) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    if let Err(v) = check_whitelist_capacity(whitelist_pubkey, wl) {
        violations.push(v);
    }
    violations
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Market;
    use bytemuck::Zeroable;

    fn make_initialized_market() -> Market {
        let mut m = Market::zeroed();
        m.set_scale_factor(WAD);
        m.set_max_total_supply(1_000_000);
        m.set_last_accrual_timestamp(1_700_000_000);
        m
    }

    #[test]
    fn vault_solvency_passes_when_no_borrows_and_sufficient_balance() {
        let mut m = make_initialized_market();
        m.set_accrued_protocol_fees(100);
        assert!(check_vault_solvency("test", &m, 200).is_ok());
    }

    #[test]
    fn vault_solvency_fails_when_balance_below_fees() {
        let mut m = make_initialized_market();
        m.set_accrued_protocol_fees(100);
        let err = check_vault_solvency("test", &m, 50).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::VaultInsolvency);
        assert_eq!(err.severity, Severity::Critical);
    }

    #[test]
    fn vault_solvency_skipped_when_active_borrows() {
        let mut m = make_initialized_market();
        m.set_accrued_protocol_fees(100);
        m.set_total_borrowed(500);
        m.set_total_repaid(200);
        // Even with vault_balance = 0, no violation because borrows are active.
        assert!(check_vault_solvency("test", &m, 0).is_ok());
    }

    #[test]
    fn scale_factor_validity_passes() {
        let m = make_initialized_market();
        assert!(check_scale_factor_validity("test", &m).is_ok());
    }

    #[test]
    fn scale_factor_validity_fails_below_wad() {
        let mut m = Market::zeroed();
        m.set_scale_factor(WAD - 1);
        let err = check_scale_factor_validity("test", &m).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::ScaleFactorBelowWad);
    }

    #[test]
    fn scale_factor_validity_skips_uninitialized() {
        let m = Market::zeroed();
        assert!(check_scale_factor_validity("test", &m).is_ok());
    }

    #[test]
    fn scale_factor_monotonicity_passes_on_increase() {
        let key = [1u8; 32];
        let mut state = MonitorState::default();
        let mut m = make_initialized_market();
        m.set_scale_factor(WAD);
        assert!(check_scale_factor_monotonicity("test", &key, &m, &mut state).is_ok());
        m.set_scale_factor(WAD + 1);
        assert!(check_scale_factor_monotonicity("test", &key, &m, &mut state).is_ok());
    }

    #[test]
    fn scale_factor_monotonicity_fails_on_decrease() {
        let key = [1u8; 32];
        let mut state = MonitorState::default();
        let mut m = make_initialized_market();
        m.set_scale_factor(WAD + 100);
        assert!(check_scale_factor_monotonicity("test", &key, &m, &mut state).is_ok());
        m.set_scale_factor(WAD);
        let err = check_scale_factor_monotonicity("test", &key, &m, &mut state).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::ScaleFactorDecreased);
    }

    #[test]
    fn settlement_factor_bounds_passes() {
        let mut m = make_initialized_market();
        m.set_settlement_factor_wad(WAD / 2);
        assert!(check_settlement_factor_bounds("test", &m).is_ok());
    }

    #[test]
    fn settlement_factor_bounds_fails_above_wad() {
        let mut m = make_initialized_market();
        m.set_settlement_factor_wad(WAD + 1);
        let err = check_settlement_factor_bounds("test", &m).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::SettlementFactorOutOfBounds);
    }

    #[test]
    fn settlement_factor_bounds_passes_when_zero() {
        let m = make_initialized_market();
        assert!(check_settlement_factor_bounds("test", &m).is_ok());
    }

    #[test]
    fn settlement_factor_monotonicity_passes() {
        let key = [2u8; 32];
        let mut state = MonitorState::default();
        let mut m = make_initialized_market();
        m.set_settlement_factor_wad(WAD / 2);
        assert!(check_settlement_factor_monotonicity("test", &key, &m, &mut state).is_ok());
        m.set_settlement_factor_wad(WAD);
        assert!(check_settlement_factor_monotonicity("test", &key, &m, &mut state).is_ok());
    }

    #[test]
    fn settlement_factor_monotonicity_fails_on_decrease() {
        let key = [2u8; 32];
        let mut state = MonitorState::default();
        let mut m = make_initialized_market();
        m.set_settlement_factor_wad(WAD);
        assert!(check_settlement_factor_monotonicity("test", &key, &m, &mut state).is_ok());
        m.set_settlement_factor_wad(WAD / 2);
        let err =
            check_settlement_factor_monotonicity("test", &key, &m, &mut state).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::SettlementFactorDecreased);
    }

    #[test]
    fn fee_non_negativity_passes_normal_value() {
        let mut m = make_initialized_market();
        m.set_accrued_protocol_fees(1_000_000);
        assert!(check_fee_non_negativity("test", &m).is_ok());
    }

    #[test]
    fn fee_non_negativity_warns_near_max() {
        let mut m = make_initialized_market();
        m.set_accrued_protocol_fees(u64::MAX);
        let err = check_fee_non_negativity("test", &m).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::SuspiciousAccruedFees);
        assert_eq!(err.severity, Severity::Warning);
    }

    #[test]
    fn supply_cap_passes() {
        let mut m = make_initialized_market();
        m.set_scaled_total_supply(500_000); // with scale_factor=WAD, real_supply=500_000
        m.set_max_total_supply(1_000_000);
        assert!(check_supply_cap("test", &m).is_ok());
    }

    #[test]
    fn supply_cap_fails_when_exceeded() {
        let mut m = make_initialized_market();
        // real_supply = 2_000_000 * WAD / WAD = 2_000_000 > 1_000_000
        m.set_scaled_total_supply(2_000_000);
        m.set_max_total_supply(1_000_000);
        let err = check_supply_cap("test", &m).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::SupplyCapExceeded);
    }

    #[test]
    fn supply_cap_skips_uninitialized() {
        let m = Market::zeroed();
        assert!(check_supply_cap("test", &m).is_ok());
    }

    #[test]
    fn lender_balance_consistency_passes() {
        let mut m = make_initialized_market();
        m.set_scaled_total_supply(300);

        let mut p1 = LenderPosition::zeroed();
        p1.set_scaled_balance(100);
        let mut p2 = LenderPosition::zeroed();
        p2.set_scaled_balance(200);

        assert!(check_lender_balance_consistency("test", &m, &[p1, p2]).is_ok());
    }

    #[test]
    fn lender_balance_consistency_fails_on_mismatch() {
        let mut m = make_initialized_market();
        m.set_scaled_total_supply(300);

        let mut p1 = LenderPosition::zeroed();
        p1.set_scaled_balance(100);

        let err = check_lender_balance_consistency("test", &m, &[p1]).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::LenderBalanceInconsistency);
    }

    #[test]
    fn whitelist_capacity_passes() {
        let mut wl = BorrowerWhitelist::zeroed();
        wl.set_max_borrow_capacity(1_000_000);
        wl.set_current_borrowed(500_000);
        assert!(check_whitelist_capacity("test", &wl).is_ok());
    }

    #[test]
    fn whitelist_capacity_fails_when_exceeded() {
        let mut wl = BorrowerWhitelist::zeroed();
        wl.set_max_borrow_capacity(1_000_000);
        wl.set_current_borrowed(1_000_001);
        let err = check_whitelist_capacity("test", &wl).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::WhitelistCapacityExceeded);
    }

    #[test]
    fn stale_accrual_passes_within_threshold() {
        let mut m = make_initialized_market();
        m.set_last_accrual_timestamp(1_000);
        assert!(check_stale_accrual("test", &m, 1_500, 3600).is_ok());
    }

    #[test]
    fn stale_accrual_fails_beyond_threshold() {
        let mut m = make_initialized_market();
        m.set_last_accrual_timestamp(1_000);
        let err = check_stale_accrual("test", &m, 5_000, 3600).unwrap_err();
        assert_eq!(err.violation_type, ViolationType::StaleAccrual);
        assert_eq!(err.severity, Severity::Warning);
    }

    #[test]
    fn stale_accrual_skips_settled_market() {
        let mut m = make_initialized_market();
        m.set_last_accrual_timestamp(1_000);
        m.set_settlement_factor_wad(WAD);
        // Even with a stale timestamp, settled markets are skipped.
        assert!(check_stale_accrual("test", &m, 100_000, 3600).is_ok());
    }

    #[test]
    fn stale_accrual_skips_uninitialized() {
        let m = Market::zeroed();
        assert!(check_stale_accrual("test", &m, 100_000, 3600).is_ok());
    }

    #[test]
    fn aggregate_check_returns_all_violations() {
        let key = [3u8; 32];
        let mut state = MonitorState::default();
        let mut m = Market::zeroed();
        // Initialize with scale_factor below WAD (violation).
        m.set_scale_factor(WAD - 1);
        // Supply cap violated: scaled_total_supply * scale_factor / WAD > max_total_supply
        // With scale_factor = WAD-1, scaled_total_supply = 2_000_000, max = 1_000_000
        m.set_scaled_total_supply(2_000_000);
        m.set_max_total_supply(1_000_000);
        // Suspicious fees
        m.set_accrued_protocol_fees(u64::MAX);

        let violations = check_all_market_invariants(
            "test", &key, &m, 0, &[], &mut state, 0, 3600,
        );
        // Should have at least: ScaleFactorBelowWad, SupplyCapExceeded,
        // SuspiciousAccruedFees, LenderBalanceInconsistency (sum=0 != 2_000_000)
        assert!(violations.len() >= 4, "got {} violations", violations.len());
    }
}
