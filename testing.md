# Testing Guide

## Prerequisites

- Rust toolchain (rustc 1.91+): `rustup update stable`
- Optional: Foundry (`cast`, `anvil`) for manual wallet operations: `curl -L https://foundry.paradigm.xyz | bash && foundryup`
- Optional: Access to Base mainnet RPC (default: `https://mainnet.base.org`) for network tests

## 1. Run all tests

```bash
cargo test -p rub3-wrapper
```

This runs all unit tests, integration tests, and license e2e tests. No external tools required — wallet generation and signing are done natively in Rust via `k256`.

To include network-dependent tests (requires internet):

```bash
cargo test -p rub3-wrapper -- --ignored
```

Or use the convenience script:

```bash
scripts/test-e2e.sh
```

## 2. Test suites

### Unit tests (in `src/`)

- **`license::tests`** — activation message hashing, personal_sign prefix, proof serialization round-trips
- **`store::tests`** — proof save/load, directory creation, overwrite, missing file handling
- **`rpc::tests`** — provider construction, contract call error paths, ENS stub

### Integration tests (`tests/integration.rs`)

Binary-level tests that spawn the wrapper process:

- `runs_child_and_exits_zero` — wrapper exits 0 when child succeeds
- `propagates_nonzero_exit_code` — wrapper forwards child's exit code
- `passes_args_to_child` — `--` separator passes trailing args to child
- `errors_on_missing_binary` — wrapper rejects nonexistent binary path

Each test provisions a valid license proof in a temp directory via `RUB3_LICENSE_DIR`.

### License E2E tests (`tests/license_e2e.rs`)

**Static tests** — use a deterministic test keypair (hardcoded private key `0xac0974...`). Fully reproducible:

- `static_license_verifies` — construct proof, verify signature recovery matches wallet address
- `static_license_loads_and_verifies` — write proof to disk, load it back, verify
- `static_wrapper_runs_with_valid_license` — run wrapper binary with a valid proof, assert child executes

**Dynamic tests** — generate a random wallet each run via `k256::ecdsa::SigningKey::random()`:

- `dynamic_wallet_generates_valid_signature` — prove the full crypto pipeline works with random keys
- `dynamic_license_round_trips` — generate, save, load, verify with fresh keypair
- `dynamic_wrapper_runs_with_fresh_license` — run wrapper with ephemeral license

**Signal handling:**

- `wrapper_forwards_sigterm` — spawn wrapper with `/bin/sleep`, send SIGTERM, assert clean exit

### Test helpers (`tests/helpers/mod.rs`)

Shared utilities available to all integration test files:

- `generate_wallet()` — random secp256k1 keypair, returns `(SigningKey, address_hex)`
- `sign_activation(key, app_id, token_id)` — compute activation message, personal_sign, return hex signature
- `create_license_json(dir, ...)` — write a valid `LicenseProof` JSON file
- `wrapper_bin()` — path to the compiled wrapper binary
- `verifying_key_to_address(key)` — derive Ethereum address from public key

## 3. Seed a license proof for manual testing

The `seed-license.sh` script generates a valid license proof so the wrapper skips the activation window. Requires Foundry (`cast`).

```bash
./scripts/seed-license.sh
```

This writes a proof to `/tmp/rub3-test/com.rub3.example.json` signed by anvil's default account 0. Then run the wrapper with:

```bash
RUB3_LICENSE_DIR=/tmp/rub3-test cargo run -p rub3-wrapper -- --binary /path/to/your/binary
```

The wrapper will verify the proof's signature, skip activation, and launch the binary directly.

To reset and force re-activation:

```bash
rm -rf /tmp/rub3-test
```

## 4. Manual wallet operations with `cast`

For ad-hoc wallet operations (not required for automated tests):

```bash
# Create a wallet
cast wallet new

# Check balance on Base
cast balance <ADDRESS> --rpc-url https://mainnet.base.org

# Query a license contract
cast call <CONTRACT_ADDRESS> "ownerOf(uint256)" 1 --rpc-url https://mainnet.base.org
cast call <CONTRACT_ADDRESS> "price()" --rpc-url https://mainnet.base.org

# Sign an activation message
cast wallet sign --private-key <KEY> <MESSAGE_HASH>

# Use a local fork
anvil --fork-url https://mainnet.base.org
cast call <CONTRACT_ADDRESS> "ownerOf(uint256)" 1 --rpc-url http://127.0.0.1:8545
```

## 5. App constants

The wrapper's identity is controlled by constants in `crates/rub3-wrapper/src/main.rs`:

| Constant | Default | Purpose |
|---|---|---|
| `APP_ID` | `com.rub3.example` | Reverse-DNS app identifier |
| `CONTRACT` | `0x0000...0000` | ERC-721 license contract address |
| `CHAIN_ID` | `8453` | EVM chain ID (Base mainnet) |
| `RPC_URL` | `https://mainnet.base.org` | JSON-RPC endpoint |
| `DEVELOPER_ENS` | `None` | Optional ENS name |

To test against a real contract, update `CONTRACT` to your deployed ERC-721 address and rebuild.
