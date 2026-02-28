#!/usr/bin/env python3
"""
generate_fuzz_seeds.py -- Coverage-guided fuzz seed generator for CoalesceFi.

Reads an LCOV file, identifies uncovered lines in src/logic/ and src/processor/,
then generates targeted binary seed files for each fuzz target's corpus.

Usage:
    python3 scripts/generate_fuzz_seeds.py --lcov coverage.lcov --output-dir fuzz/corpus

The script maps uncovered code regions to specific fuzz input patterns that are
likely to exercise those paths: boundary values, overflow-prone inputs, zero
amounts, maturity edge cases, and fee extremes.
"""

import argparse
import os
import struct
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Set, Tuple


# ---------------------------------------------------------------------------
# LCOV parser
# ---------------------------------------------------------------------------

@dataclass
class FileCoverage:
    """Coverage data for a single source file."""
    path: str
    uncovered_lines: Set[int] = field(default_factory=set)
    covered_lines: Set[int] = field(default_factory=set)
    uncovered_branches: List[Tuple[int, int, int]] = field(default_factory=list)
    # (line, block, branch) tuples where hit count == 0


def parse_lcov(lcov_path: str) -> Dict[str, FileCoverage]:
    """Parse an LCOV file and return per-file coverage data."""
    files: Dict[str, FileCoverage] = {}
    current_file = None

    with open(lcov_path, "r") as f:
        for raw_line in f:
            line = raw_line.strip()

            if line.startswith("SF:"):
                path = line[3:]
                current_file = FileCoverage(path=path)
                files[path] = current_file

            elif line.startswith("DA:") and current_file is not None:
                parts = line[3:].split(",")
                if len(parts) >= 2:
                    lineno = int(parts[0])
                    hits = int(parts[1])
                    if hits == 0:
                        current_file.uncovered_lines.add(lineno)
                    else:
                        current_file.covered_lines.add(lineno)

            elif line.startswith("BRDA:") and current_file is not None:
                parts = line[5:].split(",")
                if len(parts) >= 4:
                    lineno = int(parts[0])
                    block = int(parts[1])
                    branch = int(parts[2])
                    hit_str = parts[3]
                    hits = 0 if hit_str == "-" else int(hit_str)
                    if hits == 0:
                        current_file.uncovered_branches.append(
                            (lineno, block, branch)
                        )

            elif line == "end_of_record":
                current_file = None

    return files


# ---------------------------------------------------------------------------
# Code-path analysis: map uncovered lines to seed strategies
# ---------------------------------------------------------------------------

# Keywords/patterns that hint at which code paths need exercising
PATH_HINTS = {
    "ZeroScaledAmount": "zero_scaled",
    "ZeroAmount": "zero_amount",
    "MathOverflow": "overflow",
    "checked_mul": "overflow",
    "checked_add": "overflow",
    "checked_sub": "underflow",
    "checked_div": "division",
    "maturity": "maturity_boundary",
    "scale_factor": "scale_factor_edge",
    "fee_rate": "fee_edge",
    "CapExceeded": "cap_exceeded",
    "InsufficientBalance": "insufficient_balance",
    "BorrowAmountTooHigh": "borrow_high",
    "NotMatured": "not_matured",
    "MarketMatured": "market_matured",
    "NoBalance": "no_balance",
    "NoFeesToCollect": "no_fees",
    "SettlementNotImproved": "settlement_edge",
    "NotSettled": "not_settled",
    "WAD": "wad_boundary",
    "saturating_sub": "saturation",
}


def classify_uncovered(
    files: Dict[str, FileCoverage],
    source_root: str,
) -> Set[str]:
    """
    Read source files and classify uncovered lines into seed strategy tags.
    Returns a set of strategy tags that should be exercised.
    """
    strategies: Set[str] = set()

    for fpath, cov in files.items():
        # Only examine logic/ and processor/ files
        if "/logic/" not in fpath and "/processor/" not in fpath:
            continue

        if not cov.uncovered_lines and not cov.uncovered_branches:
            continue

        # Try to read the source file to inspect uncovered line content
        full_path = fpath
        if not os.path.isabs(full_path):
            full_path = os.path.join(source_root, full_path)

        try:
            with open(full_path, "r") as sf:
                source_lines = sf.readlines()
        except FileNotFoundError:
            # File not found at path -- try stripping prefix
            alt = os.path.join(source_root, os.path.basename(fpath))
            try:
                with open(alt, "r") as sf:
                    source_lines = sf.readlines()
            except FileNotFoundError:
                # Cannot read source; add generic strategies
                strategies.add("overflow")
                strategies.add("zero_amount")
                continue

        for lineno in cov.uncovered_lines:
            if lineno <= len(source_lines):
                content = source_lines[lineno - 1]
                for keyword, strategy in PATH_HINTS.items():
                    if keyword in content:
                        strategies.add(strategy)

        # Uncovered branches also contribute
        for lineno, _, _ in cov.uncovered_branches:
            if lineno <= len(source_lines):
                content = source_lines[lineno - 1]
                for keyword, strategy in PATH_HINTS.items():
                    if keyword in content:
                        strategies.add(strategy)

    # Always include baseline strategies
    strategies.update(["zero_amount", "overflow", "maturity_boundary", "fee_edge"])
    return strategies


# ---------------------------------------------------------------------------
# Seed generators per fuzz target
# ---------------------------------------------------------------------------

WAD = 1_000_000_000_000_000_000  # 1e18
BPS = 10_000
SECONDS_PER_YEAR = 31_536_000
U64_MAX = (1 << 64) - 1
U128_MAX = (1 << 128) - 1


def pack_u16(v: int) -> bytes:
    """Pack u16 little-endian (Arbitrary derives use LE byte sequences)."""
    return struct.pack("<H", v & 0xFFFF)


def pack_u32(v: int) -> bytes:
    return struct.pack("<I", v & 0xFFFFFFFF)


def pack_u64(v: int) -> bytes:
    return struct.pack("<Q", v & 0xFFFFFFFFFFFFFFFF)


def pack_i64(v: int) -> bytes:
    return struct.pack("<q", v)


def u128_to_hi_lo(val: int) -> Tuple[int, int]:
    """Split a u128 into (hi_u64, lo_u64)."""
    hi = (val >> 64) & U64_MAX
    lo = val & U64_MAX
    return hi, lo


def generate_interest_accrual_seeds(strategies: Set[str]) -> List[Tuple[str, bytes]]:
    """
    Generate seeds for fuzz_interest_accrual.
    Input struct (Arbitrary-derived byte sequence):
        annual_bps: u16, maturity_timestamp: i64, scale_factor_hi: u64,
        scale_factor_lo: u64, scaled_total_supply_hi: u64,
        scaled_total_supply_lo: u64, last_accrual_timestamp: i64,
        accrued_protocol_fees: u64, fee_rate_bps: u16, current_timestamp: i64
    """
    seeds = []

    def pack_input(
        annual_bps, maturity_ts, sf_hi, sf_lo,
        supply_hi, supply_lo, last_accrual, fees, fee_bps, current_ts
    ):
        return (
            pack_u16(annual_bps) + pack_i64(maturity_ts) +
            pack_u64(sf_hi) + pack_u64(sf_lo) +
            pack_u64(supply_hi) + pack_u64(supply_lo) +
            pack_i64(last_accrual) + pack_u64(fees) +
            pack_u16(fee_bps) + pack_i64(current_ts)
        )

    wad_hi, wad_lo = u128_to_hi_lo(WAD)

    # Seed 1: Zero rate -- triggers early return with no interest accrual
    seeds.append((
        "zero_rate",
        pack_input(0, 2_000_000_000, 0, wad_lo, 0, 1_000_000, 100, 0, 0, 200)
    ))

    # Seed 2: Max rate (10000 bps = 100%) with full year elapsed
    seeds.append((
        "max_rate_full_year",
        pack_input(10000, 2_000_000_000, 0, wad_lo, 0, 1_000_000_000,
                   0, 0, 5000, SECONDS_PER_YEAR)
    ))

    # Seed 3: Zero time elapsed (current_ts == last_accrual)
    seeds.append((
        "zero_time_elapsed",
        pack_input(1000, 2_000_000_000, 0, wad_lo, 0, 1_000_000, 500, 0, 500, 500)
    ))

    # Seed 4: Max time -- current_timestamp far past maturity
    seeds.append((
        "max_time_past_maturity",
        pack_input(1000, 1000, 0, wad_lo, 0, 1_000_000, 0, 0, 500, 2_000_000_000)
    ))

    # Seed 5: Maturity boundary -- current_ts exactly at maturity
    seeds.append((
        "maturity_boundary_exact",
        pack_input(1000, 100_000, 0, wad_lo, 0, 1_000_000, 0, 0, 500, 100_000)
    ))

    # Seed 6: Near-overflow scale_factor (high u128 value)
    near_max_hi, near_max_lo = u128_to_hi_lo(U128_MAX // 2)
    seeds.append((
        "near_overflow_scale_factor",
        pack_input(10000, 2_000_000_000, near_max_hi, near_max_lo,
                   0, 1, 0, 0, 0, SECONDS_PER_YEAR)
    ))

    # Seed 7: Max accrued fees (u64::MAX) -- tests fee overflow path
    seeds.append((
        "max_accrued_fees",
        pack_input(1000, 2_000_000_000, 0, wad_lo, 0, 1_000_000_000_000,
                   0, U64_MAX, 10000, SECONDS_PER_YEAR)
    ))

    # Seed 8: Negative time delta (current < last_accrual)
    seeds.append((
        "negative_time_delta",
        pack_input(1000, 2_000_000_000, 0, wad_lo, 0, 1_000_000, 1000, 0, 500, 500)
    ))

    # Seed 9: 1 second elapsed with max supply
    seeds.append((
        "one_second_max_supply",
        pack_input(1000, 2_000_000_000, 0, wad_lo, U64_MAX, U64_MAX,
                   0, 0, 10000, 1)
    ))

    # Seed 10: Fee rate 100% with large supply
    seeds.append((
        "fee_rate_100pct",
        pack_input(5000, 2_000_000_000, 0, wad_lo, 0, 1_000_000_000_000,
                   0, 0, 10000, SECONDS_PER_YEAR)
    ))

    return seeds


def generate_deposit_scaling_seeds(strategies: Set[str]) -> List[Tuple[str, bytes]]:
    """
    Generate seeds for fuzz_deposit_withdraw_roundtrip.
    Input struct: amount: u64, sf_offset: u64
    """
    seeds = []

    def pack_input(amount, sf_offset):
        return pack_u64(amount) + pack_u64(sf_offset)

    # Seed 1: Zero amount -- triggers early return
    seeds.append(("zero_amount", pack_input(0, 0)))

    # Seed 2: Max amount (u64::MAX)
    seeds.append(("max_amount", pack_input(U64_MAX, 0)))

    # Seed 3: WAD as sf_offset (scale_factor = 2 * WAD)
    seeds.append(("wad_sf_offset", pack_input(1_000_000, WAD & U64_MAX)))

    # Seed 4: Max sf_offset (u64::MAX, clamped to WAD internally)
    seeds.append(("max_sf_offset", pack_input(1_000_000, U64_MAX)))

    # Seed 5: Amount = 1 (minimum non-zero) with zero offset (sf = WAD)
    seeds.append(("min_amount_base_sf", pack_input(1, 0)))

    # Seed 6: Amount = 1 with max sf_offset -- tests rounding to zero
    seeds.append(("min_amount_max_sf", pack_input(1, U64_MAX)))

    # Seed 7: Large amount near u64 max boundary
    seeds.append(("near_max_amount", pack_input(U64_MAX - 1, 1)))

    # Seed 8: Small sf_offset = 1 (scale_factor just above WAD)
    seeds.append(("tiny_sf_offset", pack_input(1_000_000_000, 1)))

    return seeds


def generate_settlement_factor_seeds(strategies: Set[str]) -> List[Tuple[str, bytes]]:
    """
    Generate seeds for fuzz_settlement_factor.
    Input struct: vault_balance: u64, accrued_fees: u64,
        scaled_total_supply_hi: u64, scaled_total_supply_lo: u64,
        scale_factor_hi: u64, scale_factor_lo: u64
    """
    seeds = []

    def pack_input(vault_bal, fees, supply_hi, supply_lo, sf_hi, sf_lo):
        return (
            pack_u64(vault_bal) + pack_u64(fees) +
            pack_u64(supply_hi) + pack_u64(supply_lo) +
            pack_u64(sf_hi) + pack_u64(sf_lo)
        )

    wad_hi, wad_lo = u128_to_hi_lo(WAD)

    # Seed 1: Zero available (vault == fees)
    seeds.append((
        "zero_available",
        pack_input(1_000_000, 1_000_000, 0, 1_000_000, 0, wad_lo)
    ))

    # Seed 2: Max available (max vault, zero fees)
    seeds.append((
        "max_available",
        pack_input(U64_MAX, 0, 0, 1_000_000, 0, wad_lo)
    ))

    # Seed 3: Zero normalized (supply = 0)
    seeds.append((
        "zero_supply",
        pack_input(1_000_000, 0, 0, 0, 0, wad_lo)
    ))

    # Seed 4: Equal amounts -- available == total_normalized
    seeds.append((
        "equal_amounts",
        pack_input(1_000_000, 0, 0, 1_000_000, 0, wad_lo)
    ))

    # Seed 5: Vault less than fees (fees_reserved = vault_balance)
    seeds.append((
        "vault_less_than_fees",
        pack_input(100, 1_000_000, 0, 1_000_000, 0, wad_lo)
    ))

    # Seed 6: Huge scale factor (near overflow in normalization)
    near_max_hi, near_max_lo = u128_to_hi_lo(U128_MAX // WAD)
    seeds.append((
        "huge_scale_factor",
        pack_input(U64_MAX, 0, 0, 1, near_max_hi, near_max_lo)
    ))

    # Seed 7: Max supply with WAD scale factor
    seeds.append((
        "max_supply_wad_sf",
        pack_input(U64_MAX, 0, U64_MAX, U64_MAX, 0, wad_lo)
    ))

    # Seed 8: Tiny vault, large supply (settlement factor near 1)
    seeds.append((
        "tiny_vault_large_supply",
        pack_input(1, 0, 0, U64_MAX, 0, wad_lo)
    ))

    return seeds


def generate_fee_computation_seeds(strategies: Set[str]) -> List[Tuple[str, bytes]]:
    """
    Generate seeds for fuzz_fee_calculations.
    Input struct: annual_bps: u16, fee_rate_bps: u16, time_elapsed: u32, supply: u64
    """
    seeds = []

    def pack_input(annual_bps, fee_bps, time_elapsed, supply):
        return (
            pack_u16(annual_bps) + pack_u16(fee_bps) +
            pack_u32(time_elapsed) + pack_u64(supply)
        )

    # Seed 1: Zero fee rate
    seeds.append(("zero_fee_rate", pack_input(1000, 0, SECONDS_PER_YEAR, 1_000_000)))

    # Seed 2: Max fee rate (100% = 10000 bps)
    seeds.append(("max_fee_rate", pack_input(1000, 10000, SECONDS_PER_YEAR, 1_000_000)))

    # Seed 3: Zero supply
    seeds.append(("zero_supply", pack_input(1000, 500, SECONDS_PER_YEAR, 0)))

    # Seed 4: Max supply (u64::MAX)
    seeds.append(("max_supply", pack_input(1000, 500, SECONDS_PER_YEAR, U64_MAX)))

    # Seed 5: Zero time elapsed
    seeds.append(("zero_time", pack_input(1000, 500, 0, 1_000_000)))

    # Seed 6: Max time (1 year in seconds)
    seeds.append(("max_time", pack_input(10000, 10000, SECONDS_PER_YEAR, U64_MAX)))

    # Seed 7: 1 second elapsed, minimal supply
    seeds.append(("one_second_min", pack_input(1, 1, 1, 1)))

    # Seed 8: Zero annual rate (no interest, so no fees)
    seeds.append(("zero_annual_rate", pack_input(0, 10000, SECONDS_PER_YEAR, 1_000_000)))

    # Seed 9: Max annual bps with max fee and large supply
    seeds.append((
        "max_annual_max_fee",
        pack_input(10000, 10000, SECONDS_PER_YEAR, 1_000_000_000_000)
    ))

    # Seed 10: Fee edge -- 1 bps fee rate
    seeds.append(("one_bps_fee", pack_input(5000, 1, SECONDS_PER_YEAR, 1_000_000_000)))

    return seeds


# ---------------------------------------------------------------------------
# Main: parse LCOV, generate seeds, write files
# ---------------------------------------------------------------------------

# Map fuzz target names to their generator functions
TARGET_GENERATORS = {
    "fuzz_interest_accrual": generate_interest_accrual_seeds,
    "fuzz_deposit_withdraw_roundtrip": generate_deposit_scaling_seeds,
    "fuzz_settlement_factor": generate_settlement_factor_seeds,
    "fuzz_fee_calculations": generate_fee_computation_seeds,
}

# Map target names to seed subdirectory names
TARGET_SEED_DIRS = {
    "fuzz_interest_accrual": "interest_accrual",
    "fuzz_deposit_withdraw_roundtrip": "deposit_scaling",
    "fuzz_settlement_factor": "settlement_factor",
    "fuzz_fee_calculations": "fee_computation",
}


def main():
    parser = argparse.ArgumentParser(
        description="Generate fuzz seeds from LCOV coverage data."
    )
    parser.add_argument(
        "--lcov",
        required=False,
        default=None,
        help="Path to LCOV file. If omitted, generates all baseline seeds.",
    )
    parser.add_argument(
        "--output-dir",
        required=True,
        help="Base output directory for seed files (e.g., fuzz/corpus).",
    )
    parser.add_argument(
        "--source-root",
        required=False,
        default=".",
        help="Project source root for resolving file paths in LCOV data.",
    )
    parser.add_argument(
        "--targets",
        nargs="*",
        default=None,
        help="Specific fuzz targets to generate seeds for. Default: all.",
    )
    args = parser.parse_args()

    # Parse coverage if LCOV is provided
    strategies: Set[str] = set()
    if args.lcov and os.path.exists(args.lcov):
        print(f"[*] Parsing LCOV file: {args.lcov}")
        files = parse_lcov(args.lcov)

        # Filter to relevant source files
        relevant = {
            k: v for k, v in files.items()
            if "/logic/" in k or "/processor/" in k
        }

        total_uncovered = sum(len(f.uncovered_lines) for f in relevant.values())
        total_covered = sum(len(f.covered_lines) for f in relevant.values())
        total = total_uncovered + total_covered
        pct = (total_covered / total * 100) if total > 0 else 0

        print(f"[*] Relevant files: {len(relevant)}")
        print(f"[*] Coverage: {total_covered}/{total} lines ({pct:.1f}%)")
        print(f"[*] Uncovered lines: {total_uncovered}")

        # List uncovered line details
        for fpath, cov in relevant.items():
            if cov.uncovered_lines:
                basename = os.path.basename(fpath)
                lines_str = ", ".join(str(l) for l in sorted(cov.uncovered_lines)[:20])
                suffix = "..." if len(cov.uncovered_lines) > 20 else ""
                print(f"    {basename}: lines {lines_str}{suffix}")

        strategies = classify_uncovered(relevant, args.source_root)
        print(f"[*] Identified strategies: {sorted(strategies)}")
    else:
        if args.lcov:
            print(f"[!] LCOV file not found: {args.lcov} -- generating baseline seeds")
        else:
            print("[*] No LCOV file provided -- generating baseline seeds")
        # Use all strategies for baseline seed generation
        strategies = set(PATH_HINTS.values())

    # Determine which targets to generate seeds for
    targets = args.targets or list(TARGET_GENERATORS.keys())

    total_seeds = 0
    for target in targets:
        if target not in TARGET_GENERATORS:
            print(f"[!] Unknown target: {target}, skipping")
            continue

        generator = TARGET_GENERATORS[target]
        seed_dir_name = TARGET_SEED_DIRS.get(target, target)
        out_dir = os.path.join(args.output_dir, seed_dir_name)
        os.makedirs(out_dir, exist_ok=True)

        seeds = generator(strategies)
        for name, data in seeds:
            seed_path = os.path.join(out_dir, f"seed_{name}")
            with open(seed_path, "wb") as f:
                f.write(data)
            total_seeds += 1

        print(f"[+] {target}: wrote {len(seeds)} seeds to {out_dir}")

    print(f"[*] Total seeds generated: {total_seeds}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
