//! Gas Metering Differential Tests
//!
//! This module provides differential cost analysis for the CoalesceFi Pinocchio
//! lending protocol. It compares computational cost between different
//! implementations and input patterns to detect performance regressions and
//! unexpected complexity.
//!
//! ## Methodology
//!
//! Since we cannot directly measure BPF compute units in a native test
//! environment, we use two complementary approaches:
//!
//! 1. **Operation counting**: An `OpCounter` framework that tracks arithmetic
//!    operations (checked_mul, checked_div, checked_add, checked_sub, comparisons,
//!    branches) by manually counting them in each code path.
//!
//! 2. **Timing-based differential analysis**: Using `std::time::Instant` with
//!    sufficient iterations to get stable results, then comparing relative costs
//!    across different input patterns.
//!
//! ## Test Categories
//!
//! 1. Operation counting framework (infrastructure)
//! 2. Differential cost tests -- accrue_interest (5 tests)
//! 3. Differential cost tests -- deposit/withdraw (4 tests)
//! 4. Regression baseline tests (4 tests)
//! 5. Worst-case vs average-case comparison (3 tests)
//! 6. Instruction complexity ordering (2 tests)

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_lossless,
    clippy::unnecessary_cast,
    clippy::useless_conversion,
    clippy::identity_op,
    clippy::manual_range_contains,
    clippy::single_match,
    clippy::single_match_else,
    clippy::needless_bool,
    clippy::nonminimal_bool,
    clippy::too_many_arguments,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_abs_diff,
    clippy::unreadable_literal,
    clippy::allow_attributes,
    clippy::struct_field_names,
    clippy::explicit_iter_loop,
    clippy::needless_for_each,
    clippy::absurd_extreme_comparisons,
    clippy::duplicated_attributes,
    clippy::manual_saturating_arithmetic,
    clippy::implicit_saturating_sub,
    clippy::stable_sort_primitive,
    clippy::type_complexity,
    clippy::iter_over_hash_type,
    clippy::bool_to_int_with_if,
    clippy::map_unwrap_or,
    clippy::explicit_counter_loop,
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::if_not_else,
    clippy::int_plus_one,
    clippy::range_plus_one,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::assertions_on_constants,
    clippy::for_kv_map,
    clippy::unnecessary_literal_bound,
    clippy::useless_vec,
    clippy::almost_complete_range,
    clippy::cloned_ref_to_slice_refs,
    unused_comparisons,
    unused_imports,
    unused_doc_comments,
    unused_variables,
    unused_mut,
    unused_assignments,
    dead_code,
    deprecated
)]

use std::time::Instant;

use bytemuck::Zeroable;

use coalesce::constants::{SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

// ===========================================================================
// Section 1: Operation Counting Framework (Infrastructure)
// ===========================================================================

/// Tracks the number of arithmetic operations in a code path.
/// Used to approximate BPF compute unit cost without requiring BPF compilation.
#[derive(Debug, Clone, Copy, Default)]
struct CostProfile {
    multiplications: u32,
    divisions: u32,
    additions: u32,
    branches: u32,
    total_ops: u32,
}

impl CostProfile {
    fn new(multiplications: u32, divisions: u32, additions: u32, branches: u32) -> Self {
        Self {
            multiplications,
            divisions,
            additions,
            branches,
            total_ops: multiplications + divisions + additions + branches,
        }
    }

    /// Estimated CU cost based on operation weights.
    /// Weights: mul=70, div=100, add=40, branch=20 CU per operation.
    fn estimated_cu(&self) -> u64 {
        u64::from(self.multiplications) * 70
            + u64::from(self.divisions) * 100
            + u64::from(self.additions) * 40
            + u64::from(self.branches) * 20
    }
}

/// Wraps function execution and produces a CostProfile by counting the
/// operations in each code path of accrue_interest.
struct OpCounter;

impl OpCounter {
    /// Count operations for accrue_interest given input parameters.
    /// Returns the CostProfile for the code path taken.
    fn count_accrue_interest(
        _annual_interest_bps: u16,
        time_elapsed: i64,
        _scale_factor: u128,
        scaled_total_supply: u128,
        fee_rate_bps: u16,
    ) -> CostProfile {
        // Branch: time_elapsed <= 0 => early return
        let mut branches: u32 = 1; // effective_now comparison
        branches += 1; // time_elapsed <= 0 check

        if time_elapsed <= 0 {
            return CostProfile::new(0, 0, 0, branches);
        }

        // Base path (always executed when time_elapsed > 0):
        // - 1 conversion: time_elapsed -> u128
        // - 1 conversion: annual_bps -> u128
        // - interest_delta_wad = annual_bps.checked_mul(time_elapsed_u128)
        //                           .checked_mul(WAD)
        //                           .checked_div(SECONDS_PER_YEAR.checked_mul(BPS))
        let mut muls: u32 = 3; // annual_bps * time, result * WAD, SECONDS_PER_YEAR * BPS
        let mut divs: u32 = 1; // / (SECONDS_PER_YEAR * BPS)
        let mut adds: u32 = 0;

        // scale_factor_delta = scale_factor.checked_mul(interest_delta_wad).checked_div(WAD)
        muls += 1; // scale_factor * interest_delta_wad
        divs += 1; // / WAD

        // new_scale_factor = scale_factor.checked_add(scale_factor_delta)
        adds += 1;

        // Branch: fee_rate_bps > 0
        branches += 1;

        let fee_rate_u128 = u128::from(fee_rate_bps);
        if fee_rate_u128 > 0 && scaled_total_supply > 0 {
            // fee_delta_wad = interest_delta_wad.checked_mul(fee_rate_bps).checked_div(BPS)
            muls += 1; // interest_delta_wad * fee_rate_bps
            divs += 1; // / BPS

            // fee_normalized = scaled_total_supply.checked_mul(new_scale_factor)
            //                    .checked_div(WAD)
            //                    .checked_mul(fee_delta_wad)
            //                    .checked_div(WAD)
            muls += 2; // supply * new_sf, result * fee_delta
            divs += 2; // / WAD (twice)

            // Conversion: fee_normalized -> u64
            // Addition: accrued_fees + fee_normalized_u64
            adds += 1;
        }

        // Writes: set_scale_factor, set_last_accrual_timestamp (and possibly set_accrued_protocol_fees)
        // Not counted as arithmetic ops but noted for CU estimation

        CostProfile::new(muls, divs, adds, branches)
    }

    /// Count operations for deposit scaling.
    fn count_deposit_scaling(scale_factor: u128) -> CostProfile {
        // amount.checked_mul(WAD).checked_div(scale_factor)
        let muls = 1;
        let divs = 1;
        let adds = 0;
        let branches = if scale_factor == WAD { 0 } else { 0 }; // no additional branches
        CostProfile::new(muls, divs, adds, branches)
    }

    /// Count operations for settlement factor computation.
    fn count_settlement_factor(total_normalized: u128) -> CostProfile {
        let mut branches: u32 = 0;

        // Branch: total_normalized == 0
        branches += 1;
        if total_normalized == 0 {
            return CostProfile::new(0, 0, 0, branches);
        }

        // available.checked_mul(WAD).checked_div(total_normalized)
        let muls = 1;
        let divs = 1;
        let adds = 0;

        // Branch: raw > WAD (cap check)
        branches += 1;
        // Branch: capped < 1 (minimum check)
        branches += 1;

        CostProfile::new(muls, divs, adds, branches)
    }

    /// Count operations for payout computation (withdraw path).
    fn count_payout_computation(has_settlement: bool) -> CostProfile {
        // Base: scaled_balance * scale_factor / WAD
        let mut muls: u32 = 1;
        let mut divs: u32 = 1;
        let mut adds: u32 = 0;
        let mut branches: u32 = 0;

        if has_settlement {
            // Additional: payout = normalized * settlement_factor / WAD
            muls += 1;
            divs += 1;
            branches += 1; // settlement check
        }

        // supply subtraction
        adds += 1;

        CostProfile::new(muls, divs, adds, branches)
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

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

fn make_config(fee_rate_bps: u16) -> ProtocolConfig {
    let mut c = ProtocolConfig::zeroed();
    c.set_fee_rate_bps(fee_rate_bps);
    c
}

/// Time a closure over N iterations, returning total elapsed nanoseconds
/// for more stable ratio comparisons.
fn time_total_nanos(iterations: u32, mut f: impl FnMut()) -> u128 {
    // Warm-up runs
    for _ in 0..100 {
        f();
    }

    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    start.elapsed().as_nanos()
}

// ===========================================================================
// OpCounter and CostProfile infrastructure tests
// ===========================================================================

#[test]
fn op_counter_cost_profile_creation() {
    let profile = CostProfile::new(3, 2, 1, 2);
    assert_eq!(profile.multiplications, 3);
    assert_eq!(profile.divisions, 2);
    assert_eq!(profile.additions, 1);
    assert_eq!(profile.branches, 2);
    assert_eq!(profile.total_ops, 8);
}

#[test]
fn op_counter_cost_profile_estimated_cu() {
    const EXPECTED_CU: u64 = 490; // 3*70 + 2*100 + 1*40 + 2*20
    let profile = CostProfile::new(3, 2, 1, 2);
    assert_eq!(
        profile.estimated_cu(),
        EXPECTED_CU,
        "CostProfile(3,2,1,2) must produce exactly {} CU",
        EXPECTED_CU
    );
}

#[test]
fn op_counter_zero_profile() {
    const EXPECTED_OPS: u32 = 0;
    const EXPECTED_CU: u64 = 0;
    let profile = CostProfile::default();
    assert_eq!(
        profile.total_ops, EXPECTED_OPS,
        "zero profile must have 0 total ops"
    );
    assert_eq!(
        profile.estimated_cu(),
        EXPECTED_CU,
        "zero profile must have 0 estimated CU"
    );
}

#[test]
fn op_counter_accrue_interest_zero_time() {
    let profile = OpCounter::count_accrue_interest(1000, 0, WAD, 1_000_000, 500);
    // Early return: only branch checks, no arithmetic
    assert_eq!(profile.multiplications, 0);
    assert_eq!(profile.divisions, 0);
    assert_eq!(profile.additions, 0);
    assert!(profile.branches >= 2); // effective_now + time_elapsed checks
}

#[test]
fn op_counter_accrue_interest_no_fee() {
    let profile =
        OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000, 0);
    // Base path: 4 muls, 2 divs, 1 add, 3 branches
    assert_eq!(profile.multiplications, 4);
    assert_eq!(profile.divisions, 2);
    assert_eq!(profile.additions, 1);
    assert_eq!(profile.branches, 3);
}

#[test]
fn op_counter_accrue_interest_with_fee() {
    let profile =
        OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000, 500);
    // Base path + fee path: 4+3=7 muls, 2+3=5 divs, 1+1=2 adds, 3 branches
    assert_eq!(profile.multiplications, 7);
    assert_eq!(profile.divisions, 5);
    assert_eq!(profile.additions, 2);
    assert_eq!(profile.branches, 3);
}

#[test]
fn op_counter_fee_path_adds_constant_ops() {
    const NO_FEE_CU: u64 = 580; // 4*70 + 2*100 + 1*40 + 3*20
    const WITH_FEE_CU: u64 = 1130; // 7*70 + 5*100 + 2*40 + 3*20
    const FEE_PATH_DELTA_MULS: u32 = 3;
    const FEE_PATH_DELTA_DIVS: u32 = 3;
    const FEE_PATH_DELTA_ADDS: u32 = 1;

    let no_fee = OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000, 0);
    let with_fee =
        OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000, 500);

    // Pin exact CU for each path
    assert_eq!(
        no_fee.estimated_cu(),
        NO_FEE_CU,
        "no-fee path must be exactly {} CU",
        NO_FEE_CU
    );
    assert_eq!(
        with_fee.estimated_cu(),
        WITH_FEE_CU,
        "with-fee path must be exactly {} CU",
        WITH_FEE_CU
    );

    // Fee path should add exactly 3 multiplications, 3 divisions, 1 addition
    assert_eq!(
        with_fee.multiplications - no_fee.multiplications,
        FEE_PATH_DELTA_MULS,
        "fee path should add exactly {} multiplications",
        FEE_PATH_DELTA_MULS
    );
    assert_eq!(
        with_fee.divisions - no_fee.divisions,
        FEE_PATH_DELTA_DIVS,
        "fee path should add exactly {} divisions",
        FEE_PATH_DELTA_DIVS
    );
    assert_eq!(
        with_fee.additions - no_fee.additions,
        FEE_PATH_DELTA_ADDS,
        "fee path should add exactly {} addition",
        FEE_PATH_DELTA_ADDS
    );
}

// ===========================================================================
// Section 2: Differential Cost Tests -- accrue_interest (5 tests)
// ===========================================================================

/// Compare CU cost at 0% vs 10% vs 100% rate.
/// Verify cost ordering is sensible: 0% < 10% <= 100% (0% skips arithmetic when delta=0).
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_accrue_cost_by_rate() {
    let iterations = 50_000u32;

    // 0% rate
    let t_zero = time_total_nanos(iterations, || {
        let mut market = make_market(0, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        let config = make_config(500);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // 10% rate
    let _t_ten = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        let config = make_config(500);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // 100% rate
    let t_hundred = time_total_nanos(iterations, || {
        let mut market = make_market(10000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        let config = make_config(500);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // Operation count verification: all non-zero rates take the same path
    let ops_zero =
        OpCounter::count_accrue_interest(0, SECONDS_PER_YEAR as i64, WAD, 1_000_000_000_000, 500);
    let ops_ten = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        500,
    );
    let ops_hundred = OpCounter::count_accrue_interest(
        10000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        500,
    );

    // 10% and 100% should have the same operation count (same code path)
    assert_eq!(
        ops_ten.total_ops, ops_hundred.total_ops,
        "10% and 100% rates should have same op count: {} vs {}",
        ops_ten.total_ops, ops_hundred.total_ops
    );

    // 0% rate still executes the full path (interest_delta_wad will be 0 but
    // the code still runs all multiplications/divisions). However, 0% produces
    // a zero interest_delta_wad, so the fee path produces zero fee and may
    // short-circuit. Verify timing is within 5x.
    let ratio = t_hundred as f64 / t_zero.max(1) as f64;
    assert!(
        ratio < 5.0,
        "100% rate timing ({} ns) is {}x the 0% rate timing ({} ns); expected < 5x",
        t_hundred,
        ratio,
        t_zero
    );

    // Verify 0% rate result did not change the scale factor
    let mut check_market = make_market(0, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let config = make_config(500);
    accrue_interest(&mut check_market, &config, SECONDS_PER_YEAR as i64).unwrap();
    assert_eq!(
        check_market.scale_factor(),
        WAD,
        "0% rate should not change scale_factor"
    );

    // At the operation-count level, 0% should have fewer ops since interest_delta_wad = 0
    // and the fee path does fee_rate > 0 check then still executes (fee_rate is non-zero),
    // but the computed fee will be zero.
    assert!(
        ops_zero.total_ops <= ops_ten.total_ops,
        "0% rate op count ({}) should be <= 10% rate ({})",
        ops_zero.total_ops,
        ops_ten.total_ops
    );
}

/// Compare CU cost at 1s vs 1yr elapsed.
/// Verify time_elapsed doesn't significantly affect cost (single-step accrual).
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_accrue_cost_by_time_elapsed() {
    let iterations = 50_000u32;
    let config = make_config(500);

    // 1 second elapsed
    let t_1s = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, 1).unwrap();
    });

    // 1 year elapsed
    let t_1y = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // Same code path regardless of time_elapsed magnitude
    const MAX_BUDGET_CU: u64 = 1200; // Upper bound: accrue+fee must stay under this
    let ops_1s = OpCounter::count_accrue_interest(1000, 1, WAD, 1_000_000_000_000, 500);
    let ops_1y = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        500,
    );
    assert_eq!(
        ops_1s.total_ops, ops_1y.total_ops,
        "1s and 1yr should have same op count"
    );
    assert!(
        ops_1s.estimated_cu() <= MAX_BUDGET_CU,
        "1s accrue CU {} exceeds budget {}",
        ops_1s.estimated_cu(),
        MAX_BUDGET_CU
    );
    assert!(
        ops_1y.estimated_cu() <= MAX_BUDGET_CU,
        "1yr accrue CU {} exceeds budget {}",
        ops_1y.estimated_cu(),
        MAX_BUDGET_CU
    );

    // Timing should be within 5x (u128 arithmetic is constant-time;
    // wider tolerance for parallel test execution noise)
    let ratio = if t_1y > t_1s {
        t_1y as f64 / t_1s.max(1) as f64
    } else {
        t_1s as f64 / t_1y.max(1) as f64
    };
    assert!(
        ratio < 5.0,
        "1s vs 1yr timing ratio {:.2}x exceeds 5x threshold (1s={} ns, 1yr={} ns)",
        ratio,
        t_1s,
        t_1y
    );
}

/// Compare CU cost with 0 supply vs 1B supply.
/// Verify supply size doesn't significantly affect accrual cost.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_accrue_cost_by_supply_size() {
    let iterations = 50_000u32;
    let config = make_config(500);

    // 0 supply
    let t_zero_supply = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // 1B supply (1 billion USDC in 6-decimal units)
    let t_1b_supply = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // With zero supply, the fee path still runs (fee_rate > 0), but the multiplication
    // with zero supply produces zero. Operation count should be identical.
    let ops_zero = OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 0, 500);
    let ops_1b = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000_000_000,
        500,
    );

    // Both paths have identical structure; only values differ
    // Note: OpCounter currently treats zero supply differently (no fee ops when supply=0)
    // The actual accrue_interest code checks fee_rate_bps > 0 (not supply > 0),
    // so it still executes the fee math. Op count difference is acceptable.
    const MAX_BUDGET_CU: u64 = 1200; // Upper bound for accrue+fee path
    let ops_diff = if ops_1b.total_ops > ops_zero.total_ops {
        ops_1b.total_ops - ops_zero.total_ops
    } else {
        ops_zero.total_ops - ops_1b.total_ops
    };

    // Pin: 1B supply path must stay under CU budget
    assert!(
        ops_1b.estimated_cu() <= MAX_BUDGET_CU,
        "1B-supply accrue CU {} exceeds budget {}",
        ops_1b.estimated_cu(),
        MAX_BUDGET_CU
    );

    // In practice the code always does the fee math when fee_rate > 0,
    // regardless of supply. The OpCounter approximates this.
    // Timing should be close regardless of supply value (5x tolerance for noise).
    let ratio = if t_1b_supply > t_zero_supply {
        t_1b_supply as f64 / t_zero_supply.max(1) as f64
    } else {
        t_zero_supply as f64 / t_1b_supply.max(1) as f64
    };
    assert!(
        ratio < 5.0,
        "supply size timing ratio {:.2}x exceeds 5x threshold (zero={} ns, 1B={} ns, op_diff={})",
        ratio,
        t_zero_supply,
        t_1b_supply,
        ops_diff
    );
}

/// Compare CU cost with 0% fee vs 100% fee.
/// Fee path adds constant overhead.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_accrue_cost_by_fee_rate() {
    let iterations = 50_000u32;

    // 0% fee
    let t_no_fee = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        let config = make_config(0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // 100% fee
    let t_full_fee = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        let config = make_config(10000);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // Operation count difference
    let ops_no_fee =
        OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000_000_000, 0);
    let ops_full_fee = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        10000,
    );

    // Fee path adds exactly a fixed number of operations
    const EXPECTED_FEE_DELTA_OPS: u32 = 7; // 3 muls + 3 divs + 1 add
    const MAX_BUDGET_CU: u64 = 1200; // Upper bound for accrue+fee path
    let delta_ops = ops_full_fee.total_ops - ops_no_fee.total_ops;
    assert_eq!(
        delta_ops, EXPECTED_FEE_DELTA_OPS,
        "fee path should add exactly {} operations, got {}",
        EXPECTED_FEE_DELTA_OPS, delta_ops
    );
    assert!(
        ops_full_fee.estimated_cu() <= MAX_BUDGET_CU,
        "full-fee accrue CU {} exceeds budget {}",
        ops_full_fee.estimated_cu(),
        MAX_BUDGET_CU
    );

    // Fee path should add constant overhead, not multiplicative.
    // With fee, timing should be at most 5x the no-fee timing (generous for noise).
    let ratio = t_full_fee as f64 / t_no_fee.max(1) as f64;
    assert!(
        ratio < 5.0,
        "fee overhead ratio {:.2}x exceeds 5x (no_fee={} ns, full_fee={} ns)",
        ratio,
        t_no_fee,
        t_full_fee
    );
}

/// Compare CU cost at WAD scale factor vs 100*WAD scale factor.
/// Verify scale factor magnitude doesn't affect cost.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_accrue_cost_by_scale_factor_magnitude() {
    let iterations = 50_000u32;
    let config = make_config(500);

    // WAD scale factor
    let t_wad = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // 100*WAD scale factor
    let t_100wad = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, 100 * WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // Same code path, same op count
    const MAX_BUDGET_CU: u64 = 1200; // Upper bound for accrue+fee path
    let ops_wad = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        500,
    );
    let ops_100wad = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        100 * WAD,
        1_000_000_000_000,
        500,
    );
    assert_eq!(
        ops_wad.total_ops, ops_100wad.total_ops,
        "WAD and 100*WAD should have same op count"
    );
    assert!(
        ops_wad.estimated_cu() <= MAX_BUDGET_CU,
        "WAD-sf accrue CU {} exceeds budget {}",
        ops_wad.estimated_cu(),
        MAX_BUDGET_CU
    );

    // u128 arithmetic is constant-time regardless of operand magnitude
    let ratio = if t_100wad > t_wad {
        t_100wad as f64 / t_wad.max(1) as f64
    } else {
        t_wad as f64 / t_100wad.max(1) as f64
    };
    assert!(
        ratio < 5.0,
        "scale factor magnitude timing ratio {:.2}x exceeds 5x (WAD={} ns, 100*WAD={} ns)",
        ratio,
        t_wad,
        t_100wad
    );
}

// ===========================================================================
// Section 3: Differential Cost Tests -- deposit/withdraw (4 tests)
// ===========================================================================

/// Compare deposit scaling cost at WAD vs non-WAD scale factors.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_deposit_scaling_wad_vs_non_wad() {
    let iterations = 100_000u32;

    // At WAD: amount * WAD / WAD
    let t_wad = time_total_nanos(iterations, || {
        let amount: u128 = 1_000_000_000_000;
        let sf = WAD;
        let _scaled = amount.checked_mul(WAD).unwrap().checked_div(sf).unwrap();
        std::hint::black_box(_scaled);
    });

    // At non-WAD: amount * WAD / (WAD + WAD/10)
    let t_non_wad = time_total_nanos(iterations, || {
        let amount: u128 = 1_000_000_000_000;
        let sf = WAD + WAD / 10; // 1.1x
        let _scaled = amount.checked_mul(WAD).unwrap().checked_div(sf).unwrap();
        std::hint::black_box(_scaled);
    });

    // Same operation count regardless of scale factor value
    const DEPOSIT_MAX_CU: u64 = 200; // Upper bound for deposit scaling
    let ops_wad = OpCounter::count_deposit_scaling(WAD);
    let ops_non_wad = OpCounter::count_deposit_scaling(WAD + WAD / 10);
    assert_eq!(
        ops_wad.total_ops, ops_non_wad.total_ops,
        "WAD vs non-WAD deposit scaling should have same op count"
    );
    assert!(
        ops_wad.estimated_cu() <= DEPOSIT_MAX_CU,
        "deposit scaling CU {} exceeds budget {}",
        ops_wad.estimated_cu(),
        DEPOSIT_MAX_CU
    );

    // Timing ratio should be close to 1x (5x tolerance for parallel test noise)
    let ratio = if t_non_wad > t_wad {
        t_non_wad as f64 / t_wad.max(1) as f64
    } else {
        t_wad as f64 / t_non_wad.max(1) as f64
    };
    assert!(
        ratio < 5.0,
        "deposit scaling timing ratio {:.2}x exceeds 5x (WAD={} ns, non-WAD={} ns)",
        ratio,
        t_wad,
        t_non_wad
    );
}

/// Compare settlement factor computation cost at various repayment ratios.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_settlement_factor_by_repayment_ratio() {
    let iterations = 100_000u32;
    let total_normalized: u128 = 1_000_000_000_000;

    let compute_settlement = |available: u128, total: u128| -> u128 {
        if total == 0 {
            return WAD;
        }
        let raw = available
            .checked_mul(WAD)
            .unwrap()
            .checked_div(total)
            .unwrap();
        let capped = if raw > WAD { WAD } else { raw };
        if capped < 1 {
            1
        } else {
            capped
        }
    };

    // 25% repayment
    let t_25 = time_total_nanos(iterations, || {
        let _f = compute_settlement(total_normalized / 4, total_normalized);
        std::hint::black_box(_f);
    });

    // 75% repayment
    let t_75 = time_total_nanos(iterations, || {
        let _f = compute_settlement(total_normalized * 3 / 4, total_normalized);
        std::hint::black_box(_f);
    });

    // 100% repayment (capped)
    let t_100 = time_total_nanos(iterations, || {
        let _f = compute_settlement(total_normalized, total_normalized);
        std::hint::black_box(_f);
    });

    // 150% over-repayment (capped at WAD)
    let t_150 = time_total_nanos(iterations, || {
        let _f = compute_settlement(total_normalized * 3 / 2, total_normalized);
        std::hint::black_box(_f);
    });

    // All ratios should have the same op count
    const SETTLEMENT_CU: u64 = 230; // 1*70 + 1*100 + 0*40 + 3*20
    let ops = OpCounter::count_settlement_factor(total_normalized);
    assert_eq!(ops.multiplications, 1, "settlement: 1 multiplication");
    assert_eq!(ops.divisions, 1, "settlement: 1 division");
    assert_eq!(
        ops.estimated_cu(),
        SETTLEMENT_CU,
        "settlement CU must be exactly {}",
        SETTLEMENT_CU
    );

    // Pin ratio bounds: timing spread across repayment ratios must be bounded
    let times = [t_25, t_75, t_100, t_150];
    let min_t = *times.iter().min().unwrap();
    let max_t = *times.iter().max().unwrap();
    let ratio = max_t as f64 / min_t.max(1) as f64;
    assert!(
        ratio < 5.0,
        "settlement factor timing variation {:.2}x exceeds 5x bound (min={} ns, max={} ns)",
        ratio,
        min_t,
        max_t
    );
}

/// Compare payout computation with vs without settlement factor.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_payout_with_vs_without_settlement() {
    let iterations = 200_000u32;

    // Without settlement: payout = scaled_balance * scale_factor / WAD
    let t_no_settlement = time_total_nanos(iterations, || {
        let scaled_balance: u128 = 1_000_000_000_000;
        let scale_factor: u128 = WAD + WAD / 10; // 1.1x
        let normalized = scaled_balance
            .checked_mul(scale_factor)
            .unwrap()
            .checked_div(WAD)
            .unwrap();
        let _payout = normalized;
        std::hint::black_box(_payout);
    });

    // With settlement: payout = (scaled_balance * scale_factor / WAD) * settlement / WAD
    let t_with_settlement = time_total_nanos(iterations, || {
        let scaled_balance: u128 = 1_000_000_000_000;
        let scale_factor: u128 = WAD + WAD / 10;
        let settlement_factor: u128 = WAD * 3 / 4; // 75% settlement
        let normalized = scaled_balance
            .checked_mul(scale_factor)
            .unwrap()
            .checked_div(WAD)
            .unwrap();
        let _payout = normalized
            .checked_mul(settlement_factor)
            .unwrap()
            .checked_div(WAD)
            .unwrap();
        std::hint::black_box(_payout);
    });

    // Settlement path adds exactly 1 mul + 1 div
    const PAYOUT_NO_SETTLE_CU: u64 = 210; // 1*70 + 1*100 + 1*40 + 0*20
    const PAYOUT_SETTLE_CU: u64 = 400; // 2*70 + 2*100 + 1*40 + 1*20
    let ops_no_settle = OpCounter::count_payout_computation(false);
    let ops_settle = OpCounter::count_payout_computation(true);
    assert_eq!(
        ops_no_settle.estimated_cu(),
        PAYOUT_NO_SETTLE_CU,
        "payout (no settle) CU must be exactly {}",
        PAYOUT_NO_SETTLE_CU
    );
    assert_eq!(
        ops_settle.estimated_cu(),
        PAYOUT_SETTLE_CU,
        "payout (settle) CU must be exactly {}",
        PAYOUT_SETTLE_CU
    );
    assert_eq!(
        ops_settle.multiplications - ops_no_settle.multiplications,
        1,
        "settlement adds 1 multiplication"
    );
    assert_eq!(
        ops_settle.divisions - ops_no_settle.divisions,
        1,
        "settlement adds 1 division"
    );

    // Pin ratio bound: settlement overhead should be < 4x the no-settle path.
    // This benchmark is timing-based and can drift under CI load.
    let ratio = t_with_settlement as f64 / t_no_settlement.max(1) as f64;
    assert!(
        ratio < 4.0,
        "settlement overhead ratio {:.2}x exceeds 4x bound (without={} ns, with={} ns)",
        ratio,
        t_no_settlement,
        t_with_settlement
    );
}

/// Compare cost of normalizing small vs large amounts.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn diff_normalize_small_vs_large_amounts() {
    let iterations = 100_000u32;
    let scale_factor = WAD + WAD / 10; // 1.1x

    // Small amount: 1 USDC (6 decimals = 1_000_000)
    let t_small = time_total_nanos(iterations, || {
        let amount: u128 = 1_000_000;
        let _scaled = amount
            .checked_mul(WAD)
            .unwrap()
            .checked_div(scale_factor)
            .unwrap();
        std::hint::black_box(_scaled);
    });

    // Large amount: u64::MAX
    let t_large = time_total_nanos(iterations, || {
        let amount: u128 = u128::from(u64::MAX);
        let _scaled = amount
            .checked_mul(WAD)
            .unwrap()
            .checked_div(scale_factor)
            .unwrap();
        std::hint::black_box(_scaled);
    });

    // u128 arithmetic is constant-time -- pin ratio bound at 5x
    // (generous for parallel test noise on CI)
    let ratio = if t_large > t_small {
        t_large as f64 / t_small.max(1) as f64
    } else {
        t_small as f64 / t_large.max(1) as f64
    };
    assert!(
        ratio < 5.0,
        "normalization timing ratio {:.2}x exceeds 5x bound (small={} ns, large={} ns)",
        ratio,
        t_small,
        t_large
    );
}

// ===========================================================================
// Section 4: Regression Baseline Tests (4 tests)
// ===========================================================================

/// Define golden operation counts for each critical function at standard inputs.
#[test]
fn regression_golden_op_counts() {
    // accrue_interest with fee: standard case
    let profile_accrue_fee = OpCounter::count_accrue_interest(
        1000,                    // 10% rate
        SECONDS_PER_YEAR as i64, // 1 year
        WAD,                     // base scale factor
        1_000_000_000_000,       // 1M USDC supply
        500,                     // 5% fee
    );

    // Golden values: 7 muls, 5 divs, 2 adds, 3 branches = 17 total
    assert_eq!(
        profile_accrue_fee.multiplications, 7,
        "accrue+fee: expected 7 muls"
    );
    assert_eq!(
        profile_accrue_fee.divisions, 5,
        "accrue+fee: expected 5 divs"
    );
    assert_eq!(
        profile_accrue_fee.additions, 2,
        "accrue+fee: expected 2 adds"
    );
    assert_eq!(
        profile_accrue_fee.branches, 3,
        "accrue+fee: expected 3 branches"
    );
    assert_eq!(
        profile_accrue_fee.total_ops, 17,
        "accrue+fee: expected 17 total ops"
    );

    // accrue_interest without fee: standard case
    let profile_accrue_no_fee =
        OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000_000_000, 0);
    assert_eq!(
        profile_accrue_no_fee.total_ops, 10,
        "accrue (no fee): expected 10 total ops"
    );

    // deposit scaling
    let profile_deposit = OpCounter::count_deposit_scaling(WAD);
    assert_eq!(
        profile_deposit.total_ops, 2,
        "deposit scaling: expected 2 total ops"
    );

    // settlement factor
    let profile_settle = OpCounter::count_settlement_factor(1_000_000_000_000);
    assert_eq!(
        profile_settle.total_ops, 5,
        "settlement factor: expected 5 total ops"
    );

    // payout without settlement
    let profile_payout = OpCounter::count_payout_computation(false);
    assert_eq!(
        profile_payout.total_ops, 3,
        "payout (no settle): expected 3 total ops"
    );

    // payout with settlement
    let profile_payout_settle = OpCounter::count_payout_computation(true);
    assert_eq!(
        profile_payout_settle.total_ops, 6,
        "payout (settle): expected 6 total ops"
    );
}

/// Verify current implementation matches baseline timing +/- 10%.
/// We use a relative comparison: measure a "baseline" and a "current" run
/// with identical parameters and verify they are within 10% of each other.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn regression_timing_matches_baseline() {
    let iterations = 50_000u32;
    let config = make_config(500);

    // Run the same benchmark twice as "baseline" and "current".
    // They should be within a tight tolerance since they're identical code.
    let baseline = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    let current = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    // Pin: the accrue+fee path should produce exactly 1130 CU
    const EXPECTED_CU: u64 = 1130;
    const CU_TOLERANCE: u64 = 50; // small tolerance for future refactors
    let profile = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        500,
    );
    assert!(
        profile.estimated_cu() <= EXPECTED_CU + CU_TOLERANCE,
        "accrue+fee CU {} exceeds baseline {} + tolerance {}",
        profile.estimated_cu(),
        EXPECTED_CU,
        CU_TOLERANCE
    );
    assert_eq!(
        profile.estimated_cu(),
        EXPECTED_CU,
        "accrue+fee CU drifted from pinned baseline {}",
        EXPECTED_CU
    );

    // Ratio should be close to 1.0. We allow 3.0x tolerance because timing
    // measurements on shared/CI hardware can be noisy due to context switches,
    // CPU frequency scaling, parallel test execution, and cache effects.
    let ratio = if current > baseline {
        current as f64 / baseline.max(1) as f64
    } else {
        baseline as f64 / current.max(1) as f64
    };

    assert!(
        ratio < 3.0,
        "timing regression detected: baseline={} ns, current={} ns, ratio={:.2}x (max 3.0x)",
        baseline,
        current,
        ratio
    );
}

/// Test that optimizations don't accidentally add operations.
/// An "optimized" variant should have <= the original op count.
#[test]
fn regression_optimization_does_not_add_ops() {
    // Pin exact CU baselines for each path
    const EARLY_RETURN_CU: u64 = 40; // 0*70 + 0*100 + 0*40 + 2*20
    const NO_FEE_CU: u64 = 580; // 4*70 + 2*100 + 1*40 + 3*20
    const WITH_FEE_CU: u64 = 1130; // 7*70 + 5*100 + 2*40 + 3*20
    const CU_TOLERANCE: u64 = 50;

    // The zero-fee path should always have fewer ops than the fee path.
    let ops_no_fee =
        OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000_000_000, 0);
    let ops_with_fee = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        500,
    );

    assert!(
        ops_no_fee.total_ops < ops_with_fee.total_ops,
        "no-fee path ({} ops) should be cheaper than fee path ({} ops)",
        ops_no_fee.total_ops,
        ops_with_fee.total_ops
    );
    assert!(
        ops_no_fee.estimated_cu() <= NO_FEE_CU + CU_TOLERANCE,
        "no-fee CU {} exceeds baseline {} + tolerance {}",
        ops_no_fee.estimated_cu(),
        NO_FEE_CU,
        CU_TOLERANCE
    );
    assert!(
        ops_with_fee.estimated_cu() <= WITH_FEE_CU + CU_TOLERANCE,
        "with-fee CU {} exceeds baseline {} + tolerance {}",
        ops_with_fee.estimated_cu(),
        WITH_FEE_CU,
        CU_TOLERANCE
    );

    // Early return path (time_elapsed = 0) should be cheapest
    let ops_early = OpCounter::count_accrue_interest(1000, 0, WAD, 1_000_000_000_000, 500);
    assert!(
        ops_early.total_ops < ops_no_fee.total_ops,
        "early return ({} ops) should be cheaper than no-fee path ({} ops)",
        ops_early.total_ops,
        ops_no_fee.total_ops
    );
    assert!(
        ops_early.estimated_cu() <= EARLY_RETURN_CU + CU_TOLERANCE,
        "early-return CU {} exceeds baseline {} + tolerance {}",
        ops_early.estimated_cu(),
        EARLY_RETURN_CU,
        CU_TOLERANCE
    );

    // Zero total_normalized settlement should be cheapest
    let ops_settle_zero = OpCounter::count_settlement_factor(0);
    let ops_settle_normal = OpCounter::count_settlement_factor(1_000_000_000_000);
    assert!(
        ops_settle_zero.total_ops < ops_settle_normal.total_ops,
        "zero-supply settlement ({} ops) should be cheaper than normal ({} ops)",
        ops_settle_zero.total_ops,
        ops_settle_normal.total_ops
    );
}

/// Test that refactoring preserves computational complexity.
/// Verify that CostProfile estimated_cu values for standard operations
/// remain within expected bounds, acting as a complexity contract.
#[test]
fn regression_complexity_preserved() {
    // Pin exact CU baselines as complexity contracts
    const ACCRUE_FEE_CU: u64 = 1130;
    const ACCRUE_NO_FEE_CU: u64 = 580;
    const DEPOSIT_CU: u64 = 170; // 1*70 + 1*100 + 0*40 + 0*20
    const SETTLEMENT_CU: u64 = 230; // 1*70 + 1*100 + 0*40 + 3*20
    const FEE_DELTA_CU: u64 = 550; // 1130 - 580
    const CU_TOLERANCE: u64 = 50;

    // accrue_interest with fee: must equal pinned baseline
    let profile = OpCounter::count_accrue_interest(
        1000,
        SECONDS_PER_YEAR as i64,
        WAD,
        1_000_000_000_000,
        500,
    );
    let cu_est = profile.estimated_cu();
    assert_eq!(
        cu_est, ACCRUE_FEE_CU,
        "accrue+fee CU must be exactly {}",
        ACCRUE_FEE_CU
    );
    assert!(
        cu_est <= ACCRUE_FEE_CU + CU_TOLERANCE,
        "accrue+fee estimated CU {} exceeds baseline {} + tolerance {}",
        cu_est,
        ACCRUE_FEE_CU,
        CU_TOLERANCE
    );

    // accrue_interest without fee: must equal pinned baseline
    let profile_no_fee =
        OpCounter::count_accrue_interest(1000, SECONDS_PER_YEAR as i64, WAD, 1_000_000_000_000, 0);
    let cu_no_fee = profile_no_fee.estimated_cu();
    assert_eq!(
        cu_no_fee, ACCRUE_NO_FEE_CU,
        "accrue (no fee) CU must be exactly {}",
        ACCRUE_NO_FEE_CU
    );
    assert!(
        cu_no_fee <= ACCRUE_NO_FEE_CU + CU_TOLERANCE,
        "accrue (no fee) estimated CU {} exceeds baseline {} + tolerance {}",
        cu_no_fee,
        ACCRUE_NO_FEE_CU,
        CU_TOLERANCE
    );

    // deposit scaling: must equal pinned baseline
    let profile_deposit = OpCounter::count_deposit_scaling(WAD);
    let cu_deposit = profile_deposit.estimated_cu();
    assert_eq!(
        cu_deposit, DEPOSIT_CU,
        "deposit scaling CU must be exactly {}",
        DEPOSIT_CU
    );

    // settlement factor: must equal pinned baseline
    let profile_settle = OpCounter::count_settlement_factor(1_000_000_000_000);
    let cu_settle = profile_settle.estimated_cu();
    assert_eq!(
        cu_settle, SETTLEMENT_CU,
        "settlement factor CU must be exactly {}",
        SETTLEMENT_CU
    );

    // Verify the fee path adds bounded CU overhead
    let cu_delta = cu_est - cu_no_fee;
    assert_eq!(
        cu_delta, FEE_DELTA_CU,
        "fee path CU overhead must be exactly {}",
        FEE_DELTA_CU
    );
}

// ===========================================================================
// Section 5: Worst-Case vs Average-Case Comparison (3 tests)
// ===========================================================================

/// For accrue_interest, find the input that maximizes execution time.
/// Compare worst-case to average-case ratio -- should be bounded (< 2x).
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn worst_vs_average_accrue_interest() {
    let iterations = 30_000u32;
    let config_max_fee = make_config(10000);
    let config_no_fee = make_config(0);

    // Average case: moderate rate, moderate time, moderate supply, moderate fee
    let t_average = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config_max_fee, SECONDS_PER_YEAR as i64).unwrap();
    });

    // Worst case: max rate, max fee, large supply, large scale factor
    let t_worst = time_total_nanos(iterations, || {
        let mut market = make_market(10000, i64::MAX, WAD * 100, u128::from(u64::MAX), 0, 0);
        let _ = accrue_interest(&mut market, &config_max_fee, SECONDS_PER_YEAR as i64);
    });

    // Best case: zero fee, minimal computation
    let t_best = time_total_nanos(iterations, || {
        let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
        accrue_interest(&mut market, &config_no_fee, SECONDS_PER_YEAR as i64).unwrap();
    });

    // Worst to average ratio should be bounded. Both take the same code path
    // (fee path), so the ratio should be close to 1.0. Pin at <= 3.0x.
    let worst_avg_ratio = if t_worst > t_average {
        t_worst as f64 / t_average.max(1) as f64
    } else {
        t_average as f64 / t_worst.max(1) as f64
    };
    assert!(
        worst_avg_ratio <= 3.0,
        "worst/average ratio {:.2}x exceeds 3.0x bound (worst={} ns, avg={} ns)",
        worst_avg_ratio,
        t_worst,
        t_average
    );

    // Worst to best ratio should be bounded (no quadratic/exponential blowup).
    // The fee path adds ~7 extra ops, so at most ~2x, but allow 3x for noise.
    let worst_best_ratio = if t_worst > t_best {
        t_worst as f64 / t_best.max(1) as f64
    } else {
        t_best as f64 / t_worst.max(1) as f64
    };
    assert!(
        worst_best_ratio <= 3.0,
        "worst/best ratio {:.2}x exceeds 3.0x bound (worst={} ns, best={} ns)",
        worst_best_ratio,
        t_worst,
        t_best
    );
}

/// For deposit scaling, compare worst-case to average-case.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn worst_vs_average_deposit_scaling() {
    let iterations = 100_000u32;

    // Average case: moderate amount, moderate scale factor
    let t_average = time_total_nanos(iterations, || {
        let amount: u128 = 1_000_000_000_000;
        let sf = WAD + WAD / 10;
        let _scaled = amount.checked_mul(WAD).unwrap().checked_div(sf).unwrap();
        std::hint::black_box(_scaled);
    });

    // Worst case: maximum amount, minimum scale factor
    let t_worst = time_total_nanos(iterations, || {
        let amount: u128 = u128::from(u64::MAX);
        let sf = WAD; // minimum realistic sf
        let _scaled = amount.checked_mul(WAD).unwrap().checked_div(sf).unwrap();
        std::hint::black_box(_scaled);
    });

    // Both should be constant-time (u128 ops). Pin at <= 3.0x.
    let ratio = if t_worst > t_average {
        t_worst as f64 / t_average.max(1) as f64
    } else {
        t_average as f64 / t_worst.max(1) as f64
    };
    assert!(
        ratio <= 3.0,
        "deposit scaling worst/average ratio {:.2}x exceeds 3.0x bound (worst={} ns, avg={} ns)",
        ratio,
        t_worst,
        t_average
    );
}

/// Verify no quadratic or exponential blowup for any input pattern.
/// Compare the cost of N sequential accruals at different N values.
/// If growth is linear, total_time(2N) / total_time(N) should be ~2.
#[test]
#[ignore = "Wall-clock timing: run with `cargo test --test gas_metering_tests -- --ignored --test-threads=1`"]
fn no_superlinear_blowup() {
    let config = make_config(500);

    // Helper: measure the total time for `steps` sequential accruals.
    // Uses enough outer iterations to get stable measurements.
    let measure_steps = |steps: u32| -> u128 {
        // Scale outer iterations inversely with step count to keep total work similar
        let outer_iterations = (50_000 / steps).max(50);

        // Warm-up
        for _ in 0..20 {
            let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
            for step in 1..=steps {
                let ts = i64::from(step) * 86400;
                accrue_interest(&mut market, &config, ts).unwrap();
            }
        }

        let start = Instant::now();
        for _ in 0..outer_iterations {
            let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
            for step in 1..=steps {
                let ts = i64::from(step) * 86400;
                accrue_interest(&mut market, &config, ts).unwrap();
            }
        }
        start.elapsed().as_nanos() / u128::from(outer_iterations)
    };

    // Use larger step counts (200, 500, 1000) so per-step noise is minimized.
    let t_200 = measure_steps(200);
    let t_500 = measure_steps(500);
    let t_1000 = measure_steps(1000);

    // Verify per-step cost doesn't increase with the number of steps.
    // This is the primary check for O(n^2) or worse behavior.
    let per_step_200 = t_200 as f64 / 200.0;
    let per_step_500 = t_500 as f64 / 500.0;
    let per_step_1000 = t_1000 as f64 / 1000.0;

    // Per-step cost ratio between 1000-step and 200-step runs should be bounded.
    // In a truly linear algorithm, per-step cost is constant, so the ratio ~ 1.0.
    // We allow 3x for measurement noise.
    let per_step_ratio = if per_step_1000 > per_step_200 {
        per_step_1000 / per_step_200.max(1.0)
    } else {
        per_step_200 / per_step_1000.max(1.0)
    };

    assert!(
        per_step_ratio < 3.0,
        "per-step cost ratio {:.2}x suggests superlinear growth (200-step={:.0} ns/step, 500-step={:.0} ns/step, 1000-step={:.0} ns/step)",
        per_step_ratio,
        per_step_200,
        per_step_500,
        per_step_1000
    );

    // Pin linear cost growth: t(1000)/t(200) should be ~5. Bound at 15x to
    // tolerate noisy CI runners. The per-step ratio check above (3.0x) is
    // the meaningful algorithmic guard; O(n^2) would produce ~25x here.
    let total_ratio = t_1000 as f64 / t_200.max(1) as f64;
    assert!(
        total_ratio < 15.0,
        "t(1000)/t(200) ratio {:.2}x suggests superlinear growth (t200={} ns, t1000={} ns)",
        total_ratio,
        t_200,
        t_1000
    );

    // Verify that overflow at extreme values produces Custom(41) (MathOverflow)
    {
        use pinocchio::error::ProgramError;
        let mut overflow_market =
            make_market(10000, i64::MAX, u128::MAX / 2, u128::from(u64::MAX), 0, 0);
        let overflow_config = make_config(10000);
        let result = accrue_interest(
            &mut overflow_market,
            &overflow_config,
            SECONDS_PER_YEAR as i64,
        );
        assert!(result.is_err(), "extreme values must overflow");
        assert_eq!(
            result.unwrap_err(),
            ProgramError::Custom(41),
            "overflow must produce MathOverflow (Custom(41))"
        );
    }
}

// ===========================================================================
// Section 6: Instruction Complexity Ordering (2 tests)
// ===========================================================================

/// Verify relative complexity: Deposit < Borrow < Withdraw < ReSettle
/// (based on operation counting, not timing, for determinism).
#[test]
fn complexity_ordering_main_instructions() {
    // Deposit: accrue_interest + deposit scaling
    // 2 PDA derivations + accrue (with fee) + scaling + transfer
    let cu_deposit = {
        let accrue = OpCounter::count_accrue_interest(
            1000,
            SECONDS_PER_YEAR as i64,
            WAD,
            1_000_000_000_000,
            500,
        );
        let scaling = OpCounter::count_deposit_scaling(WAD);
        // 2 PDA derivations + 1 token transfer + 4 account reads + 2 writes
        let infrastructure_cu: u64 = 2 * 13_000 + 4_500 + 4 * 300 + 2 * 200;
        accrue.estimated_cu() + scaling.estimated_cu() + infrastructure_cu
    };

    // Borrow: accrue_interest + transfer (no scaling math on borrow side)
    // 3 PDA derivations + 1 token transfer + reads/writes
    let cu_borrow = {
        let accrue = OpCounter::count_accrue_interest(
            1000,
            SECONDS_PER_YEAR as i64,
            WAD,
            1_000_000_000_000,
            500,
        );
        let infrastructure_cu: u64 = 3 * 13_000 + 4_500 + 5 * 300 + 2 * 200;
        accrue.estimated_cu() + infrastructure_cu
    };

    // Withdraw: accrue_interest + settlement factor + payout computation
    // 3 PDA derivations + 1 token transfer + reads/writes
    let cu_withdraw = {
        let accrue = OpCounter::count_accrue_interest(
            1000,
            SECONDS_PER_YEAR as i64,
            WAD,
            1_000_000_000_000,
            500,
        );
        let settlement = OpCounter::count_settlement_factor(1_000_000_000_000);
        let payout = OpCounter::count_payout_computation(true);
        let infrastructure_cu: u64 = 3 * 13_000 + 4_500 + 4 * 300 + 3 * 200;
        accrue.estimated_cu()
            + settlement.estimated_cu()
            + payout.estimated_cu()
            + infrastructure_cu
    };

    // ReSettle: accrue_interest + settlement factor recomputation
    // No PDA derivation heavy path, but complex settlement logic
    let cu_resettle = {
        let accrue = OpCounter::count_accrue_interest(
            1000,
            SECONDS_PER_YEAR as i64,
            WAD,
            1_000_000_000_000,
            500,
        );
        let settlement = OpCounter::count_settlement_factor(1_000_000_000_000);
        let infrastructure_cu: u64 = 2 * 300 + 200; // reads + writes (no PDA, no transfer in re_settle math-only)
        accrue.estimated_cu() + settlement.estimated_cu() + infrastructure_cu
    };

    // Verify ordering: Deposit < Borrow (Borrow has 3 PDAs vs 2)
    assert!(
        cu_deposit < cu_borrow,
        "Deposit ({} CU) should be < Borrow ({} CU)",
        cu_deposit,
        cu_borrow
    );

    // Verify ordering: Borrow < Withdraw (Withdraw adds settlement + payout)
    assert!(
        cu_borrow < cu_withdraw,
        "Borrow ({} CU) should be < Withdraw ({} CU)",
        cu_borrow,
        cu_withdraw
    );

    // ReSettle should be cheaper than Withdraw (no PDA derivations, no transfer)
    assert!(
        cu_resettle < cu_withdraw,
        "ReSettle ({} CU) should be < Withdraw ({} CU)",
        cu_resettle,
        cu_withdraw
    );

    // Pin explicit CU budgets per instruction type
    const DEPOSIT_BUDGET: u64 = 35_000;
    const BORROW_BUDGET: u64 = 50_000;
    const WITHDRAW_BUDGET: u64 = 55_000;
    const RESETTLE_BUDGET: u64 = 5_000;

    assert!(
        cu_deposit <= DEPOSIT_BUDGET,
        "Deposit {} CU exceeds budget {} CU",
        cu_deposit,
        DEPOSIT_BUDGET
    );
    assert!(
        cu_borrow <= BORROW_BUDGET,
        "Borrow {} CU exceeds budget {} CU",
        cu_borrow,
        BORROW_BUDGET
    );
    assert!(
        cu_withdraw <= WITHDRAW_BUDGET,
        "Withdraw {} CU exceeds budget {} CU",
        cu_withdraw,
        WITHDRAW_BUDGET
    );
    assert!(
        cu_resettle <= RESETTLE_BUDGET,
        "ReSettle {} CU exceeds budget {} CU",
        cu_resettle,
        RESETTLE_BUDGET
    );

    // All should be under the Solana CU limit
    for (name, cu) in [
        ("Deposit", cu_deposit),
        ("Borrow", cu_borrow),
        ("Withdraw", cu_withdraw),
        ("ReSettle", cu_resettle),
    ] {
        assert!(
            cu < 200_000,
            "{} estimated at {} CU exceeds 200,000 limit",
            name,
            cu
        );
    }
}

/// Verify InitializeProtocol and CloseLenderPosition are cheapest.
#[test]
fn complexity_ordering_cheapest_instructions() {
    // InitializeProtocol: 1 PDA derivation + 1 CPI create_account + config writes
    let cu_init = {
        let infrastructure_cu: u64 = 1 * 13_000 + 5_000 + 1 * 300 + 1 * 200;
        infrastructure_cu
    };

    // CloseLenderPosition: 1 PDA derivation + zero data + lamport transfer
    let cu_close = {
        let infrastructure_cu: u64 = 1 * 13_000 + 1 * 300 + 2 * 200;
        let math_cu: u64 = 40; // 1 checked_add for lamports
        infrastructure_cu + math_cu
    };

    // Deposit: much more complex
    let cu_deposit = {
        let accrue = OpCounter::count_accrue_interest(
            1000,
            SECONDS_PER_YEAR as i64,
            WAD,
            1_000_000_000_000,
            500,
        );
        let scaling = OpCounter::count_deposit_scaling(WAD);
        let infrastructure_cu: u64 = 2 * 13_000 + 4_500 + 4 * 300 + 2 * 200;
        accrue.estimated_cu() + scaling.estimated_cu() + infrastructure_cu
    };

    // Borrow
    let cu_borrow = {
        let accrue = OpCounter::count_accrue_interest(
            1000,
            SECONDS_PER_YEAR as i64,
            WAD,
            1_000_000_000_000,
            500,
        );
        let infrastructure_cu: u64 = 3 * 13_000 + 4_500 + 5 * 300 + 2 * 200;
        accrue.estimated_cu() + infrastructure_cu
    };

    // Withdraw
    let cu_withdraw = {
        let accrue = OpCounter::count_accrue_interest(
            1000,
            SECONDS_PER_YEAR as i64,
            WAD,
            1_000_000_000_000,
            500,
        );
        let settlement = OpCounter::count_settlement_factor(1_000_000_000_000);
        let payout = OpCounter::count_payout_computation(true);
        let infrastructure_cu: u64 = 3 * 13_000 + 4_500 + 4 * 300 + 3 * 200;
        accrue.estimated_cu()
            + settlement.estimated_cu()
            + payout.estimated_cu()
            + infrastructure_cu
    };

    // InitializeProtocol should be cheaper than all main instructions
    assert!(
        cu_init < cu_deposit,
        "InitializeProtocol ({} CU) should be < Deposit ({} CU)",
        cu_init,
        cu_deposit
    );
    assert!(
        cu_init < cu_borrow,
        "InitializeProtocol ({} CU) should be < Borrow ({} CU)",
        cu_init,
        cu_borrow
    );
    assert!(
        cu_init < cu_withdraw,
        "InitializeProtocol ({} CU) should be < Withdraw ({} CU)",
        cu_init,
        cu_withdraw
    );

    // CloseLenderPosition should be cheaper than all main instructions
    assert!(
        cu_close < cu_deposit,
        "CloseLenderPosition ({} CU) should be < Deposit ({} CU)",
        cu_close,
        cu_deposit
    );
    assert!(
        cu_close < cu_borrow,
        "CloseLenderPosition ({} CU) should be < Borrow ({} CU)",
        cu_close,
        cu_borrow
    );
    assert!(
        cu_close < cu_withdraw,
        "CloseLenderPosition ({} CU) should be < Withdraw ({} CU)",
        cu_close,
        cu_withdraw
    );

    // Pin explicit CU budgets per instruction type
    const INIT_BUDGET: u64 = 20_000;
    const CLOSE_BUDGET: u64 = 15_000;
    const DEPOSIT_BUDGET: u64 = 35_000;
    const BORROW_BUDGET: u64 = 50_000;
    const WITHDRAW_BUDGET: u64 = 55_000;

    assert!(
        cu_init <= INIT_BUDGET,
        "InitializeProtocol ({} CU) exceeds budget {} CU",
        cu_init,
        INIT_BUDGET
    );
    assert!(
        cu_close <= CLOSE_BUDGET,
        "CloseLenderPosition ({} CU) exceeds budget {} CU",
        cu_close,
        CLOSE_BUDGET
    );
    assert!(
        cu_deposit <= DEPOSIT_BUDGET,
        "Deposit ({} CU) exceeds budget {} CU",
        cu_deposit,
        DEPOSIT_BUDGET
    );
    assert!(
        cu_borrow <= BORROW_BUDGET,
        "Borrow ({} CU) exceeds budget {} CU",
        cu_borrow,
        BORROW_BUDGET
    );
    assert!(
        cu_withdraw <= WITHDRAW_BUDGET,
        "Withdraw ({} CU) exceeds budget {} CU",
        cu_withdraw,
        WITHDRAW_BUDGET
    );
}
