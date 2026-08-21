# rub3 - Architecture

This file owns design rationale: why the system is shaped the way it is. Security tiers, the session model, identity models, launch flows, and the ownership invariants are argued here, and this is what to read before changing behaviour. It does not own status, which lives in [implementation.md](implementation.md); contract operations and fee mechanics, which live in [contracts/contracts.md](contracts/contracts.md); the test inventory, which lives in [testing.md](testing.md); positioning, which lives in [ideation.md](ideation.md); or build and run instructions, which live in [README.md](README.md).

## North Star

rub3 exists to let machines buy, verify, and resell software without asking anyone's permission. The unit of adoption is one closed loop an agent can complete end to end:

```text
discover → pay → fetch → verify → run → (resell)
```

Three commitments shape every design decision below:

**Two front doors, one rail.** All session crypto (signing, calldata encoding, receipt polling) is native Rust. Headless activation - signer in, session out - is the primary path and needs no webview; the interactive webview flow is the human fallback floor. Everything below the front door (RPC, session model, persistence, supervision) is shared.

**The token is the invariant; everything else is versioned.** License contracts are immutable - no proxies, no upgrade hooks, no revocation surface. Evolution only ever changes what is *offered* going forward (price, successor contracts, registry listings), never what was *granted* (held tokens and their validation logic).

**One licence model.** rub3 sells a licence once and it is valid for as long as it is held: `Rub3Access`, no expiry, nothing to renew, `ownerOf` the whole entitlement. A time-bounded second model was built and then removed before any deploy (implementation.md §2.10), so an agent reading a listing never has to work out which kind of licence it is about to buy.

| | Can never change | Can change (affects future only) |
|---|---|---|
| **Developer** | validity of issued tokens; transfer rights; supply cap; identity model; TBA implementation; cooldown; predecessor link; protocol fee terms (`feeBps`, `treasury`) | price for new sales on either rail (`price`, `priceToken` / `priceAmount`); wrapper hash set (append + flag only); successor pointer; registry listing |
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
| Cost | $0.01–0.05 per mint transaction |
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

**What binds an authorization.** The token signs `from`, `to`, `value`, `validAfter`, `validBefore`, and `nonce` - and nothing else, so the mint recipient is not covered by default. rub3 binds it through the nonce: `purchaseAuthorizationNonce(recipient, salt)` is derived by the contract, not accepted from the caller, so a submitter who changes the recipient derives a different nonce and produces a digest the buyer never signed. The derivation carries a domain tag and the contract's own address, so the nonce is worthless anywhere else. Replay is the token's own single-use nonce, backed by a balance-delta check in the licence contract so a mint cannot happen unless the money actually arrived.

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

```text
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

```text
┌──────────────┐     ┌─────────────────────┐     ┌──────────────────────────┐
│   Developer   │     │   Base (L2)          │     │        User              │
│              │     │                     │     │                          │
│  App binary   │     │  Rub3Access         │     │  Wallet                  │
│  rub3 CLI    │────▶│  Rub3Factory        │◀────│  rub3 Wrapper           │
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
| 3 | `cooldown` | At activation + every launch | 1 per activation | 1 session per cooldown window, new activation kills old | Seat-limited tools, high-value apps |
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

```text
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
| 30 days | Long-lived desktop apps, minimal re-signing |

Session files stored at `~/.rub3/sessions/<app_id>/<token_id>.json` - one per token, not one per app.

---

## Token Selection

A wallet may own multiple tokens from the same contract. At session creation (first launch or renewal), the wrapper presents a token selector after wallet connection.

```text
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

For access model, the display omits the Account field and shows wallet address instead.

If only one token is owned, the selector is skipped and that token is auto-selected.

If no tokens are owned, the purchase UI is shown instead.

**Implementation:** The wrapper calls `tokensOfOwner(wallet)` (ERC-721 Enumerable) to retrieve owned token IDs. If the contract does not implement enumerable, the wrapper falls back to scanning `Transfer` events filtered by recipient.

Session files are keyed on both app_id and token_id: `~/.rub3/sessions/<app_id>/<token_id>.json`. This allows each token to maintain its own cached session - switching between tokens at launch resumes the correct cached session without re-authenticating.

---

## Transaction Confirmation

Tiers 3-4 require at least one on-chain tx (purchase and/or activate) during the activation flow. In **interactive mode** the wrapper never holds keys and never broadcasts txs itself - it encodes calldata, surfaces it to the user, and waits for the tx to confirm. In **headless mode** (built, implementation.md §2.1) the operator supplies a signer explicitly - env key, keystore, or KMS-backed `Signer` impl - and the wrapper signs and broadcasts directly; there is no confirmation UI because there is no user round-trip.

For interactive builds, how the "wait" happens is an orthogonal concern. **Two implementations ship today: Manual and Auto-detect.** Both the purchase and the cooldown screens offer them as tabs, with Manual always present.

| Mode | Status | Reliance | Tolerant of offline activation | JS bundle |
|---|---|---|---|---|
| **Manual** | built, implementation.md §1.7 / §1.8 | User copies a tx hash back into the wrapper | yes (paste later) | none |
| **Auto-detect** | built, implementation.md §5.1a | Chain RPC (filter `eth_getLogs` / read `lastActivationBlock`) | no | none |
| **WalletConnect** | planned, implementation.md §5.1b | Reown relay + chain RPC | no | ~255 KB vendored |
| **Headless** | built, implementation.md §2.1 | operator-supplied signer + chain RPC | n/a - no user round-trip | none |

Manual is the floor and stays available whatever else lands: no dependencies, and the one path that still works when the user's machine is offline as they open the wrapper but they want to send the tx from a hardware wallet elsewhere and paste the hash later.

The design commitment behind the richer modes is that they are **additive tabs on the same screens, not replacements**. Whichever tab produces a tx hash hands off to the same receipt poller, which validates `status == true`, asserts `receipt.to == contract`, and recovers the minted tokenId (purchase) or the `activeSessionId` (activate). The rest of the session pipeline does not care which tab the hash came from, so adding a mode cannot change what a confirmed activation means. Availability is decided at build and deploy time: Auto-detect requires `onchain-write` (always present in tiers 3-4); WalletConnect requires the `wallet-connect` Cargo feature and a non-placeholder `wc_project_id` in the packed wrapper, since the Reown project id is developer-supplied per deployment rather than a shared rub3 credential. WalletConnect remains a Phase 5 item (implementation.md §5.1b).

---

## Components

### 1. Smart Contracts

#### Rub3Access (one-time purchase)

The only licence model. A time-bounded `Rub3Subscription` existed alongside it until implementation.md §2.10 removed it, so there is no expiry, no renewal, and no second shape a buyer has to tell this one apart from.

ERC-721 + ERC-721Enumerable with payable `purchase(address recipient)` and `purchaseWithAuthorization(address recipient, PaymentAuthorization auth)`, where `PaymentAuthorization` is `(address from, uint256 validAfter, uint256 validBefore, bytes32 salt, bytes signature)`:
- Price per token on both rails: `price` (wei) and `priceToken` / `priceAmount` (an EIP-3009 ERC-20 and its own smallest unit). Independent quotes, not a conversion - the contract holds no oracle. Optional supply cap (immutable)
- `recipient == address(0)` defaults to `msg.sender` on the ETH path and to `auth.from`, the buyer, on the authorization path - never to the submitter
- Both paths reach one `_mintPurchased`, so a licence bought with USDC is identical in state and events to one bought with ETH
- `mapping(bytes32 => HashStatus) wrapperHashes` - append-only set of distributed-binary SHA-256s (see [Binary verification](#binary-verification-all-tiers))
- `uint8 identityModel` - `0 = access`, `1 = account` - readable by wrapper

On-chain check: `ownerOf(tokenId) == walletAddress`

`Rub3Access` implements ERC-721Enumerable so the wrapper can call `tokensOfOwner()` directly.

#### Activation and session management (tiers 3-4)

`Rub3Access` includes the activation/session management interface for tiers 3-4:

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

    // Reverts PredecessorNotCanonical(address) unless params.predecessor is.
    function deployAccess(Rub3LicenseParams calldata) external returns (address);
}
```

**A factory deploy may only succeed a canonical predecessor.** `claimFromPredecessor` charges nothing, because migration must never be taxed, so an unconstrained `predecessor` would let a whole fee-free sale be laundered onto a registry-listed contract: sell on a direct deploy, then deploy the successor through the factory naming it as predecessor, and every holder claims onto a fee-bearing `isDeployed` contract with the treasury never paid. The factory therefore accepts `address(0)`, its own deployments, or those of a factory reachable through the immutable `previousFactory` chain - which is what keeps an older factory's contracts migratable when rub3 changes its take by deploying a new factory. Direct deploys and the permissionless deployer helper are untouched: they grant no `isDeployed` row, so there is nothing there to launder onto. The cost is that a pre-factory contract cannot migrate its holders onto a canonical contract *through the factory*; it migrates onto a directly deployed successor instead, keeping every ownership guarantee and forgoing only the row. See `contracts/contracts.md` → "A factory deploy may only succeed a canonical predecessor".

The fee split executes on-chain inside `purchase()`, on **both** payment rails: `feeBps` of what arrived to `treasury`, the remainder to the developer's `withdraw()` balance. **Immutable per contract** - `feeBps` and `treasury` are `immutable` on the factory *and* on every contract it deploys, so a developer's economics can never change after deploy; rub3 changes its take only by deploying a new factory, which affects contracts deployed by that factory and nothing that already exists. Direct (non-factory) deployment of the open-source contracts is always possible: fee-free and unrecorded by design, not a gap.

The factory path stamps the fee and grants an `isDeployed` row. That row is a durable canonical record today, and the eligibility criterion for the registry (implementation.md §3.2) and marketplace (§4.3) once they ship. The registry is built and the marketplace is not, neither is deployed anywhere, and the fee does not go live ahead of them: the contracts are not deployed to mainnet or declared ready for use until the registry is ready, so the factory and the registry launch together.

The getters, the deploy recipe, the split and sweep calls, where the fee's scope ends, and why a matching fingerprint is never evidence of canonical deployment all live in `contracts/contracts.md` → "The protocol fee". Only the design arguments below belong here.

#### Why the fee split is shaped this way

Three properties of the split are load-bearing rather than incidental, and each closes a specific failure:

- **Charged on the amount received, not the listed price.** Charging what arrived makes the arithmetic exact by construction: the two shares are the payment, with nothing left over. It is also the only quantity that is always the money actually in hand - on the stablecoin rail `received` is a measured balance delta, and a payment token can credit more than it was asked for, so charging a listed price there would leave a surplus untaxed and unaccounted for and the two shares would no longer sum to the payment. And it is one rail-independent rule that holds however the money arrived, rather than one that depends on trusting a price variable at accrual time. The zero-price evasion route this bullet used to rest on - list at zero, take the real price as "overpayment" - is now closed one step earlier, in `_payEth`, which reverts unless `msg.value` equals the listed price, so the fee rule no longer carries that burden.
- **Rounding favours the developer.** Integer division, so a sub-unit fee is zero rather than one. A fee that rounded up could exceed the payment at the smallest amounts.
- **Accrued in the contract, not pushed to the treasury on the money path.** `treasury` is immutable, so a transfer inside `purchase()` would let a recipient that reverts on receipt break every purchase on that contract forever, unfixably. Accruing keeps the buyer's path free of calls out, and a collection failure becomes rub3's problem rather than the buyer's. The mirror image of that immutability - a treasury that is lost or one day cannot receive strands every fee on every contract its factory deployed, permanently - is an operational requirement rather than a contract one, and is stated in `contracts/contracts.md` → "Treasury custody, and the pre-mainnet proof".

**Where the fee's scope ends is an economic argument, not a technical lock.** The fee is charged on value that arrives *through* the contract's payment functions; anything reaching the contract another way is released whole to the developer. That is deliberate. A developer who wants to route around the fee can already sell off-chain and list at zero, and the shipped contracts have no owner-mint and no airdrop, so a zero-price listing makes the licence free for everyone rather than for that developer's customers alone. Permissionlessness, not an on-contract lock, is what defends the zero-price route.

**The fee's denomination is not constrained on-chain, and that is the accepted position.** The fee accrues in whatever asset the payment arrived in, so a developer who lists `priceToken` as an asset of their own choosing decides what rub3's share is *denominated in* as well as what it is a percentage of. Two consequences follow, and both are real: at 200-300 bps a payment below 34-50 of the token's smallest units rounds the fee to zero, and even a large amount priced in a token nobody trades is a percentage of nothing. Closing either on-chain means the contracts holding an economic policy about which tokens count - an allowlist, or a minimum amount per rate - which is exactly the oracle-shaped judgement the design refuses to carry, in a contract that can never be changed once deployed and that would then have to be redeployed every time a token's standing changed. **The denomination question is answered at the discovery layer instead.** The registry (implementation.md §3.2) maintains a recognised-token list and ranks and lists canonical contracts by the token they are priced in, so a contract quoting a token rail in an asset nobody recognises ranks below one that does, while the native rail counts as recognised - an ETH-only listing quotes no token and its fee accrues in ETH, the one asset this argument was never about. That keeps the same economic argument the rest of the fee rests on - routing around it costs the carrot - rather than converting it into an on-chain lock the invariants forbid. It is a requirement on the registry, which is built and deployed nowhere; nothing enforces it today.

**Why the factory needs a helper contract.** `Rub3Access`'s creation code is over 16 KB against a 24,576-byte runtime limit, so a factory that could `new` it directly would have almost nothing left for itself. The factory builds one `Rub3AccessDeployer` in its own constructor instead and holds its address as an immutable. The consequence for an auditor: the factory's own bytecode fingerprint does not pin which licence implementation it deploys, so verifying a factory means fetching the code at `accessDeployer()` and comparing it against the canonical manifest too.

#### Rub3Metered *(planned - implementation.md §4.1)*

A second billing model unique to runtime enforcement, and the only one still planned: the launch gate requires a micropayment (per launch, per session-hour, or per N launches) settled in USDC. Same protocol fee, much higher-frequency flow than one-time sales. It is not a subscription - nothing about it makes an issued token expire - and it is `[not started]`.

#### Ownership invariants (all license contracts)

Live in `Rub3License` (implementation.md §2.4). Enforced by construction, machine-verifiable by any buyer before purchase:

- **No revocation surface.** No burn, no admin transfer, no pause on `ownerOf` / `honorsContract` / `activate` for issued tokens. Not policy - absent from the bytecode.
- **No proxies.** Contract code, and therefore license terms, are frozen at deploy. No upgrade hook, no delegatecall, no initializer.
- **Nothing a held token owes over time.** A licence is bought once and is valid for as long as it is held, so there is no expiry to move, no renewal price to reprice, and no term a later owner call could reach. That is the shape §2.10 chose over a second model whose terms had to be frozen per token to say the same thing.
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

1. **The old contract validates its tokens forever, regardless.** `successor` is a signpost, not a switch: nothing in `ownerOf`, `activate`, `cooldownReady`, or `honorsContract` reads it. Setting, repointing, or clearing it changes nothing about an issued token, and neither does the holder migrating - or the owner renouncing ownership entirely.

2. **Migration is holder-initiated, never forced.** Only the *current holder* of a predecessor token can call `claimFromPredecessor`, and only on the successor. Neither contract's owner can push a migration; there is no `forceMigrate` selector to call.

   It is a **snapshot-claim, not burn-to-mint** - necessarily so. Burn-to-mint would require the predecessor to expose a burn, which is exactly the revocation surface that must not exist. The old token is neither destroyed nor moved; the holder ends up with both.

   **A claim carries no terms, because a licence has none to carry.** It is bought once and holding it is the whole entitlement, on the predecessor and on the successor alike, so there is nothing to snapshot across and nothing a successor's own listing can change about what the claimed token is worth. That was not true of the time-bounded model §2.10 removed, where remaining time and a frozen renewal price had to be carried and `period` deliberately did not carry - the whole reason claiming had to be the moment a holder accepted the successor's terms.

   **The accepted consequence: migration can duplicate a seat.** The v1 token stays live and freely sellable after the claim, and the v2 token stays honored, so one purchase can end as two concurrently honored seats held by two different wallets. Honored seats are therefore *not* bounded by either contract's `supplyCap`, even though both caps are immutable: each cap bounds the tokens *that contract* mints, not the entitlements alive across a succession chain. This is deliberate and is not bounded in code. Bounding it would need the predecessor to invalidate the old token, which is the revocation surface, and the no-revocation guarantee wins. A developer who cannot accept the duplication ships a paid major version instead: deploy v2 *without* a predecessor, so it accepts no claims and every seat on it is sold.

   Both sides opt in, explicitly: the successor names its `predecessor` at deploy (immutable), and the predecessor's owner points `successor` at it. A v2 deployed *without* a predecessor accepts no claims - that is how a paid major version is shipped while still signposting where it lives.

   **A predecessor is probed at deploy, because the pointer is immutable.** `Rub3License`'s constructor rejects a `predecessor` with no code, or one that cannot answer `successor()`, with `IncompatiblePredecessor(address)`. A mistyped address would otherwise brick every holder's claim forever, with redeployment the only remedy. There is nothing further to check: with one licence model, a rub3 licence contract is the only thing a predecessor can be. The extra model probe that used to sit on each concrete contract went with §2.10's second model.

3. **The trust rule the contract exposes for wrappers: "contract X, or X's successor holding a token claimed from X."** `honorsContract(X, tokenId)` evaluates exactly that in one `eth_call`. A token *bought* on the successor is not a claim, so a wrapper pinned to X does not accept it. The predecessor's opt-in is checked once, at claim time, and recorded permanently - a later `setSuccessor` cannot retroactively unmake a claim that already happened, because a claim already made is a grant.

   **No shipped wrapper consumes this rule yet.** `honorsContract` is a contract capability: it is live and tested on-chain (`test_trustRule_honorsContract`, `test_trustRule_survivesSuccessorRepoint` in `contracts/test/Rub3Invariants.t.sol`), but it is not in the `sol!` interface in `crates/rub3-wrapper/src/rpc.rs` and no Rust path calls it. Every shipped wrapper still verifies ownership against its single hardcoded `CONTRACT` constant, so a holder who claims onto a successor is not honored at launch time today. Wiring the call into the wrapper is outstanding work; the wording below describes what the rule guarantees once a wrapper reads it.

   **The rule spans exactly one hop, by construction.** `honorsContract` compares its argument against this contract's own immutable `predecessor` and nothing further back, so after a second migration (v1 -> v2 -> v3) `v3.honorsContract(v1, tokenId)` is false: a wrapper pinned to v1 does not honor a v3 token. The holder is not stranded by that. No token is ever burned, so their v1 token - and their v2 token, if they claimed one - keeps validating forever on its own contract, which is what a v1-pinned wrapper checks anyway. Claiming onto v3 adds a token; it takes none away.

##### What is enforced by bytecode, and what is convention

The distinction matters because an agent can verify the first list before buying and can only trust the second.

**Bytecode** - check these against the deployed runtime code. The 25 forbidden selectors named across the rows below are exactly the set `contracts/test/Rub3Invariants.t.sol` asserts absent, the set the copy-pasteable loop in `contracts/contracts.md` scans for, and the set `attest::FORBIDDEN_SIGNATURES` mirrors in the wrapper. Those selector rows are a **diagnostic**: a blacklist of names proves nothing by its silence, and the last row - the fingerprint comparison - is what actually decides whether the deployed code is this repository's. (The rows also name `wrapperHashList()`, `feeBps()` and `treasury()`, which are functions that *do* exist and are read as part of the check.)

| Property | How an agent checks it |
|---|---|
| No burn, admin transfer, seizure, or pause | The selectors are absent from the runtime bytecode, and a raw call carrying one reverts (there is no fallback). Scan for `burn(uint256)`, `burn(address,uint256)`, `burnFrom(address,uint256)`, `adminTransfer(address,address,uint256)`, `forceTransfer(address,address,uint256)`, `seize(uint256)`, `clawback(uint256)`, `pause()`, `unpause()`, `paused()`, `setPaused(bool)`, `revoke(uint256)`, `revokeToken(uint256)`, `invalidate(uint256)`, `forceMigrate(uint256,address)` |
| No proxy, no upgrade hook | `upgradeTo(address)`, `upgradeToAndCall(address,bytes)`, `initialize()` absent; contract code hashes stable across blocks |
| Hash set is append-only | `setWrapperHash(bytes32)`, `removeWrapperHash(bytes32)`, `unrevokeWrapperHash(bytes32)` absent; `wrapperHashList()` only ever grows |
| Deploy-time parameters frozen | `identityModel`, `tbaImplementation`, `supplyCap`, `cooldownBlocks`, `predecessor` are `immutable` - no `setPredecessor(address)` selector. On `Rub3Factory`, `previousFactory` is `immutable` in the same way, with no `setPreviousFactory(address)`: it decides which predecessors a canonical deploy may name, so repointing it would grant a laundered contract standing after the fact |
| The protocol fee is frozen per contract | `feeBps` and `treasury` are `immutable` on the licence contract and on the `Rub3Factory` that stamped them; `setFeeBps(uint16)` and `setTreasury(address)` are absent from the runtime bytecode. Read `feeBps()` / `treasury()` before buying and they are what that contract will charge for as long as it exists |
| Migration cannot be forced | `claimFromPredecessor` is the only mint path outside `purchase` / `purchaseWithAuthorization`, and it checks `ownerOf(...) == msg.sender` on the predecessor |
| Registry delisting never invalidates a token | `Rub3Registry` (§3.2) reaches a licence contract only through `STATICCALL`, and the EVM refuses any state change under one. Walk the registry's runtime opcodes, skipping each `PUSH1..PUSH32` immediate, and no `CALL`, `CALLCODE`, `DELEGATECALL`, `CREATE`, `CREATE2` or `SELFDESTRUCT` appears at all - so there is no opcode left in it that could write to a licence contract, hold ETH, deploy anything, or destroy itself. `test_audit_registryHoldsNoStateChangingExternalCall` in `contracts/test/Rub3Registry.t.sol` runs exactly that walk, with a positive control on a licence contract, which does contain a `CALL`. What stays convention is how a *consumer* reads a delisting, and the wrapper's reading is that discovery has no bearing on a held token |
| The deployed code is this repository's template, not a modified copy | Zero the immutable byte ranges published in `contracts/canonical-bytecode.json`, `sha256` the result, and compare against that contract's `deployed_bytecode_sha256`. This is the check every row above depends on: they describe the template, and only a fingerprint match says the deployed contract *is* the template. It is name-independent, so a modified copy exposing seizure under an unguessed name fails it while passing the selector scan. `crates/rub3-wrapper/src/attest.rs` pins the same fingerprints and refuses to purchase without a match. `Rub3Factory.isDeployed(addr)` narrows the same question from the other side - a factory deploy is provably an unmodified template on that factory's terms - but the factory's own code has to be fingerprinted first, and its runtime code does not contain the licence implementation, so verifying one means also comparing `accessDeployer()` against the manifest, which pins all five. A published fingerprint only covers the releases the comparator already has, so a contract built from a **later** release fails this row indistinguishably from a modified copy. `Rub3CodeRegistry` is the append-only on-chain record that tells the two apart (implementation.md §2.9); it is consulted only on a miss, its own code is fingerprinted by this same row before its answer is believed, and none is deployed yet |

**Convention** - real commitments, but not provable from the bytecode:

| Property | Why it isn't bytecode |
|---|---|
| Code-registry deprecation never invalidates a token | `Rub3CodeRegistry` (§2.9) has no status that could - `Deprecated` is "not recommended for new purchases", and the record stays whole. Bytecode proves the absence of removal and rewrite, which is asserted in `test/Rub3CodeRegistry.t.sol`; what stays convention is how a *consumer* reads the status. The wrapper's reading is to warn and buy, and nothing on its launch path consults the registry at all |
| The code registry's owner key is not misused | Append-only bounds a compromise of it to *additions*, each a permanent public `Published` event, and leaves it unable to remove, rewrite, or invalidate. That is a bound and a detection surface, not a prevention: alarming on those events is monitoring, and no watcher is built |
| An honest answer from the RPC endpoint | The fingerprint row above reduces to `eth_getCode`, and its code-registry fallback to `eth_call`, being answered truthfully by whatever endpoint the wrapper was packed with. An endpoint that lies returns canonical code for a hostile contract, and lies about the registry's own code in the same breath, so the second authority neither dilutes this nor compounds it - one dishonest view of chain state defeats both reads at once, and nothing on the machine can tell. **This is the largest residual risk in the whole scheme and it is unclosed.** The honest form of the claim is "an honest view of chain state implies canonical code"; a quorum across independent endpoints, or a light-client-verified read, is what would close it, and neither is built |
| The immutables behind a canonical fingerprint | The comparison zeroes them by construction, so a match says nothing about `identityModel`, `tbaImplementation`, `supplyCap`, `cooldownBlocks`, `predecessor` or the fee terms. Byte-identical canonical code pointed at an attacker-controlled ERC-6551 implementation still matches. Read the getters and check them against a buyer policy - separate work, and not built into the wrapper |
| A revoked binary already running keeps running | Deliberate. The hash set informs new downloads and activations; a switch that could stop a running binary would be a revocation surface |
| The developer keeps publishing builds and hashes | Unenforceable by anyone. It is also the failure mode the invariants are designed to survive: an abandoned contract keeps validating forever, so vendor death depreciates a license rather than confiscating it |

#### Rub3CodeRegistry *(implementation.md §2.9)*

The version authority behind the wrapper's pinned fingerprint table, and **not** a listing of products - `Rub3Registry` above is that, and the two share nothing but a word.

```solidity
contract Rub3CodeRegistry is Ownable2Step {   // no proxy, no removal, no status downgrade
    function record(bytes32 maskedCodeHash) external view returns (Release memory);
    function latestOffsetTables(uint256 count) external view returns (ByteRange[][] memory);  // newest first
    function offsetTableWindow(uint256 start, uint256 count) external view returns (ByteRange[][] memory);
    function offsetTables() external view returns (ByteRange[][] memory);   // the whole set, for a watcher
    function publish(bytes32 mch, Role role, string calldata contractName, string calldata version,
                     bytes32 sourceCommit, string calldata solcVersion,
                     ByteRange[] calldata offsets) external onlyOwner;
    function deprecate(bytes32 mch, string calldata reason) external onlyOwner;  // advises, never invalidates
}
```

A fingerprint table compiled into a binary can only recognise the releases that build was packed against, so a contract from a **later** release is a table miss and so is a modified copy. This maps a masked code hash to the release that produced it, so a buyer's agent whose table misses can ask the chain instead of refusing. The hash *is* the version; `version` is a label nothing branches on.

Three properties carry the design:

- **Append-only.** No removal, no overwrite, no un-deprecate, no proxy. A compromised owner key can only *add*, and every addition is a permanent public `Published` event.
- **`Deprecated` never invalidates.** It means "not recommended for new purchases"; a held token and its validation are untouched, and nothing on any launch path reads this contract. Anything else would be the revocation surface the ownership invariants rule out, reached through a different door.
- **Its own code is verified before its answer is believed.** One address and one masked hash, both frozen into the wrapper at pack time; the deployer is trusted for nothing. The registry publishes the distinct immutable layouts in use so an agent can compute a hash before it has a record to look one up in - four across today's canonical set, one each for `Rub3Access`, `Rub3Factory` and `Rub3Registry` plus the empty one `Rub3AccessDeployer` and `Rub3CodeRegistry` itself share. **The wrapper reads that list bounded, from the newest end, and tries a bounded number of it**, since how many tables exist is the owner key's to choose while the read and each surviving candidate's `record` call sit on the path that spends money: it asks `latestOffsetTables(MAX_CANDIDATE_OFFSET_TABLES)` and holds the same cap over the lookups, because a node need not honour what it was asked for. So neither what a compromised owner key can put on the wire nor the round trips a purchase makes grows with what that key publishes. The newest end because this is reached only on a pinned-table miss, which is by definition a question about code newer than the binary asking: a budget spent oldest-first would make the seventeenth layout ever interned the point where fielded binaries stopped recognising new releases while the first ones stayed readable forever. Past the bound a candidate is unread or untried, which says what a dropped one says. Reachability and latency only - neither the size nor the end of this read could ever produce a wrong verdict.

An agent checks a registry-supplied offset table against the code it fetched before masking with it - each range one 32-byte word, sorted, disjoint, in bounds, and preceded by a `PUSH32` opcode - which bounds the blind spot a masked hash accepts. It does not close it the way the wrapper's own pinned table does: those ranges arrive with the binary from solc, where a masked byte provably cannot execute, while a one-byte lookback cannot prove the `PUSH32` is an instruction rather than data inside an earlier push's immediate. Shaping code and table together to exploit that needs the registry's owner key, which could more simply publish an empty table; the bound on that key is append-only publication and a permanent public event per addition, not this check.

Nothing is deployed: `code_registry` is `null` for every chain in `contracts/deployments.json`, so every wrapper refuses on a table miss exactly as it did before this contract existed.

#### Rub3Registry *(implementation.md §3.2)*

Discovery and verification, **never validity** - delisting removes the badge and the listing; it cannot invalidate a token or a session. **This is not `Rub3CodeRegistry`**: that one answers "is this bytecode a genuine rub3 release", keyed by masked code hash, and is read on the purchase path; this one answers "which apps exist and which are listable", keyed by licence contract address, and is read by a shopper before it has an address to verify. Neither is evidence for the other's question.

```solidity
contract Rub3Registry is Ownable2Step {   // no proxy, no call that is not a STATICCALL
    address public immutable factory;     // from contracts/deployments.json, per chain id

    // Listable iff a canonical factory deployed it and the caller owns it, both read live.
    function register(address license, string calldata appName, string calldata contentURI) external;
    function isCanonicalDeploy(address license) external view returns (bool);   // walks previousFactory

    // Discovery only, all four. Nothing here reaches a token, a session, or a price.
    function delist(address license) external;                  // the licence contract's owner
    function relist(address license) external;                  // the licence contract's owner
    function suspend(address license, string calldata reason) external onlyOwner;
    function reinstate(address license) external onlyOwner;

    // The judgement the licence contracts deliberately do not hold.
    function setTokenRecognised(address token, bool recognised) external onlyOwner;
    function isRecognisedToken(address token) external view returns (bool);   // address(0) always true

    // Ranked recognised-rail-first, each group in registration order, priceToken read live.
    // These scan every registered entry; a page of the global ranking still pays for the scan.
    function rankedListings() external view returns (address[] memory);
    function rankedListingWindow(uint256 start, uint256 count) external view returns (address[] memory);
    function cards(uint256 start, uint256 count) external view returns (AgentCard[] memory);

    // Bounded: a cursor over registration order, ranked within its own window only.
    function registeredWindow(uint256 start, uint256 count) external view returns (address[] memory);
    function rankedRegistrationWindow(uint256 start, uint256 count) external view returns (address[] memory);
    function cardWindow(uint256 start, uint256 count) external view returns (AgentCard[] memory);

    function card(address license) external view returns (AgentCard memory);  // newest 32 hashes + true total
}
```

**Only factory deploys are listable, and only by the contract's own owner.** `isCanonicalDeploy` walks `previousFactory` for up to eight further generations, so an older generation's deploys stay listable when rub3 ships a new factory; a directly deployed licence is good software that is simply not listable, which is the trade the fee-free path makes. Ownership is read live, so transferring the licence contract transfers control of its listing with nothing to update in the registry.

Each entry doubles as an ERC-8004-style agent card - contract address, both price rails, payment methods, content URI, wrapper hash set with each hash's status, identity model, and the frozen fee terms - so agent purchasing policies can allowlist "verified rub3 contracts" and machine-audit the invariants above before buying. Everything on a card except `appName` and `contentURI` is read off the licence contract at call time, so a card cannot describe terms the contract no longer offers.

**Ranking, and why the quote is read live.** The protocol fee accrues in whatever asset a contract lists as its `priceToken`, and the licence contracts hold no policy about which assets count - see [Why the fee split is shaped this way](#why-the-fee-split-is-shaped-this-way). The registry is where that judgement lives: entries quoting a recognised token rank above entries that do not, in a stable partition. The native rail is always recognised and cannot be un-recognised, because an ETH-only contract quotes no token at all and its fee accrues in ETH; the only entries that rank below are those quoting a token rail in an unrecognised asset. The list is registry-maintained rather than baked into a licence contract so it can move as tokens do without touching anything deployed.

`setTokenPrice(address,uint256)` stays owner-callable on a licence contract for its whole life, so **the rank reads `priceToken` live on every call rather than trusting a snapshot taken at registration**. A frozen snapshot would keep advertising a contract on a quote it no longer honours, with no event from the registry to say so. An off-chain indexer has the equivalent: re-validate on the licence contract's `TokenPriceUpdated`. Demotion is discovery like everything else here - an entry that drops to the bottom has lost placement and nothing else.

**Every whole-set read has a bounded counterpart, because the set only grows.** Registration is permissionless for anyone holding a factory deploy and nothing is ever removed, so a read that scans all of it is on a clock that runs out, and an immutable contract gets no second chance to add one later. `registeredWindow`, `rankedRegistrationWindow` and `cardWindow` take their cursor over registration order and scan only their slice; `card` caps the wrapper hash set at the newest 32 and reports the true total beside it, so no listing owner's publishing history decides what a page containing them costs. What a bounded page gives up is stated on the function: **it is ranked within its window, not globally**, and paging through does not reconstruct `rankedListings`. `rankedListingWindow` and `cards` are the pair that look bounded and are not - they index into the global ranking, so producing a page means computing that ranking first - and each says so in its own NatSpec rather than leaving it to be found from a gas limit.

**What the registry owner can and cannot do.** It can maintain the recognised-token list, and it can withhold the badge from a listing with a logged reason and give it back. That bounds a compromise of the owner key to "the discovery surface is wrong until it is fixed": there is no state in this contract whose worst case reaches a token, a session, or a price. The bytecode row above is what proves it rather than this paragraph.

---

### 2. rub3 Wrapper Runtime

```text
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
│   ├── If none owned: attest the contract's code, THEN present the purchase
│   │   screen - a refusal replaces it, never accompanies it (§2.8)
│   ├── Present token selector UI (skip if single token)
│   ├── On token selected: run ownerOf() confirmation
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
│   └── SDK channel (feature `sdk`) - answers the child's heartbeat and
│       session questions; see "Components -> rub3 SDK" for what that proves
│
└── App Host
    ├── Rust binary mode: exec embedded binary
    └── Tauri mode: launch Tauri app entry point
```

**Headless mode (built - implementation.md §2.1).** Everything in the tree above except the Wallet Connection webview is signer-agnostic. `activation::ensure_headless(signer, ctx)` runs the same pipeline - enumerate tokens, purchase if empty, cooldown check, activate, sign session, persist - with an operator-supplied signer (env key, keystore, or KMS-backed `Signer` impl). The purchase step opens by attesting the contract's deployed code against the canonical fingerprints compiled into the binary, refusing on a miss with exit code 23 before anything is signed (implementation.md §2.6; it gates purchases only, never launches). It then reads `priceToken()` and pays on the stablecoin rail when five things hold, checked in this order: the contract advertises one, the wallet holds enough of it, the payment token's EIP-712 domain is readable, the operator's spend ceiling (`RUB3_AGENT_MAX_TOKEN_AMOUNT`, in the token's own smallest unit) covers the listed amount, and only then, once a short-lived authorization has been signed, an `eth_call` pre-flight of the `purchaseWithAuthorization` transaction succeeds (which is what catches a payment token lacking the `bytes signature` overload the contracts call); the copy that is broadcast is signed after that, with a window long enough to be mined (implementation.md §2.2). ETH otherwise. The ceiling has no default and must be set before the rail is usable, because the unit belongs to whichever token the contract lists and decimals differ between them. It sits ahead of the signing on purpose: an authorization is spendable by anyone who sees it, so a ceiling that refuses only after one exists has already let the money go. A listed amount above a set ceiling is a refusal with its own exit code (22) rather than a quiet switch of rail, while an agent that holds none of the token, or faces a token with no readable domain or no ceiling configured, is simply on the ETH path it was always on and its printed reason says so without mentioning a spend limit. The cost of signing last is that a token lacking the overload *and* priced above the ceiling reports the refusal rather than the fallback; priced within the ceiling it still falls back to ETH, and exit 22 never claims the rail was healthy. A contract-level failure on a token-side read falls back to ETH with a printed cause, a transport failure stops the run - a blinking node must never silently change the currency. The ETH path it falls back to is itself bounded, by `RUB3_AGENT_MAX_ETH_WEI` in wei, checked between reading `price()` and sending the transaction so a refusal costs no gas; that ceiling has a default (0.1 ETH) because wei is a fixed unit, so the fallback is never an escape from policy. The rail it chose is reported back in `HeadlessOutcome::PurchasedAndActivated { paid }` and printed on the CLI's one-line result. Front doors are Cargo features: `webview` pulls `wry`/`tao`, `headless` pulls neither, so a headless build has no GUI dependency at all - smaller binary, container-friendly. This is the primary path for agent-operated software; the webview is the human fallback. The webview runs the same code attestation before it will show a purchase screen at all, and differs only in what it does with a refusal: a person is shown what was found and what to do about it, where an orchestrator gets exit 23 (§2.8).

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
| `rub3` | The SDK crate's wire types, so both halves of the channel share one definition (feature `sdk`) |
| `windows-sys` | `CreateNamedPipeW` and friends for the channel's named-pipe server (feature `sdk`, Windows only; the client side needs no dependency) |

---

### 3. rub3 SDK (Rust Crate) *(built - implementation.md §3.5)*

The crate a wrapped app links: `rub3::heartbeat()` and `rub3::session()`, talking to the wrapper over a per-launch local endpoint whose address arrives in `RUB3_SDK_SOCKET`. The wrapper's half is `sdk.rs`, behind the `sdk` feature, which no tier bundle enables - what an application may ask about itself is independent of how hard the launch was gated.

**Application code keys persistent data on `SessionInfo::user_id`** (the TBA under the account model, the wallet under the access model), never on the current signing wallet, so a transfer or a wallet rotation does not orphan the user's data. That is enforced by the types rather than by this paragraph: `UserId` implements `Hash`, `Eq`, `Ord` and `Display`, and `Wallet` implements only `Display`, so keying anything on the wallet does not compile.

**What `heartbeat()` proves: an honest integration and a live wrapper. That is the whole claim.** Licensing is enforced before the wrapper launches anything, so by the time the endpoint exists the gate has already run - a live heartbeat says a wrapper is there and answering, not that anything it decided was correct. It re-verifies no signature, reads no chain and consults no contract.

**What it does not prove.** It is not a defence against a determined local attacker, and no local IPC could be. Anyone able to run the wrapped binary outside its wrapper controls the machine: they can publish a socket of their own and answer every request however they like, and any credential the SDK could demand would ship inside the binary they already control. The channel therefore carries no authentication, deliberately. Nor is it tamper evidence - a patched application simply does not call it. What it catches is the ordinary set: an application run directly, a wrapper that died mid-run, a stale address, a version-skewed pair.

**Exactly six fields cross it** - `app_id`, `token_id`, `user_id`, `wallet`, `identity`, `expires_at`. The session signature, its nonce, the activation transaction and the device key stay on the wrapper's side: an application that could read the signature could replay the session somewhere the wrapper never launched it.

**It never gates a launch.** A channel that fails to start is a warning on stderr and the binary runs anyway; refusing to start a paid-for program because a socket could not be created would be a revocation surface (see "Ownership invariants"). The child is still told a wrapper launched it - `RUB3_SDK_SOCKET` carries a sentinel in place of an address, in that case and in a build without the `sdk` feature - so the application reports a wrapper serving no channel rather than no wrapper at all. Whether the application requires the channel is the developer's call, made by calling `heartbeat()` or not.

---

### 4. rub3 CLI *(`pack` and `deploy` built - implementation.md §2.5)*

`pack` and `deploy`. `fetch` and `register` are deliberately unbuilt: they are the agent-side halves of content-addressed distribution (§3.1) and the discovery registry (§3.2), and neither exists yet. The split that matters architecturally: `--tier` is a *pack*-time choice, baked into the wrapper binary, while `--identity` and `--cooldown-blocks` are *deploy*-time choices written into the contract. The two cannot be reconciled afterwards, which is why a repacked wrapper must still match a deployed contract's tier expectations. The command surface is in implementation.md §2.5.

---

### 5. Tauri Plugin *(not started - implementation.md §5.3)*

`tauri-plugin-rub3` would expose the same `SessionInfo` to a Tauri frontend over `invoke`, and render token selection and renewal in the app's own webview rather than the wrapper's. Proposed surface in implementation.md §5.3.

---

## ENS Trust Layer

### How it works

```text
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

Old binaries stay verifiable; a compromised release is flagged with a reason. Revoking a **binary hash** never touches **token validity** - the holder downloads a patched build and the same license just works. This is structural, not a promise: `ownerOf`, `honorsContract` and `activate` never read `wrapperHashes`, and `contracts/test/Rub3Invariants.t.sol` revokes every hash in the set and asserts all three are unaffected.

Honest limit: the hash set informs new downloads and activations; it cannot retroactively disable compromised binaries already running. A kill switch that could would be a revocation mechanism, and it must not exist.

Planned alongside it: `contentURI` (IPFS/Arweave) recorded on the contract, making it a complete distribution record - `rub3 fetch <contract>` downloads the binary and verifies its hash on-chain (implementation.md §3.1).

Trust chain: **ENS → contract → content URI → binary hash → running wrapper**

### Binary encryption (tiers 3-4, optional) *(deferred)*

Would ship the embedded app binary as ciphertext the wrapper decrypts into memory only after the session check passes. Not built; see "Deferred designs" below.

---

## Launch Flow

### Tier 0-2: signature/ownership verification

```text
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
                                    ownerOf() confirmation
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

```text
    ... (same as above through token selection) ...
                                            │
                                    ownerOf() confirmation
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

```text
While running:
  App ──"are you there?"──▶ Wrapper       (rub3::heartbeat(), when the app asks)
  App ──"who is running me?"──▶ Wrapper   (rub3::session())
  App panics if the wrapper does not answer
  Wrapper exits when the app exits
```

The application asks; the wrapper does not push. A pushed heartbeat would need a
policy for what the wrapper does when the application stops answering, and the
only honest answers are "nothing" or "kill a paid-for launch" - the second is a
revocation surface. So the wrapper answers, and the application decides whether
it requires an answer. Available in a build with the `sdk` feature; see
"Components → rub3 SDK" for what a heartbeat does and does not prove.

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

```text
Attestation:   masked code hash vs fingerprints pinned in the binary, then vs the append-only
               Rub3CodeRegistry on a miss (pre-purchase, both front doors: refuses to buy from
               non-canonical code, never blocks a launch)
Distribution:  on-chain binary hash (verify download)
Encryption:    AES-256-GCM binary encryption (tiers 3-4: binary useless without valid session)
Identity:      ENS resolution (verify developer identity)
Payment:       wallet transaction approval
Session:       SIWE-style signature (proves ownership at creation)
Cooldown:      on-chain rate limit (tiers 3-4: prevents mass session distribution)
Revocation:    on-chain session counter (tiers 3-4: new session kills old)
Device:        ephemeral keypair bound to hardware (tier 4: non-transferable)
Enforcement:   session TTL (tiers 1-3) or device challenge (tier 4)
Runtime:       heartbeat IPC (catches a direct launch or a dead wrapper; proves nothing against a local attacker - see "Components -> rub3 SDK")
```

---

## Scaling Considerations

- Contract deployment: one per app, ~$1–5 on Base (via `Rub3Factory` - deploys are never charged by rub3; the factory deploy itself is a one-off that rub3 pays, not the developer)
- Protocol fee: a 2–3% split executed inside `purchase()` on factory deploys, on both payment rails - no additional infrastructure; settlement is continuous and on-chain, with each side sweeping its own balance
- RPC read calls: varies by tier. Tier 0: zero. Tiers 1-2: one per renewal. Tiers 3-4: one per launch (`activeSessionId` + `ownerOf`). Public RPC or Alchemy free tier sufficient.
- RPC write calls: tiers 3-4 only. One `activate()`/`activateDevice()` tx per session creation. ~$0.001 on Base.
- Session files: ~500 bytes each, one per token per device. Negligible storage.
- Device keys (tier 4): one per token per device. Stored in OS keychain or Secure Enclave - no additional disk storage.
- No backend. No database. No auth service. All verification is either local crypto or on-chain reads.
