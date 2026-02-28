# CoalesceFi Off-Chain Invariant Monitor

A standalone Rust binary that continuously monitors the CoalesceFi lending protocol's on-chain state and alerts on invariant violations.

## Overview

The monitor periodically:

1. Fetches all program accounts (Markets, LenderPositions, BorrowerWhitelists, ProtocolConfigs) via `getProgramAccounts` RPC calls, filtering by account data size.
2. Deserializes each account using zero-copy `bytemuck` casting (identical struct layouts to the on-chain program).
3. Runs a suite of invariant checks against the deserialized state.
4. Logs violations to stdout and optionally POSTs them to a webhook URL.

## Invariant Checks

| Invariant | Severity | Description |
|---|---|---|
| **Vault solvency** | Critical | When no active borrows exist, vault token balance >= accrued protocol fees. |
| **Scale factor validity** | Critical | For initialized markets, `scale_factor >= WAD` (1e18). |
| **Scale factor monotonicity** | Critical | `scale_factor` must never decrease between polling cycles. |
| **Settlement factor bounds** | Critical | When non-zero: `1 <= settlement_factor_wad <= WAD`. |
| **Settlement factor monotonicity** | Critical | `settlement_factor_wad` must never decrease between polling cycles. |
| **Fee non-negativity** | Warning | `accrued_protocol_fees` should not be suspiciously close to `u64::MAX`. |
| **Supply cap respected** | Critical | `scaled_total_supply * scale_factor / WAD <= max_total_supply`. |
| **Lender balance consistency** | Critical | `sum(lender_positions.scaled_balance) == market.scaled_total_supply`. |
| **Whitelist capacity** | Critical | `whitelist.current_borrowed <= whitelist.max_borrow_capacity`. |
| **Stale accrual detection** | Warning | Alerts if `current_time - last_accrual_timestamp > threshold`. Daily compounding is deterministic, but stale accrual may indicate an inactive market. |

## Building

```bash
cd monitoring
cargo build --release
```

The binary is produced at `target/release/coalescefi-monitor`.

## Usage

```bash
# Minimal invocation
coalescefi-monitor --program-id <BASE58_PROGRAM_ID>

# Full options
coalescefi-monitor \
  --rpc-url https://api.mainnet-beta.solana.com \
  --program-id <BASE58_PROGRAM_ID> \
  --interval-secs 30 \
  --stale-threshold-secs 3600 \
  --alert-webhook https://hooks.slack.com/services/...
```

### CLI Arguments

| Argument | Env Var | Default | Description |
|---|---|---|---|
| `--rpc-url` | `RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana RPC endpoint |
| `--program-id` | `PROGRAM_ID` | *(required)* | CoalesceFi program ID (base58) |
| `--interval-secs` | | `60` | Polling interval in seconds |
| `--stale-threshold-secs` | | `3600` | Stale accrual alert threshold (seconds) |
| `--alert-webhook` | `ALERT_WEBHOOK` | *(none)* | Optional webhook URL for alert delivery |

### Environment Variables

All arguments marked with an env var can be set via environment:

```bash
export RPC_URL=https://my-rpc.example.com
export PROGRAM_ID=CoaL...xyz
export ALERT_WEBHOOK=https://hooks.slack.com/services/...
coalescefi-monitor
```

### Logging

Logging is controlled by the `RUST_LOG` environment variable (via `env_logger`):

```bash
# Default: info level
RUST_LOG=info coalescefi-monitor --program-id ...

# Debug logging
RUST_LOG=debug coalescefi-monitor --program-id ...

# Only warnings and errors
RUST_LOG=warn coalescefi-monitor --program-id ...
```

## Webhook Payload

When a violation is detected and `--alert-webhook` is configured, the monitor POSTs a JSON payload:

```json
{
  "severity": "CRITICAL",
  "violation_type": "VaultInsolvency",
  "market_pubkey": "7xKX...abc",
  "expected": "vault_balance >= accrued_protocol_fees (100000)",
  "actual": "vault_balance = 50000",
  "timestamp": 1700000000
}
```

## Architecture

```
monitoring/
  src/
    main.rs        -- CLI, RPC fetching, monitoring loop, webhook alerting
    types.rs       -- Mirror of on-chain state structs (bytemuck Pod, #[repr(C)])
    invariants.rs  -- Individual invariant check functions + aggregate runner
```

### Type Safety

The `types.rs` module mirrors the on-chain struct layouts byte-for-byte:

- `Market` (250 bytes) -- market configuration and mutable state
- `ProtocolConfig` (194 bytes) -- singleton protocol configuration
- `LenderPosition` (128 bytes) -- per-lender, per-market balance
- `BorrowerWhitelist` (96 bytes) -- borrower capacity tracking

All structs use `#[repr(C)]`, derive `bytemuck::Pod + Zeroable`, and store multi-byte fields as little-endian `[u8; N]` arrays with accessor methods -- exactly matching the on-chain program's `src/state/*.rs`.

## Testing

Unit tests for the invariant logic are in two locations:

1. **In-crate tests**: `monitoring/src/invariants.rs` contains `#[cfg(test)]` module tests.
2. **Integration tests**: `tests/invariant_monitor_tests.rs` in the main project root exercises the invariant logic with constructed states, edge cases, and property-based tests.

```bash
# Run monitoring crate unit tests
cd monitoring
cargo test

# Run integration tests (from the main project root)
cd ..
cargo test --test invariant_monitor_tests
```
