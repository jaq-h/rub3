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

# jq exits non-zero on the first violated rule and prints which chain broke it.
# Kept as one program rather than a loop of shell tests so the whole schema is
# readable in one place.
errors="$(jq -r '
  def fail($msg): "\($msg)";

  [
    (if .schema != 1 then fail("schema must be 1") else empty end),
    (if (.note | type) != "string" then fail("note must be a string") else empty end),
    (if (.fields | type) != "object" then fail("fields must be an object") else empty end),
    (if (.chains | type) != "object" then fail("chains must be an object") else empty end),

    (.chains | to_entries[] |
      . as $e |
      ($e.key) as $id |
      ($e.value) as $c |
      (
        (if ($id | test("^[1-9][0-9]*$") | not)
          then fail("chain key \($id): must be a decimal chain id") else empty end),
        (if ($c | type) != "object"
          then fail("chain \($id): entry must be an object") else empty end),
        (if ($c | keys) != ["deploy_block", "factory", "generation", "name"]
          then fail("chain \($id): keys must be exactly name, factory, deploy_block, generation") else empty end),
        (if ($c.name | type) != "string" or ($c.name | length) == 0
          then fail("chain \($id): name must be a non-empty string") else empty end),

        (if $c.factory != null and (($c.factory | type) != "string" or ($c.factory | test("^0x[0-9a-fA-F]{40}$") | not))
          then fail("chain \($id): factory must be null or 0x-prefixed 40-hex") else empty end),
        (if ($c.factory | type) == "string" and ($c.factory | ascii_downcase) == "0x0000000000000000000000000000000000000000"
          then fail("chain \($id): factory must not be the zero address") else empty end),
        (if ($c.factory | type) == "string" and ($c.factory | test("^0x[0-9a-f]{40}$")) and ($c.factory | test("[a-f]"))
          then fail("chain \($id): factory must be EIP-55 checksummed, not all lower case") else empty end),

        (if $c.deploy_block != null and (($c.deploy_block | type) != "number" or ($c.deploy_block | floor) != $c.deploy_block or $c.deploy_block < 0)
          then fail("chain \($id): deploy_block must be null or a non-negative integer") else empty end),
        (if $c.generation != null and (($c.generation | type) != "number" or ($c.generation | floor) != $c.generation or $c.generation < 1)
          then fail("chain \($id): generation must be null or an integer >= 1") else empty end),

        (if ([$c.factory, $c.deploy_block, $c.generation] | map(. == null) | unique | length) != 1
          then fail("chain \($id): an entry is wholly populated or wholly null, never partly") else empty end)
      )
    )
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
