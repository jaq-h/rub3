# rub3 — Implementation Plan

## Phase 1: Proof of Concept

Goal: A working wrapper that gates a Rust binary behind wallet ownership, using a cached SIWE-style session.

### 1.1 — Wrapper skeleton (Rust) `[implemented]`
- `rub3-wrapper` Rust project with CLI: `rub3-wrapper --binary <path>`
- Launches embedded app as child process
- SIGTERM/SIGCHLD handling: wrapper kills child on exit, child exits if wrapper dies
- Process supervision model proven

### 1.2 — Session verification
- Define session JSON schema (`session.rs`)
- Implement signature verification: recover signer address from ECDSA signature over `keccak256(app_id || token_id || user_id || nonce || expires_at)`
- Use `k256` crate for secp256k1
- Check `expires_at` against current time
- Session files keyed by token: `~/.rub3/sessions/<app_id>/<token_id>.json`
- Result: valid session → launch app, invalid/expired → trigger wallet flow

### 1.3 — Wallet connection + token selection + session creation
- Embed minimal webview (`wry`) with WalletConnect UI
- On connect: call `tokensOfOwner(wallet)` (ERC-721Enumerable) via `alloy` to get owned token IDs
- If 0 tokens: show purchase UI
- If 1 token: auto-select, proceed
- If >1 tokens: show token selector UI — display token IDs, active session indicator, TBA address (account model) or wallet (access model), expiry (subscription)
- On token selected: confirm `ownerOf()` / `isValid()`, read `identityModel` from contract
- Compute `user_id`: access → wallet address, account → ERC-6551 TBA address (local derivation, no RPC)
- Generate nonce + `expires_at` (now + session_ttl)
- Request wallet signature over `(app_id || token_id || user_id || nonce || expires_at)`
- Write session to `~/.rub3/sessions/<app_id>/<token_id>.json`
- Close webview, launch app

### 1.4 — ENS verification
- At session creation, resolve developer ENS via `alloy`
- Compare resolved address to embedded contract address
- Refuse session creation on mismatch, show warning in activation UI
- Display ENS name prominently in wallet connection UI

### 1.5 — Smart contracts
- `Rub3Access.sol` — ERC-721 + ERC-721Enumerable, payable `purchase(address recipient)`, `bytes32 wrapperHash`, `uint8 identityModel`
- `Rub3Subscription.sol` — same base + `expiresAt` mapping, payable `purchase()` and `renew(tokenId)`, `isValid(tokenId)` view
- `identityModel`: `0 = access`, `1 = account` — set at deploy time, readable by wrapper
- OpenZeppelin base contracts, Foundry project
- Deploy to Base Sepolia for development

### 1.6 — Identity model + TBA derivation (`identity.rs`)
- Read `identityModel` from contract at session creation (one RPC call, cached in session)
- Access model: `user_id = wallet_address`
- Account model: derive TBA address locally using ERC-6551 CREATE2 formula
  - Canonical registry: `0x000000006551c19487814612e58FE06813775758`
  - Inputs: `chainId`, `contract`, `tokenId`, `salt = 0`, `implementation` (set by developer at deploy)
  - Pure computation via `alloy` — no RPC call needed
- `user_id` is written into the session and passed to the embedded app via SDK

### 1.6 — Purchase UI
- In-wrapper purchase flow: if no token found in wallet, show purchase option
- Display price, contract details, ENS identity
- Call `purchase(recipient)` with connected wallet
- After tx confirms, proceed to session creation

**Phase 1 deliverable:** A wrapped binary that requires wallet ownership + session signature to run, with ENS verification, session caching, and automatic renewal on expiry.

---

## Phase 2: Developer Tooling

### 2.1 — rub3 CLI (`rub3 pack`)
- Input: compiled binary, app_id, contract address, chain config, session TTL
- Output: single distributable binary (wrapper + embedded app + config)
- Binary packing via `include_bytes!` at pack time or compressed payload extracted on first run
- Cross-platform output targets

### 2.2 — rub3 CLI (`rub3 deploy`)
- Deploy `Rub3Access` or `Rub3Subscription` to target chain
- `--identity access|account` sets `identityModel` in contract
- `--tba-implementation <address>` required when `--identity account` (ERC-6551 TBA implementation to use)
- Configurable: price, supply cap, period (subscription), wrapperHash
- Outputs deployed contract address

```
rub3 deploy --type access --identity account --tba-implementation 0x... --price 0.05 --chain base
rub3 deploy --type subscription --identity access --price 0.01 --period 30 --chain base
```

### 2.3 — rub3 SDK crate
- `rub3::heartbeat()` — panics if wrapper not alive (Unix socket / named pipe)
- `rub3::session()` — returns `SessionInfo`
  ```rust
  pub struct SessionInfo {
      pub app_id:     String,
      pub token_id:   u64,
      pub user_id:    String,        // stable identity: TBA (account) or wallet (access)
      pub wallet:     String,        // current signing wallet
      pub identity:   IdentityModel, // Access | Account
      pub expires_at: DateTime<Utc>,
  }
  ```
- Application code should key all persistent data on `user_id`, never on `wallet`
- Socket path passed as env var by wrapper
- Minimal dependency footprint — no `alloy` or `wry`

### 2.4 — ENS + rub3 registry
- Deploy `Rub3Registry` on Base
- `register(appName, contractAddress)` — proves ownership, sets `appName.rub3.eth` subdomain
- CLI: `rub3 register --name myapp --contract 0x...`
- Wrapper shows "verified on rub3.eth" badge when registry entry resolves

**Phase 2 deliverable:** Developer can deploy, pack, register, and distribute a wallet-gated app with a handful of CLI commands.

---

## Phase 3: Tauri Integration

### 3.1 — Tauri plugin (`tauri-plugin-rub3`)
- Auto-heartbeat in Tauri event loop
- Session renewal flow rendered inside the app's own webview — no separate window
- Frontend JS API:
  ```js
  const session = await invoke('plugin:rub3|session');
  // { token_id, wallet, expires_at }
  ```
- Emits `rub3://session-renewed` event when TTL is refreshed in background

### 3.2 — Tauri starter template
- `create-rub3-app` scaffold
- Pre-configured with `tauri-plugin-rub3`, contract config placeholders, wallet connection UI component
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
- `rub3::session().expires_at` exposed to app for in-app renewal prompts

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
rub3/
├── crates/
│   ├── rub3-wrapper/        # Wrapper runtime: session, wallet, supervisor
│   ├── rub3-sdk/            # Crate apps link against (heartbeat, session info)
│   ├── rub3-cli/            # Developer tooling (pack, deploy, register)
│   └── tauri-plugin-rub3/   # Tauri integration
├── contracts/
│   ├── src/
│   │   ├── Rub3Access.sol       # ERC-721 one-time purchase
│   │   ├── Rub3Subscription.sol # ERC-721 with expiry + renewal
│   │   └── Rub3Registry.sol     # rub3.eth subdomain registry
│   ├── test/
│   └── foundry.toml
├── examples/
│   ├── hello-rust/           # Minimal Rust app — one-time access
│   ├── hello-subscription/   # Minimal Rust app — subscription
│   └── hello-tauri/          # Minimal Tauri app with plugin
└── docs/
```
