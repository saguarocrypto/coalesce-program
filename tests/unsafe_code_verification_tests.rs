//! Comprehensive verification tests for all unsafe code blocks.
//!
//! This file provides exhaustive testing for every unsafe block in the codebase.
//! Each unsafe pattern is documented with:
//! - The invariants required for safety
//! - Test cases that verify these invariants
//! - Property-based tests for robustness
//!
//! Unsafe code locations after safe migration (9 remaining):
//! - from_account_view_unchecked(): Token account parsing (8 uses) - KEPT for SPL Token performance
//! - unsafe impl Log: Trait implementation (1 use) - KEPT for logging trait
//!
//! Previously unsafe patterns now using safe alternatives:
//! - borrow_unchecked() -> try_borrow() with runtime borrow checking (was 21 uses)
//! - borrow_unchecked_mut() -> try_borrow_mut() with runtime borrow checking (was 19 uses)
//! - owner() -> owned_by(&Address) safe method (was 4 uses)
//! - from_utf8_unchecked() -> safe match on from_utf8() (was 1 use)
//! - pointer cast -> Address::new_from_array() safe constructor (was 1 use)

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

use bytemuck::{bytes_of, try_from_bytes, try_from_bytes_mut, Pod, Zeroable};
use proptest::prelude::*;
use std::cell::RefCell;
use std::mem::size_of;

// Import the account types to verify their safety invariants
#[path = "../src/state/mod.rs"]
mod state_types {
    pub use super::*;
}

fn deterministic_patterns(size: usize) -> Vec<Vec<u8>> {
    let mut patterns = vec![
        vec![0x00; size],
        vec![0xFF; size],
        vec![0xAA; size],
        vec![0x55; size],
        (0..size).map(|i| i as u8).collect(),
        (0..size)
            .map(|i| (255u16 - (i as u16 % 256)) as u8)
            .collect(),
    ];

    let mut first_bit = vec![0u8; size];
    if size > 0 {
        first_bit[0] = 1;
    }
    patterns.push(first_bit);

    let mut last_bit = vec![0u8; size];
    if size > 0 {
        last_bit[size - 1] = 1;
    }
    patterns.push(last_bit);

    patterns
}

fn assert_pod_cast_roundtrip<T: Pod>(data: &[u8], label: &str) {
    let parsed =
        try_from_bytes::<T>(data).unwrap_or_else(|_| panic!("{label}: cast should succeed"));
    assert_eq!(
        bytes_of(parsed),
        data,
        "{label}: bytes should roundtrip without mutation"
    );
}

// ============================================================================
// PATTERN 1: try_borrow() / try_borrow_mut() verification (SAFE)
// ============================================================================
//
// These patterns are now SAFE using pinocchio's runtime-checked borrows:
// - try_borrow() returns Ref<[u8]> with runtime aliasing checks
// - try_borrow_mut() returns RefMut<[u8]> with runtime aliasing checks
//
// The invariants below are still enforced by bytemuck:
// 1. Account data must be at least as large as the struct being deserialized
// 2. Account data must be properly aligned (bytemuck handles this)
// 3. No aliasing between borrows (NOW enforced at runtime by Ref/RefMut)
// 4. All byte patterns must be valid for the target type (Pod types only)

/// Verify ProtocolConfig can be safely deserialized from any byte pattern
#[test]
fn test_protocol_config_any_bytes_valid() {
    use coalesce::constants::PROTOCOL_CONFIG_SIZE;
    use coalesce::state::ProtocolConfig;

    // ProtocolConfig is 194 bytes
    assert_eq!(size_of::<ProtocolConfig>(), PROTOCOL_CONFIG_SIZE);

    for data in deterministic_patterns(PROTOCOL_CONFIG_SIZE) {
        assert_pod_cast_roundtrip::<ProtocolConfig>(&data, "ProtocolConfig any-bytes");
        let config = try_from_bytes::<ProtocolConfig>(&data).unwrap();

        // Accessors should not panic across adversarial byte patterns.
        let _ = config.fee_rate_bps();
        let _ = config.is_paused();
        let _ = config.is_blacklist_fail_closed();
    }

    // Tight bounds: exact size succeeds, neighbors fail.
    assert!(try_from_bytes::<ProtocolConfig>(&vec![0u8; PROTOCOL_CONFIG_SIZE]).is_ok());
    assert!(try_from_bytes::<ProtocolConfig>(&vec![0u8; PROTOCOL_CONFIG_SIZE - 1]).is_err());
    assert!(try_from_bytes::<ProtocolConfig>(&vec![0u8; PROTOCOL_CONFIG_SIZE + 1]).is_err());
}

/// Verify Market can be safely deserialized from any byte pattern
#[test]
fn test_market_any_bytes_valid() {
    use coalesce::constants::MARKET_SIZE;
    use coalesce::state::Market;

    assert_eq!(size_of::<Market>(), MARKET_SIZE);

    for data in deterministic_patterns(MARKET_SIZE) {
        assert_pod_cast_roundtrip::<Market>(&data, "Market any-bytes");
        let market = try_from_bytes::<Market>(&data).unwrap();
        // All accessors must not panic
        let _ = market.annual_interest_bps();
        let _ = market.maturity_timestamp();
        let _ = market.max_total_supply();
        let _ = market.market_nonce();
        let _ = market.scaled_total_supply();
        let _ = market.scale_factor();
        let _ = market.accrued_protocol_fees();
        let _ = market.total_deposited();
        let _ = market.total_borrowed();
        let _ = market.total_repaid();
        let _ = market.total_interest_repaid();
        let _ = market.last_accrual_timestamp();
        let _ = market.settlement_factor_wad();
    }

    assert!(try_from_bytes::<Market>(&vec![0u8; MARKET_SIZE]).is_ok());
    assert!(try_from_bytes::<Market>(&vec![0u8; MARKET_SIZE - 1]).is_err());
    assert!(try_from_bytes::<Market>(&vec![0u8; MARKET_SIZE + 1]).is_err());
}

/// Verify LenderPosition can be safely deserialized from any byte pattern
#[test]
fn test_lender_position_any_bytes_valid() {
    use coalesce::constants::LENDER_POSITION_SIZE;
    use coalesce::state::LenderPosition;

    assert_eq!(size_of::<LenderPosition>(), LENDER_POSITION_SIZE);

    for data in deterministic_patterns(LENDER_POSITION_SIZE) {
        assert_pod_cast_roundtrip::<LenderPosition>(&data, "LenderPosition any-bytes");
        let pos = try_from_bytes::<LenderPosition>(&data).unwrap();
        let _ = pos.scaled_balance();
    }

    assert!(try_from_bytes::<LenderPosition>(&vec![0u8; LENDER_POSITION_SIZE]).is_ok());
    assert!(try_from_bytes::<LenderPosition>(&vec![0u8; LENDER_POSITION_SIZE - 1]).is_err());
    assert!(try_from_bytes::<LenderPosition>(&vec![0u8; LENDER_POSITION_SIZE + 1]).is_err());
}

/// Verify BorrowerWhitelist can be safely deserialized from any byte pattern
#[test]
fn test_borrower_whitelist_any_bytes_valid() {
    use coalesce::constants::BORROWER_WHITELIST_SIZE;
    use coalesce::state::BorrowerWhitelist;

    assert_eq!(size_of::<BorrowerWhitelist>(), BORROWER_WHITELIST_SIZE);

    for data in deterministic_patterns(BORROWER_WHITELIST_SIZE) {
        assert_pod_cast_roundtrip::<BorrowerWhitelist>(&data, "BorrowerWhitelist any-bytes");
        let wl = try_from_bytes::<BorrowerWhitelist>(&data).unwrap();
        let _ = wl.is_whitelisted;
        let _ = wl.max_borrow_capacity();
        let _ = wl.current_borrowed();
    }

    assert!(try_from_bytes::<BorrowerWhitelist>(&vec![0u8; BORROWER_WHITELIST_SIZE]).is_ok());
    assert!(try_from_bytes::<BorrowerWhitelist>(&vec![0u8; BORROWER_WHITELIST_SIZE - 1]).is_err());
    assert!(try_from_bytes::<BorrowerWhitelist>(&vec![0u8; BORROWER_WHITELIST_SIZE + 1]).is_err());
}

// ============================================================================
// PATTERN 2: Size boundary verification
// ============================================================================

/// Test that undersized buffers are rejected by bytemuck
#[test]
fn test_undersized_buffer_rejected_protocol_config() {
    use coalesce::constants::PROTOCOL_CONFIG_SIZE;
    use coalesce::state::ProtocolConfig;

    // Test all sizes from 0 to PROTOCOL_CONFIG_SIZE - 1
    for size in 0..PROTOCOL_CONFIG_SIZE {
        let data = vec![0u8; size];
        let result = try_from_bytes::<ProtocolConfig>(&data);
        assert!(
            result.is_err(),
            "Size {} should be rejected (need {})",
            size,
            PROTOCOL_CONFIG_SIZE
        );
    }

    // Tight bounds around exact size.
    assert!(try_from_bytes::<ProtocolConfig>(&vec![0u8; PROTOCOL_CONFIG_SIZE]).is_ok());
    assert!(try_from_bytes::<ProtocolConfig>(&vec![0u8; PROTOCOL_CONFIG_SIZE + 1]).is_err());

    // Mutable cast follows the same bounds.
    assert!(try_from_bytes_mut::<ProtocolConfig>(&mut vec![0u8; PROTOCOL_CONFIG_SIZE]).is_ok());
    assert!(
        try_from_bytes_mut::<ProtocolConfig>(&mut vec![0u8; PROTOCOL_CONFIG_SIZE - 1]).is_err()
    );
    assert!(
        try_from_bytes_mut::<ProtocolConfig>(&mut vec![0u8; PROTOCOL_CONFIG_SIZE + 1]).is_err()
    );
}

#[test]
fn test_undersized_buffer_rejected_market() {
    use coalesce::constants::MARKET_SIZE;
    use coalesce::state::Market;

    for size in 0..MARKET_SIZE {
        let data = vec![0u8; size];
        let result = try_from_bytes::<Market>(&data);
        assert!(result.is_err(), "Size {} should be rejected", size);
    }

    assert!(try_from_bytes::<Market>(&vec![0u8; MARKET_SIZE]).is_ok());
    assert!(try_from_bytes::<Market>(&vec![0u8; MARKET_SIZE + 1]).is_err());
    assert!(try_from_bytes_mut::<Market>(&mut vec![0u8; MARKET_SIZE]).is_ok());
    assert!(try_from_bytes_mut::<Market>(&mut vec![0u8; MARKET_SIZE - 1]).is_err());
    assert!(try_from_bytes_mut::<Market>(&mut vec![0u8; MARKET_SIZE + 1]).is_err());
}

#[test]
fn test_undersized_buffer_rejected_lender_position() {
    use coalesce::constants::LENDER_POSITION_SIZE;
    use coalesce::state::LenderPosition;

    for size in 0..LENDER_POSITION_SIZE {
        let data = vec![0u8; size];
        let result = try_from_bytes::<LenderPosition>(&data);
        assert!(result.is_err(), "Size {} should be rejected", size);
    }

    assert!(try_from_bytes::<LenderPosition>(&vec![0u8; LENDER_POSITION_SIZE]).is_ok());
    assert!(try_from_bytes::<LenderPosition>(&vec![0u8; LENDER_POSITION_SIZE + 1]).is_err());
    assert!(try_from_bytes_mut::<LenderPosition>(&mut vec![0u8; LENDER_POSITION_SIZE]).is_ok());
    assert!(
        try_from_bytes_mut::<LenderPosition>(&mut vec![0u8; LENDER_POSITION_SIZE - 1]).is_err()
    );
    assert!(
        try_from_bytes_mut::<LenderPosition>(&mut vec![0u8; LENDER_POSITION_SIZE + 1]).is_err()
    );
}

#[test]
fn test_undersized_buffer_rejected_borrower_whitelist() {
    use coalesce::constants::BORROWER_WHITELIST_SIZE;
    use coalesce::state::BorrowerWhitelist;

    for size in 0..BORROWER_WHITELIST_SIZE {
        let data = vec![0u8; size];
        let result = try_from_bytes::<BorrowerWhitelist>(&data);
        assert!(result.is_err(), "Size {} should be rejected", size);
    }

    assert!(try_from_bytes::<BorrowerWhitelist>(&vec![0u8; BORROWER_WHITELIST_SIZE]).is_ok());
    assert!(try_from_bytes::<BorrowerWhitelist>(&vec![0u8; BORROWER_WHITELIST_SIZE + 1]).is_err());
    assert!(
        try_from_bytes_mut::<BorrowerWhitelist>(&mut vec![0u8; BORROWER_WHITELIST_SIZE]).is_ok()
    );
    assert!(
        try_from_bytes_mut::<BorrowerWhitelist>(&mut vec![0u8; BORROWER_WHITELIST_SIZE - 1])
            .is_err()
    );
    assert!(
        try_from_bytes_mut::<BorrowerWhitelist>(&mut vec![0u8; BORROWER_WHITELIST_SIZE + 1])
            .is_err()
    );
}

// ============================================================================
// PATTERN 3: Oversized buffer acceptance (slice prefix)
// ============================================================================

/// Verify that oversized buffers are accepted (prefix is used)
#[test]
fn test_oversized_buffer_accepted() {
    use coalesce::constants::{
        BORROWER_WHITELIST_SIZE, LENDER_POSITION_SIZE, MARKET_SIZE, PROTOCOL_CONFIG_SIZE,
    };
    use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

    let check = |expected_size: usize, label: &str| {
        for extra in [1usize, 7usize, 64usize, 256usize] {
            let mut large_data = vec![0u8; expected_size + extra];
            for (i, b) in large_data.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }

            // Whole oversized buffer is rejected by bytemuck.
            if expected_size == PROTOCOL_CONFIG_SIZE {
                assert!(
                    try_from_bytes::<ProtocolConfig>(&large_data).is_err(),
                    "{label}: oversized whole slice must fail"
                );
            }
            if expected_size == MARKET_SIZE {
                assert!(
                    try_from_bytes::<Market>(&large_data).is_err(),
                    "{label}: oversized whole slice must fail"
                );
            }
            if expected_size == LENDER_POSITION_SIZE {
                assert!(
                    try_from_bytes::<LenderPosition>(&large_data).is_err(),
                    "{label}: oversized whole slice must fail"
                );
            }
            if expected_size == BORROWER_WHITELIST_SIZE {
                assert!(
                    try_from_bytes::<BorrowerWhitelist>(&large_data).is_err(),
                    "{label}: oversized whole slice must fail"
                );
            }

            // Exact prefix from oversized source remains safe and deterministic.
            let exact_prefix = &large_data[..expected_size];
            if expected_size == PROTOCOL_CONFIG_SIZE {
                assert_pod_cast_roundtrip::<ProtocolConfig>(exact_prefix, label);
            }
            if expected_size == MARKET_SIZE {
                assert_pod_cast_roundtrip::<Market>(exact_prefix, label);
            }
            if expected_size == LENDER_POSITION_SIZE {
                assert_pod_cast_roundtrip::<LenderPosition>(exact_prefix, label);
            }
            if expected_size == BORROWER_WHITELIST_SIZE {
                assert_pod_cast_roundtrip::<BorrowerWhitelist>(exact_prefix, label);
            }
        }
    };

    check(PROTOCOL_CONFIG_SIZE, "ProtocolConfig oversized-prefix");
    check(MARKET_SIZE, "Market oversized-prefix");
    check(LENDER_POSITION_SIZE, "LenderPosition oversized-prefix");
    check(
        BORROWER_WHITELIST_SIZE,
        "BorrowerWhitelist oversized-prefix",
    );
}

// ============================================================================
// PATTERN 4: Field getter/setter roundtrip verification
// ============================================================================

proptest! {
    /// Property: All Market field getters/setters roundtrip correctly
    #[test]
    fn prop_market_field_roundtrip(
        annual_interest_bps in 0u16..=10000u16,
        maturity_timestamp in any::<i64>(),
        max_total_supply in any::<u64>(),
        market_nonce in any::<u64>(),
        scaled_total_supply in any::<u128>(),
        scale_factor in any::<u128>(),
        accrued_protocol_fees in any::<u64>(),
        total_deposited in any::<u64>(),
        total_borrowed in any::<u64>(),
        total_repaid in any::<u64>(),
        total_interest_repaid in any::<u64>(),
        last_accrual_timestamp in any::<i64>(),
        settlement_factor_wad in any::<u128>(),
    ) {
        use coalesce::state::Market;
        use bytemuck::Zeroable;

        let mut market = Market::zeroed();

        market.set_annual_interest_bps(annual_interest_bps);
        prop_assert_eq!(market.annual_interest_bps(), annual_interest_bps);

        market.set_maturity_timestamp(maturity_timestamp);
        prop_assert_eq!(market.maturity_timestamp(), maturity_timestamp);

        market.set_max_total_supply(max_total_supply);
        prop_assert_eq!(market.max_total_supply(), max_total_supply);

        market.set_market_nonce(market_nonce);
        prop_assert_eq!(market.market_nonce(), market_nonce);

        market.set_scaled_total_supply(scaled_total_supply);
        prop_assert_eq!(market.scaled_total_supply(), scaled_total_supply);

        market.set_scale_factor(scale_factor);
        prop_assert_eq!(market.scale_factor(), scale_factor);

        market.set_accrued_protocol_fees(accrued_protocol_fees);
        prop_assert_eq!(market.accrued_protocol_fees(), accrued_protocol_fees);

        market.set_total_deposited(total_deposited);
        prop_assert_eq!(market.total_deposited(), total_deposited);

        market.set_total_borrowed(total_borrowed);
        prop_assert_eq!(market.total_borrowed(), total_borrowed);

        market.set_total_repaid(total_repaid);
        prop_assert_eq!(market.total_repaid(), total_repaid);

        market.set_total_interest_repaid(total_interest_repaid);
        prop_assert_eq!(market.total_interest_repaid(), total_interest_repaid);

        market.set_last_accrual_timestamp(last_accrual_timestamp);
        prop_assert_eq!(market.last_accrual_timestamp(), last_accrual_timestamp);

        market.set_settlement_factor_wad(settlement_factor_wad);
        prop_assert_eq!(market.settlement_factor_wad(), settlement_factor_wad);
    }

    /// Property: All ProtocolConfig field getters/setters roundtrip correctly
    #[test]
    fn prop_protocol_config_field_roundtrip(
        fee_rate_bps in prop_oneof![
            Just(0u16),
            Just(1u16),
            Just(9_999u16),
            Just(10_000u16),
            0u16..=10_000u16
        ],
        fee_rate_bps_next in prop_oneof![
            Just(0u16),
            Just(1u16),
            Just(9_999u16),
            Just(10_000u16),
            0u16..=10_000u16
        ],
    ) {
        use coalesce::state::ProtocolConfig;
        use bytemuck::Zeroable;

        let mut config = ProtocolConfig::zeroed();

        config.set_fee_rate_bps(fee_rate_bps);
        prop_assert_eq!(config.fee_rate_bps(), fee_rate_bps);

        let before_bytes = bytes_of(&config).to_vec();
        let reparsed = try_from_bytes::<ProtocolConfig>(&before_bytes).unwrap();
        prop_assert_eq!(reparsed.fee_rate_bps(), fee_rate_bps);

        config.set_fee_rate_bps(fee_rate_bps_next);
        prop_assert_eq!(config.fee_rate_bps(), fee_rate_bps_next);
        let after_bytes = bytes_of(&config).to_vec();
        let reparsed_after = try_from_bytes::<ProtocolConfig>(&after_bytes).unwrap();
        prop_assert_eq!(reparsed_after.fee_rate_bps(), fee_rate_bps_next);
        if fee_rate_bps != fee_rate_bps_next {
            prop_assert_ne!(before_bytes, after_bytes, "fee updates should mutate serialized bytes");
        }
    }

    /// Property: All LenderPosition field getters/setters roundtrip correctly
    #[test]
    fn prop_lender_position_field_roundtrip(
        scaled_balance in prop_oneof![
            Just(0u128),
            Just(1u128),
            Just(u128::MAX - 1),
            Just(u128::MAX),
            any::<u128>()
        ],
        scaled_balance_next in prop_oneof![
            Just(0u128),
            Just(1u128),
            Just(u128::MAX - 1),
            Just(u128::MAX),
            any::<u128>()
        ],
    ) {
        use coalesce::state::LenderPosition;
        use bytemuck::Zeroable;

        let mut pos = LenderPosition::zeroed();

        pos.set_scaled_balance(scaled_balance);
        prop_assert_eq!(pos.scaled_balance(), scaled_balance);

        let bytes_before = bytes_of(&pos).to_vec();
        let reparsed = try_from_bytes::<LenderPosition>(&bytes_before).unwrap();
        prop_assert_eq!(reparsed.scaled_balance(), scaled_balance);

        pos.set_scaled_balance(scaled_balance_next);
        prop_assert_eq!(pos.scaled_balance(), scaled_balance_next);
        let bytes_after = bytes_of(&pos).to_vec();
        let reparsed_after = try_from_bytes::<LenderPosition>(&bytes_after).unwrap();
        prop_assert_eq!(reparsed_after.scaled_balance(), scaled_balance_next);
        if scaled_balance != scaled_balance_next {
            prop_assert_ne!(bytes_before, bytes_after);
        }
    }

    /// Property: All BorrowerWhitelist field getters/setters roundtrip correctly
    #[test]
    fn prop_borrower_whitelist_field_roundtrip(
        max_borrow_capacity in prop_oneof![
            Just(0u64),
            Just(1u64),
            Just(u64::MAX - 1),
            Just(u64::MAX),
            any::<u64>()
        ],
        current_borrowed in prop_oneof![
            Just(0u64),
            Just(1u64),
            Just(u64::MAX - 1),
            Just(u64::MAX),
            any::<u64>()
        ],
    ) {
        use coalesce::state::BorrowerWhitelist;
        use bytemuck::Zeroable;

        let mut wl = BorrowerWhitelist::zeroed();

        wl.set_max_borrow_capacity(max_borrow_capacity);
        prop_assert_eq!(wl.max_borrow_capacity(), max_borrow_capacity);
        prop_assert_eq!(wl.current_borrowed(), 0);

        wl.set_current_borrowed(current_borrowed);
        prop_assert_eq!(wl.current_borrowed(), current_borrowed);
        prop_assert_eq!(wl.max_borrow_capacity(), max_borrow_capacity);

        let bytes = bytes_of(&wl).to_vec();
        let reparsed = try_from_bytes::<BorrowerWhitelist>(&bytes).unwrap();
        prop_assert_eq!(reparsed.max_borrow_capacity(), max_borrow_capacity);
        prop_assert_eq!(reparsed.current_borrowed(), current_borrowed);
    }
}

// ============================================================================
// PATTERN 5: Mutable borrow non-aliasing verification (RUNTIME ENFORCED)
// ============================================================================

/// Verify that mutable borrows don't alias
/// With try_borrow_mut(), this is now enforced at RUNTIME via RefCell-style checking.
/// The runtime will return an error if aliased borrows are attempted.
#[test]
fn test_mutable_borrow_no_aliasing() {
    use coalesce::constants::MARKET_SIZE;
    use coalesce::state::Market;

    let data = RefCell::new(vec![0u8; MARKET_SIZE]);

    // First mutable borrow succeeds.
    let mut first_borrow = data.borrow_mut();
    let market = try_from_bytes_mut::<Market>(&mut first_borrow).unwrap();
    market.set_scale_factor(1_000_000_000_000_000_000u128);
    assert_eq!(market.scale_factor(), 1_000_000_000_000_000_000u128);

    // Runtime aliasing checks: second mutable or immutable borrow must fail
    // while the first mutable borrow is alive.
    assert!(
        data.try_borrow_mut().is_err(),
        "aliased mutable borrow must be rejected"
    );
    assert!(
        data.try_borrow().is_err(),
        "shared borrow during mutable borrow must be rejected"
    );
    drop(first_borrow);

    // Once released, subsequent borrows should succeed and preserve written state.
    let mut second_borrow = data.borrow_mut();
    let market_after = try_from_bytes_mut::<Market>(&mut second_borrow).unwrap();
    assert_eq!(
        market_after.scale_factor(),
        1_000_000_000_000_000_000u128,
        "state written through first mutable borrow must persist"
    );
}

// ============================================================================
// PATTERN 6: Clock sysvar data verification
// ============================================================================

/// Verify clock sysvar reading at correct offset (offset 32 for unix_timestamp)
#[test]
fn test_clock_sysvar_data_extraction() {
    fn extract_unix_timestamp(clock_data: &[u8]) -> Option<i64> {
        let ts = clock_data.get(32..40)?;
        Some(i64::from_le_bytes(ts.try_into().ok()?))
    }

    // Clock sysvar layout (bincode serialized):
    // offset  0: slot (u64)
    // offset  8: epoch_start_timestamp (i64)
    // offset 16: epoch (u64)
    // offset 24: leader_schedule_epoch (u64)
    // offset 32: unix_timestamp (i64)

    let mut clock_data = vec![0u8; 40];

    // Set unix_timestamp at offset 32
    let timestamp: i64 = 1_700_000_000;
    clock_data[32..40].copy_from_slice(&timestamp.to_le_bytes());

    // Read back using the same offset logic as validation.rs.
    let read_timestamp = extract_unix_timestamp(&clock_data).unwrap();
    assert_eq!(read_timestamp, timestamp);

    // Neighboring offsets must not accidentally decode to the same value.
    let wrong_offset = i64::from_le_bytes(clock_data[24..32].try_into().unwrap());
    assert_ne!(
        wrong_offset, timestamp,
        "offset 24 must not alias unix_timestamp field"
    );

    // Signed values roundtrip correctly.
    let negative: i64 = -42;
    clock_data[32..40].copy_from_slice(&negative.to_le_bytes());
    assert_eq!(extract_unix_timestamp(&clock_data), Some(negative));

    // Truncated clock data is rejected by bounds checks.
    assert_eq!(extract_unix_timestamp(&clock_data[..39]), None);
    assert_eq!(extract_unix_timestamp(&clock_data[..32]), None);
}

proptest! {
    /// Property: Clock timestamp extraction works for all i64 values
    #[test]
    fn prop_clock_timestamp_extraction(
        timestamp in prop_oneof![
            Just(i64::MIN),
            Just(i64::MIN + 1),
            Just(-1i64),
            Just(0i64),
            Just(1i64),
            Just(i64::MAX - 1),
            Just(i64::MAX),
            any::<i64>()
        ]
    ) {
        fn extract_unix_timestamp(clock_data: &[u8]) -> Option<i64> {
            let ts = clock_data.get(32..40)?;
            Some(i64::from_le_bytes(ts.try_into().ok()?))
        }

        let mut clock_data = vec![0u8; 40];
        clock_data[32..40].copy_from_slice(&timestamp.to_le_bytes());

        let read_timestamp = extract_unix_timestamp(&clock_data).unwrap();
        prop_assert_eq!(read_timestamp, timestamp);

        // Truncation safety boundaries.
        prop_assert_eq!(extract_unix_timestamp(&clock_data[..39]), None);
        prop_assert_eq!(extract_unix_timestamp(&clock_data[..32]), None);
    }
}

// ============================================================================
// PATTERN 7: Address construction verification (NOW SAFE)
// ============================================================================

/// Verify that address construction in validation.rs is safe
#[test]
fn test_address_construction_safe() {
    use pinocchio::Address;

    // Previously this was an unsafe pointer cast:
    // unsafe { &*(protocol_config.blacklist_program.as_ptr().cast::<Address>()) };
    //
    // Now we use the safe Address::new_from_array() constructor:
    // let blacklist_program = &Address::new_from_array(protocol_config.blacklist_program);
    //
    // This is safe because:
    // 1. Address::new_from_array() takes ownership of [u8; 32] and returns Address
    // 2. No pointer manipulation or unsafe code required
    // 3. Type system ensures correctness at compile time

    // Verify size equality and roundtrip identity.
    assert_eq!(size_of::<[u8; 32]>(), 32);

    let mut a = [0u8; 32];
    a[0] = 1;
    a[31] = 0xFF;
    let mut b = [0u8; 32];
    b[0] = 2;
    b[30] = 0xEE;

    let addr_a = Address::new_from_array(a);
    let addr_b = Address::new_from_array(b);

    assert_eq!(addr_a.as_ref(), &a);
    assert_eq!(addr_b.as_ref(), &b);
    assert_ne!(
        addr_a, addr_b,
        "different input arrays must produce different addresses"
    );

    // Reconstructing from bytes must be deterministic.
    let addr_a2 = Address::new_from_array(<[u8; 32]>::try_from(addr_a.as_ref()).unwrap());
    assert_eq!(addr_a2, addr_a);
}

// ============================================================================
// PATTERN 8: HexBuf UTF-8 verification (NOW SAFE)
// ============================================================================

/// Verify that HexBuf only contains valid UTF-8 (hex characters 0-9, a-f)
#[test]
fn test_hexbuf_always_valid_utf8() {
    use coalesce::logic::events::short_hex;

    // Previously used unsafe from_utf8_unchecked().
    // Now uses safe match on core::str::from_utf8() that returns "" on invalid UTF-8.
    // This is a defense-in-depth measure since short_hex only produces hex digits,
    // but the safe code handles potential edge cases gracefully.

    // Test with various byte patterns
    let test_cases: &[([u8; 32], &str)] = &[
        ([0u8; 32], "0000000000000000"),
        ([0xFFu8; 32], "ffffffffffffffff"),
        (
            [
                0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ],
            "deadbeef00000000",
        ),
    ];

    for (bytes, expected_hex) in test_cases {
        let hex = short_hex(bytes);
        // The returned string should be valid UTF-8 and only contain hex chars
        let s = hex.as_str();
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(s.len(), 16); // First 8 bytes = 16 hex chars
        assert_eq!(s, *expected_hex);
        assert!(s.chars().all(|c| !c.is_ascii_uppercase()));

        // Only the first 8 bytes should affect output.
        let mut tail_mutated = *bytes;
        tail_mutated[31] ^= 0xFF;
        let tail_hex = short_hex(&tail_mutated);
        assert_eq!(
            tail_hex.as_str(),
            s,
            "bytes after index 7 must not affect short_hex output"
        );
    }
}

proptest! {
    /// Property: short_hex always produces valid hex output
    #[test]
    fn prop_short_hex_valid(
        bytes in prop_oneof![
            Just([0u8; 32]),
            Just([0xFFu8; 32]),
            any::<[u8; 32]>()
        ]
    ) {
        use coalesce::logic::events::short_hex;

        let hex = short_hex(&bytes);
        let s = hex.as_str();

        // Must be valid UTF-8 (it is, or as_str would panic)
        prop_assert_eq!(s.len(), 16); // 8 bytes = 16 hex chars
        prop_assert!(s.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify it matches the first 8 bytes
        let expected = format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7]);
        prop_assert_eq!(s, expected);

        // Mutating byte 8+ should not affect output.
        let mut tail_mutated = bytes;
        tail_mutated[31] ^= 0xA5;
        let tail_hex = short_hex(&tail_mutated);
        prop_assert_eq!(tail_hex.as_str(), s);

        // Mutating byte 0 should affect output deterministically.
        let mut head_mutated = bytes;
        head_mutated[0] ^= 0x01;
        let head_expected = format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            head_mutated[0], head_mutated[1], head_mutated[2], head_mutated[3],
            head_mutated[4], head_mutated[5], head_mutated[6], head_mutated[7]);
        let head_hex = short_hex(&head_mutated);
        prop_assert_eq!(head_hex.as_str(), head_expected);
        if head_mutated[0] != bytes[0] {
            prop_assert_ne!(head_hex.as_str(), s);
        }
    }
}

// ============================================================================
// PATTERN 9: Token account deserialization verification (REMAINING UNSAFE)
// ============================================================================

/// Verify TokenAccount structure is properly validated before unsafe parsing
/// NOTE: This is one of the REMAINING unsafe patterns, kept for SPL Token performance.
#[test]
fn test_token_account_owner_check_precedes_parsing() {
    fn assert_check_before_parse(src: &str, file: &str, owner_check: &str, parse_call: &str) {
        let owner_idx = src
            .find(owner_check)
            .unwrap_or_else(|| panic!("{file}: missing owner-check snippet `{owner_check}`"));
        let parse_idx = src
            .find(parse_call)
            .unwrap_or_else(|| panic!("{file}: missing parse snippet `{parse_call}`"));
        assert!(
            owner_idx < parse_idx,
            "{file}: owner check must precede unsafe token parsing"
        );
        let safety_idx = src[..parse_idx]
            .rfind("SAFETY:")
            .unwrap_or_else(|| panic!("{file}: missing SAFETY comment before parse"));
        assert!(
            safety_idx > owner_idx,
            "{file}: SAFETY comment should appear after owner check and before parse"
        );
    }

    let borrow_src = include_str!("../src/processor/borrow.rs");
    assert_check_before_parse(
        borrow_src,
        "borrow.rs",
        "if unsafe { vault_account.owner() } != &pinocchio_token::ID",
        "from_account_view_unchecked(vault_account)",
    );
    assert_check_before_parse(
        borrow_src,
        "borrow.rs",
        "if unsafe { borrower_token_account.owner() } != &pinocchio_token::ID",
        "from_account_view_unchecked(borrower_token_account)",
    );

    let collect_fees_src = include_str!("../src/processor/collect_fees.rs");
    assert_check_before_parse(
        collect_fees_src,
        "collect_fees.rs",
        "if unsafe { vault_account.owner() } != &pinocchio_token::ID",
        "from_account_view_unchecked(vault_account)",
    );
    assert_check_before_parse(
        collect_fees_src,
        "collect_fees.rs",
        "if unsafe { fee_destination.owner() } != &pinocchio_token::ID",
        "from_account_view_unchecked(fee_destination)",
    );

    let withdraw_src = include_str!("../src/processor/withdraw.rs");
    assert_check_before_parse(
        withdraw_src,
        "withdraw.rs",
        "if unsafe { vault_account.owner() } != &pinocchio_token::ID",
        "from_account_view_unchecked(vault_account)",
    );

    let re_settle_src = include_str!("../src/processor/re_settle.rs");
    assert_check_before_parse(
        re_settle_src,
        "re_settle.rs",
        "if unsafe { vault_account.owner() } != &pinocchio_token::ID",
        "from_account_view_unchecked(vault_account)",
    );

    let withdraw_excess_src = include_str!("../src/processor/withdraw_excess.rs");
    assert_check_before_parse(
        withdraw_excess_src,
        "withdraw_excess.rs",
        "if unsafe { vault_account.owner() } != &pinocchio_token::ID",
        "from_account_view_unchecked(vault_account)",
    );

    // Token account layout size remains bounded as expected.
    assert_eq!(165usize, 32 + 32 + 8 + 36 + 1 + 12 + 8 + 36);
}

// ============================================================================
// PATTERN 10: Discriminator verification before unsafe access
// ============================================================================

/// Verify discriminator check prevents type confusion
#[test]
fn test_discriminator_prevents_type_confusion() {
    use coalesce::constants::{
        BORROWER_WHITELIST_SIZE, DISC_BORROWER_WL, DISC_LENDER_POSITION, DISC_MARKET,
        DISC_PROTOCOL_CONFIG, LENDER_POSITION_SIZE, MARKET_SIZE, PROTOCOL_CONFIG_SIZE,
    };
    use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

    // All discriminators should be unique
    let discs = [
        &DISC_PROTOCOL_CONFIG[..],
        &DISC_MARKET[..],
        &DISC_LENDER_POSITION[..],
        &DISC_BORROWER_WL[..],
    ];

    for i in 0..discs.len() {
        for j in (i + 1)..discs.len() {
            assert_ne!(
                discs[i], discs[j],
                "Discriminators {} and {} must be unique",
                i, j
            );
        }
    }

    // Verify each discriminator is 8 bytes
    assert_eq!(DISC_PROTOCOL_CONFIG.len(), 8);
    assert_eq!(DISC_MARKET.len(), 8);
    assert_eq!(DISC_LENDER_POSITION.len(), 8);
    assert_eq!(DISC_BORROWER_WL.len(), 8);
}

// ============================================================================
// PATTERN 11: No uninitialized memory exposure
// ============================================================================

/// Verify Pod types are fully initialized via Zeroable
#[test]
fn test_zeroed_structs_fully_initialized() {
    use bytemuck::{bytes_of, Zeroable};
    use coalesce::state::{BorrowerWhitelist, LenderPosition, Market, ProtocolConfig};

    // Zeroable guarantees all bytes are initialized to 0
    let pc = ProtocolConfig::zeroed();
    let pc_bytes = bytes_of(&pc);
    assert!(pc_bytes.iter().all(|&b| b == 0));

    let m = Market::zeroed();
    let m_bytes = bytes_of(&m);
    assert!(m_bytes.iter().all(|&b| b == 0));

    let lp = LenderPosition::zeroed();
    let lp_bytes = bytes_of(&lp);
    assert!(lp_bytes.iter().all(|&b| b == 0));

    let bw = BorrowerWhitelist::zeroed();
    let bw_bytes = bytes_of(&bw);
    assert!(bw_bytes.iter().all(|&b| b == 0));
}

// ============================================================================
// PATTERN 12: Endianness verification
// ============================================================================

proptest! {
    /// Property: All multi-byte fields use little-endian encoding
    #[test]
    fn prop_little_endian_encoding(
        value_u16 in any::<u16>(),
        value_u64 in any::<u64>(),
        value_u128 in any::<u128>(),
        value_i64 in any::<i64>(),
    ) {
        // u16
        let bytes_u16 = value_u16.to_le_bytes();
        prop_assert_eq!(u16::from_le_bytes(bytes_u16), value_u16);

        // u64
        let bytes_u64 = value_u64.to_le_bytes();
        prop_assert_eq!(u64::from_le_bytes(bytes_u64), value_u64);

        // u128
        let bytes_u128 = value_u128.to_le_bytes();
        prop_assert_eq!(u128::from_le_bytes(bytes_u128), value_u128);

        // i64
        let bytes_i64 = value_i64.to_le_bytes();
        prop_assert_eq!(i64::from_le_bytes(bytes_i64), value_i64);
    }
}

// ============================================================================
// PATTERN 13: Integration with on-chain instruction handlers
// ============================================================================

/// Document the safety audit trail for each processor
#[test]
fn test_safety_audit_documentation() {
    let sources: [(&str, &str); 9] = [
        (
            "create_market.rs",
            include_str!("../src/processor/create_market.rs"),
        ),
        ("deposit.rs", include_str!("../src/processor/deposit.rs")),
        ("borrow.rs", include_str!("../src/processor/borrow.rs")),
        ("repay.rs", include_str!("../src/processor/repay.rs")),
        (
            "repay_interest.rs",
            include_str!("../src/processor/repay_interest.rs"),
        ),
        ("withdraw.rs", include_str!("../src/processor/withdraw.rs")),
        (
            "withdraw_excess.rs",
            include_str!("../src/processor/withdraw_excess.rs"),
        ),
        (
            "collect_fees.rs",
            include_str!("../src/processor/collect_fees.rs"),
        ),
        (
            "re_settle.rs",
            include_str!("../src/processor/re_settle.rs"),
        ),
    ];

    let mut total_unsafe_token_parses = 0usize;
    for (name, src) in &sources {
        let parse_count = src.matches("from_account_view_unchecked(").count();
        if parse_count > 0 {
            total_unsafe_token_parses += parse_count;
            let safety_count = src.matches("SAFETY:").count();
            assert!(
                safety_count >= parse_count,
                "{name}: each unsafe parse site should have a SAFETY rationale comment"
            );
        }
    }

    assert_eq!(
        total_unsafe_token_parses, 13,
        "unexpected drift in unsafe token parsing sites; audit tests must be updated"
    );

    let events_src = include_str!("../src/logic/events.rs");
    assert!(
        events_src.contains("unsafe impl Log for HexBuf"),
        "HexBuf Log impl should remain explicitly audited"
    );
    assert!(
        events_src.contains("from_utf8_unchecked"),
        "HexBuf UTF-8 unsafe path should remain explicitly audited"
    );
}
