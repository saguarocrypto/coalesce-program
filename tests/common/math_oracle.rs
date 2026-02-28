//! Shared u128-based math oracle for test files.
//!
//! Provides `mul_wad`, `pow_wad`, and `growth_factor_wad` in both panicking
//! and checked (`Option`) variants so that individual test files don't need
//! to duplicate the same implementations.

#![allow(dead_code)]

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};

pub const SECONDS_PER_DAY: u128 = 86_400;
pub const DAYS_PER_YEAR: u128 = 365;

// ---------------------------------------------------------------------------
// Panicking variants (return u128, panic on overflow)
// ---------------------------------------------------------------------------

pub fn mul_wad(a: u128, b: u128) -> u128 {
    a.checked_mul(b).unwrap().checked_div(WAD).unwrap()
}

pub fn pow_wad(base: u128, exp: u32) -> u128 {
    let mut result = WAD;
    let mut b = base;
    let mut e = exp;

    while e > 0 {
        if e & 1 == 1 {
            result = mul_wad(result, b);
        }
        e >>= 1;
        if e > 0 {
            b = mul_wad(b, b);
        }
    }

    result
}

pub fn growth_factor_wad(annual_interest_bps: u16, elapsed_seconds: i64) -> u128 {
    let elapsed = u128::try_from(elapsed_seconds).unwrap();
    let whole_days = elapsed / SECONDS_PER_DAY;
    let remaining_seconds = elapsed % SECONDS_PER_DAY;

    let daily_rate_wad = u128::from(annual_interest_bps) * WAD / (DAYS_PER_YEAR * BPS);
    let compounded_days = pow_wad(WAD + daily_rate_wad, u32::try_from(whole_days).unwrap());

    let remaining_delta_wad = u128::from(annual_interest_bps) * remaining_seconds * WAD
        / (u128::from(SECONDS_PER_YEAR) * BPS);
    let remaining_growth = WAD + remaining_delta_wad;

    mul_wad(compounded_days, remaining_growth)
}

// ---------------------------------------------------------------------------
// Checked variants (return Option<u128>, None on overflow)
// ---------------------------------------------------------------------------

pub fn mul_wad_checked(a: u128, b: u128) -> Option<u128> {
    a.checked_mul(b)?.checked_div(WAD)
}

pub fn pow_wad_checked(base: u128, exp: u32) -> Option<u128> {
    let mut result = WAD;
    let mut b = base;
    let mut e = exp;
    while e > 0 {
        if e & 1 == 1 {
            result = mul_wad_checked(result, b)?;
        }
        e >>= 1;
        if e > 0 {
            b = mul_wad_checked(b, b)?;
        }
    }
    Some(result)
}

pub fn growth_factor_wad_checked(annual_interest_bps: u16, elapsed_seconds: i64) -> Option<u128> {
    let elapsed = u128::try_from(elapsed_seconds).ok()?;
    let whole_days = elapsed.checked_div(SECONDS_PER_DAY)?;
    let remaining_seconds = elapsed.checked_rem(SECONDS_PER_DAY)?;

    let daily_rate_wad = u128::from(annual_interest_bps)
        .checked_mul(WAD)?
        .checked_div(DAYS_PER_YEAR.checked_mul(BPS)?)?;
    let days_growth = pow_wad_checked(
        WAD.checked_add(daily_rate_wad)?,
        u32::try_from(whole_days).ok()?,
    )?;

    let remaining_delta_wad = u128::from(annual_interest_bps)
        .checked_mul(remaining_seconds)?
        .checked_mul(WAD)?
        .checked_div(u128::from(SECONDS_PER_YEAR).checked_mul(BPS)?)?;
    let remaining_growth = WAD.checked_add(remaining_delta_wad)?;

    mul_wad_checked(days_growth, remaining_growth)
}
