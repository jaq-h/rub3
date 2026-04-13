# rub3 — Architecture

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

---

## Identity Models

The contract issuer chooses one of two identity models when deploying. This is the most fundamental design decision in rub3 — it determines what the NFT means to the application.

### Access Model (`identity = "access"`)

**`wallet_address` is the user identity.**

The NFT is a gate. Owning it proves the right to use the application. The user's wallet is their account. If the NFT is transferred, the new holder gets access but the old holder's session eventually expires — no account history moves.

Use when:
- The app has no persistent user data, or stores it server-side keyed on wallet address
- Transfer is expected to be uncommon (resale market, gifting)
- The developer wants the simplest possible model

Session identity field: `wallet_address`

### Account Model (`identity = "account"`)

**`token_id` is the user identity**, specifically via its ERC-6551 Token Bound Account (TBA).

The NFT is an account. Its TBA address is deterministic and permanent — it never changes regardless of who holds the NFT. The current holder controls the TBA (and therefore the account), but the account's identity is the TBA address, not the wallet address.

Use when:
- The app stores user data, preferences, or history
- Wallet rotation should not reset the user's account
- Transfer should sell the account to the buyer — they inherit the history
- The developer wants native web3 account composability

Session identity field: `tba_address` (deterministic TBA derived from token)

### TBA Address Derivation (ERC-6551)

The TBA address for any token is deterministic and computed locally — no on-chain call needed:

```
tba = CREATE2(
  registry:       0x000000006551c19487814612e58FE06813775758,  // canonical ERC-6551 registry
  implementation: <developer-chosen TBA implementation>,
  salt:           0,
  chainId:        8453,
  contract:       "0x1234...abcd",
  tokenId:        42
)
```

The wrapper computes this address from the token ID at session creation. The TBA may or may not be deployed — rub3 does not require it to be deployed, it only uses the address as a stable identity key.

If the developer wants the TBA to actually hold assets or execute transactions on behalf of the user, they deploy it separately. That is opt-in and outside rub3's scope.

---

## System Overview

```
┌──────────────┐     ┌─────────────────────┐     ┌──────────────────────────┐
│   Developer   │     │   Base (L2)          │     │        User              │
│              │     │                     │     │                          │
│  App binary   │     │  Rub3Access or      │     │  Wallet                  │
│  rub3 CLI    │────▶│  Rub3Subscription   │◀────│  rub3 Wrapper           │
│  ENS name     │     │  Rub3Registry       │     │  Token selector UI       │
│  identity=    │     │  ERC-6551 Registry   │     │  Session Cache           │
│  access|acct  │     │                     │     │  Embedded App            │
└──────────────┘     └─────────────────────┘     └──────────────────────────┘
```

---

## Session Model

Rather than verifying on-chain at every launch, the wrapper issues and caches a short-lived session after each wallet verification.

```
session = {
  app_id:       "com.example.myapp",
  token_id:     42,
  identity:     "access" | "account",

  -- access model --
  user_id:      "0xabc...wallet",

  -- account model --
  user_id:      "0xTBA...deterministic",   // TBA address, stable across transfers
  tba:          "0xTBA...deterministic",
  wallet:       "0xabc...current holder",  // used for signing only

  nonce:        "<random 32 bytes>",
  issued_at:    "2026-04-10T09:00:00Z",
  expires_at:   "2026-04-17T09:00:00Z",
  signature:    "0x<wallet ECDSA sig over keccak256(app_id || token_id || user_id || nonce || expires_at)>",
  chain:        "base",
  contract:     "0x1234...abcd"
}
```

`user_id` is what the application uses as a stable identity key. In access model it is the wallet address. In account model it is the TBA address.

The signature always comes from the current wallet (NFT holder). The wrapper verifies signature locally on each launch. On expiry, re-verifies on-chain.

**Multi-device**: Each device holds its own session. Same wallet, different nonces, independent TTLs.

**Transfer semantics**:
- Access model: new holder activates a fresh session with their wallet as `user_id`. Old sessions expire at TTL.
- Account model: new holder activates a fresh session. `user_id` (TBA) is unchanged. The application sees the same account with a new controller wallet.

### Session TTL

```toml
[license]
session_ttl_days = 7   # wallet prompt once per week
```

| TTL | Use case |
|---|---|
| 1 day | High-value tools, strict ownership enforcement |
| 7 days | Standard (default) |
| 30 days | Matches subscription billing cycle |

---

## Token Selection

A wallet may own multiple tokens from the same contract. At session creation (first launch or renewal), the wrapper presents a token selector after wallet connection.

```
┌────────────────────────────────────────────────┐
│  Connect to My App                             │
│                                                │
│  Developer:  myapp.eth  ✓ verified rub3.eth   │
│  Identity:   Account (NFT = your account)      │
│                                                │
│  Select which token to use:                    │
│                                                │
│  ┌──────────────────────────────────────────┐  │
│  │ ● Token #42   (active session)           │  │
│  │   Account: 0xTBA...a1b2   [selected]     │  │
│  ├──────────────────────────────────────────┤  │
│  │ ○ Token #91                              │  │
│  │   Account: 0xTBA...c3d4                  │  │
│  ├──────────────────────────────────────────┤  │
│  │ ○ Token #107                             │  │
│  │   Account: 0xTBA...e5f6                  │  │
│  └──────────────────────────────────────────┘  │
│                                                │
│  [Sign in with Token #42]                      │
└────────────────────────────────────────────────┘
```

For access model, the display omits the Account field and shows wallet address instead. For subscriptions, each token shows its expiry date.

If only one token is owned, the selector is skipped and that token is auto-selected.

If no tokens are owned, the purchase UI is shown instead.

**Implementation:** The wrapper calls `tokensOfOwner(wallet)` (ERC-721 Enumerable) to retrieve owned token IDs. If the contract does not implement enumerable, the wrapper falls back to scanning `Transfer` events filtered by recipient.

Session files are keyed on both app_id and token_id: `~/.rub3/sessions/<app_id>/<token_id>.json`. This allows each token to maintain its own cached session — switching between tokens at launch resumes the correct cached session without re-authenticating.

---

## Components

### 1. Smart Contracts

#### Rub3Access (one-time purchase)

ERC-721 + ERC-721Enumerable with payable `purchase(address recipient)`:
- Price per token, optional supply cap
- `recipient == address(0)` defaults to `msg.sender`
- `bytes32 wrapperHash` — SHA-256 of distributed binary
- `uint8 identityModel` — `0 = access`, `1 = account` — readable by wrapper

On-chain check: `ownerOf(tokenId) == walletAddress`

#### Rub3Subscription (recurring)

ERC-721 + ERC-721Enumerable extended with time-based validity:
- `mapping(uint256 => uint256) public expiresAt`
- `purchase()` sets `expiresAt[tokenId] = block.timestamp + period`
- `renew(uint256 tokenId)` payable, extends by one period
- `uint8 identityModel` — same flag as above

On-chain check: `ownerOf(tokenId) == walletAddress && block.timestamp < expiresAt[tokenId]`

Both contracts implement ERC-721Enumerable so the wrapper can call `tokensOfOwner()` directly.

#### Rub3Registry

Permissionless registry under `rub3.eth`:

```solidity
contract Rub3Registry {
    function register(string calldata appName, address licenseContract) external {
        require(IOwnable(licenseContract).owner() == msg.sender, "not contract owner");
        // sets appName.rub3.eth → licenseContract
    }
}
```

---

### 2. rub3 Wrapper Runtime

```
rub3-wrapper
├── Session Manager
│   ├── Read cached session ~/.rub3/sessions/<app_id>/<token_id>.json
│   ├── Verify session signature (local, fast)
│   ├── Check session expiry
│   ├── On expiry/absence: trigger wallet connection flow
│   └── Write renewed session to disk
│
├── Wallet Connection
│   ├── Open embedded webview (wry) with WalletConnect UI
│   ├── On connect: fetch tokensOfOwner(wallet) via alloy RPC
│   ├── Present token selector UI (skip if single token)
│   ├── On token selected: run ownerOf() / isValid() confirmation
│   ├── Read identityModel from contract
│   ├── Compute TBA address if account model
│   ├── Generate nonce + expires_at
│   ├── Request ECDSA signature over (app_id || token_id || user_id || nonce || expires_at)
│   └── Store session, close webview
│
├── ENS Verification
│   ├── Resolve developer ENS at session creation
│   ├── Compare to embedded contract address
│   └── Refuse on mismatch
│
├── Process Supervisor
│   ├── Launch embedded binary as child process
│   ├── Forward SIGTERM to child on wrapper exit
│   ├── Exit if child exits
│   └── Heartbeat IPC — child cannot run if wrapper dies
│
└── App Host
    ├── Rust binary mode: exec embedded binary
    └── Tauri mode: launch Tauri app entry point
```

#### Source layout (current)

```
crates/rub3-wrapper/
├── src/
│   ├── main.rs          — CLI entry point, app constants (APP_ID, CONTRACT, CHAIN_ID, RPC_URL)
│   ├── lib.rs           — public module re-exports (license, store, activation, supervisor)
│   ├── license.rs       — license proof schema, activation message, ECDSA signature verification
│   ├── store.rs         — proof persistence (~/.rub3/licenses/ or $RUB3_LICENSE_DIR)
│   ├── activation.rs    — activation flow: check proof → verify → launch or open webview
│   ├── rpc.rs           — on-chain queries (ownerOf, price) via alloy JSON-RPC
│   ├── supervisor.rs    — child process lifecycle, SIGTERM forwarding
│   └── webview.rs       — native activation window (wry/tao), JS↔Rust IPC
├── assets/
│   └── activation.html  — activation UI (connect, signature input, processing screens)
└── tests/
    ├── helpers/mod.rs   — test utilities (wallet gen, signing, license creation)
    ├── integration.rs   — wrapper binary tests (exit codes, args, missing binary)
    └── license_e2e.rs   — static + dynamic license tests, SIGTERM forwarding
```

Planned but not yet created: `session.rs`, `wallet.rs`, `identity.rs`, `token_select.rs`, `ens.rs`

#### Dependencies

| Crate | Purpose |
|---|---|
| `clap` | CLI argument parsing |
| `alloy` | Ethereum RPC, ABI encoding (ownerOf, price) |
| `k256` | secp256k1 ECDSA signature recovery |
| `sha2` | SHA-256 for activation message hash |
| `sha3` | Keccak-256 for Ethereum address derivation + personal_sign |
| `hex` | Hex encoding/decoding |
| `wry` | Embedded webview for activation UI |
| `tao` | Native window/event loop |
| `serde` / `serde_json` | License proof serialization |
| `dirs` | Platform data directory resolution |
| `chrono` | RFC-3339 timestamps |
| `nix` / `libc` | Unix signal handling |

---

### 3. rub3 SDK (Rust Crate)

```rust
rub3::heartbeat();              // panics if wrapper is not alive
let info = rub3::session();     // returns SessionInfo

pub struct SessionInfo {
    pub app_id:    String,
    pub token_id:  u64,
    pub user_id:   String,   // wallet (access) or TBA (account) — stable identity key
    pub wallet:    String,   // current signing wallet, may differ from user_id in account model
    pub identity:  IdentityModel,
    pub expires_at: DateTime<Utc>,
}

pub enum IdentityModel { Access, Account }
```

The `user_id` field is what application code should use for all persistent data keying. It is always stable for the account model and stable-per-holder for the access model.

---

### 4. rub3 CLI

```
rub3 pack \
  --binary ./target/release/myapp \
  --app-id com.example.myapp \
  --contract 0x1234...abcd \
  --chain base \
  --session-ttl 7 \
  --output ./dist/myapp

rub3 deploy --type access --identity account --price 0.05 --chain base
rub3 deploy --type subscription --identity access --price 0.01 --period 30 --chain base

rub3 register --name myapp --contract 0x1234...abcd
```

`--identity` at deploy time sets the `identityModel` flag in the contract. The wrapper reads this flag on-chain at session creation.

---

### 5. Tauri Plugin

```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_rub3::init())
        .run(tauri::generate_context!())
        .expect("error running app");
}
```

Frontend JS API:
```js
const session = await invoke('plugin:rub3|session');
// {
//   token_id:   42,
//   user_id:    "0xTBA..." | "0xwallet...",
//   wallet:     "0xwallet...",
//   identity:   "account" | "access",
//   expires_at: "2026-04-17T09:00:00Z"
// }
```

Token selection and renewal flow render in the Tauri app's own webview.

---

## ENS Trust Layer

### How it works

```
Wrapper config embeds:
  contract: "0x1234...abcd"
  ens:      "myapp.eth"           # developer's own ENS, OR
            "myapp.rub3.eth"     # rub3 registry subdomain

At session creation:
  1. Resolve ENS → address
  2. Compare to embedded contract address
  3. Mismatch → refuse, warn user
  4. Match → proceed
```

### Two layers of trust

**Layer 1 — Developer's own ENS** (`myapp.eth`) — decentralized, developer-controlled.

**Layer 2 — rub3.eth subdomain** (`myapp.rub3.eth`) — permissionless, on-chain proof of contract ownership. Adds "verified" badge in UI.

---

## Binary Verification (Distribution Trust)

```solidity
bytes32 public wrapperHash;

function setWrapperHash(bytes32 hash) external onlyOwner {
    wrapperHash = hash;
}
```

Trust chain: **ENS → contract → binary hash → running wrapper**

---

## Launch Flow

```
Wrapper starts
    │
    Read ~/.rub3/sessions/<app_id>/<token_id>.json
    │                           (token_id from last session, or none)
    │
    ┌───────────────┴───────────────┐
Session valid?                  No session /
Sig OK + not expired            Expired / Invalid
    │                               │
Launch app                      Open webview
                                    │
                            Connect wallet (WalletConnect)
                                    │
                            Resolve ENS → verify contract
                                    │
                            tokensOfOwner(wallet) → token list
                                    │
                         ┌──────────┴──────────┐
                    0 tokens               ≥1 token
                         │                     │
                  Show purchase UI      Show token selector
                         │              (auto-select if 1 token)
                  User purchases             │
                  → loop back          User selects token
                                            │
                                    ownerOf() / isValid()
                                            │
                                    Read identityModel from contract
                                            │
                                    Compute user_id:
                                    access → wallet_address
                                    account → TBA address
                                            │
                                    Request session signature
                                            │
                                    Cache session
                                            │
                                       Launch app

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
  "identity":   "account",
  "user_id":    "0xTBA...deterministic",
  "tba":        "0xTBA...deterministic",
  "wallet":     "0xabc...123",
  "nonce":      "a3f8...c921",
  "issued_at":  "2026-04-10T09:00:00Z",
  "expires_at": "2026-04-17T09:00:00Z",
  "signature":  "0x...",
  "chain":      "base",
  "contract":   "0x1234...abcd"
}
```

Access model omits `tba` and sets `user_id` to the wallet address.

Signature covers: `keccak256(app_id || token_id || user_id || nonce || expires_at)`.

Session files stored at `~/.rub3/sessions/<app_id>/<token_id>.json` — one per token, not one per app.

---

## Security Model

### Wallet as trust boundary

The wrapper never holds a private key. Signing happens in the wallet via WalletConnect. Session signatures are free — no on-chain effect.

### Threat model

| Attack | Mitigation |
|---|---|
| Copy session file to another machine | Nonce is single-use; new session requires wallet re-auth |
| Replay expired session | `expires_at` verified locally on every launch |
| Redirect payment to wrong contract | ENS resolution at session creation |
| NFT transferred, old user still has session | Session expires at TTL; ownership re-verified on renewal |
| Subscription lapsed, session still valid | `isValid()` checked at renewal — returns false, session not issued |
| Account model: attacker has session file | `user_id` is a TBA address — useless without the wallet that controls the NFT |
| Forged session signature | secp256k1 ECDSA — requires wallet private key |
| Compromised wrapper | ENS + on-chain binary hash allow detection |

### Account model: what transfer means to security

In account model, the TBA address (`user_id`) is stable across transfers. An attacker who obtains a cached session file gets a `user_id` that is currently controlled by someone else's wallet. The session signature verifies against the wallet that signed it — after transfer, the old wallet no longer controls the NFT, but the session remains valid until TTL.

This is intentional and matches the semantics: **transfer sells the account to the new holder, who takes full control at the next session renewal.** The old holder's session is a time-limited lame duck.

### Defense layers summary

```
Distribution:  on-chain binary hash (verify download)
Identity:      ENS resolution (verify developer identity)
Payment:       wallet transaction approval
Session:       SIWE-style signature (proves ownership at creation)
Enforcement:   session TTL (periodic on-chain re-verification)
Runtime:       heartbeat IPC (app cannot run without wrapper)
```

---

## Scaling Considerations

- Contract deployment: one per app, ~$1–5 on Base
- RPC calls: one per session renewal (`tokensOfOwner` + `ownerOf`/`isValid`). Public RPC or Alchemy free tier sufficient.
- Session files: ~500 bytes each, one per token per device. Negligible storage.
- No backend. No database. No auth service.
