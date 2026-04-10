# deotp — Implementation Plan

## Phase 1: Proof of Concept

Goal: A working wrapper that gates a Rust binary behind wallet ownership, using a cached SIWE-style session.

### 1.1 — Wrapper skeleton (Rust) `[implemented]`
- `deotp-wrapper` Rust project with CLI: `deotp-wrapper --binary <path>`
- Launches embedded app as child process
- SIGTERM/SIGCHLD handling: wrapper kills child on exit, child exits if wrapper dies
- Process supervision model proven

### 1.2 — Session verification
- Define session JSON schema (`session.rs`)
- Implement signature verification: recover signer address from ECDSA signature over `keccak256(app_id || token_id || nonce || expires_at)`
- Use `k256` crate for secp256k1
- Check `expires_at` against current time
- Result: valid session → launch app, invalid/expired → trigger wallet flow

### 1.3 — Wallet connection + session creation
- Embed minimal webview (`wry`) with WalletConnect UI
- On connect: query `ownerOf(tokenId)` or `isValid(tokenId)` via `alloy` RPC
- If ownership confirmed: generate nonce + `expires_at` (now + session_ttl)
- Request wallet signature over session payload
- Write session to `~/.deotp/sessions/<app_id>.json`
- Close webview, launch app

### 1.4 — ENS verification
- At session creation, resolve developer ENS via `alloy`
- Compare resolved address to embedded contract address
- Refuse session creation on mismatch, show warning in activation UI
- Display ENS name prominently in wallet connection UI

### 1.5 — Smart contracts
- `DeotpAccess.sol` — ERC-721, payable `purchase(address recipient)`, `bytes32 wrapperHash`
- `DeotpSubscription.sol` — ERC-721 + `expiresAt` mapping, payable `purchase()` and `renew(tokenId)`
- OpenZeppelin base contracts, Foundry project
- Deploy to Base Sepolia for development
- `isValid(tokenId)` view function on subscription contract

### 1.6 — Purchase UI
- In-wrapper purchase flow: if no token found in wallet, show purchase option
- Display price, contract details, ENS identity
- Call `purchase(recipient)` with connected wallet
- After tx confirms, proceed to session creation

**Phase 1 deliverable:** A wrapped binary that requires wallet ownership + session signature to run, with ENS verification, session caching, and automatic renewal on expiry.

---

## Phase 2: Developer Tooling

### 2.1 — deotp CLI (`deotp pack`)
- Input: compiled binary, app_id, contract address, chain config, session TTL
- Output: single distributable binary (wrapper + embedded app + config)
- Binary packing via `include_bytes!` at pack time or compressed payload extracted on first run
- Cross-platform output targets

### 2.2 — deotp CLI (`deotp deploy`)
- Deploy `DeotpAccess` or `DeotpSubscription` contract to target chain
- Configurable: price, supply cap, period (subscription), wrapperHash
- Outputs deployed contract address
- Requires `cast`/Foundry or uses bundled deployer via `alloy`

```
deotp deploy --type access --price 0.05 --chain base
deotp deploy --type subscription --price 0.01 --period 30 --chain base
```

### 2.3 — deotp SDK crate
- `deotp::heartbeat()` — panics if wrapper not alive (Unix socket / named pipe)
- `deotp::session()` — returns `SessionInfo { token_id, wallet, app_id, expires_at }`
- Socket path passed as env var by wrapper
- Minimal dependency footprint — should not pull in `alloy` or `wry`

### 2.4 — ENS + deotp registry
- Deploy `DeotpRegistry` on Base
- `register(appName, contractAddress)` — proves ownership, sets `appName.deotp.eth` subdomain
- CLI: `deotp register --name myapp --contract 0x...`
- Wrapper shows "verified on deotp.eth" badge when registry entry resolves

**Phase 2 deliverable:** Developer can deploy, pack, register, and distribute a wallet-gated app with a handful of CLI commands.

---

## Phase 3: Tauri Integration

### 3.1 — Tauri plugin (`tauri-plugin-deotp`)
- Auto-heartbeat in Tauri event loop
- Session renewal flow rendered inside the app's own webview — no separate window
- Frontend JS API:
  ```js
  const session = await invoke('plugin:deotp|session');
  // { token_id, wallet, expires_at }
  ```
- Emits `deotp://session-renewed` event when TTL is refreshed in background

### 3.2 — Tauri starter template
- `create-deotp-app` scaffold
- Pre-configured with `tauri-plugin-deotp`, contract config placeholders, wallet connection UI component
- Works out of the box against Base Sepolia

**Phase 3 deliverable:** Tauri developers add wallet-gated access with a plugin and a few lines of config.

---

## Phase 4: Polish and Hardening

### 4.1 — Background session renewal
- Wrapper monitors `expires_at` and triggers renewal in the background N hours before expiry
- User prompted via OS notification: "Your session expires soon — reconnect wallet to continue"
- App continues running during renewal; suspension only if renewal is declined or fails

### 4.2 — Windows support
- Named pipes instead of Unix domain sockets for heartbeat IPC
- MSVC build target for wrapper
- WalletConnect webview tested on Windows WebView2

### 4.3 — Subscription renewal UI
- In-wrapper subscription management: view expiry, renew from the tray/menu
- `deotp::session().expires_at` exposed to app for in-app renewal prompts

### 4.4 — Multi-wallet support
- User can associate multiple wallets with a session (e.g. hardware wallet for ownership, hot wallet for daily use)
- Pattern: hot wallet signs sessions, ownership wallet proves NFT ownership once — requires a delegation mechanism (EIP-7702 or a simple delegation registry)
- Phase 4 exploration — not required for core functionality

### 4.5 — Binary obfuscation (optional)
- UPX-style compression to raise the bar for casual extraction
- Documented as a deterrent, not a guarantee

---

## Tech Stack

| Component | Technology |
|---|---|
| Wrapper runtime | Rust |
| Crypto (secp256k1) | `k256` crate |
| Ethereum RPC | `alloy` crate |
| Webview (wallet connection) | `wry` crate |
| IPC (wrapper ↔ app) | Unix domain sockets / named pipes |
| Smart contracts | Solidity, OpenZeppelin, Foundry |
| Target chain | Base (primary). Config-abstracted for other EVM L2s |
| CLI | `clap` crate |
| Packaging | `include_bytes!` embedding or custom bundler |

---

## Directory Structure

```
deotp/
├── crates/
│   ├── deotp-wrapper/        # Wrapper runtime: session, wallet, supervisor
│   ├── deotp-sdk/            # Crate apps link against (heartbeat, session info)
│   ├── deotp-cli/            # Developer tooling (pack, deploy, register)
│   └── tauri-plugin-deotp/   # Tauri integration
├── contracts/
│   ├── src/
│   │   ├── DeotpAccess.sol       # ERC-721 one-time purchase
│   │   ├── DeotpSubscription.sol # ERC-721 with expiry + renewal
│   │   └── DeotpRegistry.sol     # deotp.eth subdomain registry
│   ├── test/
│   └── foundry.toml
├── examples/
│   ├── hello-rust/           # Minimal Rust app — one-time access
│   ├── hello-subscription/   # Minimal Rust app — subscription
│   └── hello-tauri/          # Minimal Tauri app with plugin
└── docs/
```
