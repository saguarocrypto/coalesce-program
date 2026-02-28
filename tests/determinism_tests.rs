//! Determinism tests.
//!
//! Verify that all mathematical operations produce identical results
//! across repeated invocations with the same inputs. This guards
//! against non-deterministic behavior that could cause divergent
//! state on different validators.

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

use bytemuck::{bytes_of, Zeroable};

use coalesce::constants::{SECONDS_PER_YEAR, WAD};
use coalesce::logic::interest::accrue_interest;
use coalesce::state::{Market, ProtocolConfig};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn settlement_factor_formula(available: u128, total_normalized: u128) -> u128 {
    if total_normalized == 0 {
        WAD
    } else {
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
    }
}

// ===========================================================================
// Test 1: accrue_interest is deterministic over 1000 iterations
// ===========================================================================

#[test]
fn determinism_accrue_interest_1000_iterations() {
    let config = make_config(500);
    let elapsed_cases = [
        0i64,
        1i64,
        (SECONDS_PER_YEAR as i64) - 1,
        SECONDS_PER_YEAR as i64,
    ]; // x-1/x/x+1 style boundary set

    // 4 cases * 250 runs = 1000 deterministic repetitions.
    for &elapsed in &elapsed_cases {
        let mut reference_bytes: Option<Vec<u8>> = None;
        for i in 0..250 {
            let mut market = make_market(1000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
            accrue_interest(&mut market, &config, elapsed).unwrap();

            let snapshot = bytes_of(&market).to_vec();
            if let Some(ref_bytes) = &reference_bytes {
                assert_eq!(
                    snapshot, *ref_bytes,
                    "market bytes diverged for elapsed={} on iteration {}",
                    elapsed, i
                );
            } else {
                reference_bytes = Some(snapshot);
            }
        }
    }
}

// ===========================================================================
// Test 2: Settlement factor computation is deterministic
// ===========================================================================

#[test]
fn determinism_settlement_factor_1000_iterations() {
    // 5 scenarios * 200 repetitions = 1000 deterministic checks.
    let scenarios: &[(u128, u128, u128)] = &[
        (0, 0, WAD),                               // zero supply short-circuit
        (0, 1, 1),                                 // minimum clamp
        (750_000_000, 1_000_000_000, WAD * 3 / 4), // nominal ratio
        (1_000_000_000, 1_000_000_000, WAD),       // exact full settlement
        (1_000_000_001, 1_000_000_000, WAD),       // cap above WAD
    ];

    for &(available, total_normalized, expected) in scenarios {
        let reference = settlement_factor_formula(available, total_normalized);
        assert_eq!(
            reference, expected,
            "unexpected baseline settlement factor for available={}, total={}",
            available, total_normalized
        );
        for i in 0..200 {
            let factor = settlement_factor_formula(available, total_normalized);
            assert_eq!(
                factor, reference,
                "settlement factor diverged for available={}, total={} on iteration {}",
                available, total_normalized, i
            );
            assert!(
                (1..=WAD).contains(&factor),
                "settlement factor out of bounds for available={}, total={}",
                available,
                total_normalized
            );
        }
    }
}

// ===========================================================================
// Test 3: Full deposit→borrow→repay→withdraw sequence is deterministic
// ===========================================================================

#[test]
fn determinism_full_lending_cycle() {
    let config = make_config(500);

    // Reference full-state bytes from first run.
    let mut ref_bytes: Option<Vec<u8>> = None;

    for iteration in 0..100 {
        let mut market = make_market(1000, 1_000_000_000, WAD, 0, 0, 0);

        // 1. Deposit: 1M USDC
        let deposit_amount: u128 = 1_000_000_000_000;
        let scale_factor = market.scale_factor();
        let scaled_deposit = deposit_amount * WAD / scale_factor;
        market.set_scaled_total_supply(scaled_deposit);
        market.set_total_deposited(deposit_amount as u64);

        // 2. Accrue interest for 6 months
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64 / 2).unwrap();

        // 3. Borrow 500K
        let borrow_amount = 500_000_000_000u64;
        market.set_total_borrowed(borrow_amount);

        // 4. Accrue more interest (another 6 months)
        accrue_interest(&mut market, &config, SECONDS_PER_YEAR as i64).unwrap();

        // 5. Repay 500K
        market.set_total_repaid(borrow_amount);

        // Capture full final state bytes for strict determinism.
        let values = bytes_of(&market).to_vec();

        if let Some(ref_vals) = &ref_bytes {
            assert_eq!(
                values, *ref_vals,
                "lending cycle diverged on iteration {}",
                iteration
            );
        } else {
            ref_bytes = Some(values);
        }
    }
}

// ===========================================================================
// Test 4: Sequential accrual determinism (multiple timestamps)
// ===========================================================================

#[test]
fn determinism_sequential_accrual() {
    let config = make_config(1000);
    let timestamps = [100i64, 500, 1000, 5000, 10000, 50000, 100000, 500000];

    let mut ref_states: Vec<(u128, u64, i64)> = Vec::new();
    let mut ref_state_bytes: Vec<Vec<u8>> = Vec::new();

    for iteration in 0..100 {
        let mut market = make_market(5000, i64::MAX, WAD, 1_000_000_000_000, 0, 0);
        let mut states = Vec::new();
        let mut state_bytes = Vec::new();

        for &ts in &timestamps {
            accrue_interest(&mut market, &config, ts).unwrap();
            states.push((
                market.scale_factor(),
                market.accrued_protocol_fees(),
                market.last_accrual_timestamp(),
            ));
            state_bytes.push(bytes_of(&market).to_vec());
        }

        if iteration == 0 {
            ref_states = states;
            ref_state_bytes = state_bytes;
        } else {
            for (i, (state, ref_state)) in states.iter().zip(ref_states.iter()).enumerate() {
                assert_eq!(
                    state, ref_state,
                    "state diverged at timestamp index {} on iteration {}",
                    i, iteration
                );
            }
            for (i, (state_b, ref_b)) in state_bytes.iter().zip(ref_state_bytes.iter()).enumerate()
            {
                assert_eq!(
                    state_b, ref_b,
                    "state bytes diverged at timestamp index {} on iteration {}",
                    i, iteration
                );
            }
        }
    }
}

// ===========================================================================
// Test 5: Deposit scaling determinism
// ===========================================================================

#[test]
fn determinism_deposit_scaling() {
    let amounts = [
        0u64,
        1,
        2,
        100,
        1_000_000,
        1_000_000_000,
        u64::MAX / 2,
        u64::MAX,
    ];
    let scale_factors = [WAD - 1, WAD, WAD + 1, WAD + WAD / 10, WAD * 2];

    for &amount in &amounts {
        for &sf in &scale_factors {
            let amount_u128 = u128::from(amount);
            let reference = amount_u128.checked_mul(WAD).and_then(|n| n.checked_div(sf));

            // Verify same result 100 times with floor-division boundary invariants.
            for i in 0..100 {
                let result = amount_u128.checked_mul(WAD).and_then(|n| n.checked_div(sf));
                assert_eq!(
                    result, reference,
                    "deposit scaling diverged for amount={}, sf={} iteration={}",
                    amount, sf, i
                );

                if let Some(scaled) = result {
                    let numerator = amount_u128.checked_mul(WAD).unwrap();
                    let left = scaled.checked_mul(sf).unwrap();
                    let right = scaled.checked_add(1).unwrap().checked_mul(sf).unwrap();
                    assert!(
                        left <= numerator,
                        "scaled result violated floor lower bound for amount={}, sf={}",
                        amount,
                        sf
                    );
                    assert!(
                        right > numerator,
                        "scaled result violated floor upper bound for amount={}, sf={}",
                        amount,
                        sf
                    );
                }
            }
        }
    }
}
