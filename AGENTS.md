# rub3

Wallet-native licensing for locally executed software (CLI tools, MCP servers, desktop apps). An ERC-721 token on Base *is* the access credential; the `rub3-wrapper` Rust runtime verifies ownership on the machine at launch, then runs the wrapped binary as a supervised child process. There is no backend: the chain is the source of truth, the wallet is the identity. The roadmap is agent-first, an autonomous buyer completing discover → pay → fetch → verify → run → resell with no human in the loop.

## Layout

Two halves, different toolchains:

| Path | What | Toolchain |
|---|---|---|
| `crates/rub3-wrapper/` | wrapper runtime, the only workspace member | `cargo`, from the repo root |
| `contracts/` | Foundry project, ERC-721 license contracts | `forge`, **from `contracts/`** |

`README.md` → "Project structure" is the per-module map. It omits two feature-gated scaffolds, `src/device.rs` (tier 4) and `src/decrypt.rs` (binary encryption), plus `tests/session_onchain_e2e.rs`; `architecture.md` → "Source layout (current)" covers those two but omits `src/identity.rs`. Between them they describe every module.

## Build and test

```bash
cargo build -p rub3-wrapper
cargo test  -p rub3-wrapper               # default bundle = tier-2
cargo test  -p rub3-wrapper -- --ignored  # adds a live Base mainnet RPC test

cd contracts && forge test                # in-process EVM: no network, no .env
```

`forge` resolves `contracts/foundry.toml`, so it must run from `contracts/`; it fails at the repo root. OpenZeppelin and forge-std are git submodules, and `forge test` clones them at the pinned revisions on first run, so `git submodule update --init --recursive` is optional.

There is no CI workflow in this repo: every check here is the contributor's job.

## Tier feature bundles (read before touching the wrapper)

`crates/rub3-wrapper/Cargo.toml` defines five tier bundles over six composable capability flags. Exactly one bundle is selected at pack time; `binary-encryption` is an orthogonal add-on that composes with tier-3+.

| Bundle | Capability flags |
|---|---|
| `tier-0` | none |
| `tier-1` | `session` |
| `tier-2` *(default)* | `session`, `onchain-read` |
| `tier-3` | `session`, `onchain-read`, `onchain-write`, `cooldown` |
| `tier-4` | `tier-3` + `device-key` |

Consequences that catch people out:

- **Whole modules are `#[cfg]`-gated out.** `src/lib.rs` is the authority. Under `tier-0` the `session`, `identity`, and `session_store` modules do not exist at all, so the set of compiled tests changes with the bundle. Code that builds and passes under the default proves nothing about `tier-0` or `tier-3`.
- **Cargo features are additive, so `--no-default-features` is mandatory.** `--features tier-0` on its own leaves the `tier-2` default enabled and silently tests tier-2 instead. Always pass `--no-default-features --features <bundle>`.
- **Run the matrix before claiming a wrapper change works.** All six must pass. The last entry is the only one that compiles `src/decrypt.rs`, since no tier bundle enables `binary-encryption`:

```bash
fail=0
for t in tier-0 tier-1 tier-2 tier-3 tier-4 tier-3,binary-encryption; do
  cargo test -p rub3-wrapper --no-default-features --features "$t" \
    && echo "$t ok" || { echo "$t FAILED"; fail=1; }
done
[ "$fail" -eq 0 ]
```

What each tier actually enforces is specified in `architecture.md` → "Security Tiers".

## Anvil-gated tests

`tests/session_onchain_e2e.rs` exercises `session::verify_onchain` against a real EVM. It is double-gated, `#![cfg(feature = "cooldown")]` at file scope plus `#[ignore]` on the test, so it does not exist under the default bundle and is skipped even under `tier-3` unless requested:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3 \
  -- --ignored session_verify_onchain_e2e
```

It spawns `anvil` on port 8547, deploys `Rub3Access` via `forge create`, and performs a real `purchase` + `activate`. It needs `anvil`, `forge`, and `cast` on `PATH`; when any is missing it prints `SKIP: …` and passes, so it never fails for a missing toolchain.

`--ignored` covers two unrelated gates: under the default bundle it instead runs `rpc::tests::owner_of_unminted_token_returns_contract_error`, which needs live Base mainnet RPC.

## Where the design authority lives

These are the spec; this file is only the map. Read the relevant one before changing behavior.

| File | Authority over |
|---|---|
| `implementation.md` | phased roadmap **and** the build record; the `[complete]` / `[partial]` / `[not started]` tags say what actually exists |
| `architecture.md` | system design: security tiers, session model, identity models, launch flows, components |
| `ideation.md` | vision, principles, what rub3 is and isn't |
| `contracts/contracts.md` | local Anvil and Base Sepolia setup, deploy env-var reference |
| `testing.md` | per-suite test inventory, manual testing, seeding a license proof |

The wrapper's app identity (`APP_ID`, `CONTRACT`, `CHAIN_ID`, `RPC_URL`) is hardcoded as placeholder constants in `src/main.rs`, pending `rub3 pack`. `CONTRACT` defaults to the zero address, which the wrapper reads as "no contract configured" and skips on-chain ownership checks, so a stock build never touches the chain. See `testing.md` → "App constants".

## Ownership invariants: a hard constraint, not a preference

"The token is the invariant; everything else is versioned." Encoded in bytecode, not policy:

- **No proxies, no upgradeable contracts.** Contract code, and therefore license terms, is frozen at deploy.
- **No revocation surface.** No burn, no admin transfer, no pause affecting `ownerOf` / `isValid` / `activate` for issued tokens. Absent from the bytecode, not merely unused.
- **Evolution changes what is *offered* going forward** (price, supply, successors, listings), **never what was *granted*** (held tokens, their validation, their renewal terms).
- **Migration is a new deploy plus an opt-in `successor` pointer**, holder-initiated. The old contract validates its own tokens forever.

An upgrade hook, an admin escape hatch, or any path that invalidates an issued token is not a missing feature; it is a design this project has deliberately ruled out. Do not propose one. Where a problem appears to require it, the sanctioned answer is a new deploy behind the successor pattern.

Full statements: `architecture.md` → "Ownership invariants (all license contracts)", `implementation.md` §2.4, `contracts/contracts.md` → "Planned contract evolution".

## Working conventions (repo owner's standing instructions)

These apply to every agent session here, on top of the project rules above.

- Never use em dashes in anything you write. Use plain dashes.
- Never add an agent or model name as a commit co-author.
- Never hand-edit `CHANGELOG.md` or any auto-generated file. Change its generator instead.
- Weight quality, simplicity, robustness, scalability, and long-term maintainability over development cost.
- Reproduce a bug end to end, the way an end user hits it, before fixing it.
- When testing end to end, be picky about UI and pixel-level correctness.
- Fix anything that clearly looks off along the way, including lint, test failures, and flakiness, even when unrelated to the task at hand.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
