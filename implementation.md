# deotp — Implementation Plan

## Phase 1: Proof of Concept

Goal: A working wrapper that gates a simple Rust binary behind a wallet signature check.

### 1.1 — Wrapper skeleton (Rust)
- Create `deotp-wrapper` Rust project
- Implement basic CLI: `deotp-wrapper --binary <path>` launches embedded app as child process
- SIGTERM/SIGCHLD handling: wrapper kills child on exit, child exits if wrapper dies
- No license check yet — just prove the process supervision model works

### 1.2 — Machine ID
- Implement `machine_id()` for macOS first (IOPlatformUUID via IOKit FFI)
- Linux (`/sys/class/dmi/id/product_uuid`) as second target
- Hash with SHA-256, salt with app_id
- Write tests to verify stability across runs

### 1.3 — License verification (offline)
- Define license proof JSON schema
- Implement signature verification: recover signer address from ECDSA signature, compare to stored address
- Use `k256` crate for secp256k1 (same curve as Ethereum wallets)
- Hardcode a test license proof for development

### 1.4 — Wallet connection + activation
- Embed a minimal webview (via `wry` crate — same engine Tauri uses) for WalletConnect
- On activation: query `ownerOf(tokenId)` via JSON-RPC to Base
- Request wallet signature over `H(app_id || tokenId || machine_id)`
- Store license proof to `~/.deotp/licenses/<app_id>.json`
- Use `alloy` crate for Ethereum RPC, ABI encoding, and ENS resolution

### 1.5 — Smart contract
- Standard ERC-721 with payable `mint()` function
- Add `bytes32 wrapperHash` for binary verification
- Use OpenZeppelin contracts, deploy to Base Sepolia (testnet) for development
- Foundry project for contract development/testing

### 1.6 — ENS verification
- At activation, resolve developer's ENS name via `alloy`
- Compare resolved address to embedded contract address
- Refuse activation on mismatch, show clear warning
- Display ENS name prominently in activation UI

**Phase 1 deliverable:** A wrapped binary that requires NFT ownership + wallet signature to run, with ENS-based contract verification, verified offline on subsequent launches.

## Phase 2: Developer Tooling

### 2.1 — deotp CLI (`deotp pack`)
- Takes a compiled binary, app_id, contract address, chain config
- Bundles wrapper + app into single distributable binary
- Binary packing: embed app as a compressed payload, extract to temp on first run (or use `include_bytes!` at pack time for static embedding)
- Output: single executable for target platform

### 2.2 — deotp SDK crate
- `deotp::heartbeat()` — IPC check against wrapper, panics if dead
- `deotp::license_info()` — returns license metadata
- Communication via Unix domain socket (path passed as env var by wrapper)
- Minimal dependency footprint

### 2.3 — Contract templates
- Provide a ready-to-deploy Solidity contract template
- Configurable: price, max supply, royalty (ERC-2981), metadata URI, wrapperHash
- CLI command: `deotp deploy --chain base --price 0.01`
- Requires user to have Foundry/cast installed, or use a bundled deployer

### 2.4 — ENS + deotp registry
- Deploy `DeotpRegistry` contract on Base
- Permissionless `register()` — developer proves contract ownership on-chain
- Sets `appName.deotp.eth` subdomain pointing to their license contract
- CLI command: `deotp register --name myapp --contract 0x1234...abcd`
- Wrapper shows "verified on deotp.eth" badge when registry entry exists

**Phase 2 deliverable:** A developer can package, deploy, register, and distribute a licensed application with a few CLI commands.

## Phase 3: Tauri Integration

### 3.1 — Tauri plugin
- `tauri-plugin-deotp` crate
- Auto-heartbeat in the Tauri event loop
- Frontend JS API: `invoke('plugin:deotp|license_info')`
- Activation flow rendered in the app's own webview (no separate window needed)

### 3.2 — Tauri starter template
- `create-deotp-app` or similar scaffold
- Pre-configured Tauri app with deotp plugin, ready to build and package

**Phase 3 deliverable:** Tauri developers can add license gating to their apps with a plugin and a few lines of config.

## Phase 4: Polish and Hardening

### 4.1 — Multi-machine support
- Allow N activations per NFT (configurable by developer in contract)
- Track activation count on-chain or via signed activation receipts
- Deactivation flow: user can release a machine slot

### 4.2 — License transfer
- When NFT is transferred, old activations should expire
- Options: time-limited signatures (re-check every 30 days), or activation includes block number and wrapper periodically verifies ownership hasn't changed

### 4.3 — Windows support
- Machine ID from registry (`MachineGuid`)
- Named pipes instead of Unix domain sockets
- MSVC build target for wrapper

### 4.4 — Binary obfuscation (optional)
- UPX-style compression to raise the bar for casual extraction
- Not encryption — just inconvenience
- Clearly documented as a deterrent, not a guarantee

## Tech Stack

| Component | Technology |
|---|---|
| Wrapper runtime | Rust |
| Crypto (secp256k1) | `k256` crate |
| Ethereum RPC | `alloy` crate |
| Webview (wallet connection) | `wry` crate |
| IPC (wrapper ↔ app) | Unix domain sockets / named pipes |
| Smart contracts | Solidity, OpenZeppelin, Foundry |
| Target chain | Base (primary). Chain abstracted behind config for future EVM L2 support |
| CLI | `clap` crate |
| Packaging | Custom binary bundler or `goblin` crate for ELF/Mach-O manipulation |

## Directory Structure

```
deotp/
├── crates/
│   ├── deotp-wrapper/       # The wrapper runtime
│   ├── deotp-sdk/           # Crate apps link against
│   ├── deotp-cli/           # Developer packaging tool
│   └── tauri-plugin-deotp/  # Tauri integration
├── contracts/
│   ├── src/
│   │   ├── DeotpLicense.sol  # ERC-721 license contract
│   │   └── DeotpRegistry.sol # ENS subdomain registry
│   ├── test/
│   └── foundry.toml
├── examples/
│   ├── hello-rust/          # Minimal Rust app example
│   └── hello-tauri/         # Minimal Tauri app example
└── docs/
```
