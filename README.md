# rub3

Wallet-native software licensing. NFT-gated access for native desktop applications, without a browser.

rub3 replaces username/password with wallet connect for native apps. The NFT is the access credential — owned by a wallet, verifiable on-chain, transferrable, composable. The wrapper is the runtime that enforces this on the user's machine.

## How it works

1. Developer packages their binary inside the rub3 wrapper
2. Developer deploys an ERC-721 license contract on Base (`Rub3Access` or `Rub3Subscription`), picking an identity model (`access` — wallet is the user, or `account` — the token's ERC-6551 TBA is the user)
3. User launches the wrapped app — the wrapper checks for a valid cached session
4. If no session (or session expired): the wrapper opens a native activation window. If the wallet owns no token, the purchase screen encodes `purchase(address)` calldata for the user to send from their wallet. Once a token is owned, the wrapper polls `cooldownReady`, surfaces `activate(uint256)` calldata, waits for the receipt, and requests a session signature over the session message
5. On success: session cached at `~/.rub3/sessions/<app_id>/<token_id>.json`, wrapped binary launches
6. On subsequent launches within TTL: session verified locally (signature + expiry + ~1-in-5 on-chain re-verification at tier 3), binary launches immediately

There is no backend. The chain is the source of truth. The wallet is the identity.

## Project structure

```
rub3/
├── crates/
│   └── rub3-wrapper/                 # Wrapper runtime
│       ├── src/
│       │   ├── main.rs               # CLI entry point (clap), app constants
│       │   ├── lib.rs                # Public module re-exports (feature-gated)
│       │   ├── license.rs            # Legacy license proof schema, activation message, ECDSA verification
│       │   ├── store.rs              # Proof persistence (~/.rub3/licenses/ or RUB3_LICENSE_DIR)
│       │   ├── activation.rs         # Activation flow orchestration (session fast path → legacy proof → webview)
│       │   ├── rpc.rs                # On-chain queries + calldata encoding + receipt polling via alloy
│       │   ├── webview.rs            # Native activation window (wry/tao), IPC message handling
│       │   ├── supervisor.rs         # Child process lifecycle, SIGTERM forwarding
│       │   ├── session.rs            # [feature = "session"] Session schema, message hash, verify_local, verify_onchain
│       │   ├── session_store.rs      # [feature = "session"] Session persistence, load_latest_session
│       │   ├── identity.rs           # [feature = "session"] Identity model + ERC-6551 TBA derivation
│       │   ├── device.rs             # [scaffold, feature = "device-key"] Device keypair mgmt (tier 4)
│       │   └── decrypt.rs            # [scaffold, feature = "binary-encryption"] AES-256-GCM binary unwrap
│       ├── assets/
│       │   └── activation.html       # Activation UI (connect, token select, purchase, cooldown, sign)
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
| `sha3` | Keccak-256 for Ethereum address derivation + personal_sign + TBA derivation |
| `hex` | Hex encoding/decoding |
| `alloy` | Ethereum JSON-RPC, ABI encoding, receipt polling, log parsing |
| `url` | RPC URL parsing |
| `tokio` | `rt` only — minimal runtime for `block_on` around alloy's async surface |
| `wry` | Embedded webview for activation UI |
| `tao` | Native window/event loop |
| `serde` / `serde_json` | Proof and session serialization |
| `dirs` | Platform data directory resolution |
| `chrono` | RFC-3339 timestamps, session TTL |
| `rand` | Nonce generation (optional — pulled in by feature `session`) |
| `keyring` | OS keychain (optional — pulled in by feature `device-key`, tier 4) |
| `aes-gcm` | AES-256-GCM binary encryption (optional — pulled in by feature `binary-encryption`) |
| `nix` / `libc` | Unix signal handling (SIGTERM forwarding) |

Dev dependencies: `rand`, `tempfile`.

## Feature flags and tier bundles

The wrapper is a single crate with Cargo features selecting compile-time behaviour. Packing a distributable picks one tier bundle; orthogonal add-ons compose independently.

| Feature | Composed capabilities |
|---|---|
| `tier-0` | — (legacy proof, signature only) |
| `tier-1` | `session` |
| `tier-2` (default) | `session` + `onchain-read` |
| `tier-3` | `session` + `onchain-read` + `onchain-write` + `cooldown` |
| `tier-4` | `tier-3` + `device-key` |
| `binary-encryption` | orthogonal add-on, composes with tier-3+ |

See `architecture.md` for the tier semantics and `implementation.md §1.9` for the capability matrix.

## Building

```bash
cargo build -p rub3-wrapper
```

## Testing

### Rust

```bash
# All tests (unit + integration + license e2e)
cargo test -p rub3-wrapper

# Include network-dependent tests
cargo test -p rub3-wrapper -- --ignored
```

**Unit tests** (`src/`): `license`, `store`, `rpc`, `session`, `session_store`, `identity`. 57 pass under default (tier-2); 61 under `--no-default-features --features tier-3`.

**Integration tests** (`tests/`): wrapper binary exit codes, argument passing, SIGTERM forwarding, static + dynamic license E2E, plus an anvil-gated tier-3 chain E2E (`session_onchain_e2e.rs`, `#[ignore]`) that spawns `anvil`, deploys `Rub3Access` via `forge create`, exercises purchase + activate + `verify_onchain`.

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
- Legacy license proof: schema, ECDSA signature verification (`personal_sign` / secp256k1), local proof caching (`~/.rub3/licenses/`)
- Activation window with five screens: connect, token select, purchase, cooldown, sign-session. Native wry/tao webview, JS↔Rust IPC
- On-chain queries via alloy: `ownerOf`, `price`, `balanceOf`, `tokenOfOwnerByIndex`, `supplyCap`, `nextTokenId`, `identityModel`, `tbaImplementation`, `cooldownReady`, `activeSessionId`, `lastActivationBlock`, `cooldownBlocks`, `get_tx_receipt`, `get_block_number`; pure calldata encoders for `activate(uint256)` and `purchase(address)`; ERC-721 `Transfer` log parser to recover minted tokenIds
- Session model (tier 1-4): schema, `session_message()` hash, `verify_local`, `verify_onchain`, `is_expired`, `new_nonce`, persistence with `load_latest_session`
- Identity model + ERC-6551 TBA derivation (§1.6): `identity.rs` computes TBA addresses locally via CREATE2 against the canonical registry, no RPC needed. Session message binds `identity` + `user_id` so tampering requires re-signing
- Tier-3 activation flow (`cooldown` feature): cooldown screen → user-submitted `activate()` tx → receipt polling (10 × 3s) → `activeSessionId` read → session-sign screen → `verify_local` → session persisted. Fast path tries session first, falls back to legacy `LicenseProof` for zero-contract builds
- Tier-3 on-chain re-verification: `session::verify_onchain` confirms tx status / contract / block hash; `try_session_fast_path` re-verifies ~1 in 5 cold starts (offline errors fall open, verdict-contradicting errors fall closed)
- Purchase UI (§1.7, `onchain-write` feature): zero-token wallets land on an in-wrapper purchase screen that reads `supplyCap` / `nextTokenId` / `price`, encodes `purchase(address)` calldata, surfaces it, and polls the receipt for the minted token before re-entering the cooldown/activate flow
- Tier scaffold + feature flags (§1.9): five tier bundles + orthogonal `binary-encryption` composition — all compile clean
- Smart contracts: abstract `Rub3License` + concrete `Rub3Access` (one-time purchase) + `Rub3Subscription` (time-bounded with `renew` / `isValid`). Tier-3 `activate` + cooldown + `activeSessionId`; tier-4 `activateDevice` + `registeredDevice`. Immutable identity model + TBA implementation, immutable cooldown (`MIN_COOLDOWN_BLOCKS = 15`). **43 forge tests pass.**
- Deploy script: env-driven `forge script` targets Anvil, Base Sepolia, or mainnet

**Planned (near-term):**
- Frictionless tx confirmation (implementation.md §1.10): WalletConnect v2 + RPC auto-detect tabs layered over the existing manual-paste floor on purchase and cooldown screens
- Base Sepolia deployment and end-to-end against a live chain
- Tier-4 wrapper wiring (`device.rs` + `decrypt.rs` are scaffolds today; contract side is done)
- ENS verification + `Rub3Registry` (§2.4)
- CLI tooling (`rub3 pack` / `rub3 deploy` / `rub3 register`) — §2.1, §2.2
- `rub3-sdk` crate (`rub3::heartbeat`, `rub3::session`) — §2.3
- Tauri plugin (`tauri-plugin-rub3`) — §3.1
- Activation UI refactor to Preact + vendored ESM via `include_dir` custom protocol — §2.5

## Design documents

- [ideation.md](ideation.md) — project vision, design principles, what rub3 is and isn't
- [architecture.md](architecture.md) — system design, session model, security tiers, components
- [implementation.md](implementation.md) — phased development plan with current status
- [contracts/contracts.md](contracts/contracts.md) — contract setup, local testing, deployment
- [testing.md](testing.md) — manual testing guide
