# rub3

Wallet-native software licensing for the machine economy. NFT-gated access for locally executed software — CLI tools, MCP servers, desktop apps — without a browser or a backend.

rub3 lets machines (and humans) buy, verify, run, and resell software without asking anyone's permission. The NFT is the access credential — owned by a wallet, verifiable on-chain, transferrable, composable — which also makes it a liquid asset: buy a license for a workload, resell it when the job ends. The wrapper is the runtime that enforces this on the machine where the software runs.


## How it works

1. Developer packages their binary inside the rub3 wrapper
2. Developer deploys an ERC-721 license contract on Base (`Rub3Access` or `Rub3Subscription`)
3. User launches the wrapped app — the wrapper checks for a valid cached session
4. If no session (or session expired): the wrapper opens a native activation window, verifies on-chain ownership, and requests a wallet signature
5. On success: session is cached locally, wrapped binary launches
6. On subsequent launches within TTL: session is verified locally, binary launches immediately

There is no backend. The chain is the source of truth. The wallet is the identity.

The flow above is the interactive (human) path that exists today. The top roadmap item is headless activation — signer in, session out, no webview — so an agent can complete the same loop programmatically. See [implementation.md](implementation.md) Phase 2.

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
│       │   ├── activation.rs         # Activation flow orchestration (load proof → verify → webview)
│       │   ├── rpc.rs                # On-chain queries (ownerOf, price, tokensOfOwner) via alloy
│       │   ├── webview.rs            # Native activation window (wry/tao), IPC message handling
│       │   ├── supervisor.rs         # Child process lifecycle, SIGTERM forwarding
│       │   ├── session.rs            # Session schema, message hash, verify_local, is_expired
│       │   └── session_store.rs      # Session persistence, load_latest_session
│       ├── assets/
│       │   └── activation.html       # Activation UI (address input, token select, signature)
│       └── tests/
│           ├── helpers/mod.rs        # Shared test utilities (wallet gen, signing, license creation)
│           ├── integration.rs        # Wrapper binary tests (exit codes, args, missing binary)
│           └── license_e2e.rs        # License verification tests (static + dynamic wallets, SIGTERM)
├── contracts/                        # Foundry project — ERC-721 license contracts
│   ├── src/
│   │   ├── Rub3License.sol           # Abstract base (ERC-721 + Enumerable + Ownable)
│   │   ├── Rub3Access.sol            # One-time purchase license
│   │   └── Rub3Subscription.sol      # Time-bounded license (expiresAt, renew, isValid)
│   ├── test/
│   │   ├── Rub3Access.t.sol
│   │   └── Rub3Subscription.t.sol
│   ├── script/
│   │   └── Deploy.s.sol              # Deploy either contract to any EVM chain
│   ├── foundry.toml
│   ├── .env.example
│   └── contracts.md                  # Local (Anvil) + on-chain (Base Sepolia) setup guide
├── licenses/
│   └── com.rub3.example.json         # Example license proof with valid signature
├── scripts/
│   └── test-e2e.sh                   # Convenience script — runs cargo test
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
| `wry` | Embedded webview for activation UI |
| `tao` | Native window/event loop |
| `serde` / `serde_json` | Proof and session serialization |
| `dirs` | Platform data directory resolution |
| `chrono` | RFC-3339 timestamps, session TTL |
| `rand` | Nonce generation (feature = `session`) |
| `nix` / `libc` | Unix signal handling (SIGTERM forwarding) |

Dev dependencies: `rand`, `tempfile`.

## Building

```bash
cargo build -p rub3-wrapper
```

## Testing

### Rust

```bash
# All tests (unit + integration + license e2e)
cargo test -p rub3-wrapper

# Only the network-dependent tests (--ignored filters out the suite above)
cargo test -p rub3-wrapper -- --ignored
```

**Unit tests** (`src/`): `license`, `store`, `rpc`, `session`, `session_store`

**Integration tests** (`tests/`): wrapper binary exit codes, argument passing, SIGTERM forwarding, static + dynamic license E2E

### Contracts

```bash
cd contracts
forge test
```

See [contracts/contracts.md](contracts/contracts.md) for local Anvil setup and Base Sepolia deployment.

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
- Smart contracts: `Rub3Access` + `Rub3Subscription` (ERC-721 + Enumerable, purchase, renew, `isValid`, tier-3 `activate` + cooldown), 33 forge tests
- Deploy script: `forge script` deploys either contract to any EVM chain from env vars

**Not yet implemented (agent-first roadmap):** headless activation (signer in, session out — no webview), USDC purchases via EIP-3009, `Rub3Factory` with immutable protocol fee split, ownership-invariant hardening (append-only wrapper hash set, successor pointer, per-token renewal snapshot), CLI tooling (`pack` / `deploy` / `fetch` / `register`), content-addressed distribution, registry with ERC-8004-style agent cards, concurrent-seat licensing, SDK, metered billing, marketplace. Human-surface polish (WalletConnect tabs, auto-detect, Preact refactor, Tauri plugin) is demoted behind the agent path; tier-4 device binding and binary encryption are deferred.

## Direction

The plan is agent-first (July 2026 revision — see [implementation.md](implementation.md)):

- **Let machines buy, verify, and resell software without asking anyone's permission.** The adoption unit is one closed loop an agent completes end to end: discover → pay → fetch → verify → run → resell.
- **Open-source the rails; own the factory, registry, and marketplace.** Revenue is a 2–3% fee on a payment flow only the wrapper can meter — priced low enough that no agent bothers to route around it. No token.
- **The token is the invariant; everything else is versioned.** Evolution only ever changes what is offered going forward (price, supply, successors, listings), never what was granted (held tokens, their validation, their renewal terms). No proxies, no revocation surface — structurally, not by promise.

First target market: wallet-gated MCP servers — paid MCP servers have no licensing primitive today, and agents are their natural customers.

## Design documents

- [ideation.md](ideation.md) — project vision, design principles, what rub3 is and isn't
- [architecture.md](architecture.md) — system design, session model, security tiers, components
- [implementation.md](implementation.md) — phased development plan with current status
- [contracts/contracts.md](contracts/contracts.md) — contract setup, local testing, deployment
- [testing.md](testing.md) — manual testing guide
