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

The default bundle is `tier-2` + `webview`, so it compiles neither the tier-3
capabilities nor the headless front door. Cargo features are additive, so
`--no-default-features` is mandatory when selecting another bundle:

```bash
# tier-3 (adds onchain-write + cooldown): 67 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib

# tier-3 + the headless (agent) front door: 112 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless --lib
```

For reference, `--lib` counts per bundle: `tier-0` 29, `tier-1`/`tier-2` 59,
`tier-3`/`tier-4` 67, `tier-3,headless` 112. Each total includes the one
`#[ignore]`d network test, which a plain run skips, so a bundle reports one
fewer as passed.

Network-dependent tests (requires internet). `--ignored` runs *only* the ignored tests, so this replaces the suite above rather than adding to it:

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
- **`rpc::tests`** — provider construction, contract call error paths, `encode_activate_calldata` selector + layout, `get_tx_receipt` / `get_block_number` error paths, ENS stub
- **`session::tests`** (requires `session` feature) — message determinism, tier-diffing, expiry edge cases, sign/verify round-trip, wrong-wallet failure; with `cooldown` adds: `verify_onchain` missing-field + bad-URL paths, `should_reverify` distribution sanity
- **`session_store::tests`** (requires `session` feature) — save/load round-trip, missing-session, `load_latest_session` picking the freshest valid session (`load_latest_session_for_wallet` narrows the same scan to one signer, covered from `activation::tests`)
- **`identity::tests`** - `IdentityModel` parsing and wire format, ERC-6551 TBA derivation determinism and sensitivity to each input, `resolve_user_id` for both models
- **`signer::tests`** (requires `headless` feature) - hex key parsing (bare/prefixed/padded, and every rejection: wrong length, non-hex, zero and out-of-curve-order scalars), `Debug` redaction and error messages asserted not to echo the input, `personal_sign` / `sign_prehash` recovery, RFC-6979 determinism, keystore decrypt, password-file precedence, and the strict env-key-over-keystore resolution order with no fall-through on a malformed key
- **`tx::tests`** (requires `headless` feature) - invalid-URL transport error, the node's `insufficient funds` classifier, and the shortfall message with and without known amounts
- **`activation::tests`** (requires `headless` feature) - the exit-code table asserted value-by-value, all classified codes distinct and disjoint from 0/1/2, `machine_detail` contents, `lowest_token` selection, the token- and wallet-scoped session fast path, and every unconfirmed-purchase outcome mapping to the terminal code 21
- **`rpc::tests::receipt_polling`** (requires `onchain-write` or `cooldown`) - the receipt poll loop driven over scripted answers: a transient transport failure does not end the wait, one that outlasts the budget is reported as `Transport`, a recovered poll ends as `Timeout`, and both report the elapsed budget

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

### Tier-3 on-chain session E2E (`tests/session_onchain_e2e.rs`)

Exercises `session::verify_onchain` against a live EVM node. Requires the Foundry toolchain (`anvil`, `forge`, `cast`) on PATH; gracefully prints `SKIP:` and returns when any of those are missing. Marked `#[ignore]` so default `cargo test` runs skip it.

- `session_verify_onchain_e2e` — spawns `anvil` on port 8547, deploys `Rub3Access` via `forge create`, runs `purchase(address)` + `activate(uint256)` via `cast send`, extracts the receipt's block hash, then:
  - asserts `verify_onchain` succeeds for a correctly-populated session,
  - tampers the `contract` field → `VerifyError::ContractMismatch`,
  - tampers the `activation_block_hash` → `VerifyError::BlockHashMismatch`,
  - points `activation_tx` at a non-existent hash → `VerifyError::ReceiptNotFound`.

Run with:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3 \
    -- --ignored session_verify_onchain_e2e
```

### Headless (agent) E2E (`tests/headless_e2e.rs`)

Drives `activation::ensure_headless` end to end with no webview involved: the test binary links neither `wry` nor `tao`. Same gating as the suite above (`#[ignore]`, prints `SKIP:` and passes when Foundry is absent). Runs anvil on port **8549** so it can run alongside `session_onchain_e2e.rs` (8547), and serialises its own tests through a file-level mutex covering the port and the process-global env vars, so no `--test-threads=1` is needed. Every test generates a fresh key and resolves it through the real `RUB3_AGENT_KEY` path with an isolated `RUB3_SESSION_DIR`.

- `headless_purchase_activate_persist_e2e` - funds a fresh key, purchases at 0.01 ETH, activates, asserts ownership / `nextTokenId` / every session field / `verify_local` / `verify_onchain` / on-disk persistence, then relaunches and asserts `Reused` with no new mint
- `headless_insufficient_funds_e2e` - unfunded key → exit code 11
- `headless_sold_out_e2e` - supply cap of 1, already minted → exit code 12
- `headless_cooldown_active_then_ready_e2e` - exit code 13 reporting `blocks_remaining`, then succeeds after `anvil_mine` with `session_id` bumped to 2
- `headless_explicit_token_not_owned_e2e` - `--token-id` the signer does not hold → exit code 20, minting nothing
- `headless_explicit_token_id_does_not_reuse_another_tokens_session_e2e` - a cached session for a different token cannot satisfy `--token-id` → exit code 20
- `headless_chain_id_mismatch_e2e` - endpoint chain id ≠ build chain id → exit code 19, refused before anything is signed

Run with:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless \
    -- --ignored headless
```

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
