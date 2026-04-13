#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

echo "Running all rub3-wrapper tests..."
cargo test -p rub3-wrapper 2>&1

if [ "${RUN_NETWORK_TESTS:-0}" = "1" ]; then
    echo ""
    echo "Running network-dependent tests..."
    cargo test -p rub3-wrapper -- --ignored 2>&1
fi

echo ""
echo "All tests passed."
