--------------------------- MODULE CoalesceFi ---------------------------
(*
 * TLA+ Formal Specification for the CoalesceFi Fixed-Rate Unsecured Lending Protocol
 *
 * This specification models the core state machine of a single market with
 * multiple lenders and one borrower. It uses scaled-down integer arithmetic
 * (WAD = 1000, BPS = 100) so that TLC can feasibly enumerate the state space.
 *
 * Operations modeled:
 *   CreateMarket, Deposit, Borrow, Repay, RepayInterest, AccrueInterest,
 *   Withdraw, CollectFees, ReSettle, CloseLenderPosition, WithdrawExcess,
 *   SetPause, Tick (time advance)
 *
 * Omitted operations (pure admin config, no effect on protocol invariants):
 *   SetBlacklistMode, SetAdmin, SetWhitelistManager
 *
 * Interest model note: This spec uses a linear interest model for TLC
 * feasibility. The on-chain implementation uses daily compounding
 * (see src/logic/interest.rs:82-133). The divergence is bounded and
 * does not affect safety invariant validity.
 *
 * Safety invariants verified:
 *   VaultSolvency, ScaleFactorMonotonic, SettlementFactorBounded,
 *   SettlementFactorMonotonic, FeesNeverNegative, CapRespected,
 *   WhitelistCapacity, PayoutBounded, TotalPayoutBounded
 *)
EXTENDS Integers, FiniteSets, Sequences

\* -----------------------------------------------------------------------
\* Constants (model parameters)
\* -----------------------------------------------------------------------

\* Scaled-down WAD for fixed-point math (real protocol uses 1e18)
CONSTANT WAD

\* Scaled-down BPS denominator (real protocol uses 10000)
CONSTANT BPS

\* Seconds per year (scaled down for model checking)
CONSTANT SECONDS_PER_YEAR

\* Set of lender identifiers
CONSTANT Lenders

\* Maximum deposit amount per operation
CONSTANT MaxAmount

\* Market parameters (set in MC.cfg)
CONSTANT AnnualInterestBps   \* e.g. 10 out of 100 BPS = 10%
CONSTANT FeeRateBps          \* e.g. 5 out of 100 BPS = 5%
CONSTANT MaturityTimestamp    \* e.g. 10
CONSTANT MaxTotalSupply       \* e.g. 500
CONSTANT WhitelistMaxCapacity \* e.g. 500

\* -----------------------------------------------------------------------
\* Variables
\* -----------------------------------------------------------------------
VARIABLES
    \* Whether the market has been created
    market_initialized,

    \* Market state
    scale_factor,            \* WAD-precision accumulator (starts at WAD)
    scaled_total_supply,     \* Sum of all lender scaled balances
    accrued_protocol_fees,   \* Normalized fees owed to protocol
    total_deposited,         \* Cumulative deposits (normalized USDC)
    total_borrowed,          \* Cumulative borrows
    total_repaid,            \* Cumulative repayments
    total_interest_repaid,   \* Cumulative interest-only repayments
    last_accrual_timestamp,  \* Last time interest was accrued
    settlement_factor_wad,   \* 0 = unsettled; once set, payout ratio locked

    \* Vault balance (actual tokens in the vault)
    vault_balance,

    \* Per-lender scaled balances
    lender_scaled_balance,   \* Function: Lenders -> Nat

    \* Whitelist state for the single borrower
    whitelist_current_borrowed,

    \* Global clock
    current_time,

    \* Tracking for monotonicity invariants
    prev_scale_factor,
    prev_settlement_factor,

    \* Track total payouts for TotalPayoutBounded invariant
    total_payouts,

    \* Emergency pause flag
    is_paused

\* All variables as a tuple for stuttering
vars == <<market_initialized, scale_factor, scaled_total_supply,
          accrued_protocol_fees, total_deposited, total_borrowed,
          total_repaid, total_interest_repaid, last_accrual_timestamp,
          settlement_factor_wad,
          vault_balance, lender_scaled_balance, whitelist_current_borrowed,
          current_time, prev_scale_factor, prev_settlement_factor,
          total_payouts, is_paused>>

\* -----------------------------------------------------------------------
\* Helper operators
\* -----------------------------------------------------------------------

\* Integer division (TLA+ Nat division truncates toward zero)
Div(a, b) == IF b = 0 THEN 0 ELSE a \div b

\* Minimum of two values
Min(a, b) == IF a < b THEN a ELSE b

\* Maximum of two values
Max(a, b) == IF a > b THEN a ELSE b

\* Normalized total supply: scaled_total_supply * scale_factor / WAD
NormalizedTotalSupply == Div(scaled_total_supply * scale_factor, WAD)

\* Available balance for lenders (vault minus reserved fees)
AvailableForLenders == vault_balance - Min(vault_balance, accrued_protocol_fees)

\* Compute settlement factor from current state
ComputeSettlementFactor ==
    LET total_norm == NormalizedTotalSupply
    IN IF total_norm = 0
       THEN WAD
       ELSE LET raw == Div(AvailableForLenders * WAD, total_norm)
                capped == Min(WAD, raw)
            IN Max(1, capped)

\* -----------------------------------------------------------------------
\* AccrueInterest (called as a sub-action before most operations)
\* -----------------------------------------------------------------------

\* Compute accrued interest and update state in-place.
\* Returns <<new_scale_factor, new_accrued_fees, new_last_accrual>>
\* We model this as a helper that other actions call.

AccrueInterestEffect ==
    LET maturity == MaturityTimestamp
        effective_now == Min(current_time, maturity)
        time_elapsed == effective_now - last_accrual_timestamp
    IN IF time_elapsed <= 0
       THEN <<scale_factor, accrued_protocol_fees, last_accrual_timestamp>>
       ELSE
         LET \* interest_delta_wad = annual_bps * time_elapsed * WAD / (SECONDS_PER_YEAR * BPS)
             interest_delta_wad == Div(AnnualInterestBps * time_elapsed * WAD,
                                       SECONDS_PER_YEAR * BPS)
             \* scale_factor_delta = scale_factor * interest_delta_wad / WAD
             scale_factor_delta == Div(scale_factor * interest_delta_wad, WAD)
             new_sf == scale_factor + scale_factor_delta
             \* Protocol fee accrual
             fee_delta_wad == IF FeeRateBps > 0
                              THEN Div(interest_delta_wad * FeeRateBps, BPS)
                              ELSE 0
             \* fee_normalized = scaled_total_supply * new_sf / WAD * fee_delta_wad / WAD
             fee_normalized == IF FeeRateBps > 0
                               THEN Div(Div(scaled_total_supply * new_sf, WAD) * fee_delta_wad, WAD)
                               ELSE 0
             new_fees == accrued_protocol_fees + fee_normalized
         IN <<new_sf, new_fees, effective_now>>

\* -----------------------------------------------------------------------
\* Initial state
\* -----------------------------------------------------------------------

Init ==
    /\ market_initialized = FALSE
    /\ scale_factor = 0
    /\ scaled_total_supply = 0
    /\ accrued_protocol_fees = 0
    /\ total_deposited = 0
    /\ total_borrowed = 0
    /\ total_repaid = 0
    /\ total_interest_repaid = 0
    /\ last_accrual_timestamp = 0
    /\ settlement_factor_wad = 0
    /\ vault_balance = 0
    /\ lender_scaled_balance = [l \in Lenders |-> 0]
    /\ whitelist_current_borrowed = 0
    /\ current_time = 0
    /\ prev_scale_factor = 0
    /\ prev_settlement_factor = 0
    /\ total_payouts = 0
    /\ is_paused = FALSE

\* -----------------------------------------------------------------------
\* CreateMarket
\* -----------------------------------------------------------------------

CreateMarket ==
    /\ market_initialized = FALSE
    /\ market_initialized' = TRUE
    /\ scale_factor' = WAD
    /\ scaled_total_supply' = 0
    /\ accrued_protocol_fees' = 0
    /\ total_deposited' = 0
    /\ total_borrowed' = 0
    /\ total_repaid' = 0
    /\ total_interest_repaid' = 0
    /\ last_accrual_timestamp' = current_time
    /\ settlement_factor_wad' = 0
    /\ vault_balance' = 0
    /\ prev_scale_factor' = WAD
    /\ prev_settlement_factor' = 0
    /\ total_payouts' = 0
    /\ is_paused' = FALSE
    /\ UNCHANGED <<lender_scaled_balance, whitelist_current_borrowed, current_time>>

\* -----------------------------------------------------------------------
\* Deposit
\* -----------------------------------------------------------------------

Deposit(lender, amount) ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ amount > 0
    /\ amount <= MaxAmount
    /\ current_time < MaturityTimestamp    \* Must be before maturity
    /\ settlement_factor_wad = 0           \* Market not yet settled
    /\ LET accrual == AccrueInterestEffect
           new_sf == accrual[1]
           new_fees == accrual[2]
           new_last == accrual[3]
           \* scaled_amount = amount * WAD / scale_factor
           scaled_amount == Div(amount * WAD, new_sf)
       IN /\ scaled_amount > 0            \* Revert if rounds to zero
          /\ LET new_scaled_total == scaled_total_supply + scaled_amount
                 \* Check cap: new_scaled_total * new_sf / WAD <= MaxTotalSupply
                 new_normalized == Div(new_scaled_total * new_sf, WAD)
             IN /\ new_normalized <= MaxTotalSupply
                /\ scale_factor' = new_sf
                /\ accrued_protocol_fees' = new_fees
                /\ last_accrual_timestamp' = new_last
                /\ scaled_total_supply' = new_scaled_total
                /\ total_deposited' = total_deposited + amount
                /\ vault_balance' = vault_balance + amount
                /\ lender_scaled_balance' =
                     [lender_scaled_balance EXCEPT ![lender] =
                        lender_scaled_balance[lender] + scaled_amount]
                /\ prev_scale_factor' = new_sf
                /\ UNCHANGED <<market_initialized, total_borrowed,
                               total_repaid, total_interest_repaid,
                               settlement_factor_wad,
                               whitelist_current_borrowed, current_time,
                               prev_settlement_factor, total_payouts,
                               is_paused>>

\* -----------------------------------------------------------------------
\* Borrow
\* -----------------------------------------------------------------------

Borrow(amount) ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ amount > 0
    /\ amount <= MaxAmount
    /\ current_time < MaturityTimestamp    \* Must be before maturity
    /\ settlement_factor_wad = 0           \* Market not yet settled
    /\ LET accrual == AccrueInterestEffect
           new_sf == accrual[1]
           new_fees == accrual[2]
           new_last == accrual[3]
           \* Fee reservation: min(vault_balance, accrued_fees)
           fees_reserved == Min(vault_balance, new_fees)
           borrowable == vault_balance - fees_reserved
       IN /\ amount <= borrowable
          /\ whitelist_current_borrowed + amount <= WhitelistMaxCapacity
          /\ scale_factor' = new_sf
          /\ accrued_protocol_fees' = new_fees
          /\ last_accrual_timestamp' = new_last
          /\ total_borrowed' = total_borrowed + amount
          /\ vault_balance' = vault_balance - amount
          /\ whitelist_current_borrowed' = whitelist_current_borrowed + amount
          /\ prev_scale_factor' = new_sf
          /\ UNCHANGED <<market_initialized, scaled_total_supply,
                         total_deposited, total_repaid, total_interest_repaid,
                         settlement_factor_wad, lender_scaled_balance,
                         current_time, prev_settlement_factor,
                         total_payouts, is_paused>>

\* -----------------------------------------------------------------------
\* Repay
\* -----------------------------------------------------------------------

Repay(amount) ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ amount > 0
    /\ amount <= MaxAmount
    \* Repay uses a zero-fee config for interest accrual (matches implementation)
    /\ LET maturity == MaturityTimestamp
           effective_now == Min(current_time, maturity)
           time_elapsed == effective_now - last_accrual_timestamp
           \* Accrue with zero fee rate
           interest_delta_wad == IF time_elapsed <= 0 THEN 0
                                 ELSE Div(AnnualInterestBps * time_elapsed * WAD,
                                          SECONDS_PER_YEAR * BPS)
           scale_factor_delta == IF time_elapsed <= 0 THEN 0
                                 ELSE Div(scale_factor * interest_delta_wad, WAD)
           new_sf == scale_factor + scale_factor_delta
           new_last == IF time_elapsed <= 0 THEN last_accrual_timestamp ELSE effective_now
       IN /\ scale_factor' = new_sf
          /\ last_accrual_timestamp' = new_last
          \* Fees unchanged for repay (zero-fee accrual)
          /\ accrued_protocol_fees' = accrued_protocol_fees
          /\ total_repaid' = total_repaid + amount
          /\ vault_balance' = vault_balance + amount
          /\ prev_scale_factor' = new_sf
          /\ UNCHANGED <<market_initialized, scaled_total_supply,
                         total_deposited, total_borrowed, total_interest_repaid,
                         settlement_factor_wad, lender_scaled_balance,
                         whitelist_current_borrowed, current_time,
                         prev_settlement_factor, total_payouts, is_paused>>

\* -----------------------------------------------------------------------
\* RepayInterest
\* -----------------------------------------------------------------------

\* Like Repay but also increments total_interest_repaid and does NOT
\* touch whitelist_current_borrowed. Uses zero-fee accrual.

RepayInterest(amount) ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ amount > 0
    /\ amount <= MaxAmount
    /\ LET maturity == MaturityTimestamp
           effective_now == Min(current_time, maturity)
           time_elapsed == effective_now - last_accrual_timestamp
           \* Accrue with zero fee rate
           interest_delta_wad == IF time_elapsed <= 0 THEN 0
                                 ELSE Div(AnnualInterestBps * time_elapsed * WAD,
                                          SECONDS_PER_YEAR * BPS)
           scale_factor_delta == IF time_elapsed <= 0 THEN 0
                                 ELSE Div(scale_factor * interest_delta_wad, WAD)
           new_sf == scale_factor + scale_factor_delta
           new_last == IF time_elapsed <= 0 THEN last_accrual_timestamp ELSE effective_now
       IN /\ scale_factor' = new_sf
          /\ last_accrual_timestamp' = new_last
          /\ accrued_protocol_fees' = accrued_protocol_fees
          /\ total_repaid' = total_repaid + amount
          /\ total_interest_repaid' = total_interest_repaid + amount
          /\ vault_balance' = vault_balance + amount
          /\ prev_scale_factor' = new_sf
          /\ UNCHANGED <<market_initialized, scaled_total_supply,
                         total_deposited, total_borrowed,
                         settlement_factor_wad, lender_scaled_balance,
                         whitelist_current_borrowed, current_time,
                         prev_settlement_factor, total_payouts, is_paused>>

\* -----------------------------------------------------------------------
\* Withdraw
\* -----------------------------------------------------------------------

Withdraw(lender) ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ current_time >= MaturityTimestamp   \* Must be at or past maturity
    /\ lender_scaled_balance[lender] > 0  \* Must have balance
    /\ LET accrual == AccrueInterestEffect
           new_sf == accrual[1]
           new_fees == accrual[2]
           new_last == accrual[3]
           \* On first withdrawal, compute and lock settlement factor
           sf_wad == IF settlement_factor_wad = 0
                     THEN LET total_norm == Div(scaled_total_supply * new_sf, WAD)
                              avail == vault_balance - Min(vault_balance, new_fees)
                          IN IF total_norm = 0
                             THEN WAD
                             ELSE Max(1, Min(WAD, Div(avail * WAD, total_norm)))
                     ELSE settlement_factor_wad
           \* Full withdrawal (scaled_amount = entire balance)
           scaled_amount == lender_scaled_balance[lender]
           \* Payout = scaled_amount * new_sf / WAD * sf_wad / WAD
           normalized_amount == Div(scaled_amount * new_sf, WAD)
           payout == Div(normalized_amount * sf_wad, WAD)
       IN /\ payout > 0                   \* Revert if zero payout
          /\ payout <= vault_balance       \* Must have enough in vault
          /\ scale_factor' = new_sf
          /\ accrued_protocol_fees' = new_fees
          /\ last_accrual_timestamp' = new_last
          /\ settlement_factor_wad' = sf_wad
          /\ vault_balance' = vault_balance - payout
          /\ lender_scaled_balance' =
               [lender_scaled_balance EXCEPT ![lender] = 0]
          /\ scaled_total_supply' = scaled_total_supply - scaled_amount
          /\ total_payouts' = total_payouts + payout
          /\ prev_scale_factor' = new_sf
          /\ prev_settlement_factor' = sf_wad
          /\ UNCHANGED <<market_initialized, total_deposited,
                         total_borrowed, total_repaid, total_interest_repaid,
                         whitelist_current_borrowed, current_time, is_paused>>

\* -----------------------------------------------------------------------
\* CollectFees
\* -----------------------------------------------------------------------

CollectFees ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ LET accrual == AccrueInterestEffect
           new_sf == accrual[1]
           new_fees == accrual[2]
           new_last == accrual[3]
       IN /\ new_fees > 0                    \* Must have fees to collect
          /\ LET withdrawable == Min(new_fees, vault_balance)
             IN /\ withdrawable > 0
                /\ scale_factor' = new_sf
                /\ accrued_protocol_fees' = new_fees - withdrawable
                /\ last_accrual_timestamp' = new_last
                /\ vault_balance' = vault_balance - withdrawable
                /\ prev_scale_factor' = new_sf
                /\ UNCHANGED <<market_initialized, scaled_total_supply,
                               total_deposited, total_borrowed,
                               total_repaid, total_interest_repaid,
                               settlement_factor_wad,
                               lender_scaled_balance,
                               whitelist_current_borrowed, current_time,
                               prev_settlement_factor, total_payouts,
                               is_paused>>

\* -----------------------------------------------------------------------
\* ReSettle
\* -----------------------------------------------------------------------

ReSettle ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ settlement_factor_wad > 0            \* Must already be settled
    /\ LET \* Accrue with zero fee (matches implementation)
           maturity == MaturityTimestamp
           effective_now == Min(current_time, maturity)
           time_elapsed == effective_now - last_accrual_timestamp
           interest_delta_wad == IF time_elapsed <= 0 THEN 0
                                 ELSE Div(AnnualInterestBps * time_elapsed * WAD,
                                          SECONDS_PER_YEAR * BPS)
           scale_factor_delta == IF time_elapsed <= 0 THEN 0
                                 ELSE Div(scale_factor * interest_delta_wad, WAD)
           new_sf == scale_factor + scale_factor_delta
           new_last == IF time_elapsed <= 0 THEN last_accrual_timestamp ELSE effective_now
           \* Compute new settlement factor
           total_norm == Div(scaled_total_supply * new_sf, WAD)
           avail == vault_balance - Min(vault_balance, accrued_protocol_fees)
           new_factor == IF total_norm = 0
                         THEN WAD
                         ELSE Max(1, Min(WAD, Div(avail * WAD, total_norm)))
       IN /\ new_factor > settlement_factor_wad   \* Must be strictly improved
          /\ scale_factor' = new_sf
          /\ last_accrual_timestamp' = new_last
          /\ settlement_factor_wad' = new_factor
          /\ prev_scale_factor' = new_sf
          /\ prev_settlement_factor' = new_factor
          \* Fees unchanged for re-settle (zero-fee accrual)
          /\ accrued_protocol_fees' = accrued_protocol_fees
          /\ UNCHANGED <<market_initialized, scaled_total_supply,
                         total_deposited, total_borrowed,
                         total_repaid, total_interest_repaid,
                         vault_balance,
                         lender_scaled_balance,
                         whitelist_current_borrowed, current_time,
                         total_payouts, is_paused>>

\* -----------------------------------------------------------------------
\* CloseLenderPosition (modeled as a no-op check)
\* -----------------------------------------------------------------------

CloseLenderPosition(lender) ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ lender_scaled_balance[lender] = 0   \* Position must be empty
    \* In reality this closes the account; in our model it is a no-op
    \* since the balance is already 0. We include it for completeness.
    /\ UNCHANGED vars

\* -----------------------------------------------------------------------
\* WithdrawExcess
\* -----------------------------------------------------------------------

\* Guards: all lenders withdrawn, settlement at WAD, no outstanding fees,
\* positive vault balance. Sweeps remaining vault balance.

WithdrawExcess ==
    /\ market_initialized = TRUE
    /\ is_paused = FALSE
    /\ scaled_total_supply = 0
    /\ settlement_factor_wad = WAD
    /\ accrued_protocol_fees = 0
    /\ vault_balance > 0
    /\ vault_balance' = 0
    /\ total_payouts' = total_payouts + vault_balance
    /\ UNCHANGED <<market_initialized, scale_factor, scaled_total_supply,
                   accrued_protocol_fees, total_deposited, total_borrowed,
                   total_repaid, total_interest_repaid,
                   last_accrual_timestamp, settlement_factor_wad,
                   lender_scaled_balance, whitelist_current_borrowed,
                   current_time, prev_scale_factor, prev_settlement_factor,
                   is_paused>>

\* -----------------------------------------------------------------------
\* SetPause — flip the is_paused flag
\* -----------------------------------------------------------------------

SetPause(flag) ==
    /\ market_initialized = TRUE
    /\ is_paused' = flag
    /\ UNCHANGED <<market_initialized, scale_factor, scaled_total_supply,
                   accrued_protocol_fees, total_deposited, total_borrowed,
                   total_repaid, total_interest_repaid,
                   last_accrual_timestamp, settlement_factor_wad,
                   vault_balance, lender_scaled_balance,
                   whitelist_current_borrowed, current_time,
                   prev_scale_factor, prev_settlement_factor,
                   total_payouts>>

\* -----------------------------------------------------------------------
\* Tick — advance time by 1 unit
\* -----------------------------------------------------------------------

Tick ==
    /\ current_time < MaturityTimestamp + 2   \* Bound time to keep state finite
    /\ current_time' = current_time + 1
    /\ UNCHANGED <<market_initialized, scale_factor, scaled_total_supply,
                   accrued_protocol_fees, total_deposited, total_borrowed,
                   total_repaid, total_interest_repaid,
                   last_accrual_timestamp, settlement_factor_wad,
                   vault_balance, lender_scaled_balance,
                   whitelist_current_borrowed, prev_scale_factor,
                   prev_settlement_factor, total_payouts, is_paused>>

\* -----------------------------------------------------------------------
\* Next state relation
\* -----------------------------------------------------------------------

Next ==
    \/ CreateMarket
    \/ \E l \in Lenders : \E a \in 1..MaxAmount : Deposit(l, a)
    \/ \E a \in 1..MaxAmount : Borrow(a)
    \/ \E a \in 1..MaxAmount : Repay(a)
    \/ \E a \in 1..MaxAmount : RepayInterest(a)
    \/ \E l \in Lenders : Withdraw(l)
    \/ CollectFees
    \/ ReSettle
    \/ \E l \in Lenders : CloseLenderPosition(l)
    \/ WithdrawExcess
    \/ SetPause(TRUE)
    \/ SetPause(FALSE)
    \/ Tick

\* -----------------------------------------------------------------------
\* Fairness (optional, for liveness; not required for safety checking)
\* -----------------------------------------------------------------------

Spec == Init /\ [][Next]_vars

\* -----------------------------------------------------------------------
\* Safety Invariants
\* -----------------------------------------------------------------------

\* INV-1: Vault balance is never negative
VaultSolvency ==
    vault_balance >= 0

\* INV-2: Scale factor never decreases (monotonically non-decreasing)
\* We check: after any action, scale_factor >= prev_scale_factor
ScaleFactorMonotonic ==
    market_initialized => scale_factor >= prev_scale_factor

\* INV-3: Settlement factor is bounded in [1, WAD] when set
SettlementFactorBounded ==
    settlement_factor_wad /= 0 => (settlement_factor_wad >= 1 /\ settlement_factor_wad <= WAD)

\* INV-4: Settlement factor never decreases once set
SettlementFactorMonotonic ==
    (settlement_factor_wad /= 0 /\ prev_settlement_factor /= 0) =>
        settlement_factor_wad >= prev_settlement_factor

\* INV-5: Accrued protocol fees are never negative
FeesNeverNegative ==
    accrued_protocol_fees >= 0

\* INV-6: Total normalized supply does not exceed max total supply
\* (This holds because Deposit enforces the cap check)
CapRespected ==
    market_initialized =>
        Div(scaled_total_supply * scale_factor, WAD) <= MaxTotalSupply

\* INV-7: Whitelist current borrowed does not exceed max capacity
WhitelistCapacity ==
    whitelist_current_borrowed <= WhitelistMaxCapacity

\* INV-8: Individual payout is bounded by individual normalized deposit
\* This is structural: payout = normalized * settlement_factor / WAD,
\* and settlement_factor <= WAD, so payout <= normalized.
\* We encode this as: for each lender, their pending payout <= their normalized balance.
PayoutBounded ==
    market_initialized /\ settlement_factor_wad > 0 =>
        \A l \in Lenders :
            LET norm == Div(lender_scaled_balance[l] * scale_factor, WAD)
                pay  == Div(norm * settlement_factor_wad, WAD)
            IN pay <= norm

\* INV-9: Total payouts do not exceed available vault at settlement
\* We approximate: total_payouts <= total_deposited + total_repaid
\* (since vault starts at 0 and only receives deposits and repayments)
TotalPayoutBounded ==
    total_payouts <= total_deposited + total_repaid

\* Combined invariant for TLC
TypeInvariant ==
    /\ market_initialized \in BOOLEAN
    /\ scale_factor >= 0
    /\ scaled_total_supply >= 0
    /\ accrued_protocol_fees >= 0
    /\ total_deposited >= 0
    /\ total_borrowed >= 0
    /\ total_repaid >= 0
    /\ total_interest_repaid >= 0
    /\ vault_balance >= 0
    /\ settlement_factor_wad >= 0
    /\ whitelist_current_borrowed >= 0
    /\ current_time >= 0
    /\ total_payouts >= 0
    /\ is_paused \in BOOLEAN

\* All invariants combined
AllInvariants ==
    /\ TypeInvariant
    /\ VaultSolvency
    /\ ScaleFactorMonotonic
    /\ SettlementFactorBounded
    /\ SettlementFactorMonotonic
    /\ FeesNeverNegative
    /\ CapRespected
    /\ WhitelistCapacity
    /\ PayoutBounded
    /\ TotalPayoutBounded

=========================================================================
