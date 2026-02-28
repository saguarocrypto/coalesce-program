# Fuzz Seed Generation Strategy

This directory contains hand-crafted seed files for each fuzz target in the
CoalesceFi Pinocchio lending protocol. Seeds are organized by target and
designed to exercise specific code paths, boundary conditions, and error cases.

## Seed Organization

```
fuzz/seeds/
  interest_accrual/     -> fuzz_interest_accrual target
  deposit_scaling/      -> fuzz_deposit_withdraw_roundtrip target
  settlement_factor/    -> fuzz_settlement_factor target
  fee_computation/      -> fuzz_fee_calculations target
```

Each seed file is a raw binary blob matching the `Arbitrary`-derived `Input`
struct layout for its target. The `libfuzzer` engine deserializes these bytes
via the `Arbitrary` trait, which reads fields sequentially in little-endian
byte order.

## Target: interest_accrual

Input struct fields (in order):
- `annual_bps: u16` -- annual interest rate in basis points
- `maturity_timestamp: i64` -- market maturity unix timestamp
- `scale_factor_hi: u64` -- upper 64 bits of u128 scale_factor
- `scale_factor_lo: u64` -- lower 64 bits of u128 scale_factor
- `scaled_total_supply_hi: u64` -- upper 64 bits of u128 supply
- `scaled_total_supply_lo: u64` -- lower 64 bits of u128 supply
- `last_accrual_timestamp: i64` -- last interest accrual timestamp
- `accrued_protocol_fees: u64` -- accumulated protocol fees
- `fee_rate_bps: u16` -- protocol fee rate in basis points
- `current_timestamp: i64` -- current clock timestamp

Seeds:

| File | Path Targeted | Description |
|------|---------------|-------------|
| `seed_zero_rate` | Early return (annual_bps=0) | Zero interest rate produces no scale_factor change |
| `seed_max_rate_full_year` | Full interest computation | 100% annual rate over a full year with 50% fee rate (daily compounding via pow_wad) |
| `seed_zero_time` | time_elapsed <= 0 branch | current_timestamp == last_accrual_timestamp |
| `seed_past_maturity` | Maturity cap logic | current_ts far past maturity, tests effective_now capping |
| `seed_maturity_exact` | Boundary: current_ts == maturity | Exact maturity boundary |
| `seed_near_overflow_sf` | MathOverflow error paths | scale_factor near u128::MAX/2, triggers checked_mul overflow |
| `seed_max_fees_overflow` | Fee overflow path | accrued_protocol_fees at u64::MAX, tests checked_add overflow |
| `seed_negative_delta` | time_elapsed <= 0 (negative) | current_ts < last_accrual, tests saturating_sub |

## Target: deposit_scaling

Input struct fields (in order):
- `amount: u64` -- deposit amount in token base units
- `sf_offset: u64` -- offset added to WAD for scale_factor

Seeds:

| File | Path Targeted | Description |
|------|---------------|-------------|
| `seed_zero_amount` | Early return (amount == 0) | Zero deposit triggers immediate return |
| `seed_max_amount` | u64::MAX amount | Maximum possible deposit, tests overflow in amount * WAD |
| `seed_wad_sf` | scale_factor = 2 * WAD | Large offset doubles the scale factor |
| `seed_max_sf` | sf_offset = u64::MAX | Maximum offset (clamped to WAD internally) |
| `seed_min_amount_base` | amount=1, sf=WAD | Minimum non-zero deposit at base scale |
| `seed_min_amount_max_sf` | Rounding to zero path | amount=1 with max scale factor, scaled amount may round to zero |

## Target: settlement_factor

Input struct fields (in order):
- `vault_balance: u64` -- vault token balance
- `accrued_fees: u64` -- accumulated protocol fees
- `scaled_total_supply_hi: u64` -- upper 64 bits of u128
- `scaled_total_supply_lo: u64` -- lower 64 bits of u128
- `scale_factor_hi: u64` -- upper 64 bits of u128
- `scale_factor_lo: u64` -- lower 64 bits of u128

Seeds:

| File | Path Targeted | Description |
|------|---------------|-------------|
| `seed_zero_available` | available = 0 | vault_balance == accrued_fees, settlement factor hits min |
| `seed_max_available` | Max vault, zero fees | Maximum available balance for settlement |
| `seed_zero_supply` | Early return (supply == 0) | Zero supply triggers guard clause |
| `seed_equal_amounts` | factor = WAD (full settlement) | available exactly equals total_normalized |
| `seed_vault_lt_fees` | fees_reserved = vault_balance | Vault less than accrued fees |
| `seed_tiny_vault` | Settlement factor near minimum | Tiny vault with large supply, factor near 1 |

## Target: fee_computation

Input struct fields (in order):
- `annual_bps: u16` -- annual interest rate in basis points
- `fee_rate_bps: u16` -- protocol fee rate in basis points
- `time_elapsed: u32` -- seconds of interest accrual
- `supply: u64` -- scaled total supply

Seeds:

| File | Path Targeted | Description |
|------|---------------|-------------|
| `seed_zero_fee` | fee_rate_bps = 0 branch | Zero fee rate produces zero accrued fees |
| `seed_max_fee` | fee_rate_bps = 10000 | 100% fee rate, all interest goes to protocol |
| `seed_zero_supply` | supply == 0 early return | No supply means no fee computation |
| `seed_max_supply` | Large supply overflow paths | u64::MAX supply, tests checked_mul overflow in fee calc |
| `seed_zero_time` | time_elapsed == 0 return | Zero time produces no interest or fees |
| `seed_max_all` | Max annual + max fee + max time | All parameters at maximum, tests overflow cascade |
| `seed_one_bps_fee` | Tiny fee, precision test | 1 bps fee rate, tests whether small fees round to zero |

## Automated Seed Generation

The `scripts/generate_fuzz_seeds.py` script can generate additional seeds
from LCOV coverage data:

```bash
# Generate seeds from coverage report
python3 scripts/generate_fuzz_seeds.py \
    --lcov coverage.lcov \
    --output-dir fuzz/corpus \
    --source-root .

# Generate baseline seeds (no LCOV needed)
python3 scripts/generate_fuzz_seeds.py --output-dir fuzz/corpus
```

## Coverage Feedback Loop

The `scripts/fuzz_coverage_loop.sh` script automates the full cycle:

```bash
# Run one iteration with 60s fuzzing per target
./scripts/fuzz_coverage_loop.sh

# Run 3 iterations with 300s per target
./scripts/fuzz_coverage_loop.sh --iterations 3 --fuzz-duration 300

# Target specific fuzz targets only
./scripts/fuzz_coverage_loop.sh --targets fuzz_interest_accrual,fuzz_fee_calculations
```

The loop measures baseline coverage, generates targeted seeds, fuzzes, then
re-measures to report the coverage delta.

## Measuring Coverage with cargo fuzz

Use `cargo fuzz coverage` to generate LLVM coverage data for a specific target,
then convert it to a human-readable report.

```bash
# Generate raw coverage data for a target
cargo fuzz coverage fuzz_interest_accrual

# The raw profdata is written to:
#   fuzz/coverage/fuzz_interest_accrual/coverage.profdata

# Convert to LCOV format using llvm-cov
cargo cov -- export \
    target/x86_64-unknown-linux-gnu/coverage/x86_64-unknown-linux-gnu/release/fuzz_interest_accrual \
    --instr-profile=fuzz/coverage/fuzz_interest_accrual/coverage.profdata \
    --format=lcov > coverage.lcov

# Or generate an HTML report
cargo cov -- show \
    target/x86_64-unknown-linux-gnu/coverage/x86_64-unknown-linux-gnu/release/fuzz_interest_accrual \
    --instr-profile=fuzz/coverage/fuzz_interest_accrual/coverage.profdata \
    --format=html --output-dir=fuzz/coverage/html

# Quick summary (line/function/region counts)
cargo cov -- report \
    target/x86_64-unknown-linux-gnu/coverage/x86_64-unknown-linux-gnu/release/fuzz_interest_accrual \
    --instr-profile=fuzz/coverage/fuzz_interest_accrual/coverage.profdata
```

**Tips:**
- Run coverage after a fuzzing session so the corpus is populated.
- Compare coverage before and after adding new seeds to measure improvement.
- Focus on uncovered branches in `src/logic/` -- these represent untested
  arithmetic and validation paths.
