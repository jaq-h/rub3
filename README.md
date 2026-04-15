# rub3

Wallet-native software licensing. NFT-gated access for native desktop applications, without a browser.

rub3 replaces username/password with wallet connect for native apps. The NFT is the access credential — owned by a wallet, verifiable on-chain, transferrable, composable. The wrapper is the runtime that enforces this on the user's machine.

## How it works

1. Developer packages their binary inside the rub3 wrapper
2. Developer deploys an ERC-721 license contract on Base
3. User launches the wrapped app — the wrapper checks for a cached license proof
4. If no proof exists: the wrapper opens a native activation window (wallet connect, signature)
5. On success: proof is cached locally, wrapped binary launches
6. On subsequent launches: proof is verified locally, binary launches immediately

There is no backend. The chain is the source of truth. The wallet is the identity.

## Project structure

```
rub3/
├── crates/
│   └── rub3-wrapper/                 # Wrapper runtime (the only crate implemented so far)
│       ├── src/
│       │   ├── main.rs               # CLI entry point (clap), app constants
│       │   ├── lib.rs                # Public module re-exports
│       │   ├── license.rs            # License proof schema, activation message, ECDSA verification
│       │   ├── store.rs              # Proof persistence (~/.rub3/licenses/ or RUB3_LICENSE_DIR)
│       │   ├── activation.rs         # Activation flow orchestration (load proof → verify → webview)
│       │   ├── rpc.rs                # On-chain queries (ownerOf, price) via alloy
│       │   ├── webview.rs            # Native activation window (wry/tao), IPC message handling
│       │   └── supervisor.rs         # Child process lifecycle, SIGTERM forwarding
│       ├── assets/
│       │   └── activation.html       # Activation window UI (dark theme, wallet input, signature)
│       └── tests/
│           ├── helpers/mod.rs        # Shared test utilities (wallet gen, signing, license creation)
│           ├── integration.rs        # Wrapper binary tests (exit codes, args, missing binary)
│           └── license_e2e.rs        # License verification tests (static + dynamic wallets, SIGTERM)
├── licenses/
│   └── com.rub3.example.json         # Example license proof with valid signature
├── scripts/
│   ├── test-e2e.sh                   # Convenience script — runs cargo test
│   └── seed-license.sh               # Generate a valid license proof for manual testing
├── architecture.md                   # System design, session model, security model
├── implementation.md                 # Phased development plan with status
├── ideation.md                       # Project vision and design principles
└── testing.md                        # Manual testing guide (wallet setup, activation flow)
```

## Dependencies

| Crate | Purpose |
|---|---|
| `clap` | CLI argument parsing |
| `k256` | secp256k1 ECDSA signature recovery |
| `sha2` | SHA-256 activation message hash |
| `sha3` | Keccak-256 for Ethereum address derivation + personal_sign |
| `hex` | Hex encoding/decoding |
| `alloy` | Ethereum JSON-RPC (ownerOf, price queries) |
| `wry` | Embedded webview for activation UI |
| `tao` | Native window/event loop |
| `serde` / `serde_json` | License proof serialization |
| `dirs` | Platform data directory resolution |
| `chrono` | RFC-3339 timestamps |
| `nix` / `libc` | Unix signal handling (SIGTERM forwarding) |

Dev dependencies: `rand`, `tempfile` (for integration tests).

## Building

```bash
cargo build -p rub3-wrapper
```

## Testing

```bash
# All tests (unit + integration + license e2e)
cargo test -p rub3-wrapper

# Include network-dependent tests
cargo test -p rub3-wrapper -- --ignored
```

### Test suites

**Unit tests** (in `src/`):
- `license::tests` — activation message hashing, personal_sign, proof serialization
- `store::tests` — proof persistence round-trips, directory creation, overwrite
- `rpc::tests` — provider construction, error handling

**Integration tests** (`tests/integration.rs`):
- Wrapper binary: exit codes, argument passing, missing binary rejection

**License E2E tests** (`tests/license_e2e.rs`):
- **Static tests** — deterministic keypair, repeatable: proof verification, save/load/verify pipeline, wrapper binary execution with valid license
- **Dynamic tests** — random wallet per run: signature generation, round-trip verification, wrapper execution with fresh license
- **SIGTERM forwarding** — wrapper forwards signals to child process

All crypto (wallet generation, signing) is done natively in Rust via `k256` — no external tools required.

## Running the wrapper

On first run with no cached proof, the wrapper opens an activation window. To skip activation during development, seed a valid license proof:

```bash
# Generate a valid proof (requires Foundry's cast)
./scripts/seed-license.sh

# Run the wrapper — skips activation, launches binary directly
RUB3_LICENSE_DIR=/tmp/rub3-test cargo run -p rub3-wrapper -- --binary /path/to/your/app
```

Without the seed script, the wrapper opens the activation window for wallet connect + signature:

```bash
cargo run -p rub3-wrapper -- --binary /path/to/your/app
```

After activation, the proof is cached and the binary launches immediately on subsequent runs.

## License proof format

```json
{
  "app_id": "com.rub3.example",
  "token_id": 1,
  "wallet_address": "0x...",
  "signature": "0x...",
  "activated_at": "2026-04-09T00:00:00Z",
  "chain": "base",
  "contract": "0x..."
}
```

The signature is an ECDSA `personal_sign` over `SHA-256(app_id || token_id_be_bytes)`. Verification recovers the signer address and compares it to `wallet_address`.

## Current status

See [implementation.md](implementation.md) for the full roadmap. Currently implemented:

- Wrapper skeleton with process supervision and signal forwarding
- License proof schema, ECDSA signature verification, local proof caching
- Activation flow with native webview (wallet connect UI stubbed, manual signature input works)
- On-chain queries via alloy (ownerOf, price)
- Static and dynamic integration test suite

Not yet implemented: sessions (TTL-based), WalletConnect integration, token selection, ENS verification, identity models, smart contracts, CLI tooling, SDK, Tauri plugin.

## Design documents

- [ideation.md](ideation.md) — project vision, design principles, what rub3 is and isn't
- [architecture.md](architecture.md) — system design, session model, components, security model
- [implementation.md](implementation.md) — phased development plan with current status
- [testing.md](testing.md) — manual testing guide
