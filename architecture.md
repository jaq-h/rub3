# deotp — Architecture

## Chain

**Base (Ethereum L2)** is the primary target chain.

| Why Base | Detail |
|---|---|
| User onboarding | Coinbase on-ramp — users buy ETH without bridging |
| ENS support | Resolves L1 ENS natively, critical for trust layer |
| Cost | $0.01–0.05 per mint/renewal transaction |
| Finality | ~2 sec soft confirmation |
| Rust crates | `alloy` is lean (~30 deps), handles RPC, ABI, ENS resolution |
| Wallet support | Native in Coinbase Wallet, MetaMask, Rainbow, and WalletConnect-compatible wallets |

Chain is abstracted behind config — switching to Arbitrum or another EVM L2 is a config change:

```toml
[chain]
name = "base"
rpc  = "https://mainnet.base.org"
chain_id = 8453
```

## System Overview

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────────────┐
│   Developer   │     │   Base (L2)       │     │        User          │
│              │     │                  │     │                      │
│  App binary   │     │  DeotpAccess      │     │  Wallet              │
│  deotp CLI    │────▶│  DeotpSubscription│◀────│  deotp Wrapper       │
│  ENS name     │     │  DeotpRegistry    │     │  Session Cache       │
│              │     │                  │     │  Embedded App        │
└──────────────┘     └──────────────────┘     └──────────────────────┘
```

## Core Concepts

### Wallet as Identity

There is no machine ID in the security model. The user's wallet is their identity. Proving ownership of an NFT in that wallet proves the right to run the application. This maps directly to how every other web3 context works — the desktop wrapper simply extends this model to native binaries.

### Session Model

Rather than verifying on-chain at every launch (slow, requires wallet UI), the wrapper uses a short-lived session:

```
session = {
  app_id:      "com.example.myapp",
  token_id:    42,
  wallet:      "0xabc...123",
  nonce:       "<random 32 bytes, wrapper-generated>",
  issued_at:   "2026-04-10T09:00:00Z",
  expires_at:  "2026-04-17T09:00:00Z",
  signature:   "0x<wallet ECDSA sig over keccak256(app_id || token_id || nonce || expires_at)>",
  chain:       "base",
  contract:    "0x1234...abcd"
}
```

On each launch the wrapper verifies the signature locally (fast, offline). When the session expires, it re-verifies on-chain and issues a new session. The session TTL is set by the developer in the wrapper config.

**Multi-device**: Each device holds its own session. Same wallet, different nonces, independent TTLs. No coordination needed.

**Transfer semantics**: When an NFT is sold, the new owner activates a fresh session. The old owner's sessions expire at their next TTL. No active revocation required.

### Session TTL as Developer Knob

```toml
[license]
session_ttl_days = 7   # default — wallet prompt once per week
```

| TTL | Behavior | Use case |
|---|---|---|
| 1 day | Daily re-auth | High-value tools, strict ownership enforcement |
| 7 days | Weekly (default) | Standard desktop software |
| 30 days | Monthly | Matches subscription billing cycle |

---

## Components

### 1. Smart Contracts

Two contract templates, enforced identically at the wrapper level.

#### DeotpAccess (one-time purchase)

Standard ERC-721 with payable `purchase(address recipient)`:
- Price per token, optional supply cap
- `recipient == address(0)` defaults to `msg.sender`
- `bytes32 wrapperHash` — SHA-256 of distributed binary
- Transferrable — selling the NFT transfers access

On-chain check: `ownerOf(tokenId) == walletAddress`

#### DeotpSubscription (recurring)

ERC-721 extended with time-based validity:
- `mapping(uint256 => uint256) public expiresAt`
- `purchase()` sets `expiresAt[tokenId] = block.timestamp + period`
- `renew(uint256 tokenId)` payable, extends by one period
- Transferrable — new owner can renew

On-chain check: `ownerOf(tokenId) == walletAddress && block.timestamp < expiresAt[tokenId]`

#### DeotpRegistry

Permissionless registry mapping app names to contracts under `deotp.eth`:

```solidity
contract DeotpRegistry {
    function register(string calldata appName, address licenseContract) external {
        require(IOwnable(licenseContract).owner() == msg.sender, "not contract owner");
        // sets appName.deotp.eth → licenseContract
    }
}
```

---

### 2. deotp Wrapper Runtime

The core product. A Rust binary that manages wallet sessions and gates the embedded application.

```
deotp-wrapper
├── Session Manager
│   ├── Read cached session from ~/.deotp/sessions/<app_id>.json
│   ├── Verify session signature (local, fast)
│   ├── Check session expiry
│   ├── On expiry/absence: trigger wallet connection flow
│   └── Write renewed session to disk
│
├── Wallet Connection
│   ├── Open embedded webview (wry) with WalletConnect UI
│   ├── On connect: query ownerOf() or isValid() via alloy RPC
│   ├── Generate nonce + expires_at
│   ├── Request ECDSA signature from wallet
│   └── Store session, close webview
│
├── ENS Verification
│   ├── Resolve developer ENS at session creation
│   ├── Compare resolved address to embedded contract address
│   └── Refuse session creation on mismatch
│
├── Process Supervisor
│   ├── Launch embedded binary as child process
│   ├── Forward SIGTERM to child on wrapper exit
│   ├── Exit wrapper if child exits
│   └── Heartbeat IPC — child cannot run if wrapper dies
│
└── App Host
    ├── Rust binary mode: exec embedded binary
    └── Tauri mode: launch Tauri app entry point
```

#### Source layout

```
crates/deotp-wrapper/
├── src/
│   ├── main.rs          — CLI entry point
│   ├── session.rs       — session cache read/write/verify
│   ├── wallet.rs        — WalletConnect flow, RPC ownership check
│   ├── ens.rs           — ENS resolution and contract verification
│   ├── supervisor.rs    — child process lifecycle
│   └── webview.rs       — embedded wallet connection UI (wry)
└── tests/
    └── integration.rs
```

#### Dependencies

| Crate | Purpose |
|---|---|
| `clap` | CLI argument parsing |
| `alloy` | Ethereum RPC, ABI encoding, ENS resolution |
| `k256` | secp256k1 ECDSA signature verification |
| `wry` | Embedded webview for wallet connection UI |
| `serde_json` | Session cache serialization |
| `sha2` | SHA-256 for binary hash verification |
| `nix` | Unix signal handling |

---

### 3. deotp SDK (Rust Crate)

Lightweight crate apps link against:

```rust
deotp::heartbeat();            // panics if wrapper is not alive
let info = deotp::session();   // returns token_id, wallet, app_id, expires_at
```

Communication via Unix domain socket (path passed as env var by wrapper). If the wrapper process dies, `heartbeat()` fails and the app exits immediately.

---

### 4. deotp CLI

Developer tooling:

```
# Package a binary into a wrapped distributable
deotp pack \
  --binary ./target/release/myapp \
  --app-id com.example.myapp \
  --contract 0x1234...abcd \
  --chain base \
  --session-ttl 7 \
  --output ./dist/myapp

# Deploy a new license contract
deotp deploy --type access --price 0.05 --chain base
deotp deploy --type subscription --price 0.01 --period 30 --chain base

# Register in the deotp.eth registry
deotp register --name myapp --contract 0x1234...abcd
```

---

### 5. Tauri Plugin

```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_deotp::init())
        .run(tauri::generate_context!())
        .expect("error running app");
}
```

Frontend JS API:
```js
const session = await invoke('plugin:deotp|session');
// { token_id, wallet, expires_at }
```

The activation/renewal flow renders in the Tauri app's own webview. No separate window.

---

## ENS Trust Layer

Contract addresses are opaque hex strings. ENS provides human-readable identity that ties the contract to a developer's verifiable on-chain identity.

### How it works

```
Wrapper config embeds:
  contract: "0x1234...abcd"
  ens:      "myapp.eth"              # developer's ENS, OR
            "myapp.deotp.eth"        # deotp registry subdomain

At session creation, wrapper:
  1. Resolves ENS name → address
  2. Compares to embedded contract address
  3. Mismatch → refuse, warn user
  4. Match → proceed
```

### Two layers of trust

**Layer 1 — Developer's own ENS** (`myapp.eth`)
- Fully decentralized, developer controls it
- Trust comes from the developer's established identity

**Layer 2 — deotp.eth subdomain** (`myapp.deotp.eth`)
- Permissionless via `DeotpRegistry.register()` — developer proves contract ownership on-chain
- Adds "verified on deotp.eth" badge in the activation UI
- No manual approval — on-chain proof is sufficient

### Activation UI

```
┌────────────────────────────────────────────┐
│  Connect to My App                         │
│                                            │
│  Developer:   myapp.eth                    │
│  Contract:    0x1234...abcd (Base)         │
│  Registry:    ✓ verified on deotp.eth      │
│  Access:      One-time purchase            │
│  Price:       0.05 ETH                     │
│                                            │
│  Session valid for: 7 days                 │
│                                            │
│  [Connect Wallet]                          │
└────────────────────────────────────────────┘
```

---

## Binary Verification (Distribution Trust)

The license contract stores a SHA-256 hash of the distributed wrapper binary:

```solidity
bytes32 public wrapperHash;

function setWrapperHash(bytes32 hash) external onlyOwner {
    wrapperHash = hash;
}
```

Trust chain: **ENS → contract → binary hash → running wrapper**

Users can verify their download against the on-chain hash before running. This closes the distribution trust gap without a centralized app store or code signing authority.

---

## Launch Flow

```
App launch:
┌─────────────────────────────────────────────────────────┐
│                      Wrapper starts                     │
└────────────────────────┬────────────────────────────────┘
                         │
              Read ~/.deotp/sessions/<app_id>.json
                         │
              ┌──────────┴──────────┐
          Session valid?        No session /
          Sig OK + not expired   Expired / Invalid
              │                      │
         Launch app              Open webview
              │                      │
              │               Connect wallet (WalletConnect)
              │                      │
              │               Resolve ENS → verify contract
              │                      │
              │               ownerOf() / isValid() on-chain
              │                      │
              │                ┌─────┴─────┐
              │            Owns token    No token
              │                │              │
              │           Request sig    Show purchase UI
              │           (SIWE-style)        │
              │                │         User purchases →
              │           Cache session   loop back
              │                │
              └────────────────┘
                    Launch app
```

```
While running:
  Wrapper ──heartbeat IPC──▶ App (every 5s)
  App panics/exits if heartbeat stops
  Wrapper exits if app exits
```

---

## Session Format

```json
{
  "app_id":     "com.example.myapp",
  "token_id":   42,
  "wallet":     "0xabc...123",
  "nonce":      "a3f8...c921",
  "issued_at":  "2026-04-10T09:00:00Z",
  "expires_at": "2026-04-17T09:00:00Z",
  "signature":  "0x...",
  "chain":      "base",
  "contract":   "0x1234...abcd"
}
```

The signature is `wallet.sign(keccak256(app_id || token_id || nonce || expires_at))`.

The wrapper verifies this locally on every launch. No RPC call needed until the session expires.

---

## Security Model

### Wallet as trust boundary

The wrapper never holds a private key. All signing happens in the user's wallet (separate process or device) via WalletConnect. A session signature is free (no on-chain effect) — it cannot move assets.

**Spending the NFT requires a wallet transaction** the user must explicitly approve. A compromised wrapper cannot silently transfer the NFT.

### Threat model

| Attack | Mitigation |
|---|---|
| Copy session file to another machine | Nonce is single-use; new session requires wallet re-auth. Session is not machine-bound but is wallet-bound — you'd need the wallet too. |
| Replay expired session | `expires_at` is verified locally on every launch |
| Redirect payment to wrong contract | ENS resolution at session creation catches address mismatch |
| NFT transferred, old user still has session | Session expires at TTL; ownership re-verified on renewal |
| Subscription lapsed, user still has session | `isValid()` checked at renewal — returns false, session not issued |
| Forged session signature | secp256k1 ECDSA — not forgeable without the wallet's private key |
| Compromised wrapper binary | ENS + on-chain binary hash allow users to detect tampering |

### What this does NOT protect against

- Determined reverse engineering (binary is not encrypted, can be patched)
- Memory dumping (running app is in cleartext memory)
- Users who share their wallet (wallet = identity, sharing a wallet is sharing an identity — same as any web3 context)

This is honest access control. The goal is to make paying easier than not paying, and to make the access model as familiar as the rest of web3.

### Defense layers summary

```
Distribution:  on-chain binary hash (verify download before running)
Identity:      ENS resolution (verify contract belongs to the developer)
Payment:       wallet transaction approval (user controls their wallet)
Session:       SIWE-style signature (proves ownership at session creation)
Enforcement:   session expiry + TTL (ownership re-verified periodically)
Runtime:       heartbeat IPC (app cannot run without wrapper alive)
```

---

## Scaling Considerations

- Contract deployment: one per app, ~$1–5 on Base
- RPC calls: one per session renewal (not per launch). Public RPCs or Alchemy free tier sufficient.
- Session files: ~400 bytes. Negligible storage.
- No backend. No database. No auth service. Fully client-side after initial deployment.
