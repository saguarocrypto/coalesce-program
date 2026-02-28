# Coalesce Lending Program

Coalesce is an unsecured, fixed interest rate lending protocol smart contract for the Solana blockchain. Built with [Pinocchio](https://github.com/anza-xyz/pinocchio) for raw BPF performance — no Anchor dependency.

## Deployments

| Network | Program Id |
| ------- | --- |
| Mainnet | `GooseA4bSoxitTMPa4ppe2zUQ9fu4139u8pEk6x65SR` |

## Building

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain, installed automatically via `rust-toolchain.toml`)
- [Solana CLI](https://docs.solanalabs.com/cli/install) with `cargo-build-sbf`

### Build the BPF binary

```bash
cargo build-sbf
```

The compiled program is output to `target/deploy/coalesce.so`.

### Build for native (tests and tooling)

```bash
cargo build
```

## Testing

### Integration tests

Integration tests execute the real sBPF binary via `solana-program-test`. After building, copy the binary to the test fixtures directory:

```bash
cargo build-sbf
cp target/deploy/coalesce.so tests/fixtures/
```

Then run the full test suite:

```bash
cargo test --features no-entrypoint
```

### Fuzz testing

13 coverage-guided fuzz targets are in `fuzz/`. Requires `cargo-fuzz` and the nightly toolchain:

```bash
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run <target> -- -max_total_time=120
```

### TLA+ model checking

Formal specifications live in `specs/`. Requires Java 17+ and [TLA+ tools](https://github.com/tlaplus/tlaplus):

```bash
cd specs
java -jar tla2tools.jar -config MC.cfg -workers auto -deadlock CoalesceFi.tla
```

## Project Structure

```
src/
  constants.rs      — Protocol constants (WAD, BPS, seeds, discriminators)
  error.rs          — Custom program error codes
  lib.rs            — Entrypoint and instruction dispatch
  logic/            — Pure business logic (interest accrual, validation)
  processor/        — Instruction handlers (deposit, borrow, repay, withdraw, etc.)
  state/            — Account structs (Market, LenderPosition, ProtocolConfig)
tests/              — 285+ integration tests (BPF execution)
fuzz/               — 13 libfuzzer targets
specs/              — TLA+ formal specification
monitoring/         — Off-chain invariant monitor
scripts/            — Transaction capture and seed generation utilities
```

## License

[Business Source License 1.1](LICENSE)
