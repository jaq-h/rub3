# Testing Guide

## Prerequisites

- Rust toolchain (rustc 1.91+): `rustup update stable`
- Foundry (`cast`, `anvil`): `curl -L https://foundry.paradigm.xyz | bash && foundryup`
- Access to Base mainnet RPC (default: `https://mainnet.base.org`)

## 1. Build the wrapper

```bash
cargo build -p rub3-wrapper
```

The binary lands at `target/debug/rub3-wrapper`.

## 2. Run unit tests

```bash
# All offline tests
cargo test -p rub3-wrapper

# Include network-dependent tests (requires internet)
cargo test -p rub3-wrapper -- --ignored
```

Key test modules:
- `license` — activation message hashing, signature recovery, proof serialisation
- `store` — license proof read/write/overwrite on disk
- `rpc` — provider construction, contract call error paths

## 3. Set up a test wallet with `cast`

### Create a new wallet

```bash
cast wallet new
```

This outputs a private key and address. To persist it in a keystore:

```bash
cast wallet import test-wallet --interactive
# Paste your private key when prompted, set a password
```

### Check wallet balance on Base

```bash
cast balance <ADDRESS> --rpc-url https://mainnet.base.org
```

### Query the license contract

Check NFT ownership:

```bash
cast call <CONTRACT_ADDRESS> "ownerOf(uint256)" 1 --rpc-url https://mainnet.base.org
```

Check the license price:

```bash
cast call <CONTRACT_ADDRESS> "price()" --rpc-url https://mainnet.base.org
```

### Sign an activation message (for manual testing)

```bash
# Build the activation message hash (SHA-256 of app_id || token_id_be)
# Then sign with personal_sign:
cast wallet sign --keystore ~/.foundry/keystores/test-wallet <MESSAGE_HASH>
```

### Use a local testnet with `anvil`

For offline testing without real funds:

```bash
# Start a local fork of Base
anvil --fork-url https://mainnet.base.org

# Use one of the default anvil accounts (printed on startup)
cast call <CONTRACT_ADDRESS> "ownerOf(uint256)" 1 --rpc-url http://127.0.0.1:8545
```

## 4. Run the wrapper against a test binary

Create a trivial binary to wrap:

```bash
echo '#!/bin/sh
echo "hello from wrapped app"' > /tmp/test-app.sh
chmod +x /tmp/test-app.sh
```

Launch the wrapper:

```bash
cargo run -p rub3-wrapper -- --binary /tmp/test-app.sh
```

### What happens

1. It checks `~/Library/Application Support/rub3/licenses/com.rub3.example.json` for a cached proof.
2. If no valid proof exists, it opens an **activation window** (native webview).
3. The webview walks you through: connect wallet → verify NFT ownership → sign activation message.
4. On success, the proof is saved and the wrapped binary (`test-app.sh`) is launched.
5. On subsequent runs with a valid proof, the binary launches immediately (no window). The license is wallet-bound (not machine-bound), so the same proof works on any device.

### Overriding the license directory

Set `RUB3_LICENSE_DIR` to store proofs somewhere other than the platform default:

```bash
RUB3_LICENSE_DIR=/tmp/rub3-test cargo run -p rub3-wrapper -- --binary /tmp/test-app.sh
```

This is useful for testing activation from scratch without touching your real license store.

## 5. Reset activation state

Delete the cached proof to force re-activation:

```bash
rm ~/Library/Application\ Support/rub3/licenses/com.rub3.example.json
```

Or if using the override:

```bash
rm -rf /tmp/rub3-test
```

## 6. Configuring app constants

The wrapper's identity is controlled by constants in `crates/rub3-wrapper/src/main.rs`:

| Constant        | Default                                      | Purpose                          |
|-----------------|----------------------------------------------|----------------------------------|
| `APP_ID`        | `com.rub3.example`                           | Reverse-DNS app identifier       |
| `CONTRACT`      | `0x0000...0000`                              | ERC-721 license contract address |
| `CHAIN_ID`      | `8453`                                       | EVM chain ID (Base mainnet)      |
| `RPC_URL`       | `https://mainnet.base.org`                   | JSON-RPC endpoint                |
| `DEVELOPER_ENS` | `None`                                       | Optional ENS name                |

To test against a real contract, update `CONTRACT` to your deployed ERC-721 address and rebuild.

## 7. Signal handling (Unix)

The wrapper forwards `SIGTERM` to the child process. To test:

```bash
# In one terminal
cargo run -p rub3-wrapper -- --binary /bin/sleep -- 300

# In another terminal
kill -TERM <wrapper-pid>
```

The wrapper should forward the signal and exit cleanly.
