# rub3

Wallet-native licensing for locally executed software (CLI tools, MCP servers, desktop apps). An ERC-721 token on Base *is* the access credential; the `rub3-wrapper` Rust runtime verifies ownership on the machine at launch, then runs the wrapped binary as a supervised child process. There is no backend: the chain is the source of truth, the wallet is the identity. The roadmap is agent-first, an autonomous buyer completing discover → pay → fetch → verify → run → resell with no human in the loop.

## Layout

Five parts, two toolchains:

| Path | What | Toolchain |
|---|---|---|
| `crates/rub3-wrapper/` | wrapper runtime, the crate that ships | `cargo`, from the repo root |
| `crates/rub3-cli/` | the `rub3` command (§2.5): `pack` and `deploy`. Package `rub3-cli`, binary `rub3`; off the wrapper's dependency path | `cargo`, from the repo root |
| `crates/rub3-sdk/` | the `rub3` SDK crate a wrapped application links (§3.5). Directory and package names differ on purpose, so cargo commands are `-p rub3` | `cargo`, from the repo root |
| `crates/rub3-docs-mcp/` | developer-facing docs MCP server (§3.3); off the wrapper's dependency path | `cargo`, from the repo root |
| `contracts/` | Foundry project, ERC-721 license contracts | `forge`, **from `contracts/`** |

`README.md` → "Project structure" is the per-module map, and it is the only one: it covers every module, including the two deferred feature-gated scaffolds `src/device.rs` and `src/decrypt.rs`.

## Build and test

```bash
cargo build -p rub3-wrapper
cargo test  -p rub3-wrapper               # default bundle = tier-2
cargo test  -p rub3-wrapper -- --ignored  # runs ONLY the ignored tests: a live Base mainnet RPC test
cargo test  -p rub3                       # the SDK crate, including its doctests
cargo test  -p rub3-docs-mcp              # docs server + the docs legibility gate
cargo test  -p rub3-cli                   # the CLI; add `--test pack_build_gate -- --ignored` for the build gate

cd contracts && forge test                # in-process EVM: no network, no .env
cd contracts && forge fmt --check         # blocking style gate; `forge fmt` is the fix
```

`forge` resolves `contracts/foundry.toml`, so it must run from `contracts/`; it fails at the repo root. That file also carries a tuned `[fmt]` section whose non-default settings are justified in comments beside them, so **do not reformat Solidity under stock defaults**: see `contracts/contracts.md` -> "Formatting" for the settings, the single `forgefmt: disable-next-item` in the tree and why it is there, and the fact that `forge fmt` needs two passes to converge on a tree it has not formatted before. OpenZeppelin and forge-std are git submodules, and `forge test` clones them at the pinned revisions on first run, so `git submodule update --init --recursive` is optional.

`.github/workflows/ci.yml` runs on every PR and on pushes to `main`: the wrapper matrix for `tier-0` through `tier-4` plus the `headless`, `webview` and `sdk` entries, the SDK crate's own job, the CLI's own job (its suites, the `#[ignore]`d build-gate tests, and the `cargo tree` check that keeps it off the wrapper's dependency path), `forge test` preceded by `scripts/check-deployments.sh` (the schema gate on `contracts/deployments.json`, the per-chain record of the canonical factory and the code registry), the blocking canonical-bytecode-fingerprint job (see `contracts/contracts.md` -> "Reproducible builds and canonical fingerprints"), the anvil-gated e2e, the docs-surface job (`forge build`, then `cargo test -p rub3-docs-mcp` with `RUB3_DOCS_MCP_REQUIRE_ARTIFACTS=1`), and a lint job where `cargo clippy -- -D warnings`, `cargo fmt --check` and `forge fmt --check` are all blocking gates. Read it for the exact invocations. It is macOS-only, and it does **not** build `tier-3,binary-encryption`, the only bundle that compiles `src/decrypt.rs`, so running the full local matrix below is still the contributor's job.

**A contract change is also a wrapper change.** `crates/rub3-wrapper/src/attest.rs` pins a copy of the canonical fingerprints and immutable ranges so the wrapper can verify a contract before buying from it (`implementation.md` §2.6), and a copy of `contracts/deployments.json`'s per-chain `code_registry` (§2.9), since a binary cannot read a file at runtime. Unit tests fail when either copy drifts. Touch anything under `contracts/src/` and the sequence is: `scripts/canonical-bytecode-hashes.sh update`, then **add** a row to `attest::CANONICAL` for each moved fingerprint - never overwrite or drop one once the contract it describes is deployed, because a deployed contract goes on validating its own tokens forever. That accumulate-only rule switches on at the first deploy: nothing is deployed to any public network yet, so the table holds exactly one row per contract today (`attest::CANONICAL`'s doc comment and `implementation.md` §2.6 carry the reasoning). A unit test in `attest` fails until you do, in every tier-2-and-up matrix job.

**`contracts/src/` is the publishing boundary, so a test-only contract must not live there.** The fingerprint script derives its deployable set from every artifact whose `compilationTarget` sits under the resolved source directory, which means anything added under `contracts/src/` is published as canonical rub3 code and then *required* to have a row in `attest::CANONICAL`. For a deliberately non-canonical fixture, that requirement inverts the check into a security regression with a green CI run. Fixtures go in `contracts/test/mocks/`; `NonCanonicalRub3Access.sol` is the worked example and its header explains the trap.

**Two contracts have "registry" in the name and they are not related.** `Rub3CodeRegistry` (§2.9) is the append-only version authority: "is this bytecode a genuine rub3 release", keyed by masked code hash, read by the wrapper on the purchase path when its pinned table missed. `Rub3Registry` (§3.2) is the discovery registry: "which apps exist and which are listable", keyed by licence contract address, read by a shopper before it has an address to verify. Neither is evidence for the other's question. They carry different `Rub3CodeRegistry.Role` values, and `test_neitherRegistryCanStandInForTheOther` asserts neither one's bytecode carries the other's selectors, so a change that blurs them fails rather than merges. Adding a `Role` variant moves `Rub3CodeRegistry`'s own fingerprint, because the variant count is solc's enum decoder bound.

## Tier feature bundles (read before touching the wrapper)

`crates/rub3-wrapper/Cargo.toml` defines five tier bundles over composable capability flags, plus two front-door features. A build selects exactly one tier bundle at pack time AND at least one front door: `webview` (native activation window, pulls `wry`/`tao`) or `headless` (signer in, session out, no GUI dependency at all). Tier bundles name no front door, so `--no-default-features --features tier-3` alone builds a binary whose interactive activation always fails with `NoInteractiveFrontDoor`. Two orthogonal add-ons compose with any tier and are named by no bundle: `binary-encryption` (tier-3+ only) and `sdk`, which serves the SDK channel to the wrapped application - what an application may ask about itself does not depend on how hard the launch was gated, and tier-0 gets heartbeats with no session to report.

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
- **Run the matrix before claiming a wrapper change works.** All eleven must pass. `tier-3,binary-encryption` is the only entry that compiles `src/decrypt.rs` and the two `sdk` entries the only ones that compile `src/sdk.rs`, since no tier bundle enables either; `tier-2,webview`, `tier-3,webview` and `tier-3,headless` are the only ones that compile a front door. None of the pairs is redundant: the window's purchase screen and its pre-purchase attestation are gated on `onchain-write`, so only `tier-3,webview` compiles them, and the SDK channel's session projection is gated on `session`, so only `tier-3,sdk` compiles it while `tier-0,sdk` proves the heartbeat-only build.

```bash
fail=0
for t in tier-0 tier-1 tier-2 tier-3 tier-4 tier-3,binary-encryption \
         tier-2,webview tier-3,webview tier-3,headless tier-0,sdk tier-3,sdk; do
  cargo test -p rub3-wrapper --no-default-features --features "$t" \
    && echo "$t ok" || { echo "$t FAILED"; fail=1; }
done
cargo test -p rub3 || fail=1
[ "$fail" -eq 0 ]
```

**Clippy needs the same treatment, and the `sdk` feature is the live example.** `cargo clippy --workspace --all-targets` builds the default bundle, which compiles neither `src/sdk.rs` nor `tests/sdk_e2e.rs`; three real findings in them survived a clean workspace run. CI lints `tier-3,webview` and `tier-3,sdk` on top of the default for that reason, and a bundle a new module only compiles under needs its own invocation added there.

What each tier actually enforces is specified in `architecture.md` → "Security Tiers".

## Anvil-gated tests

`tests/session_onchain_e2e.rs` exercises `session::verify_onchain` against a real EVM. It is double-gated, `#![cfg(feature = "cooldown")]` at file scope plus `#[ignore]` on the test, so it does not exist under the default bundle and is skipped even under `tier-3` unless requested:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3 \
  -- --ignored session_verify_onchain_e2e
```

It spawns `anvil` on port 8547, deploys `Rub3Access` via `forge create`, and performs a real `purchase` + `activate`. It needs `anvil`, `forge`, and `cast` on `PATH`; when any is missing it prints `SKIP: …` and passes, so it never fails for a missing toolchain.

`tests/headless_e2e.rs` is the same shape for the agent front door, gated on `headless` instead, on port 8549 so both can run at once, and self-serialising so it needs no `--test-threads=1`:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless \
  -- --ignored headless
```

The webview front door's §1.8 flows (`src/webview/session_flow.rs`) are the same shape again on port 8551, but they are **lib tests, not an integration suite**: the seam they drive, `webview::IpcState`, is private to `src/webview.rs`, so no `tests/` binary can reach it. Select them with `--lib` plus the module path:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3,webview \
    --lib -- --ignored webview::session_flow
```

`tests/code_registry_e2e.rs` is the fourth, gated on `onchain-read` alone and on port 8553. It deploys `Rub3CodeRegistry` and reads a published release back through the wrapper's own ABI mirror, which is the only place a drifted `sol!` interface or a pinned fingerprint that no real deploy produces can be caught:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-2 \
  --test code_registry_e2e -- --ignored code_registry
```

All four self-skip when Foundry is missing, so **a pass in 0.00s is a skip, not a green run.**

`--ignored` covers unrelated gates: under the default bundle it instead runs `rpc::tests::owner_of_unminted_token_returns_contract_error`, which needs live Base mainnet RPC, and the module-path filter above is what keeps that test out of the webview run.

## Where the design authority lives

These are the spec; this file is only the map. Read the relevant one before changing behavior.

| File | Authority over |
|---|---|
| `implementation.md` | phased roadmap **and** the build record; the `[complete]` / `[partial]` / `[not started]` tags say what actually exists |
| `architecture.md` | system design: security tiers, session model, identity models, launch flows, components |
| `ideation.md` | vision, principles, what rub3 is and isn't |
| `contracts/contracts.md` | local Anvil and Base Sepolia setup, deploy env-var reference, the EIP-3009 purchase recipe, the pre-purchase audit |
| `testing.md` | per-suite test inventory, manual testing, seeding a license proof, and the three-tier on-chain test plan (local anvil, unrecorded Base Sepolia scratch, the one recorded canonical set) |

The wrapper's app identity (`APP_ID`, `CONTRACT`, `CHAIN_ID`, `RPC_URL`, `FACTORY`) lives in `src/packed.rs`: a placeholder an ordinary `cargo build` compiles, or the value `rub3 pack` injects through a `RUB3_PACK_*` variable and `build.rs` validates. `CONTRACT` defaults to the zero address, which the wrapper reads as "no contract configured" and skips on-chain ownership checks, so a stock build never touches the chain. `FACTORY` is the one constant with no placeholder: it comes out of `contracts/deployments.json` at pack time, and a `null` entry there is a hard error rather than a fallback, at the CLI *and* again in `build.rs`. Never add a fallback; see `implementation.md` §2.5. `rub3-wrapper --version` prints the whole packed identity. See `testing.md` → "App constants".

## The docs surface serves derived facts only

`crates/rub3-docs-mcp` is the docs MCP server of `implementation.md` §3.3, plus the machine-legibility gate over the documents. **Nothing it serves may be transcribed.** Contract ABIs come from the artifacts `forge build` wrote, selectors from the artifact's own `methodIdentifiers` map, and Rust signatures are byte ranges of the source file rather than re-renderings, so a served signature that is not in the file is a test failure. A fact that cannot be derived is refused with the command that would produce it: `contracts/out/` is not checked in, so the contract tools ask for `forge build` rather than answering. This is the same line `scripts/canonical-bytecode-hashes.sh` and `attest`'s manifest mirror test hold, and a docs server is where a second hand-maintained copy would rot fastest, because a wrong signature surfaces only when an agent has already written calldata against it.

Two consequences for unrelated changes. `tests/docs_legibility.rs` turns red on an untagged code fence, a cross-reference to a heading that no longer exists, a document with no purpose statement under its title, an em dash, and a new Markdown document that `llms.txt` does not link - each failure names the file and the fix. And the crate must stay off the wrapper's dependency path: `cargo tree -p rub3-wrapper` is the check.

## Ownership invariants: a hard constraint, not a preference

"The token is the invariant; everything else is versioned." The prohibitions are enforced by absence from the bytecode, not by policy; the paths that remain open are specified in the docs cited at the end of this section:

- **No proxies, no upgradeable contracts.** Contract code, and therefore license terms, is frozen at deploy.
- **No revocation surface.** No burn, no admin transfer, no pause affecting `ownerOf` / `honorsContract` / `activate` for issued tokens. Absent from the bytecode, not merely unused.
- **Evolution changes what is *offered* going forward** (price, supply, successors, listings), **never what was *granted*** (held tokens and their validation).
- **Migration is a new deploy plus an opt-in `successor` pointer**, holder-initiated: the old contract validates its own tokens forever. This is the sanctioned migration path as specified; see `implementation.md` §2.4 for what is built.

An upgrade hook, an admin escape hatch, or any path that invalidates an issued token is not a missing feature; it is a design this project has deliberately ruled out. Do not propose one. Where a problem appears to require it, the sanctioned answer is a new deploy behind the successor pattern.

**Two size limits bound the contracts, and both have bitten.** A licence constructor's argument count runs into solc's stack limit in the ABI decoder: `SaleTerms` (§2.2), `FeeTerms` and `IdentityTerms` (§2.3) each exist because one more loose parameter failed the build with "stack too deep". Group a related pair into a struct rather than reaching for `viaIR`, which would move every canonical fingerprint. Separately, `Rub3Factory` carries `Rub3Access`'s creation code in its *initcode* (it builds `Rub3AccessDeployer` in its constructor); `test_factory_initcodeFitsUnderEip3860` fails before an undeployable factory reaches a chain, and `test_factory_runtimeFitsUnderTheCodeSizeLimit` is why the deployer helper is a separate contract at all.

**rub3 sells one licence model.** `Rub3Access`, bought once and valid while held; there is no expiry, no renewal, and no `isValid`, so `ownerOf` is validity and `honorsContract` is the read that spans a succession. A second, time-bounded `Rub3Subscription` was built and then removed before any deploy - `implementation.md` §2.10 is the removal record and says what was deliberately kept (the grouped constructor structs, the deployer-helper split, the abstract base). Do not reintroduce a second model, or a licence-model flag on anything, without going back to that section first.

**Adding an external function to a license contract or to `Rub3Factory` has a checklist.** `test/Rub3Invariants.t.sol` asserts a fixed list of forbidden signatures is absent from the runtime bytecode of all three audited targets (the two licence deploys, plus `Rub3Factory` since §2.3), and the same list is written out in four more places: the `string[N]` array itself, the copy-pasteable loop in `contracts/contracts.md`, the bytecode table in `architecture.md`, and `attest::FORBIDDEN_SIGNATURES` in the wrapper, with the count stated in each plus `implementation.md` §2.4 and §3.4. The wrapper copy is checked against the Solidity one by a unit test, so that one fails loudly rather than rotting. If the new function introduces state that must not be rewritten later, add the setter names that *would* rewrite it and sweep every count in the same pass - the list has churned repeatedly and a stale count is the usual casualty.

**A seat is a session, and its cooldown stamp is never cleared.** A licence grants `seatsPerToken` concurrent sessions (§3.4). `release(tokenId, sessionId)` and a TTL lapse free a seat's *occupancy*; both leave its `activatedAt` exactly where it was, which is the whole of the churn defence - a token lands at most `seatsPerToken` activations per `cooldownBlocks` window whatever anybody releases. Clearing the stamp on release is an unlimited churn bypass that every other seat test still passes; the four `churnDefence` tests in `contracts/test/Rub3Seats.t.sol` are what catch it, and they were verified red against that mutation. **`seatsPerToken == 1` is a deliberate special case**: the one seat is always its holder's to retake, so a single-seat licence is exactly the tier-3 licence and a sole holder is never locked out by a lost session record. The rule stops at one seat on purpose, and `activationStatus.fleetExhausted` is the contract's own answer rather than `seatsInUse == seats`, because at one seat those counts say "full" about a seat that is not.

Full statements: `architecture.md` → "Ownership invariants (all license contracts)", `implementation.md` §2.4 and §3.4, `contracts/contracts.md` → "Planned contract evolution".

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
