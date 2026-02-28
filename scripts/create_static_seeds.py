#!/usr/bin/env python3
"""
create_static_seeds.py -- Generate hand-crafted binary seed files for fuzz targets.

Run this once to populate fuzz/seeds/ with binary files:
    python3 scripts/create_static_seeds.py

Each seed is a binary blob matching the Arbitrary-derived Input struct layout
for its fuzz target. Fields are serialized as little-endian byte sequences.
"""

import os
import struct
import sys

# ---------------------------------------------------------------------------
# Constants matching src/constants.rs
# ---------------------------------------------------------------------------

WAD = 1_000_000_000_000_000_000  # 1e18
BPS = 10_000
SECONDS_PER_YEAR = 31_536_000
U64_MAX = (1 << 64) - 1
U128_MAX = (1 << 128) - 1

# ---------------------------------------------------------------------------
# Packing helpers
# ---------------------------------------------------------------------------


def pack_u16(v):
    return struct.pack("<H", v & 0xFFFF)


def pack_u32(v):
    return struct.pack("<I", v & 0xFFFFFFFF)


def pack_u64(v):
    return struct.pack("<Q", v & 0xFFFFFFFFFFFFFFFF)


def pack_i64(v):
    return struct.pack("<q", v)


def u128_hi_lo(val):
    """Split a u128 value into (hi_u64, lo_u64)."""
    hi = (val >> 64) & U64_MAX
    lo = val & U64_MAX
    return hi, lo


def write_seed(base_dir, name, data):
    path = os.path.join(base_dir, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    print(f"  wrote {path} ({len(data)} bytes)")


# ---------------------------------------------------------------------------
# Pre-computed constants
# ---------------------------------------------------------------------------

wad_hi, wad_lo = u128_hi_lo(WAD)
near_max_hi, near_max_lo = u128_hi_lo(U128_MAX // 2)
near_max_wad_hi, near_max_wad_lo = u128_hi_lo(U128_MAX // WAD)


def main():
    script_dir = os.path.dirname(os.path.abspath(__file__))
    project_root = os.path.dirname(script_dir)
    seeds_root = os.path.join(project_root, "fuzz", "seeds")

    print(f"Generating seed files in {seeds_root}")

    # ==================================================================
    # interest_accrual seeds
    # ==================================================================
    # Input: annual_bps(u16), maturity_timestamp(i64), scale_factor_hi(u64),
    #   scale_factor_lo(u64), scaled_total_supply_hi(u64),
    #   scaled_total_supply_lo(u64), last_accrual_timestamp(i64),
    #   accrued_protocol_fees(u64), fee_rate_bps(u16), current_timestamp(i64)
    base = os.path.join(seeds_root, "interest_accrual")

    def ia(annual_bps, maturity_ts, sf_hi, sf_lo, supply_hi, supply_lo,
           last_accrual, fees, fee_bps, current_ts):
        return (
            pack_u16(annual_bps) + pack_i64(maturity_ts) +
            pack_u64(sf_hi) + pack_u64(sf_lo) +
            pack_u64(supply_hi) + pack_u64(supply_lo) +
            pack_i64(last_accrual) + pack_u64(fees) +
            pack_u16(fee_bps) + pack_i64(current_ts)
        )

    print("\n[interest_accrual]")

    # Seed 1: Zero rate -- annual_bps=0, no interest accrued
    write_seed(base, "seed_zero_rate",
               ia(0, 2_000_000_000, 0, wad_lo, 0, 1_000_000, 100, 0, 0, 200))

    # Seed 2: Max rate (10000 bps = 100%) over full year, 50% fee
    write_seed(base, "seed_max_rate_full_year",
               ia(10000, 2_000_000_000, 0, wad_lo, 0, 1_000_000_000,
                  0, 0, 5000, SECONDS_PER_YEAR))

    # Seed 3: Zero time elapsed -- current_ts == last_accrual
    write_seed(base, "seed_zero_time",
               ia(1000, 2_000_000_000, 0, wad_lo, 0, 1_000_000, 500, 0, 500, 500))

    # Seed 4: Far past maturity -- tests effective_now capping
    write_seed(base, "seed_past_maturity",
               ia(1000, 1000, 0, wad_lo, 0, 1_000_000, 0, 0, 500, 2_000_000_000))

    # Seed 5: Exact maturity boundary
    write_seed(base, "seed_maturity_exact",
               ia(1000, 100_000, 0, wad_lo, 0, 1_000_000, 0, 0, 500, 100_000))

    # Seed 6: Near-overflow scale_factor (u128::MAX / 2)
    write_seed(base, "seed_near_overflow_sf",
               ia(10000, 2_000_000_000, near_max_hi, near_max_lo,
                  0, 1, 0, 0, 0, SECONDS_PER_YEAR))

    # Seed 7: Max accrued fees (u64::MAX), tests fee checked_add overflow
    write_seed(base, "seed_max_fees_overflow",
               ia(1000, 2_000_000_000, 0, wad_lo, 0, 1_000_000_000_000,
                  0, U64_MAX, 10000, SECONDS_PER_YEAR))

    # Seed 8: Negative time delta (current < last_accrual)
    write_seed(base, "seed_negative_delta",
               ia(1000, 2_000_000_000, 0, wad_lo, 0, 1_000_000, 1000, 0, 500, 500))

    # ==================================================================
    # deposit_scaling seeds
    # ==================================================================
    # Input: amount(u64), sf_offset(u64)
    base = os.path.join(seeds_root, "deposit_scaling")

    def ds(amount, sf_offset):
        return pack_u64(amount) + pack_u64(sf_offset)

    print("\n[deposit_scaling]")

    # Seed 1: Zero amount -- triggers early return
    write_seed(base, "seed_zero_amount", ds(0, 0))

    # Seed 2: Max amount (u64::MAX)
    write_seed(base, "seed_max_amount", ds(U64_MAX, 0))

    # Seed 3: WAD as sf_offset (scale_factor = 2 * WAD)
    write_seed(base, "seed_wad_sf", ds(1_000_000, WAD & U64_MAX))

    # Seed 4: Max sf_offset (clamped to WAD internally)
    write_seed(base, "seed_max_sf", ds(1_000_000, U64_MAX))

    # Seed 5: Amount = 1 at base scale (sf = WAD)
    write_seed(base, "seed_min_amount_base", ds(1, 0))

    # Seed 6: Amount = 1 with max sf -- scaled may round to zero
    write_seed(base, "seed_min_amount_max_sf", ds(1, U64_MAX))

    # ==================================================================
    # settlement_factor seeds
    # ==================================================================
    # Input: vault_balance(u64), accrued_fees(u64),
    #   scaled_total_supply_hi(u64), scaled_total_supply_lo(u64),
    #   scale_factor_hi(u64), scale_factor_lo(u64)
    base = os.path.join(seeds_root, "settlement_factor")

    def sf(vault_bal, fees, supply_hi, supply_lo, sf_hi, sf_lo):
        return (
            pack_u64(vault_bal) + pack_u64(fees) +
            pack_u64(supply_hi) + pack_u64(supply_lo) +
            pack_u64(sf_hi) + pack_u64(sf_lo)
        )

    print("\n[settlement_factor]")

    # Seed 1: Zero available (vault == fees)
    write_seed(base, "seed_zero_available",
               sf(1_000_000, 1_000_000, 0, 1_000_000, 0, wad_lo))

    # Seed 2: Max available (max vault, zero fees)
    write_seed(base, "seed_max_available",
               sf(U64_MAX, 0, 0, 1_000_000, 0, wad_lo))

    # Seed 3: Zero supply (triggers guard)
    write_seed(base, "seed_zero_supply",
               sf(1_000_000, 0, 0, 0, 0, wad_lo))

    # Seed 4: Equal amounts -- settlement factor = WAD
    write_seed(base, "seed_equal_amounts",
               sf(1_000_000, 0, 0, 1_000_000, 0, wad_lo))

    # Seed 5: Vault less than fees
    write_seed(base, "seed_vault_lt_fees",
               sf(100, 1_000_000, 0, 1_000_000, 0, wad_lo))

    # Seed 6: Tiny vault with large supply (factor near 1)
    write_seed(base, "seed_tiny_vault",
               sf(1, 0, 0, U64_MAX, 0, wad_lo))

    # ==================================================================
    # fee_computation seeds
    # ==================================================================
    # Input: annual_bps(u16), fee_rate_bps(u16), time_elapsed(u32), supply(u64)
    base = os.path.join(seeds_root, "fee_computation")

    def fc(annual_bps, fee_bps, time_elapsed, supply):
        return (
            pack_u16(annual_bps) + pack_u16(fee_bps) +
            pack_u32(time_elapsed) + pack_u64(supply)
        )

    print("\n[fee_computation]")

    # Seed 1: Zero fee rate -- no fees accrued
    write_seed(base, "seed_zero_fee",
               fc(1000, 0, SECONDS_PER_YEAR, 1_000_000))

    # Seed 2: Max fee rate (100%)
    write_seed(base, "seed_max_fee",
               fc(1000, 10000, SECONDS_PER_YEAR, 1_000_000))

    # Seed 3: Zero supply (early return)
    write_seed(base, "seed_zero_supply",
               fc(1000, 500, SECONDS_PER_YEAR, 0))

    # Seed 4: Max supply (u64::MAX) -- overflow in fee calc
    write_seed(base, "seed_max_supply",
               fc(1000, 500, SECONDS_PER_YEAR, U64_MAX))

    # Seed 5: Zero time elapsed
    write_seed(base, "seed_zero_time",
               fc(1000, 500, 0, 1_000_000))

    # Seed 6: Max everything -- overflow cascade
    write_seed(base, "seed_max_all",
               fc(10000, 10000, SECONDS_PER_YEAR, U64_MAX))

    # Seed 7: 1 bps fee (precision test)
    write_seed(base, "seed_one_bps_fee",
               fc(5000, 1, SECONDS_PER_YEAR, 1_000_000_000))

    print(f"\nDone. All seed files created in {seeds_root}")


if __name__ == "__main__":
    main()
