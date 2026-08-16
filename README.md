# rub3

Wallet-native software licensing for the machine economy. NFT-gated access for locally executed software - CLI tools, MCP servers, desktop apps - without a browser or a backend.

rub3 lets machines (and humans) buy, verify, run, and resell software without asking anyone's permission. The NFT is the access credential - owned by a wallet, verifiable on-chain, transferrable, composable - which also makes it a liquid asset: buy a license for a workload, resell it when the job ends. The wrapper is the runtime that enforces this on the machine where the software runs.

This file owns orientation: enough to understand rub3, build it, test it, run it, and know where to go next. Everything else has a single owner. [ideation.md](ideation.md) owns vision, positioning, and the business model; [architecture.md](architecture.md) owns design rationale; [implementation.md](implementation.md) owns the roadmap and what is actually built; [contracts/contracts.md](contracts/contracts.md) owns contract operations, from deployment to the protocol fee mechanics; [testing.md](testing.md) owns the test inventory.


## How it works

1. Developer packages their binary inside the rub3 wrapper
2. Developer deploys an ERC-721 license contract on Base (`Rub3Access` or `Rub3Subscription`)
3. User launches the wrapped app - the wrapper checks for a valid cached session
4. If no session (or session expired): the wrapper opens a native activation window, verifies on-chain ownership, and requests a wallet signature
5. On success: session is cached locally, wrapped binary launches
6. On subsequent launches within TTL: session is verified locally, binary launches immediately

There is no backend. The chain is the source of truth. The wallet is the identity.

The flow above is the interactive (human) path. Agents take the same loop through the **headless** door - signer in, session out, no webview:

```bash
RUB3_AGENT_KEY=0x<hex> rub3-wrapper --headless --binary /path/to/your/app
```

One call runs `tokensOfOwner` → purchase if empty → cooldown check → `activate()` → sign the session locally → verify → persist, then launches. Documented exit codes let an orchestrator react programmatically. See [Headless activation](#headless-activation-agents) below.

## Project structure

```
rub3/
├── crates/
│   └── rub3-wrapper/                 # Wrapper runtime
│       ├── src/
│       │   ├── main.rs               # CLI entry point (clap), app constants
│       │   ├── lib.rs                # Public module re-exports (feature-gated)
│       │   ├── license.rs            # License proof schema, activation message, ECDSA verification
│       │   ├── identity.rs           # Identity models (access/account), ERC-6551 TBA derivation
│       │   ├── store.rs              # Proof persistence (~/.rub3/licenses/ or RUB3_LICENSE_DIR)
│       │   ├── activation.rs         # Activation orchestration: fast paths, `ensure` (webview), `ensure_headless` (agent), exit codes
│       │   ├── agent_env.rs          # Names of the `RUB3_AGENT_*` credential vars: read by `signer`, stripped by `supervisor`
│       │   ├── signer.rs             # `Signer` trait + `LocalSigner` - the only holder of raw key material (feature `headless`)
│       │   ├── tx.rs                 # EIP-1559 build / sign / broadcast for headless (feature `headless`)
│       │   ├── rpc.rs                # On-chain queries (ownerOf, price, tokensOfOwner, chainId, getCode) via alloy
│       │   ├── attest.rs             # Pre-purchase contract attestation: masked code hash vs the pinned canonical table (feature `onchain-read`, tier 2+)
│       │   ├── webview.rs            # Native activation window (wry/tao), IPC message handling (feature `webview`)
│       │   ├── supervisor.rs         # Child process lifecycle, SIGTERM forwarding, `RUB3_AGENT_*` stripped from the child
│       │   ├── session.rs            # Session schema, message hash, verify_local, is_expired
│       │   ├── session_store.rs      # Session persistence, load_latest_session
│       │   ├── device.rs             # Device keypair scaffold (feature `device-key`, tier 4 - deferred)
│       │   └── decrypt.rs            # Binary decryption scaffold (feature `binary-encryption` - deferred)
│       ├── assets/
│       │   └── activation.html       # Activation UI (address input, token select, signature)
│       └── tests/
│           ├── helpers/mod.rs        # Shared test utilities (wallet gen, signing, license creation)
│           ├── integration.rs        # Wrapper binary tests (exit codes, args, missing binary)
│           ├── license_e2e.rs        # License verification tests (static + dynamic wallets, SIGTERM)
│           ├── session_onchain_e2e.rs # Anvil-gated: verify_onchain against a live chain
│           └── headless_e2e.rs       # Anvil-gated: fresh key → purchase → activate → persist → fast path
├── contracts/                        # Foundry project - ERC-721 license contracts
│   ├── src/
│   │   ├── Rub3License.sol           # Abstract base (ERC-721 + Enumerable + Ownable)
│   │   ├── Rub3Access.sol            # One-time purchase license
│   │   ├── Rub3Subscription.sol      # Time-bounded license (expiresAt, renew, isValid)
│   │   └── Rub3Factory.sol           # §2.3 - fee-stamping deploys + isDeployed, and its two deployer helpers
│   ├── test/
│   │   ├── Rub3Access.t.sol
│   │   ├── Rub3Subscription.t.sol
│   │   ├── Rub3Invariants.t.sol      # Ownership invariants (§2.4) + no-revocation bytecode audit
│   │   ├── Rub3TokenPurchase.t.sol   # Stablecoin rail (§2.2): EIP-3009 authorization, replay, front-running
│   │   ├── Rub3Factory.t.sol         # §2.3: fee immutability, exact split on both rails, direct deploys
│   │   └── mocks/
│   │       └── MockEIP3009Token.sol  # Faithful EIP-3009 stand-in for USDC, plus its negative fixtures
│   ├── script/
│   │   ├── Deploy.s.sol              # Deploy either contract to any EVM chain, directly or via FACTORY
│   │   └── DeployFactory.s.sol       # Deploy a Rub3Factory (FEE_BPS + TREASURY required; PREVIOUS_FACTORY optional)
│   ├── foundry.toml
│   ├── remappings.txt                # Import remappings pinned in-tree (a reproducibility input)
│   ├── foundry.lock                  # Pinned dependency revisions, mirrors the submodule gitlinks
│   ├── canonical-bytecode.json       # Expected sha256 of each contract's deployedBytecode + immutable ranges
│   ├── .env.example
│   └── contracts.md                  # Setup guide (Anvil, Base Sepolia) + reproducible builds
├── licenses/
│   └── com.rub3.example.json         # Example license proof with valid signature
├── scripts/
│   ├── canonical-bytecode-hashes.sh  # check/update/print the canonical fingerprints
│   ├── seed-license.sh               # Generate a signed license proof for local testing
│   └── test-e2e.sh                   # Convenience script - runs cargo test
├── architecture.md                   # System design, session model, security tiers
├── implementation.md                 # Phased development plan with status
├── ideation.md                       # Project vision and design principles
├── testing.md                        # Test inventory and manual testing guide
└── README.md                         # This file: orientation, build, test, run
```

## Rust dependencies

| Crate | Purpose |
|---|---|
| `clap` | CLI argument parsing |
| `k256` | secp256k1 ECDSA signature recovery |
| `sha2` | SHA-256 for activation message + session message |
| `sha3` | Keccak-256 for Ethereum address derivation + personal_sign |
| `hex` | Hex encoding/decoding |
| `alloy` | Ethereum JSON-RPC (ownerOf, tokensOfOwner, price) |
| `wry` | Embedded webview for activation UI (feature `webview`) |
| `tao` | Native window/event loop (feature `webview`) |
| `zeroize` | Wiping decoded key bytes / keystore passwords (feature `headless`) |
| `serde` / `serde_json` | Proof and session serialization |
| `dirs` | Platform data directory resolution |
| `chrono` | RFC-3339 timestamps, session TTL |
| `rand` | Nonce generation (feature = `session`) |
| `nix` / `libc` | Unix signal handling (SIGTERM forwarding) |

Dev dependencies: `rand`, `tempfile`.

## Building

```bash
cargo build -p rub3-wrapper                                              # default: tier-2 + webview
```

A build picks one **tier bundle** (`tier-0`…`tier-4`) and at least one **front
door**. Front doors are independent features and compose:

| Feature | Pulls | Use |
|---|---|---|
| `webview` | `wry`, `tao` | Native activation window - the human path |
| `headless` | no GUI deps at all | Signer in, session out - the agent path |

```bash
cargo build -p rub3-wrapper --no-default-features --features tier-3,webview    # human
cargo build -p rub3-wrapper --no-default-features --features tier-3,headless   # agent
cargo build -p rub3-wrapper --no-default-features --features tier-3,webview,headless  # both
```

A `headless` build links neither `wry` nor `tao`, nor any of the GUI crates the
webview build pulls - no WebKit, no AppKit, no ObjC runtime. Verify with:

```bash
cargo tree -p rub3-wrapper --no-default-features --features tier-3,headless | grep -E 'wry|tao'
```

## Testing

### Rust

```bash
# All tests (unit + integration + license e2e)
cargo test -p rub3-wrapper

# Tier-3 and the headless (agent) path
cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless --lib

# Only the network-dependent tests (--ignored filters out the suite above)
cargo test -p rub3-wrapper -- --ignored
```

**Unit tests** (`src/`): `license`, `store`, `rpc`, `session`, `session_store`, `identity`, `activation`, `signer`, `tx`

**Integration tests** (`tests/`): wrapper binary exit codes, argument passing, SIGTERM forwarding, static + dynamic license E2E

**Anvil-gated E2E** - needs the Foundry toolchain (`anvil`, `forge`, `cast`) on
PATH; each test prints `SKIP:` and passes when it is missing:

```bash
# On-chain session re-verification
cargo test -p rub3-wrapper --no-default-features --features tier-3 \
    -- --ignored session_verify_onchain_e2e

# Headless: fresh key → funded → purchase → activate → persist → fast path
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless \
    -- --ignored headless
```

### Contracts

```bash
cd contracts
forge test
```

See [contracts/contracts.md](contracts/contracts.md) for local Anvil setup, Base Sepolia deployment, and the reproducible-build contract behind the canonical bytecode fingerprints.

## Running the wrapper

On first run with no cached proof, the wrapper opens an activation window:

```bash
cargo run -p rub3-wrapper -- --binary /path/to/your/app
```

To skip activation during development, seed a valid license proof:

```bash
./scripts/seed-license.sh

RUB3_LICENSE_DIR=/tmp/rub3-test cargo run -p rub3-wrapper -- --binary /path/to/your/app
```

## Headless activation (agents)

`--headless` runs the whole activation pipeline with no window and no human:
enumerate the signer's tokens, purchase one if it holds none (verifying the
contract's code first - see [Contract code check](#contract-code-check)), check
the cooldown, send `activate()`, sign the session message locally, verify it,
and persist it - then launch the wrapped binary.

```bash
RUB3_AGENT_KEY=0x<64 hex chars>   rub3-wrapper --headless --binary /path/to/your/app

# Activate one specific token instead of letting the flow choose:
rub3-wrapper --headless --token-id 3 --binary /path/to/your/app
```

`--headless` requires a build with the `headless` feature; other builds exit 18.

**`rub3-wrapper --help` owns the exit-code and signer-source contract.** The
binary prints both tables below, with the reasoning that matters at the call
site: why the spend ceiling starts unavailable rather than unlimited, and why
code 21 is deliberately not code 14. The tables are reproduced here for
discoverability.

Two properties to know before reading them. Every `RUB3_AGENT_*` credential
variable is removed from the wrapped binary's environment before launch -
unconditionally, on every build, with no flag to pass them through. That is
containment, not a sandbox: the child runs as the same UID and can read anything
that user can. And for KMS-, HSM-, or enclave-backed keys, implementing the
`Signer` trait and passing it to `activation::ensure_headless` keeps key
material out of the wrapper's process entirely, since its only primitive is
"sign this 32-byte digest" and `signer::LocalSigner` is the one type in the
crate that ever holds a raw key.

### Signer sources

Highest precedence first.

| Variable | Meaning |
|---|---|
| `RUB3_AGENT_KEY` | Raw hex private key. **Dev / CI only** - an env var is readable by anything sharing the process environment |
| `RUB3_AGENT_KEYSTORE` | Path to an encrypted Web3 Secret Storage (V3) keystore. Defaults to `~/.rub3/agent-key.json` when that file exists |
| `RUB3_AGENT_KEYSTORE_PASSWORD_FILE` | File holding the keystore password - preferred, because a file can be mode 0600 |
| `RUB3_AGENT_KEYSTORE_PASSWORD` | Keystore password, inline |

### Spend policy

| Variable | Meaning |
|---|---|
| `RUB3_AGENT_MAX_TOKEN_AMOUNT` | The most this agent may authorize on a contract's stablecoin rail, an integer in that payment token's own smallest unit (USDC has 6 decimals, so 5 USDC is `5000000`) |

Policy, not a credential, so unlike the signer sources above it is **not**
stripped from the wrapped binary's environment. There is no default: until it is
set the stablecoin rail is unavailable rather than unlimited, and the wrapper
falls back to ETH and prints why. A malformed value is a hard error.

### Contract code check

Before it buys, the wrapper asks whether the contract is the code it thinks it
is. It reads the deployed runtime code once, zeroes the immutable byte ranges,
hashes the result, and compares that against a table of canonical fingerprints
compiled into the binary. A match costs no extra network round trip and is the
common case; a miss refuses the purchase with exit code 23, before anything is
signed and with no transaction sent.

Masking the immutables is what makes the comparison work at all: they hold the
constructor's arguments, so two legitimate deploys of identical code that chose
a different `supplyCap` return different bytes. Every masked range is a `PUSH32`
immediate and no control flow can reach one as an instruction, so a match is a
complete statement about the contract's **executable code**.

It is not a complete statement about anything else, and the wrapper does not
claim to be one:

- It says nothing about the masked values themselves. Canonical code pointed at
  a hostile ERC-6551 implementation matches. Read `identityModel()`,
  `tbaImplementation()`, `supplyCap()`, `cooldownBlocks()`, `predecessor()`,
  `feeBps()` and `treasury()` and check them against your own policy.
- It says nothing about how a contract owner will use the powers the invariants
  deliberately keep (`setPrice`, `setSuccessor`, `revokeWrapperHash`,
  `withdraw`).
- It rests on the RPC endpoint answering honestly. The claim it supports is "an
  honest view of chain state implies canonical code", and no stronger one.
- A miss is not an accusation. A contract deployed from a newer release of the
  templates than this build was packed with looks the same way.

**It gates purchases, never launches.** A launch is a program you have already
paid for; refusing to start it because a check could not complete would be a
revocation surface, which this project has ruled out. The two are different code
paths with different defaults, and nothing on the launch path consults the
check.

Fingerprints and immutable ranges are published in
`contracts/canonical-bytecode.json`, a blocking CI job keeps them honest against
the contracts, and a unit test keeps the wrapper's pinned table honest against
them. `contracts/contracts.md` -> "Auditing the invariants before buying" has
the manual form of the same comparison.

### Exit codes

Stable and machine-readable, so an orchestrator branches on the code instead of
parsing stderr. Also printed by `rub3-wrapper --help`.

These codes are emitted only when headless activation itself fails. Once the
wrapped binary launches, its own exit status is passed through unchanged, so a
code in this range coming from a launched child is the child's status and not an
activation failure.

| Code | Meaning | What an orchestrator should do |
|---|---|---|
| 0 | Success - session valid, binary launched | - |
| 1 | Unclassified failure | Inspect stderr |
| 2 | Command-line usage error (clap) | Fix the invocation |
| 10 | No usable signer | Configure `RUB3_AGENT_KEY` or a keystore |
| 11 | Insufficient funds for purchase + gas | Top up the wallet |
| 12 | No token held and supply sold out | Terminal - try another contract |
| 13 | Cooldown active | Back off `blocks_remaining` blocks, then retry |
| 14 | `activate()` failed (reverted, or not confirmed in time) | Retry - a re-run re-reads ownership, so a purchase this run already completed is activated, not paid for twice |
| 15 | Session verification failed | Signer/config bug - do not retry blindly |
| 16 | Chain RPC / transport failure | Retry, or switch endpoint |
| 17 | Session could not be persisted | Check the session dir is writable |
| 18 | Headless not compiled into this build | Use a `headless` build |
| 19 | Chain id mismatch between endpoint and build | Fix `RPC_URL` |
| 20 | `--token-id` names a token this signer does not hold | Fix the id, or drop the flag to purchase |
| 21 | Purchase broadcast but not confirmed - timed out, or the receipt query kept failing | Do not retry blindly - resolve the `tx_hash` on the detail line, then re-run once it has mined or been dropped |
| 22 | The listed price is above the configured spend ceiling. The ceiling is weighed before anything is signed, so the rail was not exercised and this is no evidence it is otherwise usable | Terminal - raise `RUB3_AGENT_MAX_TOKEN_AMOUNT` if the price is acceptable, or do not buy |
| 23 | The contract's deployed code is not canonical rub3 code. Checked before anything is signed, so no transaction was sent and nothing was spent | Terminal - the same address holds the same code. Verify the address, or use a build packed with the release that contract came from |

Failures with structured parameters also print one parseable line, carrying only
parameters the wrapper actually measured:

```
error: cooldown active on token 0: retry in 12 blocks
rub3-detail: token_id=0 blocks_remaining=12
```

Code 11's detail line carries `required_wei` and `required_covers`, which says
what that figure includes:

| `required_covers` | `required_wei` is | Top up |
|---|---|---|
| `price_plus_gas` | price + `gas_limit * max_fee_per_gas`, the full ceiling | that amount |
| `price` | the purchase price alone, measured before gas could be estimated | that amount plus gas |

`RUB3_SESSION_DIR` overrides where sessions are cached (default
`~/.rub3/sessions/<app_id>/<token_id>.json`) - useful for containers with a
mounted volume.

## Current status

Shipped capabilities, one line each. [implementation.md](implementation.md) is
the authority on what each covers, what it cost, and what comes next.

- **Wrapper runtime** - process supervision, SIGTERM forwarding, `RUB3_AGENT_*` stripped from the child's environment
- **Sessions** - schema, local sign and verify, TTL, per-token persistence, and tier-3 on-chain re-verification on a sampled fraction of cold starts
- **Interactive front door** - native activation window: address input, token enumeration and selection, purchase and cooldown screens, signature paste
- **Headless front door (§2.1)** - `--headless` runs enumerate → purchase → activate → sign → verify → persist → launch in one call, with stable exit codes and a `Signer` trait for KMS- or enclave-backed keys
- **On-chain reads** - `ownerOf`, price on both rails, supply, enumeration, cooldown, session id, receipt polling, via alloy
- **Identity models** - `access` and `account`, with local ERC-6551 TBA derivation signed into the session preimage
- **Contracts** - `Rub3Access` and `Rub3Subscription` (ERC-721 + Enumerable, purchase, renew, `isValid`, tier-3 `activate` + cooldown), 185 forge tests
- **Stablecoin rail (§2.2)** - USDC purchases and renewals through EIP-3009 authorizations anyone may submit, including from EIP-1271 smart-contract wallets, so an agent holding no ETH can still buy the price
- **`Rub3Factory` + protocol fee (§2.3)** - immutable fee terms stamped into every canonical deploy and recorded in `isDeployed`; direct deploys stay fee-free and unrecorded. The registry and marketplace the row is for are not built, and the factory and the registry launch together: nothing reaches mainnet or is declared ready before then
- **Ownership invariants (§2.4)** - append-only wrapper hash set with on-chain revocation reasons, opt-in successor pointer with holder-initiated `claimFromPredecessor`, the contract-side `honorsContract` trust rule, per-token renewal snapshots, and a no-revocation bytecode audit over four deployed contracts
- **Reproducible builds** - canonical bytecode fingerprints for five contracts, gated in CI
- **Pre-purchase contract attestation (§2.6)** - before an agent spends, the wrapper compares the contract's masked code hash against fingerprints pinned in the binary and refuses on a miss (exit 23), catching a modified copy that a selector-name scan passes in silence. It gates purchases only, never launches
- **Deploy scripts** - `forge script` deploys either licence contract to any EVM chain from env vars, directly or through a factory; `DeployFactory.s.sol` deploys the factory itself

**Not yet implemented (agent-first roadmap):** wrapper support for the `honorsContract` trust rule (the contract exposes and tests it; no shipped wrapper calls it, so a holder who claims onto a successor is not yet honored at launch), CLI tooling (`pack` / `deploy` / `fetch` / `register`), content-addressed distribution, registry with ERC-8004-style agent cards, concurrent-seat licensing, SDK, metered billing, marketplace. Human-surface polish (WalletConnect tabs, auto-detect, Preact refactor, Tauri plugin) is demoted behind the agent path; tier-4 device binding and binary encryption are deferred.

## Direction

The plan is agent-first (July 2026 revision - see [implementation.md](implementation.md)):

- **Let machines buy, verify, and resell software without asking anyone's permission.** The adoption unit is one closed loop an agent completes end to end: discover → pay → fetch → verify → run → resell.
- **Open-source the rails; own the factory, registry, and marketplace.** Revenue is intended to be a 2–3% fee on a payment flow only the wrapper can meter, priced low enough that routing around it is not worth the trouble. Only the factory and its on-chain fee split are built; the registry and marketplace are not, and the contracts are not deployed to mainnet or declared ready for use until the registry is ready. No token.
- **The token is the invariant; everything else is versioned.** Evolution only ever changes what is offered going forward (price, successor contracts, registry listings), never what was granted (held tokens, their validation, their renewal terms). No proxies, no revocation surface - structurally, not by promise.

First target market: wallet-gated MCP servers - paid MCP servers have no licensing primitive today, and agents are their natural customers.

## Design documents

- [ideation.md](ideation.md) - project vision, design principles, what rub3 is and isn't
- [architecture.md](architecture.md) - system design, session model, security tiers, components
- [implementation.md](implementation.md) - phased development plan with current status
- [contracts/contracts.md](contracts/contracts.md) - contract setup, local testing, deployment
- [testing.md](testing.md) - manual testing guide
