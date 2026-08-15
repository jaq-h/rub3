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

case "$mode" in
  check|update|print) ;;
  *) echo "usage: $0 [check|update|print]" >&2; exit 2 ;;
esac

for bin in forge jq shasum xxd; do
  command -v "$bin" >/dev/null 2>&1 || { echo "error: $bin not found on PATH" >&2; exit 1; }
done

cd "$contracts_dir"

# --force so a stale out/ cannot make a drifted build look clean.
forge build --force >/dev/null

# forge writes artifacts as out/<declaring file basename>/<contract name>.json,
# so both the artifact path and the manifest's `source` field are derived from
# the file that actually declared the contract rather than assumed from its
# name. That keeps a contract in a subdirectory, and a second contract declared
# inside an existing file, honest.
artifact_of() {
  local name="$1" source="$2"
  printf 'out/%s/%s.json' "$(basename "$source")" "$name"
}

# sha256 of the raw bytes, not of the hex text: the hex string's case and its
# "0x" prefix are presentation, and a future consumer hashing bytes must agree.
hash_of() {
  local name="$1" source="$2" artifact object
  artifact="$(artifact_of "$name" "$source")"
  if [ ! -f "$artifact" ]; then
    echo "error: no artifact at contracts/$artifact for contract $name declared in contracts/$source" >&2
    return 1
  fi
  object="$(jq -r '.deployedBytecode.object // ""' "$artifact")"
  # xxd -r -p silently skips anything that is not a hex digit, so an unlinked
  # library placeholder ("__$<hash>$__"), an empty "0x", or a null object would
  # otherwise hash to a plausible-looking but meaningless fingerprint.
  if ! printf '%s' "$object" | grep -qE '^0x([0-9a-fA-F]{2})+$'; then
    cat >&2 <<MSG
error: $name (contracts/$source) has a deployedBytecode.object that is not a
non-empty, even-length 0x-prefixed hex string, so it cannot be fingerprinted:

    ${object:-<null>}

A contract linking an external library carries unresolved "__\$<hash>\$__"
placeholders here; hashing it would record a fingerprint covering only the bytes
before the first placeholder. An abstract contract or interface yields "0x" and
should not be in the deployable set at all.
MSG
    return 1
  fi
  printf '%s' "${object#0x}" | xxd -r -p | shasum -a 256 | cut -d' ' -f1
}

# The deployable set is every concrete contract under src/, at any depth.
# Abstract bases such as Rub3License have no deployedBytecode of their own and
# are excluded here by construction (their declaration starts with `abstract`),
# so a new sibling contract is picked up without editing this script.
entries=()
while IFS= read -r line; do
  if [ -n "$line" ]; then entries+=("$line"); fi
done < <(
  find src -type f -name '*.sol' -print0 \
    | while IFS= read -r -d '' file; do
        { grep -oE '^contract[[:space:]]+[A-Za-z0-9_]+' "$file" || true; } \
          | awk -v f="$file" '{print $2 "\t" f}'
      done \
    | sort
)
[ "${#entries[@]}" -gt 0 ] || { echo "error: no deployable contracts found under contracts/src" >&2; exit 1; }

names=()
sources=()
hashes=()
for entry in "${entries[@]}"; do
  name="${entry%%$'\t'*}"
  source="${entry#*$'\t'}"
  hash="$(hash_of "$name" "$source")" || exit 1
  names+=("$name")
  sources+=("$source")
  hashes+=("$hash")
done

if [ "$mode" = "print" ]; then
  for i in "${!names[@]}"; do printf '%s\t%s\n' "${names[$i]}" "${hashes[$i]}"; done
  exit 0
fi

# Build settings are read out of an emitted artifact's own solc metadata, not
# out of foundry.toml text, so the recorded inputs describe the build that
# actually produced these hashes. A `[profile.*]` selection or a FOUNDRY_* env
# override changes the artifact too, and is therefore visible here.
ref_artifact="$(artifact_of "${names[0]}" "${sources[0]}")"
build_settings="$(
  jq -e '{
    solc_version: .metadata.compiler.version,
    optimizer: .metadata.settings.optimizer.enabled,
    optimizer_runs: .metadata.settings.optimizer.runs,
    evm_version: .metadata.settings.evmVersion,
    bytecode_hash: .metadata.settings.metadata.bytecodeHash
  } | if any(.[]; . == null) then error("missing build settings") else . end' \
    "$ref_artifact"
)" || {
  echo "error: contracts/$ref_artifact carries no complete solc .metadata block, so the" >&2
  echo "       build inputs behind these fingerprints cannot be recorded. Ensure forge" >&2
  echo "       is emitting artifact metadata (do not disable it in foundry.toml)." >&2
  exit 1
}

bch="$(printf '%s' "$build_settings" | jq -r '.bytecode_hash')"
if [ "$bch" != "none" ]; then
  cat >&2 <<MSG
error: contracts were compiled with bytecode_hash = "$bch"; it must be "none".
With the solc default ("ipfs") the compiler appends a CBOR trailer hashing the
metadata JSON, which covers comment text and source file paths. The fingerprint
would then move on edits that change no behaviour, and a third party would have
to replicate this repo's directory layout to reproduce it.

Set bytecode_hash = "none" in contracts/foundry.toml and make sure no profile or
FOUNDRY_BYTECODE_HASH override is overriding it.
MSG
  exit 1
fi

if [ ! -f foundry.lock ]; then
  cat >&2 <<'MSG'
error: contracts/foundry.lock is missing, so the pinned dependency revisions
cannot be recorded. It is checked in; restore it with

    git checkout -- contracts/foundry.lock

The same two revisions are also readable from the submodule gitlinks, which are
the git-authoritative record:

    git ls-tree HEAD contracts/lib/
MSG
  exit 1
fi

generated="$(
  jq -n \
    --argjson build "$build_settings" \
    --slurpfile lock foundry.lock \
    --argjson contracts "$(
      for i in "${!names[@]}"; do
        jq -n --arg n "${names[$i]}" --arg s "${sources[$i]}" --arg h "${hashes[$i]}" \
          '{key: $n, value: {source: $s, deployed_bytecode_sha256: $h}}'
      done | jq -s 'from_entries'
    )" \
    '{
      schema: 1,
      algorithm: "sha256(deployedBytecode.object)",
      note: "Regenerate with scripts/canonical-bytecode-hashes.sh update. See contracts/contracts.md -> Reproducible builds.",
      build: ($build + {
        dependencies: ($lock[0] | with_entries(.value = .value.rev))
      }),
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
      for i in "${!names[@]}"; do printf '  %-20s %s\n' "${names[$i]}" "${hashes[$i]}"; done
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
esac
