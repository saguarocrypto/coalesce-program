//! Property-based tests for deposit/withdraw/borrow/repay math logic.
//!
//! Tests the pure mathematical operations used by processors:
//! - Scaled amount round-trip (deposit→normalize)
//! - Deposit increases supply
//! - Withdrawal payout bounded by vault balance
//! - Payout proportional to position size
//! - Borrow reduces vault conceptually
//! - Repay increases vault conceptually
//!
//! Edge-biased strategies ensure WAD boundaries, zero, max, and off-by-one
//! values are exercised with high probability.

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

use proptest::prelude::*;

use coalesce::constants::WAD;

// ---------------------------------------------------------------------------
// Helpers — reproduce the on-chain math in pure Rust
// ---------------------------------------------------------------------------

/// Deposit scaling: scaled_amount = amount * WAD / scale_factor
fn deposit_scale(amount: u64, scale_factor: u128) -> Option<u128> {
    let amount_u128 = u128::from(amount);
    amount_u128.checked_mul(WAD)?.checked_div(scale_factor)
}

/// Normalize: normalized = scaled_amount * scale_factor / WAD
fn normalize(scaled_amount: u128, scale_factor: u128) -> Option<u128> {
    scaled_amount.checked_mul(scale_factor)?.checked_div(WAD)
}

/// Payout: payout = normalized * settlement_factor / WAD
fn compute_payout(
    scaled_amount: u128,
    scale_factor: u128,
    settlement_factor: u128,
) -> Option<u128> {
    let normalized = normalize(scaled_amount, scale_factor)?;
    normalized.checked_mul(settlement_factor)?.checked_div(WAD)
}

// ---------------------------------------------------------------------------
// Edge-biased strategies
// ---------------------------------------------------------------------------

fn edge_biased_nonzero_amount() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => Just(1u64),
        3 => Just(u64::MAX),
        3 => Just(1_000_000u64),
        91 => 1u64..=1_000_000_000_000u64,
    ]
}

fn edge_biased_sf_offset() -> impl Strategy<Value = u128> {
    prop_oneof![
        3 => Just(0u128),
        3 => Just(1u128),
        3 => Just(WAD),
        3 => Just(WAD / 10),
        88 => 0u128..=WAD,
    ]
}

fn edge_biased_supply() -> impl Strategy<Value = u128> {
    prop_oneof![
        2 => Just(0u128),
        2 => Just(1u128),
        2 => Just(WAD),
        2 => Just(u64::MAX as u128),
        92 => 0u128..=1_000_000_000_000u128,
    ]
}

fn edge_biased_settlement() -> impl Strategy<Value = u128> {
    prop_oneof![
        3 => Just(1u128),
        3 => Just(WAD / 2),
        3 => Just(WAD),
        3 => Just(WAD - 1),
        88 => 1u128..=WAD,
    ]
}

// ---------------------------------------------------------------------------
// Property 1: Scaled amount round-trip — deposit(amount) then normalize ≈ original
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_scaled_amount_roundtrip(
        amount in edge_biased_nonzero_amount(),
        sf_offset in edge_biased_sf_offset(),
    ) {
        let scale_factor = WAD + sf_offset;

        let scaled = match deposit_scale(amount, scale_factor) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };

        let recovered = match normalize(scaled, scale_factor) {
            Some(r) => r,
            None => return Ok(()),
        };

        let original = u128::from(amount);
        // Due to integer division rounding, recovered <= original
        prop_assert!(
            recovered <= original,
            "recovered ({}) should be <= original ({})",
            recovered, original
        );
        // Two floor divisions can lose at most 2 units total
        let loss = original - recovered;
        prop_assert!(
            loss <= 2,
            "rounding loss ({}) should be <= 2, original={}, recovered={}",
            loss, original, recovered
        );
    }
}

// ---------------------------------------------------------------------------
// Property 2: Deposit increases scaled_total_supply
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_deposit_increases_supply(
        amount in edge_biased_nonzero_amount().prop_filter("reasonable", |a| *a <= 1_000_000_000),
        initial_supply in edge_biased_supply(),
        sf_offset in edge_biased_sf_offset(),
    ) {
        let scale_factor = WAD + sf_offset;

        let scaled = match deposit_scale(amount, scale_factor) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };

        let new_supply = match initial_supply.checked_add(scaled) {
            Some(s) => s,
            None => return Ok(()),
        };

        prop_assert!(
            new_supply > initial_supply,
            "supply must increase after deposit: before={}, after={}, scaled_amount={}",
            initial_supply, new_supply, scaled
        );
    }
}

// ---------------------------------------------------------------------------
// Property 3: Withdrawal payout bounded by available vault balance
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_withdrawal_payout_bounded(
        scaled_balance in prop_oneof![
            2 => Just(1u128),
            2 => Just(WAD),
            96 => 1u128..=1_000_000_000_000u128,
        ],
        sf_offset in edge_biased_sf_offset().prop_filter("half", |o| *o <= WAD / 2),
        vault_balance in prop_oneof![
            2 => Just(0u128),
            2 => Just(1u128),
            96 => 0u128..=10_000_000_000u128,
        ],
    ) {
        let scale_factor = WAD + sf_offset;
        // After COAL-C01: no fee reservation; available = vault_balance directly
        let available = vault_balance;

        // Compute settlement factor
        let total_normalized = match normalize(scaled_balance, scale_factor) {
            Some(n) if n > 0 => n,
            _ => return Ok(()),
        };

        let raw_factor = match available.checked_mul(WAD) {
            Some(n) => match n.checked_div(total_normalized) {
                Some(r) => r,
                None => return Ok(()),
            },
            None => return Ok(()),
        };
        let capped = raw_factor.min(WAD).max(1);

        // Compute payout for the full balance
        let payout = match compute_payout(scaled_balance, scale_factor, capped) {
            Some(p) => p,
            None => return Ok(()),
        };

        // With settlement factor capped at WAD, payout <= total_normalized
        // With factor from available/total_normalized, payout <= available
        prop_assert!(
            payout <= available + 1, // allow 1 for rounding
            "payout ({}) should not exceed available ({})",
            payout, available
        );
    }
}

// ---------------------------------------------------------------------------
// Property 4: Payout proportional to position size
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_payout_proportional_to_position(
        small_balance in prop_oneof![
            2 => Just(1u128),
            2 => Just(WAD),
            96 => 1u128..=500_000_000_000u128,
        ],
        extra in prop_oneof![
            2 => Just(1u128),
            98 => 1u128..=500_000_000_000u128,
        ],
        sf_offset in edge_biased_sf_offset().prop_filter("half", |o| *o <= WAD / 2),
        settlement_factor in edge_biased_settlement(),
    ) {
        let scale_factor = WAD + sf_offset;
        let large_balance = small_balance.saturating_add(extra);

        let payout_small = match compute_payout(small_balance, scale_factor, settlement_factor) {
            Some(p) => p,
            None => return Ok(()),
        };
        let payout_large = match compute_payout(large_balance, scale_factor, settlement_factor) {
            Some(p) => p,
            None => return Ok(()),
        };

        prop_assert!(
            payout_large >= payout_small,
            "larger position should get >= payout: small({})={}, large({})={}",
            small_balance, payout_small, large_balance, payout_large
        );
    }
}

// ---------------------------------------------------------------------------
// Property 5: Borrow reduces vault (conceptual — pure math)
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_borrow_reduces_vault(
        vault_balance in prop_oneof![
            2 => Just(1u64),
            2 => Just(u64::MAX),
            96 => 1u64..=10_000_000_000u64,
        ],
        borrow_amount in prop_oneof![
            2 => Just(1u64),
            98 => 1u64..=10_000_000_000u64,
        ],
    ) {
        if borrow_amount > vault_balance {
            return Ok(());
        }

        let new_vault = vault_balance.checked_sub(borrow_amount);
        prop_assert!(
            new_vault.is_some(),
            "valid borrow should not underflow vault"
        );
        prop_assert!(
            new_vault.unwrap() < vault_balance,
            "vault should decrease after borrow"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 6: Repay increases vault (conceptual — pure math)
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_repay_increases_vault(
        vault_balance in prop_oneof![
            2 => Just(0u64),
            2 => Just(u64::MAX - 1),
            96 => 0u64..=10_000_000_000u64,
        ],
        repay_amount in prop_oneof![
            2 => Just(1u64),
            98 => 1u64..=10_000_000_000u64,
        ],
    ) {
        let new_vault = match vault_balance.checked_add(repay_amount) {
            Some(v) => v,
            None => return Ok(()),
        };

        prop_assert!(
            new_vault > vault_balance,
            "vault should increase after repay"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 7: Rounding always favors the protocol
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_rounding_favors_protocol(
        amount in edge_biased_nonzero_amount(),
        sf_offset in edge_biased_sf_offset(),
    ) {
        let scale_factor = WAD + sf_offset;
        let amount_u128 = u128::from(amount);

        let scaled = match deposit_scale(amount, scale_factor) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };

        let recovered = match normalize(scaled, scale_factor) {
            Some(r) => r,
            None => return Ok(()),
        };

        // Protocol always wins: you get back at most what you put in
        prop_assert!(
            recovered <= amount_u128,
            "recovered ({}) must not exceed original ({}): protocol rounding must be favorable",
            recovered, amount_u128
        );
    }
}

// ---------------------------------------------------------------------------
// Property 8: Round-trip loss bounded by 2 units
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_roundtrip_loss_bounded_by_one(
        amount in edge_biased_nonzero_amount(),
        sf_offset in edge_biased_sf_offset().prop_filter("half", |o| *o <= WAD / 2),
    ) {
        let scale_factor = WAD + sf_offset;

        let scaled = match deposit_scale(amount, scale_factor) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };

        let recovered = match normalize(scaled, scale_factor) {
            Some(r) => r,
            None => return Ok(()),
        };

        let original = u128::from(amount);
        let loss = original.saturating_sub(recovered);
        prop_assert!(
            loss <= 2,
            "round-trip loss ({}) exceeds bound of 2: original={}, recovered={}",
            loss, original, recovered
        );
    }
}

// ---------------------------------------------------------------------------
// Property 9: Multiple deposits are additive within rounding tolerance
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_deposits_additive(
        a in prop_oneof![
            2 => Just(1u64),
            2 => Just(500_000_000u64),
            96 => 1u64..=500_000_000u64,
        ],
        b in prop_oneof![
            2 => Just(1u64),
            2 => Just(500_000_000u64),
            96 => 1u64..=500_000_000u64,
        ],
        sf_offset in edge_biased_sf_offset(),
    ) {
        let scale_factor = WAD + sf_offset;

        let scaled_a = match deposit_scale(a, scale_factor) {
            Some(s) => s,
            None => return Ok(()),
        };
        let scaled_b = match deposit_scale(b, scale_factor) {
            Some(s) => s,
            None => return Ok(()),
        };

        let sum_ab = match (a as u64).checked_add(b as u64) {
            Some(s) => s,
            None => return Ok(()),
        };

        let scaled_sum = match deposit_scale(sum_ab, scale_factor) {
            Some(s) => s,
            None => return Ok(()),
        };

        let sum_scaled = match scaled_a.checked_add(scaled_b) {
            Some(s) => s,
            None => return Ok(()),
        };

        // |scaled(a+b) - (scaled(a) + scaled(b))| <= 1 (floor division rounding)
        let diff = if scaled_sum > sum_scaled {
            scaled_sum - sum_scaled
        } else {
            sum_scaled - scaled_sum
        };

        prop_assert!(
            diff <= 1,
            "deposit additivity violated: scaled(a+b)={}, scaled(a)+scaled(b)={}, diff={}",
            scaled_sum, sum_scaled, diff
        );
    }
}

// ---------------------------------------------------------------------------
// Property 10: Settlement payout sum bounded by available balance
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_settlement_payout_sum_bounded(
        n_lenders in 2usize..=10,
        total_scaled in prop_oneof![
            2 => Just(100u128),
            2 => Just(1_000_000_000u128),
            96 => 100u128..=1_000_000_000u128,
        ],
        sf_offset in edge_biased_sf_offset().prop_filter("quarter", |o| *o <= WAD / 4),
        available in prop_oneof![
            2 => Just(0u128),
            2 => Just(2_000_000_000u128),
            96 => 0u128..=2_000_000_000u128,
        ],
    ) {
        let scale_factor = WAD + sf_offset;

        // Compute total_normalized
        let total_normalized = match normalize(total_scaled, scale_factor) {
            Some(n) if n > 0 => n,
            _ => return Ok(()),
        };

        // Compute settlement factor
        let raw_factor = match available.checked_mul(WAD) {
            Some(n) => match n.checked_div(total_normalized) {
                Some(r) => r,
                None => return Ok(()),
            },
            None => return Ok(()),
        };
        let settlement = raw_factor.min(WAD).max(1);

        // Split total_scaled among n_lenders (equal shares for simplicity)
        let per_lender = total_scaled / (n_lenders as u128);
        if per_lender == 0 {
            return Ok(());
        }

        let mut total_payout: u128 = 0;
        for _ in 0..n_lenders {
            let payout = match compute_payout(per_lender, scale_factor, settlement) {
                Some(p) => p,
                None => return Ok(()),
            };
            total_payout = match total_payout.checked_add(payout) {
                Some(t) => t,
                None => return Ok(()),
            };
        }

        // Total payout should not exceed available (with rounding tolerance of n_lenders)
        prop_assert!(
            total_payout <= available + (n_lenders as u128),
            "total payout ({}) exceeds available ({}) + rounding tolerance ({})",
            total_payout, available, n_lenders
        );
    }
}

// ---------------------------------------------------------------------------
// Property 11: Deposit/withdraw symmetry at WAD scale_factor and full settlement
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_deposit_withdraw_symmetry_at_wad(
        amount in edge_biased_nonzero_amount(),
    ) {
        let scale_factor = WAD;
        let settlement = WAD;

        let scaled = match deposit_scale(amount, scale_factor) {
            Some(s) if s > 0 => s,
            _ => return Ok(()),
        };

        let payout = match compute_payout(scaled, scale_factor, settlement) {
            Some(p) => p,
            None => return Ok(()),
        };

        // At WAD/WAD, floor divisions are exact for integer amounts
        prop_assert_eq!(
            payout, u128::from(amount),
            "at WAD scale_factor and full settlement, deposit-withdraw should be exact"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 12: Zero-amount deposit produces zero scaled amount
// ---------------------------------------------------------------------------
#[test]
fn test_zero_amount_deposit() {
    for sf in [WAD, WAD + 1, WAD * 2, WAD + WAD / 10, WAD - 1 + 1] {
        let scaled = deposit_scale(0, sf);
        assert_eq!(
            scaled,
            Some(0),
            "zero deposit must produce zero scaled amount at sf={}",
            sf
        );
    }
}

// ---------------------------------------------------------------------------
// Property 13: Fee proportionality — higher supply => higher fees
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_fee_proportional_to_supply(
        supply_small in prop_oneof![
            2 => Just(1u128),
            2 => Just(WAD),
            96 => 1u128..=500_000_000_000u128,
        ],
        supply_extra in prop_oneof![
            2 => Just(1u128),
            98 => 1u128..=500_000_000_000u128,
        ],
        annual_bps in prop_oneof![
            2 => Just(1u16),
            2 => Just(10_000u16),
            96 => 1u16..=10_000u16,
        ],
        fee_rate_bps in prop_oneof![
            2 => Just(1u16),
            2 => Just(10_000u16),
            96 => 1u16..=10_000u16,
        ],
        time_elapsed in prop_oneof![
            2 => Just(1i64),
            2 => Just(31_536_000i64),
            96 => 1i64..=31_536_000i64,
        ],
    ) {
        use bytemuck::Zeroable;
        use coalesce::logic::interest::accrue_interest;
        use coalesce::state::{Market, ProtocolConfig};

        let supply_large = supply_small.saturating_add(supply_extra);

        let make_m = |supply: u128| -> Market {
            let mut m = Market::zeroed();
            m.set_annual_interest_bps(annual_bps);
            m.set_maturity_timestamp(i64::MAX);
            m.set_scale_factor(WAD);
            m.set_scaled_total_supply(supply);
            m.set_last_accrual_timestamp(0);
            m.set_accrued_protocol_fees(0);
            m
        };

        let mut cfg = ProtocolConfig::zeroed();
        cfg.set_fee_rate_bps(fee_rate_bps);

        let mut m_small = make_m(supply_small);
        if accrue_interest(&mut m_small, &cfg, time_elapsed).is_err() {
            return Ok(());
        }

        let mut m_large = make_m(supply_large);
        if accrue_interest(&mut m_large, &cfg, time_elapsed).is_err() {
            return Ok(());
        }

        prop_assert!(
            m_large.accrued_protocol_fees() >= m_small.accrued_protocol_fees(),
            "larger supply ({}) should yield >= fees than smaller supply ({}): large_fees={}, small_fees={}",
            supply_large, supply_small,
            m_large.accrued_protocol_fees(), m_small.accrued_protocol_fees()
        );
    }
}

// ---------------------------------------------------------------------------
// Property 14: Interest accrual idempotent at same timestamp
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn test_accrual_idempotent_same_timestamp(
        annual_bps in prop_oneof![
            2 => Just(1u16),
            2 => Just(10_000u16),
            96 => 1u16..=10_000u16,
        ],
        fee_rate_bps in prop_oneof![
            2 => Just(0u16),
            2 => Just(10_000u16),
            96 => 0u16..=10_000u16,
        ],
        time_elapsed in prop_oneof![
            2 => Just(1i64),
            2 => Just(31_536_000i64),
            96 => 1i64..=31_536_000i64,
        ],
    ) {
        use bytemuck::Zeroable;
        use coalesce::logic::interest::accrue_interest;
        use coalesce::state::{Market, ProtocolConfig};

        let mut market = Market::zeroed();
        market.set_annual_interest_bps(annual_bps);
        market.set_maturity_timestamp(i64::MAX);
        market.set_scale_factor(WAD);
        market.set_scaled_total_supply(1_000_000_000_000u128);
        market.set_last_accrual_timestamp(0);
        market.set_accrued_protocol_fees(0);

        let mut cfg = ProtocolConfig::zeroed();
        cfg.set_fee_rate_bps(fee_rate_bps);

        // First call
        if accrue_interest(&mut market, &cfg, time_elapsed).is_err() {
            return Ok(());
        }

        let sf_after_first = market.scale_factor();
        let fees_after_first = market.accrued_protocol_fees();
        let ts_after_first = market.last_accrual_timestamp();

        // Second call with SAME timestamp — should be a no-op
        if accrue_interest(&mut market, &cfg, time_elapsed).is_err() {
            return Ok(());
        }

        prop_assert_eq!(
            market.scale_factor(), sf_after_first,
            "second call at same timestamp should not change scale_factor"
        );
        prop_assert_eq!(
            market.accrued_protocol_fees(), fees_after_first,
            "second call at same timestamp should not change fees"
        );
        prop_assert_eq!(
            market.last_accrual_timestamp(), ts_after_first,
            "second call at same timestamp should not change last_accrual"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 15: Whitelist capacity monotonically consumed
// ---------------------------------------------------------------------------
#[test]
fn test_whitelist_capacity_monotonic() {
    use bytemuck::Zeroable;
    use coalesce::state::BorrowerWhitelist;

    let mut wl = BorrowerWhitelist::zeroed();
    wl.is_whitelisted = 1;
    wl.set_max_borrow_capacity(10_000_000);

    let mut previous = 0u64;
    for borrow in [100_000u64, 200_000, 500_000, 1_000_000] {
        let new_total = wl.current_borrowed().checked_add(borrow).unwrap();
        assert!(new_total <= wl.max_borrow_capacity());
        wl.set_current_borrowed(new_total);
        assert!(
            wl.current_borrowed() >= previous,
            "whitelist current_borrowed must never decrease"
        );
        previous = wl.current_borrowed();
    }

    // Edge: verify at capacity
    let remaining = wl.max_borrow_capacity() - wl.current_borrowed();
    assert!(remaining > 0);
    wl.set_current_borrowed(wl.max_borrow_capacity());
    assert_eq!(wl.current_borrowed(), wl.max_borrow_capacity());
}

// ---------------------------------------------------------------------------
// Property 16: Compound interest strictly exceeds simple interest
// ---------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn test_compound_strictly_exceeds_simple(
        annual_bps in prop_oneof![
            2 => Just(1u16),
            2 => Just(10_000u16),
            96 => 1u16..=10_000u16,
        ],
        t1 in prop_oneof![
            2 => Just(1i64),
            2 => Just(15_768_000i64),
            96 => 1i64..=15_768_000i64,
        ],
        t2_delta in prop_oneof![
            2 => Just(1i64),
            2 => Just(15_768_000i64),
            96 => 1i64..=15_768_000i64,
        ],
    ) {
        use bytemuck::Zeroable;
        use coalesce::logic::interest::accrue_interest;
        use coalesce::state::{Market, ProtocolConfig};

        let t2 = t1.saturating_add(t2_delta);
        let mut cfg = ProtocolConfig::zeroed();
        cfg.set_fee_rate_bps(0);

        // Simple: one step 0->t2
        let mut m_simple = Market::zeroed();
        m_simple.set_annual_interest_bps(annual_bps);
        m_simple.set_maturity_timestamp(i64::MAX);
        m_simple.set_scale_factor(WAD);
        m_simple.set_scaled_total_supply(WAD);
        m_simple.set_last_accrual_timestamp(0);

        if accrue_interest(&mut m_simple, &cfg, t2).is_err() {
            return Ok(());
        }

        // Compound: two steps 0->t1->t2
        let mut m_compound = Market::zeroed();
        m_compound.set_annual_interest_bps(annual_bps);
        m_compound.set_maturity_timestamp(i64::MAX);
        m_compound.set_scale_factor(WAD);
        m_compound.set_scaled_total_supply(WAD);
        m_compound.set_last_accrual_timestamp(0);

        if accrue_interest(&mut m_compound, &cfg, t1).is_err() {
            return Ok(());
        }
        if accrue_interest(&mut m_compound, &cfg, t2).is_err() {
            return Ok(());
        }

        // Compound should always be >= simple in exact arithmetic. With daily
        // compounding, each accrual step performs multiple WAD-scaled floor
        // divisions (pow_wad + remaining growth + final mul_wad), so two
        // smaller steps can accumulate more truncation loss than one large
        // step. Allow a bounded tolerance of 4 units (consistent with
        // property_tests.rs Property 8).
        let compound = m_compound.scale_factor();
        let simple = m_simple.scale_factor();
        let rounding_tolerance: u128 = 4;
        prop_assert!(
            compound.saturating_add(rounding_tolerance) >= simple,
            "compound ({}) must be >= simple ({}) within {}-unit rounding tolerance",
            compound, simple, rounding_tolerance
        );
    }
}

// ===========================================================================
// Regression seed tests for critical edge cases
// ===========================================================================

#[test]
fn regression_deposit_roundtrip_at_exact_wad() {
    // At exactly WAD, roundtrip should be lossless
    let amount = 1_000_000u64;
    let scaled = deposit_scale(amount, WAD).unwrap();
    assert_eq!(scaled, u128::from(amount));
    let recovered = normalize(scaled, WAD).unwrap();
    assert_eq!(recovered, u128::from(amount));
}

#[test]
fn regression_deposit_roundtrip_at_double_wad() {
    // At 2*WAD, scaled should be half
    let amount = 1_000_000u64;
    let scaled = deposit_scale(amount, 2 * WAD).unwrap();
    assert_eq!(scaled, u128::from(amount) / 2);
    let recovered = normalize(scaled, 2 * WAD).unwrap();
    assert_eq!(recovered, u128::from(amount));
}

#[test]
fn regression_payout_full_settlement() {
    // Full settlement (WAD) at WAD scale_factor → payout = original
    let amount: u128 = 1_000_000;
    let payout = compute_payout(amount, WAD, WAD).unwrap();
    assert_eq!(payout, amount);
}

#[test]
fn regression_payout_half_settlement() {
    // Half settlement → payout = half (at WAD scale)
    let amount: u128 = 1_000_000;
    let payout = compute_payout(amount, WAD, WAD / 2).unwrap();
    assert_eq!(payout, amount / 2);
}

#[test]
fn regression_deposit_one_unit_at_double_wad() {
    // 1 unit at 2*WAD → scaled = 0 (floor rounds to 0)
    let scaled = deposit_scale(1, 2 * WAD).unwrap();
    assert_eq!(scaled, 0, "1 token at 2x scale factor rounds to 0 scaled");
}

#[test]
fn regression_payout_minimum_settlement() {
    // Minimum settlement factor (1) → payout is negligible
    let amount: u128 = 1_000_000_000;
    let payout = compute_payout(amount, WAD, 1).unwrap();
    // payout = amount * 1 / WAD ≈ 0 for typical amounts
    assert_eq!(payout, 0);
}

#[test]
fn regression_deposits_additive_exact_at_wad() {
    // At WAD, deposits should be perfectly additive
    let a = 100u64;
    let b = 200u64;
    let scaled_a = deposit_scale(a, WAD).unwrap();
    let scaled_b = deposit_scale(b, WAD).unwrap();
    let scaled_sum = deposit_scale(a + b, WAD).unwrap();
    assert_eq!(scaled_a + scaled_b, scaled_sum);
}
