# Coalesce Test Manifest

## Instruction Coverage Matrix

Maps each program instruction (by discriminator) to the test files that
exercise it. All tests run via `solana-program-test` with `prefer_bpf(true)`,
executing the real sBPF binary.

| Disc | Instruction | Primary Test File(s) | Notes |
|------|-------------|---------------------|-------|
| 0 | `initialize_protocol` | `test_admin.rs` | Protocol bootstrap, single-use |
| 1 | `set_fee_config` | `test_admin_instructions.rs`, `test_admin.rs` | Admin-only fee config update |
| 2 | `create_market` | `test_market.rs`, `test_advanced_scenarios.rs` | Market creation + PDA derivation |
| 3 | `set_borrower_whitelist` | `test_admin_instructions.rs`, `test_blacklist.rs` | Whitelist manager gated |
| 4 | `set_pause` | `test_admin_instructions.rs` | Pause/unpause market operations |
| 5 | `deposit` | `test_deposit_borrow.rs`, `test_lending.rs` | Lender deposits, scaling math |
| 6 | `borrow` | `test_deposit_borrow.rs`, `test_lending.rs`, `test_reborrow_capacity.rs` | Borrower draws, whitelist check |
| 7 | `repay` | `test_repay_withdraw.rs`, `test_lending.rs` | Principal + interest repayment |
| 8 | `withdraw` | `test_settlement.rs`, `test_repay_withdraw.rs` | Post-settlement lender withdrawal |
| 9 | `collect_fees` | `test_fees_close.rs`, `test_settlement.rs` | Protocol fee collection |
| 10 | `close_lender_position` | `test_settlement.rs`, `test_fees_close.rs`, `test_advanced_scenarios.rs` | Close position + rent reclaim |
| 11 | `re_settle` | `test_resettle.rs`, `test_settlement.rs` | Re-settlement after partial repay |
| 12 | `set_blacklist_mode` | `test_admin_instructions.rs`, `test_blacklist.rs` | Toggle whitelist/blacklist mode |
| 13 | `set_admin` | `test_admin_instructions.rs`, `test_admin.rs` | Transfer protocol admin |
| 14 | `set_whitelist_manager` | `test_admin_instructions.rs` | Assign per-market manager |
| 15 | reserved | -- | Not implemented |
| 16 | reserved | -- | Not implemented |
| 17 | `repay_interest` | `test_repay_interest.rs` | Interest-only repayment |
| 18 | `withdraw_excess` | `test_withdraw_excess.rs` | Withdraw excess vault balance |

## Additional Test Files

| File | Purpose |
|------|---------|
| `test_advanced_scenarios.rs` | Multi-step workflows, edge cases |
| `test_reborrow_capacity.rs` | Borrow capacity after partial repay |
| `test_clock_diag.rs` | Clock/timestamp diagnostics |
| `bpf_integration_tests.rs` | Cross-instruction BPF integration |
| `security_*.rs` | Security-focused tests (auth, CPI, overflow, etc.) |
| `common/mod.rs` | Shared test helpers and setup utilities |

## Running Tests

```bash
# Run all integration tests
cargo test-sbf

# Run a specific test file
cargo test-sbf --test test_settlement

# Run with output for debugging
cargo test-sbf -- --nocapture
```
