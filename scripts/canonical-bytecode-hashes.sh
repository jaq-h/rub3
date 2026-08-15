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
# It is NOT sha256(eth_getCode(addr)). A live deploy returns those immutable
# slots filled with the constructor's values, so a comparator MUST first zero
# every byte range the manifest publishes as `immutable_ranges` before hashing.
# This script publishes those ranges; it deliberately implements no comparison.
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

# Both directories come from the resolved config rather than being hardcoded,
# because `forge build` honours whatever the active profile or a FOUNDRY_*
# override selects. Reading them anywhere else would let this script hash
# artifacts from a build it did not perform.
foundry_config="$(forge config --json)" || {
  echo "error: 'forge config --json' failed, so the source and artifact directories" >&2
  echo "       behind this build cannot be resolved." >&2
  exit 1
}
foundry_src="$(jq -er '.src' <<<"$foundry_config")" || {
  echo "error: could not read the source directory from 'forge config --json'." >&2
  exit 1
}
foundry_out="$(jq -er '.out' <<<"$foundry_config")" || {
  echo "error: could not read the artifact directory from 'forge config --json'." >&2
  exit 1
}

# --force so a stale build output directory cannot make a drifted build look clean.
forge build --force >/dev/null

# forge writes artifacts as <out>/<declaring file basename>/<contract name>.json,
# so both the artifact path and the manifest's `source` field are derived from
# the file that actually declared the contract rather than assumed from its
# name. That keeps a contract in a subdirectory, and a second contract declared
# inside an existing file, honest.
artifact_of() {
  local name="$1" source="$2"
  printf '%s/%s/%s.json' "$foundry_out" "$(basename "$source")" "$name"
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

# The byte ranges solc reserved for this contract's immutables, flattened out of
# .deployedBytecode.immutableReferences and sorted. They are published so that a
# third party comparing a live deploy can zero exactly these bytes of
# eth_getCode(addr) before hashing; without them the published fingerprint can
# never be reproduced from chain data, because a real deploy carries the
# constructor's values in these slots while deployedBytecode.object has them
# zeroed. The AST-node keys solc groups them under are dropped: they are compiler
# internals with no meaning outside this artifact, and a masker needs the ranges
# themselves. Validated against the object's own length so a malformed or
# out-of-bounds range is a hard failure rather than a published lie.
immutable_ranges_of() {
  local name="$1" source="$2" artifact object size
  artifact="$(artifact_of "$name" "$source")"
  object="$(jq -r '.deployedBytecode.object // ""' "$artifact")"
  size=$(( (${#object} - 2) / 2 ))
  jq -e --argjson size "$size" '
    (.deployedBytecode.immutableReferences // {})
    | if type != "object" then error("immutableReferences is not an object") else . end
    | [ .[] | if type != "array" then error("immutableReferences entry is not an array") else .[] end ]
    | map(
        if (.start | type) != "number" or (.length | type) != "number"
           or (.start | floor) != .start or (.length | floor) != .length
           or .start < 0 or .length <= 0 or (.start + .length) > $size
        then error("immutable range outside the deployed bytecode")
        else {start: .start, length: .length} end)
    | sort_by(.start, .length)' "$artifact" 2>/dev/null
}

# The deployable set is every concrete contract under src/, at any depth.
# Abstract bases such as Rub3License have no deployedBytecode of their own and
# are excluded here by construction (their declaration starts with `abstract`),
# so a new sibling contract is picked up without editing this script.
entries=()
while IFS= read -r line; do
  if [ -n "$line" ]; then entries+=("$line"); fi
done < <(
  find "$foundry_src" -type f -name '*.sol' -print0 \
    | while IFS= read -r -d '' file; do
        { grep -oE '^[[:space:]]*contract[[:space:]]+[A-Za-z0-9_]+' "$file" || true; } \
          | awk -v f="$file" '{print $2 "\t" f}'
      done \
    | sort
)
[ "${#entries[@]}" -gt 0 ] || { echo "error: no deployable contracts found under contracts/$foundry_src" >&2; exit 1; }

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
ranges=()
for entry in "${entries[@]}"; do
  name="${entry%%$'\t'*}"
  source="${entry#*$'\t'}"
  hash="$(hash_of "$name" "$source")" || exit 1
  range="$(immutable_ranges_of "$name" "$source")" || {
    echo "error: contracts/$(artifact_of "$name" "$source") has a" >&2
    echo "       .deployedBytecode.immutableReferences block that is missing, malformed, or" >&2
    echo "       names a byte range outside the deployed bytecode, so the immutable slots a" >&2
    echo "       comparator must zero before hashing an on-chain deploy cannot be published." >&2
    echo "       A contract with no immutables yields an empty object here, which is fine;" >&2
    echo "       anything else means forge is not emitting usable artifact output." >&2
    exit 1
  }
  names+=("$name")
  sources+=("$source")
  hashes+=("$hash")
  ranges+=("$range")
done

# Build settings are read out of the emitted artifacts' own solc metadata, not
# out of foundry.toml text, so the recorded inputs describe the build that
# actually produced these hashes. `compilationTarget` is dropped because it is
# per-contract, and `remappings` because forge derives them from how deep the
# submodules happen to be initialised rather than from anything pinned here; a
# remapping that actually changes compiled output still moves the fingerprint. A `[profile.*]` selection or a FOUNDRY_* env
# override changes the artifacts too, and is therefore visible here. The manifest
# records one build block for all contracts, so every artifact has to agree on
# it: solc resolves a compiler per pragma when solc_version is unpinned, and
# per-path compilation restrictions can settle a sibling contract differently.
settings_of() {
  local name="$1" source="$2" artifact
  artifact="$(artifact_of "$name" "$source")"
  jq -e -S '{
    solc_version: .metadata.compiler.version,
    solc_settings: (.metadata.settings | del(.compilationTarget, .remappings))
  }
  | if (.solc_version | type) != "string"
       or (.solc_settings | type) != "object"
       or (.solc_settings.metadata.bytecodeHash | type) != "string"
    then error("missing build settings") else . end' \
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
      'def flat:
         {solc_version: .solc_version}
         + (.solc_settings | with_entries(.key = "solc_settings." + .key));
       (($ref | flat) as $a | ($other | flat) as $b
        | [ (($a | keys) + ($b | keys) | unique)[]
            | select($a[.] != $b[.])
            | {field: ., ref: $a[.], other: $b[.]} ])'
  )"
  if [ "$(printf '%s' "$differing" | jq 'length')" -ne 0 ]; then
    {
      echo "error: ${names[$i]} was compiled under different build inputs than $ref_name:"
      echo
      printf '%s' "$differing" | jq -r --arg ref "$ref_name" --arg other "${names[$i]}" '.[] |
        "    \(.field)\n      \($ref): \(.ref | tojson)\n      \($other): \(.other | tojson)"'
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

bch="$(printf '%s' "$build_settings" | jq -r '.solc_settings.metadata.bytecodeHash')"
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

# Only now, past the guard and the per-artifact settings validation, so `print`
# can never emit a fingerprint produced under the wrong build inputs. It stays
# clear of the git and foundry.lock requirements, which come after this.
if [ "$mode" = "print" ]; then
  for i in "${!names[@]}"; do printf '%s\t%s\n' "${names[$i]}" "${hashes[$i]}"; done
  exit 0
fi

if [ ! -f foundry.lock ]; then
  cat >&2 <<'MSG'
error: contracts/foundry.lock is missing, so the pinned dependency revisions
cannot be recorded. It is checked in; restore it with

    git checkout -- contracts/foundry.lock

`forge build` never writes the lock, but `forge install` does write it when it is
absent, so this also rebuilds it from the checked-out submodules:

    (cd contracts && forge install)

The same revisions are readable from the submodule records git keeps, which are
the git-authoritative pin:

    git ls-files -s contracts/lib/
MSG
  exit 1
fi

lock_schema_help() {
  cat >&2 <<'MSG'

Every dependency revision is a build input behind the recorded fingerprints, so
the manifest cannot record a null for one. This usually means forge changed the
lock file's schema. `forge build` never writes the lock, and `forge install`
writes it only when it is absent, so regenerate it from the checked-out
submodules by removing it first:

    rm contracts/foundry.lock && (cd contracts && forge install)

If the regenerated lock still has no usable revision, read the revisions off the
submodule records in git instead:

    git ls-files -s contracts/lib/

Fix this and commit the refreshed contracts/foundry.lock together with a
regenerated contracts/canonical-bytecode.json in the same pull request.
MSG
  exit 1
}

if ! jq -e 'type == "object"' foundry.lock >/dev/null 2>&1; then
  echo "error: contracts/foundry.lock is not a JSON object mapping each dependency path" >&2
  echo "       to a record carrying its revision." >&2
  lock_schema_help
fi

missing_revs="$(jq -r 'to_entries | map(select((.value | type) != "object" or (.value.rev | type) != "string" or .value.rev == "")) | .[].key' foundry.lock)"
if [ -n "$missing_revs" ]; then
  {
    echo "error: contracts/foundry.lock records no usable revision for:"
    echo
    printf '    %s\n' $missing_revs
  } >&2
  lock_schema_help
fi

lock_deps="$(jq 'with_entries(.value = .value.rev)' foundry.lock)"

if ! git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  cat >&2 <<'MSG'
error: cannot read the submodule gitlinks, because this tree is not a git
checkout. The gate cross-checks contracts/foundry.lock against the record git
keeps of the pinned dependency revisions,

    git ls-files -s contracts/lib/

so it cannot run here. Run it from a clone of this repository.
MSG
  exit 1
fi

# The index, not HEAD: after a checkout the two are identical, so CI compares
# against exactly what is committed, while locally a staged submodule bump is
# accepted. That lets a contributor regenerate the manifest in the same pull
# request as the bump, which is the workflow this gate documents.
gitlink_deps="$(
  git -C "$repo_root" ls-files -s contracts/lib/ \
    | awk '$1 == "160000" { path = $4; sub(/^contracts\//, "", path); print path "\t" $2 }' \
    | jq -R -s 'split("\n") | map(select(length > 0) | split("\t") | {key: .[0], value: .[1]}) | from_entries'
)"

# contracts.md advertises `git ls-files -s contracts/lib/` as the independent
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
    echo "error: contracts/foundry.lock and the submodule revisions git records disagree:"
    echo
    printf '%s' "$dependency_disagreements" | jq -r '.[] |
      "    \(.key)\n      contracts/foundry.lock: \(.lock // "(not recorded)")\n      git ls-files -s:        \(.gitlink // "(no submodule recorded at contracts/\(.key))")"'
    cat <<'MSG'

The dependency revisions are a build input behind the recorded fingerprints, and
contracts/contracts.md publishes both records as pinning the same revisions. If
they disagree, that published reproducibility claim is false, so this gate is red
on purpose. Reconcile them in this pull request, whichever record is right:

  - if you are bumping a dependency and the submodule is already checked out at
    the revision you want, stage it so git records it too:
        git add contracts/lib/<dep>

  - if the revisions git records are right, check the submodules out at them
    (`git submodule update` reads the same index this gate compares against) and
    rebuild the lock. `forge build` never writes the lock, and `forge install`
    writes it only when it is absent, so remove it first:
        git submodule update --init --recursive
        rm contracts/foundry.lock && (cd contracts && forge install)
    then commit contracts/foundry.lock

  - if the lock is right, check each submodule out at the locked revision and
    stage it:
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
          --argjson r "${ranges[$i]}" \
          '{key: $n, value: {source: $s, deployed_bytecode_sha256: $h, immutable_ranges: $r}}'
      done | jq -s 'from_entries'
    )" \
    '{
      schema: 1,
      algorithm: "sha256(deployedBytecode.object)",
      note: "Regenerate with scripts/canonical-bytecode-hashes.sh update. deployed_bytecode_sha256 covers deployedBytecode.object, which has every immutable slot zeroed. A live deploy carries the constructor values in those slots, so zero the byte ranges listed in immutable_ranges (start and length, in bytes, into the runtime code) before hashing eth_getCode(addr) and comparing. See contracts/contracts.md -> Reproducible builds.",
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
