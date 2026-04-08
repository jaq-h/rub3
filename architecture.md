# deotp — Architecture

## Chain

**Base (Ethereum L2)** is the primary target chain.

| Why Base | Detail |
|---|---|
| User onboarding | Coinbase on-ramp — users buy ETH and pay without bridging |
| ENS support | Resolves L1 ENS natively, critical for trust layer |
| Cost | $0.01-0.05 per mint transaction |
| Finality | ~2 sec soft confirmation |
| Rust crates | `alloy` is lean (~30 deps) and handles RPC, ABI, ENS resolution |
| Wallet support | Native in Coinbase Wallet (100M+ users), MetaMask, Rainbow, etc. |

The chain is abstracted behind configuration — switching to Arbitrum or another EVM L2 is a config change, not a code change:

```toml
[chain]
name = "base"
rpc = "https://mainnet.base.org"
chain_id = 8453
```

Solana was evaluated and rejected: no ENS equivalent, heavy Rust SDK (~150 deps), and the cost/speed advantages are negligible for one-time license purchases.

## System Overview

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────────┐
│   Developer   │     │   Base (L2)       │     │      User        │
│              │     │                  │     │                  │
│  App binary   │     │  ERC-721 License  │     │  Wallet          │
│  deotp CLI    │────▶│  Contract         │◀────│  deotp Wrapper   │
│  ENS name     │     │  ENS Registry     │     │  Embedded App    │
│              │     │  deotp Registry   │     │                  │
└──────────────┘     └──────────────────┘     └──────────────────┘
```

## Components

### 1. Smart Contract (existing infrastructure)

Standard ERC-721 with a payable `purchase(address recipient)` function. No custom logic needed beyond:
- Price per license
- Optional supply cap
- Mint to `recipient` (defaults to `msg.sender` if zero address is passed)
- `bytes32 wrapperHash` — SHA-256 of the distributed binary, set by the developer. Users can verify their download before running.

The `recipient` parameter decouples payment from delivery: the buyer pays with their funding wallet but the NFT lands in any address they specify — a fresh wallet, a colleague's wallet, or a gift recipient.

OpenZeppelin's ERC-721 template covers this. The contract is deployed once per application on Base.

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

## ENS Trust Layer

Contract addresses are opaque hex strings. Phishing contracts are trivial to deploy. ENS provides human-readable identity verification.

### How it works

Developer registers an ENS name and points it at their license contract. The wrapper resolves the name at activation time and verifies it matches the embedded contract address.

```
Wrapper config embeds:
  contract: "0x1234...abcd"
  ens: "myapp.eth"                    # developer's own ENS
  deotp_registry: "myapp.deotp.eth"   # optional, verified by deotp

At activation, wrapper resolves ENS:
  embedded contract ≠ ENS resolution → REFUSE TO ACTIVATE, warn user
```

### Two layers of trust

**Layer 1 — Developer's own ENS** (e.g., `myapp.eth`)
- Developer registers and controls it
- Fully decentralized, no approval process
- Trust comes from the developer's reputation

**Layer 2 — deotp.eth subdomain** (e.g., `myapp.deotp.eth`)
- Permissionless registration via a registry contract
- Developer proves contract ownership on-chain (calls `register()` from the deployer wallet)
- Adds a "verified" badge in the activation UI
- No manual approval — trust comes from on-chain proof

### Registry Contract

```solidity
contract DeotpRegistry {
    ENS public ens;
    bytes32 public rootNode;            // deotp.eth

    mapping(bytes32 => address) public licenses;
    mapping(address => address) public developers;

    function register(string calldata appName, address licenseContract) external {
        require(IOwnable(licenseContract).owner() == msg.sender, "not contract owner");
        bytes32 label = keccak256(bytes(appName));
        require(licenses[label] == address(0), "name taken");

        licenses[label] = licenseContract;
        developers[licenseContract] = msg.sender;

        // Set ENS subdomain: appName.deotp.eth → licenseContract
        bytes32 subnode = keccak256(abi.encodePacked(rootNode, label));
        ens.setSubnodeOwner(rootNode, label, address(this));
        resolver.setAddr(subnode, licenseContract);
    }
}
```

### Activation UI

```
┌────────────────────────────────────────────┐
│  Activate License                          │
│                                            │
│  Application: My App                       │
│  Developer:   myapp.eth                    │
│  Contract:    0x1234...abcd (Base)         │
│  Registry:    ✓ verified on deotp.eth      │
│                                            │
│  Price: 0.01 ETH (one-time)               │
│                                            │
│  Deliver license to:                       │
│  ┌──────────────────────────────────────┐  │
│  │ 0x... or ENS name  (leave blank for  │  │
│  │ paying wallet)                       │  │
│  └──────────────────────────────────────┘  │
│                                            │
│  [Connect Wallet]                          │
└────────────────────────────────────────────┘
```

If a recipient address is provided, the wrapper stores it in the license proof and uses it for the `ownerOf()` check at activation. The paying wallet never needs to reconnect after purchase.

### What ENS prevents

| Attack | Without ENS | With ENS |
|---|---|---|
| Scammer deploys copycat contract | User sees raw 0x address, can't distinguish | Wrapper resolves real ENS and warns on mismatch |
| Compromised wrapper with wrong contract | User has no way to verify | ENS resolution catches the mismatch |
| Developer domain hijack | N/A | ENS ownership is wallet-based, harder to hijack than DNS |

### What ENS does NOT prevent

- Users who ignore warnings
- Typosquatting (`myaap.deotp.eth`) — mitigated by minimum name length / dispute process
- Social engineering outside the wrapper (fake websites, Discord links)

## Binary Verification (Distribution Trust)

The license contract stores a SHA-256 hash of the distributed wrapper binary:

```solidity
contract DeotpLicense is ERC721 {
    bytes32 public wrapperHash;

    function setWrapperHash(bytes32 hash) external onlyOwner {
        wrapperHash = hash;
    }
}
```

Trust chain: **ENS → contract → binary hash → running wrapper**

User downloads the wrapper, hashes it, checks against the on-chain value at the ENS-resolved contract address. This closes the distribution trust gap without requiring a centralized app store.

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
  "paid_by": "0xdef...456",
  "machine_id": "sha256:...",
  "signature": "0x...",
  "activated_at": "2026-04-07T12:00:00Z",
  "chain": "base",
  "contract": "0x1234...abcd"
}
```

`wallet_address` is the address that owns the NFT and signed the activation message. `paid_by` records the funding wallet only when it differs from `wallet_address`; omitted otherwise.

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

### Wallet as trust boundary

The wrapper never holds the user's private key. All wallet interaction happens via WalletConnect or an embedded webview — the wallet is a separate process/device.

**License theft requires an on-chain transaction** (e.g., `transferFrom` or `setApprovalForAll`) that the user must explicitly approve in their wallet. Wallets clearly distinguish message signing (free, no on-chain effect) from transactions (costs gas, moves assets). A compromised wrapper cannot silently steal NFTs.

### What a compromised wrapper CAN do

| Attack | Mechanism | Severity |
|---|---|---|
| Redirect payment | Embed a different contract address | High — ENS resolution prevents this |
| Phish via typed data | Craft EIP-712 permits or gasless approvals | Medium — wallet rendering varies |
| Exfiltrate local data | Wrapper runs with user privileges | High — true of any compromised software |
| Steal license proof file | Copy `~/.deotp/licenses/` | Low — machine-bound, useless elsewhere |

### What this protects against
- Casual piracy (can't just copy the binary and share it — activation is machine-bound)
- License sharing (each activation is tied to a specific machine)
- Tampering detection (signature verification fails if any component is modified)
- Payment redirection (ENS resolution catches contract address mismatches)
- Distribution tampering (on-chain binary hash allows verification before running)

### What this does NOT protect against
- Determined reverse engineering (binary is not encrypted, can be patched)
- Memory dumping (the running app is in cleartext memory)
- Virtual machine cloning (VM can snapshot a machine_id)
- Users who ignore warnings

This is opt-in, honest DRM. The goal is to make paying easier than pirating, not to make pirating impossible.

### Defense layers summary

```
Distribution:  on-chain binary hash (verify download)
Identity:      ENS resolution (verify contract is legitimate)
Payment:       wallet transaction approval (user controls their wallet)
Activation:    wallet message signature (proves NFT ownership)
Enforcement:   machine-bound license proof (offline verification)
Runtime:       heartbeat IPC (app can't run without wrapper)
```

## Scaling Considerations

- Contract deployment: one per app, costs ~$1-5 on Base
- Chain RPC: one call per activation (not per launch). Can use public RPCs or Alchemy free tier.
- No backend servers to scale. The wrapper is fully client-side after activation.
- License proofs are ~500 bytes. Storage is negligible.
