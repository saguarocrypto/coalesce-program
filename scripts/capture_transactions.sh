#!/usr/bin/env bash
# capture_transactions.sh
#
# Fetches all transactions for a CoalesceFi program from a Solana RPC endpoint,
# extracts instruction data and metadata, and writes a chronologically sorted
# JSON file suitable for offline replay / regression test generation.
#
# Usage:
#   ./scripts/capture_transactions.sh <PROGRAM_ID> <RPC_URL> [OUTPUT_FILE]
#
# Examples:
#   # Capture from mainnet
#   ./scripts/capture_transactions.sh \
#       CoAL1234abcdef1234abcdef1234abcdef1234abcde \
#       https://api.mainnet-beta.solana.com \
#       captured_transactions.json
#
#   # Capture from devnet (default output file)
#   ./scripts/capture_transactions.sh \
#       CoAL1234abcdef1234abcdef1234abcdef1234abcde \
#       https://api.devnet.solana.com
#
#   # Using a custom RPC provider
#   ./scripts/capture_transactions.sh \
#       CoAL1234abcdef1234abcdef1234abcdef1234abcde \
#       <YOUR_RPC_URL> \
#       mainnet_capture.json
#
# Dependencies: bash 4+, curl, python3 (with json module)
#
# The output JSON has the following structure:
# {
#   "program_id": "<PROGRAM_ID>",
#   "rpc_url": "<RPC_URL>",
#   "captured_at": "<ISO8601>",
#   "total_signatures": <N>,
#   "total_transactions": <N>,
#   "transactions": [
#     {
#       "signature": "...",
#       "slot": 123456,
#       "block_time": 1700000000,
#       "err": null,
#       "instructions": [
#         {
#           "program_id_index": 2,
#           "program_id": "CoAL...",
#           "accounts": ["acct1", "acct2", ...],
#           "data_base64": "...",
#           "data_hex": "...",
#           "discriminant": 5,
#           "params_hex": "..."
#         }
#       ],
#       "account_keys": ["key1", "key2", ...]
#     },
#     ...
#   ]
# }

set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# Argument parsing
# ─────────────────────────────────────────────────────────────────────────────

if [ $# -lt 2 ]; then
    echo "Usage: $0 <PROGRAM_ID> <RPC_URL> [OUTPUT_FILE]" >&2
    echo "" >&2
    echo "Arguments:" >&2
    echo "  PROGRAM_ID   Base58-encoded program address" >&2
    echo "  RPC_URL      Solana JSON-RPC endpoint URL" >&2
    echo "  OUTPUT_FILE  Output JSON path (default: captured_transactions.json)" >&2
    exit 1
fi

PROGRAM_ID="$1"
RPC_URL="$2"
OUTPUT_FILE="${3:-captured_transactions.json}"

# Validate program_id looks like a base58 address (32-44 chars, base58 alphabet)
if ! echo "$PROGRAM_ID" | grep -qE '^[1-9A-HJ-NP-Za-km-z]{32,44}$'; then
    echo "ERROR: Invalid program ID format: $PROGRAM_ID" >&2
    echo "Expected a base58-encoded Solana address (32-44 characters)." >&2
    exit 1
fi

# Validate RPC URL
if ! echo "$RPC_URL" | grep -qiE '^https?://'; then
    echo "ERROR: Invalid RPC URL: $RPC_URL" >&2
    echo "Expected an HTTP(S) URL." >&2
    exit 1
fi

# ─────────────────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────────────────

PAGE_LIMIT=1000          # Max signatures per getSignaturesForAddress call
REQUEST_DELAY_MS=200     # Delay between RPC requests (ms) to avoid rate limits
TMP_DIR=$(mktemp -d)
SIGNATURES_FILE="$TMP_DIR/all_signatures.json"
TRANSACTIONS_FILE="$TMP_DIR/all_transactions.json"

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo "=== CoalesceFi Transaction Capture ==="
echo "Program ID:  $PROGRAM_ID"
echo "RPC URL:     $RPC_URL"
echo "Output file: $OUTPUT_FILE"
echo ""

# ─────────────────────────────────────────────────────────────────────────────
# Helper: JSON-RPC call
# ─────────────────────────────────────────────────────────────────────────────

rpc_call() {
    local method="$1"
    local params="$2"
    local result

    result=$(curl -s --max-time 30 "$RPC_URL" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{
            \"jsonrpc\": \"2.0\",
            \"id\": 1,
            \"method\": \"$method\",
            \"params\": $params
        }" 2>/dev/null)

    if [ -z "$result" ]; then
        echo "ERROR: Empty response from RPC for method $method" >&2
        return 1
    fi

    # Check for RPC errors
    local rpc_error
    rpc_error=$(echo "$result" | python3 -c "
import json, sys
data = json.load(sys.stdin)
if 'error' in data:
    print(json.dumps(data['error']))
else:
    print('')
" 2>/dev/null || echo "PARSE_ERROR")

    if [ "$rpc_error" = "PARSE_ERROR" ]; then
        echo "ERROR: Failed to parse RPC response for method $method" >&2
        echo "Response: $result" >&2
        return 1
    fi

    if [ -n "$rpc_error" ]; then
        echo "ERROR: RPC error for method $method: $rpc_error" >&2
        return 1
    fi

    echo "$result"
}

# ─────────────────────────────────────────────────────────────────────────────
# Step 1: Fetch all signatures via paginated getSignaturesForAddress
# ─────────────────────────────────────────────────────────────────────────────

echo "[1/3] Fetching transaction signatures..."

all_signatures="[]"
before_sig=""
page=0
total_sigs=0

while true; do
    page=$((page + 1))

    # Build params with optional "before" cursor for pagination
    if [ -z "$before_sig" ]; then
        params="[\"$PROGRAM_ID\", {\"limit\": $PAGE_LIMIT}]"
    else
        params="[\"$PROGRAM_ID\", {\"limit\": $PAGE_LIMIT, \"before\": \"$before_sig\"}]"
    fi

    echo "  Page $page (before=$( [ -z "$before_sig" ] && echo "none" || echo "${before_sig:0:12}..." ))..."

    response=$(rpc_call "getSignaturesForAddress" "$params") || {
        echo "ERROR: Failed to fetch signatures on page $page" >&2
        exit 1
    }

    # Extract signatures from response
    page_count=$(echo "$response" | python3 -c "
import json, sys
data = json.load(sys.stdin)
results = data.get('result', [])
print(len(results))
" 2>/dev/null)

    if [ "$page_count" = "0" ] || [ -z "$page_count" ]; then
        echo "  No more signatures found."
        break
    fi

    echo "  Found $page_count signatures on page $page."
    total_sigs=$((total_sigs + page_count))

    # Append to accumulated signatures
    all_signatures=$(echo "$response" | python3 -c "
import json, sys
data = json.load(sys.stdin)
existing = json.loads('$all_signatures') if '$all_signatures' != '[]' else []
existing.extend(data.get('result', []))
print(json.dumps(existing))
" 2>/dev/null)

    # Get the last signature for pagination cursor
    before_sig=$(echo "$response" | python3 -c "
import json, sys
data = json.load(sys.stdin)
results = data.get('result', [])
if results:
    print(results[-1]['signature'])
else:
    print('')
" 2>/dev/null)

    # If we got fewer than PAGE_LIMIT, we have reached the end
    if [ "$page_count" -lt "$PAGE_LIMIT" ]; then
        break
    fi

    # Rate limit
    sleep "$(echo "scale=3; $REQUEST_DELAY_MS / 1000" | bc)"
done

echo "  Total signatures found: $total_sigs"
echo "$all_signatures" > "$SIGNATURES_FILE"

if [ "$total_sigs" -eq 0 ]; then
    echo ""
    echo "WARNING: No transactions found for program $PROGRAM_ID"
    echo "This could mean:"
    echo "  - The program has not been called yet"
    echo "  - The program ID is incorrect"
    echo "  - The RPC endpoint does not have transaction history"
    echo ""
    # Write empty output
    python3 -c "
import json
from datetime import datetime, timezone
output = {
    'program_id': '$PROGRAM_ID',
    'rpc_url': '$RPC_URL',
    'captured_at': datetime.now(timezone.utc).isoformat(),
    'total_signatures': 0,
    'total_transactions': 0,
    'transactions': []
}
with open('$OUTPUT_FILE', 'w') as f:
    json.dump(output, f, indent=2)
print('Empty capture written to $OUTPUT_FILE')
"
    exit 0
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 2: Fetch full transaction data for each signature
# ─────────────────────────────────────────────────────────────────────────────

echo ""
echo "[2/3] Fetching full transaction data ($total_sigs transactions)..."

python3 << 'PYTHON_FETCH_SCRIPT'
import json
import subprocess
import sys
import time
import base64
import os

PROGRAM_ID = os.environ.get("_PROG_ID", "")
RPC_URL = os.environ.get("_RPC_URL", "")
SIGS_FILE = os.environ.get("_SIGS_FILE", "")
TXS_FILE = os.environ.get("_TXS_FILE", "")
DELAY_S = float(os.environ.get("_DELAY_S", "0.2"))

if not all([PROGRAM_ID, RPC_URL, SIGS_FILE, TXS_FILE]):
    # Fall back to reading from arguments embedded by the shell
    pass

with open(SIGS_FILE) as f:
    signatures = json.load(f)

transactions = []
total = len(signatures)

for i, sig_info in enumerate(signatures):
    sig = sig_info["signature"]
    slot = sig_info.get("slot", 0)
    block_time = sig_info.get("blockTime")
    err = sig_info.get("err")

    if (i + 1) % 50 == 0 or i == 0:
        print(f"  Fetching {i+1}/{total}...", file=sys.stderr)

    # Skip errored transactions (they did not mutate state)
    if err is not None:
        transactions.append({
            "signature": sig,
            "slot": slot,
            "block_time": block_time,
            "err": err,
            "instructions": [],
            "account_keys": [],
            "skipped": True,
            "skip_reason": "transaction_error"
        })
        continue

    # Fetch full transaction
    payload = json.dumps({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [sig, {
            "encoding": "jsonParsed",
            "maxSupportedTransactionVersion": 0
        }]
    })

    try:
        result = subprocess.run(
            ["curl", "-s", "--max-time", "30", RPC_URL,
             "-X", "POST",
             "-H", "Content-Type: application/json",
             "-d", payload],
            capture_output=True, text=True, timeout=35
        )
        response = json.loads(result.stdout)
    except Exception as e:
        print(f"  WARNING: Failed to fetch tx {sig[:12]}...: {e}", file=sys.stderr)
        transactions.append({
            "signature": sig,
            "slot": slot,
            "block_time": block_time,
            "err": None,
            "instructions": [],
            "account_keys": [],
            "skipped": True,
            "skip_reason": f"fetch_error: {str(e)}"
        })
        time.sleep(DELAY_S)
        continue

    if "error" in response:
        print(f"  WARNING: RPC error for tx {sig[:12]}...: {response['error']}", file=sys.stderr)
        transactions.append({
            "signature": sig,
            "slot": slot,
            "block_time": block_time,
            "err": None,
            "instructions": [],
            "account_keys": [],
            "skipped": True,
            "skip_reason": f"rpc_error: {json.dumps(response['error'])}"
        })
        time.sleep(DELAY_S)
        continue

    tx_data = response.get("result")
    if tx_data is None:
        transactions.append({
            "signature": sig,
            "slot": slot,
            "block_time": block_time,
            "err": None,
            "instructions": [],
            "account_keys": [],
            "skipped": True,
            "skip_reason": "null_result"
        })
        time.sleep(DELAY_S)
        continue

    # Extract block_time from transaction if available
    bt = tx_data.get("blockTime", block_time)

    # Extract account keys
    message = tx_data.get("transaction", {}).get("message", {})
    account_keys_raw = message.get("accountKeys", [])
    account_keys = []
    for ak in account_keys_raw:
        if isinstance(ak, dict):
            account_keys.append(ak.get("pubkey", ""))
        else:
            account_keys.append(str(ak))

    # Extract instructions targeting our program
    instructions_raw = message.get("instructions", [])
    our_instructions = []

    for ix in instructions_raw:
        # Check if this instruction targets our program
        ix_program = None
        if isinstance(ix.get("programId"), str):
            ix_program = ix["programId"]
        elif "programIdIndex" in ix and ix["programIdIndex"] < len(account_keys):
            ix_program = account_keys[ix["programIdIndex"]]

        if ix_program != PROGRAM_ID:
            continue

        # Extract data
        data_b64 = ix.get("data", "")
        try:
            data_bytes = base64.b64decode(data_b64)
        except Exception:
            data_bytes = b""

        data_hex = data_bytes.hex()
        discriminant = data_bytes[0] if len(data_bytes) > 0 else None
        params_hex = data_bytes[1:].hex() if len(data_bytes) > 1 else ""

        # Extract account indices/keys for this instruction
        ix_accounts = ix.get("accounts", [])
        ix_account_keys = []
        for a in ix_accounts:
            if isinstance(a, int) and a < len(account_keys):
                ix_account_keys.append(account_keys[a])
            elif isinstance(a, str):
                ix_account_keys.append(a)

        our_instructions.append({
            "program_id": ix_program,
            "accounts": ix_account_keys,
            "data_base64": data_b64,
            "data_hex": data_hex,
            "discriminant": discriminant,
            "params_hex": params_hex
        })

    # Also check inner instructions (from CPI)
    inner_instructions = tx_data.get("meta", {}).get("innerInstructions", []) or []
    for inner_group in inner_instructions:
        for ix in inner_group.get("instructions", []):
            ix_program = None
            if isinstance(ix.get("programId"), str):
                ix_program = ix["programId"]
            elif "programIdIndex" in ix and ix["programIdIndex"] < len(account_keys):
                ix_program = account_keys[ix["programIdIndex"]]

            if ix_program != PROGRAM_ID:
                continue

            data_b64 = ix.get("data", "")
            try:
                data_bytes = base64.b64decode(data_b64)
            except Exception:
                data_bytes = b""

            data_hex = data_bytes.hex()
            discriminant = data_bytes[0] if len(data_bytes) > 0 else None
            params_hex = data_bytes[1:].hex() if len(data_bytes) > 1 else ""

            ix_accounts = ix.get("accounts", [])
            ix_account_keys = []
            for a in ix_accounts:
                if isinstance(a, int) and a < len(account_keys):
                    ix_account_keys.append(account_keys[a])
                elif isinstance(a, str):
                    ix_account_keys.append(a)

            our_instructions.append({
                "program_id": ix_program,
                "accounts": ix_account_keys,
                "data_base64": data_b64,
                "data_hex": data_hex,
                "discriminant": discriminant,
                "params_hex": params_hex
            })

    transactions.append({
        "signature": sig,
        "slot": slot,
        "block_time": bt,
        "err": None,
        "instructions": our_instructions,
        "account_keys": account_keys,
        "skipped": False
    })

    time.sleep(DELAY_S)

with open(TXS_FILE, "w") as f:
    json.dump(transactions, f)

print(f"  Fetched {len(transactions)} transactions.", file=sys.stderr)
PYTHON_FETCH_SCRIPT

export _PROG_ID="$PROGRAM_ID"
export _RPC_URL="$RPC_URL"
export _SIGS_FILE="$SIGNATURES_FILE"
export _TXS_FILE="$TRANSACTIONS_FILE"
export _DELAY_S="$(echo "scale=3; $REQUEST_DELAY_MS / 1000" | bc)"

# Re-run the python script with env vars set
_PROG_ID="$PROGRAM_ID" \
_RPC_URL="$RPC_URL" \
_SIGS_FILE="$SIGNATURES_FILE" \
_TXS_FILE="$TRANSACTIONS_FILE" \
_DELAY_S="$(echo "scale=3; $REQUEST_DELAY_MS / 1000" | bc)" \
python3 << 'PYTHON_FETCH'
import json
import subprocess
import sys
import time
import base64
import os

PROGRAM_ID = os.environ["_PROG_ID"]
RPC_URL = os.environ["_RPC_URL"]
SIGS_FILE = os.environ["_SIGS_FILE"]
TXS_FILE = os.environ["_TXS_FILE"]
DELAY_S = float(os.environ["_DELAY_S"])

with open(SIGS_FILE) as f:
    signatures = json.load(f)

transactions = []
total = len(signatures)

for i, sig_info in enumerate(signatures):
    sig = sig_info["signature"]
    slot = sig_info.get("slot", 0)
    block_time = sig_info.get("blockTime")
    err = sig_info.get("err")

    if (i + 1) % 50 == 0 or i == 0:
        print(f"  Fetching {i+1}/{total}...", file=sys.stderr)

    if err is not None:
        transactions.append({
            "signature": sig,
            "slot": slot,
            "block_time": block_time,
            "err": err,
            "instructions": [],
            "account_keys": [],
            "skipped": True,
            "skip_reason": "transaction_error"
        })
        continue

    payload = json.dumps({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [sig, {
            "encoding": "jsonParsed",
            "maxSupportedTransactionVersion": 0
        }]
    })

    try:
        result = subprocess.run(
            ["curl", "-s", "--max-time", "30", RPC_URL,
             "-X", "POST",
             "-H", "Content-Type: application/json",
             "-d", payload],
            capture_output=True, text=True, timeout=35
        )
        response = json.loads(result.stdout)
    except Exception as e:
        print(f"  WARNING: Failed to fetch tx {sig[:12]}...: {e}", file=sys.stderr)
        transactions.append({
            "signature": sig, "slot": slot, "block_time": block_time,
            "err": None, "instructions": [], "account_keys": [],
            "skipped": True, "skip_reason": f"fetch_error: {e}"
        })
        time.sleep(DELAY_S)
        continue

    if "error" in response:
        print(f"  WARNING: RPC error for {sig[:12]}...: {response['error']}", file=sys.stderr)
        transactions.append({
            "signature": sig, "slot": slot, "block_time": block_time,
            "err": None, "instructions": [], "account_keys": [],
            "skipped": True, "skip_reason": f"rpc_error: {json.dumps(response['error'])}"
        })
        time.sleep(DELAY_S)
        continue

    tx_data = response.get("result")
    if tx_data is None:
        transactions.append({
            "signature": sig, "slot": slot, "block_time": block_time,
            "err": None, "instructions": [], "account_keys": [],
            "skipped": True, "skip_reason": "null_result"
        })
        time.sleep(DELAY_S)
        continue

    bt = tx_data.get("blockTime", block_time)
    message = tx_data.get("transaction", {}).get("message", {})
    account_keys_raw = message.get("accountKeys", [])
    account_keys = []
    for ak in account_keys_raw:
        if isinstance(ak, dict):
            account_keys.append(ak.get("pubkey", ""))
        else:
            account_keys.append(str(ak))

    instructions_raw = message.get("instructions", [])
    our_instructions = []

    for ix in instructions_raw:
        ix_program = None
        if isinstance(ix.get("programId"), str):
            ix_program = ix["programId"]
        elif "programIdIndex" in ix and ix["programIdIndex"] < len(account_keys):
            ix_program = account_keys[ix["programIdIndex"]]

        if ix_program != PROGRAM_ID:
            continue

        data_b64 = ix.get("data", "")
        try:
            data_bytes = base64.b64decode(data_b64)
        except Exception:
            data_bytes = b""

        data_hex = data_bytes.hex()
        discriminant = data_bytes[0] if data_bytes else None
        params_hex = data_bytes[1:].hex() if len(data_bytes) > 1 else ""

        ix_accounts = ix.get("accounts", [])
        ix_account_keys = []
        for a in ix_accounts:
            if isinstance(a, int) and a < len(account_keys):
                ix_account_keys.append(account_keys[a])
            elif isinstance(a, str):
                ix_account_keys.append(a)

        our_instructions.append({
            "program_id": ix_program,
            "accounts": ix_account_keys,
            "data_base64": data_b64,
            "data_hex": data_hex,
            "discriminant": discriminant,
            "params_hex": params_hex
        })

    # Check inner instructions for CPI
    inner_instructions = tx_data.get("meta", {}).get("innerInstructions", []) or []
    for inner_group in inner_instructions:
        for ix in inner_group.get("instructions", []):
            ix_program = None
            if isinstance(ix.get("programId"), str):
                ix_program = ix["programId"]
            elif "programIdIndex" in ix and ix["programIdIndex"] < len(account_keys):
                ix_program = account_keys[ix["programIdIndex"]]
            if ix_program != PROGRAM_ID:
                continue

            data_b64 = ix.get("data", "")
            try:
                data_bytes = base64.b64decode(data_b64)
            except Exception:
                data_bytes = b""

            data_hex = data_bytes.hex()
            discriminant = data_bytes[0] if data_bytes else None
            params_hex = data_bytes[1:].hex() if len(data_bytes) > 1 else ""

            ix_accounts = ix.get("accounts", [])
            ix_account_keys = []
            for a in ix_accounts:
                if isinstance(a, int) and a < len(account_keys):
                    ix_account_keys.append(account_keys[a])
                elif isinstance(a, str):
                    ix_account_keys.append(a)

            our_instructions.append({
                "program_id": ix_program,
                "accounts": ix_account_keys,
                "data_base64": data_b64,
                "data_hex": data_hex,
                "discriminant": discriminant,
                "params_hex": params_hex
            })

    transactions.append({
        "signature": sig,
        "slot": slot,
        "block_time": bt,
        "err": None,
        "instructions": our_instructions,
        "account_keys": account_keys,
        "skipped": False
    })

    time.sleep(DELAY_S)

with open(TXS_FILE, "w") as f:
    json.dump(transactions, f)

print(f"  Fetched {len(transactions)} transactions.", file=sys.stderr)
PYTHON_FETCH

# ─────────────────────────────────────────────────────────────────────────────
# Step 3: Sort chronologically and write final output
# ─────────────────────────────────────────────────────────────────────────────

echo ""
echo "[3/3] Sorting and writing output..."

_PROG_ID="$PROGRAM_ID" \
_RPC_URL="$RPC_URL" \
_TXS_FILE="$TRANSACTIONS_FILE" \
_OUTPUT_FILE="$OUTPUT_FILE" \
python3 << 'PYTHON_SORT'
import json
import os
from datetime import datetime, timezone

PROGRAM_ID = os.environ["_PROG_ID"]
RPC_URL = os.environ["_RPC_URL"]
TXS_FILE = os.environ["_TXS_FILE"]
OUTPUT_FILE = os.environ["_OUTPUT_FILE"]

with open(TXS_FILE) as f:
    transactions = json.load(f)

# Sort by (block_time, slot) ascending for chronological order
# Transactions without block_time are placed at the end
def sort_key(tx):
    bt = tx.get("block_time") or float("inf")
    sl = tx.get("slot", 0)
    return (bt, sl)

transactions.sort(key=sort_key)

# Count non-skipped
non_skipped = sum(1 for tx in transactions if not tx.get("skipped", False))

output = {
    "program_id": PROGRAM_ID,
    "rpc_url": RPC_URL,
    "captured_at": datetime.now(timezone.utc).isoformat(),
    "total_signatures": len(transactions),
    "total_transactions": non_skipped,
    "transactions": transactions
}

with open(OUTPUT_FILE, "w") as f:
    json.dump(output, f, indent=2)

print(f"  Output written to {OUTPUT_FILE}")
print(f"  Total signatures: {len(transactions)}")
print(f"  Successful transactions: {non_skipped}")
skipped = len(transactions) - non_skipped
if skipped > 0:
    print(f"  Skipped transactions: {skipped}")
PYTHON_SORT

echo ""
echo "=== Capture complete ==="
echo "Output: $OUTPUT_FILE"
echo ""
echo "Next steps:"
echo "  1. Review the captured data:"
echo "     python3 -m json.tool $OUTPUT_FILE | head -50"
echo ""
echo "  2. Convert to regression tests:"
echo "     python3 scripts/convert_to_regression.py $OUTPUT_FILE > tests/generated_scenarios.rs"
echo ""
echo "  3. Paste generated scenarios into tests/regression_tests.rs or"
echo "     tests/historical_replay_tests.rs and run:"
echo "     cargo test --features no-entrypoint"
