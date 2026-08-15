#!/usr/bin/env bash
#
# Canonical contract fingerprints: sha256 of each deployable contract's
# compiled `deployedBytecode.object`.
#
# That object is the runtime code with every immutable slot left zeroed, so it
# is a function of the contract's compiled semantics alone and not of the
# constructor arguments a particular deploy chose. It is the number a buyer's
# agent will later compare an on-chain contract against, so it must not move
# unless the contracts really changed.
#
# Usage, from anywhere in the repo:
#   scripts/canonical-bytecode-hashes.sh check    # compare against the manifest (default)
#   scripts/canonical-bytecode-hashes.sh update   # rewrite the manifest from the current build
#   scripts/canonical-bytecode-hashes.sh print    # print name<TAB>hash and exit
#
# `check` is what CI runs, and it is blocking. When the contracts legitimately
# change it fails until somebody updates the manifest in the same pull request.
# That is the point: the fingerprint must never move silently.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contracts_dir="$repo_root/contracts"
manifest="$contracts_dir/canonical-bytecode.json"
mode="${1:-check}"

for bin in forge jq shasum; do
  command -v "$bin" >/dev/null 2>&1 || { echo "error: $bin not found on PATH" >&2; exit 1; }
done

cd "$contracts_dir"

# --force so a stale out/ cannot make a drifted build look clean.
forge build --force >/dev/null

# sha256 of the raw bytes, not of the hex text: the hex string's case and its
# "0x" prefix are presentation, and a future consumer hashing bytes must agree.
hash_of() {
  local name="$1" artifact="out/$1.sol/$1.json"
  [ -f "$artifact" ] || { echo "error: no artifact at contracts/$artifact" >&2; exit 1; }
  jq -r '.deployedBytecode.object' "$artifact" \
    | tail -c +3 | tr -d '\n' | xxd -r -p | shasum -a 256 | cut -d' ' -f1
}

# The deployable set is every concrete contract under src/. Abstract bases such
# as Rub3License have no deployedBytecode of their own and are excluded here by
# construction, so a new sibling contract is picked up without editing this
# script.
contracts=()
while IFS= read -r name; do contracts+=("$name"); done < <(
  grep -hoE '^contract [A-Za-z0-9_]+' src/*.sol | awk '{print $2}' | sort
)
[ "${#contracts[@]}" -gt 0 ] || { echo "error: no deployable contracts found under contracts/src" >&2; exit 1; }

if [ "$mode" = "print" ]; then
  for c in "${contracts[@]}"; do printf '%s\t%s\n' "$c" "$(hash_of "$c")"; done
  exit 0
fi

# Build settings that the fingerprint depends on, read back out of the files
# that define them so the manifest can never drift from the real build.
read_toml() { grep -E "^$1[[:space:]]*=" foundry.toml | head -1 | sed -E 's/^[^=]+=[[:space:]]*//; s/"//g'; }
solc="$(read_toml solc_version)"
optimizer="$(read_toml optimizer)"
runs="$(read_toml optimizer_runs)"
evm="$(read_toml evm_version)"
bch="$(read_toml bytecode_hash)"

if [ "$bch" != "none" ]; then
  cat >&2 <<'MSG'
error: contracts/foundry.toml must set bytecode_hash = "none".
With the solc default ("ipfs") the compiler appends a CBOR trailer hashing the
metadata JSON, which covers comment text and source file paths. The fingerprint
would then move on edits that change no behaviour, and a third party would have
to replicate this repo's directory layout to reproduce it.
MSG
  exit 1
fi

generated="$(
  jq -n \
    --arg solc "$solc" --argjson optimizer "$optimizer" --argjson runs "$runs" \
    --arg evm "$evm" --arg bch "$bch" \
    --slurpfile lock foundry.lock \
    --argjson contracts "$(
      for c in "${contracts[@]}"; do
        jq -n --arg n "$c" --arg h "$(hash_of "$c")" \
          '{key: $n, value: {source: ("src/" + $n + ".sol"), deployed_bytecode_sha256: $h}}'
      done | jq -s 'from_entries'
    )" \
    '{
      schema: 1,
      algorithm: "sha256(deployedBytecode.object)",
      note: "Regenerate with scripts/canonical-bytecode-hashes.sh update. See contracts/contracts.md -> Reproducible builds.",
      build: {
        solc_version: $solc,
        optimizer: $optimizer,
        optimizer_runs: $runs,
        evm_version: $evm,
        bytecode_hash: $bch,
        dependencies: ($lock[0] | with_entries(.value = .value.rev))
      },
      contracts: $contracts
    }'
)"

case "$mode" in
  update)
    printf '%s\n' "$generated" > "$manifest"
    echo "wrote $manifest"
    ;;
  check)
    [ -f "$manifest" ] || { echo "error: missing $manifest; run 'scripts/canonical-bytecode-hashes.sh update'" >&2; exit 1; }
    drift_diff="$(mktemp)"
    trap 'rm -f "$drift_diff"' EXIT
    if diff -u <(jq -S . "$manifest") <(printf '%s\n' "$generated" | jq -S .) > "$drift_diff"; then
      echo "canonical bytecode fingerprints match contracts/canonical-bytecode.json"
      for c in "${contracts[@]}"; do printf '  %-20s %s\n' "$c" "$(hash_of "$c")"; done
    else
      echo "::error::canonical contract fingerprints drifted from contracts/canonical-bytecode.json"
      cat "$drift_diff"
      cat <<'MSG'

The compiled output of at least one contract changed, or a pinned build input
did. If that change is intended, run

    scripts/canonical-bytecode-hashes.sh update

and commit the updated contracts/canonical-bytecode.json in the same pull
request. Never update it in a separate commit from the contract change.
MSG
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 [check|update|print]" >&2
    exit 2
    ;;
esac
