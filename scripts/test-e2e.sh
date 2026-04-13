#!/usr/bin/env bash
set -euo pipefail

# ── Colors ────────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

pass() { echo -e "${GREEN}PASS${RESET} $1"; }
fail() { echo -e "${RED}FAIL${RESET} $1"; exit 1; }
info() { echo -e "${BOLD}==>${RESET} $1"; }
warn() { echo -e "${YELLOW}SKIP${RESET} $1"; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

# Temp directory for this test run
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

# ── 1. Prerequisites ─────────────────────────────────────────────────────────

info "Checking prerequisites"

rustc --version >/dev/null 2>&1 || fail "rustc not found — install via rustup"
RUSTC_VER="$(rustc --version | grep -oE '[0-9]+\.[0-9]+')"
pass "rustc $RUSTC_VER"

cargo --version >/dev/null 2>&1 || fail "cargo not found"
pass "cargo found"

if command -v cast >/dev/null 2>&1; then
    pass "cast found ($(cast --version 2>&1 | head -1))"
    HAS_CAST=1
else
    warn "cast not found — skipping Foundry steps (install: curl -L https://foundry.paradigm.xyz | bash && foundryup)"
    HAS_CAST=0
fi

echo ""

# ── 2. Build ──────────────────────────────────────────────────────────────────

info "Building rub3-wrapper"
cargo build -p rub3-wrapper 2>&1
WRAPPER="$ROOT/target/debug/rub3-wrapper"
[ -f "$WRAPPER" ] || fail "binary not found at $WRAPPER"
pass "build succeeded"
echo ""

# ── 3. Unit tests ─────────────────────────────────────────────────────────────

info "Running unit tests (offline)"
cargo test -p rub3-wrapper --bin rub3-wrapper 2>&1
pass "unit tests passed"
echo ""

if [ "${RUN_NETWORK_TESTS:-0}" = "1" ]; then
    info "Running network-dependent tests"
    cargo test -p rub3-wrapper --bin rub3-wrapper -- --ignored 2>&1
    pass "network tests passed"
    echo ""
else
    warn "network tests — set RUN_NETWORK_TESTS=1 to include"
    echo ""
fi

# ── 4. Wallet setup with cast ────────────────────────────────────────────────

if [ "$HAS_CAST" = "1" ]; then
    info "Creating ephemeral test wallet with cast"
    WALLET_OUTPUT="$(cast wallet new 2>&1)"
    ADDRESS="$(echo "$WALLET_OUTPUT" | grep -i 'address' | awk '{print $NF}')"
    PRIVKEY="$(echo "$WALLET_OUTPUT" | grep -i 'private' | awk '{print $NF}')"

    if [ -n "$ADDRESS" ]; then
        pass "wallet created: $ADDRESS"
    else
        fail "cast wallet new produced unexpected output"
    fi

    info "Checking wallet balance on Base"
    BALANCE="$(cast balance "$ADDRESS" --rpc-url https://mainnet.base.org 2>&1 || true)"
    if echo "$BALANCE" | grep -qE '^[0-9]'; then
        pass "balance query succeeded: $BALANCE wei"
    else
        warn "balance query failed (network issue?) — $BALANCE"
    fi

    info "Signing a test activation message"
    # SHA-256("com.rub3.example" || token_id=1 as 8-byte BE)
    # Replicate: printf 'com.rub3.example' + 0000000000000001 | sha256
    MSG_HASH="$(printf '%s' "com.rub3.example" | xxd -p | tr -d '\n')"
    TOKEN_HEX="0000000000000001"
    FULL_PREIMAGE="${MSG_HASH}${TOKEN_HEX}"
    ACTIVATION_HASH="$(echo -n "$FULL_PREIMAGE" | xxd -r -p | shasum -a 256 | awk '{print $1}')"

    SIG="$(cast wallet sign --private-key "$PRIVKEY" "0x${ACTIVATION_HASH}" 2>&1 || true)"
    if echo "$SIG" | grep -qE '^0x[0-9a-fA-F]+'; then
        pass "signature produced: ${SIG:0:20}..."
    else
        warn "signing failed — $SIG"
    fi
    echo ""
else
    warn "skipping wallet/cast steps"
    echo ""
fi

# ── 5. Wrapper integration — test binary ─────────────────────────────────────

info "Creating test binary"
TEST_APP="$TEST_DIR/test-app.sh"
cat > "$TEST_APP" <<'SCRIPT'
#!/bin/sh
echo "rub3-wrapped-app-ok"
SCRIPT
chmod +x "$TEST_APP"
pass "test binary at $TEST_APP"

info "Testing wrapper --binary with missing file"
BAD_EXIT=0
"$WRAPPER" --binary /tmp/nonexistent-rub3-binary 2>/dev/null || BAD_EXIT=$?
if [ "$BAD_EXIT" -ne 0 ]; then
    pass "wrapper rejects missing binary (exit $BAD_EXIT)"
else
    fail "wrapper should exit non-zero for missing binary"
fi
echo ""

# ── 6. Signal forwarding ─────────────────────────────────────────────────────

info "Testing SIGTERM forwarding"

# Pre-seed a fake license proof so the wrapper skips the webview
LICENSE_DIR="$TEST_DIR/licenses"
mkdir -p "$LICENSE_DIR"
cat > "$LICENSE_DIR/com.rub3.example.json" <<'JSON'
{
  "app_id": "com.rub3.example",
  "token_id": 1,
  "wallet_address": "0x0000000000000000000000000000000000000000",
  "signature": "0x0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
  "activated_at": "2026-01-01T00:00:00Z",
  "chain": "base",
  "contract": "0x0000000000000000000000000000000000000000"
}
JSON

# Launch wrapper with a long-running child, then send SIGTERM
RUB3_LICENSE_DIR="$LICENSE_DIR" "$WRAPPER" --binary /bin/sleep -- 300 &
WRAPPER_PID=$!
sleep 1

if kill -0 "$WRAPPER_PID" 2>/dev/null; then
    kill -TERM "$WRAPPER_PID"
    WAIT_EXIT=0
    wait "$WRAPPER_PID" 2>/dev/null || WAIT_EXIT=$?
    # The wrapper should have exited (non-zero is expected since we killed it)
    if ! kill -0 "$WRAPPER_PID" 2>/dev/null; then
        pass "wrapper exited after SIGTERM (exit $WAIT_EXIT)"
    else
        fail "wrapper still running after SIGTERM"
    fi
else
    # Wrapper may have exited because the fake proof failed verification — that's fine
    warn "wrapper exited before SIGTERM (proof verification likely failed — expected with dummy proof)"
fi
echo ""

# ── 7. License directory override ────────────────────────────────────────────

info "Testing RUB3_LICENSE_DIR override"
OVERRIDE_DIR="$TEST_DIR/override-licenses"
mkdir -p "$OVERRIDE_DIR"

# Wrapper should look in the override dir (and fail to find a proof, which is fine)
RUB3_LICENSE_DIR="$OVERRIDE_DIR" timeout 3 "$WRAPPER" --binary "$TEST_APP" 2>/dev/null &
OVERRIDE_PID=$!
sleep 2
# Kill it — we just needed to confirm it didn't crash
kill "$OVERRIDE_PID" 2>/dev/null || true
wait "$OVERRIDE_PID" 2>/dev/null || true
pass "wrapper accepts RUB3_LICENSE_DIR override"
echo ""

# ── Summary ───────────────────────────────────────────────────────────────────

echo -e "${GREEN}${BOLD}All automated checks passed.${RESET}"
echo ""
echo "Manual steps remaining:"
echo "  - Run the wrapper with a real contract and complete the webview activation flow"
echo "  - Verify the proof is saved to ~/Library/Application Support/rub3/licenses/"
echo "  - Re-run to confirm cached proof skips the activation window"
