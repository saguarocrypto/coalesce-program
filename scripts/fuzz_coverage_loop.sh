#!/usr/bin/env bash
#
# fuzz_coverage_loop.sh -- Coverage-guided fuzzing feedback loop.
#
# Workflow:
#   1. Run cargo-llvm-cov to measure baseline coverage
#   2. Parse LCOV output to identify uncovered lines in src/logic/ and src/processor/
#   3. Generate targeted seed inputs using generate_fuzz_seeds.py
#   4. Run cargo fuzz on each target with the new seeds
#   5. Re-run coverage to measure improvement
#   6. Output summary of coverage delta
#
# Usage:
#   ./scripts/fuzz_coverage_loop.sh [--fuzz-duration SECONDS] [--iterations N] [--targets TARGET1,TARGET2]
#
# Environment:
#   FUZZ_DURATION  -- seconds to fuzz each target (default: 60)
#   FUZZ_ITERS     -- number of feedback loop iterations (default: 1)
#   FUZZ_TARGETS   -- comma-separated list of targets (default: all four core targets)

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FUZZ_DIR="$PROJECT_ROOT/fuzz"
COVERAGE_DIR="$PROJECT_ROOT/.coverage-loop"
SEEDS_SCRIPT="$SCRIPT_DIR/generate_fuzz_seeds.py"
STATIC_SEEDS_SCRIPT="$SCRIPT_DIR/create_static_seeds.py"

# Defaults
FUZZ_DURATION="${FUZZ_DURATION:-60}"
FUZZ_ITERS="${FUZZ_ITERS:-1}"
FUZZ_TARGETS="${FUZZ_TARGETS:-fuzz_interest_accrual,fuzz_deposit_withdraw_roundtrip,fuzz_settlement_factor,fuzz_fee_calculations}"

# Parse command-line arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --fuzz-duration)
            FUZZ_DURATION="$2"
            shift 2
            ;;
        --iterations)
            FUZZ_ITERS="$2"
            shift 2
            ;;
        --targets)
            FUZZ_TARGETS="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [--fuzz-duration SECONDS] [--iterations N] [--targets TARGET1,TARGET2]"
            echo ""
            echo "Options:"
            echo "  --fuzz-duration  Seconds to fuzz each target (default: 60)"
            echo "  --iterations     Number of feedback loop iterations (default: 1)"
            echo "  --targets        Comma-separated fuzz targets (default: all core targets)"
            echo ""
            echo "Environment variables: FUZZ_DURATION, FUZZ_ITERS, FUZZ_TARGETS"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

IFS=',' read -ra TARGETS <<< "$FUZZ_TARGETS"

# ---------------------------------------------------------------------------
# Utility functions
# ---------------------------------------------------------------------------

log() {
    echo "[$(date '+%H:%M:%S')] $*"
}

ensure_tools() {
    local missing=0

    if ! command -v cargo &>/dev/null; then
        echo "ERROR: cargo not found"
        missing=1
    fi

    if ! cargo llvm-cov --version &>/dev/null 2>&1; then
        log "Installing cargo-llvm-cov..."
        cargo install cargo-llvm-cov --locked
    fi

    if ! command -v cargo-fuzz &>/dev/null 2>&1; then
        if ! cargo fuzz --version &>/dev/null 2>&1; then
            log "Installing cargo-fuzz..."
            cargo install cargo-fuzz --locked
        fi
    fi

    if ! command -v python3 &>/dev/null; then
        echo "ERROR: python3 not found (needed for seed generation)"
        missing=1
    fi

    if [[ $missing -ne 0 ]]; then
        exit 1
    fi
}

# Run coverage and produce LCOV output.
# Args: $1 = output lcov path, $2 = label
run_coverage() {
    local lcov_path="$1"
    local label="$2"

    log "Running coverage ($label)..."
    cd "$PROJECT_ROOT"

    cargo llvm-cov clean --workspace 2>/dev/null || true

    cargo llvm-cov test \
        --features no-entrypoint \
        --lcov --output-path "$lcov_path" \
        --branch \
        --ignore-filename-regex '(tests/|fuzz/|kani_proofs)' \
        2>&1 | tail -5

    log "LCOV written to $lcov_path"
}

# Extract coverage percentage from LCOV file for logic/ and processor/ files.
# Outputs: "lines_covered lines_total branches_covered branches_total"
extract_coverage_metrics() {
    local lcov_path="$1"

    python3 - "$lcov_path" <<'PYEOF'
import sys

lcov_path = sys.argv[1]
lines_covered = 0
lines_total = 0
branches_covered = 0
branches_total = 0
in_relevant = False

with open(lcov_path) as f:
    for line in f:
        line = line.strip()
        if line.startswith("SF:"):
            path = line[3:]
            in_relevant = "/logic/" in path or "/processor/" in path
        elif line == "end_of_record":
            in_relevant = False
        elif in_relevant:
            if line.startswith("DA:"):
                parts = line[3:].split(",")
                if len(parts) >= 2:
                    lines_total += 1
                    if int(parts[1]) > 0:
                        lines_covered += 1
            elif line.startswith("BRDA:"):
                parts = line[5:].split(",")
                if len(parts) >= 4:
                    branches_total += 1
                    hit = parts[3]
                    if hit != "-" and int(hit) > 0:
                        branches_covered += 1

line_pct = (lines_covered / lines_total * 100) if lines_total > 0 else 0
branch_pct = (branches_covered / branches_total * 100) if branches_total > 0 else 0
print(f"{lines_covered} {lines_total} {branches_covered} {branches_total} {line_pct:.2f} {branch_pct:.2f}")
PYEOF
}

# Generate seeds from LCOV coverage data.
generate_seeds() {
    local lcov_path="$1"
    local corpus_base="$FUZZ_DIR/corpus"

    log "Generating targeted seeds from coverage data..."

    local target_args=""
    for target in "${TARGETS[@]}"; do
        target_args="$target_args $target"
    done

    python3 "$SEEDS_SCRIPT" \
        --lcov "$lcov_path" \
        --output-dir "$corpus_base" \
        --source-root "$PROJECT_ROOT" \
        --targets $target_args
}

# Copy hand-crafted seeds from fuzz/seeds/ into corpus directories.
seed_from_static() {
    local corpus_base="$FUZZ_DIR/corpus"

    if [[ -d "$FUZZ_DIR/seeds" ]]; then
        log "Copying static seeds from fuzz/seeds/ to corpus..."
        for seed_dir in "$FUZZ_DIR/seeds"/*/; do
            local dirname
            dirname="$(basename "$seed_dir")"
            # Map seed directory names to fuzz target names
            local target_name
            case "$dirname" in
                interest_accrual)    target_name="fuzz_interest_accrual" ;;
                deposit_scaling)     target_name="fuzz_deposit_withdraw_roundtrip" ;;
                settlement_factor)   target_name="fuzz_settlement_factor" ;;
                fee_computation)     target_name="fuzz_fee_calculations" ;;
                *)                   target_name="$dirname" ;;
            esac

            local corpus_target="$corpus_base/$dirname"
            mkdir -p "$corpus_target"
            cp -n "$seed_dir"* "$corpus_target/" 2>/dev/null || true
            log "  Seeded $dirname with $(ls "$seed_dir" | wc -l | tr -d ' ') static seeds"
        done
    fi
}

# Run fuzzing for each target.
run_fuzzing() {
    local corpus_base="$FUZZ_DIR/corpus"

    for target in "${TARGETS[@]}"; do
        # Determine the corpus subdirectory
        local seed_dir_name
        case "$target" in
            fuzz_interest_accrual)           seed_dir_name="interest_accrual" ;;
            fuzz_deposit_withdraw_roundtrip) seed_dir_name="deposit_scaling" ;;
            fuzz_settlement_factor)          seed_dir_name="settlement_factor" ;;
            fuzz_fee_calculations)           seed_dir_name="fee_computation" ;;
            *)                               seed_dir_name="$target" ;;
        esac

        local corpus_dir="$corpus_base/$seed_dir_name"
        mkdir -p "$corpus_dir"

        local seed_count
        seed_count=$(find "$corpus_dir" -type f 2>/dev/null | wc -l | tr -d ' ')
        log "Fuzzing $target for ${FUZZ_DURATION}s (corpus: $seed_count seeds)..."

        cd "$FUZZ_DIR"

        # Run cargo fuzz with the corpus directory and time limit.
        # The corpus directory is passed as the first positional argument.
        # -max_total_time limits the total fuzzing duration.
        cargo fuzz run "$target" "$corpus_dir" \
            -- -max_total_time="$FUZZ_DURATION" \
            2>&1 | tail -20 || {
            log "  [!] $target exited (may have found a crash or timed out)"
        }

        local new_count
        new_count=$(find "$corpus_dir" -type f 2>/dev/null | wc -l | tr -d ' ')
        log "  $target corpus: $seed_count -> $new_count inputs"
    done
}

# Print a coverage comparison summary.
print_summary() {
    local before_metrics="$1"
    local after_metrics="$2"

    # Parse metrics: "lines_covered lines_total branches_covered branches_total line_pct branch_pct"
    read -r b_lc b_lt b_bc b_bt b_lpct b_bpct <<< "$before_metrics"
    read -r a_lc a_lt a_bc a_bt a_lpct a_bpct <<< "$after_metrics"

    local line_delta
    local branch_delta
    line_delta=$(python3 -c "print(f'{$a_lpct - $b_lpct:+.2f}')")
    branch_delta=$(python3 -c "print(f'{$a_bpct - $b_bpct:+.2f}')")

    echo ""
    echo "================================================================="
    echo "  COVERAGE FEEDBACK LOOP SUMMARY"
    echo "================================================================="
    echo ""
    printf "  %-20s %10s %10s %10s\n" "Metric" "Before" "After" "Delta"
    printf "  %-20s %10s %10s %10s\n" "--------------------" "----------" "----------" "----------"
    printf "  %-20s %9s%% %9s%% %9s%%\n" "Line Coverage" "$b_lpct" "$a_lpct" "$line_delta"
    printf "  %-20s %9s%% %9s%% %9s%%\n" "Branch Coverage" "$b_bpct" "$a_bpct" "$branch_delta"
    echo ""
    printf "  %-20s %10s %10s\n" "Lines (covered/total)" "$b_lc/$b_lt" "$a_lc/$a_lt"
    printf "  %-20s %10s %10s\n" "Branches (cov/total)" "$b_bc/$b_bt" "$a_bc/$a_bt"
    echo ""
    echo "================================================================="
    echo ""
}

# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

main() {
    log "=== Fuzz Coverage Feedback Loop ==="
    log "Targets:  ${TARGETS[*]}"
    log "Duration: ${FUZZ_DURATION}s per target"
    log "Iterations: $FUZZ_ITERS"
    echo ""

    ensure_tools

    mkdir -p "$COVERAGE_DIR"

    # Generate static seeds (binary files from create_static_seeds.py)
    log "Generating static seed files..."
    python3 "$STATIC_SEEDS_SCRIPT"

    # Copy static seeds into corpus directories
    seed_from_static

    # Measure baseline coverage
    local baseline_lcov="$COVERAGE_DIR/baseline.lcov"
    run_coverage "$baseline_lcov" "baseline"
    local baseline_metrics
    baseline_metrics=$(extract_coverage_metrics "$baseline_lcov")
    read -r _ _ _ _ baseline_lpct baseline_bpct <<< "$baseline_metrics"
    log "Baseline: line=${baseline_lpct}%, branch=${baseline_bpct}%"

    local prev_metrics="$baseline_metrics"

    for iter in $(seq 1 "$FUZZ_ITERS"); do
        log ""
        log "=== Iteration $iter / $FUZZ_ITERS ==="

        # Generate seeds from the latest coverage data
        local latest_lcov="$COVERAGE_DIR/iter${iter}_before.lcov"
        cp "$COVERAGE_DIR/baseline.lcov" "$latest_lcov" 2>/dev/null || true
        if [[ $iter -gt 1 ]]; then
            local prev_lcov="$COVERAGE_DIR/iter$((iter-1))_after.lcov"
            if [[ -f "$prev_lcov" ]]; then
                latest_lcov="$prev_lcov"
            fi
        fi

        generate_seeds "$latest_lcov"

        # Run fuzzing
        run_fuzzing

        # Re-measure coverage
        local after_lcov="$COVERAGE_DIR/iter${iter}_after.lcov"
        run_coverage "$after_lcov" "iteration $iter"
        local after_metrics
        after_metrics=$(extract_coverage_metrics "$after_lcov")

        print_summary "$prev_metrics" "$after_metrics"

        prev_metrics="$after_metrics"
    done

    # Final summary against the original baseline
    if [[ "$FUZZ_ITERS" -gt 1 ]]; then
        log "=== Overall Summary (baseline vs final) ==="
        print_summary "$baseline_metrics" "$prev_metrics"
    fi

    log "Coverage loop complete."
}

main "$@"
