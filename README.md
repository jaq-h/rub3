# rub3

Wallet-native software licensing for the machine economy. NFT-gated access for locally executed software - CLI tools, MCP servers, desktop apps - without a browser or a backend.

rub3 lets machines (and humans) buy, verify, run, and resell software without asking anyone's permission. The NFT is the access credential - owned by a wallet, verifiable on-chain, transferrable, composable - which also makes it a liquid asset: buy a license for a workload, resell it when the job ends. The wrapper is the runtime that enforces this on the machine where the software runs.


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
│       │   ├── rpc.rs                # On-chain queries (ownerOf, price, tokensOfOwner, chainId) via alloy
│       │   ├── webview.rs            # Native activation window (wry/tao), IPC message handling (feature `webview`)
│       │   ├── supervisor.rs         # Child process lifecycle, SIGTERM forwarding, `RUB3_AGENT_*` stripped from the child
│       │   ├── session.rs            # Session schema, message hash, verify_local, is_expired
│       │   └── session_store.rs      # Session persistence, load_latest_session
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
└── testing.md                        # Manual testing guide
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
enumerate the signer's tokens, purchase one if it holds none, check the
cooldown, send `activate()`, sign the session message locally, verify it, and
persist it - then launch the wrapped binary.

```bash
RUB3_AGENT_KEY=0x<64 hex chars>   rub3-wrapper --headless --binary /path/to/your/app

# Activate one specific token instead of letting the flow choose:
rub3-wrapper --headless --token-id 3 --binary /path/to/your/app
```

`--headless` requires a build with the `headless` feature; other builds exit 18.

Before launching the wrapped binary the wrapper removes every `RUB3_AGENT_*`
credential variable (`RUB3_AGENT_KEY`, `RUB3_AGENT_KEYSTORE`,
`RUB3_AGENT_KEYSTORE_PASSWORD`, `RUB3_AGENT_KEYSTORE_PASSWORD_FILE`) from the
child's environment, so it does not hand the licensed product the agent
credential or the location of one. This is unconditional: there is no flag to
pass them through, and it applies to every build, not only headless ones.

That is containment, not a sandbox. The child runs as the same UID as the
wrapper and can read any file that user can read, including the default
keystore path `~/.rub3/agent-key.json`.

### Signer sources

Highest precedence first. A malformed `RUB3_AGENT_KEY` is a hard error, never a
silent fall-through to a keystore.

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

This one is policy, not a credential, so unlike the signer sources above it is
**not** stripped from the wrapped binary's environment.

There is no default, and until it is set the stablecoin rail is **unavailable**
rather than unlimited: the wrapper falls back to ETH and prints why. A default
is not well defined here, because the unit belongs to whichever token the
contract lists and decimals differ between tokens, so any fixed number would be
wrongly scaled for some of them. A malformed value is a hard error, never a
silent zero and never a silent unlimited.

A contract's ETH price and its stablecoin price are independent quotes with no
on-chain relation - the contract holds no oracle - so this ceiling is what
bounds the amount an agent will sign for. It is weighed after the rail is known
to be advertised, affordable, and signable, and before anything is signed: an
authorization is submittable by anyone, so one that exists for a refused amount
has already let the money go. A listed amount above it exits 22 and buys nothing
on either rail, rather than quietly switching currency, so a policy breach is
distinguishable from a network failure. An agent that holds none of the token is
not refused: it buys in ETH, exactly as it did before the stablecoin rail
existed.

For KMS, HSM, or enclave-backed keys, implement the `Signer` trait and pass it
to `activation::ensure_headless` directly. Its only primitive is "sign this
32-byte digest", so no key material ever enters the wrapper's process. Exactly
one type in the crate - `signer::LocalSigner` - holds a raw key at all, and none
of the `RUB3_AGENT_*` credential variables listed above survives into the
wrapped binary's environment.

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

Code 21 is deliberately not 14: the price may already have left the wallet, so
a blind retry can buy a second license. Once the named transaction has mined,
re-running takes the ordinary `tokensOfOwner` path and activates the token that
was bought; once it has been dropped, re-running purchases exactly once.

Every way of failing to confirm a broadcast purchase lands on 21, including the
RPC endpoint going away while the wrapper polls for the receipt: a transaction
whose fate is unknown is unresolved, not failed. Transient poll failures are
retried inside the 30s budget first, so a single 502 does not end a run.

Failures with structured parameters also print one parseable line:

```
error: cooldown active on token 0: retry in 12 blocks
rub3-detail: token_id=0 blocks_remaining=12
```

The line carries only parameters the wrapper actually measured. Code 11 prints
`required_wei` / `available_wei` when the wrapper's own balance check found the
shortfall, and no `rub3-detail:` line at all when the node rejected the
transaction without reporting amounts - never a placeholder zero.

Code 11 also says what `required_wei` covers, because the two balance checks run
at different points:

| `required_covers` | `required_wei` is | Top up |
|---|---|---|
| `price_plus_gas` | price + `gas_limit * max_fee_per_gas`, the full ceiling | that amount |
| `price` | the purchase price alone, measured before gas could be estimated | that amount plus gas |

`RUB3_SESSION_DIR` overrides where sessions are cached (default
`~/.rub3/sessions/<app_id>/<token_id>.json`) - useful for containers with a
mounted volume.

## Current status

See [implementation.md](implementation.md) for the full roadmap.

**Implemented:**
- Wrapper skeleton with process supervision and SIGTERM forwarding
- License proof schema, ECDSA signature verification (`personal_sign` / secp256k1), local proof caching
- Activation window: wallet address input, `tokensOfOwner()` enumeration, multi-token selection, activation message display, signature paste
- On-chain queries via alloy: `ownerOf`, `price`, `balanceOf`, `tokenOfOwnerByIndex`, `cooldown_ready`, `active_session_id`, `get_tx_receipt`, `get_block_number`; pure `encode_activate_calldata`
- Session model (tier 1-4): schema, `session_message()` hash, `verify_local()`, `is_expired()`, `new_nonce()`, full persistence with `load_latest_session()`
- Tier-3 activation flow (cooldown feature): cooldown screen → user-submitted `activate()` tx → receipt polling (10 × 3s) → `activeSessionId` read → session-sign screen → `verify_local` → session persisted. Fast path tries session first, falls back to legacy `LicenseProof` for zero-contract builds.
- Tier-3 on-chain re-verification: `session::verify_onchain` confirms tx status/contract/block hash; `try_session_fast_path` re-verifies ~1 in 5 cold starts (offline errors fall open, verdict-contradicting errors fall closed). Covered by an anvil-gated E2E test (`tests/session_onchain_e2e.rs`)
- Identity models: `identityModel` + `tbaImplementation` on-chain, local ERC-6551 TBA derivation (`identity.rs`), identity fields signed into the session preimage
- Purchase UI: in-wrapper purchase flow for tier 3+ (price/supply reads, calldata encoding, receipt polling, minted-token recovery)
- Smart contracts: `Rub3Access` + `Rub3Subscription` (ERC-721 + Enumerable, purchase, renew, `isValid`, tier-3 `activate` + cooldown), 174 forge tests
- Ownership invariants (§2.4): append-only wrapper hash set with on-chain revocation reasons, opt-in successor pointer with holder-initiated `claimFromPredecessor`, the contract-side `honorsContract` trust rule, per-token `renewPrice` snapshot, and a no-revocation bytecode audit
- **USDC purchases via EIP-3009 (§2.2):** `purchaseWithAuthorization` / `renewWithAuthorization` alongside the ETH path, taking a payment authorization the buyer signs off-chain that anyone may submit - so an agent holding only stablecoins can obtain a licence without ever owning ETH. Uses `receiveWithAuthorization` (payee-only) so the authorization cannot be spent outside the licence contract, and binds the mint recipient into the derived nonce so a submitter cannot redirect it. Both rails reach one mint; subscriptions freeze both per token. The authorization carries an opaque `bytes signature`, so an EIP-1271 smart-contract wallet buys on the same entry point as an EOA - which requires a payment token exposing Circle's FiatTokenV2_2-style `bytes` overload of `receiveWithAuthorization`; a token implementing only EIP-3009's `(v, r, s)` form is not supported. The wrapper's headless path prefers the stablecoin rail whenever the contract advertises one, the wallet can cover it, and the operator's `RUB3_AGENT_MAX_TOKEN_AMOUNT` ceiling covers the listed amount - see [Spend policy](#spend-policy)
- **`Rub3Factory` + protocol fee (§2.3):** the canonical deployment path. `deployAccess` / `deploySubscription` stamp an immutable protocol fee (`feeBps`, `treasury`) into every contract they deploy and record it in `isDeployed`, which is the whole trust rule the planned registry and marketplace will read. The split runs on-chain inside `purchase()` / `renew()` on **both** payment rails, charged on the amount received so a zero-price listing cannot route around it, and accrued in the contract rather than pushed - so an immutable treasury that cannot receive can never block a purchase. Both fee terms are `immutable` on the licence contract *and* the factory, with no setter on either, so a developer's economics can never change after deploy; rub3 changes its take only by deploying a new factory. Direct deployment stays possible, carries no fee, and is unrecorded by design. Neither the registry nor the marketplace is built yet, and the factory and the registry launch together: nothing is deployed to mainnet or declared ready for use before then. Nothing is charged on deploys, the CLI, the SDK, or the wrapper, and there is no token
- Deploy script: `forge script` deploys either contract to any EVM chain from env vars, directly or through a factory (`FACTORY`); `script/DeployFactory.s.sol` deploys the factory itself
- **Headless activation (the agent front door):** `activation::ensure_headless(signer, ctx)` runs `tokensOfOwner` → purchase if empty → cooldown check → `activate()` → local session signature → `verify_local` → persist, in one call. A `Signer` trait (env key / encrypted keystore / KMS-backed impl) keeps raw key handling in a single auditable type; `webview` and `headless` are independent Cargo features, and a headless build links no GUI dependency at all. `--headless [--token-id N]` with documented exit codes, covered by an anvil-gated E2E

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
