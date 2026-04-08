# deotp — Architecture

## System Overview

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────────┐
│   Developer   │     │    Blockchain     │     │      User        │
│              │     │                  │     │                  │
│  App binary   │     │  ERC-721 License  │     │  Wallet          │
│  deotp CLI    │────▶│  Contract         │◀────│  deotp Wrapper   │
│              │     │  (purchase/mint)  │     │  Embedded App    │
└──────────────┘     └──────────────────┘     └──────────────────┘
```

## Components

### 1. Smart Contract (existing infrastructure)

Standard ERC-721 with a payable `purchase()` function. No custom logic needed beyond:
- Price per license
- Optional supply cap
- Mint to `msg.sender`

OpenZeppelin's ERC-721 template covers this. The contract is deployed once per application.

### 2. deotp Wrapper Runtime

The core product. A Rust binary that:

```
deotp-wrapper
├── Activation Module
│   ├── Wallet connection interface (WalletConnect or embedded webview)
│   ├── Chain RPC query: ownerOf() on the license contract
│   ├── Signature request: wallet signs H(app_id || tokenId || machine_id)
│   └── License store: writes proof to ~/.deotp/licenses/<app_id>.json
│
├── Verification Module (runs every launch, offline)
│   ├── Read stored license proof
│   ├── Re-derive machine_id from hardware fingerprint
│   ├── Verify ECDSA signature against stored wallet address
│   └── Result: pass → launch app, fail → show activation prompt
│
├── Process Supervisor
│   ├── Launch embedded binary as child process
│   ├── Heartbeat IPC channel (Unix domain socket)
│   ├── Monitor child health, restart on crash (optional)
│   └── Kill child if wrapper is terminated (SIGTERM handler)
│
└── App Host
    ├── Rust binary mode: exec the embedded binary
    └── Tauri mode: launch the Tauri app entry point
```

### 3. deotp SDK (Rust Crate)

A lightweight crate that apps link against:

```rust
// In the embedded app's main loop or startup:
deotp::heartbeat(); // panics if wrapper is not alive
let info = deotp::license_info(); // returns tokenId, wallet, app_id
```

The SDK communicates with the wrapper over a Unix domain socket (Linux/macOS) or named pipe (Windows). If the wrapper process dies, `heartbeat()` fails and the app exits.

### 4. deotp CLI

Packaging tool for developers:

```
deotp pack \
  --binary ./target/release/myapp \
  --app-id com.example.myapp \
  --contract 0x1234...abcd \
  --chain base \
  --output ./dist/myapp-wrapped
```

Produces a single distributable binary containing:
- The wrapper runtime
- The embedded app binary
- Configuration (app_id, contract address, chain RPC endpoints)

### 5. Tauri Plugin

```rust
// In a Tauri app's setup:
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_deotp::init())
        .run(tauri::generate_context!())
        .expect("error running app");
}
```

The plugin handles heartbeat automatically in the Tauri event loop and exposes license info to the frontend via IPC commands.

## Activation Flow (detailed)

```
First Launch:
┌─────────┐         ┌─────────┐         ┌──────────┐
│ Wrapper  │         │ Wallet  │         │  Chain   │
└────┬────┘         └────┬────┘         └─────┬────┘
     │                    │                     │
     │  Show activation   │                     │
     │  prompt (webview   │                     │
     │  or QR code)       │                     │
     │───────────────────▶│                     │
     │                    │                     │
     │  User approves     │                     │
     │  connection        │                     │
     │◀───────────────────│                     │
     │                    │                     │
     │  Query ownerOf()   │                     │
     │──────────────────────────────────────────▶
     │                    │                     │
     │  Confirm ownership │                     │
     │◀─────────────────────────────────────────│
     │                    │                     │
     │  Sign message:     │                     │
     │  H(app_id ||       │                     │
     │    tokenId ||      │                     │
     │    machine_id)     │                     │
     │───────────────────▶│                     │
     │                    │                     │
     │  Return signature  │                     │
     │◀───────────────────│                     │
     │                    │                     │
     │  Store license     │                     │
     │  proof locally     │                     │
     │  Launch app ✓      │                     │
     ▼                    ▼                     ▼
```

```
Subsequent Launches (offline):
┌─────────┐
│ Wrapper  │
└────┬────┘
     │
     │  Read ~/.deotp/licenses/<app_id>.json
     │  Re-derive machine_id
     │  Verify signature against stored address
     │
     │  ✓ Valid → launch embedded app
     │  ✗ Invalid → show activation prompt
     ▼
```

## License Proof Format

```json
{
  "app_id": "com.example.myapp",
  "token_id": 42,
  "wallet_address": "0xabc...123",
  "machine_id": "sha256:...",
  "signature": "0x...",
  "activated_at": "2026-04-07T12:00:00Z",
  "chain": "base",
  "contract": "0x1234...abcd"
}
```

## Machine ID Derivation

Cross-platform hardware fingerprint combining:
- **Linux**: `/sys/class/dmi/id/product_uuid` + MAC address of first non-virtual NIC
- **macOS**: `IOPlatformUUID` from IOKit
- **Windows**: `MachineGuid` from registry + SMBIOS UUID

Hashed with SHA-256 and salted with the app_id to prevent cross-app tracking:
```
machine_id = SHA256(platform_uuid || primary_mac || app_id)
```

## Security Model

**What this protects against:**
- Casual piracy (can't just copy the binary and share it — activation is machine-bound)
- License sharing (each activation is tied to a specific machine)
- Tampering detection (signature verification fails if any component is modified)

**What this does NOT protect against:**
- Determined reverse engineering (binary is not encrypted, can be patched)
- Memory dumping (the running app is in cleartext memory)
- Virtual machine cloning (VM can snapshot a machine_id)

This is opt-in, honest DRM. The goal is to make paying easier than pirating, not to make pirating impossible.

## Scaling Considerations

- Contract deployment: one per app, costs ~$5-20 on L2
- Chain RPC: one call per activation (not per launch). Can use public RPCs or Alchemy free tier.
- No backend servers to scale. The wrapper is fully client-side after activation.
- License proofs are ~500 bytes. Storage is negligible.
