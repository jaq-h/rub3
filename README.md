# rub3

Wallet-native software licensing. NFT-gated access for native desktop applications, without a browser.

rub3 replaces username/password with wallet connect for native apps. The NFT is the access credential — owned by a wallet, verifiable on-chain, transferrable, composable. The wrapper is the runtime that enforces this on the user's machine.


## How it works

1. Developer packages their binary inside the rub3 wrapper
2. Developer deploys an ERC-721 license contract on Base (`Rub3Access` or `Rub3Subscription`)
3. User launches the wrapped app — the wrapper checks for a valid cached session
4. If no session (or session expired): the wrapper opens a native activation window, verifies on-chain ownership, and requests a wallet signature
5. On success: session is cached locally, wrapped binary launches
6. On subsequent launches within TTL: session is verified locally, binary launches immediately

There is no backend. The chain is the source of truth. The wallet is the identity.

## Project structure

```
rub3/
├── crates/
│   └── rub3-wrapper/                 # Wrapper runtime
│       ├── src/
│       │   ├── main.rs               # CLI entry point (clap), app constants
│       │   ├── lib.rs                # Public module re-exports (feature-gated)
│       │   ├── license.rs            # License proof schema, activation message, ECDSA verification
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

# Include network-dependent tests
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
- Smart contracts: `Rub3Access` + `Rub3Subscription` (ERC-721 + Enumerable, purchase, renew, `isValid`, tier-3 `activate` + cooldown), 30 forge tests
- Deploy script: `forge script` deploys either contract to any EVM chain from env vars

**Not yet implemented:** WalletConnect integration, cooldown extension in contracts, ENS verification, identity models (TBA derivation), purchase UI, session wiring into activation flow, CLI tooling, SDK, Tauri plugin.

## Design documents

- [ideation.md](ideation.md) — project vision, design principles, what rub3 is and isn't
- [architecture.md](architecture.md) — system design, session model, security tiers, components
- [implementation.md](implementation.md) — phased development plan with current status
- [contracts/contracts.md](contracts/contracts.md) — contract setup, local testing, deployment
- [testing.md](testing.md) — manual testing guide
