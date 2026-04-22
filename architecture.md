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

## Security Tiers

The developer chooses a security tier when packaging their app. Each tier is a coherent bundle of verification behaviors — higher tiers add on-chain enforcement and device binding to prevent license sharing.

```toml
[license]
tier = "cooldown"           # offline | cached | verified | cooldown | hardened
session_ttl_days = 7        # tiers 1-3 (ignored by tier 0 and 4)
cooldown_blocks = 1800      # tiers 3-4 (~1hr on Base at 2s/block); min 15 (~30s, one TOTP window)
offline_grace_hours = 24    # tiers 2-3: allow launch without network within window
device_key_storage = "keychain"  # tier 4: "file" | "keychain" | "enclave"
```

### Tier overview

| Tier | Name | Network at launch | On-chain writes | Piracy resistance | Use case |
|------|------|-------------------|-----------------|-------------------|----------|
| 0 | `offline` | Never | 0 | File copy defeats it | Free/honor-system, offline-first tools |
| 1 | `cached` | At activation + renewal | 0 | Shared file works until TTL | Low-value desktop apps, long TTL (30d) |
| 2 | `verified` | At activation + every launch | 0 | Shared file fails if token transfers | Standard apps, moderate value |
| 3 | `cooldown` | At activation + every launch | 1 per activation | 1 session per cooldown window, new activation kills old | SaaS-equivalent, subscriptions |
| 4 | `hardened` | Every launch | 1 per activation | Session bound to hardware device key, non-transferable | High-value tools, trading software |

### Tier 0: `offline`

Signature-only verification. The wallet signs once at activation, the proof is stored locally, and the wrapper never contacts the chain again. Anyone who copies the proof file can use the software.

- **Hash inputs**: `SHA-256(app_id || token_id)`
- **Verification**: ECDSA recovery — recovered address must match `wallet_address` in proof
- **Session file**: `~/.rub3/licenses/<app_id>.json`
- **Threat model**: Trusts the user. Suitable for open-source tools that want a soft gate or honor-system monetization.

### Tier 1: `cached`

Adds a session with TTL. The wallet signs a session message at activation. The wrapper checks signature and expiry locally on each launch. On expiry, the user must re-authenticate with their wallet (re-sign). No on-chain calls at launch.

- **Hash inputs**: `SHA-256(app_id || token_id || wallet || nonce || expires_at)`
- **Verification**: Signature recovery + expiry check
- **Renewal**: Wallet re-signs a new session (off-chain, no gas)
- **Sharing risk**: Copied session file works until `expires_at`. Setting a shorter TTL reduces the window.

### Tier 2: `verified`

Adds an `ownerOf()` RPC read on every launch. The wrapper confirms the wallet in the session still owns the NFT on-chain. If the token has been transferred, the session is invalid.

- **Hash inputs**: Same as tier 1
- **Verification**: Signature + expiry + `ownerOf(tokenId)` view call (free, no gas)
- **Offline grace**: If network is unavailable, the wrapper allows launch if the session was last verified within `offline_grace_hours`. Set to 0 to require network on every launch.
- **Sharing risk**: Copied session works only if the original wallet still owns the token. A signing oracle (holder signs for pirates) still works because the holder is the real owner.

### Tier 3: `cooldown`

Adds on-chain activation with a cooldown and session revocation counter. At activation, the wallet sends an `activate()` transaction that records the current block and increments a `sessionId` on-chain. Only one session per token is valid at a time — creating a new one invalidates the old one.

- **Hash inputs**: `SHA-256(app_id || token_id || wallet || nonce || expires_at || activation_block_hash || session_id)`
- **On-chain state**:
  ```solidity
  mapping(uint256 => uint256) public lastActivationBlock;
  mapping(uint256 => uint256) public activeSessionId;
  uint256 public immutable cooldownBlocks;
  uint256 public constant MIN_COOLDOWN_BLOCKS = 15; // ~30s on Base; one TOTP window

  function activate(uint256 tokenId) external returns (uint256 sessionId) {
      require(ownerOf(tokenId) == msg.sender, "not owner");
      uint256 last = lastActivationBlock[tokenId];
      if (last != 0) require(block.number - last >= cooldownBlocks, "cooldown");
      lastActivationBlock[tokenId] = block.number;
      activeSessionId[tokenId] = ++_sessionCounter;
      return activeSessionId[tokenId];
  }
  ```
- **Verification**: Signature + expiry + `ownerOf()` + `activeSessionId()` view call. If session_id doesn't match on-chain value, the session has been superseded.
- **Sharing risk**: Holder can generate 1 session per cooldown window. Creating a session for a pirate kills the holder's own session. The holder must choose: keep access or give it away. Cannot scale to multiple pirates.

### Tier 4: `hardened`

Adds a device-bound ephemeral keypair. At activation, the wrapper generates a fresh secp256k1 keypair (the "device key"). The public key is registered on-chain alongside the session. At every launch, the wrapper signs the current block hash with its device key and verifies the signature matches the on-chain registered public key.

- **Hash inputs**: `SHA-256(app_id || token_id || wallet || nonce || expires_at || activation_block_hash || session_id || device_pubkey)`
- **On-chain state**:
  ```solidity
  mapping(uint256 => bytes32) public registeredDevice;

  function activate(uint256 tokenId, bytes32 devicePubKey) external returns (uint256 sessionId) {
      require(ownerOf(tokenId) == msg.sender, "not owner");
      uint256 last = lastActivationBlock[tokenId];
      if (last != 0) require(block.number - last >= cooldownBlocks, "cooldown");
      lastActivationBlock[tokenId] = block.number;
      activeSessionId[tokenId] = ++_sessionCounter;
      registeredDevice[tokenId] = devicePubKey;
      return activeSessionId[tokenId];
  }
  ```
- **Launch verification**:
  1. Read `registeredDevice(tokenId)` from chain (view call, free)
  2. Wrapper signs current block hash with local device private key
  3. Verify signature matches on-chain registered pubkey
  4. No match → session invalid → re-activate
- **Device key storage** (developer configurable):
  | `device_key_storage` | Extractable? | Platform |
  |---|---|---|
  | `file` | Yes, with file access | All |
  | `keychain` | Yes, with OS password | All (via `keyring` crate) |
  | `enclave` | No — hardware-backed, non-extractable | macOS Secure Enclave, Windows TPM |
- **Sharing risk**: Session file is useless without the device private key. Device key cannot produce valid signatures on another machine (different hardware). Even with the `file` storage option, the attacker needs both the session file AND the device key file. With `enclave`, extraction is not possible — the key never enters process memory.
- **No session caching / no TTL**: Every launch requires the device key challenge against the on-chain pubkey. The session is verified live, not cached. There is no `expires_at` to exploit.

### Tier comparison matrix

| | Tier 0 | Tier 1 | Tier 2 | Tier 3 | Tier 4 |
|---|---|---|---|---|---|
| **Name** | `offline` | `cached` | `verified` | `cooldown` | `hardened` |
| **Activation cost** | 0 gas | 0 gas | 0 gas | ~$0.001 | ~$0.001 |
| **Launch cost** | 0 | 0 | 0 (view call) | 0 (view call) | 0 (view call) |
| **Network at launch** | No | No | Yes | Yes | Yes |
| **Offline support** | Full | Within TTL | Grace window | Grace window | None |
| **Copy session file** | Works | Works until TTL | Fails on transfer | Fails (session_id) | Fails (no device key) |
| **Signing oracle** | Works | Works until TTL | Works (real owner) | 1 per cooldown, kills own session | 1 per cooldown + bound to 1 device |
| **VM clone attack** | Works | Works | Works | Works (1 active) | Blocked by enclave; possible with vTPM |
| **Hash components** | `app_id`, `token_id` | + `wallet`, `nonce`, `expires_at` | Same as 1 | + `block_hash`, `session_id` | + `device_pubkey` |

---

## Session Model

The session format varies by tier. Tiers 0 uses the legacy `LicenseProof` format. Tiers 1-4 use the full session format.

### Session schema (tiers 1-4)

```
session = {
  app_id:       "com.example.myapp",
  token_id:     42,
  identity:     "access" | "account",

  -- access model --
  user_id:      "0xabc...wallet",

  -- account model --
  user_id:      "0xTBA...deterministic",
  tba:          "0xTBA...deterministic",
  wallet:       "0xabc...current holder",

  nonce:        "<random 32 bytes hex>",
  issued_at:    "2026-04-10T09:00:00Z",
  expires_at:   "2026-04-17T09:00:00Z",      // tiers 1-3; absent in tier 4
  signature:    "0x<wallet ECDSA sig>",
  chain:        "base",
  contract:     "0x1234...abcd",

  -- tier 3+ --
  activation_tx:         "0x<tx hash>",
  activation_block:      12345678,
  activation_block_hash: "0x<block hash>",
  session_id:            1,

  -- tier 4 --
  device_pubkey:         "0x<compressed secp256k1 pubkey>"
}
```

`user_id` is what the application uses as a stable identity key. In access model it is the wallet address. In account model it is the TBA address.

The signature always comes from the current wallet (NFT holder). The wrapper verifies the signature locally on each launch. On expiry (tiers 1-3) or device challenge failure (tier 4), re-verification is required.

**Multi-device (tiers 1-3)**: Each device holds its own session. Same wallet, different nonces, independent TTLs.

**Single-device (tier 4)**: Only one device can hold a valid session per token. The on-chain `registeredDevice` mapping enforces this. Re-activating on a new device overwrites the old device key.

**Transfer semantics**:
- Access model: new holder activates a fresh session with their wallet as `user_id`. Old sessions expire at TTL (tiers 1-3) or are immediately invalid (tier 4, device key mismatch).
- Account model: new holder activates a fresh session. `user_id` (TBA) is unchanged. The application sees the same account with a new controller wallet.

### Session TTL (tiers 1-3)

```toml
[license]
session_ttl_days = 7
```

| TTL | Use case |
|---|---|
| 1 day | High-value tools, strict ownership enforcement |
| 7 days | Standard (default) |
| 30 days | Matches subscription billing cycle |

Session files stored at `~/.rub3/sessions/<app_id>/<token_id>.json` — one per token, not one per app.

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

## Transaction Confirmation

Tiers 3-4 require the user to send at least one on-chain tx (purchase and/or activate) during the activation flow. The wrapper never holds keys and never broadcasts txs itself — it encodes calldata, surfaces it to the user, and waits for the tx to confirm. How that "wait" happens is an orthogonal concern with three implementations, rendered side-by-side as tabs on the purchase and cooldown screens:

| Mode | Reliance | Tolerant of offline activation | JS bundle |
|---|---|---|---|
| **WalletConnect** | Reown relay + chain RPC | no | ~255 KB vendored |
| **Auto-detect** | Chain RPC (filter `eth_getLogs` / read `lastActivationBlock`) | no | none |
| **Manual** | User copies a tx hash back into the wrapper | yes (paste later) | none |

The modes share one downstream path: whichever tab produces a tx hash hands off to the same receipt poller that validates `status == true`, asserts `receipt.to == contract`, and recovers the minted tokenId (purchase) or the `activeSessionId` (activate). The rest of the session pipeline does not care which tab the hash came from.

**Why all three.**
- **WalletConnect** is the lowest-friction path — the user sees the standard dApp pairing QR in their wallet, approves, and the wrapper receives the tx hash directly. Cost is a vendored JS bundle and a developer-supplied Reown project id per deployment (branding + abuse boundary; not a shared rub3 credential).
- **Auto-detect** is the fall-back when the developer does not want to adopt WalletConnect. The wrapper watches the chain directly for the expected event (ERC-721 `Transfer` mint or a bumped `lastActivationBlock`) and silently continues when the event appears.
- **Manual** is the floor — always available, no dependencies, and the one path that still works if the user's machine is offline when they open the wrapper but they want to send the tx from a hardware wallet elsewhere and paste the hash later.

Which tabs are offered is determined at build + deploy time:
- WalletConnect tab: requires the `wallet-connect` Cargo feature (opt-in, adds the vendored JS bundle) **and** a non-placeholder `wc_project_id` in the packed wrapper.
- Auto-detect tab: requires `onchain-write` (always present in tiers 3-4).
- Manual tab: always on.

The wrapper picks the most capable available tab as the default and lets the user tab over to the others at will. Today both purchase and cooldown screens expose only the Manual path; Auto-detect and WalletConnect are tracked in implementation.md §1.10.

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

#### Activation and session management (tiers 3-4)

Both Rub3Access and Rub3Subscription include the activation/session management interface for tiers 3-4:

```solidity
// ── State ──
mapping(uint256 => uint256) public lastActivationBlock;
mapping(uint256 => uint256) public activeSessionId;
mapping(uint256 => bytes32) public registeredDevice;  // tier 4 only
uint256 public immutable cooldownBlocks;
uint256 public constant MIN_COOLDOWN_BLOCKS = 15; // ~30s on Base; one TOTP window
uint256 private _sessionCounter;

// ── Events ──
event Activated(uint256 indexed tokenId, address indexed owner, uint256 sessionId);

// ── Helpers ──
function cooldownReady(uint256 tokenId)
    external view returns (bool ready, uint256 blocksRemaining)
{
    uint256 last = lastActivationBlock[tokenId];
    if (last == 0) return (true, 0);
    uint256 elapsed = block.number - last;
    if (elapsed >= cooldownBlocks) return (true, 0);
    return (false, cooldownBlocks - elapsed);
}

// ── Tier 3: cooldown activation ──
function activate(uint256 tokenId) external returns (uint256 sessionId) {
    require(ownerOf(tokenId) == msg.sender, "not owner");
    uint256 last = lastActivationBlock[tokenId];
    if (last != 0) require(block.number - last >= cooldownBlocks, "cooldown");
    lastActivationBlock[tokenId] = block.number;
    activeSessionId[tokenId] = ++_sessionCounter;
    emit Activated(tokenId, msg.sender, activeSessionId[tokenId]);
    return activeSessionId[tokenId];
}

// ── Tier 4: hardened activation with device key registration ──
function activateDevice(uint256 tokenId, bytes32 devicePubKey) external returns (uint256 sessionId) {
    require(ownerOf(tokenId) == msg.sender, "not owner");
    uint256 last = lastActivationBlock[tokenId];
    if (last != 0) require(block.number - last >= cooldownBlocks, "cooldown");
    lastActivationBlock[tokenId] = block.number;
    activeSessionId[tokenId] = ++_sessionCounter;
    registeredDevice[tokenId] = devicePubKey;
    emit Activated(tokenId, msg.sender, activeSessionId[tokenId]);
    return activeSessionId[tokenId];
}
```

Key behaviors:
- **Cooldown**: `activate()`/`activateDevice()` reverts if fewer than `cooldownBlocks` have elapsed since the last activation for that token. Limits how often new sessions can be created.
- **Session revocation**: Each activation increments `activeSessionId`. The wrapper reads this value on launch — if the cached session's `session_id` doesn't match, the session has been superseded and is invalid.
- **Device binding (tier 4)**: `registeredDevice` stores the public key of the device that activated. The wrapper signs each launch's block hash with its device private key and verifies against this on-chain value.
- **Single active session**: Creating a new session (for a pirate) immediately invalidates the holder's own session. The holder must choose between keeping access or giving it away.

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
│   ├── Check session expiry (tiers 1-3)
│   ├── Verify ownerOf() on-chain (tiers 2-4)
│   ├── Verify activeSessionId() on-chain (tiers 3-4)
│   ├── Device key challenge — sign block hash, verify vs on-chain pubkey (tier 4)
│   ├── On failure: trigger wallet connection flow
│   └── Write renewed session to disk
│
├── Wallet Connection
│   ├── Open embedded webview (wry) with WalletConnect UI
│   ├── On connect: fetch tokensOfOwner(wallet) via alloy RPC
│   ├── Present token selector UI (skip if single token)
│   ├── On token selected: run ownerOf() / isValid() confirmation
│   ├── Read identityModel from contract
│   ├── Compute TBA address if account model
│   ├── Check cooldown elapsed (tiers 3-4)
│   ├── Send activate()/activateDevice() tx via wallet (tiers 3-4)
│   ├── Generate device keypair, register pubkey on-chain (tier 4)
│   ├── Generate nonce + expires_at
│   ├── Request ECDSA signature over session message
│   └── Store session, close webview
│
├── Device Key Manager (tier 4)
│   ├── Generate ephemeral secp256k1 keypair at activation
│   ├── Store private key: file / OS keychain / Secure Enclave (configurable)
│   ├── Sign block hash challenges at each launch
│   └── Key never leaves storage — signing happens in-place
│
├── Binary Decryption (tiers 3-4, when encrypt_binary = true)
│   ├── Derive KEK from on-chain state (+ device key at tier 4)
│   ├── Unwrap AEK, verify hash against on-chain encryptedBinaryKeyHash
│   ├── Decrypt embedded binary with AES-256-GCM
│   ├── Execute from memory (memfd_create / tmpdir / CreateFileMapping)
│   └── Shred plaintext after child maps it
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
│   ├── main.rs          — CLI entry point, app constants (APP_ID, CONTRACT, CHAIN_ID, RPC_URL, TIER)
│   ├── lib.rs           — public module re-exports
│   ├── license.rs       — tier 0 legacy proof schema + shared crypto helpers
│   ├── session.rs       — session schema, message construction, signature verification (planned)
│   ├── session_store.rs — session persistence ~/.rub3/sessions/ (planned)
│   ├── device.rs        — device keypair generation, storage, challenge-response (planned, tier 4)
│   ├── decrypt.rs       — binary decryption, KEK derivation, in-memory exec (planned, tiers 3-4)
│   ├── store.rs         — tier 0 proof persistence (~/.rub3/licenses/ or $RUB3_LICENSE_DIR)
│   ├── activation.rs    — tier-aware activation flow: check session → verify → launch or open webview
│   ├── rpc.rs           — on-chain queries (ownerOf, price, cooldown, sessionId, registeredDevice)
│   ├── supervisor.rs    — child process lifecycle, SIGTERM forwarding
│   └── webview.rs       — native activation window (wry/tao), JS↔Rust IPC
├── assets/
│   └── activation.html  — activation UI (connect, cooldown, tx-pending, sign, processing screens)
└── tests/
    ├── helpers/mod.rs   — test utilities (wallet gen, signing, license creation)
    ├── integration.rs   — wrapper binary tests (exit codes, args, missing binary)
    └── license_e2e.rs   — static + dynamic license tests, SIGTERM forwarding
```

#### Dependencies

| Crate | Purpose |
|---|---|
| `clap` | CLI argument parsing |
| `alloy` | Ethereum RPC, ABI encoding (ownerOf, price, activate, cooldown) |
| `k256` | secp256k1 ECDSA signature recovery + device keypair generation |
| `sha2` | SHA-256 for activation/session message hash |
| `sha3` | Keccak-256 for Ethereum address derivation + personal_sign |
| `hex` | Hex encoding/decoding |
| `wry` | Embedded webview for activation UI |
| `tao` | Native window/event loop |
| `serde` / `serde_json` | Session/proof serialization |
| `dirs` | Platform data directory resolution |
| `chrono` | RFC-3339 timestamps |
| `nix` / `libc` | Unix signal handling |
| `keyring` | Cross-platform OS keychain access (tier 4, keychain/enclave storage) |
| `rand` | Cryptographic random nonce generation |
| `aes-gcm` | AES-256-GCM binary encryption/decryption (tiers 3-4, when encrypt_binary = true) |

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
  --tier cooldown \
  --session-ttl 7 \
  --cooldown-blocks 1800 \
  --output ./dist/myapp

rub3 deploy --type access --identity account --price 0.05 --chain base \
  --cooldown-blocks 1800       # tiers 3-4: blocks between activations

rub3 deploy --type subscription --identity access --price 0.01 --period 30 --chain base

rub3 register --name myapp --contract 0x1234...abcd
```

`--identity` at deploy time sets the `identityModel` flag in the contract. `--tier` at pack time sets the verification behavior baked into the wrapper binary. `--cooldown-blocks` at deploy time sets the on-chain cooldown period (tiers 3-4).

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

## Binary Protection

### Binary verification (all tiers)

```solidity
bytes32 public wrapperHash;

function setWrapperHash(bytes32 hash) external onlyOwner {
    wrapperHash = hash;
}
```

Trust chain: **ENS → contract → binary hash → running wrapper**

### Binary encryption (tiers 3-4, optional)

Without encryption, the app binary is embedded in the wrapper as plaintext bytes — extractable with `binwalk`, a hex editor, or by reading the wrapper source to find the offset. Binary encryption makes the distributed file useless without a valid on-chain session.

#### How it works

**At pack time (`rub3 pack --encrypt`)**:

1. Generate a random 32-byte **app encryption key** (AEK)
2. Encrypt the app binary with AES-256-GCM using the AEK
3. Derive a **key encryption key** (KEK) from on-chain values: `KEK = SHA-256(contract_address || chain_id || salt)`
4. Encrypt the AEK with the KEK, producing **wrapped-AEK**
5. Embed in the wrapper: `[encrypted binary] [wrapped-AEK] [salt] [nonce] [auth-tag]`
6. Store `SHA-256(AEK)` on-chain as `encryptedBinaryKeyHash` for verification

```toml
[license]
tier = "cooldown"
encrypt_binary = true    # default false; requires tier ≥ 3
```

**At launch (after session verified)**:

```
Wrapper starts
    │
    Verify session (tier 3: cooldown check, tier 4: device challenge)
    │
    Read contract_address + chain_id from embedded config
    │
    Reconstruct KEK = SHA-256(contract_address || chain_id || salt)
    │
    Unwrap AEK using KEK
    │
    Verify SHA-256(AEK) matches on-chain encryptedBinaryKeyHash
    │
    Decrypt app binary into memory (never written to disk)
    │
    Execute from memory:
      Linux:   memfd_create() → write → fexecve()
      macOS:   write to tmpdir with restrictive permissions → exec → unlink
      Windows: CreateFileMapping(INVALID_HANDLE_VALUE) → MapViewOfFile → execute
    │
    Shred plaintext from memory after child process maps it
```

#### Tier 4 enhancement: device-key-derived decryption

At tier 4, the KEK derivation includes the device key, making decryption impossible without the registered device:

```
KEK = SHA-256(contract_address || chain_id || salt || device_privkey_fingerprint)
```

The `device_privkey_fingerprint` is derived from the device private key (a hash of the public key). Since the device key lives in Secure Enclave / TPM / keychain, the KEK can only be reconstructed on the device that activated.

At pack time, the AEK is wrapped with a **generic KEK** (without device fingerprint). At first activation, the wrapper re-wraps the AEK with the device-specific KEK and overwrites the wrapped-AEK on disk. Subsequent launches use the device-specific KEK.

#### Contract interface

```solidity
bytes32 public encryptedBinaryKeyHash;  // SHA-256(AEK), set at deploy

function setEncryptedBinaryKeyHash(bytes32 hash) external onlyOwner {
    encryptedBinaryKeyHash = hash;
}
```

The AEK itself is never stored on-chain — only its hash, used to verify the decrypted key is correct before attempting to decrypt the binary (prevents silent corruption).

#### What this prevents

| Attack | Without encryption | With encryption |
|---|---|---|
| Extract binary from distributed file | `binwalk` / hex editor | Encrypted blob, useless without KEK |
| Extract binary from memory at runtime | Memory dump | Still works — fundamental limit of running code on untrusted hardware |
| Distribute cracked binary | Extract once, share everywhere | Must have valid session to decrypt; each extraction requires on-chain interaction |
| Reverse-engineer the wrapper to find the key | Key is in the binary | KEK is derived from on-chain state + device key; not stored in the wrapper |

#### Execution from memory

The decrypted binary is never written to permanent storage. Platform-specific approaches:

| Platform | Method | Notes |
|---|---|---|
| Linux | `memfd_create()` + `fexecve()` | Anonymous in-memory file descriptor; invisible to filesystem |
| macOS | Write to `$TMPDIR` with `0700` permissions, exec, unlink before child starts | macOS doesn't support `fexecve`; the temp file exists briefly |
| Windows | `CreateFileMapping(INVALID_HANDLE_VALUE)` + section mapping | In-memory execution via PE loader |

After the child process has mapped the binary, the wrapper zeroes and deallocates its copy of the plaintext. The child's own memory mapping remains (necessary for execution) but is protected by normal OS process isolation.

#### CLI

```
rub3 pack \
  --binary ./target/release/myapp \
  --app-id com.example.myapp \
  --contract 0x1234...abcd \
  --chain base \
  --tier cooldown \
  --encrypt \
  --output ./dist/myapp
```

---

## Launch Flow

### Tier 0-2: signature/ownership verification

```
Wrapper starts
    │
    Read cached session/proof
    │
    ┌───────────────┴───────────────┐
Session valid?                  No session /
(tier 0: sig only)              Expired / Invalid
(tier 1: sig + TTL)                 │
(tier 2: sig + TTL + ownerOf)       │
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
                  (WalletConnect /      (auto-select if 1 token)
                   auto-detect /              │
                   manual paste)              │
                         │                    │
                  User purchases              │
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
                                    [encrypt_binary?] → Decrypt binary into memory
                                            │
                                       Launch app
```

### Tier 3: cooldown activation

```
    ... (same as above through token selection) ...
                                            │
                                    ownerOf() / isValid()
                                            │
                                    Check cooldown elapsed?
                                    (lastActivationBlock + cooldownBlocks ≤ block.number)
                                            │
                                 ┌──────────┴──────────┐
                            Cooldown active          Cooldown ready
                                 │                       │
                          Show "wait N blocks"     User sends activate() tx
                          + retry button           (WalletConnect / auto-detect /
                                                    manual paste — see
                                                    Transaction Confirmation)
                                                         │
                                                   Wrapper confirms the receipt
                                                   (tab-specific watcher;
                                                    same downstream poller)
                                                         │
                                                   Extract block_hash, session_id
                                                         │
                                                   Wallet signs session message
                                                   (includes block_hash + session_id)
                                                         │
                                                   Cache session
                                                         │
                                                   [encrypt_binary?]
                                                   Derive KEK from on-chain state
                                                   Unwrap AEK, verify hash
                                                   Decrypt binary into memory
                                                         │
                                                    Launch app
```

### Tier 4: hardened (device-bound)

```
    ... (same as tier 3 through cooldown check) ...
                                            │
                                    Cooldown ready
                                            │
                                    Generate device keypair
                                    (Secure Enclave / keychain / file)
                                            │
                                    Wallet sends activateDevice(tokenId, devicePubKey) tx
                                            │
                                    Wait for tx confirmation
                                            │
                                    Wallet signs session message
                                    (includes block_hash + session_id + device_pubkey)
                                            │
                                    Cache session → Launch app

On every subsequent launch:
    Read cached session
        │
    Read registeredDevice(tokenId) from chain
        │
    Sign current block hash with local device key
        │
    Verify signature matches on-chain pubkey
        │
    ┌────┴────┐
  Match     No match
    │           │
    │       Re-activate
    │
    [encrypt_binary?]
    Derive KEK from on-chain state + device key fingerprint
    Unwrap AEK, verify hash
    Decrypt binary into memory
        │
  Launch app
```

### Runtime (all tiers)

```
While running:
  Wrapper ──heartbeat IPC──▶ App (every 5s)
  App panics/exits if heartbeat stops
  Wrapper exits if app exits
```

---

## Session Format

### Tier 0 (legacy license proof)

```json
{
  "app_id":         "com.example.myapp",
  "token_id":       42,
  "wallet_address": "0xabc...123",
  "signature":      "0x...",
  "activated_at":   "2026-04-10T09:00:00Z",
  "chain":          "base",
  "contract":       "0x1234...abcd"
}
```

Stored at `~/.rub3/licenses/<app_id>.json`.

### Tiers 1-3 (session with TTL)

```json
{
  "app_id":                 "com.example.myapp",
  "token_id":               42,
  "identity":               "account",
  "user_id":                "0xTBA...deterministic",
  "tba":                    "0xTBA...deterministic",
  "wallet":                 "0xabc...123",
  "nonce":                  "a3f8...c921",
  "issued_at":              "2026-04-10T09:00:00Z",
  "expires_at":             "2026-04-17T09:00:00Z",
  "signature":              "0x...",
  "chain":                  "base",
  "contract":               "0x1234...abcd",
  "activation_tx":          "0x...",
  "activation_block":       12345678,
  "activation_block_hash":  "0x...",
  "session_id":             1
}
```

Tiers 1-2 omit `activation_tx`, `activation_block`, `activation_block_hash`, `session_id`. Access model omits `tba` and sets `user_id` to the wallet address.

Signature covers: `SHA-256(app_id || token_id || wallet || nonce || expires_at [|| activation_block_hash || session_id])`.

### Tier 4 (hardened, device-bound)

```json
{
  "app_id":                 "com.example.myapp",
  "token_id":               42,
  "identity":               "account",
  "user_id":                "0xTBA...deterministic",
  "tba":                    "0xTBA...deterministic",
  "wallet":                 "0xabc...123",
  "nonce":                  "a3f8...c921",
  "issued_at":              "2026-04-10T09:00:00Z",
  "signature":              "0x...",
  "chain":                  "base",
  "contract":               "0x1234...abcd",
  "activation_tx":          "0x...",
  "activation_block":       12345678,
  "activation_block_hash":  "0x...",
  "session_id":             1,
  "device_pubkey":          "0x<33-byte compressed secp256k1>"
}
```

No `expires_at` — tier 4 sessions do not expire by time. They are valid as long as the device key matches the on-chain `registeredDevice` and the `session_id` matches `activeSessionId`.

Signature covers: `SHA-256(app_id || token_id || wallet || nonce || activation_block_hash || session_id || device_pubkey)`.

Session files stored at `~/.rub3/sessions/<app_id>/<token_id>.json` — one per token, not one per app.

---

## Security Model

### Wallet as trust boundary

The wrapper never holds a wallet private key. Signing happens in the wallet via WalletConnect. Session signatures are free — no on-chain effect. The wrapper does hold a device private key (tier 4), but this is an ephemeral key used only for device binding — it cannot sign transactions or move funds.

### Threat model by tier

| Attack | Tier 0 | Tier 1 | Tier 2 | Tier 3 | Tier 4 |
|---|---|---|---|---|---|
| Copy session file to another machine | Works | Works until TTL | Fails on transfer | Fails (session_id mismatch after re-activation) | Fails (no device key) |
| Replay expired session | N/A (no expiry) | Blocked by `expires_at` | Blocked | Blocked | N/A (no TTL, device challenge instead) |
| Signing oracle (holder signs for pirates) | Works | Works until TTL | Works (real owner) | 1 per cooldown, kills own session | 1 per cooldown + bound to 1 device |
| NFT transferred, old session still valid | Works forever | Expires at TTL | Fails (`ownerOf` mismatch) | Fails (session_id reset on new activation) | Fails (device key + session_id) |
| Subscription lapsed | N/A | Valid until TTL | `isValid()` fails | `isValid()` fails | `isValid()` fails |
| Forged session signature | Requires wallet key | Requires wallet key | Requires wallet key | Requires wallet key | Requires wallet key + device key |
| VM clone with vTPM | Works | Works | Works | Works (1 active) | Blocked by Secure Enclave; possible with vTPM |
| Compromised wrapper binary | ENS + binary hash | ENS + binary hash | ENS + binary hash | ENS + binary hash | ENS + binary hash |

### Account model: what transfer means to security

In account model, the TBA address (`user_id`) is stable across transfers. An attacker who obtains a cached session file gets a `user_id` that is currently controlled by someone else's wallet. The session signature verifies against the wallet that signed it — after transfer, the old wallet no longer controls the NFT, but the session remains valid until invalidated.

Invalidation timing depends on tier:
- Tiers 1: old session valid until TTL expires (time-limited lame duck)
- Tier 2: invalid on next launch (`ownerOf` check fails)
- Tier 3: invalid immediately if new holder activates (session_id changes)
- Tier 4: invalid immediately (device key + session_id)

This is intentional and matches the semantics: **transfer sells the account to the new holder, who takes full control at the next activation.** Higher tiers make the handover faster.

### Defense layers summary

```
Distribution:  on-chain binary hash (verify download)
Encryption:    AES-256-GCM binary encryption (tiers 3-4: binary useless without valid session)
Identity:      ENS resolution (verify developer identity)
Payment:       wallet transaction approval
Session:       SIWE-style signature (proves ownership at creation)
Cooldown:      on-chain rate limit (tiers 3-4: prevents mass session distribution)
Revocation:    on-chain session counter (tiers 3-4: new session kills old)
Device:        ephemeral keypair bound to hardware (tier 4: non-transferable)
Enforcement:   session TTL (tiers 1-3) or device challenge (tier 4)
Runtime:       heartbeat IPC (app cannot run without wrapper)
```

---

## Scaling Considerations

- Contract deployment: one per app, ~$1–5 on Base
- RPC read calls: varies by tier. Tier 0: zero. Tiers 1-2: one per renewal. Tiers 3-4: one per launch (`activeSessionId` + `ownerOf`). Public RPC or Alchemy free tier sufficient.
- RPC write calls: tiers 3-4 only. One `activate()`/`activateDevice()` tx per session creation. ~$0.001 on Base.
- Session files: ~500 bytes each, one per token per device. Negligible storage.
- Device keys (tier 4): one per token per device. Stored in OS keychain or Secure Enclave — no additional disk storage.
- No backend. No database. No auth service. All verification is either local crypto or on-chain reads.
