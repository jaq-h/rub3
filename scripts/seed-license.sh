#!/usr/bin/env bash
set -euo pipefail

# Generates a valid license proof so the wrapper skips the activation window.
# Requires: cast (Foundry)
#
# NOTE: This proof only passes the signature check. When a real contract
# address is configured (non-zero), the wrapper also verifies on-chain
# ownership via ownerOf(). To test that path, deploy a contract to a
# local Anvil node and update CONTRACT in main.rs.
#
# Usage:
#   ./scripts/seed-license.sh
#   RUB3_LICENSE_DIR=/tmp/rub3-test cargo run -p rub3-wrapper -- --binary <path>

LICENSE_DIR="/tmp/rub3-test"
APP_ID="com.rub3.example"
TOKEN_ID=1

# Anvil's default account 0
PRIVKEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

command -v cast >/dev/null 2>&1 || { echo "error: cast not found — install Foundry: curl -L https://foundry.paradigm.xyz | bash && foundryup"; exit 1; }

ADDRESS=$(cast wallet address --private-key "$PRIVKEY")

# Build the activation message: SHA-256(app_id || token_id as 8-byte BE)
# token_id=1 → 0x0000000000000001
TOKEN_HEX=$(printf '%016x' "$TOKEN_ID")
MSG_HASH=$(printf '%s' "$APP_ID" | xxd -p | tr -d '\n' | (cat; echo "$TOKEN_HEX") | xxd -r -p | shasum -a 256 | awk '{print $1}')

# Sign with personal_sign (cast wallet sign applies the Ethereum prefix)
SIG=$(cast wallet sign --private-key "$PRIVKEY" "0x$MSG_HASH")

# Write the proof
mkdir -p "$LICENSE_DIR"
PROOF_PATH="$LICENSE_DIR/$APP_ID.json"

cat > "$PROOF_PATH" <<EOF
{
  "app_id": "$APP_ID",
  "token_id": $TOKEN_ID,
  "wallet_address": "$ADDRESS",
  "signature": "$SIG",
  "activated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "chain": "base",
  "contract": "0x0000000000000000000000000000000000000000"
}
EOF

echo "License proof written to $PROOF_PATH"
echo "  app_id:  $APP_ID"
echo "  address: $ADDRESS"
echo "  token:   $TOKEN_ID"
echo ""
echo "Run the wrapper with:"
echo "  RUB3_LICENSE_DIR=$LICENSE_DIR cargo run -p rub3-wrapper -- --binary <path>"
