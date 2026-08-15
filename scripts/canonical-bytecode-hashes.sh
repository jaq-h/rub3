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

for bin in forge git jq shasum xxd; do
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

# The manifest keys contracts by name, and forge writes artifacts to
# out/<file basename>/<contract>.json, so a name declared twice is ambiguous in
# both places. Fail closed rather than let one entry silently replace the other
# and leave a deployable contract with no recorded fingerprint.
duplicate_names="$(printf '%s\n' "${entries[@]}" | cut -f1 | sort | uniq -d)"
if [ -n "$duplicate_names" ]; then
  {
    echo "error: a contract name is declared in more than one file under contracts/src:"
    echo
    while IFS= read -r dup; do
      printf '    %s, declared in:\n' "$dup"
      printf '%s\n' "${entries[@]}" | awk -F'\t' -v d="$dup" '$1 == d { printf "      contracts/%s\n", $2 }'
    done <<< "$duplicate_names"
    cat <<'MSG'

The manifest in contracts/canonical-bytecode.json keys contracts by name, so one
of these would silently replace the other and a deployable contract would end up
with no recorded fingerprint while this gate still passed. forge also writes
artifacts to out/<declaring file basename>/<contract>.json, so two source files
sharing a basename are ambiguous there as well.

Rename one of the contracts so every contract name under contracts/src is
unique, then run

    scripts/canonical-bytecode-hashes.sh update

and commit the updated contracts/canonical-bytecode.json in the same pull
request.
MSG
  } >&2
  exit 1
fi

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

# Build settings are read out of the emitted artifacts' own solc metadata, not
# out of foundry.toml text, so the recorded inputs describe the build that
# actually produced these hashes. A `[profile.*]` selection or a FOUNDRY_* env
# override changes the artifacts too, and is therefore visible here. The manifest
# records one build block for all contracts, so every artifact has to agree on
# it: solc resolves a compiler per pragma when solc_version is unpinned, and
# per-path compilation restrictions can settle a sibling contract differently.
settings_of() {
  local name="$1" source="$2" artifact
  artifact="$(artifact_of "$name" "$source")"
  jq -e '{
    solc_version: .metadata.compiler.version,
    optimizer: .metadata.settings.optimizer.enabled,
    optimizer_runs: .metadata.settings.optimizer.runs,
    evm_version: .metadata.settings.evmVersion,
    bytecode_hash: .metadata.settings.metadata.bytecodeHash
  } | if any(.[]; . == null) then error("missing build settings") else . end' \
    "$artifact" 2>/dev/null
}

build_settings=""
ref_name=""
for i in "${!names[@]}"; do
  settings="$(settings_of "${names[$i]}" "${sources[$i]}")" || {
    echo "error: contracts/$(artifact_of "${names[$i]}" "${sources[$i]}") carries no complete solc" >&2
    echo "       .metadata block, so the build inputs behind ${names[$i]}'s fingerprint cannot be" >&2
    echo "       recorded. Ensure forge is emitting artifact metadata (do not disable it in" >&2
    echo "       contracts/foundry.toml)." >&2
    exit 1
  }
  if [ -z "$build_settings" ]; then
    build_settings="$settings"
    ref_name="${names[$i]}"
    continue
  fi
  differing="$(
    jq -n --argjson ref "$build_settings" --argjson other "$settings" \
      '[ $ref | keys[] | select($ref[.] != $other[.]) | {field: ., ref: $ref[.], other: $other[.]} ]'
  )"
  if [ "$(printf '%s' "$differing" | jq 'length')" -ne 0 ]; then
    {
      echo "error: ${names[$i]} was compiled under different build inputs than $ref_name:"
      echo
      printf '%s' "$differing" | jq -r --arg ref "$ref_name" --arg other "${names[$i]}" '.[] |
        "    \(.field)\n      \($ref): \(.ref | tostring)\n      \($other): \(.other | tostring)"'
      cat <<'MSG'

The manifest records a single build block for every fingerprint it publishes, so
a third party following it would reproduce one of these contracts under inputs
that never applied to it. This happens when solc_version is unpinned, so solc
resolves a compiler per pragma, or when per-path compilation restrictions settle
part of contracts/src differently.

Give the whole of contracts/src one set of build inputs, then run

    scripts/canonical-bytecode-hashes.sh update

and commit the updated contracts/canonical-bytecode.json in the same pull
request.
MSG
    } >&2
    exit 1
  fi
done

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

`forge build` never writes the lock, but `forge install` does write it when it is
absent, so this also rebuilds it from the checked-out submodules:

    (cd contracts && forge install)

The same revisions are readable from the submodule gitlinks, which are the
git-authoritative record:

    git ls-tree HEAD contracts/lib/
MSG
  exit 1
fi

missing_revs="$(jq -r 'to_entries | map(select((.value.rev | type) != "string" or .value.rev == "")) | .[].key' foundry.lock)"
if [ -n "$missing_revs" ]; then
  {
    echo "error: contracts/foundry.lock records no revision for:"
    echo
    printf '    %s\n' $missing_revs
    cat <<'MSG'

Every dependency revision is a build input behind the recorded fingerprints, so
the manifest cannot record a null for one. This usually means forge changed the
lock file's schema. `forge build` never writes the lock, and `forge install`
writes it only when it is absent, so regenerate it from the checked-out
submodules by removing it first:

    rm contracts/foundry.lock && (cd contracts && forge install)

If the regenerated lock still has no rev for those keys, read the revisions off
the submodule gitlinks instead:

    git ls-tree HEAD contracts/lib/

Fix this and commit the refreshed contracts/foundry.lock together with a
regenerated contracts/canonical-bytecode.json in the same pull request.
MSG
  } >&2
  exit 1
fi

lock_deps="$(jq 'with_entries(.value = .value.rev)' foundry.lock)"

if ! git -C "$repo_root" rev-parse --verify HEAD >/dev/null 2>&1; then
  cat >&2 <<'MSG'
error: cannot read the submodule gitlinks, because this tree is not a git
checkout with a commit at HEAD. The gate cross-checks contracts/foundry.lock
against the git-authoritative record of the pinned dependency revisions,

    git ls-tree HEAD contracts/lib/

so it cannot run here. Run it from a clone of this repository.
MSG
  exit 1
fi

gitlink_deps="$(
  git -C "$repo_root" ls-tree HEAD contracts/lib/ \
    | awk '$2 == "commit" { path = $4; sub(/^contracts\//, "", path); print path "\t" $3 }' \
    | jq -R -s 'split("\n") | map(select(length > 0) | split("\t") | {key: .[0], value: .[1]}) | from_entries'
)"

# contracts.md advertises `git ls-tree HEAD contracts/lib/` as the independent
# confirmation path for the pinned revisions. Now that foundry.lock is tracked
# rather than regenerated into every fresh clone, the two can disagree in git,
# so the gate enforces the claim instead of asserting it.
dependency_disagreements="$(
  jq -n --argjson lock "$lock_deps" --argjson links "$gitlink_deps" '
    ([($lock | keys[]), ($links | keys[])] | unique) as $keys
    | [ $keys[]
        | select(($lock[.] // null) != ($links[.] // null))
        | {key: ., lock: ($lock[.] // null), gitlink: ($links[.] // null)} ]'
)"
if [ "$(printf '%s' "$dependency_disagreements" | jq 'length')" -ne 0 ]; then
  {
    echo "error: contracts/foundry.lock and the submodule gitlinks at HEAD disagree:"
    echo
    printf '%s' "$dependency_disagreements" | jq -r '.[] |
      "    \(.key)\n      contracts/foundry.lock: \(.lock // "(not recorded)")\n      git ls-tree HEAD:       \(.gitlink // "(no gitlink at contracts/\(.key))")"'
    cat <<'MSG'

The dependency revisions are a build input behind the recorded fingerprints, and
contracts/contracts.md publishes both records as pinning the same revisions. If
they disagree, that published reproducibility claim is false, so this gate is red
on purpose. Reconcile them in this pull request, whichever record is right:

  - if the submodule gitlinks are right, check the submodules out at them and
    rebuild the lock from what is on disk. `forge build` never writes the lock,
    and `forge install` writes it only when it is absent, so remove it first:
        git submodule update --init --recursive
        rm contracts/foundry.lock && (cd contracts && forge install)
    then commit contracts/foundry.lock

  - if the lock is right, check each submodule out at the locked revision and
    stage the gitlink:
        git -C contracts/lib/<dep> checkout <rev> && git add contracts/lib/<dep>

Then run

    scripts/canonical-bytecode-hashes.sh update

and commit the refreshed contracts/canonical-bytecode.json alongside it.
MSG
  } >&2
  exit 1
fi

generated="$(
  jq -n \
    --argjson build "$build_settings" \
    --argjson deps "$lock_deps" \
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
      build: ($build + {dependencies: $deps}),
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
