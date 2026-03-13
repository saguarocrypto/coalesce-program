#![allow(clippy::expect_used, clippy::unwrap_used)]

use num_bigint::BigUint;

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};

const SECONDS_PER_DAY: u128 = 86_400;
const DAYS_PER_YEAR: u128 = 365;

fn bigint_growth_factor(annual_bps: u16, elapsed_seconds: u128) -> BigUint {
    let whole_days = elapsed_seconds / SECONDS_PER_DAY;
    let remaining_secs = elapsed_seconds % SECONDS_PER_DAY;
    let wad = BigUint::from(WAD);
    let bps = BigUint::from(BPS);
    let days_per_year = BigUint::from(DAYS_PER_YEAR);
    let seconds_per_year = BigUint::from(SECONDS_PER_YEAR);

    let mul_wad = |a: &BigUint, b: &BigUint| -> BigUint { (a * b) / &wad };

    let daily_rate = BigUint::from(u128::from(annual_bps)) * &wad / (&days_per_year * &bps);
    let mut result = wad.clone();
    let mut base = &wad + daily_rate;
    let mut exp = whole_days;

    while exp > 0 {
        if exp & 1 == 1 {
            result = mul_wad(&result, &base);
        }
        exp >>= 1;
        if exp > 0 {
            base = mul_wad(&base, &base);
        }
    }

    let remaining_delta =
        BigUint::from(u128::from(annual_bps)) * BigUint::from(remaining_secs) * &wad
            / (&seconds_per_year * &bps);
    let remaining_growth = &wad + remaining_delta;

    mul_wad(&result, &remaining_growth)
}

pub fn growth_factor_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    assert!(elapsed_seconds >= 0, "elapsed_seconds must be non-negative");
    let growth = bigint_growth_factor(
        annual_bps,
        u128::try_from(elapsed_seconds).expect("elapsed fits u128"),
    );
    u128::try_from(growth).expect("growth factor should fit in u128")
}

pub fn interest_delta_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    growth_factor_wad_exact(annual_bps, elapsed_seconds)
        .checked_sub(WAD)
        .expect("growth should be >= WAD")
}

pub fn scale_factor_after_exact(scale_factor: u128, annual_bps: u16, elapsed_seconds: i64) -> u128 {
    let growth = BigUint::from(growth_factor_wad_exact(annual_bps, elapsed_seconds));
    let sf = BigUint::from(scale_factor);
    let wad = BigUint::from(WAD);
    let out = sf * growth / wad;
    u128::try_from(out).expect("scale factor should fit in u128")
}

pub fn fee_delta_exact(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> u64 {
    if scaled_total_supply == 0 || fee_rate_bps == 0 || elapsed_seconds <= 0 {
        return 0;
    }

    let interest_delta_wad = BigUint::from(interest_delta_wad_exact(annual_bps, elapsed_seconds));
    let bps = BigUint::from(BPS);
    let wad = BigUint::from(WAD);

    let fee_delta_wad = interest_delta_wad * BigUint::from(u128::from(fee_rate_bps)) / &bps;

    // Use pre-accrual scale_factor_before (matches on-chain logic after Finding 10 fix)
    let fee_normalized =
        BigUint::from(scaled_total_supply) * BigUint::from(scale_factor_before) / &wad * fee_delta_wad
            / &wad;

    u64::try_from(fee_normalized).expect("fee should fit in u64 for bounded tests")
}
