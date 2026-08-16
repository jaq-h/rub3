#!/usr/bin/env bash
#
# Schema check for contracts/deployments.json, the committed answer to "which
# Rub3Factory is the canonical one on this chain".
#
# Nothing is deployed yet, so today this mostly guards the shape. Its real job
# starts at launch: a wrong address here sends a developer's deploy - and its
# immutable fee - to a factory nobody agreed on, and that is unfixable after the
# fact. So the rules are deliberately strict about what may be written:
#
#   * every field is either a real value or null; there is no placeholder form,
#     because a placeholder that reads like an address is exactly the failure
#     this file exists to prevent
#   * an entry is wholly unpopulated or wholly populated, never half of each: a
#     factory address with no deploy block is a record nobody can verify
#   * an address is checksummed 0x-hex and never the zero address, which the
#     deploy scripts already read as "no factory"
#   * both chains the project targets stay present, so deleting the entry the
#     file exists to answer cannot land silently
#   * each of those chains carries the exact [rpc_endpoints] key from
#     contracts/foundry.toml that belongs to it, which is the contract the
#     manifest declares for that field. Pinned per chain id rather than checked
#     for membership in the alias set, because a swapped pair is a valid set and
#     would aim a mainnet deploy at Sepolia. The first version of this file
#     shipped the [etherscan] chain value instead, which looks identical to the
#     rpc alias and is not one.
#
# Usage, from anywhere in the repo:
#   scripts/check-deployments.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/contracts/deployments.json"

command -v jq >/dev/null 2>&1 || { echo "error: jq not found on PATH" >&2; exit 1; }
[ -f "$manifest" ] || { echo "error: $manifest is missing" >&2; exit 1; }

jq -e . "$manifest" >/dev/null 2>&1 || {
  echo "error: contracts/deployments.json is not valid JSON" >&2
  exit 1
}

# The rpc aliases foundry itself resolves, read out of the one section that
# defines them. Deliberately a key scrape rather than a TOML parser: the only
# thing needed here is the set of keys in [rpc_endpoints].
rpc_aliases="$(awk '
  /^\[rpc_endpoints\]/ { in_section = 1; next }
  /^\[/                 { in_section = 0 }
  in_section && /^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=/ {
    sub(/[[:space:]]*=.*/, "")
    gsub(/[[:space:]]/, "")
    print
  }
' "$repo_root/contracts/foundry.toml")"

[ -n "$rpc_aliases" ] || {
  echo "error: no [rpc_endpoints] keys found in contracts/foundry.toml" >&2
  exit 1
}
aliases_json="$(printf '%s\n' "$rpc_aliases" | jq -R . | jq -s .)"

# The chains this file must answer for, each pinned to the [rpc_endpoints] alias
# that belongs to it. The single source of both the "which chains" and the
# "which name" rules below; the aliases themselves are checked against
# foundry.toml, so renaming one there fails here until this map moves with it.
expected_chains='{"8453": "base", "84532": "base_sepolia"}'

# The program below emits one line per violated rule and exits 0 either way, so
# the shell test on this captured output is what actually fails the gate. Do not
# drop the capture and lean on jq's exit status: that turns the whole check into
# a no-op that still prints "ok". Kept as one program rather than a loop of
# shell tests so the whole schema is readable in one place.
errors="$(jq -r --argjson aliases "$aliases_json" --argjson expected "$expected_chains" '
  def fail($msg): "\($msg)";

  [
    (if .schema != 1 then fail("schema must be 1") else empty end),
    (if (.note | type) != "string" then fail("note must be a string") else empty end),
    (if (.fields | type) != "object" then fail("fields must be an object") else empty end),
    (if (.chains | type) != "object"
      then fail("chains must be an object")
      else (
        (($expected | keys[]) as $id |
          (if (.chains | has($id)) | not
            then fail("chains must include \($id) (\($expected[$id])), a chain this file must answer for")
            else empty end)),

        (($expected | to_entries[]) as $p |
          (if ($aliases | index($p.value)) == null
            then fail("chain \($p.key): the pinned name \"\($p.value)\" is no longer an [rpc_endpoints] key in contracts/foundry.toml (\($aliases | join(", ")))")
            else empty end)),

        (.chains | to_entries[] |
          . as $e |
          ($e.key) as $id |
          ($e.value) as $c |
          (
            (if ($id | test("^[1-9][0-9]*$") | not)
              then fail("chain key \($id): must be a decimal chain id") else empty end),
            (if ($c | type) != "object"
              then fail("chain \($id): entry must be an object")
              else (
                (if ($c | keys) != ["deploy_block", "factory", "generation", "name"]
                  then fail("chain \($id): keys must be exactly name, factory, deploy_block, generation") else empty end),
                (if ($c.name | type) != "string" or ($c.name | length) == 0
                  then fail("chain \($id): name must be a non-empty string")
                  elif ($expected | has($id)) and ($c.name != $expected[$id])
                    then fail("chain \($id): name must be \"\($expected[$id])\", the [rpc_endpoints] key in contracts/foundry.toml for this chain, not \"\($c.name)\"")
                  elif ($aliases | index($c.name)) == null
                    then fail("chain \($id): name \"\($c.name)\" is not an [rpc_endpoints] key in contracts/foundry.toml (\($aliases | join(", ")))")
                  else empty end),

                (if $c.factory != null and (($c.factory | type) != "string" or ($c.factory | test("^0x[0-9a-fA-F]{40}$") | not))
                  then fail("chain \($id): factory must be null or 0x-prefixed 40-hex") else empty end),
                (if ($c.factory | type) == "string" and ($c.factory | ascii_downcase) == "0x0000000000000000000000000000000000000000"
                  then fail("chain \($id): factory must not be the zero address") else empty end),
                (if ($c.factory | type) == "string" and ($c.factory | test("^0x[0-9a-f]{40}$")) and ($c.factory | test("[a-f]"))
                  then fail("chain \($id): factory must be EIP-55 checksummed, not all lower case") else empty end),
                (if ($c.factory | type) == "string" and ($c.factory | test("^0x[0-9A-F]{40}$")) and ($c.factory | test("[A-F]"))
                  then fail("chain \($id): factory must be EIP-55 checksummed, not all upper case") else empty end),

                (if $c.deploy_block != null and (($c.deploy_block | type) != "number" or ($c.deploy_block | floor) != $c.deploy_block or $c.deploy_block < 0)
                  then fail("chain \($id): deploy_block must be null or a non-negative integer") else empty end),
                (if $c.generation != null and (($c.generation | type) != "number" or ($c.generation | floor) != $c.generation or $c.generation < 1)
                  then fail("chain \($id): generation must be null or an integer >= 1") else empty end),

                (if ([$c.factory, $c.deploy_block, $c.generation] | map(. == null) | unique | length) != 1
                  then fail("chain \($id): an entry is wholly populated or wholly null, never partly") else empty end)
              )
              end)
          )
        )
      )
      end)
  ] | .[]
' "$manifest")"

if [ -n "$errors" ]; then
  echo "contracts/deployments.json failed its schema check:" >&2
  while IFS= read -r line; do echo "  - $line" >&2; done <<<"$errors"
  exit 1
fi

populated="$(jq -r '[.chains | to_entries[] | select(.value.factory != null) | .key] | length' "$manifest")"
total="$(jq -r '.chains | length' "$manifest")"
echo "contracts/deployments.json ok: $total chain(s), $populated with a published canonical factory"
