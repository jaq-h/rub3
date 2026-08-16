# rub3 - Architecture

This file owns design rationale: why the system is shaped the way it is. Security tiers, the session model, identity models, launch flows, and the ownership invariants are argued here, and this is what to read before changing behaviour. It does not own status, which lives in [implementation.md](implementation.md); contract operations and fee mechanics, which live in [contracts/contracts.md](contracts/contracts.md); the test inventory, which lives in [testing.md](testing.md); positioning, which lives in [ideation.md](ideation.md); or build and run instructions, which live in [README.md](README.md).

## North Star

rub3 exists to let machines buy, verify, and resell software without asking anyone's permission. The unit of adoption is one closed loop an agent can complete end to end:

```
discover → pay → fetch → verify → run → (resell)
```

Three commitments shape every design decision below:

**Two front doors, one rail.** All session crypto (signing, calldata encoding, receipt polling) is native Rust. Headless activation - signer in, session out - is the primary path and needs no webview; the interactive webview flow is the human fallback floor. Everything below the front door (RPC, session model, persistence, supervision) is shared.

**The token is the invariant; everything else is versioned.** License contracts are immutable - no proxies, no upgrade hooks, no revocation surface. Evolution only ever changes what is *offered* going forward (price, successor contracts, registry listings), never what was *granted* (held tokens, their validation logic, their renewal terms).

| | Can never change | Can change (affects future only) |
|---|---|---|
| **Developer** | validity of issued tokens; transfer rights; per-token renewal terms (`renewPrice`, `renewPriceToken`, `renewPriceAmount`, `period`); supply cap; identity model; TBA implementation; cooldown; predecessor link; protocol fee terms (`feeBps`, `treasury`) | price for new sales on either rail (`price`, `priceToken` / `priceAmount`); wrapper hash set (append + flag only); successor pointer; registry listing |
| **rub3** | fee on any deployed contract; validation logic | factory versions; registry curation; marketplace; facilitator |

Every "can never change" cell in the developer row is enforced by bytecode today and checkable before purchase - and since §2.3 that includes the fee, so "rub3 cannot raise its take on a contract you already deployed" is now a property of the bytecode rather than a promise. See [Ownership invariants](#ownership-invariants-all-license-contracts) for the audit procedure and for the shorter list of properties that are still convention rather than proof.

**Open rails, owned network.** The wrapper, SDK, CLI, and contract templates are open source and free. Revenue lives where network effects live: an immutable 2–3% protocol fee stamped into factory-deployed contracts, metered per-launch billing only the wrapper can enforce, and (once volume exists) marketplace fees on secondary license trades. x402 can meter API calls because the server is a choke point; the wrapper is that choke point for locally executed software.

---

## Chain

**Base (Ethereum L2)** is the primary target chain.

| Why Base | Detail |
|---|---|
| User onboarding | Coinbase on-ramp - users buy ETH without bridging |
| ENS support | Resolves L1 ENS natively, critical for trust layer |
| Cost | $0.01–0.05 per mint/renewal transaction |
| Finality | ~2 sec soft confirmation |
| Rust crates | `alloy` is lean (~30 deps), handles RPC, ABI, ENS resolution |
| Wallet support | Native in Coinbase Wallet, MetaMask, Rainbow, and WalletConnect-compatible wallets |

Chain is abstracted behind config - switching to Arbitrum or another EVM L2 is a config change:

```toml
[chain]
name = "base"
rpc  = "https://mainnet.base.org"
chain_id = 8453
```

### Money

Two rails, and they mint identically (implementation.md §2.2, built).

**ETH** is the `payable` path a human wallet uses: `purchase{value: price}(recipient)`.

**USDC via EIP-3009** is the default for the machine economy: the buyer signs a payment authorization off-chain, and anyone - the developer, a facilitator, or the buyer itself - submits it on-chain. Gasless for the buyer; an agent holding only stablecoins can obtain a licence without ever owning ETH. A contract advertises the rail by returning a non-zero `priceToken()` alongside `priceAmount()`; that single `eth_call` is how the wrapper decides which currency to pay in, and a contract deployed before §2.2 has no such getter, so the call reverts and the wrapper reads it as "ETH only". The two rails are independently quoted with no on-chain relation, so on the wrapper side the stablecoin rail is additionally gated on an operator-set ceiling, `RUB3_AGENT_MAX_TOKEN_AMOUNT`, in the payment token's own smallest unit. It has no default - token decimals differ, so no single number means the same thing twice - and until it is set the rail is unavailable and the wrapper buys in ETH. It is weighed after advertised, affordable and signable, and deliberately **before anything is signed**: `purchaseWithAuthorization` is submittable by anyone, so an authorization the ceiling refuses must never be created at all, let alone handed to an RPC endpoint that could broadcast it. A listed amount above a set ceiling is refused outright (exit 22) rather than routed to the other rail, so an orchestrator can tell a policy breach from a network failure, while an agent that cannot use the rail at all - not advertised, cannot afford it, no readable domain, no ceiling configured - is left on ETH rather than refused over money it could not have spent. Because the refusal comes before the pre-flight, exit 22 says only that the price was refused and is not evidence the rail is otherwise healthy; the message says so.

**The ETH rail carries the same ceiling**, `RUB3_AGENT_MAX_ETH_WEI`, in wei, weighed after `price()` is read and before the transaction is sent, so a refusal costs no gas and arrives as a policy answer rather than an on-chain error. It is the same mechanism - one more field on `SpendPolicy`, one more sibling check, the same `PriceAbovePolicy` and the same exit 22 - and it differs from the stablecoin ceiling in exactly one way: **it has a default, 0.1 ETH**. The stablecoin ceiling cannot have one because it is denominated in whichever token a contract lists and decimals differ between them, so no single number means the same thing twice. That argument is about a unit this wrapper cannot know, and it does not transfer to ETH: wei is fixed on every contract on every chain, so a default here is exactly as well defined as an operator's own value. The consequence is the one that matters: **neither rail is ever unbounded**, and an operator who configures nothing gets a bounded ETH rail rather than an unlimited one. The number is not a price opinion - ETH's value moves and no compiled constant tracks it - it is the blast radius of one unattended purchase, above ordinary licence prices and below what a funded agent holds. Since §2.4 the ETH rail also requires exact payment, so a price that moves between the read and the send reverts on-chain rather than overpaying; the ceiling is a different guarantee, bounding what the agent will agree to pay at all.

**The token rail requires a Circle FiatTokenV2_2-style payment token.** The licence contracts call the `bytes signature` overload of `receiveWithAuthorization` - the one that takes an opaque signature rather than split `(v, r, s)`. A token implementing only the `(v, r, s)` form specified by EIP-3009 is conforming and still **not supported**: it passes the deploy-time probe (which reads `authorizationState`, common to both) and then reverts for every buyer. This narrowing is deliberate and was chosen to admit smart-contract wallets. The `bytes` form validates through a signature checker - ECDSA recovery for a 65-byte EOA signature, falling through to EIP-1271 `isValidSignature` for a contract signer - so an ERC-4337 smart account, which is how a growing share of agent wallets hold funds, buys on the same single entry point an EOA uses. The split form can only ever serve an EOA. Code is frozen at deploy, so the choice is permanent; narrower token support was judged the better price than permanently excluding the buyers the rail exists for. Nothing on-chain can verify the overload (a staticcall probe cannot tell "no such function" from "bad signature"), so the wrapper pre-flights the `purchaseWithAuthorization` call as an `eth_call` before broadcasting - the same call, differing only in the authorization's validity window - and falls back to ETH with a printed reason if it reverts, no gas spent and no activation lost.

**`receiveWithAuthorization`, not `transferWithAuthorization`.** EIP-3009 defines both over the same six signed fields; only the receive variant requires `msg.sender == to`. With the transfer variant, anyone watching the mempool could push a buyer's authorization straight at the token, moving the money to the licence contract *without* the mint and burning the nonce - the buyer paid, holds nothing, and there is no recovery. Requiring the receive variant makes the licence contract the only address that can spend the authorization at all, so payment and mint are inseparable. Anyone may still submit the *purchase*, which is what keeps it gasless.

**What binds an authorization.** The token signs `from`, `to`, `value`, `validAfter`, `validBefore`, and `nonce` - and nothing else, so the mint recipient is not covered by default. rub3 binds it through the nonce: `purchaseAuthorizationNonce(recipient, salt)` (and `renewAuthorizationNonce(tokenId, salt)` for renewals) is derived by the contract, not accepted from the caller, so a submitter who changes the recipient derives a different nonce and produces a digest the buyer never signed. Distinct domain tags keep a purchase authorization from being spent as a renewal, or the reverse. Replay is the token's own single-use nonce, backed by a balance-delta check in the licence contract so a mint cannot happen unless the money actually arrived.

This is the prerequisite for x402-style catalog listings (implementation.md §3.3).

---

## Identity Models

The contract issuer chooses one of two identity models when deploying. This is the most fundamental design decision in rub3 - it determines what the NFT means to the application.

### Access Model (`identity = "access"`)

**`wallet_address` is the user identity.**

The NFT is a gate. Owning it proves the right to use the application. The user's wallet is their account. If the NFT is transferred, the new holder gets access but the old holder's session eventually expires - no account history moves.

Use when:
- The app has no persistent user data, or stores it server-side keyed on wallet address
- Transfer is expected to be uncommon (resale market, gifting)
- The developer wants the simplest possible model

Session identity field: `wallet_address`

### Account Model (`identity = "account"`)

**`token_id` is the user identity**, specifically via its ERC-6551 Token Bound Account (TBA).

The NFT is an account. Its TBA address is deterministic and permanent - it never changes regardless of who holds the NFT. The current holder controls the TBA (and therefore the account), but the account's identity is the TBA address, not the wallet address.

Use when:
- The app stores user data, preferences, or history
- Wallet rotation should not reset the user's account
- Transfer should sell the account to the buyer - they inherit the history
- The developer wants native web3 account composability

Session identity field: `tba_address` (deterministic TBA derived from token)

### TBA Address Derivation (ERC-6551)

The TBA address for any token is deterministic and computed locally - no on-chain call needed:

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

The wrapper computes this address from the token ID at session creation. The TBA may or may not be deployed - rub3 does not require it to be deployed, it only uses the address as a stable identity key.

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

The developer chooses a security tier when packaging their app. Each tier is a coherent bundle of verification behaviors - higher tiers add on-chain enforcement and device binding to prevent license sharing.

**Agent-consumer note.** The ladder above tier 2 is calibrated to *human* piracy economics (signing oracles, license sharing). Agent customers change the calculus: agents don't pirate - paying cents in USDC is cheaper than engineering theft, and their operators impose spend policy and compliance. Meanwhile agent fleets legitimately clone VMs and scale horizontally, which device binding treats as an attack. In practice: tiers 0–2 cover most agent-consumed software; tier 3's session counter generalizes to **concurrent seats** (`maxConcurrentSessions[tokenId] = K` - an on-chain semaphore licensing K fleet instances per token; implementation.md §3.4); and tier 4 plus binary encryption are **deferred** (implementation.md §Deferred).

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
- **Verification**: ECDSA recovery - recovered address must match `wallet_address` in proof
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

Adds on-chain activation with a cooldown and session revocation counter. At activation, the wallet sends an `activate()` transaction that records the current block and increments a `sessionId` on-chain. Only one session per token is valid at a time - creating a new one invalidates the old one.

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

### Tier 4: `hardened` *(deferred)*

Would bind a session to the machine that created it with an on-chain registered device key, so a copied session file cannot be replayed elsewhere. Not built; see "Deferred designs" below.

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

Session files stored at `~/.rub3/sessions/<app_id>/<token_id>.json` - one per token, not one per app.

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

Session files are keyed on both app_id and token_id: `~/.rub3/sessions/<app_id>/<token_id>.json`. This allows each token to maintain its own cached session - switching between tokens at launch resumes the correct cached session without re-authenticating.

---

## Transaction Confirmation

Tiers 3-4 require at least one on-chain tx (purchase and/or activate) during the activation flow. In **interactive mode** the wrapper never holds keys and never broadcasts txs itself - it encodes calldata, surfaces it to the user, and waits for the tx to confirm. In **headless mode** (built, implementation.md §2.1) the operator supplies a signer explicitly - env key, keystore, or KMS-backed `Signer` impl - and the wrapper signs and broadcasts directly; there is no confirmation UI because there is no user round-trip.

For interactive builds, how the "wait" happens is an orthogonal concern. **Today there is exactly one implementation: Manual.** Both the purchase and the cooldown screens ask the user to copy a tx hash back into the wrapper, and nothing else is offered.

| Mode | Status | Reliance | Tolerant of offline activation | JS bundle |
|---|---|---|---|---|
| **Manual** | **the only interactive path today** | User copies a tx hash back into the wrapper | yes (paste later) | none |
| **Auto-detect** | planned, implementation.md §5.1a | Chain RPC (filter `eth_getLogs` / read `lastActivationBlock`) | no | none |
| **WalletConnect** | planned, implementation.md §5.1b | Reown relay + chain RPC | no | ~255 KB vendored |
| **Headless** | built, implementation.md §2.1 | operator-supplied signer + chain RPC | n/a - no user round-trip | none |

Manual is the floor and stays available whatever else lands: no dependencies, and the one path that still works when the user's machine is offline as they open the wrapper but they want to send the tx from a hardware wallet elsewhere and paste the hash later.

The design commitment behind the two planned modes is that they are **additive tabs on the same screens, not replacements**. Whichever tab produces a tx hash hands off to the same receipt poller, which validates `status == true`, asserts `receipt.to == contract`, and recovers the minted tokenId (purchase) or the `activeSessionId` (activate). The rest of the session pipeline does not care which tab the hash came from, so adding a mode cannot change what a confirmed activation means. Availability would be decided at build and deploy time: Auto-detect requires `onchain-write` (always present in tiers 3-4); WalletConnect requires the `wallet-connect` Cargo feature and a non-placeholder `wc_project_id` in the packed wrapper, since the Reown project id is developer-supplied per deployment rather than a shared rub3 credential. Both are demoted to Phase 5 (implementation.md §5.1).

---

## Components

### 1. Smart Contracts

#### Rub3Access (one-time purchase)

ERC-721 + ERC-721Enumerable with payable `purchase(address recipient)` and `purchaseWithAuthorization(address recipient, PaymentAuthorization auth)`, where `PaymentAuthorization` is `(address from, uint256 validAfter, uint256 validBefore, bytes32 salt, bytes signature)`:
- Price per token on both rails: `price` (wei) and `priceToken` / `priceAmount` (an EIP-3009 ERC-20 and its own smallest unit). Independent quotes, not a conversion - the contract holds no oracle. Optional supply cap (immutable)
- `recipient == address(0)` defaults to `msg.sender` on the ETH path and to `auth.from`, the buyer, on the authorization path - never to the submitter
- Both paths reach one `_mintPurchased`, so a licence bought with USDC is identical in state and events to one bought with ETH
- `mapping(bytes32 => HashStatus) wrapperHashes` - append-only set of distributed-binary SHA-256s (see [Binary verification](#binary-verification-all-tiers))
- `uint8 identityModel` - `0 = access`, `1 = account` - readable by wrapper

On-chain check: `ownerOf(tokenId) == walletAddress`

#### Rub3Subscription (recurring)

ERC-721 + ERC-721Enumerable extended with time-based validity:
- `mapping(uint256 => uint256) public expiresAt`
- `mapping(uint256 => uint256) public renewPrice` - the token's ETH renewal price, snapshotted from `price` at mint and written once
- `mapping(uint256 => address) public renewPriceToken` + `mapping(uint256 => uint256) public renewPriceAmount` - the same freeze for the stablecoin rail. **A second snapshot, not a conversion of the first**: both listed prices are frozen at the same instant, and a token minted while the contract offered no stablecoin rail carries none and renews in ETH, which every token always can
- `purchase()` / `purchaseWithAuthorization()` set `expiresAt[tokenId] = block.timestamp + period` and snapshot all three renewal terms
- `renew(uint256 tokenId)` payable, and `renewWithAuthorization(uint256 tokenId, PaymentAuthorization auth)`, each extending by one period at that token's own snapshot - never the current listed price
- `uint256 immutable period` - the other half of "renewal terms are frozen per token"
- `uint8 identityModel` - same flag as above

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
- **Session revocation**: Each activation increments `activeSessionId`. The wrapper reads this value on launch - if the cached session's `session_id` doesn't match, the session has been superseded and is invalid.
- **Device binding (tier 4)**: `registeredDevice` stores the public key of the device that activated. The wrapper signs each launch's block hash with its device private key and verifies against this on-chain value.
- **Single active session**: Creating a new session (for a pirate) immediately invalidates the holder's own session. The holder must choose between keeping access or giving it away.

#### Rub3Factory *(implementation.md §2.3)*

All canonical deployments go through a factory that stamps the protocol's economics and invariants:

```solidity
contract Rub3Factory {
    uint16  public constant  MIN_FEE_BPS = 200;  // the range any rub3 factory may charge
    uint16  public constant  MAX_FEE_BPS = 300;
    uint16  public immutable feeBps;    // chosen per factory deploy, frozen at construction
    address public immutable treasury;  // rub3 fee recipient
    address public immutable previousFactory;      // the factory this one supersedes; 0x0 on the first
    mapping(address => bool) public isDeployed;    // registry + marketplace trust only these

    // 0x0, this factory's own row, or a row on a factory reachable through
    // previousFactory within MAX_PREDECESSOR_FACTORY_HOPS (8) hops.
    function isCanonicalPredecessor(address) external view returns (bool);

    // Both revert PredecessorNotCanonical(address) unless params.predecessor is.
    function deployAccess(Rub3LicenseParams calldata) external returns (address);
    function deploySubscription(Rub3LicenseParams calldata, uint256 period) external returns (address);
}
```

**A factory deploy may only succeed a canonical predecessor.** `claimFromPredecessor` charges nothing, because migration must never be taxed, so an unconstrained `predecessor` would let a whole fee-free sale be laundered onto a registry-listed contract: sell on a direct deploy, then deploy the successor through the factory naming it as predecessor, and every holder claims onto a fee-bearing `isDeployed` contract with the treasury never paid. The factory therefore accepts `address(0)`, its own deployments, or those of a factory reachable through the immutable `previousFactory` chain - which is what keeps an older factory's contracts migratable when rub3 changes its take by deploying a new factory. Direct deploys and the permissionless deployer helpers are untouched: they grant no `isDeployed` row, so there is nothing there to launder onto. The cost is that a pre-factory contract cannot migrate its holders onto a canonical contract *through the factory*; it migrates onto a directly deployed successor instead, keeping every ownership guarantee and forgoing only the row. See `contracts/contracts.md` → "A factory deploy may only succeed a canonical predecessor".

The fee split executes on-chain inside `purchase()` / `renew()`, on **both** payment rails: `feeBps` of what arrived to `treasury`, the remainder to the developer's `withdraw()` balance. **Immutable per contract** - `feeBps` and `treasury` are `immutable` on the factory *and* on every contract it deploys, so a developer's economics can never change after deploy; rub3 changes its take only by deploying a new factory, which affects contracts deployed by that factory and nothing that already exists. Direct (non-factory) deployment of the open-source contracts is always possible: fee-free and unrecorded by design, not a gap.

The factory path stamps the fee and grants an `isDeployed` row. That row is a durable canonical record today, and the eligibility criterion for the registry (implementation.md §3.2) and marketplace (§4.3) once they ship. Neither is built yet, and the fee does not go live ahead of them: the contracts are not deployed to mainnet or declared ready for use until the registry is ready, so the factory and the registry launch together.

The getters, the deploy recipe, the split and sweep calls, where the fee's scope ends, and why a matching fingerprint is never evidence of canonical deployment all live in `contracts/contracts.md` → "The protocol fee". Only the design arguments below belong here.

#### Why the fee split is shaped this way

Three properties of the split are load-bearing rather than incidental, and each closes a specific failure:

- **Charged on the amount received, not the listed price.** Charging what arrived makes the arithmetic exact by construction: the two shares are the payment, with nothing left over. It is also the only quantity that is always the money actually in hand - on the stablecoin rail `received` is a measured balance delta, and a payment token can credit more than it was asked for, so charging a listed price there would leave a surplus untaxed and unaccounted for and the two shares would no longer sum to the payment. And it is one rail-independent rule that holds however the money arrived, rather than one that depends on trusting a price variable at accrual time. The zero-price evasion route this bullet used to rest on - list at zero, take the real price as "overpayment" - is now closed one step earlier, in `_payEth`, which reverts unless `msg.value` equals the listed price, so the fee rule no longer carries that burden.
- **Rounding favours the developer.** Integer division, so a sub-unit fee is zero rather than one. A fee that rounded up could exceed the payment at the smallest amounts.
- **Accrued in the contract, not pushed to the treasury on the money path.** `treasury` is immutable, so a transfer inside `purchase()` would let a recipient that reverts on receipt break every purchase on that contract forever, unfixably. Accruing keeps the buyer's path free of calls out, and a collection failure becomes rub3's problem rather than the buyer's. The mirror image of that immutability - a treasury that is lost or one day cannot receive strands every fee on every contract its factory deployed, permanently - is an operational requirement rather than a contract one, and is stated in `contracts/contracts.md` → "Treasury custody, and the pre-mainnet proof".

**Where the fee's scope ends is an economic argument, not a technical lock.** The fee is charged on value that arrives *through* the contract's payment functions; anything reaching the contract another way is released whole to the developer. That is deliberate. A developer who wants to route around the fee can already sell off-chain and list at zero, and the shipped contracts have no owner-mint and no airdrop, so a zero-price listing makes the licence free for everyone rather than for that developer's customers alone. Permissionlessness, not an on-contract lock, is what defends the zero-price route.

**The fee's denomination is not constrained on-chain, and that is the accepted position.** The fee accrues in whatever asset the payment arrived in, so a developer who lists `priceToken` as an asset of their own choosing decides what rub3's share is *denominated in* as well as what it is a percentage of. Two consequences follow, and both are real: at 200-300 bps a payment below 34-50 of the token's smallest units rounds the fee to zero, and even a large amount priced in a token nobody trades is a percentage of nothing. Closing either on-chain means the contracts holding an economic policy about which tokens count - an allowlist, or a minimum amount per rate - which is exactly the oracle-shaped judgement the design refuses to carry, in a contract that can never be changed once deployed and that would then have to be redeployed every time a token's standing changed. **The denomination question is answered at the discovery layer instead.** The registry (implementation.md §3.2) maintains a recognised-token list and ranks and lists canonical contracts by the token they are priced in, so a contract quoting a token rail in an asset nobody recognises ranks below one that does, while the native rail counts as recognised - an ETH-only listing quotes no token and its fee accrues in ETH, the one asset this argument was never about. That keeps the same economic argument the rest of the fee rests on - routing around it costs the carrot - rather than converting it into an on-chain lock the invariants forbid. It is a requirement on the registry, which is `[not started]`; nothing enforces it today.

**Why the factory needs two helper contracts.** The two licence contracts' creation code together is over 30 KB against a 24,576-byte runtime limit, so the factory builds one `Rub3AccessDeployer` and one `Rub3SubscriptionDeployer` in its own constructor and holds their addresses as immutables. The consequence for an auditor: the factory's own bytecode fingerprint does not pin which licence implementations it deploys, so verifying a factory means fetching the code at `accessDeployer()` / `subscriptionDeployer()` and comparing those against the canonical manifest too.

#### Rub3Metered *(planned - implementation.md §4.1)*

A third billing model unique to runtime enforcement: the launch gate requires a micropayment (per launch, per session-hour, or per N launches) settled in USDC. Same protocol fee, much higher-frequency flow than one-time sales.

#### Ownership invariants (all license contracts)

Live in `Rub3License` (implementation.md §2.4). Enforced by construction, machine-verifiable by any buyer before purchase:

- **No revocation surface.** No burn, no admin transfer, no pause on `ownerOf` / `isValid` / `activate` for issued tokens. Not policy - absent from the bytecode.
- **No proxies.** Contract code, and therefore license terms, are frozen at deploy. No upgrade hook, no delegatecall, no initializer.
- **Renewal terms frozen per token, on both rails.** `renewPrice[tokenId]`, `renewPriceToken[tokenId]`, and `renewPriceAmount[tokenId]` all snapshot at mint and are written once; `period` is immutable. `renew()` and `renewWithAuthorization()` charge the snapshot. A developer cannot reprice a held subscription in either currency - and there is no function that could.
- **Append-only wrapper hash set.** Replaces the single rotatable `wrapperHash` slot - see [Binary verification](#binary-verification-all-tiers).
- **Successor pattern for migrations.** See below.

Developers can deprecate *offerings* - stop selling, stop updating, sunset a version. They cannot deprecate *entitlements*.

##### Successor pattern

```solidity
address public immutable predecessor;   // whose holders this contract honors; frozen at deploy
address public successor;               // where this contract's holders may go; owner-settable

function setSuccessor(address newSuccessor) external onlyOwner;
function claimFromPredecessor(uint256 predecessorTokenId) external returns (uint256 tokenId);
function honorsContract(address configuredContract, uint256 tokenId) external view returns (bool);
```

Covers contract bugs, paid major versions, and chain migration. Three hard guarantees, each with a dedicated test in `contracts/test/Rub3Invariants.t.sol` that fails if the guarantee is removed:

1. **The old contract validates its tokens forever, regardless.** `successor` is a signpost, not a switch: nothing in `ownerOf`, `activate`, `cooldownReady`, or `isValid` reads it. Setting, repointing, or clearing it changes nothing about an issued token, and neither does the holder migrating - or the owner renouncing ownership entirely.

2. **Migration is holder-initiated, never forced.** Only the *current holder* of a predecessor token can call `claimFromPredecessor`, and only on the successor. Neither contract's owner can push a migration; there is no `forceMigrate` selector to call.

   It is a **snapshot-claim, not burn-to-mint** - necessarily so. Burn-to-mint would require the predecessor to expose a burn, which is exactly the revocation surface that must not exist. The old token is neither destroyed nor moved; the holder ends up with both.

   **What a subscription carries across, and what it does not.** The claim carries the holder's remaining time *and* their snapshotted `renewPrice`. It does **not** carry `period`, which is immutable per contract: the successor's own `period` governs what the carried price buys from then on, so a successor declaring a shorter period raises the effective rate even though the price itself never moved. That takes nothing already granted, because claiming is opt-in and holder-initiated and the original token keeps validating on the old contract at its original terms forever. It does mean **a holder should read the successor's `period` and `price` before claiming**: the claim is the moment they accept the successor's terms, and it is the only protection here. A holder who dislikes those terms simply does not claim.

   **The accepted consequence: migration can duplicate a seat.** The v1 token stays live and freely sellable after the claim, and the v2 token stays honored, so one purchase can end as two concurrently honored seats held by two different wallets. Honored seats are therefore *not* bounded by either contract's `supplyCap`, even though both caps are immutable: each cap bounds the tokens *that contract* mints, not the entitlements alive across a succession chain. This is deliberate and is not bounded in code. Bounding it would need the predecessor to invalidate the old token, which is the revocation surface, and the no-revocation guarantee wins. A developer who cannot accept the duplication ships a paid major version instead: deploy v2 *without* a predecessor, so it accepts no claims and every seat on it is sold.

   Both sides opt in, explicitly: the successor names its `predecessor` at deploy (immutable), and the predecessor's owner points `successor` at it. A v2 deployed *without* a predecessor accepts no claims - that is how a paid major version is shipped while still signposting where it lives.

   **Succession is same-model, by construction.** An access contract cannot declare a subscription predecessor, and a subscription cannot declare an access one: both constructors probe the predecessor over the same discriminator, `period()`, which `Rub3Subscription` requires it to answer and `Rub3Access` requires it to fail. A cross-model pairing reverts at deploy with `IncompatiblePredecessor(address)`, so it is not a mistake a deployer can make. That closes the one path where a claim could grant more than the holder had: an access license carries nothing across in `_afterClaim`, so a subscription predecessor would have let any subscriber - including one lapsed years ago - mint a perpetual license for free.

3. **The trust rule the contract exposes for wrappers: "contract X, or X's successor holding a token claimed from X."** `honorsContract(X, tokenId)` evaluates exactly that in one `eth_call`. A token *bought* on the successor is not a claim, so a wrapper pinned to X does not accept it. The predecessor's opt-in is checked once, at claim time, and recorded permanently - a later `setSuccessor` cannot retroactively unmake a claim that already happened, because a claim already made is a grant.

   **No shipped wrapper consumes this rule yet.** `honorsContract` is a contract capability: it is live and tested on-chain (`test_trustRule_honorsContract`, `test_trustRule_survivesSuccessorRepoint` in `contracts/test/Rub3Invariants.t.sol`), but it is not in the `sol!` interface in `crates/rub3-wrapper/src/rpc.rs` and no Rust path calls it. Every shipped wrapper still verifies ownership against its single hardcoded `CONTRACT` constant, so a holder who claims onto a successor is not honored at launch time today. Wiring the call into the wrapper is outstanding work; the wording below describes what the rule guarantees once a wrapper reads it.

   **The rule spans exactly one hop, by construction.** `honorsContract` compares its argument against this contract's own immutable `predecessor` and nothing further back, so after a second migration (v1 -> v2 -> v3) `v3.honorsContract(v1, tokenId)` is false: a wrapper pinned to v1 does not honor a v3 token. The holder is not stranded by that. No token is ever burned, so their v1 token - and their v2 token, if they claimed one - keeps validating forever on its own contract, which is what a v1-pinned wrapper checks anyway. Claiming onto v3 adds a token; it takes none away.

##### What is enforced by bytecode, and what is convention

The distinction matters because an agent can verify the first list before buying and can only trust the second.

**Bytecode** - check these against the deployed runtime code. The 30 forbidden selectors named across the rows below are exactly the set `contracts/test/Rub3Invariants.t.sol` asserts absent, the set the copy-pasteable loop in `contracts/contracts.md` scans for, and the set `attest::FORBIDDEN_SIGNATURES` mirrors in the wrapper. Those selector rows are a **diagnostic**: a blacklist of names proves nothing by its silence, and the last row - the fingerprint comparison - is what actually decides whether the deployed code is this repository's. (The rows also name `renewPrice(tokenId)`, `renewPriceToken(tokenId)`, `renewPriceAmount(tokenId)`, `wrapperHashList()`, `feeBps()` and `treasury()`, which are functions that *do* exist and are read as part of the check.)

| Property | How an agent checks it |
|---|---|
| No burn, admin transfer, seizure, or pause | The selectors are absent from the runtime bytecode, and a raw call carrying one reverts (there is no fallback). Scan for `burn(uint256)`, `burn(address,uint256)`, `burnFrom(address,uint256)`, `adminTransfer(address,address,uint256)`, `forceTransfer(address,address,uint256)`, `seize(uint256)`, `clawback(uint256)`, `pause()`, `unpause()`, `paused()`, `setPaused(bool)`, `revoke(uint256)`, `revokeToken(uint256)`, `invalidate(uint256)`, `setExpiresAt(uint256,uint256)`, `setRenewPrice(uint256,uint256)`, `forceMigrate(uint256,address)` |
| No proxy, no upgrade hook | `upgradeTo(address)`, `upgradeToAndCall(address,bytes)`, `initialize()` absent; contract code hashes stable across blocks |
| Hash set is append-only | `setWrapperHash(bytes32)`, `removeWrapperHash(bytes32)`, `unrevokeWrapperHash(bytes32)` absent; `wrapperHashList()` only ever grows |
| Renewal terms frozen per token | `renewPrice(tokenId)`, `renewPriceToken(tokenId)` and `renewPriceAmount(tokenId)` do not move after mint; `setRenewPrice(uint256,uint256)`, `setRenewPriceToken(uint256,address)`, `setRenewPriceAmount(uint256,uint256)`, `setExpiresAt(uint256,uint256)` and any other renewal setter are absent from the runtime bytecode; `period` is `immutable`, with no `setPeriod(uint256)`. Free tiers are legitimate, so a `renewPrice` of `0` is conforming |
| Deploy-time parameters frozen | `identityModel`, `tbaImplementation`, `supplyCap`, `cooldownBlocks`, `predecessor` are `immutable` - no `setPredecessor(address)` selector. On `Rub3Factory`, `previousFactory` is `immutable` in the same way, with no `setPreviousFactory(address)`: it decides which predecessors a canonical deploy may name, so repointing it would grant a laundered contract standing after the fact |
| The protocol fee is frozen per contract | `feeBps` and `treasury` are `immutable` on the licence contract and on the `Rub3Factory` that stamped them; `setFeeBps(uint16)` and `setTreasury(address)` are absent from the runtime bytecode. Read `feeBps()` / `treasury()` before buying and they are what that contract will charge for as long as it exists |
| Migration cannot be forced | `claimFromPredecessor` is the only mint path outside `purchase` / `purchaseWithAuthorization`, and it checks `ownerOf(...) == msg.sender` on the predecessor |
| The deployed code is this repository's template, not a modified copy | Zero the immutable byte ranges published in `contracts/canonical-bytecode.json`, `sha256` the result, and compare against that contract's `deployed_bytecode_sha256`. This is the check every row above depends on: they describe the template, and only a fingerprint match says the deployed contract *is* the template. It is name-independent, so a modified copy exposing seizure under an unguessed name fails it while passing the selector scan. `crates/rub3-wrapper/src/attest.rs` pins the same fingerprints and refuses to purchase without a match. `Rub3Factory.isDeployed(addr)` narrows the same question from the other side - a factory deploy is provably an unmodified template on that factory's terms - but the factory's own code has to be fingerprinted first, and its runtime code does not contain the licence implementations, so verifying one means also comparing `accessDeployer()` / `subscriptionDeployer()` against the manifest, which pins all five |

**Convention** - real commitments, but not provable from the bytecode:

| Property | Why it isn't bytecode |
|---|---|
| Registry delisting never invalidates a token | `Rub3Registry` is not built yet (§3.2). Today it is a design commitment; once built, the property holds because the registry has no call into the license contract at all |
| An honest answer from the RPC endpoint | The fingerprint row above reduces to `eth_getCode` being answered truthfully by whatever endpoint the wrapper was packed with. An endpoint that lies returns canonical code for a hostile contract, and nothing on the machine can tell. The honest form of the claim is "an honest view of chain state implies canonical code"; a quorum across independent endpoints, or a light-client-verified read, is what would close it, and neither is built |
| The immutables behind a canonical fingerprint | The comparison zeroes them by construction, so a match says nothing about `identityModel`, `tbaImplementation`, `supplyCap`, `cooldownBlocks`, `predecessor` or the fee terms. Byte-identical canonical code pointed at an attacker-controlled ERC-6551 implementation still matches. Read the getters and check them against a buyer policy - separate work, and not built into the wrapper |
| A revoked binary already running keeps running | Deliberate. The hash set informs new downloads and activations; a switch that could stop a running binary would be a revocation surface |
| The developer keeps publishing builds and hashes | Unenforceable by anyone. It is also the failure mode the invariants are designed to survive: an abandoned contract keeps validating forever, so vendor death depreciates a license rather than confiscating it |

#### Rub3Registry *(planned - implementation.md §3.2)*

Discovery and verification, **never validity** - delisting removes the badge and the listing; it cannot invalidate a token or a session.

```solidity
contract Rub3Registry {
    function register(string calldata appName, address licenseContract) external {
        require(factory.isDeployed(licenseContract), "not canonical");
        require(IOwnable(licenseContract).owner() == msg.sender, "not contract owner");
        // sets appName.rub3.eth → licenseContract + agent card
    }
}
```

Only factory deploys are listable. Each entry doubles as an ERC-8004-style agent card - contract address, price(s), payment methods, content URI, hash set, identity model - so agent purchasing policies can allowlist "verified rub3 contracts" and machine-audit the invariants above before buying.

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
│   ├── Device key challenge - sign block hash, verify vs on-chain pubkey (tier 4)
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
├── Device Key Manager (tier 4)         - deferred, see "Deferred designs"
├── Binary Decryption (tiers 3-4)       - deferred, see "Deferred designs"
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
│   └── Heartbeat IPC - child cannot run if wrapper dies
│
└── App Host
    ├── Rust binary mode: exec embedded binary
    └── Tauri mode: launch Tauri app entry point
```

**Headless mode (built - implementation.md §2.1).** Everything in the tree above except the Wallet Connection webview is signer-agnostic. `activation::ensure_headless(signer, ctx)` runs the same pipeline - enumerate tokens, purchase if empty, cooldown check, activate, sign session, persist - with an operator-supplied signer (env key, keystore, or KMS-backed `Signer` impl). The purchase step opens by attesting the contract's deployed code against the canonical fingerprints compiled into the binary, refusing on a miss with exit code 23 before anything is signed (implementation.md §2.6; it gates purchases only, never launches). It then reads `priceToken()` and pays on the stablecoin rail when five things hold, checked in this order: the contract advertises one, the wallet holds enough of it, the payment token's EIP-712 domain is readable, the operator's spend ceiling (`RUB3_AGENT_MAX_TOKEN_AMOUNT`, in the token's own smallest unit) covers the listed amount, and only then, once a short-lived authorization has been signed, an `eth_call` pre-flight of the `purchaseWithAuthorization` transaction succeeds (which is what catches a payment token lacking the `bytes signature` overload the contracts call); the copy that is broadcast is signed after that, with a window long enough to be mined (implementation.md §2.2). ETH otherwise. The ceiling has no default and must be set before the rail is usable, because the unit belongs to whichever token the contract lists and decimals differ between them. It sits ahead of the signing on purpose: an authorization is spendable by anyone who sees it, so a ceiling that refuses only after one exists has already let the money go. A listed amount above a set ceiling is a refusal with its own exit code (22) rather than a quiet switch of rail, while an agent that holds none of the token, or faces a token with no readable domain or no ceiling configured, is simply on the ETH path it was always on and its printed reason says so without mentioning a spend limit. The cost of signing last is that a token lacking the overload *and* priced above the ceiling reports the refusal rather than the fallback; priced within the ceiling it still falls back to ETH, and exit 22 never claims the rail was healthy. A contract-level failure on a token-side read falls back to ETH with a printed cause, a transport failure stops the run - a blinking node must never silently change the currency. The ETH path it falls back to is itself bounded, by `RUB3_AGENT_MAX_ETH_WEI` in wei, checked between reading `price()` and sending the transaction so a refusal costs no gas; that ceiling has a default (0.1 ETH) because wei is a fixed unit, so the fallback is never an escape from policy. The rail it chose is reported back in `HeadlessOutcome::PurchasedAndActivated { paid }` and printed on the CLI's one-line result. Front doors are Cargo features: `webview` pulls `wry`/`tao`, `headless` pulls neither, so a headless build has no GUI dependency at all - smaller binary, container-friendly. This is the primary path for agent-operated software; the webview is the human fallback.

Key handling is contained rather than spread. Headless necessarily signs and broadcasts, which the interactive flows never do, so the capability lives behind one feature and one object-safe trait whose only primitive is "sign this 32-byte digest" - a KMS or enclave serves it without releasing a key. Exactly one type, `signer::LocalSigner`, ever holds raw key material. The launcher strips every `RUB3_AGENT_*` credential variable from the wrapped binary's environment - the key, the keystore path, and both password sources - so the licensed product is not handed the credential or its location. `agent_env::AGENT_ENV_VARS` is that list, and it is scoped to credentials rather than to the whole `RUB3_AGENT_*` family: `RUB3_AGENT_MAX_TOKEN_AMOUNT` and `RUB3_AGENT_MAX_ETH_WEI` are spend policy, not secrets, so they are deliberately not on it. The child still runs as the same UID, so this is containment, not a sandbox.

#### Source layout

The per-module map lives in `README.md` → "Project structure", which is the single place it is maintained. It covers every module, including the two deferred scaffolds (`device.rs`, `decrypt.rs`) and everything `lib.rs` feature-gates.

#### Dependencies

| Crate | Purpose |
|---|---|
| `clap` | CLI argument parsing |
| `alloy` | Ethereum RPC, ABI encoding (ownerOf, price, activate, cooldown) |
| `k256` | secp256k1 ECDSA signature recovery + device keypair generation |
| `sha2` | SHA-256 for activation/session message hash |
| `sha3` | Keccak-256 for Ethereum address derivation + personal_sign |
| `hex` | Hex encoding/decoding |
| `wry` | Embedded webview for activation UI (feature `webview`; absent from headless builds) |
| `tao` | Native window/event loop (feature `webview`; absent from headless builds) |
| `zeroize` | Wiping decoded key bytes and keystore passwords (feature `headless`) |
| `serde` / `serde_json` | Session/proof serialization |
| `dirs` | Platform data directory resolution |
| `chrono` | RFC-3339 timestamps |
| `nix` / `libc` | Unix signal handling |
| `keyring` | Cross-platform OS keychain access (tier 4, keychain/enclave storage) |
| `rand` | Cryptographic random nonce generation |
| `aes-gcm` | AES-256-GCM binary encryption/decryption (tiers 3-4, when encrypt_binary = true) |

---

### 3. rub3 SDK (Rust Crate) *(not started - implementation.md §3.5)*

A thin in-process crate the wrapped app links: `rub3::heartbeat()` and `rub3::session()`, talking to the wrapper over the IPC socket. The one design constraint worth stating here is that application code keys persistent data on `SessionInfo::user_id` (the TBA under the account model, the wallet under the access model), never on the current signing wallet, so a transfer or a wallet rotation does not orphan the user's data. The proposed surface is in implementation.md §3.5.

---

### 4. rub3 CLI *(not started - implementation.md §2.5)*

`pack`, `deploy`, `fetch`, `register`. The split that matters architecturally: `--tier` is a *pack*-time choice, baked into the wrapper binary, while `--identity` and `--cooldown-blocks` are *deploy*-time choices written into the contract. The two cannot be reconciled afterwards, which is why a repacked wrapper must still match a deployed contract's tier expectations. The proposed command surface is in implementation.md §2.5.

---

### 5. Tauri Plugin *(not started - implementation.md §5.3)*

`tauri-plugin-rub3` would expose the same `SessionInfo` to a Tauri frontend over `invoke`, and render token selection and renewal in the app's own webview rather than the wrapper's. Proposed surface in implementation.md §5.3.

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
  3. Resolves to a DIFFERENT address → hard fail (active-attack signature)
  4. Fails to resolve (lapsed name, dead registry, offline) → warn and proceed
  5. Match → proceed, show verified badge
```

### Two layers of trust

**Layer 1 - Developer's own ENS** (`myapp.eth`) - decentralized, developer-controlled.

**Layer 2 - rub3.eth subdomain** (`myapp.rub3.eth`) - permissionless, on-chain proof of contract ownership. Adds "verified" badge in UI.

ENS and the registry are **purchase-time trust signals, not launch-time dependencies**. The embedded contract address is the root of trust after first purchase - a lapsed ENS registration or a registry edit must never brick re-activation for paid holders. Registry state affects discovery and badges only; validity comes from the license contract alone.

---

## Binary Protection

### Binary verification (all tiers)

Rotating a single hash slot would invalidate the verifiability of every binary already downloaded, so the slot is an append-only set (implementation.md §2.4):

```solidity
enum HashStatus { Unknown, Valid, Revoked }
mapping(bytes32 => HashStatus) public wrapperHashes;
mapping(bytes32 => string)     public revocationReason;

function addWrapperHash(bytes32 hash) external onlyOwner;                             // append a release
function revokeWrapperHash(bytes32 hash, string calldata reason) external onlyOwner;  // flag a compromised build

function isWrapperHashValid(bytes32 hash) external view returns (bool);
function wrapperHashCount() external view returns (uint256);
function wrapperHashAt(uint256 index) external view returns (bytes32);
function wrapperHashList() external view returns (bytes32[] memory);   // full set, for pre-purchase audit
```

The constructor seeds the set from a `bytes32[]` - one release ships several binaries, one per platform - and every later build is appended on-chain.

Status is monotone: `Unknown → Valid → Revoked`, terminal. A hash already in the set is rejected by `addWrapperHash` whether it is valid *or* revoked, so the set can never be rewritten; a mistaken revocation is corrected by publishing a fresh build, not by editing history. Revocation requires a non-empty reason - a compromised build is flagged on-chain *with the reason stated*, not silently.

Old binaries stay verifiable; a compromised release is flagged with a reason. Revoking a **binary hash** never touches **token validity** - the holder downloads a patched build and the same license just works. This is structural, not a promise: `ownerOf`, `isValid`, and `activate` never read `wrapperHashes`, and `contracts/test/Rub3Invariants.t.sol` revokes every hash in the set and asserts all three are unaffected.

Honest limit: the hash set informs new downloads and activations; it cannot retroactively disable compromised binaries already running. A kill switch that could would be a revocation mechanism, and it must not exist.

Planned alongside it: `contentURI` (IPFS/Arweave) recorded on the contract, making it a complete distribution record - `rub3 fetch <contract>` downloads the binary and verifies its hash on-chain (implementation.md §3.1).

Trust chain: **ENS → contract → content URI → binary hash → running wrapper**

### Binary encryption (tiers 3-4, optional) *(deferred)*

Would ship the embedded app binary as ciphertext the wrapper decrypts into memory only after the session check passes. Not built; see "Deferred designs" below.

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
                                    [encrypt_binary?] → Decrypt (deferred)
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
                                                    manual paste - see
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
                                                   [encrypt_binary?] Decrypt (deferred)
                                                         │
                                                    Launch app
```

### Tier 4: hardened (device-bound)

Deferred; no launch flow is implemented. See "Deferred designs".

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

Deferred; no session format is fixed. See "Deferred designs".

---

## Deferred designs

Two designs beyond tier 3 are specified only as intent. Both are cut from the active roadmap; the rationale is in `implementation.md` → "Deferred".

**Tier 4 (`hardened`)** would bind a session to one machine. At activation the wrapper would generate an ephemeral secp256k1 device key, register its public key on-chain alongside the session, and sign the current block hash at every launch, so a copied session file is useless without the device key and no TTL is needed. It is deferred because device binding treats fleet cloning as an attack while agent fleets clone VMs as a legitimate pattern; seats (implementation.md §3.4) are the right concurrency primitive.

**Binary encryption** would ship the embedded app binary as AES-256-GCM ciphertext, decrypted into memory only after the session check passes. The key-encryption key would be derived from public on-chain values, SHA-256 over the contract address, chain id and salt, plus the device-key fingerprint at tier 4; the contract would store only SHA-256 of the binary key, for verification, and never release a secret, since a public chain cannot release one to a single holder. It is deferred because extraction resistance was never a goal, as `ideation.md` → "What This Is Not" states.

The `src/device.rs` and `src/decrypt.rs` scaffolds stay in the tree behind the `device-key` and `binary-encryption` Cargo features. `device.rs` is compiled by the `tier-4` bundle, which enables `device-key` and which CI and the AGENTS.md matrix build; `decrypt.rs` is compiled by no tier bundle at all, because `binary-encryption` is an orthogonal add-on that only an explicit composition such as `tier-3,binary-encryption` enables.

---

## Security Model

### Wallet as trust boundary

In interactive mode the wrapper never holds a wallet private key - signing happens in the user's wallet. In headless mode (built, implementation.md §2.1) the operator explicitly supplies a signer (env key, keystore, or KMS-backed trait impl); key custody is the operator's policy decision, and serious agent deployments front it with spending limits and allowlists rather than granting an unconstrained wallet. Session signatures are free - no on-chain effect. The wrapper does hold a device private key (tier 4), but this is an ephemeral key used only for device binding - it cannot sign transactions or move funds.

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

In account model, the TBA address (`user_id`) is stable across transfers. An attacker who obtains a cached session file gets a `user_id` that is currently controlled by someone else's wallet. The session signature verifies against the wallet that signed it - after transfer, the old wallet no longer controls the NFT, but the session remains valid until invalidated.

Invalidation timing depends on tier:
- Tiers 1: old session valid until TTL expires (time-limited lame duck)
- Tier 2: invalid on next launch (`ownerOf` check fails)
- Tier 3: invalid immediately if new holder activates (session_id changes)
- Tier 4: invalid immediately (device key + session_id)

This is intentional and matches the semantics: **transfer sells the account to the new holder, who takes full control at the next activation.** Higher tiers make the handover faster.

### Defense layers summary

```
Attestation:   masked code hash vs fingerprints pinned in the binary (pre-purchase: refuses to buy from non-canonical code)
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

- Contract deployment: one per app, ~$1–5 on Base (via `Rub3Factory` - deploys are never charged by rub3; the factory deploy itself is a one-off that rub3 pays, not the developer)
- Protocol fee: a 2–3% split executed inside `purchase()`/`renew()` on factory deploys, on both payment rails - no additional infrastructure; settlement is continuous and on-chain, with each side sweeping its own balance
- RPC read calls: varies by tier. Tier 0: zero. Tiers 1-2: one per renewal. Tiers 3-4: one per launch (`activeSessionId` + `ownerOf`). Public RPC or Alchemy free tier sufficient.
- RPC write calls: tiers 3-4 only. One `activate()`/`activateDevice()` tx per session creation. ~$0.001 on Base.
- Session files: ~500 bytes each, one per token per device. Negligible storage.
- Device keys (tier 4): one per token per device. Stored in OS keychain or Secure Enclave - no additional disk storage.
- No backend. No database. No auth service. All verification is either local crypto or on-chain reads.
