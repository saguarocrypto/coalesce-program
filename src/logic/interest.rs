//! Daily compound interest model via scale factor.
//!
//! Interest accrues by multiplying the market's `scale_factor` by a growth
//! factor made of:
//! - whole-day discrete compounding at `annual_rate / 365`
//! - sub-day linear accrual for remaining seconds
//!
//! Lender balances are stored in "scaled" units. The actual (normalized) balance
//! is `scaled_balance * scale_factor / WAD`. This design means deposits at
//! different times automatically earn the correct compound interest without
//! per-position bookkeeping.
//!
//! Protocol fees are computed as a fraction of each interest delta and tracked
//! separately in `accrued_protocol_fees`.

use crate::constants::{BPS, SECONDS_PER_YEAR, WAD};
use crate::error::LendingError;
use crate::state::{Market, ProtocolConfig};
use pinocchio::error::ProgramError;

const SECONDS_PER_DAY: i64 = 86_400;
const DAYS_PER_YEAR: u128 = 365;

fn mul_wad(a: u128, b: u128) -> Result<u128, ProgramError> {
    a.checked_mul(b)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(WAD)
        .ok_or(LendingError::MathOverflow.into())
}

/// Compute `base^exp` for WAD-scaled values using exponentiation by squaring.
fn pow_wad(base: u128, exp: u32) -> Result<u128, ProgramError> {
    let mut result = WAD;
    let mut b = base;
    let mut e = exp;

    while e > 0 {
        if e & 1 == 1 {
            result = mul_wad(result, b)?;
        }
        e >>= 1;
        if e > 0 {
            b = mul_wad(b, b)?;
        }
    }

    Ok(result)
}

/// Accrue interest on a market up to `current_timestamp`.
/// Interest stops at maturity (§6.1, SR-058).
///
/// Updates: `market.scale_factor`, `market.accrued_protocol_fees`,
///          `market.last_accrual_timestamp`.
pub fn accrue_interest(
    market: &mut Market,
    protocol_config: &ProtocolConfig,
    current_timestamp: i64,
) -> Result<(), ProgramError> {
    let maturity = market.maturity_timestamp();
    let last_accrual = market.last_accrual_timestamp();

    // Cap accrual at maturity
    let effective_now = if current_timestamp > maturity {
        maturity
    } else {
        current_timestamp
    };

    // SR-114: Validate timestamp ordering to prevent manipulation attacks
    if effective_now < last_accrual {
        return Err(LendingError::InvalidTimestamp.into());
    }

    let time_elapsed = effective_now
        .checked_sub(last_accrual)
        .ok_or(LendingError::MathOverflow)?;
    if time_elapsed <= 0 {
        return Ok(());
    }

    let annual_bps = u128::from(market.annual_interest_bps());
    let scale_factor = market.scale_factor();

    let days_elapsed = time_elapsed
        .checked_div(SECONDS_PER_DAY)
        .ok_or(LendingError::MathOverflow)?;
    let remaining_seconds = time_elapsed
        .checked_rem(SECONDS_PER_DAY)
        .ok_or(LendingError::MathOverflow)?;

    let days_elapsed_u32 = u32::try_from(days_elapsed).map_err(|_| LendingError::MathOverflow)?;
    let remaining_seconds_u128 =
        u128::try_from(remaining_seconds).map_err(|_| LendingError::MathOverflow)?;

    // daily_rate_wad = annual_bps * WAD / (365 * BPS)
    let daily_rate_wad = annual_bps
        .checked_mul(WAD)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(
            DAYS_PER_YEAR
                .checked_mul(BPS)
                .ok_or(LendingError::MathOverflow)?,
        )
        .ok_or(LendingError::MathOverflow)?;

    let daily_base_wad = WAD
        .checked_add(daily_rate_wad)
        .ok_or(LendingError::MathOverflow)?;

    // Compound full days: sf *= (1 + daily_rate) ^ whole_days
    let days_growth_wad = pow_wad(daily_base_wad, days_elapsed_u32)?;

    // Linear remaining seconds: sf *= 1 + annual_rate * remaining / SECONDS_PER_YEAR
    let remaining_delta_wad = annual_bps
        .checked_mul(remaining_seconds_u128)
        .ok_or(LendingError::MathOverflow)?
        .checked_mul(WAD)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(
            SECONDS_PER_YEAR
                .checked_mul(BPS)
                .ok_or(LendingError::MathOverflow)?,
        )
        .ok_or(LendingError::MathOverflow)?;

    let remaining_growth_wad = WAD
        .checked_add(remaining_delta_wad)
        .ok_or(LendingError::MathOverflow)?;

    let total_growth_wad = mul_wad(days_growth_wad, remaining_growth_wad)?;

    let new_scale_factor = mul_wad(scale_factor, total_growth_wad)?;

    let interest_delta_wad = total_growth_wad
        .checked_sub(WAD)
        .ok_or(LendingError::MathOverflow)?;

    // Protocol fee accrual (§6.1.3)
    let fee_rate_bps = u128::from(protocol_config.fee_rate_bps());
    if fee_rate_bps > 0 {
        let scaled_total_supply = market.scaled_total_supply();

        // fee_delta_wad = interest_delta_wad * fee_rate_bps / BPS
        let fee_delta_wad = interest_delta_wad
            .checked_mul(fee_rate_bps)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(BPS)
            .ok_or(LendingError::MathOverflow)?;

        // fee_normalized = scaled_total_supply * scale_factor / WAD * fee_delta_wad / WAD
        // Use new scale_factor for the fee computation (matches spec: fee computed on current supply at current scale)
        let fee_normalized = scaled_total_supply
            .checked_mul(new_scale_factor)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?
            .checked_mul(fee_delta_wad)
            .ok_or(LendingError::MathOverflow)?
            .checked_div(WAD)
            .ok_or(LendingError::MathOverflow)?;

        let fee_normalized_u64 =
            u64::try_from(fee_normalized).map_err(|_| LendingError::MathOverflow)?;

        let new_fees = market
            .accrued_protocol_fees()
            .checked_add(fee_normalized_u64)
            .ok_or(LendingError::MathOverflow)?;
        market.set_accrued_protocol_fees(new_fees);
    }

    market.set_scale_factor(new_scale_factor);
    market.set_last_accrual_timestamp(effective_now);

    Ok(())
}

/// Compute the settlement factor for a market withdrawal.
///
/// This is the ratio of available vault funds to total normalized deposits,
/// clamped to [1, WAD]. Used in `processor/withdraw.rs` to determine
/// lender payouts when the vault is underfunded.
///
/// # Arguments
/// * `available` - vault balance available for lenders (vault - fee reserve)
/// * `total_normalized` - total deposits at current scale (scaled_supply * scale_factor / WAD)
pub fn compute_settlement_factor(
    available: u128,
    total_normalized: u128,
) -> Result<u128, ProgramError> {
    if total_normalized == 0 {
        return Ok(WAD);
    }
    let raw = available
        .checked_mul(WAD)
        .ok_or(LendingError::MathOverflow)?
        .checked_div(total_normalized)
        .ok_or(LendingError::MathOverflow)?;
    let capped = if raw > WAD { WAD } else { raw };
    Ok(if capped < 1 { 1 } else { capped })
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    const WAD_VAL: u128 = 1_000_000_000_000_000_000;
    const BPS_VAL: u128 = 10_000;
    const SECONDS_PER_YEAR_VAL: u128 = 31_536_000;
    const SECONDS_PER_DAY_VAL: u128 = 86_400;
    const DAYS_PER_YEAR_VAL: u128 = 365;

    /// Helper: create a zeroed Market with specific fields set.
    fn make_market(
        annual_interest_bps: u16,
        maturity_timestamp: i64,
        scale_factor: u128,
        scaled_total_supply: u128,
        last_accrual_timestamp: i64,
        accrued_protocol_fees: u64,
    ) -> Market {
        let mut m = Market::zeroed();
        m.set_annual_interest_bps(annual_interest_bps);
        m.set_maturity_timestamp(maturity_timestamp);
        m.set_scale_factor(scale_factor);
        m.set_scaled_total_supply(scaled_total_supply);
        m.set_last_accrual_timestamp(last_accrual_timestamp);
        m.set_accrued_protocol_fees(accrued_protocol_fees);
        m
    }

    /// Helper: create a zeroed ProtocolConfig with a specific fee rate.
    fn make_config(fee_rate_bps: u16) -> ProtocolConfig {
        let mut c = ProtocolConfig::zeroed();
        c.set_fee_rate_bps(fee_rate_bps);
        c
    }

    fn mul_wad_oracle(a: u128, b: u128) -> u128 {
        a.checked_mul(b).unwrap().checked_div(WAD_VAL).unwrap()
    }

    fn pow_wad_oracle(base: u128, exp: u32) -> u128 {
        let mut result = WAD_VAL;
        let mut b = base;
        let mut e = exp;
        while e > 0 {
            if e & 1 == 1 {
                result = mul_wad_oracle(result, b);
            }
            e >>= 1;
            if e > 0 {
                b = mul_wad_oracle(b, b);
            }
        }
        result
    }

    fn growth_factor_wad(annual_bps: u16, elapsed_seconds: i64) -> u128 {
        let elapsed_u128 = u128::try_from(elapsed_seconds).unwrap();
        let days_elapsed = elapsed_u128 / SECONDS_PER_DAY_VAL;
        let remaining_seconds = elapsed_u128 % SECONDS_PER_DAY_VAL;

        let daily_rate_wad = u128::from(annual_bps) * WAD_VAL / (DAYS_PER_YEAR_VAL * BPS_VAL);
        let days_growth = pow_wad_oracle(
            WAD_VAL + daily_rate_wad,
            u32::try_from(days_elapsed).unwrap(),
        );

        let remaining_delta_wad =
            u128::from(annual_bps) * remaining_seconds * WAD_VAL / (SECONDS_PER_YEAR_VAL * BPS_VAL);
        let remaining_growth = WAD_VAL + remaining_delta_wad;

        mul_wad_oracle(days_growth, remaining_growth)
    }

    fn scale_factor_after_elapsed(
        scale_factor: u128,
        annual_bps: u16,
        elapsed_seconds: i64,
    ) -> u128 {
        mul_wad_oracle(scale_factor, growth_factor_wad(annual_bps, elapsed_seconds))
    }

    fn interest_delta_wad_after_elapsed(annual_bps: u16, elapsed_seconds: i64) -> u128 {
        growth_factor_wad(annual_bps, elapsed_seconds) - WAD_VAL
    }

    fn fee_delta_after_elapsed(
        scaled_supply: u128,
        scale_factor_before: u128,
        annual_bps: u16,
        fee_rate_bps: u16,
        elapsed_seconds: i64,
    ) -> u64 {
        if scaled_supply == 0 || fee_rate_bps == 0 || elapsed_seconds <= 0 {
            return 0;
        }

        let new_sf = scale_factor_after_elapsed(scale_factor_before, annual_bps, elapsed_seconds);
        let interest_delta_wad = interest_delta_wad_after_elapsed(annual_bps, elapsed_seconds);
        let fee_delta_wad = interest_delta_wad * u128::from(fee_rate_bps) / BPS_VAL;
        let fee_normalized = scaled_supply * new_sf / WAD_VAL * fee_delta_wad / WAD_VAL;
        u64::try_from(fee_normalized).unwrap()
    }

    #[test]
    fn test_pow_wad_zero_exp() {
        let base = WAD_VAL + 123_456_789;
        assert_eq!(pow_wad(base, 0).unwrap(), WAD_VAL);
    }

    #[test]
    fn test_pow_wad_one_exp() {
        let base = WAD_VAL + 123_456_789;
        assert_eq!(pow_wad(base, 1).unwrap(), base);
    }

    #[test]
    fn test_pow_wad_overflow() {
        let result = pow_wad(u128::MAX, 2);
        assert!(result.is_err());
    }

    // T1-1: time_elapsed = 0 => scale_factor and fees unchanged
    #[test]
    fn test_accrue_zero_time_elapsed() {
        let mut market = make_market(1000, 2_000_000_000, WAD_VAL, WAD_VAL, 100, 0);
        let config = make_config(500);
        // current_timestamp == last_accrual_timestamp => time_elapsed = 0
        accrue_interest(&mut market, &config, 100).unwrap();
        assert_eq!(market.scale_factor(), WAD_VAL);
        assert_eq!(market.accrued_protocol_fees(), 0);
        assert_eq!(market.last_accrual_timestamp(), 100);
    }

    // T1-2: current_ts > maturity => interest accrues only to maturity
    #[test]
    fn test_accrue_capped_at_maturity() {
        let maturity = 1000i64;
        let last_accrual = 0i64;
        let mut market = make_market(1000, maturity, WAD_VAL, WAD_VAL, last_accrual, 0);
        let config = make_config(0);

        // Call with current_ts far past maturity
        accrue_interest(&mut market, &config, 2_000_000).unwrap();

        // last_accrual should be capped at maturity, not 2_000_000
        assert_eq!(market.last_accrual_timestamp(), maturity);

        // Verify the scale_factor reflects exactly maturity - last_accrual = 1000 seconds.
        let expected = scale_factor_after_elapsed(WAD_VAL, 1000, 1000);
        assert_eq!(market.scale_factor(), expected);
    }

    // T1-3: 10% annual, 1M scaled supply, 365 days => scale_factor increases by ~10%
    #[test]
    fn test_accrue_known_values() {
        let annual_bps: u16 = 1000; // 10%
        let seconds = SECONDS_PER_YEAR_VAL as i64; // full year
        let supply = 1_000_000u128 * WAD_VAL; // large scaled supply (not used for scale_factor itself)
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, supply, 0, 0);
        let config = make_config(0);

        accrue_interest(&mut market, &config, seconds).unwrap();

        let expected = scale_factor_after_elapsed(WAD_VAL, annual_bps, seconds);
        assert_eq!(market.scale_factor(), expected);
    }

    // T1-4: 5% fee rate, verify fee_delta = interest_delta * fee_rate / BPS
    #[test]
    fn test_accrue_fee_accrual() {
        let annual_bps: u16 = 1000; // 10%
        let fee_rate_bps: u16 = 500; // 5%
        let seconds = SECONDS_PER_YEAR_VAL as i64;
        // Supply = 1M USDC worth of scaled tokens at WAD scale
        let scaled_supply = 1_000_000_000_000u128; // 1M USDC in 6-decimal base units
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, scaled_supply, 0, 0);
        let config = make_config(fee_rate_bps);

        accrue_interest(&mut market, &config, seconds).unwrap();

        let expected_fee =
            fee_delta_after_elapsed(scaled_supply, WAD_VAL, annual_bps, fee_rate_bps, seconds);

        assert_eq!(market.accrued_protocol_fees(), expected_fee);
    }

    // T1-5: fee_rate_bps = 0 => accrued_protocol_fees stays 0, scale factor exact
    #[test]
    fn test_accrue_zero_fee_rate() {
        let annual_bps: u16 = 1000;
        let seconds = SECONDS_PER_YEAR_VAL as i64;
        let scaled_supply = 1_000_000_000_000u128;
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, scaled_supply, 0, 0);
        let config = make_config(0);

        accrue_interest(&mut market, &config, seconds).unwrap();

        assert_eq!(market.accrued_protocol_fees(), 0);
        let expected = scale_factor_after_elapsed(WAD_VAL, annual_bps, seconds);
        assert_eq!(market.scale_factor(), expected);
    }

    // T1-6: Two sequential calls compound; exact values verified
    #[test]
    fn test_accrue_sequential_calls() {
        let annual_bps: u16 = 1000;
        let scaled_supply = 1_000_000_000_000u128;

        // Single call: 0 -> 1000
        let mut market_single = make_market(annual_bps, i64::MAX, WAD_VAL, scaled_supply, 0, 0);
        let config = make_config(0);
        accrue_interest(&mut market_single, &config, 1000).unwrap();

        // Verify exact single-call result
        let expected_single = scale_factor_after_elapsed(WAD_VAL, annual_bps, 1000);
        assert_eq!(market_single.scale_factor(), expected_single);

        // Two calls: 0 -> 500, 500 -> 1000
        let mut market_double = make_market(annual_bps, i64::MAX, WAD_VAL, scaled_supply, 0, 0);
        accrue_interest(&mut market_double, &config, 500).unwrap();
        accrue_interest(&mut market_double, &config, 1000).unwrap();

        // Verify exact compound result: step 2 uses increased scale_factor from step 1.
        let sf_after_step1 = scale_factor_after_elapsed(WAD_VAL, annual_bps, 500);
        let expected_double = scale_factor_after_elapsed(sf_after_step1, annual_bps, 500);
        assert_eq!(market_double.scale_factor(), expected_double);

        // Compound (two-step) must exceed simple (single-step)
        assert!(expected_double > expected_single);
    }

    // T1-7: 1 second elapsed, exact interest delta
    #[test]
    fn test_accrue_one_second() {
        let annual_bps: u16 = 1000;
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, WAD_VAL, 0, 0);
        let config = make_config(0);

        accrue_interest(&mut market, &config, 1).unwrap();

        let expected = scale_factor_after_elapsed(WAD_VAL, annual_bps, 1);
        assert_eq!(market.scale_factor(), expected);
    }

    // T1-8: Exactly SECONDS_PER_YEAR => interest_delta_wad = annual_bps * WAD / BPS
    #[test]
    fn test_accrue_full_year() {
        let annual_bps: u16 = 500; // 5%
        let seconds = SECONDS_PER_YEAR_VAL as i64;
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, WAD_VAL, 0, 0);
        let config = make_config(0);

        accrue_interest(&mut market, &config, seconds).unwrap();

        let expected = scale_factor_after_elapsed(WAD_VAL, annual_bps, seconds);
        assert_eq!(market.scale_factor(), expected);
    }

    // T1-9: Large amounts near u128 boundaries (verify no overflow panic)
    #[test]
    fn test_accrue_large_amounts() {
        // Use a huge scale_factor that might overflow on multiplication
        let huge_scale = u128::MAX / 2;
        let annual_bps: u16 = 10000; // 100%
        let seconds = SECONDS_PER_YEAR_VAL as i64;
        let mut market = make_market(annual_bps, i64::MAX, huge_scale, 1, 0, 0);
        let config = make_config(0);

        // This should either succeed or return MathOverflow, not panic
        let result = accrue_interest(&mut market, &config, seconds);
        // With huge_scale, the multiplication will overflow
        assert!(result.is_err());
    }

    // T1-10: fee_rate_bps = 10000 => all interest goes to fees
    #[test]
    fn test_accrue_fee_rate_100_percent() {
        let annual_bps: u16 = 1000; // 10%
        let fee_rate_bps: u16 = 10000; // 100% of interest goes to fees
        let seconds = SECONDS_PER_YEAR_VAL as i64;
        let scaled_supply = 1_000_000_000_000u128; // 1M USDC
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, scaled_supply, 0, 0);
        let config = make_config(fee_rate_bps);

        accrue_interest(&mut market, &config, seconds).unwrap();

        let expected_fee =
            fee_delta_after_elapsed(scaled_supply, WAD_VAL, annual_bps, fee_rate_bps, seconds);

        assert_eq!(market.accrued_protocol_fees(), expected_fee);
    }

    // T1-12: effective_now < last_accrual returns InvalidTimestamp (error 20)
    #[test]
    fn test_accrue_negative_timestamp_rejected() {
        let mut market = make_market(1000, 2_000_000_000, WAD_VAL, WAD_VAL, 1000, 0);
        let config = make_config(0);
        // current_timestamp before last_accrual but also before maturity
        let result = accrue_interest(&mut market, &config, 500);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(
            err,
            pinocchio::error::ProgramError::Custom(LendingError::InvalidTimestamp as u32)
        );
    }

    // T1-13: 100% rate over full year: scale_factor = WAD + WAD = 2*WAD
    #[test]
    fn test_accrue_max_annual_bps_one_year() {
        let annual_bps: u16 = 10000; // 100%
        let seconds = SECONDS_PER_YEAR_VAL as i64;
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, WAD_VAL, 0, 0);
        let config = make_config(0);

        accrue_interest(&mut market, &config, seconds).unwrap();

        let expected = scale_factor_after_elapsed(WAD_VAL, annual_bps, seconds);
        assert_eq!(market.scale_factor(), expected);
    }

    // T1-14: 1 bps, 1 second: minimum meaningful delta is non-zero
    #[test]
    fn test_accrue_1_bps_one_second() {
        let mut market = make_market(1, i64::MAX, WAD_VAL, WAD_VAL, 0, 0);
        let config = make_config(0);

        accrue_interest(&mut market, &config, 1).unwrap();

        let interest_delta = interest_delta_wad_after_elapsed(1, 1);
        assert!(interest_delta > 0, "delta should be non-zero");
        let delta = market.scale_factor() - WAD_VAL;
        assert_eq!(delta, interest_delta);
    }

    // T1-15: Three sequential accruals, exact fee accumulation verified
    #[test]
    fn test_accrue_fee_accumulates_across_calls() {
        let annual_bps: u16 = 1000; // 10%
        let fee_rate_bps: u16 = 500; // 5%
        let scaled_supply = 1_000_000_000_000u128;
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, scaled_supply, 0, 0);
        let config = make_config(fee_rate_bps);

        // Helper: replicate on-chain fee computation for a 1000-second interval.
        let compute_expected_fee = |sf_before: u128| -> (u128, u64) {
            let sf_after = scale_factor_after_elapsed(sf_before, annual_bps, 1000);
            let fee =
                fee_delta_after_elapsed(scaled_supply, sf_before, annual_bps, fee_rate_bps, 1000);
            (sf_after, fee)
        };

        // Step 1: 0 → 1000
        accrue_interest(&mut market, &config, 1000).unwrap();
        let (sf1, fee1) = compute_expected_fee(WAD_VAL);
        assert_eq!(market.scale_factor(), sf1);
        assert_eq!(market.accrued_protocol_fees(), fee1);

        // Step 2: 1000 → 2000
        accrue_interest(&mut market, &config, 2000).unwrap();
        let (sf2, fee2_delta) = compute_expected_fee(sf1);
        assert_eq!(market.scale_factor(), sf2);
        assert_eq!(market.accrued_protocol_fees(), fee1 + fee2_delta);

        // Step 3: 2000 → 3000
        accrue_interest(&mut market, &config, 3000).unwrap();
        let (sf3, fee3_delta) = compute_expected_fee(sf2);
        assert_eq!(market.scale_factor(), sf3);
        assert_eq!(
            market.accrued_protocol_fees(),
            fee1 + fee2_delta + fee3_delta
        );
    }

    // T1-16: last_accrual == maturity, no change (early exit)
    #[test]
    fn test_accrue_already_at_maturity() {
        let maturity = 1000i64;
        let mut market = make_market(1000, maturity, WAD_VAL, WAD_VAL, maturity, 0);
        let config = make_config(500);

        let sf_before = market.scale_factor();
        let fees_before = market.accrued_protocol_fees();

        // Call with any timestamp past maturity
        accrue_interest(&mut market, &config, maturity + 1000).unwrap();

        assert_eq!(market.scale_factor(), sf_before);
        assert_eq!(market.accrued_protocol_fees(), fees_before);
        assert_eq!(market.last_accrual_timestamp(), maturity);
    }

    // T1-17: Second call past maturity produces no additional change
    #[test]
    fn test_accrue_past_maturity_idempotent() {
        let maturity = 1000i64;
        let mut market = make_market(1000, maturity, WAD_VAL, WAD_VAL, 0, 0);
        let config = make_config(500);

        // First call past maturity
        accrue_interest(&mut market, &config, maturity + 5000).unwrap();
        let sf_after_1 = market.scale_factor();
        let fees_after_1 = market.accrued_protocol_fees();

        // Second call even further past maturity
        accrue_interest(&mut market, &config, maturity + 100_000).unwrap();
        assert_eq!(market.scale_factor(), sf_after_1);
        assert_eq!(market.accrued_protocol_fees(), fees_after_1);
    }

    // T1-18: 100% fee rate, 1s accrual, verify exact fee value
    #[test]
    fn test_accrue_max_fee_rate_one_second() {
        let annual_bps: u16 = 1000; // 10%
        let fee_rate_bps: u16 = 10_000; // 100% of interest
        let scaled_supply = 1_000_000_000_000u128;
        let mut market = make_market(annual_bps, i64::MAX, WAD_VAL, scaled_supply, 0, 0);
        let config = make_config(fee_rate_bps);

        accrue_interest(&mut market, &config, 1).unwrap();

        let expected_fee =
            fee_delta_after_elapsed(scaled_supply, WAD_VAL, annual_bps, fee_rate_bps, 1);

        assert_eq!(market.accrued_protocol_fees(), expected_fee);
    }

    // T1-19: compute_settlement_factor with available == total returns WAD
    #[test]
    fn test_compute_sf_available_equals_total() {
        let factor = compute_settlement_factor(1_000_000, 1_000_000).unwrap();
        assert_eq!(factor, WAD_VAL);
    }

    // T1-20: compute_settlement_factor with available == 0 returns 1 (minimum clamp)
    #[test]
    fn test_compute_sf_available_zero() {
        let factor = compute_settlement_factor(0, 1_000_000).unwrap();
        assert_eq!(factor, 1);
    }

    // T1-21: compute_settlement_factor with 75% recovery
    #[test]
    fn test_compute_sf_partial_recovery() {
        let total = 1_000_000u128;
        let available = 750_000u128;
        let factor = compute_settlement_factor(available, total).unwrap();
        // expected = 750_000 * WAD / 1_000_000 = 0.75 * WAD
        let expected = available * WAD_VAL / total;
        assert_eq!(factor, expected);
        assert!(factor < WAD_VAL);
        assert!(factor > 1);
    }
}
