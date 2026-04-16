# rub3 — Implementation Plan

## Phase 1: Proof of Concept

Goal: A working wrapper that gates a Rust binary behind wallet ownership, using a cached SIWE-style session.

### 1.1 — Wrapper skeleton `[complete]`
- `rub3-wrapper` Rust project with CLI: `rub3-wrapper --binary <path>` (clap)
- Launches embedded app as child process (`supervisor.rs`)
- SIGTERM forwarding: wrapper forwards signals to child, exits when child exits
- Process supervision proven with integration tests

### 1.2 — License proof + signature verification `[complete]`
- License proof JSON schema (`license.rs`): `app_id`, `token_id`, `wallet_address`, `signature`, `activated_at`, `chain`, `contract`, optional `paid_by`
- Activation message: `SHA-256(app_id || token_id_be_bytes)` — deterministic, fixed-width
- Signature verification: `personal_sign` prefix (keccak256), secp256k1 ECDSA recovery via `k256`, address comparison
- Proof persistence (`store.rs`): save/load to `~/.rub3/licenses/<app_id>.json` or `$RUB3_LICENSE_DIR`
- Static and dynamic integration tests verify the full crypto pipeline natively in Rust (no external tools)
- Result: valid proof → launch app, invalid/missing → trigger activation flow

### 1.3 — Activation flow + webview `[partial]`
- Activation orchestration (`activation.rs`): check cached proof → verify → launch, or open activation window
- Native webview (`wry`/`tao`) with dark-themed activation UI (`assets/activation.html`)
- IPC message protocol: JS ↔ Rust (ready, connect, token_selected, signed, cancel, error)
- Screens: connect (address input) → token-select (when multiple tokens owned) → activate (message + signature input) → processing
- Activate screen surfaces the exact `personal_sign` preimage (hex) so the user knows what to sign in their wallet
- **Done:** manual wallet address input, `tokensOfOwner()` enumeration, multi-token selection UI, activation message display, manual signature paste, proof storage on success
- **Not yet done:** WalletConnect integration (requires WC v2 JS SDK + developer-supplied project ID)

### 1.4 — On-chain queries `[complete]`
- `rpc.rs`: `ownerOf(tokenId)`, `price()`, `balanceOf(owner)`, `tokenOfOwnerByIndex(owner, index)` via alloy JSON-RPC with minimal ABI (`IRub3License`)
- `tokens_of_owner(rpc_url, contract, owner)` enumerates all tokens held by a wallet via ERC-721Enumerable
- Synchronous wrapper over async alloy calls (`block_on` with single-threaded tokio runtime)
- Ownership check wired into webview `Connect` handler: 0 tokens → error, 1 → auto-proceed to activate, N → token-select screen
- ENS resolution remains a stub (`EnsNotSupported`) — deferred to §1.6 where it is the primary deliverable

### 1.5 — Smart contracts `[not started]`
- `Rub3Access.sol` — ERC-721 + ERC-721Enumerable, payable `purchase(address recipient)`, `bytes32 wrapperHash`, `uint8 identityModel`
- `Rub3Subscription.sol` — same base + `expiresAt` mapping, payable `purchase()` and `renew(tokenId)`, `isValid(tokenId)` view
- `identityModel`: `0 = access`, `1 = account` — set at deploy time, readable by wrapper
- OpenZeppelin base contracts, Foundry project
- Deploy to Base Sepolia for development

### 1.6 — Identity model + TBA derivation `[not started]`
- Read `identityModel` from contract at session creation (one RPC call, cached in session)
- Access model: `user_id = wallet_address`
- Account model: derive TBA address locally using ERC-6551 CREATE2 formula
  - Canonical registry: `0x000000006551c19487814612e58FE06813775758`
  - Inputs: `chainId`, `contract`, `tokenId`, `salt = 0`, `implementation` (set by developer at deploy)
  - Pure computation via `alloy` — no RPC call needed
- `user_id` is written into the session and passed to the embedded app via SDK

### 1.7 — Purchase UI `[not started]`
- In-wrapper purchase flow: if no token found in wallet, show purchase option
- Display price, contract details, ENS identity
- Call `purchase(recipient)` with connected wallet
- After tx confirms, proceed to activation

### 1.8 — On-chain cooldown + session model (tier 3) `[not started]`

Replaces the legacy `LicenseProof` flow with a full session model backed by an on-chain cooldown. An NFT holder can otherwise run a signing oracle to distribute fresh sessions to non-holders; a contract-enforced `activate()` cooldown rate-limits how many sessions a single token can mint. The wrapper reads cooldown state and encodes calldata — it never sends txs or holds keys.

**Expected contract interface** (not in this repo):
```solidity
mapping(uint256 => uint256) public lastActivationBlock;
uint256 public cooldownBlocks; // e.g. 1800 (~1hr on Base)

function activate(uint256 tokenId) external {
    require(ownerOf(tokenId) == msg.sender, "not owner");
    require(block.number - lastActivationBlock[tokenId] >= cooldownBlocks, "cooldown");
    lastActivationBlock[tokenId] = block.number;
    emit Activated(tokenId, msg.sender, block.number);
}
```

**Phase A — foundation modules** (testable in isolation)
- `session.rs` — session schema, `session_message()` hash construction, `verify_local()` signature recovery, `is_expired()`. Extends `LicenseProof` with `nonce`, `issued_at`, `expires_at`, and tier-3 activation proof fields (`activation_tx`, `activation_block`, `activation_block_hash`); adds tier-4 fields (`session_id`, `device_pubkey`) as `Option<T>`
- `session_store.rs` — `~/.rub3/sessions/<app_id>/<token_id>.json` with `RUB3_SESSION_DIR` override; `load_latest_session(app_id)` scans the directory for the most recent valid session (solves "don't know token_id at startup")
- Extract `personal_sign_hash`, `recover_address`, `public_key_to_address` to `pub(crate)` in `license.rs` for session reuse

**Phase B — RPC + IPC wiring**
- `rpc.rs` additions: `last_activation_block`, `cooldown_blocks`, `cooldown_ready` → `(is_ready, blocks_remaining)`, `encode_activate_calldata` (pure, no RPC), `get_tx_receipt`, `get_block_number`
- `webview.rs` new IPC variants: `ActivateTxSent { tx_hash }`, `SessionSigned { signature }` (replaces legacy `Signed`). Outbound JS: `onShowCooldown`, `onTxConfirmed`, `onProcessing`
- Connect handler: `owner_of` → `cooldown_ready` → `encode_activate_calldata` → `onShowCooldown`
- ActivateTxSent handler: spawn polling thread for `get_tx_receipt` (3 × 4s), extract block info on confirmation, build session message, send `onTxConfirmed`
- SessionSigned handler: assemble `Session`, `verify_local` → return `SessionSuccess { session }`
- `activation.rs` fast path: `load_latest_session` → `is_expired` + `verify_local` → launch; falls through to legacy `LicenseProof` path (zero-address contract) for backward compat
- `assets/activation.html` new screens: cooldown, tx-pending, sign-session

**Phase C — verification hardening**
- `session::verify_onchain(session, rpc_url)` — fetch tx receipt, confirm status=1, `to` matches contract, block hash matches session
- Probabilistic on-chain re-verify (~1 in 5 cold starts) to catch forged tx hashes without adding latency to every launch
- Tx polling: 30s timeout, revert detection with user-facing error

**Verification**
- `cargo test` — unit tests for session message determinism, sign/verify round-trip, expiry; RPC tests for calldata encoding and receipt parsing
- End-to-end against local Anvil: deploy minimal ERC-721 + cooldown contract, exercise connect → tx → sign → session persistence across restarts
- Cooldown enforcement: second activation within the window must revert
- Expiry: short-TTL session must re-activate after expiry
- Backward compat: legacy `LicenseProof` (zero-address contract) still launches

### 1.9 — Tier scaffold + feature flags `[complete]`

Branch: `feature/tier-scaffold`. The wrapper is a single crate with Cargo features selecting compile-time behavior. Packing a distributable picks one tier bundle; orthogonal add-ons (e.g. binary encryption) compose independently. See `architecture.md` §Security Tiers for tier semantics.

**Tier bundles** (pick exactly one at pack time):

| Feature | Composed capabilities |
|---|---|
| `tier-0` | — |
| `tier-1` | `session` |
| `tier-2` (default) | `session` + `onchain-read` |
| `tier-3` | `session` + `onchain-read` + `onchain-write` + `cooldown` |
| `tier-4` | `tier-3` + `device-key` |

**Composable capability flags:**
- `session` — session schema + persistence (pulls `rand`)
- `onchain-read` — `ownerOf`, view calls
- `onchain-write` — calldata encoding, tx receipt polling
- `cooldown` — cooldown interval check
- `device-key` — ephemeral secp256k1 device keypair + storage (pulls `keyring`)
- `binary-encryption` — AES-256-GCM ciphertext unwrap + in-memory exec (pulls `aes-gcm`); orthogonal, composes with tier-3+

**Module scaffolds** (all `unimplemented!()` stubs behind `#[cfg(feature = "...")]`):
- `session.rs`, `session_store.rs` — gated on `session`
- `device.rs` — gated on `device-key`; `StorageBackend` = File | Keychain | Enclave
- `decrypt.rs` — gated on `binary-encryption`; KEK derivation, AEK unwrap, AES-256-GCM decrypt, in-memory exec (`memfd_create`/`fexecve` on Linux, `$TMPDIR` 0700 + unlink on macOS, `CreateFileMapping` on Windows)

All five tier bundles + `binary-encryption` composition compile clean. The 15 existing lib tests pass under default features. The scaffold establishes the wiring; tier 3 behavior is implemented in §1.8, tier 4 and binary encryption in later phases.

**Phase 1 deliverable:** A wrapped binary that requires wallet ownership + session signature to run, with session caching, on-chain cooldown enforcement (tier 3), and automatic re-activation on expiry.

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

Current (implemented):

```
rub3/
├── crates/
│   └── rub3-wrapper/                 # Wrapper runtime
│       ├── src/
│       │   ├── main.rs               # CLI entry point, app constants
│       │   ├── lib.rs                # Public module re-exports (feature-gated)
│       │   ├── license.rs            # Proof schema, activation message, ECDSA verification
│       │   ├── store.rs              # Proof persistence (RUB3_LICENSE_DIR override)
│       │   ├── activation.rs         # Activation flow orchestration
│       │   ├── rpc.rs                # On-chain queries (ownerOf, price) via alloy
│       │   ├── webview.rs            # Native activation window (wry/tao), IPC
│       │   ├── supervisor.rs         # Child process lifecycle, signal forwarding
│       │   ├── session.rs            # [scaffold, feature = "session"] session schema + verify
│       │   ├── session_store.rs      # [scaffold, feature = "session"] session persistence
│       │   ├── device.rs             # [scaffold, feature = "device-key"] device keypair mgmt (tier 4)
│       │   └── decrypt.rs            # [scaffold, feature = "binary-encryption"] AES-256-GCM binary unwrap
│       ├── assets/
│       │   └── activation.html       # Activation UI
│       └── tests/
│           ├── helpers/mod.rs        # Wallet gen, signing, license creation
│           ├── integration.rs        # Wrapper binary tests
│           └── license_e2e.rs        # Static + dynamic license verification tests
├── licenses/
│   └── com.rub3.example.json         # Valid example license proof
├── scripts/
│   └── test-e2e.sh                   # Runs cargo test
├── architecture.md
├── implementation.md
├── ideation.md
└── testing.md
```

Planned (not yet created):

```
├── crates/
│   ├── rub3-sdk/            # Crate apps link against (heartbeat, session info)
│   ├── rub3-cli/            # Developer tooling (pack, deploy, register)
│   └── tauri-plugin-rub3/   # Tauri integration
├── contracts/
│   ├── src/
│   │   ├── Rub3Access.sol
│   │   ├── Rub3Subscription.sol
│   │   └── Rub3Registry.sol
│   └── foundry.toml
└── examples/
    ├── hello-rust/
    ├── hello-subscription/
    └── hello-tauri/
```
