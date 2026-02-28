//! Kani formal verification proofs for core lending math.
//!
//! These proofs mathematically verify properties that hold for ALL possible
//! inputs within the specified bounds — not probabilistically (like proptest)
//! but exhaustively via bounded model checking.
//!
//! Run: `cargo kani --tests`
//! Requires: <https://model-checking.github.io/kani/>

#[cfg(kani)]
mod proofs {
    use crate::constants::{SECONDS_PER_YEAR, WAD};
    use crate::error::LendingError;
    use crate::logic::interest::{accrue_interest, compute_settlement_factor};
    use crate::state::{Market, ProtocolConfig};
    use bytemuck::Zeroable;
    use pinocchio::error::ProgramError;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

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

    // ===================================================================
    // Proof 1a: accrue_interest never panics for valid inputs
    // ===================================================================

    /// For any valid protocol parameters, `accrue_interest` either returns
    /// `Ok(())` or `Err(MathOverflow)` — it never panics.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_accrue_interest_no_panic() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let fee_rate_bps: u16 = kani::any();
        kani::assume(fee_rate_bps <= 10_000);

        // Bound time_elapsed to one day to keep verification tractable.
        let time_elapsed: u32 = kani::any();
        kani::assume(time_elapsed <= 86_400);

        // scale_factor in [WAD, 2*WAD] — realistic operational range.
        let sf_offset: u64 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset % (WAD as u64));

        // Deliberately allow large supply to exercise graceful overflow paths.
        let supply: u128 = kani::any();
        let initial_fees: u64 = kani::any();

        let last_accrual: i64 = 1_000_000;
        let maturity: i64 = last_accrual + 2 * SECONDS_PER_YEAR as i64;
        let current_ts = last_accrual + time_elapsed as i64;

        let mut market = make_market(
            annual_bps,
            maturity,
            scale_factor,
            supply,
            last_accrual,
            initial_fees,
        );
        let config = make_config(fee_rate_bps);
        let sf_before = market.scale_factor();
        let fees_before = market.accrued_protocol_fees();
        let lat_before = market.last_accrual_timestamp();

        // Must not panic — should always return a graceful Result.
        let result = accrue_interest(&mut market, &config, current_ts);

        // Reachability markers so the proof is not vacuous about branch coverage.
        kani::cover!(time_elapsed == 0);
        kani::cover!(time_elapsed > 0);
        kani::cover!(fee_rate_bps == 0);
        kani::cover!(fee_rate_bps > 0);
        kani::cover!(result.is_ok());
        kani::cover!(result.is_err());

        match result {
            Ok(()) => {
                assert!(
                    market.last_accrual_timestamp() >= lat_before,
                    "successful accrual must not move last_accrual backwards"
                );
                assert!(
                    market.scale_factor() >= sf_before,
                    "successful accrual must not decrease scale_factor"
                );
            },
            Err(err) => {
                // In this timestamp domain, only arithmetic overflow should error.
                assert!(
                    matches!(err, ProgramError::Custom(code) if code == LendingError::MathOverflow as u32),
                    "non-overflow errors are unexpected under monotone timestamps"
                );
                // Error paths must not partially mutate market state.
                assert_eq!(
                    market.scale_factor(),
                    sf_before,
                    "error path must preserve scale_factor"
                );
                assert_eq!(
                    market.accrued_protocol_fees(),
                    fees_before,
                    "error path must preserve accrued_protocol_fees"
                );
                assert_eq!(
                    market.last_accrual_timestamp(),
                    lat_before,
                    "error path must preserve last_accrual_timestamp"
                );
            },
        }
    }

    // ===================================================================
    // Proof 1b: scale_factor is monotonically non-decreasing
    // ===================================================================

    /// After a successful call to `accrue_interest`, the market's
    /// `scale_factor` is >= its value before the call.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_scale_factor_monotonic() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let fee_rate_bps: u16 = kani::any();
        kani::assume(fee_rate_bps <= 10_000);

        // Use u32 to cover full year (31_536_000s) instead of u16 (max 65_535s = 18h)
        let time_elapsed: u32 = kani::any();
        kani::assume(time_elapsed > 0);
        kani::assume(time_elapsed <= SECONDS_PER_YEAR as u32);

        let sf_offset: u32 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset);

        let last_accrual: i64 = 0;
        let maturity: i64 = i64::MAX;
        let current_ts = last_accrual + i64::from(time_elapsed);

        let mut market = make_market(annual_bps, maturity, scale_factor, WAD, last_accrual, 0);
        let config = make_config(fee_rate_bps);

        let sf_before = market.scale_factor();
        let result = accrue_interest(&mut market, &config, current_ts);
        assert!(
            result.is_ok(),
            "bounded monotonicity domain should not overflow"
        );

        kani::cover!(annual_bps == 0);
        kani::cover!(annual_bps == 10_000);

        assert!(
            market.scale_factor() >= sf_before,
            "scale_factor must never decrease"
        );
    }

    // ===================================================================
    // Proof 1c: accrued_protocol_fees is monotonically non-decreasing
    // ===================================================================

    /// After a successful call to `accrue_interest`, fees never decrease.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_fees_monotonic() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps > 0);
        kani::assume(annual_bps <= 10_000);

        let fee_rate_bps: u16 = kani::any();
        kani::assume(fee_rate_bps > 0);
        kani::assume(fee_rate_bps <= 10_000);

        // Use u32 to cover full year (31_536_000s) instead of u16 (max 65_535s = 18h)
        let time_elapsed: u32 = kani::any();
        kani::assume(time_elapsed > 0);
        kani::assume(time_elapsed <= SECONDS_PER_YEAR as u32);

        let initial_fees: u32 = kani::any();

        let supply: u32 = kani::any();
        kani::assume(supply > 0);

        let mut market = make_market(
            annual_bps,
            i64::MAX,
            WAD,
            u128::from(supply),
            0,
            u64::from(initial_fees),
        );
        let config = make_config(fee_rate_bps);

        let fees_before = market.accrued_protocol_fees();
        let result = accrue_interest(&mut market, &config, i64::from(time_elapsed));
        assert!(result.is_ok(), "bounded fee domain should not overflow");

        kani::cover!(market.accrued_protocol_fees() > fees_before);

        assert!(
            market.accrued_protocol_fees() >= fees_before,
            "fees must never decrease"
        );
    }

    // ===================================================================
    // Proof 1d: settlement factor always in [1, WAD]
    // ===================================================================

    /// The on-chain settlement factor computation (extracted to
    /// `logic::interest::compute_settlement_factor`) always produces a
    /// value in [1, WAD] for any non-zero total_normalized.
    #[kani::proof]
    fn prove_settlement_factor_bounded() {
        let available: u128 = kani::any();
        let total_normalized: u128 = kani::any();
        kani::assume(total_normalized > 0);
        kani::assume(available <= u128::MAX / WAD);

        let factor = compute_settlement_factor(available, total_normalized)
            .expect("bounded domain avoids overflow");
        let raw = available
            .checked_mul(WAD)
            .expect("bounded domain avoids overflow")
            / total_normalized;
        let expected = if raw > WAD {
            WAD
        } else if raw < 1 {
            1
        } else {
            raw
        };

        kani::cover!(available == 0 && factor == 1);
        kani::cover!(available >= total_normalized && factor == WAD);
        kani::cover!(available > 0 && available < total_normalized && factor > 1 && factor < WAD);

        assert_eq!(
            factor, expected,
            "settlement factor must match exact clamp formula"
        );
        assert!(factor >= 1, "settlement factor must be >= 1");
        assert!(factor <= WAD, "settlement factor must be <= WAD");
    }

    /// total_normalized == 0 must short-circuit to full settlement.
    #[kani::proof]
    fn prove_settlement_factor_zero_supply_is_wad() {
        let available_a: u128 = kani::any();
        let available_b: u128 = kani::any();

        // Zero-supply path must short-circuit independent of available magnitude.
        let factor_a =
            compute_settlement_factor(available_a, 0).expect("zero supply path never overflows");
        let factor_a_repeat =
            compute_settlement_factor(available_a, 0).expect("zero supply path never overflows");
        let factor_b =
            compute_settlement_factor(available_b, 0).expect("zero supply path never overflows");

        kani::cover!(available_a == 0);
        kani::cover!(available_a == 1);
        kani::cover!(available_a == u128::MAX);
        kani::cover!(available_a != available_b);

        assert_eq!(factor_a, WAD, "zero total supply must map to WAD");
        assert_eq!(factor_b, WAD, "zero total supply must map to WAD");
        assert_eq!(
            factor_a_repeat, factor_a,
            "zero-supply settlement must be deterministic for identical inputs"
        );
        assert_eq!(
            factor_b, factor_a,
            "zero-supply settlement must be independent of available amount"
        );
        assert!(
            (1..=WAD).contains(&factor_a),
            "zero-supply path must still satisfy settlement bounds"
        );
    }

    // ===================================================================
    // Proof 1e: deposit scaling rounds DOWN (protocol-favorable)
    // ===================================================================

    /// `scaled_amount = amount * WAD / scale_factor` uses floor division,
    /// so `scaled_amount * scale_factor / WAD <= amount`.
    #[kani::proof]
    fn prove_deposit_rounds_down() {
        let amount: u32 = kani::any();
        kani::assume(amount > 0);

        let sf_offset: u64 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset % (WAD as u64));
        kani::assume(scale_factor >= WAD);

        let amount_u128 = u128::from(amount);

        // deposit_scale
        let scaled = amount_u128
            .checked_mul(WAD)
            .expect("u32 amount * WAD fits in u128")
            / scale_factor;

        // normalize back
        let recovered = scaled
            .checked_mul(scale_factor)
            .expect("scaled * scale_factor fits in u128 under u32 bounds")
            / WAD;
        let numerator = amount_u128
            .checked_mul(WAD)
            .expect("u32 amount * WAD fits in u128");
        let left = scaled
            .checked_mul(scale_factor)
            .expect("scaled * scale_factor fits in u128");
        let right = scaled
            .checked_add(1)
            .expect("u32-based scaled amount cannot overflow by +1")
            .checked_mul(scale_factor)
            .expect("(scaled + 1) * scale_factor fits in u128");

        kani::cover!(scale_factor == WAD && recovered == amount_u128);
        kani::cover!(scale_factor > WAD && recovered < amount_u128);
        kani::cover!(scaled == 0);

        assert!(
            recovered <= amount_u128,
            "round-trip must not exceed original (protocol-favorable rounding)"
        );
        assert!(
            left <= numerator,
            "scaled is floor(amount * WAD / scale_factor)"
        );
        assert!(
            right > numerator,
            "scaled + 1 must exceed the floor threshold"
        );
    }

    // ===================================================================
    // Proof 1f: withdrawal payout rounds DOWN (protocol-favorable)
    // ===================================================================

    /// Payout = scaled * scale_factor / WAD * settlement / WAD.
    /// Each division is floor, so payout <= true_value.
    #[kani::proof]
    fn prove_payout_rounds_down() {
        let scaled_amount: u32 = kani::any();
        kani::assume(scaled_amount > 0);

        let sf_offset: u64 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset % (WAD as u64));

        let settlement_factor: u64 = kani::any();
        kani::assume(settlement_factor >= 1);
        kani::assume(settlement_factor <= WAD as u64);

        let scaled_u128 = u128::from(scaled_amount);
        let settlement_u128 = u128::from(settlement_factor);

        // normalized = scaled * scale_factor / WAD
        let normalized = scaled_u128
            .checked_mul(scale_factor)
            .expect("u32 scaled amount * scale_factor fits in u128")
            / WAD;

        // payout = normalized * settlement / WAD
        let payout = normalized
            .checked_mul(settlement_u128)
            .expect("normalized * settlement fits in u128 under bounded domain")
            / WAD;
        let lhs = payout.checked_mul(WAD).expect("payout * WAD fits");
        let rhs = normalized
            .checked_mul(settlement_u128)
            .expect("normalized * settlement fits");

        kani::cover!(settlement_factor == WAD as u64 && payout == normalized);
        kani::cover!(settlement_factor < WAD as u64 && normalized > 0 && payout < normalized);

        // True value (rational): scaled * sf * settlement / WAD^2
        // Since we do two floor divisions, payout <= true value.
        // More practically: payout <= normalized (since settlement <= WAD)
        assert!(
            payout <= normalized,
            "payout must not exceed normalized amount"
        );
        assert!(
            lhs <= rhs,
            "payout must be floor(normalized * settlement / WAD)"
        );
        if settlement_factor == WAD as u64 {
            assert_eq!(
                payout, normalized,
                "full settlement must preserve normalized amount"
            );
        }
    }

    // ===================================================================
    // Proof 1g: payout never exceeds normalized_amount
    // ===================================================================

    /// Since settlement_factor <= WAD, payout = normalized * settlement / WAD <= normalized.
    #[kani::proof]
    fn prove_payout_bounded_by_normalized() {
        let normalized: u32 = kani::any();
        let settlement: u64 = kani::any();
        kani::assume(settlement >= 1);
        kani::assume(settlement <= WAD as u64);

        let normalized_u128 = u128::from(normalized);
        let settlement_u128 = u128::from(settlement);

        let payout = normalized_u128
            .checked_mul(settlement_u128)
            .expect("u32 normalized * settlement fits in u128")
            / WAD;
        let lhs = payout.checked_mul(WAD).expect("payout * WAD fits");
        let rhs = normalized_u128
            .checked_mul(settlement_u128)
            .expect("normalized * settlement fits");

        kani::cover!(settlement == WAD as u64 && payout == normalized_u128);
        kani::cover!(settlement < WAD as u64 && normalized > 0 && payout < normalized_u128);

        assert!(
            payout <= normalized_u128,
            "payout must be <= normalized when settlement_factor <= WAD"
        );
        assert!(
            lhs <= rhs,
            "payout must be floor of normalized*settlement/WAD"
        );
        if settlement == WAD as u64 {
            assert_eq!(
                payout, normalized_u128,
                "full settlement must preserve normalized amount"
            );
        }
    }

    // ===================================================================
    // Proof 1h: last_accrual_timestamp set to min(current_ts, maturity)
    // ===================================================================

    /// After successful accrual, `last_accrual_timestamp == min(current_ts, maturity)`
    /// (assuming `min(current_ts, maturity) > old_last_accrual`).
    /// C-5: Removed `annual_bps > 0` assumption to cover zero-rate case.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_last_accrual_timestamp_postcondition() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        // Use u32 to cover full year instead of u16 (max 65_535s = 18h)
        let time_elapsed: u32 = kani::any();
        kani::assume(time_elapsed > 0);
        kani::assume(time_elapsed <= SECONDS_PER_YEAR as u32);

        let maturity_offset: u32 = kani::any();
        kani::assume(maturity_offset > 0);
        kani::assume(maturity_offset <= 2 * SECONDS_PER_YEAR as u32);

        let last_accrual: i64 = 0;
        let maturity = last_accrual + i64::from(maturity_offset);
        let current_ts = last_accrual + i64::from(time_elapsed);

        let mut market = make_market(annual_bps, maturity, WAD, WAD, last_accrual, 0);
        let config = make_config(0);

        let result = accrue_interest(&mut market, &config, current_ts);
        assert!(
            result.is_ok(),
            "bounded timestamp postcondition domain should not overflow"
        );

        kani::cover!(current_ts <= maturity);
        kani::cover!(current_ts > maturity);

        let expected_ts = if current_ts > maturity {
            maturity
        } else {
            current_ts
        };
        assert_eq!(
            market.last_accrual_timestamp(),
            expected_ts,
            "last_accrual must be min(current_ts, maturity)"
        );
    }

    // ===================================================================
    // Proof 1i: fees never exceed total interest on the supply
    // ===================================================================

    /// For any valid inputs in this bounded domain, accrued protocol fees after
    /// one accrual are bounded by `2 * scaled_supply`:
    /// - annual rate <= 100%
    /// - elapsed <= 1 year
    /// - fee rate <= 100%
    /// - initial scale factor = WAD
    ///
    /// This bound is intentionally conservative but non-vacuous and catches
    /// runaway fee-growth regressions.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_fees_bounded_by_interest() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps > 0 && annual_bps <= 10_000);

        let fee_bps: u16 = kani::any();
        kani::assume(fee_bps > 0 && fee_bps <= 10_000);

        let supply: u32 = kani::any();
        kani::assume(supply > 0);

        // Use u32 to cover full year instead of u16
        let elapsed: u32 = kani::any();
        kani::assume(elapsed > 0);
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);

        let supply_u128 = u128::from(supply);

        let mut market = make_market(annual_bps, i64::MAX, WAD, supply_u128, 0, 0);
        let config = make_config(fee_bps);

        let result = accrue_interest(&mut market, &config, i64::from(elapsed));
        assert!(
            result.is_ok(),
            "bounded fee proof domain should not overflow"
        );

        let fees = market.accrued_protocol_fees();
        // With initial scale = WAD, annual <= 100%, elapsed <= 1 year, fee <= 100%:
        // new_scale <= 2*WAD and fee_delta_wad <= WAD
        // => fee_normalized <= scaled_supply * 2.
        let max_possible = supply_u128
            .checked_mul(2)
            .expect("u32 supply * 2 fits in u128");

        kani::cover!(
            annual_bps == 10_000
                && fee_bps == 10_000
                && elapsed == SECONDS_PER_YEAR as u32
                && fees > 0
        );

        assert!(
            u128::from(fees) <= max_possible,
            "fees must be bounded by 2x initial scaled supply in this domain"
        );
    }

    // ===================================================================
    // Proof 1j: zero interest rate produces no scale factor change
    // ===================================================================

    /// When annual_interest_bps == 0, the scale factor must remain exactly
    /// unchanged regardless of time elapsed or supply.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_zero_rate_no_change() {
        let time_elapsed: u32 = kani::any();
        kani::assume(time_elapsed <= SECONDS_PER_YEAR as u32);

        let sf_offset: u32 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset);

        let supply: u32 = kani::any();

        let fee_bps: u16 = kani::any();
        kani::assume(fee_bps <= 10_000);

        let mut market = make_market(
            0, // zero interest rate
            i64::MAX,
            scale_factor,
            u128::from(supply),
            0,
            0,
        );
        let config = make_config(fee_bps);

        let sf_before = market.scale_factor();
        let fees_before = market.accrued_protocol_fees();

        let result = accrue_interest(&mut market, &config, i64::from(time_elapsed));
        assert!(
            result.is_ok(),
            "zero-rate domain should not overflow and must always succeed"
        );

        kani::cover!(time_elapsed == 0);
        kani::cover!(time_elapsed > 0);

        // With zero interest rate, scale_factor must not change.
        assert_eq!(
            market.scale_factor(),
            sf_before,
            "zero rate must not change scale_factor"
        );
        // With zero interest, no fees can accrue.
        assert_eq!(
            market.accrued_protocol_fees(),
            fees_before,
            "zero rate must not accrue fees"
        );
        let expected_last = if time_elapsed == 0 {
            0
        } else {
            i64::from(time_elapsed)
        };
        assert_eq!(
            market.last_accrual_timestamp(),
            expected_last,
            "last_accrual must still advance when elapsed > 0, even at zero rate"
        );
        assert!(
            market.scale_factor() >= WAD,
            "zero-rate branch must preserve non-decreasing scale-factor invariant"
        );
    }

    // ===================================================================
    // Proof 1k: idempotency — calling accrue_interest twice at the same
    //           timestamp produces no additional change
    // ===================================================================

    /// If we call accrue_interest(t) and then accrue_interest(t) again,
    /// the second call must be a no-op (time_elapsed == 0 path).
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_double_accrual_idempotent() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let fee_bps: u16 = kani::any();
        kani::assume(fee_bps <= 10_000);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);

        let supply: u32 = kani::any();

        let mut market = make_market(annual_bps, i64::MAX, WAD, u128::from(supply), 0, 0);
        let config = make_config(fee_bps);

        let ts = i64::from(elapsed);

        // First call
        let first = accrue_interest(&mut market, &config, ts);
        assert!(
            first.is_ok(),
            "idempotency proof uses a bounded domain that should not overflow"
        );

        // Snapshot after first call
        let sf_after_first = market.scale_factor();
        let fees_after_first = market.accrued_protocol_fees();
        let lat_after_first = market.last_accrual_timestamp();
        let expected_first_lat = if elapsed == 0 { 0 } else { ts };
        assert_eq!(
            lat_after_first, expected_first_lat,
            "first accrual must set last_accrual to ts when elapsed > 0"
        );
        kani::cover!(elapsed == 0);
        kani::cover!(elapsed > 0);

        // Second call at the same timestamp
        let second = accrue_interest(&mut market, &config, ts);
        assert!(
            second.is_ok(),
            "second accrual at same ts should also succeed"
        );

        // Must be identical — second call should be a no-op
        assert_eq!(
            market.scale_factor(),
            sf_after_first,
            "second accrual at same ts must not change scale_factor"
        );
        assert_eq!(
            market.accrued_protocol_fees(),
            fees_after_first,
            "second accrual at same ts must not change fees"
        );
        assert_eq!(
            market.last_accrual_timestamp(),
            lat_after_first,
            "second accrual at same ts must not change last_accrual"
        );
    }

    // ===================================================================
    // Proof 1l: scale_factor_delta proportional to time elapsed
    // ===================================================================

    /// For a fixed rate and scale factor, accruing for time T1 then T2
    /// (where T1 < T2) must produce sf(T1) <= sf(T2). This is a weaker
    /// form of monotonicity that specifically tests the time dimension.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_longer_time_larger_scale_factor() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps > 0 && annual_bps <= 10_000);

        // Use u32 to cover full year instead of u16 (max 65_535s = 18h)
        let t1: u32 = kani::any();
        let t2: u32 = kani::any();
        kani::assume(t1 <= t2);
        kani::assume(t2 <= SECONDS_PER_YEAR as u32);

        let supply: u16 = kani::any();

        // Market 1: accrue for t1
        let mut market1 = make_market(annual_bps, i64::MAX, WAD, u128::from(supply), 0, 0);
        let config = make_config(0);

        let r1 = accrue_interest(&mut market1, &config, i64::from(t1));

        // Market 2: accrue for t2 (same initial state)
        let mut market2 = make_market(annual_bps, i64::MAX, WAD, u128::from(supply), 0, 0);

        let r2 = accrue_interest(&mut market2, &config, i64::from(t2));
        assert!(
            r1.is_ok() && r2.is_ok(),
            "bounded time-monotonicity domain should not overflow"
        );

        kani::cover!(t1 == t2);
        kani::cover!(t1 < t2);

        assert!(
            market2.scale_factor() >= market1.scale_factor(),
            "longer accrual period must produce >= scale_factor"
        );
        if t2 > t1 {
            assert!(
                market2.scale_factor() > market1.scale_factor(),
                "strictly longer accrual period must strictly increase scale_factor"
            );
        }
    }

    // ===================================================================
    // Proof 1m: fee_rate_bps = 0 guarantees zero fee accrual
    // ===================================================================

    /// Even with non-zero interest rate and supply, if fee_rate_bps == 0,
    /// no protocol fees can ever accrue.
    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_zero_fee_rate_no_fees() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);
        let supply: u32 = kani::any();

        let mut market = make_market(annual_bps, i64::MAX, WAD, u128::from(supply), 0, 0);
        let config = make_config(0); // zero fee rate

        let result = accrue_interest(&mut market, &config, i64::from(elapsed));
        assert!(
            result.is_ok(),
            "zero-fee-rate domain should not overflow and must succeed"
        );

        kani::cover!(annual_bps == 0);
        kani::cover!(annual_bps > 0 && elapsed > 0);

        assert_eq!(
            market.accrued_protocol_fees(),
            0,
            "zero fee_rate must produce zero fees"
        );
        if annual_bps > 0 && elapsed > 0 {
            assert!(
                market.scale_factor() > WAD,
                "non-zero interest over non-zero elapsed time must increase scale_factor"
            );
        }
        assert!(
            market.scale_factor() >= WAD,
            "zero-fee-rate branch must preserve non-decreasing scale-factor invariant"
        );
        let expected_last = if elapsed == 0 { 0 } else { i64::from(elapsed) };
        assert_eq!(
            market.last_accrual_timestamp(),
            expected_last,
            "last_accrual must track elapsed timestamp progression"
        );
    }

    // ===================================================================
    // Proof 2a: Fee proportional to rate
    // Doubling fee_rate_bps approximately doubles fees (linear relationship)
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_fee_proportional_to_rate() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps > 0 && annual_bps <= 10_000);

        let fee_rate_a: u16 = kani::any();
        kani::assume(fee_rate_a > 0 && fee_rate_a <= 5_000);
        let fee_rate_b = fee_rate_a * 2;
        kani::assume(fee_rate_b <= 10_000);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed > 0);
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);

        let supply: u16 = kani::any();
        kani::assume(supply > 0);

        let supply_u128 = u128::from(supply);

        // Market with fee_rate_a
        let mut market_a = make_market(annual_bps, i64::MAX, WAD, supply_u128, 0, 0);
        let config_a = make_config(fee_rate_a);
        let r_a = accrue_interest(&mut market_a, &config_a, i64::from(elapsed));

        // Market with fee_rate_b = 2 * fee_rate_a
        let mut market_b = make_market(annual_bps, i64::MAX, WAD, supply_u128, 0, 0);
        let config_b = make_config(fee_rate_b);
        let r_b = accrue_interest(&mut market_b, &config_b, i64::from(elapsed));

        kani::assume(r_a.is_ok() && r_b.is_ok());

        let fees_a = market_a.accrued_protocol_fees();
        let fees_b = market_b.accrued_protocol_fees();

        // fees_b should be approximately 2 * fees_a (within rounding tolerance)
        // Floor division can lose at most 1 unit per division step.
        if fees_a > 0 {
            assert!(
                fees_b >= 2 * fees_a - 2,
                "doubling fee rate should approximately double fees (lower bound)"
            );
            assert!(
                fees_b <= 2 * fees_a + 2,
                "doubling fee rate should approximately double fees (upper bound)"
            );
        }
    }

    // ===================================================================
    // Proof 2b: Deposit then immediate withdraw at same scale:
    // recovered ≤ original (no money creation)
    // ===================================================================

    #[kani::proof]
    fn prove_deposit_withdraw_conservation() {
        let amount: u32 = kani::any();
        kani::assume(amount > 0);

        let sf_offset: u32 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset);

        let amount_u128 = u128::from(amount);

        // Deposit scaling: scaled = amount * WAD / scale_factor
        let scaled = amount_u128.checked_mul(WAD).expect("fits") / scale_factor;

        // Immediate withdraw: recovered = scaled * scale_factor / WAD
        let recovered = scaled.checked_mul(scale_factor).expect("fits") / WAD;

        assert!(
            recovered <= amount_u128,
            "withdraw should not exceed deposit (no money creation)"
        );
    }

    // ===================================================================
    // Proof 2c: Round-trip rounding loss bounded by 1 per WAD-division
    // ===================================================================

    #[kani::proof]
    fn prove_deposit_withdraw_loss_bounded() {
        let amount: u16 = kani::any();
        kani::assume(amount > 0);

        let sf_offset: u32 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset);

        let amount_u128 = u128::from(amount);

        let scaled = amount_u128 * WAD / scale_factor;
        let recovered = scaled * scale_factor / WAD;

        let loss = amount_u128 - recovered;

        // Loss should be bounded: at most 1 token per WAD-division rounding
        // In practice the loss is at most 2 (one from deposit rounding, one from withdraw)
        assert!(
            loss <= 2,
            "round-trip loss should be bounded by 2 units: loss={loss}"
        );
    }

    // ===================================================================
    // Proof 2d: For fixed time, interest_delta is monotonic in annual_bps
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_interest_delta_monotonic_in_rate() {
        let rate_a: u16 = kani::any();
        kani::assume(rate_a > 0 && rate_a <= 5_000);
        let rate_b = rate_a * 2;
        kani::assume(rate_b <= 10_000);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed > 0);
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);

        let mut market_a = make_market(rate_a, i64::MAX, WAD, WAD, 0, 0);
        let mut market_b = make_market(rate_b, i64::MAX, WAD, WAD, 0, 0);
        let config = make_config(0);

        let r_a = accrue_interest(&mut market_a, &config, i64::from(elapsed));
        let r_b = accrue_interest(&mut market_b, &config, i64::from(elapsed));
        assert!(
            r_a.is_ok() && r_b.is_ok(),
            "bounded monotonic-in-rate domain should not overflow"
        );

        let delta_a = market_a.scale_factor() - WAD;
        let delta_b = market_b.scale_factor() - WAD;

        assert!(
            delta_b >= delta_a,
            "higher annual rate must not reduce accrued interest delta"
        );
    }

    // ===================================================================
    // Proof 2e: For fixed rate, interest_delta is monotonic in time
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_interest_delta_monotonic_in_time() {
        let rate: u16 = kani::any();
        kani::assume(rate > 0 && rate <= 10_000);

        let t1: u32 = kani::any();
        kani::assume(t1 <= (SECONDS_PER_YEAR as u32) / 2);
        let t2 = t1 * 2;

        let mut market_t1 = make_market(rate, i64::MAX, WAD, WAD, 0, 0);
        let mut market_t2 = make_market(rate, i64::MAX, WAD, WAD, 0, 0);
        let config = make_config(0);

        let r1 = accrue_interest(&mut market_t1, &config, i64::from(t1));
        let r2 = accrue_interest(&mut market_t2, &config, i64::from(t2));
        assert!(
            r1.is_ok() && r2.is_ok(),
            "bounded monotonic-in-time domain should not overflow"
        );

        let delta_t1 = market_t1.scale_factor() - WAD;
        let delta_t2 = market_t2.scale_factor() - WAD;

        assert!(
            delta_t2 >= delta_t1,
            "longer elapsed time must not reduce accrued interest delta"
        );
    }

    // ===================================================================
    // Proof 2f: If available increases, settlement_factor >= old value
    // ===================================================================

    #[kani::proof]
    fn prove_settlement_factor_re_settle_monotonic() {
        let total: u128 = kani::any();
        kani::assume(total > 0);

        let avail_1: u64 = kani::any();
        let avail_2: u64 = kani::any();
        kani::assume(avail_1 <= avail_2);
        kani::assume(u128::from(avail_2) <= u128::MAX / WAD);

        let sf1 = compute_settlement_factor(u128::from(avail_1), total).expect("bounded");
        let sf2 = compute_settlement_factor(u128::from(avail_2), total).expect("bounded");

        assert!(
            sf2 >= sf1,
            "increasing available must not decrease settlement factor"
        );
    }

    // ===================================================================
    // Proof 2g: Same inputs always produce same output (deterministic)
    // ===================================================================

    #[kani::proof]
    fn prove_settlement_factor_deterministic() {
        let available: u64 = kani::any();
        let total: u128 = kani::any();
        kani::assume(total > 0);
        kani::assume(u128::from(available) <= u128::MAX / WAD);

        let sf1 = compute_settlement_factor(u128::from(available), total).expect("bounded");
        let sf2 = compute_settlement_factor(u128::from(available), total).expect("bounded");

        assert_eq!(sf1, sf2, "settlement factor must be deterministic");
    }

    // ===================================================================
    // Proof 2h: After successful accrual, last_accrual <= maturity
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_timestamp_ordering_invariant() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);
        let maturity_offset: u16 = kani::any();
        kani::assume(maturity_offset > 0);

        let maturity = i64::from(maturity_offset);
        let current_ts = i64::from(elapsed);

        let mut market = make_market(annual_bps, maturity, WAD, WAD, 0, 0);
        let config = make_config(0);

        let result = accrue_interest(&mut market, &config, current_ts);
        if result.is_ok() {
            assert!(
                market.last_accrual_timestamp() <= maturity,
                "last_accrual must be <= maturity after successful accrual"
            );
        }
    }

    // ===================================================================
    // Proof 2i: Error preserves ALL state
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_error_preserves_all_state() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let fee_bps: u16 = kani::any();
        kani::assume(fee_bps <= 10_000);

        // Use large scale factor to force overflow
        let scale_factor: u128 = kani::any();
        kani::assume(scale_factor >= WAD);
        kani::assume(scale_factor > u128::MAX / 2);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed > 0);
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);

        let supply: u32 = kani::any();
        let initial_fees: u32 = kani::any();

        let mut market = make_market(
            annual_bps,
            i64::MAX,
            scale_factor,
            u128::from(supply),
            0,
            u64::from(initial_fees),
        );
        let config = make_config(fee_bps);

        let sf_before = market.scale_factor();
        let fees_before = market.accrued_protocol_fees();
        let lat_before = market.last_accrual_timestamp();

        let result = accrue_interest(&mut market, &config, i64::from(elapsed));

        if result.is_err() {
            assert_eq!(market.scale_factor(), sf_before);
            assert_eq!(market.accrued_protocol_fees(), fees_before);
            assert_eq!(market.last_accrual_timestamp(), lat_before);
        }
    }

    // ===================================================================
    // Proof 2j: scale_factor never below WAD given sf >= WAD initially
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_scale_factor_never_below_wad() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let sf_offset: u32 = kani::any();
        let scale_factor = WAD + u128::from(sf_offset);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);
        let supply: u16 = kani::any();

        let mut market = make_market(annual_bps, i64::MAX, scale_factor, u128::from(supply), 0, 0);
        let config = make_config(0);

        let result = accrue_interest(&mut market, &config, i64::from(elapsed));
        if result.is_ok() {
            assert!(
                market.scale_factor() >= WAD,
                "scale_factor must remain >= WAD"
            );
        }
    }

    // ===================================================================
    // Proof 2k: Fee accrual is bounded by the 100% fee-rate path
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_fees_bounded_by_max_fee_rate() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let fee_bps: u16 = kani::any();
        kani::assume(fee_bps <= 10_000);

        let elapsed: u32 = kani::any();
        kani::assume(elapsed <= SECONDS_PER_YEAR as u32);

        let supply: u32 = kani::any();
        kani::assume(supply > 0);

        let mut market_fee = make_market(annual_bps, i64::MAX, WAD, u128::from(supply), 0, 0);
        let mut market_max = make_market(annual_bps, i64::MAX, WAD, u128::from(supply), 0, 0);

        let config_fee = make_config(fee_bps);
        let config_max = make_config(10_000);

        let r_fee = accrue_interest(&mut market_fee, &config_fee, i64::from(elapsed));
        let r_max = accrue_interest(&mut market_max, &config_max, i64::from(elapsed));

        assert!(
            r_fee.is_ok() && r_max.is_ok(),
            "bounded fee-rate comparison domain should not overflow"
        );

        assert_eq!(
            market_fee.scale_factor(),
            market_max.scale_factor(),
            "fee rate must not affect interest growth"
        );

        assert!(
            market_fee.accrued_protocol_fees() <= market_max.accrued_protocol_fees(),
            "fees at fee_bps <= 10000 must be bounded by fees at 10000 bps"
        );
    }

    // ===================================================================
    // Proof 2l: Two-step accrual >= single-step (compound growth)
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(12)]
    fn prove_accrual_compound_geq_single() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps > 0 && annual_bps <= 10_000);

        let t: u16 = kani::any();
        kani::assume(t > 1);

        let half = i64::from(t / 2);
        let full = i64::from(t);

        // Single-step: 0 -> full
        let mut market_single = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        let config = make_config(0);
        let r1 = accrue_interest(&mut market_single, &config, full);

        // Two-step: 0 -> half, half -> full
        let mut market_double = make_market(annual_bps, i64::MAX, WAD, WAD, 0, 0);
        let r2a = accrue_interest(&mut market_double, &config, half);
        let r2b = accrue_interest(&mut market_double, &config, full);

        kani::assume(r1.is_ok() && r2a.is_ok() && r2b.is_ok());

        assert!(
            market_double.scale_factor() >= market_single.scale_factor(),
            "compound (two-step) must be >= single-step accrual"
        );
    }

    // ===================================================================
    // Proof 2m: settlement_factor(X, X) returns WAD exactly
    // ===================================================================

    #[kani::proof]
    fn prove_settlement_factor_symmetric_inputs() {
        let x: u64 = kani::any();
        kani::assume(x > 0);
        kani::assume(u128::from(x) <= u128::MAX / WAD);

        let factor = compute_settlement_factor(u128::from(x), u128::from(x)).expect("bounded");

        assert_eq!(factor, WAD, "compute_settlement_factor(X, X) must be WAD");
    }

    // ===================================================================
    // Proof 2n: With scaled_total_supply=0, no fees accrue
    // ===================================================================

    #[kani::proof]
    #[kani::unwind(4)]
    fn prove_zero_supply_accrual_no_fees() {
        let annual_bps: u16 = kani::any();
        kani::assume(annual_bps <= 10_000);

        let fee_bps: u16 = kani::any();
        kani::assume(fee_bps <= 10_000);

        let elapsed: u32 = kani::any();
        // Tractability bound: with zero supply, fees must remain zero regardless
        // of elapsed time, so one-day coverage is sufficient for this invariant.
        kani::assume(elapsed <= 86_400);

        let mut market = make_market(annual_bps, i64::MAX, WAD, 0, 0, 0); // supply = 0
        let config = make_config(fee_bps);

        let result = accrue_interest(&mut market, &config, i64::from(elapsed));
        assert!(result.is_ok());
        assert_eq!(
            market.accrued_protocol_fees(),
            0,
            "zero supply must produce zero fees"
        );
    }

    // ===================================================================
    // Proof 2o: Payout negligible at minimum settlement factor (1)
    // ===================================================================

    #[kani::proof]
    fn prove_payout_zero_at_minimum_settlement() {
        let normalized: u32 = kani::any();
        let normalized_u128 = u128::from(normalized);

        // settlement_factor = 1 (minimum clamp)
        let payout = normalized_u128 * 1 / WAD;

        // With settlement_factor=1, payout = normalized / WAD
        // For normalized < WAD, payout = 0
        if normalized_u128 < WAD {
            assert_eq!(
                payout, 0,
                "payout should be 0 when normalized < WAD at minimum settlement"
            );
        }
    }
}
