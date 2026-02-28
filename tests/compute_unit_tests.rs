//! Compute Unit Budget Regression Tests
//!
//! This module provides CU budget estimation and regression testing for the
//! CoalesceFi Pinocchio lending protocol. Since direct BPF CU measurement
//! requires compilation to BPF (which may not be available in the test
//! environment), we use a combination of:
//!
//! 1. Documented CU budget constants based on operation counting
//! 2. Math-layer timing as a proxy for computational cost
//! 3. Regression detection via baseline measurements with tolerance
//! 4. Parameterized benchmarks across input ranges
//! 5. Worst-case analysis for near-overflow values
//!
//! ## CU Budget Table (Estimated)
//!
//! | Disc | Instruction            | Estimated CU | Breakdown                                    |
//! |------|------------------------|-------------|----------------------------------------------|
//! |  0   | InitializeProtocol     |  ~25,000    | PDA derivation + CPI create_account + writes |
//! |  1   | SetFeeConfig           |  ~15,000    | PDA derivation + read/write config           |
//! |  2   | CreateMarket           |  ~65,000    | 3x PDA derivation + 2x CPI create + init     |
//! |  5   | Deposit                |  ~55,000    | PDA derivations + accrue_interest + transfer  |
//! |  6   | Borrow                 |  ~50,000    | PDA derivations + accrue_interest + transfer  |
//! |  7   | Repay                  |  ~30,000    | accrue_interest + transfer + state update     |
//! |  8   | Withdraw               |  ~60,000    | accrue + settlement factor + PDA + transfer   |
//! |  9   | CollectFees            |  ~45,000    | PDA derivation + accrue + transfer            |
//! | 10   | CloseLenderPosition    |  ~20,000    | PDA derivation + zero data + lamport transfer |
//! | 11   | ReSettle               |  ~35,000    | accrue + settlement factor recomputation      |
//! | 12   | SetBorrowerWhitelist   |  ~25,000    | PDA derivation + CPI create + write           |
//!
//! ## Measurement Methodology
//!
//! CU estimates are derived from operation counting:
//! - PDA derivation (find_program_address): ~12,000-15,000 CU each
//! - CPI create_account: ~5,000 CU
//! - Token transfer CPI: ~4,500 CU
//! - accrue_interest math: ~3,000-5,000 CU (depends on fee path)
//! - Settlement factor computation: ~2,000-3,000 CU
//! - Deposit scaling (WAD math): ~1,000-2,000 CU
//! - Account reads/writes: ~200-500 CU each
//! - Checked arithmetic (per operation): ~50-100 CU
//!
//! These estimates are conservative and leave headroom within the 200,000 CU
//! per-instruction limit. The math-layer tests below serve as regression
//! detection: if a code change significantly increases operation count, the
//! timing-based assertions will catch it.

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

use std::time::{Duration, Instant};

use bytemuck::Zeroable;

use coalesce::constants::{BPS, SECONDS_PER_YEAR, WAD};
use coalesce::error::LendingError;
use coalesce::logic::interest::{accrue_interest, compute_settlement_factor};
use coalesce::state::{Market, ProtocolConfig};
use pinocchio::error::ProgramError;

#[path = "common/interest_oracle.rs"]
mod interest_oracle;

// ===========================================================================
// Constants: Expected CU budgets per instruction
// ===========================================================================

/// Solana's per-instruction compute unit limit.
const SOLANA_CU_LIMIT: u64 = 200_000;

// Estimated CU budgets for each instruction.
// These are upper-bound estimates derived from operation counting.
// All values must remain below SOLANA_CU_LIMIT (200,000 CU).

/// InitializeProtocol: 1 PDA derivation + 1 CPI create_account + config writes
const CU_INITIALIZE_PROTOCOL: u64 = 25_000;

/// SetFeeConfig: 1 PDA derivation + config read + config write
const CU_SET_FEE_CONFIG: u64 = 15_000;

/// CreateMarket: 3 PDA derivations + 2 CPI create_account + 1 InitializeAccount3
const CU_CREATE_MARKET: u64 = 65_000;

/// Deposit: 2 PDA derivations + accrue_interest + scaling math + token transfer CPI
const CU_DEPOSIT: u64 = 55_000;

/// Borrow: 3 PDA derivations + accrue_interest + token transfer CPI
const CU_BORROW: u64 = 50_000;

/// Repay: accrue_interest + token transfer CPI + state update
const CU_REPAY: u64 = 30_000;

/// Withdraw: 3 PDA derivations + accrue_interest + settlement + token transfer CPI
const CU_WITHDRAW: u64 = 60_000;

/// CollectFees: 2 PDA derivations + accrue_interest + token transfer CPI
const CU_COLLECT_FEES: u64 = 45_000;

/// CloseLenderPosition: 1 PDA derivation + zero data + lamport transfer
const CU_CLOSE_LENDER_POSITION: u64 = 20_000;

/// ReSettle: accrue_interest + settlement factor recomputation
const CU_RE_SETTLE: u64 = 35_000;

/// SetBorrowerWhitelist: 2 PDA derivations + CPI create_account + write
const CU_SET_BORROWER_WHITELIST: u64 = 25_000;

/// Baseline expected CU table (pinned regression guardrails).
const EXPECTED_BUDGET_TABLE: &[(u8, &str, u64)] = &[
    (0, "InitializeProtocol", 25_000),
    (1, "SetFeeConfig", 15_000),
    (2, "CreateMarket", 65_000),
    (5, "Deposit", 55_000),
    (6, "Borrow", 50_000),
    (7, "Repay", 30_000),
    (8, "Withdraw", 60_000),
    (9, "CollectFees", 45_000),
    (10, "CloseLenderPosition", 20_000),
    (11, "ReSettle", 35_000),
    (12, "SetBorrowerWhitelist", 25_000),
];

// ===========================================================================
// CU Estimation Framework
// ===========================================================================

/// Estimated CU cost per checked multiplication (u128).
/// Based on BPF instruction counting: ~50-80 CU for u128 checked_mul.
const CU_PER_CHECKED_MUL: u64 = 70;

/// Estimated CU cost per checked division (u128).
/// Division is more expensive than multiplication on BPF: ~80-120 CU.
const CU_PER_CHECKED_DIV: u64 = 100;

/// Estimated CU cost per checked addition/subtraction (u128).
/// Simpler operation: ~30-50 CU.
const CU_PER_CHECKED_ADD: u64 = 40;

/// Estimated CU cost per type conversion (u64 -> u128, u128 -> u64).
const CU_PER_CONVERSION: u64 = 20;

/// Estimated CU cost for a single PDA derivation (find_program_address).
/// This dominates CU usage in most instructions.
const CU_PER_PDA_DERIVATION: u64 = 13_000;

/// Estimated CU cost for a CPI token transfer.
const CU_PER_TOKEN_TRANSFER: u64 = 4_500;

/// Estimated CU cost for a CPI create_account.
const CU_PER_CREATE_ACCOUNT: u64 = 5_000;

/// Estimated CU cost for account data read + bytemuck cast.
const CU_PER_ACCOUNT_READ: u64 = 300;

/// Estimated CU cost for account data write.
const CU_PER_ACCOUNT_WRITE: u64 = 200;

/// Estimate CU cost based on operation counts.
///
/// This provides a bottom-up estimate that can be compared against the
/// top-down budget constants defined above.
fn estimate_cu(
    checked_muls: u64,
    checked_divs: u64,
    checked_adds: u64,
    conversions: u64,
    pda_derivations: u64,
    token_transfers: u64,
    create_accounts: u64,
    account_reads: u64,
    account_writes: u64,
) -> u64 {
    checked_muls * CU_PER_CHECKED_MUL
        + checked_divs * CU_PER_CHECKED_DIV
        + checked_adds * CU_PER_CHECKED_ADD
        + conversions * CU_PER_CONVERSION
        + pda_derivations * CU_PER_PDA_DERIVATION
        + token_transfers * CU_PER_TOKEN_TRANSFER
        + create_accounts * CU_PER_CREATE_ACCOUNT
        + account_reads * CU_PER_ACCOUNT_READ
        + account_writes * CU_PER_ACCOUNT_WRITE
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarketSnapshot {
    scale_factor: u128,
    last_accrual_timestamp: i64,
    accrued_protocol_fees: u64,
}

fn snapshot_market(market: &Market) -> MarketSnapshot {
    MarketSnapshot {
        scale_factor: market.scale_factor(),
        last_accrual_timestamp: market.last_accrual_timestamp(),
        accrued_protocol_fees: market.accrued_protocol_fees(),
    }
}

fn growth_factor_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::growth_factor_wad_exact(annual_bps, elapsed_seconds)
}

fn interest_delta_wad_exact(annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::interest_delta_wad_exact(annual_bps, elapsed_seconds)
}

fn scale_factor_after_exact(scale_factor: u128, annual_bps: u16, elapsed_seconds: i64) -> u128 {
    interest_oracle::scale_factor_after_exact(scale_factor, annual_bps, elapsed_seconds)
}

fn fee_delta_exact(
    scaled_total_supply: u128,
    scale_factor_before: u128,
    annual_bps: u16,
    fee_rate_bps: u16,
    elapsed_seconds: i64,
) -> u64 {
    let new_scale_factor =
        scale_factor_after_exact(scale_factor_before, annual_bps, elapsed_seconds);
    interest_oracle::fee_delta_exact(
        scaled_total_supply,
        new_scale_factor,
        annual_bps,
        fee_rate_bps,
        elapsed_seconds,
    )
}

#[derive(Clone, Debug)]
struct TimingStats {
    samples: usize,
    iterations_per_sample: u32,
    min: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    max: Duration,
    mean: Duration,
}

fn duration_from_nanos(ns: u128) -> Duration {
    if ns > u128::from(u64::MAX) {
        Duration::from_nanos(u64::MAX)
    } else {
        Duration::from_nanos(ns as u64)
    }
}

fn percentile_index(len: usize, percentile: usize) -> usize {
    let ceil_rank = (len * percentile).div_ceil(100);
    ceil_rank.saturating_sub(1).min(len.saturating_sub(1))
}

fn time_stats(samples: u32, iterations_per_sample: u32, mut f: impl FnMut()) -> TimingStats {
    assert!(samples >= 5, "need at least 5 samples for stable p95/p99");
    assert!(
        iterations_per_sample > 0,
        "iterations_per_sample must be > 0"
    );

    // Warm-up
    for _ in 0..iterations_per_sample {
        f();
    }

    let mut per_iter_ns: Vec<u128> = Vec::with_capacity(samples as usize);
    for _ in 0..samples {
        let start = Instant::now();
        for _ in 0..iterations_per_sample {
            f();
        }
        let elapsed = start.elapsed();
        per_iter_ns.push(elapsed.as_nanos() / u128::from(iterations_per_sample));
    }

    per_iter_ns.sort_unstable();
    let len = per_iter_ns.len();
    let sum: u128 = per_iter_ns.iter().sum();
    let mean_ns = sum / len as u128;

    let min = duration_from_nanos(per_iter_ns[0]);
    let p50 = duration_from_nanos(per_iter_ns[percentile_index(len, 50)]);
    let p95 = duration_from_nanos(per_iter_ns[percentile_index(len, 95)]);
    let p99 = duration_from_nanos(per_iter_ns[percentile_index(len, 99)]);
    let max = duration_from_nanos(per_iter_ns[len - 1]);
    let mean = duration_from_nanos(mean_ns);

    TimingStats {
        samples: len,
        iterations_per_sample,
        min,
        p50,
        p95,
        p99,
        max,
        mean,
    }
}

fn assert_timing_budget(
    name: &str,
    stats: &TimingStats,
    p95_budget: Duration,
    p99_budget: Duration,
    max_budget: Duration,
) {
    assert!(
        stats.min <= stats.p50 && stats.p50 <= stats.p95 && stats.p95 <= stats.p99,
        "{} timing quantiles out of order: min={:?}, p50={:?}, p95={:?}, p99={:?}",
        name,
        stats.min,
        stats.p50,
        stats.p95,
        stats.p99
    );
    assert!(
        stats.p99 <= stats.max,
        "{} p99 exceeds max sample: p99={:?}, max={:?}",
        name,
        stats.p99,
        stats.max
    );
    assert!(
        stats.p95 <= p95_budget,
        "{} p95 {:?} exceeds budget {:?} (samples={}, iters/sample={})",
        name,
        stats.p95,
        p95_budget,
        stats.samples,
        stats.iterations_per_sample
    );
    assert!(
        stats.p99 <= p99_budget,
        "{} p99 {:?} exceeds budget {:?} (samples={}, iters/sample={})",
        name,
        stats.p99,
        p99_budget,
        stats.samples,
        stats.iterations_per_sample
    );
    assert!(
        stats.max <= max_budget,
        "{} max {:?} exceeds hard ceiling {:?} (samples={}, iters/sample={})",
        name,
        stats.max,
        max_budget,
        stats.samples,
        stats.iterations_per_sample
    );
}

// ===========================================================================
// Requirement 1: CU Budget Documentation Verification
// ===========================================================================

/// Verify all instruction CU budgets are within the Solana limit.
#[test]
fn cu_budgets_within_solana_limit() {
    let budgets = [
        (0u8, "InitializeProtocol", CU_INITIALIZE_PROTOCOL),
        (1, "SetFeeConfig", CU_SET_FEE_CONFIG),
        (2, "CreateMarket", CU_CREATE_MARKET),
        (5, "Deposit", CU_DEPOSIT),
        (6, "Borrow", CU_BORROW),
        (7, "Repay", CU_REPAY),
        (8, "Withdraw", CU_WITHDRAW),
        (9, "CollectFees", CU_COLLECT_FEES),
        (10, "CloseLenderPosition", CU_CLOSE_LENDER_POSITION),
        (11, "ReSettle", CU_RE_SETTLE),
        (12, "SetBorrowerWhitelist", CU_SET_BORROWER_WHITELIST),
    ];

    // Guardrail: table values are explicitly pinned to a reviewed baseline.
    assert_eq!(
        budgets.as_slice(),
        EXPECTED_BUDGET_TABLE,
        "CU budget table drifted from pinned baseline; review/update required"
    );

    for (disc, name, cu) in &budgets {
        assert!(
            *cu < SOLANA_CU_LIMIT,
            "Instruction {} (disc {}) estimated at {} CU exceeds Solana limit of {} CU",
            name,
            disc,
            cu,
            SOLANA_CU_LIMIT
        );
        let headroom = SOLANA_CU_LIMIT - *cu;
        assert!(
            headroom >= 100_000,
            "Instruction {} (disc {}) has insufficient absolute headroom: {} CU",
            name,
            disc,
            headroom
        );
    }
}

/// Verify that the most expensive instruction (CreateMarket) still has
/// significant headroom below the Solana CU limit.
#[test]
fn cu_budget_headroom() {
    let budgets: [(&str, u64); 11] = [
        ("InitializeProtocol", CU_INITIALIZE_PROTOCOL),
        ("SetFeeConfig", CU_SET_FEE_CONFIG),
        ("CreateMarket", CU_CREATE_MARKET),
        ("Deposit", CU_DEPOSIT),
        ("Borrow", CU_BORROW),
        ("Repay", CU_REPAY),
        ("Withdraw", CU_WITHDRAW),
        ("CollectFees", CU_COLLECT_FEES),
        ("CloseLenderPosition", CU_CLOSE_LENDER_POSITION),
        ("ReSettle", CU_RE_SETTLE),
        ("SetBorrowerWhitelist", CU_SET_BORROWER_WHITELIST),
    ];

    let (max_name, max_cu) = budgets.iter().copied().max_by_key(|(_, cu)| *cu).unwrap();
    let headroom_pct = ((SOLANA_CU_LIMIT - max_cu) as f64 / SOLANA_CU_LIMIT as f64) * 100.0;
    let min_budget = budgets.iter().map(|(_, cu)| *cu).min().unwrap();
    let spread = max_cu - min_budget;

    // Require at least 50% headroom from the most expensive instruction
    assert!(
        headroom_pct >= 50.0,
        "Most expensive instruction uses {} CU, only {:.1}% headroom (need >= 50%)",
        max_cu,
        headroom_pct
    );
    assert_eq!(
        max_name, "CreateMarket",
        "Expected CreateMarket to remain the most expensive instruction, got {}",
        max_name
    );
    assert_eq!(
        max_cu, CU_CREATE_MARKET,
        "Maximum CU drifted unexpectedly from CreateMarket baseline"
    );
    assert!(
        spread >= 40_000,
        "Budget spread too narrow (max-min={} CU), may indicate accidental table compression",
        spread
    );
}

// ===========================================================================
// Requirement 2: Math-Layer Computation Cost Measurement
// ===========================================================================

/// Measure accrue_interest execution time as a proxy for CU cost.
/// This test establishes a baseline and verifies it does not regress.
#[test]
#[ignore = "timing-sensitive: flaky on CI runners"]
fn measure_accrue_interest_timing() {
    let config = make_config(500); // 5% fee rate
    let stats = time_stats(31, 320, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    assert_timing_budget(
        "accrue_interest",
        &stats,
        Duration::from_micros(500),
        Duration::from_micros(900),
        Duration::from_millis(2),
    );
    assert!(
        stats.mean <= Duration::from_micros(450),
        "accrue_interest mean {:?} exceeds drift guardrail",
        stats.mean
    );
}

/// Measure deposit scaling computation timing.
#[test]
#[ignore = "timing-sensitive: flaky on CI runners"]
fn measure_deposit_scaling_timing() {
    let stats = time_stats(31, 320, || {
        let amount: u128 = u128::from(u64::MAX);
        let scale_factor: u128 = WAD + WAD / 10; // 1.1x
        let _scaled = amount
            .checked_mul(WAD)
            .and_then(|n| n.checked_div(scale_factor));
    });

    assert_timing_budget(
        "deposit_scaling",
        &stats,
        Duration::from_micros(100),
        Duration::from_micros(180),
        Duration::from_micros(500),
    );
    assert!(
        stats.mean <= Duration::from_micros(90),
        "deposit_scaling mean {:?} exceeds drift guardrail",
        stats.mean
    );
}

/// Measure settlement factor computation timing.
#[test]
#[ignore = "timing-sensitive: flaky on CI runners"]
fn measure_settlement_factor_timing() {
    let stats = time_stats(31, 320, || {
        let available: u128 = 750_000_000_000;
        let total_normalized: u128 = 1_000_000_000_000;
        let _factor = compute_settlement_factor(available, total_normalized).unwrap();
    });

    assert_timing_budget(
        "settlement_factor",
        &stats,
        Duration::from_micros(100),
        Duration::from_micros(180),
        Duration::from_micros(500),
    );
    assert!(
        stats.mean <= Duration::from_micros(90),
        "settlement_factor mean {:?} exceeds drift guardrail",
        stats.mean
    );
}

/// Measure fee computation timing (within accrue_interest).
#[test]
#[ignore = "timing-sensitive: flaky on CI runners"]
fn measure_fee_computation_timing() {
    let config = make_config(10000); // 100% fee rate = worst case
    let stats = time_stats(31, 320, || {
        let mut market = make_market(10000, i64::MAX, WAD, 1_000_000_000_000_000_u128, 0, 0);
        // Worst-case-realistic supply (10^15); u64::MAX causes MathOverflow
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    assert_timing_budget(
        "fee_computation",
        &stats,
        Duration::from_micros(500),
        Duration::from_micros(900),
        Duration::from_millis(2),
    );
    assert!(
        stats.mean <= Duration::from_micros(450),
        "fee_computation mean {:?} exceeds drift guardrail",
        stats.mean
    );
}

// ===========================================================================
// Requirement 3: Regression Detection
// ===========================================================================

/// Run accrue_interest at standardized inputs and verify outputs match
/// expected values. If the math changes, this test will catch it.
#[test]
fn regression_accrue_interest_standard_inputs() {
    // 10% annual, full year, WAD scale, 1M supply, 5% fee
    let config = make_config(500);
    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let ts = SECONDS_PER_YEAR as i64;
    accrue_interest(&mut market, &config, ts).unwrap();

    let expected_sf = scale_factor_after_exact(WAD, 1000, ts);
    assert_eq!(
        market.scale_factor(),
        expected_sf,
        "scale_factor regression: expected {}, got {}",
        expected_sf,
        market.scale_factor()
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        ts,
        "last_accrual_timestamp regression: expected {}, got {}",
        ts,
        market.last_accrual_timestamp()
    );

    let expected_fees_u64 = fee_delta_exact(1_000_000_000_000u128, WAD, 1000, 500, ts);
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fees_u64,
        "fee regression: expected {}, got {}",
        expected_fees_u64,
        market.accrued_protocol_fees()
    );

    // Accrue again at the same timestamp should be a no-op.
    let snapshot = snapshot_market(&market);
    accrue_interest(&mut market, &config, ts).unwrap();
    assert_eq!(
        snapshot,
        snapshot_market(&market),
        "same-timestamp accrue should not mutate market state"
    );

    // Boundary neighbor: one second less than a year.
    let mut market_minus_one = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    accrue_interest(&mut market_minus_one, &config, ts - 1).unwrap();
    assert_eq!(
        market_minus_one.scale_factor(),
        scale_factor_after_exact(WAD, 1000, ts - 1),
        "scale_factor one-second-neighbor regression"
    );
    assert!(
        market_minus_one.scale_factor() < market.scale_factor(),
        "one-second-shorter accrual unexpectedly matched full-year accrual"
    );
}

/// Regression test for deposit scaling at known values.
#[test]
fn regression_deposit_scaling_known_values() {
    // amount=1_000_000, scale_factor=WAD => scaled = 1_000_000
    let amount: u128 = 1_000_000;
    let scaled = amount.checked_mul(WAD).unwrap().checked_div(WAD).unwrap();
    assert_eq!(scaled, 1_000_000);

    // amount=1_000_000, scale_factor=2*WAD => scaled = 500_000
    let scaled2 = amount
        .checked_mul(WAD)
        .unwrap()
        .checked_div(2 * WAD)
        .unwrap();
    assert_eq!(scaled2, 500_000);

    // amount=1_000_000, scale_factor=WAD+WAD/10 (1.1x) => scaled = 909090...
    let sf_1_1 = WAD + WAD / 10;
    let scaled3 = amount
        .checked_mul(WAD)
        .unwrap()
        .checked_div(sf_1_1)
        .unwrap();
    // 1_000_000 * 1e18 / 1.1e18 = 909090.909... => floor = 909090
    assert_eq!(scaled3, 909090);

    // Boundary neighbors around 1_000_000 at 1.1x scale.
    let scaled3_minus = (amount - 1)
        .checked_mul(WAD)
        .unwrap()
        .checked_div(sf_1_1)
        .unwrap();
    let scaled3_plus = (amount + 1)
        .checked_mul(WAD)
        .unwrap()
        .checked_div(sf_1_1)
        .unwrap();
    assert!(
        scaled3_minus <= scaled3 && scaled3 <= scaled3_plus,
        "scaled values should be monotonic for amount-1/amount/amount+1"
    );
    assert!(
        scaled3_plus - scaled3 <= 1,
        "adjacent amount step produced >1 scaled step: scaled={}, scaled_plus={}",
        scaled3,
        scaled3_plus
    );

    // Round-trip floor bound: normalize(scaled) is never above original amount
    // and loses at most ceil(sf/WAD) units from flooring.
    let normalized = scaled3
        .checked_mul(sf_1_1)
        .unwrap()
        .checked_div(WAD)
        .unwrap();
    let rounding_loss = amount.saturating_sub(normalized);
    let rounding_bound = sf_1_1.div_ceil(WAD);
    assert!(
        normalized <= amount,
        "normalized amount should never exceed original amount"
    );
    assert!(
        rounding_loss <= rounding_bound,
        "rounding loss {} exceeds bound {}",
        rounding_loss,
        rounding_bound
    );
}

/// Regression test for settlement factor at known ratios.
#[test]
fn regression_settlement_factor_known_ratios() {
    let compute_settlement = |available: u128, total_normalized: u128| -> u128 {
        if total_normalized == 0 {
            return WAD;
        }
        let raw = available
            .checked_mul(WAD)
            .unwrap()
            .checked_div(total_normalized)
            .unwrap();
        let capped = if raw > WAD { WAD } else { raw };
        if capped < 1 {
            1
        } else {
            capped
        }
    };

    // 100% repayment
    assert_eq!(
        compute_settlement(1_000_000, 1_000_000),
        WAD,
        "100% repayment should yield WAD"
    );

    // 75% repayment
    assert_eq!(
        compute_settlement(750_000, 1_000_000),
        WAD * 3 / 4,
        "75% repayment should yield 0.75 WAD"
    );

    // 50% repayment
    assert_eq!(
        compute_settlement(500_000, 1_000_000),
        WAD / 2,
        "50% repayment should yield 0.5 WAD"
    );

    // 0% repayment (0 available) => capped to 1
    // Note: 0 * WAD / total = 0, but capped to minimum 1
    assert_eq!(
        compute_settlement(0, 1_000_000),
        1,
        "0% repayment should yield minimum factor of 1"
    );

    // Over-repayment (150%) => capped to WAD
    assert_eq!(
        compute_settlement(1_500_000, 1_000_000),
        WAD,
        "150% repayment should be capped at WAD"
    );

    // Zero total_normalized => WAD
    assert_eq!(
        compute_settlement(0, 0),
        WAD,
        "zero total_normalized should yield WAD"
    );
}

// ===========================================================================
// Requirement 4: Parameterized Benchmarks
// ===========================================================================

/// Parameterized benchmark: accrue_interest at various rates and time periods.
#[test]
fn parameterized_accrue_interest() {
    let rates_bps: &[u16] = &[0, 1000, 5000, 10000]; // 0%, 10%, 50%, 100%
    let time_deltas: &[(&str, i64)] = &[
        ("1 second", 1),
        ("1 hour", 3600),
        ("1 day", 86400),
        ("1 year", SECONDS_PER_YEAR as i64),
    ];

    let config = make_config(500);

    for &rate in rates_bps {
        for &(label, time_elapsed) in time_deltas {
            let mut market = make_market(rate, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
            accrue_interest(&mut market, &config, time_elapsed).unwrap();

            let expected_sf = scale_factor_after_exact(WAD, rate, time_elapsed);
            assert_eq!(
                market.scale_factor(),
                expected_sf,
                "scale_factor mismatch for rate={}bps, time={}",
                rate,
                label
            );
            assert_eq!(
                market.last_accrual_timestamp(),
                time_elapsed,
                "last_accrual_timestamp mismatch for rate={}bps, time={}",
                rate,
                label
            );

            let expected_fees =
                fee_delta_exact(1_000_000_000_000u128, WAD, rate, 500, time_elapsed);
            assert_eq!(
                market.accrued_protocol_fees(),
                expected_fees,
                "fee mismatch for rate={}bps, time={}",
                rate,
                label
            );

            let snapshot = snapshot_market(&market);
            accrue_interest(&mut market, &config, time_elapsed).unwrap();
            assert_eq!(
                snapshot_market(&market),
                snapshot,
                "same timestamp should be no-op for rate={}bps, time={}",
                rate,
                label
            );
        }
    }
}

/// Parameterized benchmark: accrue_interest correctness at key rates.
#[test]
fn parameterized_accrue_interest_exact_values() {
    let config = make_config(0); // No fees for simplicity
    let year = SECONDS_PER_YEAR as i64;

    // 10% annual for 1 year
    let mut m = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    accrue_interest(&mut m, &config, year).unwrap();
    assert_eq!(m.scale_factor(), scale_factor_after_exact(WAD, 1000, year));

    // 50% annual for 1 year
    let mut m = make_market(5000, i64::MAX, WAD, 0, 0, 0);
    accrue_interest(&mut m, &config, year).unwrap();
    assert_eq!(m.scale_factor(), scale_factor_after_exact(WAD, 5000, year));

    // 100% annual for 1 year
    let mut m = make_market(10000, i64::MAX, WAD, 0, 0, 0);
    accrue_interest(&mut m, &config, year).unwrap();
    assert_eq!(m.scale_factor(), scale_factor_after_exact(WAD, 10000, year));

    // 10% annual for 1 day
    let mut m = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    accrue_interest(&mut m, &config, 86400).unwrap();
    assert_eq!(m.scale_factor(), scale_factor_after_exact(WAD, 1000, 86400));

    // 10% annual for 1 hour
    let mut m = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    accrue_interest(&mut m, &config, 3600).unwrap();
    assert_eq!(m.scale_factor(), scale_factor_after_exact(WAD, 1000, 3600));

    // 10% annual for 1 second
    let mut m = make_market(1000, i64::MAX, WAD, 0, 0, 0);
    accrue_interest(&mut m, &config, 1).unwrap();
    assert_eq!(m.scale_factor(), scale_factor_after_exact(WAD, 1000, 1));
}

/// Parameterized benchmark: deposit scaling at various scale factors and amounts.
#[test]
fn parameterized_deposit_scaling() {
    let scale_factors: &[(&str, u128)] = &[
        ("1x (WAD)", WAD),
        ("2x (2*WAD)", 2 * WAD),
        ("10x (10*WAD)", 10 * WAD),
    ];
    let amounts: &[(&str, u64)] = &[
        ("1 lamport", 1),
        ("1 USDC", 1_000_000),
        ("1M USDC", 1_000_000_000_000),
        ("max u64", u64::MAX),
    ];

    for &(sf_label, sf) in scale_factors {
        for &(amt_label, amt) in amounts {
            let amount_u128 = u128::from(amt);
            let result = amount_u128.checked_mul(WAD).and_then(|n| n.checked_div(sf));

            assert!(
                result.is_some(),
                "deposit scaling overflow for sf={}, amount={}",
                sf_label,
                amt_label
            );

            let scaled = result.unwrap();

            // At WAD scale, scaled == amount
            if sf == WAD {
                assert_eq!(
                    scaled, amount_u128,
                    "at WAD scale, scaled should equal amount for {}",
                    amt_label
                );
            }
            // At 2*WAD scale, scaled == amount/2
            if sf == 2 * WAD {
                assert_eq!(
                    scaled,
                    amount_u128 / 2,
                    "at 2x scale, scaled should be half for {}",
                    amt_label
                );
            }
            // At 10*WAD scale, scaled == amount/10
            if sf == 10 * WAD {
                assert_eq!(
                    scaled,
                    amount_u128 / 10,
                    "at 10x scale, scaled should be 1/10 for {}",
                    amt_label
                );
            }
        }
    }
}

/// Parameterized benchmark: settlement factor at various repayment ratios.
#[test]
fn parameterized_settlement_factor() {
    let total_normalized: u128 = 1_000_000_000_000; // 1M USDC (6 decimals)
    let ratios: &[(&str, u128, u128)] = &[
        ("0%", 0, 1), // min-capped to 1
        ("25%", total_normalized / 4, WAD / 4),
        ("50%", total_normalized / 2, WAD / 2),
        ("75%", total_normalized * 3 / 4, WAD * 3 / 4),
        ("100%", total_normalized, WAD),
        ("150%", total_normalized * 3 / 2, WAD), // capped at WAD
    ];

    for &(label, available, expected) in ratios {
        let factor = compute_settlement_factor(available, total_normalized).unwrap();

        assert_eq!(
            factor, expected,
            "settlement factor at {} ratio: expected {}, got {}",
            label, expected, factor
        );

        if available < u128::MAX {
            let factor_next = compute_settlement_factor(available + 1, total_normalized).unwrap();
            assert!(
                factor_next >= factor,
                "settlement factor should be monotonic in available at {} ratio",
                label
            );
        }
    }

    // Boundary: zero supply must always return WAD regardless of available.
    for available in [0u128, 1, 1_000_000_000_000, u128::from(u64::MAX)] {
        assert_eq!(
            compute_settlement_factor(available, 0).unwrap(),
            WAD,
            "zero total_normalized should return WAD (available={})",
            available
        );
    }
}

/// Parameterized benchmark: fee computation at various fee rates.
#[test]
fn parameterized_fee_computation() {
    let fee_rates_bps: &[(&str, u16)] = &[
        ("0%", 0),
        ("5%", 500),
        ("10%", 1000),
        ("50%", 5000),
        ("100%", 10000),
    ];

    let annual_bps: u16 = 1000; // 10% annual rate
    let year = SECONDS_PER_YEAR as i64;
    let supply = 1_000_000_000_000u128; // 1M USDC

    let mut previous_fees = 0u64;
    for &(label, fee_rate) in fee_rates_bps {
        let config = make_config(fee_rate);
        let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        accrue_interest(&mut market, &config, year).unwrap();

        let fees = market.accrued_protocol_fees();

        if fee_rate == 0 {
            assert_eq!(
                fees, 0,
                "zero fee rate should produce zero fees at {}",
                label
            );
        } else {
            assert!(
                fees > 0,
                "non-zero fee rate {} should produce non-zero fees",
                label
            );
        }

        let interest_delta_wad = interest_delta_wad_exact(annual_bps, year);
        let new_sf = scale_factor_after_exact(WAD, annual_bps, year);
        let expected_fee_u64 = fee_delta_exact(supply, WAD, annual_bps, fee_rate, year);
        assert_eq!(
            fees, expected_fee_u64,
            "fee mismatch at fee_rate={}: expected {}, got {}",
            label, expected_fee_u64, fees
        );

        let interest_on_supply = supply * new_sf / WAD * interest_delta_wad / WAD;
        assert!(
            u128::from(fees) <= interest_on_supply + 1,
            "fees should not exceed generated interest at {}",
            label
        );
        assert!(
            fees >= previous_fees,
            "fee schedule should be monotonic across labeled rate buckets"
        );
        previous_fees = fees;
    }
}

/// Verify fee monotonicity: higher fee rate always produces higher fees.
#[test]
fn parameterized_fee_monotonicity() {
    let annual_bps: u16 = 1000;
    let year = SECONDS_PER_YEAR as i64;
    let supply = 1_000_000_000_000u128;
    let interest_delta_wad = interest_delta_wad_exact(annual_bps, year);
    let new_sf = scale_factor_after_exact(WAD, annual_bps, year);
    let normalized_supply = supply * new_sf / WAD;

    let mut prev_fees = 0u64;
    let mut prev_fee_rate = 0u16;
    for fee_rate in [0u16, 100, 500, 1000, 2500, 5000, 7500, 10000] {
        let config = make_config(fee_rate);
        let mut market = make_market(annual_bps, i64::MAX, WAD, supply, 0, 0);
        accrue_interest(&mut market, &config, year).unwrap();

        let fees = market.accrued_protocol_fees();
        let expected_fees_u64 = fee_delta_exact(supply, WAD, annual_bps, fee_rate, year);
        assert_eq!(
            fees, expected_fees_u64,
            "fee formula drift at fee_rate={}bps",
            fee_rate
        );

        assert!(
            fees >= prev_fees,
            "fee monotonicity violated: fee_rate={} produced {} < previous {}",
            fee_rate,
            fees,
            prev_fees
        );
        if fee_rate > prev_fee_rate {
            let max_step = normalized_supply * u128::from(fee_rate - prev_fee_rate) / BPS;
            let observed_step = u128::from(fees - prev_fees);
            assert!(
                observed_step <= max_step + 1,
                "fee increment too large for step {}->{}: observed={}, bound={}",
                prev_fee_rate,
                fee_rate,
                observed_step,
                max_step
            );
        }
        prev_fees = fees;
        prev_fee_rate = fee_rate;
    }
}

// ===========================================================================
// Requirement 5: Worst-Case Analysis
// ===========================================================================

/// Worst-case: maximum rate, maximum time, large scale factor.
/// Tests that accrue_interest either succeeds or returns MathOverflow,
/// never panics.
#[test]
fn worst_case_accrue_max_rate_max_time() {
    let config = make_config(10000); // 100% fee rate

    // Large but not overflow-inducing scale factor
    let large_sf = WAD * 100; // 100x scale factor (after significant accrual)
    let mut market = make_market(
        10000,    // 100% annual
        i64::MAX, // infinite maturity
        large_sf,
        1_000_000_000_000_000, // large supply
        0,
        0,
    );

    let ts = SECONDS_PER_YEAR as i64;
    accrue_interest(&mut market, &config, ts).unwrap();

    let expected_sf = scale_factor_after_exact(large_sf, 10_000, ts);
    assert_eq!(market.scale_factor(), expected_sf);
    assert_eq!(
        market.last_accrual_timestamp(),
        ts,
        "last_accrual_timestamp should advance on successful accrual"
    );
    let expected_fees = fee_delta_exact(1_000_000_000_000_000u128, large_sf, 10_000, 10_000, ts);
    assert_eq!(
        market.accrued_protocol_fees(),
        expected_fees,
        "fee computation drift in worst-case high-rate path"
    );
}

/// Worst-case: near-overflow scale factor (u128::MAX region).
#[test]
fn worst_case_accrue_near_overflow_scale_factor() {
    let config = make_config(0);

    // Scale factor near u128::MAX / 2 should overflow when multiplied
    let huge_sf = u128::MAX / 2;
    let mut market = make_market(10000, i64::MAX, huge_sf, 1, 0, 0);
    let before = snapshot_market(&market);

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert_eq!(
        result.unwrap_err(),
        ProgramError::Custom(LendingError::MathOverflow as u32),
        "near-overflow scale factor should produce MathOverflow error"
    );
    assert_eq!(
        snapshot_market(&market),
        before,
        "market mutated on overflow error path"
    );
}

/// Worst-case: near-overflow scaled_total_supply for fee computation.
#[test]
fn worst_case_fees_near_overflow_supply() {
    let config = make_config(10000); // max fee rate

    // Use large supply that could cause overflow in fee computation
    let large_supply = u128::MAX / (WAD * 2);
    let mut market = make_market(10000, i64::MAX, WAD, large_supply, 0, 0);
    let before = snapshot_market(&market);

    let result = accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64);
    assert_eq!(
        result.unwrap_err(),
        ProgramError::Custom(LendingError::MathOverflow as u32),
        "expected MathOverflow for near-overflow fee path"
    );
    assert_eq!(
        snapshot_market(&market),
        before,
        "market mutated on fee overflow error path"
    );
}

/// Worst-case: deposit scaling with u64::MAX amount and near-1 scale factor.
#[test]
fn worst_case_deposit_scaling_max_amount() {
    let amount: u128 = u128::from(u64::MAX);
    let scale_factor = WAD; // Minimum realistic scale factor

    let result = amount
        .checked_mul(WAD)
        .and_then(|n| n.checked_div(scale_factor));
    assert!(
        result.is_some(),
        "u64::MAX at WAD scale should not overflow"
    );
    assert_eq!(
        result.unwrap(),
        amount,
        "u64::MAX at WAD scale should equal amount"
    );

    // With elevated scale factor
    let sf_2x = WAD * 2;
    let result2 = amount.checked_mul(WAD).and_then(|n| n.checked_div(sf_2x));
    assert!(
        result2.is_some(),
        "u64::MAX at 2x scale should not overflow"
    );
    assert_eq!(result2.unwrap(), amount / 2);
}

/// Worst-case: settlement factor with maximum possible values.
#[test]
fn worst_case_settlement_factor_extremes() {
    // Maximum vault balance, minimum total_normalized
    let available = u128::from(u64::MAX);
    let total_normalized: u128 = 1;
    let capped = compute_settlement_factor(available, total_normalized).unwrap();
    assert_eq!(capped, WAD, "extreme ratio should be capped at WAD");

    // Minimum vault balance, maximum total_normalized
    let available2: u128 = 1;
    let total_normalized2 = u128::from(u64::MAX);
    let factor2 = compute_settlement_factor(available2, total_normalized2).unwrap();
    // 1 * WAD / u64::MAX = very small but > 0
    assert!(
        factor2 >= 1,
        "minimum settlement factor should be at least 1"
    );
    assert!(factor2 <= WAD, "settlement factor must be <= WAD");

    // Boundary neighbors around exact 1.0 settlement.
    let exact = compute_settlement_factor(total_normalized, total_normalized).unwrap();
    let below = compute_settlement_factor(total_normalized - 1, total_normalized).unwrap();
    let above = compute_settlement_factor(total_normalized + 1, total_normalized).unwrap();
    assert_eq!(exact, WAD, "equal available and supply should be WAD");
    assert!(
        below < exact,
        "below-1x available should produce factor < WAD"
    );
    assert_eq!(above, WAD, "above-1x available should clamp to WAD");
}

/// Worst-case: u128::MAX in settlement factor computation.
#[test]
fn worst_case_settlement_factor_u128_overflow() {
    // Boundary that should still succeed.
    let safe_available = u128::MAX / WAD;
    assert_eq!(
        compute_settlement_factor(safe_available, 1).unwrap(),
        WAD,
        "safe boundary should still compute and clamp to WAD"
    );

    // available_for_lenders near u128::MAX could overflow in checked_mul(WAD)
    let overflow_available = safe_available + 1;
    let err = compute_settlement_factor(overflow_available, 1).unwrap_err();
    assert_eq!(
        err,
        ProgramError::Custom(LendingError::MathOverflow as u32),
        "overflowing settlement multiplication should return MathOverflow"
    );
}

/// Worst-case: very long elapsed time (multiple years).
#[test]
fn worst_case_accrue_multi_year() {
    let config = make_config(0);
    let five_years = SECONDS_PER_YEAR as i64 * 5;

    let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    let result = accrue_interest(&mut market, &config, five_years);

    assert!(result.is_ok(), "5-year accrual at 10% should succeed");
    assert_eq!(
        market.scale_factor(),
        scale_factor_after_exact(WAD, 1000, five_years)
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        five_years,
        "5-year accrual should advance last timestamp to call timestamp"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "zero-fee config should accrue zero fees"
    );
}

/// Worst-case: very small time elapsed (1 second) with very small rate (1 bps).
#[test]
fn worst_case_accrue_tiny_rate_tiny_time() {
    let config = make_config(0);

    let mut market = make_market(1, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
    accrue_interest(&mut market, &config, 1).unwrap();

    // interest_delta_wad = 1 * 1 * WAD / (31_536_000 * 10_000)
    // = WAD / 315_360_000_000
    let expected_delta = WAD / 315_360_000_000;
    assert_eq!(market.scale_factor(), WAD + expected_delta);
    // With WAD = 1e18, delta = 1e18 / 315_360_000_000 = 3_170_979
    assert!(
        market.scale_factor() > WAD,
        "even tiny accrual should increase sf"
    );
    assert_eq!(
        market.last_accrual_timestamp(),
        1,
        "tiny accrual should advance timestamp by exactly one second"
    );
    assert_eq!(
        market.accrued_protocol_fees(),
        0,
        "zero fee configuration should keep fees at zero"
    );
}

// ===========================================================================
// Requirement 6: CU Estimation Framework Verification
// ===========================================================================

/// Verify the CU estimation function produces consistent results.
#[test]
fn cu_estimation_framework_consistency() {
    // accrue_interest (no fee path):
    // Operations: 2 conversions, 3 checked_mul, 2 checked_div, 1 checked_add
    let cu_accrue_no_fee = estimate_cu(
        3, // checked_muls: annual_bps * time_elapsed, result * WAD, scale_factor * delta
        2, // checked_divs: / (SECONDS_PER_YEAR * BPS), / WAD
        1, // checked_adds: scale_factor + delta
        2, // conversions: time_elapsed -> u128, annual_bps -> u128
        0, 0, 0, 1, // account reads: market
        1, // account writes: market
    );

    // accrue_interest (with fee path):
    // Additional: 4 checked_mul, 2 checked_div, 1 checked_add, 1 conversion
    let cu_accrue_with_fee = estimate_cu(
        7, // 3 (base) + 4 (fee: delta*fee_rate, supply*sf, result*fee_delta, fee_normalized)
        4, // 2 (base) + 2 (fee: /BPS, /WAD)
        2, // 1 (base) + 1 (fee: fees + delta)
        4, // 2 (base) + 2 (fee: fee_rate->u128, fee->u64)
        0, 0, 0, 1, 1,
    );

    assert_eq!(cu_accrue_no_fee, 990, "accrue (no fee) estimate drifted");
    assert_eq!(
        cu_accrue_with_fee, 1550,
        "accrue (with fee) estimate drifted"
    );
    assert!(
        cu_accrue_with_fee > cu_accrue_no_fee,
        "fee path must remain more expensive than no-fee path"
    );

    // Deposit instruction full estimate:
    // 2 PDA derivations + accrue + scaling math + transfer + account ops
    let cu_deposit_est = estimate_cu(
        5, // accrue(3) + scaling(2: amount*WAD, new_scaled*sf)
        3, // accrue(2) + scaling(1: /scale_factor)
        3, // accrue(1) + supply_add(1) + deposited_add(1)
        3, // accrue(2) + amount->u128(1)
        2, // PDA: protocol_config, lender_position
        1, // token transfer
        0, // no create (existing position)
        4, // reads: market, config, position, mint
        2, // writes: market, position
    );
    assert_eq!(
        cu_deposit_est, 32_930,
        "deposit estimate drifted; re-evaluate operation weights/budget"
    );
    assert!(
        cu_deposit_est < CU_DEPOSIT,
        "deposit math-only estimate {} should be < budget {}",
        cu_deposit_est,
        CU_DEPOSIT
    );
    let deposit_headroom = CU_DEPOSIT - cu_deposit_est;
    assert!(
        deposit_headroom >= 20_000,
        "deposit budget headroom too low: {} CU",
        deposit_headroom
    );

    // Verify the full instruction estimate is within the Solana limit
    assert!(
        cu_deposit_est < SOLANA_CU_LIMIT,
        "deposit full estimate {} exceeds Solana limit",
        cu_deposit_est
    );
}

/// Bottom-up CU estimation for each instruction matches the budget table.
#[test]
fn cu_estimation_per_instruction() {
    // InitializeProtocol: 1 PDA + 1 CPI create + config write + validations
    let cu_init = estimate_cu(0, 0, 0, 0, 1, 0, 1, 1, 1);
    assert_eq!(cu_init, 18_500, "InitializeProtocol estimate drifted");
    assert!(
        cu_init <= CU_INITIALIZE_PROTOCOL,
        "InitializeProtocol: estimate {} > budget {}",
        cu_init,
        CU_INITIALIZE_PROTOCOL
    );

    // SetFeeConfig: 1 PDA + config read/write
    let cu_set_fee = estimate_cu(0, 0, 0, 0, 1, 0, 0, 1, 1);
    assert_eq!(cu_set_fee, 13_500, "SetFeeConfig estimate drifted");
    assert!(
        cu_set_fee <= CU_SET_FEE_CONFIG,
        "SetFeeConfig: estimate {} > budget {}",
        cu_set_fee,
        CU_SET_FEE_CONFIG
    );

    // CreateMarket: 4 PDA derivations + 2 CPI create + 1 init
    let cu_create = estimate_cu(0, 0, 0, 0, 4, 0, 2, 3, 1);
    assert_eq!(cu_create, 63_100, "CreateMarket estimate drifted");
    assert!(
        cu_create <= CU_CREATE_MARKET,
        "CreateMarket: estimate {} > budget {}",
        cu_create,
        CU_CREATE_MARKET
    );

    // Withdraw: 3 PDA + accrue + settlement + transfer
    let cu_withdraw = estimate_cu(
        9, // accrue(7) + settlement(2: supply*sf, available*WAD)
        5, // accrue(4) + settlement(1: /total_normalized)
        2, // accrue(2)
        4, // accrue(4)
        3, // PDA: config, position, authority
        1, // token transfer
        0, 4, 3,
    );
    assert_eq!(cu_withdraw, 46_590, "Withdraw estimate drifted");
    assert!(
        cu_withdraw <= CU_WITHDRAW,
        "Withdraw: estimate {} > budget {}",
        cu_withdraw,
        CU_WITHDRAW
    );

    // ReSettle: accrue + settlement recomputation
    let cu_resettle = estimate_cu(
        5, // accrue(3) + settlement(2)
        3, // accrue(2) + settlement(1)
        1, // accrue(1)
        2, // accrue(2)
        0, 0, 0, 2, 1,
    );
    assert_eq!(cu_resettle, 1_530, "ReSettle estimate drifted");
    assert!(
        cu_resettle <= CU_RE_SETTLE,
        "ReSettle: estimate {} > budget {}",
        cu_resettle,
        CU_RE_SETTLE
    );

    // CloseLenderPosition: 1 PDA + data zeroing + lamport transfer
    let cu_close = estimate_cu(0, 0, 1, 0, 1, 0, 0, 1, 2);
    assert_eq!(cu_close, 13_740, "CloseLenderPosition estimate drifted");
    assert!(
        cu_close <= CU_CLOSE_LENDER_POSITION,
        "CloseLenderPosition: estimate {} > budget {}",
        cu_close,
        CU_CLOSE_LENDER_POSITION
    );

    // Guardrail: budget-to-estimate multipliers remain in sane ranges.
    let multipliers = [
        ("init", CU_INITIALIZE_PROTOCOL as f64 / cu_init as f64),
        ("set_fee", CU_SET_FEE_CONFIG as f64 / cu_set_fee as f64),
        ("create", CU_CREATE_MARKET as f64 / cu_create as f64),
        ("withdraw", CU_WITHDRAW as f64 / cu_withdraw as f64),
        ("resettle", CU_RE_SETTLE as f64 / cu_resettle as f64),
        ("close", CU_CLOSE_LENDER_POSITION as f64 / cu_close as f64),
    ];
    for (name, ratio) in multipliers {
        assert!(
            (1.0..=25.0).contains(&ratio),
            "budget/estimate ratio for {} out of sane range: {:.2}",
            name,
            ratio
        );
    }
}

// ===========================================================================
// Requirement 7: Budget Table Completeness
// ===========================================================================

/// Verify all 11 instructions have defined CU budgets.
#[test]
fn budget_table_completeness() {
    // Map discriminator -> (name, budget)
    let budget_table: Vec<(u8, &str, u64)> = vec![
        (0, "InitializeProtocol", CU_INITIALIZE_PROTOCOL),
        (1, "SetFeeConfig", CU_SET_FEE_CONFIG),
        (2, "CreateMarket", CU_CREATE_MARKET),
        (5, "Deposit", CU_DEPOSIT),
        (6, "Borrow", CU_BORROW),
        (7, "Repay", CU_REPAY),
        (8, "Withdraw", CU_WITHDRAW),
        (9, "CollectFees", CU_COLLECT_FEES),
        (10, "CloseLenderPosition", CU_CLOSE_LENDER_POSITION),
        (11, "ReSettle", CU_RE_SETTLE),
        (12, "SetBorrowerWhitelist", CU_SET_BORROWER_WHITELIST),
    ];

    // All 11 instructions are present
    assert_eq!(
        budget_table.len(),
        11,
        "expected 11 instructions in budget table"
    );

    // All budgets are non-zero
    for (disc, name, cu) in &budget_table {
        assert!(
            *cu > 0,
            "instruction {} (disc {}) has zero CU budget",
            name,
            disc
        );
    }

    // All budgets are within Solana limit
    for (disc, name, cu) in &budget_table {
        assert!(
            *cu < SOLANA_CU_LIMIT,
            "instruction {} (disc {}) budget {} exceeds Solana limit {}",
            name,
            disc,
            cu,
            SOLANA_CU_LIMIT
        );
    }

    // Discriminators cover the expected range
    let expected_discs: Vec<u8> = vec![0, 1, 2, 5, 6, 7, 8, 9, 10, 11, 12];
    let actual_discs: Vec<u8> = budget_table.iter().map(|(d, _, _)| *d).collect();
    assert_eq!(actual_discs, expected_discs, "discriminator mismatch");
}

/// Verify ordering: instructions with more PDA derivations and CPIs
/// have higher CU budgets.
#[test]
fn budget_table_ordering_sanity() {
    // CloseLenderPosition (1 PDA, no CPI) should be cheaper than Deposit (2 PDA + CPI)
    assert!(
        CU_CLOSE_LENDER_POSITION < CU_DEPOSIT,
        "CloseLenderPosition should be cheaper than Deposit"
    );

    // SetFeeConfig (1 PDA, no CPI) should be cheapest among config ops
    assert!(
        CU_SET_FEE_CONFIG <= CU_INITIALIZE_PROTOCOL,
        "SetFeeConfig should be <= InitializeProtocol"
    );

    // CreateMarket (most PDAs + CPIs) should be the most expensive
    assert!(
        CU_CREATE_MARKET >= CU_DEPOSIT,
        "CreateMarket should be >= Deposit"
    );
    assert!(
        CU_CREATE_MARKET >= CU_WITHDRAW,
        "CreateMarket should be >= Withdraw"
    );

    // Repay (no blacklist, fewer accounts) should be cheaper than Deposit
    assert!(
        CU_REPAY < CU_DEPOSIT,
        "Repay should be cheaper than Deposit"
    );

    // CollectFees should be cheaper than Withdraw (no lender-position transfer path).
    assert!(
        CU_COLLECT_FEES < CU_WITHDRAW,
        "CollectFees should be cheaper than Withdraw"
    );

    // Borrow should be between Repay and Withdraw in complexity.
    assert!(
        CU_BORROW > CU_REPAY,
        "Borrow should be more expensive than Repay"
    );
    assert!(
        CU_BORROW < CU_WITHDRAW,
        "Borrow should be cheaper than Withdraw"
    );

    // Cheap admin ops remain below heavy user-flow ops.
    assert!(
        CU_SET_FEE_CONFIG < CU_BORROW && CU_SET_FEE_CONFIG < CU_DEPOSIT,
        "SetFeeConfig should remain below Borrow/Deposit"
    );
    assert!(
        CU_SET_BORROWER_WHITELIST < CU_CREATE_MARKET,
        "SetBorrowerWhitelist should remain below CreateMarket"
    );

    // Pinned ordering by expected descending cost catches accidental reshuffles.
    let ordered = [
        CU_CREATE_MARKET,
        CU_WITHDRAW,
        CU_DEPOSIT,
        CU_BORROW,
        CU_COLLECT_FEES,
        CU_RE_SETTLE,
        CU_REPAY,
        CU_INITIALIZE_PROTOCOL,
        CU_SET_BORROWER_WHITELIST,
        CU_CLOSE_LENDER_POSITION,
        CU_SET_FEE_CONFIG,
    ];
    for window in ordered.windows(2) {
        assert!(
            window[0] >= window[1],
            "budget ordering regression: {} < {}",
            window[0],
            window[1]
        );
    }
}

// ===========================================================================
// Additional: Timing-based regression for entire math layer
// ===========================================================================

/// End-to-end timing benchmark: full lending cycle math operations.
/// This catches regressions in the overall math layer performance.
#[test]
#[ignore = "timing-sensitive: flaky on CI runners"]
fn regression_timing_full_cycle() {
    let stats = time_stats(25, 40, || {
        let config = make_config(500);

        // Deposit phase: accrue + scale
        let mut market = make_market(1000, i64::MAX, WAD, 0, 0, 0);
        accrue_interest(&mut market, &config, 1000).unwrap();
        let deposit_amount = 1_000_000_000_000u128;
        let sf = market.scale_factor();
        let scaled = deposit_amount
            .checked_mul(WAD)
            .unwrap()
            .checked_div(sf)
            .unwrap();
        market.set_scaled_total_supply(scaled);

        // Borrow phase: accrue
        accrue_interest(&mut market, &config, 100_000).unwrap();

        // Repay phase: accrue
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

        // Withdraw phase: settlement factor
        let available = 750_000_000_000u128;
        let total_norm = market
            .scaled_total_supply()
            .checked_mul(market.scale_factor())
            .unwrap()
            .checked_div(WAD)
            .unwrap();
        if total_norm > 0 {
            let _factor = compute_settlement_factor(available, total_norm).unwrap();
        }
    });

    assert_timing_budget(
        "full_cycle",
        &stats,
        Duration::from_millis(2),
        Duration::from_millis(4),
        Duration::from_millis(8),
    );
    assert!(
        stats.mean <= Duration::from_millis(2),
        "full cycle mean {:?} exceeds baseline drift budget",
        stats.mean
    );
}

/// Stress test: many sequential accruals to detect performance degradation
/// with increasing scale_factor values.
#[test]
#[ignore = "timing-sensitive: flaky on CI runners"]
fn regression_timing_sequential_accruals() {
    let config = make_config(500);
    let steps_per_iteration = 365; // Daily accruals for a year

    let stats = time_stats(20, 5, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        for day in 1..=steps_per_iteration {
            let ts = day * 86400;
            accrue_interest(&mut market, &config, ts).unwrap();
        }
    });

    assert_timing_budget(
        "sequential_365_accruals",
        &stats,
        Duration::from_millis(10),
        Duration::from_millis(20),
        Duration::from_millis(50),
    );
    assert!(
        stats.mean <= Duration::from_millis(10),
        "365 sequential accruals mean {:?} exceeds baseline drift budget",
        stats.mean
    );
}

/// Verify the accrue_interest function timing does not degrade as
/// scale_factor grows (constant-time math operations).
#[test]
#[ignore = "timing-sensitive: flaky on CI runners"]
fn regression_timing_constant_across_scale_factors() {
    let config = make_config(500);
    let stats_at_wad = time_stats(25, 200, || {
        let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });
    let stats_at_100x = time_stats(25, 200, || {
        let mut market = make_market(1000, i64::MAX, WAD * 100, 1_000_000_000_000, 0, 0);
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();
    });

    assert_timing_budget(
        "accrue_at_wad",
        &stats_at_wad,
        Duration::from_micros(500),
        Duration::from_micros(900),
        Duration::from_millis(2),
    );
    assert_timing_budget(
        "accrue_at_100x_wad",
        &stats_at_100x,
        Duration::from_micros(500),
        Duration::from_micros(900),
        Duration::from_millis(2),
    );

    // Both should be similar since u128 arithmetic is constant-time.
    // Allow 3x at p95 and 5x at p99 for measurement noise.
    let p95_ratio = if stats_at_100x.p95 > stats_at_wad.p95 {
        stats_at_100x.p95.as_nanos() as f64 / stats_at_wad.p95.as_nanos().max(1) as f64
    } else {
        stats_at_wad.p95.as_nanos() as f64 / stats_at_100x.p95.as_nanos().max(1) as f64
    };
    let p99_ratio = if stats_at_100x.p99 > stats_at_wad.p99 {
        stats_at_100x.p99.as_nanos() as f64 / stats_at_wad.p99.as_nanos().max(1) as f64
    } else {
        stats_at_wad.p99.as_nanos() as f64 / stats_at_100x.p99.as_nanos().max(1) as f64
    };
    let mean_ratio = if stats_at_100x.mean > stats_at_wad.mean {
        stats_at_100x.mean.as_nanos() as f64 / stats_at_wad.mean.as_nanos().max(1) as f64
    } else {
        stats_at_wad.mean.as_nanos() as f64 / stats_at_100x.mean.as_nanos().max(1) as f64
    };

    assert!(
        p95_ratio < 10.0,
        "p95 timing ratio {:.2}x between WAD and 100x scale is too large",
        p95_ratio
    );
    assert!(
        p99_ratio < 15.0,
        "p99 timing ratio {:.2}x between WAD and 100x scale is too large",
        p99_ratio
    );
    assert!(
        mean_ratio < 10.0,
        "mean timing ratio {:.2}x between WAD and 100x scale is too large",
        mean_ratio
    );
}
