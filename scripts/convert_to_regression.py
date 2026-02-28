#!/usr/bin/env python3
"""
convert_to_regression.py

Converts captured CoalesceFi transaction JSON (from capture_transactions.sh)
into Rust source fragments containing RegressionScenario structs ready to
paste into tests/regression_tests.rs or tests/historical_replay_tests.rs.

Usage:
    python3 scripts/convert_to_regression.py captured_transactions.json
    python3 scripts/convert_to_regression.py captured_transactions.json > tests/generated_scenarios.rs
    python3 scripts/convert_to_regression.py captured_transactions.json --market <MARKET_PUBKEY>

The script:
1. Reads the captured JSON file
2. Groups transactions by market address
3. Maps each instruction discriminant to a Transaction enum variant
4. Replays the math to compute expected outcomes at each step
5. Outputs Rust source with RegressionScenario structs

Instruction discriminants (from lib.rs):
  0  = InitializeProtocol   (fee_rate_bps: u16)
  1  = SetFeeConfig          (new_fee_rate_bps: u16)
  2  = CreateMarket          (nonce: u64, annual_interest_bps: u16, maturity_timestamp: i64, max_total_supply: u64)
  5  = Deposit               (amount: u64)
  6  = Borrow                (amount: u64)
  7  = Repay                 (amount: u64)
  8  = Withdraw              (scaled_amount: u128)
  9  = CollectFees           (no params)
  10 = CloseLenderPosition   (no params)
  11 = ReSettle              (no params)
  12 = SetBorrowerWhitelist  (is_whitelisted: u8, max_borrow_capacity: u64)
"""

import json
import struct
import sys
import argparse
from collections import defaultdict
from typing import Any

# ---------------------------------------------------------------------------
# Constants (must match src/constants.rs)
# ---------------------------------------------------------------------------

WAD = 10**18
BPS = 10_000
SECONDS_PER_YEAR = 31_536_000

# ---------------------------------------------------------------------------
# Instruction parameter decoding
# ---------------------------------------------------------------------------

DISC_NAMES = {
    0: "InitializeProtocol",
    1: "SetFeeConfig",
    2: "CreateMarket",
    5: "Deposit",
    6: "Borrow",
    7: "Repay",
    8: "Withdraw",
    9: "CollectFees",
    10: "CloseLenderPosition",
    11: "ReSettle",
    12: "SetBorrowerWhitelist",
}


def decode_params(disc: int, params_hex: str) -> dict[str, Any]:
    """Decode instruction parameters from hex bytes based on discriminant."""
    data = bytes.fromhex(params_hex) if params_hex else b""

    if disc == 0:
        # InitializeProtocol: fee_rate_bps (u16 LE)
        if len(data) >= 2:
            fee_rate_bps = struct.unpack_from("<H", data, 0)[0]
            return {"fee_rate_bps": fee_rate_bps}
        return {}

    if disc == 1:
        # SetFeeConfig: new_fee_rate_bps (u16 LE)
        if len(data) >= 2:
            new_fee_rate_bps = struct.unpack_from("<H", data, 0)[0]
            return {"new_fee_rate_bps": new_fee_rate_bps}
        return {}

    if disc == 2:
        # CreateMarket: nonce (u64) + annual_interest_bps (u16) + maturity_timestamp (i64) + max_total_supply (u64)
        if len(data) >= 26:
            nonce = struct.unpack_from("<Q", data, 0)[0]
            annual_interest_bps = struct.unpack_from("<H", data, 8)[0]
            maturity_timestamp = struct.unpack_from("<q", data, 10)[0]
            max_total_supply = struct.unpack_from("<Q", data, 18)[0]
            return {
                "nonce": nonce,
                "annual_interest_bps": annual_interest_bps,
                "maturity_timestamp": maturity_timestamp,
                "max_total_supply": max_total_supply,
            }
        return {}

    if disc in (5, 6, 7):
        # Deposit/Borrow/Repay: amount (u64 LE)
        if len(data) >= 8:
            amount = struct.unpack_from("<Q", data, 0)[0]
            return {"amount": amount}
        return {}

    if disc == 8:
        # Withdraw: scaled_amount (u128 LE)
        if len(data) >= 16:
            scaled_amount = int.from_bytes(data[0:16], "little")
            return {"scaled_amount": scaled_amount}
        return {}

    if disc in (9, 10, 11):
        # CollectFees / CloseLenderPosition / ReSettle: no params
        return {}

    if disc == 12:
        # SetBorrowerWhitelist: is_whitelisted (u8) + max_borrow_capacity (u64 LE)
        if len(data) >= 9:
            is_whitelisted = data[0]
            max_borrow_capacity = struct.unpack_from("<Q", data, 1)[0]
            return {
                "is_whitelisted": bool(is_whitelisted),
                "max_borrow_capacity": max_borrow_capacity,
            }
        return {}

    return {}


# ---------------------------------------------------------------------------
# Math replay (matches logic/interest.rs)
# ---------------------------------------------------------------------------


class ReplayState:
    """In-memory protocol state for math replay."""

    def __init__(self):
        self.fee_rate_bps: int = 0
        self.annual_interest_bps: int = 0
        self.maturity_timestamp: int = 0
        self.max_total_supply: int = 0
        self.creation_timestamp: int = 0
        self.scale_factor: int = WAD
        self.scaled_total_supply: int = 0
        self.accrued_protocol_fees: int = 0
        self.total_deposited: int = 0
        self.total_borrowed: int = 0
        self.total_repaid: int = 0
        self.last_accrual_timestamp: int = 0
        self.settlement_factor_wad: int = 0
        self.vault_balance: int = 0
        self.lender_positions: dict[str, int] = {}  # lender_key -> scaled_balance
        self.lender_indices: dict[str, int] = {}     # lender_key -> index
        self.borrower_whitelisted: bool = False
        self.borrower_max_capacity: int = 0
        self.borrower_total_borrowed: int = 0

    def get_lender_index(self, lender_key: str) -> int:
        if lender_key not in self.lender_indices:
            idx = len(self.lender_indices)
            self.lender_indices[lender_key] = idx
            self.lender_positions[lender_key] = 0
        return self.lender_indices[lender_key]

    def accrue_interest(self, current_timestamp: int, use_zero_fee: bool = False):
        """Accrue interest, matching logic/interest.rs."""
        maturity = self.maturity_timestamp
        effective_now = min(current_timestamp, maturity)
        time_elapsed = effective_now - self.last_accrual_timestamp
        if time_elapsed <= 0:
            return

        annual_bps = self.annual_interest_bps
        interest_delta_wad = (annual_bps * time_elapsed * WAD) // (SECONDS_PER_YEAR * BPS)
        scale_factor_delta = (self.scale_factor * interest_delta_wad) // WAD
        new_scale_factor = self.scale_factor + scale_factor_delta

        fee_rate = 0 if use_zero_fee else self.fee_rate_bps
        if fee_rate > 0:
            fee_delta_wad = (interest_delta_wad * fee_rate) // BPS
            fee_normalized = (
                (self.scaled_total_supply * new_scale_factor // WAD)
                * fee_delta_wad
                // WAD
            )
            self.accrued_protocol_fees += fee_normalized

        self.scale_factor = new_scale_factor
        self.last_accrual_timestamp = effective_now


def snapshot(state: ReplayState) -> dict[str, Any]:
    """Capture a snapshot of replay state for expected outcome."""
    return {
        "vault_balance": state.vault_balance,
        "scale_factor": state.scale_factor,
        "scaled_total_supply": state.scaled_total_supply,
        "total_deposited": state.total_deposited,
        "total_borrowed": state.total_borrowed,
        "total_repaid": state.total_repaid,
        "accrued_protocol_fees": state.accrued_protocol_fees,
        "settlement_factor_wad": state.settlement_factor_wad,
    }


# ---------------------------------------------------------------------------
# Transaction -> Rust code generation
# ---------------------------------------------------------------------------


def identify_lender_key(disc: int, accounts: list[str]) -> str | None:
    """Identify the lender address from instruction accounts.

    For Deposit (disc=5), the lender is typically accounts[0] (the signer).
    For Withdraw (disc=8), the lender is typically accounts[0].
    For CloseLenderPosition (disc=10), the lender is accounts[0].
    """
    if disc in (5, 8, 10) and accounts:
        return accounts[0]
    return None


def identify_market_key(disc: int, accounts: list[str]) -> str | None:
    """Identify the market account from instruction accounts.

    For most instructions, the market is accounts[1] or accounts[2]
    depending on the instruction layout. For CreateMarket (disc=2),
    the market is the newly created account.
    """
    # Heuristic: for most instructions, market is at index 1 or 2
    if disc == 2 and len(accounts) > 1:
        return accounts[1]  # The market PDA being initialized
    if disc in (5, 6, 7, 8, 9, 10, 11) and len(accounts) > 1:
        return accounts[1]  # Market account
    return None


def format_u128_rust(val: int) -> str:
    """Format a u128 value for Rust source."""
    if val == WAD:
        return "WAD"
    if val == 0:
        return "0"
    return f"{val}_u128"


def format_outcome_rust(snap: dict, lender_info: tuple | None = None, desc: str = "") -> str:
    """Generate Rust ExpectedOutcome struct literal."""
    lines = ["ExpectedOutcome {"]
    if snap.get("vault_balance") is not None:
        lines.append(f"    vault_balance: Some({snap['vault_balance']}),")
    if snap.get("scale_factor") is not None:
        lines.append(f"    scale_factor: Some({format_u128_rust(snap['scale_factor'])}),")
    if snap.get("scaled_total_supply") is not None:
        lines.append(
            f"    scaled_total_supply: Some({format_u128_rust(snap['scaled_total_supply'])}),")
    if snap.get("total_deposited") is not None:
        lines.append(f"    total_deposited: Some({snap['total_deposited']}),")
    if snap.get("total_borrowed") is not None:
        lines.append(f"    total_borrowed: Some({snap['total_borrowed']}),")
    if snap.get("total_repaid") is not None:
        lines.append(f"    total_repaid: Some({snap['total_repaid']}),")
    if snap.get("accrued_protocol_fees") is not None:
        lines.append(f"    accrued_protocol_fees: Some({snap['accrued_protocol_fees']}),")
    if snap.get("settlement_factor_wad") is not None and snap["settlement_factor_wad"] != 0:
        lines.append(
            f"    settlement_factor_wad: Some({format_u128_rust(snap['settlement_factor_wad'])}),")
    if lender_info is not None:
        idx, balance = lender_info
        lines.append(f"    lender_scaled_balance: Some(({idx}, {format_u128_rust(balance)})),")
    if desc:
        lines.append(f'    description: Some("{desc}"),')
    lines.append("    ..Default::default()")
    lines.append("}")
    return "\n".join(lines)


def generate_transaction_rust(
    disc: int, params: dict, block_time: int | None, state: ReplayState, accounts: list[str]
) -> tuple[str, dict, tuple | None, str]:
    """Generate Rust Transaction variant and replay the math.

    Returns: (rust_code, snapshot, lender_info, description)
    """
    ts = block_time or 0
    lender_info = None

    if disc == 0:
        fee_rate = params.get("fee_rate_bps", 0)
        state.fee_rate_bps = fee_rate
        desc = f"initialize protocol (fee_rate={fee_rate}bps)"
        rust = f"Transaction::InitializeProtocol {{ fee_rate_bps: {fee_rate} }}"
        return rust, snapshot(state), None, desc

    if disc == 1:
        new_fee = params.get("new_fee_rate_bps", 0)
        state.fee_rate_bps = new_fee
        desc = f"set fee config to {new_fee}bps"
        rust = f"Transaction::SetFeeConfig {{ new_fee_rate_bps: {new_fee} }}"
        return rust, snapshot(state), None, desc

    if disc == 2:
        aib = params.get("annual_interest_bps", 0)
        mt = params.get("maturity_timestamp", 0)
        mts = params.get("max_total_supply", 0)
        state.annual_interest_bps = aib
        state.maturity_timestamp = mt
        state.max_total_supply = mts
        state.creation_timestamp = ts
        state.scale_factor = WAD
        state.last_accrual_timestamp = ts
        state.vault_balance = 0
        state.scaled_total_supply = 0
        desc = f"create market (rate={aib}bps, maturity={mt})"
        rust = (
            f"Transaction::CreateMarket {{\n"
            f"    annual_interest_bps: {aib},\n"
            f"    maturity_timestamp: {mt},\n"
            f"    max_total_supply: {mts},\n"
            f"    creation_timestamp: {ts},\n"
            f"}}"
        )
        return rust, snapshot(state), None, desc

    if disc == 5:
        amount = params.get("amount", 0)
        state.accrue_interest(ts)
        scaled_amount = (amount * WAD) // state.scale_factor if state.scale_factor > 0 else 0
        state.vault_balance += amount
        state.scaled_total_supply += scaled_amount
        state.total_deposited += amount
        lender_key = identify_lender_key(disc, accounts) or f"lender_{len(state.lender_indices)}"
        idx = state.get_lender_index(lender_key)
        state.lender_positions[lender_key] = state.lender_positions.get(lender_key, 0) + scaled_amount
        lender_info = (idx, state.lender_positions[lender_key])
        desc = f"deposit {amount} (lender {idx})"
        rust = (
            f"Transaction::Deposit {{\n"
            f"    lender_index: {idx},\n"
            f"    amount: {amount},\n"
            f"    current_timestamp: {ts},\n"
            f"}}"
        )
        return rust, snapshot(state), lender_info, desc

    if disc == 6:
        amount = params.get("amount", 0)
        state.accrue_interest(ts)
        state.vault_balance -= amount
        state.total_borrowed += amount
        state.borrower_total_borrowed += amount
        desc = f"borrow {amount}"
        rust = (
            f"Transaction::Borrow {{\n"
            f"    amount: {amount},\n"
            f"    current_timestamp: {ts},\n"
            f"}}"
        )
        return rust, snapshot(state), None, desc

    if disc == 7:
        amount = params.get("amount", 0)
        state.accrue_interest(ts, use_zero_fee=True)
        state.vault_balance += amount
        state.total_repaid += amount
        desc = f"repay {amount}"
        rust = (
            f"Transaction::Repay {{\n"
            f"    amount: {amount},\n"
            f"    current_timestamp: {ts},\n"
            f"}}"
        )
        return rust, snapshot(state), None, desc

    if disc == 8:
        scaled_amount = params.get("scaled_amount", 0)
        state.accrue_interest(ts)
        lender_key = identify_lender_key(disc, accounts) or "unknown_lender"
        idx = state.get_lender_index(lender_key)

        # Compute settlement factor if not yet settled
        if state.settlement_factor_wad == 0:
            fees_reserved = min(state.vault_balance, state.accrued_protocol_fees)
            available = state.vault_balance - fees_reserved
            total_normalized = (state.scaled_total_supply * state.scale_factor) // WAD
            if total_normalized == 0:
                sf = WAD
            else:
                raw = (available * WAD) // total_normalized
                sf = min(raw, WAD)
                sf = max(sf, 1)
            state.settlement_factor_wad = sf

        pos_balance = state.lender_positions.get(lender_key, 0)
        effective_scaled = pos_balance if scaled_amount == 0 else scaled_amount
        normalized = (effective_scaled * state.scale_factor) // WAD
        payout = (normalized * state.settlement_factor_wad) // WAD
        payout = min(payout, state.vault_balance)

        state.vault_balance -= payout
        state.lender_positions[lender_key] = pos_balance - effective_scaled
        state.scaled_total_supply -= effective_scaled

        lender_info = (idx, state.lender_positions[lender_key])
        desc = f"withdraw scaled={scaled_amount} (lender {idx}, payout={payout})"
        rust = (
            f"Transaction::Withdraw {{\n"
            f"    lender_index: {idx},\n"
            f"    scaled_amount: {scaled_amount},\n"
            f"    current_timestamp: {ts},\n"
            f"}}"
        )
        return rust, snapshot(state), lender_info, desc

    if disc == 9:
        state.accrue_interest(ts)
        fees = state.accrued_protocol_fees
        withdrawable = min(fees, state.vault_balance)
        state.vault_balance -= withdrawable
        state.accrued_protocol_fees -= withdrawable
        desc = f"collect fees ({withdrawable})"
        rust = f"Transaction::CollectFees {{ current_timestamp: {ts} }}"
        return rust, snapshot(state), None, desc

    if disc == 10:
        lender_key = identify_lender_key(disc, accounts) or "unknown_lender"
        idx = state.get_lender_index(lender_key)
        desc = f"close lender position {idx}"
        rust = f"Transaction::CloseLenderPosition {{ lender_index: {idx} }}"
        return rust, snapshot(state), (idx, 0), desc

    if disc == 11:
        state.accrue_interest(ts, use_zero_fee=True)
        fees_reserved = min(state.vault_balance, state.accrued_protocol_fees)
        available = state.vault_balance - fees_reserved
        total_normalized = (state.scaled_total_supply * state.scale_factor) // WAD
        if total_normalized == 0:
            new_sf = WAD
        else:
            raw = (available * WAD) // total_normalized
            new_sf = min(raw, WAD)
            new_sf = max(new_sf, 1)
        state.settlement_factor_wad = new_sf
        desc = f"re-settle (factor={new_sf})"
        rust = f"Transaction::ReSettle {{ current_timestamp: {ts} }}"
        return rust, snapshot(state), None, desc

    if disc == 12:
        is_wl = params.get("is_whitelisted", False)
        cap = params.get("max_borrow_capacity", 0)
        state.borrower_whitelisted = is_wl
        state.borrower_max_capacity = cap
        desc = f"set borrower whitelist (wl={is_wl}, cap={cap})"
        rust = (
            f"Transaction::SetBorrowerWhitelist {{\n"
            f"    is_whitelisted: {'true' if is_wl else 'false'},\n"
            f"    max_borrow_capacity: {cap},\n"
            f"}}"
        )
        return rust, snapshot(state), None, desc

    desc = f"unknown instruction disc={disc}"
    rust = f"// Unknown instruction discriminant {disc}"
    return rust, snapshot(state), None, desc


# ---------------------------------------------------------------------------
# Main conversion logic
# ---------------------------------------------------------------------------


def convert_market_to_scenario(
    market_key: str, txs: list[dict], state: ReplayState
) -> str:
    """Generate a Rust regression scenario for a single market."""
    safe_name = market_key[:8].lower().replace(" ", "_")
    fn_name = f"regression_mainnet_{safe_name}"

    # Find the CreateMarket instruction to extract initial state
    create_tx = None
    for tx in txs:
        for ix in tx.get("instructions", []):
            if ix.get("discriminant") == 2:
                create_tx = tx
                break
        if create_tx:
            break

    # Build initial state from CreateMarket or defaults
    if create_tx:
        for ix in create_tx.get("instructions", []):
            if ix.get("discriminant") == 2:
                params = decode_params(2, ix.get("params_hex", ""))
                state.annual_interest_bps = params.get("annual_interest_bps", 0)
                state.maturity_timestamp = params.get("maturity_timestamp", 0)
                state.max_total_supply = params.get("max_total_supply", 0)
                state.creation_timestamp = create_tx.get("block_time", 0) or 0
                state.last_accrual_timestamp = state.creation_timestamp
                break

    lines = []
    lines.append(f"#[test]")
    lines.append(f"fn {fn_name}() {{")
    lines.append(f'    // Auto-generated from captured mainnet transactions')
    lines.append(f'    // Market: {market_key}')
    lines.append(f"    let scenario = RegressionScenario {{")
    lines.append(f'        name: "{fn_name}",')
    lines.append(f"        initial_state: InitialState {{")
    lines.append(f"            fee_rate_bps: {state.fee_rate_bps},")
    lines.append(f"            annual_interest_bps: {state.annual_interest_bps},")
    lines.append(f"            maturity_timestamp: {state.maturity_timestamp},")
    lines.append(f"            max_total_supply: {state.max_total_supply},")
    lines.append(f"            creation_timestamp: {state.creation_timestamp},")
    lines.append(f"            num_lenders: {max(len(state.lender_indices), 1)},")
    lines.append(f"            borrower_whitelisted: {'true' if state.borrower_whitelisted else 'false'},")
    lines.append(f"            borrower_max_capacity: {state.borrower_max_capacity},")
    lines.append(f"        }},")
    lines.append(f"        transactions: vec![")

    # Reset state for replay
    replay = ReplayState()
    replay.fee_rate_bps = state.fee_rate_bps
    replay.annual_interest_bps = state.annual_interest_bps
    replay.maturity_timestamp = state.maturity_timestamp
    replay.max_total_supply = state.max_total_supply
    replay.creation_timestamp = state.creation_timestamp
    replay.last_accrual_timestamp = state.creation_timestamp
    replay.scale_factor = WAD
    replay.borrower_whitelisted = state.borrower_whitelisted
    replay.borrower_max_capacity = state.borrower_max_capacity

    step = 0
    for tx in txs:
        for ix in tx.get("instructions", []):
            disc = ix.get("discriminant")
            if disc is None:
                continue
            # Skip CreateMarket (disc=2) as it is handled via initial_state
            if disc == 2:
                continue

            params = decode_params(disc, ix.get("params_hex", ""))
            bt = tx.get("block_time", 0) or 0
            accounts = ix.get("accounts", [])

            rust_tx, snap, lender_info, desc = generate_transaction_rust(
                disc, params, bt, replay, accounts
            )

            outcome = format_outcome_rust(snap, lender_info, f"step {step}: {desc}")
            lines.append(f"            // Step {step}: {desc}")
            lines.append(f"            (")
            for rl in rust_tx.split("\n"):
                lines.append(f"                {rl}")
            lines.append(f"                ,")
            for ol in outcome.split("\n"):
                lines.append(f"                {ol}")
            lines.append(f"            ),")
            step += 1

    lines.append(f"        ],")
    lines.append(f"    }};")
    lines.append(f"")
    lines.append(f"    replay_scenario(&scenario);")
    lines.append(f"}}")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(
        description="Convert captured CoalesceFi transactions to Rust regression tests"
    )
    parser.add_argument("input_file", help="Captured transactions JSON file")
    parser.add_argument(
        "--market",
        help="Only generate scenario for this specific market address",
        default=None,
    )
    parser.add_argument(
        "--fee-rate-bps",
        type=int,
        default=0,
        help="Protocol fee rate in bps (default: 0, read from InitializeProtocol if present)",
    )
    args = parser.parse_args()

    with open(args.input_file) as f:
        data = json.load(f)

    transactions = data.get("transactions", [])
    program_id = data.get("program_id", "unknown")

    if not transactions:
        print(f"// No transactions found in {args.input_file}", file=sys.stderr)
        print("// Empty input file -- no scenarios to generate")
        return

    # Group transactions by market address
    market_txs: dict[str, list[dict]] = defaultdict(list)
    global_fee_rate_bps = args.fee_rate_bps
    borrower_whitelisted = False
    borrower_max_capacity = 0

    for tx in transactions:
        if tx.get("skipped"):
            continue
        for ix in tx.get("instructions", []):
            disc = ix.get("discriminant")
            if disc is None:
                continue

            # Track global state from InitializeProtocol / SetFeeConfig
            if disc == 0:
                params = decode_params(0, ix.get("params_hex", ""))
                global_fee_rate_bps = params.get("fee_rate_bps", global_fee_rate_bps)

            if disc == 1:
                params = decode_params(1, ix.get("params_hex", ""))
                global_fee_rate_bps = params.get("new_fee_rate_bps", global_fee_rate_bps)

            if disc == 12:
                params = decode_params(12, ix.get("params_hex", ""))
                borrower_whitelisted = params.get("is_whitelisted", False)
                borrower_max_capacity = params.get("max_borrow_capacity", 0)

            # Identify market for this instruction
            market_key = identify_market_key(disc, ix.get("accounts", []))
            if market_key:
                market_txs[market_key].append(tx)

    # Filter to requested market if specified
    if args.market:
        if args.market in market_txs:
            market_txs = {args.market: market_txs[args.market]}
        else:
            print(f"// Market {args.market} not found in captured data", file=sys.stderr)
            print(f"// Available markets: {list(market_txs.keys())}", file=sys.stderr)
            return

    # Generate header
    print("// Auto-generated regression scenarios from captured mainnet transactions")
    print(f"// Source program: {program_id}")
    print(f"// Generated from: {args.input_file}")
    print(f"// Total markets: {len(market_txs)}")
    print()

    # Generate scenario for each market
    for market_key, txs in market_txs.items():
        # Deduplicate transactions (same tx can appear multiple times if it has multiple ixs)
        seen_sigs = set()
        unique_txs = []
        for tx in txs:
            sig = tx.get("signature", "")
            if sig not in seen_sigs:
                seen_sigs.add(sig)
                unique_txs.append(tx)

        state = ReplayState()
        state.fee_rate_bps = global_fee_rate_bps
        state.borrower_whitelisted = borrower_whitelisted
        state.borrower_max_capacity = borrower_max_capacity

        scenario_code = convert_market_to_scenario(market_key, unique_txs, state)
        print(scenario_code)
        print()


if __name__ == "__main__":
    main()
