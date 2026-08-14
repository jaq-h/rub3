# rub3 — Implementation Plan

> **Plan revision — July 2026 (agent-first reorientation).**
> Everything below Phase 1 has been resequenced around a single thesis: agents will do an increasing share of software development, deployment, and purchasing, and they need to buy, verify, run, and resell locally executed software — low cost, high speed, secure payments — with no human in the loop.
>
> What changes:
> - **Headless is the front door.** All session crypto is already native Rust; the webview exists only because humans keep keys in wallet apps. Signer-in/session-out activation (§2.1) becomes the primary mode; the webview is the human fallback floor.
> - **Machine money.** USDC purchases via EIP-3009 signed authorizations join ETH pricing as the default path (§2.2).
> - **Revenue at the network layers.** The rails (wrapper, SDK, CLI, contracts) stay open source and free. Revenue is an immutable 2–3% fee stamped by `Rub3Factory` (§2.3), metered per-launch billing only the wrapper can enforce (§4.1), and — once volume shows — a registry-filtered resale marketplace (§4.3).
> - **The token is the invariant.** No proxies, no revocation surface, no pause on validation. Evolution only changes what is offered going forward, never what was granted (§2.4).
> - **Distribution completes the loop.** `contentURI` on-chain + `rub3 fetch` + hash verification = discover → pay → fetch → verify → run (§3.1).
> - **Seats, not devices.** Fleet concurrency is licensed as K on-chain seats per token (§3.4). Tier-4 device binding and binary encryption move to Deferred.
> - **Human UX polish is demoted, not dropped.** WalletConnect tabs (§1.10), the Preact refactor, and the Tauri plugin move to Phase 5.
>
> Phase 1 sections are preserved unchanged below as the build record of what exists today.

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
- **Not yet done:** WalletConnect integration — tracked as §1.10b (requires WC v2 JS SDK + developer-supplied project ID)

### 1.4 — On-chain queries `[complete]`
- `rpc.rs`: `ownerOf(tokenId)`, `price()`, `balanceOf(owner)`, `tokenOfOwnerByIndex(owner, index)` via alloy JSON-RPC with minimal ABI (`IRub3License`)
- `tokens_of_owner(rpc_url, contract, owner)` enumerates all tokens held by a wallet via ERC-721Enumerable
- Synchronous wrapper over async alloy calls (`block_on` with single-threaded tokio runtime)
- Ownership check wired into webview `Connect` handler: 0 tokens → error, 1 → auto-proceed to activate, N → token-select screen
- ENS resolution remains a stub (`EnsNotSupported`) — deferred to §1.6 where it is the primary deliverable

### 1.5 — Smart contracts `[scaffolded]`

Branch: `feature/smart-contract`. Foundry project under `contracts/` with OpenZeppelin v5.1.0 and forge-std installed as submodules under `contracts/lib/`.

**Abstract base — `Rub3License.sol`**
- Inherits `ERC721`, `ERC721Enumerable`, `Ownable` (OZ v5)
- Immutable: `identityModel` (0 = access, 1 = account; rejects values > 1), `supplyCap` (0 = uncapped), `cooldownBlocks` (floor `MIN_COOLDOWN_BLOCKS = 15` ≈ 30s on Base)
- Mutable + owner-gated: `price` (`setPrice`), `wrapperHash` (`setWrapperHash`) — hash is rotatable so developers can rebuild the wrapper without redeploying
- `nextTokenId` counter + internal `_mintNext` helper for sequential ids from 0
- `_resolveRecipient(address)` helper: `address(0)` → `msg.sender` (per architecture.md §1)
- `withdraw(address payable)` owner-only sweep
- `_update` / `_increaseBalance` / `supportsInterface` overrides for ERC-721 + Enumerable composition
- **Activation (tier 3)**: `activate(uint256) returns (sessionId)` — owner-only, bumps `activeSessionId[tokenId]` from a monotonic `_sessionCounter`, records `lastActivationBlock`, reverts `CooldownActive(blocksRemaining)` if called again inside the window (first call, `last == 0`, bypasses); `cooldownReady(tokenId) view returns (bool, uint256)` for the wrapper's pre-tx check; `Activated(tokenId, owner, sessionId)` event

**`Rub3Access.sol`** — concrete, one-time purchase:
- `purchase(address recipient) payable returns (uint256 tokenId)` — pays `price`, mints next id
- `Purchased(tokenId, recipient, payer)` event

**`Rub3Subscription.sol`** — concrete, time-bounded:
- Immutable `period`, `mapping(uint256 => uint256) expiresAt`
- `purchase(address recipient) payable` — mints + sets `expiresAt = now + period`
- `renew(uint256 tokenId) payable` — extends from current expiry if still valid, else resets to `now + period`
- `isValid(uint256 tokenId) view` — `expiresAt[tokenId] > block.timestamp`
- `Purchased` + `Renewed` events

**Tests:** 30 forge tests (`forge test`) covering metadata, sequential mint, zero-recipient default, over/underpay, supply cap, enumeration via `tokenOfOwnerByIndex`, owner-gated setters, withdraw, subscription expiry, mid-period renewal, post-expiry renewal, nonexistent-token revert, plus activation: first-call success, session-id increments across tokens, cooldown-window revert, post-cooldown success, non-owner revert, nonexistent-token revert, `cooldownReady` in all three states, constructor floor check (`cooldownBlocks < 15`), and transfer-then-activate (new owner authorized, old owner rejected).

**`script/Deploy.s.sol`** — forge script that deploys either contract from env vars:
- `CONTRACT_TYPE`, `TOKEN_NAME`, `TOKEN_SYMBOL`, `IDENTITY_MODEL`, `WRAPPER_HASH`, `PRICE` required; `SUPPLY_CAP`, `OWNER`, `COOLDOWN_BLOCKS` (default 1800 ≈ 1hr on Base), `PERIOD` optional
- Dry run (no `--broadcast`): simulates deployment, prints summary with all params
- Live: add `--broadcast --verify --etherscan-api-key $BASESCAN_API_KEY`
- Local: run against `anvil` with `--rpc-url http://localhost:8545` and a pre-funded Anvil key — no `.env` needed

**Not yet done:**
- Tier 4: `activateDevice(tokenId, devicePubKey)` + `registeredDevice` mapping — deferred to tier-4 work
- Base Sepolia deployment

### 1.6 — Identity model + TBA derivation `[complete]`

**Contract change** — `Rub3License.sol` gains `address public immutable tbaImplementation`. Constructor now validates that account-model deploys supply a non-zero impl and access-model deploys supply `address(0)` (new errors `TbaImplementationRequired` / `TbaImplementationForbidden`). Threaded through `Rub3Access` + `Rub3Subscription` constructors, the `Deploy.s.sol` script (new `TBA_IMPLEMENTATION` env var), and the Foundry test fixtures. Forge test suite: 33 pass, up from 29 (4 new tests covering the two new reverts plus the happy-path account-model construction).

**Wrapper changes**
- `identity.rs` (new, gated on `session`) — `IdentityModel { Access, Account }` with `from_u8` / `as_str`; `derive_tba(implementation, chain_id, contract, token_id)` computes the ERC-6551 TBA via CREATE2 against canonical registry `0x000000006551c19487814612e58FE06813775758` with `salt = 0` and the reference account-proxy init bytecode (pure, no RPC); `resolve_user_id(model, wallet, tba)` returns lower-case 0x-hex; `format_addr(addr)` helper
- `rpc.rs` — `IRub3License` gains `identityModel() -> uint8` + `tbaImplementation() -> address` getters; new `identity_model()` and `tba_implementation()` pub fns
- `session.rs` — `Session` gains `identity: String`, `user_id: String`, `tba: Option<String>`; `session_message()` adds `identity` + `user_id` into the preimage (between `wallet` and the existing fields) so a forger cannot flip an access-model session into account-model without re-signing. Ordering: `app_id, token_id, identity, user_id, wallet, nonce, [expires_at], [activation_block_hash], [session_id], [device_pubkey]`
- `webview.rs::spawn_tx_poller` — after the existing `active_session_id` read, calls `identity_model()`; for account model also calls `tba_implementation()` and derives the TBA locally. Includes the resolved `identity`, `user_id`, and optional `tba` in the signed preimage + `onTxConfirmed` payload. `IpcMessage::SessionSigned` / `FinalizeArgs` carry the three identity fields through back to the final `Session`
- `activation.html` — sign-session screen shows the identity model label, user_id, and (for account model) TBA address. Echoes all three back in the `session_signed` IPC message

**Tests**
- `identity.rs`: 11 tests — `IdentityModel` from_u8 / as_str / rejects-out-of-range; TBA determinism + sensitivity to each of `{implementation, chain_id, contract, token_id}`; `resolve_user_id` for both models + panic on missing TBA
- `session.rs`: 2 new preimage tests — differs by identity (access → account), differs by user_id alone; 1 new verify test — tampered identity fails `verify_local` with `AddressMismatch`; all existing tests updated to the new 10-arg `session_message()` signature
- `rpc.rs`: 2 new transport-error tests for `identity_model()` + `tba_implementation()`
- `tests/session_onchain_e2e.rs`: updated `forge create` to pass the new `tbaImplementation = address(0)` arg; `Session` struct literal updated. Passes against anvil.

**Verification**
- `cargo test -p rub3-wrapper --lib` (default tier-2): 51 pass (up from 35)
- `cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib`: 55 pass (up from 39)
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean
- `forge test` (contracts/): 33 pass
- Anvil-gated e2e (`session_verify_onchain_e2e`): passes with the new 9-arg constructor

### 1.7 — Purchase UI `[complete]`

In-wrapper purchase flow when the connected wallet owns no token. Gated on
`onchain-write` (tier 3+). Wrapper never holds keys — it encodes calldata,
surfaces it to the user, and polls the receipt they paste back.

**RPC additions (`rpc.rs`)**
- `supplyCap()`, `nextTokenId()`, `purchase(address)` added to the `sol!` interface
- `supply_cap()` / `next_token_id()` public readers
- `encode_purchase_calldata(recipient)` — pure, `SolCall::abi_encode` over `purchase(address)`
- `mint_token_id(rpc_url, tx_hash, contract, recipient)` — fetches the receipt and walks `receipt.inner.logs()` for the ERC-721 `Transfer(0x0, recipient, tokenId)` log (topic0 = `0xddf252ad…`), returning the minted id. Constant `ERC721_TRANSFER_SIG` for comparison
- `pub mod rpc` (was private) so integration tests can drive these directly

**Webview wiring (`webview.rs`)**
- New IPC variant `PurchaseTxSent { tx_hash, owner_address }` gated on `onchain-write`
- `Connect` handler's empty-tokens branch now calls `show_purchase` under `onchain-write`; tier 0-2 still surface the legacy "no token" error
- `show_purchase` reads `supplyCap` / `nextTokenId` / `price`, rejects sold-out state, encodes calldata, emits `onShowPurchase({ ownerAddress, contractAddress, chainId, priceWei, valueHex, supplyCap, nextTokenId, calldata })`. Price is serialised as a decimal string + hex string so a full uint256 price survives JSON
- `spawn_purchase_poller` mirrors `spawn_tx_poller`: polls receipt (30s / 10 × 3s), asserts `status == true` and `receipt.to == contract`, then calls `mint_token_id` to recover the id and re-enters `proceed_after_token_selected` — the downstream cooldown/activate flow is reused verbatim

**HTML (`assets/activation.html`)**
- New `#screen-purchase` with price (ETH + wei), supply counter, recipient, send-to / value / calldata boxes, tx-hash input
- `onShowPurchase` callback populates the screen, stores `pendingPurchaseCtx.ownerAddress`
- `formatEth(weiStr)` — BigInt-based wei→ETH formatter with up to 4 fractional digits; 0 renders as "Free"
- `'purchase'` added to the `SCREENS` array so `show('purchase')` hides the others

**Tests**
- 6 new `rpc` unit tests: purchase calldata selector (`0x25b31a97`) + recipient layout + differs-by-recipient; `supply_cap`, `next_token_id`, `mint_token_id` (both bad-URL and bad-hash) transport-error paths
- Anvil e2e (`tests/session_onchain_e2e.rs`) extended with `supply_cap`/`next_token_id` pre- and post-purchase checks and a `mint_token_id` parse against the real `purchase()` receipt — all four assertions pass against a live Rub3Access on anvil

**Deferred**
- Refactor `activation.html` to Preact (vendored `preact.mjs` + `htm.mjs`, custom-protocol handler via `include_dir` — no Node/build step). Tracked as §5.2.
- Replace the "paste your tx hash" box with auto-detect + WalletConnect tabs while keeping manual paste as the fallback floor. Tracked as §1.10.

**Verification**
- `cargo test -p rub3-wrapper --lib` (default tier-2): 57 pass (up from 51)
- `cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib`: 61 pass (up from 55)
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean
- `forge test` (contracts/): 33 pass
- Anvil-gated e2e (`session_verify_onchain_e2e`): passes with the new purchase-path assertions

### 1.8 — On-chain cooldown + session model (tier 3) `[partial]`

Replaces the legacy `LicenseProof` flow with a full session model backed by an on-chain cooldown. An NFT holder can otherwise run a signing oracle to distribute fresh sessions to non-holders; a contract-enforced `activate()` cooldown rate-limits how many sessions a single token can mint. The wrapper reads cooldown state and encodes calldata — it never sends txs or holds keys.

**Contract interface** (now live in `Rub3License.sol`, see §1.5):
```solidity
uint256 public constant MIN_COOLDOWN_BLOCKS = 15; // ~30s on Base; minimum is one TOTP window
uint256 public immutable cooldownBlocks;           // default 1800 (~1hr); must be >= MIN_COOLDOWN_BLOCKS

mapping(uint256 => uint256) public lastActivationBlock;

function activate(uint256 tokenId) external {
    require(ownerOf(tokenId) == msg.sender, "not owner");
    uint256 last = lastActivationBlock[tokenId];
    if (last != 0) require(block.number - last >= cooldownBlocks, "cooldown");
    lastActivationBlock[tokenId] = block.number;
    emit Activated(tokenId, msg.sender, block.number);
}

function cooldownReady(uint256 tokenId)
    external view returns (bool ready, uint256 blocksRemaining) { ... }
```

**Phase A — foundation modules `[complete]`**
- `session.rs` — `Session` schema; `session_message()` (SHA-256 over tier-appropriate field set, BE integers, optional fields omitted when `None`); `new_nonce()` (32-byte random hex); `verify_local()` (reconstruct message → `personal_sign` recover → compare to `session.wallet` → expiry check); `is_expired()` (RFC3339 parse vs `Utc::now()`; `None` → false for tier 4)
- `session_store.rs` — `session_path()` (`RUB3_SESSION_DIR` override or `~/.rub3/sessions/<app_id>/<token_id>.json`); `load_session()` / `save_session()`; `load_latest_session()` scans app_id dir, filters expired + invalid-signature sessions, returns most-recently-issued valid one
- `personal_sign_hash`, `recover_address`, `public_key_to_address` promoted to `pub(crate)` in `license.rs`
- 15 tests: message determinism + tier diffing, expiry edge cases (future/past/None/unparseable), sign/verify round-trip, wrong-wallet failure, save/load round-trip, load_latest with mixed valid/expired sessions

**Phase B — RPC + IPC wiring `[complete]`**
- `rpc.rs` additions: `cooldown_ready` → `(is_ready, blocks_remaining)`, `last_activation_block`, `cooldown_blocks`, `active_session_id` (post-tx revocation read), `encode_activate_calldata` (pure, `SolCall::abi_encode`), `get_tx_receipt` → `TxReceipt { status, block_number, block_hash, to }`, `get_block_number`
- `webview.rs` new IPC variants (gated on `cooldown` feature): `ActivateTxSent { tx_hash, token_id, owner_address }`, `SessionSigned { signature, ... }` — JS echoes back all state needed to assemble the `Session`, so the Rust handler is stateless across messages. Outbound JS: `onShowCooldown`, `onTxConfirmed`, `onProcessing`, `onError`. Legacy `Signed` path kept for zero-contract fallback.
- `ActivationResult` gains `SessionSuccess { session }` variant (gated); `LegacySuccess { proof }` replaces the old plain `Success`
- Connect handler branches: zero contract → legacy `show_activate`. Non-zero + `cooldown` → `tokens_of_owner` → `proceed_after_token_selected` → `cooldown_ready` + `encode_activate_calldata` → `onShowCooldown`
- ActivateTxSent handler: spawns a background polling thread (10 × 3s; 30s total timeout) calling `get_tx_receipt`; on confirmation asserts `receipt.to == contract` and `status == true`, reads `activeSessionId`, mints a `new_nonce()`, computes `expires_at` from `SESSION_TTL_SECS`, builds the session message, and emits `onTxConfirmed`
- SessionSigned handler: assembles `Session` (tier-3 fields populated from echoed state), calls `verify_local`, sends `ActivationResult::SessionSuccess`
- `activation.rs::ensure` — tries three paths in order: (1) tier-3 session fast path (`load_latest_session` → `verify_local`), (2) legacy proof fast path, (3) webview. Takes a new `session_ttl_secs` param threaded through from `main.rs` (`SESSION_TTL_SECS = 7 days`). On `SessionSuccess` persists via `session_store::save_session`.
- `assets/activation.html` new screens: `cooldown` (shows calldata + tx-hash input with per-block-remaining banner when cooldown is active), `sign-session` (shows tx hash / block / session id / session message, captures signature). JS tracks `pendingSessionCtx` across the cooldown → tx-confirm → sign-session flow and echoes it back in `session_signed`. The tx-hash input is the "manual paste" path today; the richer auto-detect and WalletConnect tabs layered on top are tracked as §1.10.

**Phase C — verification hardening `[complete]`**
- `session::verify_onchain(session, rpc_url)` (gated on `cooldown`) — fetches the activation tx receipt and confirms `status == true`, `receipt.to` matches `session.contract`, `receipt.block_hash` matches `session.activation_block_hash`. Each failure mode has a dedicated `VerifyError` variant (`MissingTxHash`, `MissingBlockHash`, `Rpc`, `ReceiptNotFound`, `TxReverted`, `ContractMismatch`, `BlockHashMismatch`)
- `session::should_reverify()` — Bernoulli gate (`rand::thread_rng().gen_range(0..5) == 0`) amortising the re-verify cost across cold starts
- `activation.rs::try_session_fast_path` now re-verifies tier-3 sessions (session_id present) on ~1 in 5 launches. `Rpc(_)` errors fall open (offline launches still work); verdict-contradicting errors fall closed (forged session → re-activate)
- Tx polling (already in Phase B): 30s total (10 × 3s), revert → user-facing error via the existing `onError` IPC path

**Verification**
- `cargo test` — 35 lib tests pass under default (tier-2); 39 pass under `--no-default-features --features tier-3` (adds 4 new tests: missing tx-hash, missing block-hash, bad-RPC transport, non-constant sampler); integration + license-e2e suites unchanged
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean
- Phase B `rpc` additions covered by pure tests: selector + calldata layout for `encode_activate_calldata(uint256)`, invalid-hash transport errors for `get_tx_receipt` and `get_block_number`
- Phase C anvil-gated integration test (`tests/session_onchain_e2e.rs`, `#[ignore]`): spawns `anvil`, deploys `Rub3Access` via `forge create`, runs `purchase(address)` + `activate(uint256)` via `cast send`, extracts the real block hash, and exercises `verify_onchain` on (a) the happy path, (b) a tampered contract field, (c) a tampered block hash, and (d) a non-existent tx hash. Gracefully skips when the Foundry toolchain is unavailable. Run with `cargo test -p rub3-wrapper --no-default-features --features tier-3 -- --ignored session_verify_onchain_e2e`
- Still to do separately from Phase C: end-to-end against anvil of the full connect → tx → sign → persistence-across-restarts webview flow (that belongs in §1.7's manual testing), cooldown enforcement path, short-TTL expiry re-activation, zero-contract legacy backward-compat test

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

> **Amended by §2.1:** tier bundles are pure capability sets and no longer imply
> a front door. Compose one with `webview` (human) and/or `headless` (agent).
> The default build is `tier-2` + `webview`, unchanged in behaviour.

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

### 1.10 — Frictionless tx confirmation `[not started — demoted to Phase 5]`

> **Plan revision:** this work now lands as §5.1, after the agent-native core. The manual-paste floor already works and stays reachable forever; richer confirmation modes are human-surface polish. The specs below (§1.10, §1.10a, §1.10b) apply unchanged when picked up.

The purchase (§1.7) and activate (§1.8) flows currently ask the user to paste a transaction hash back into the webview after sending from their wallet. That manual-paste path is our robust fallback — it works with any wallet / any tool / any chain, requires no JS dependencies, and has no external points of failure. But it is not the UX we want people to see first. This section layers two richer confirmation modes on top, while leaving manual paste as the always-available floor.

**Three modes, in order of preference:**

| Mode | Project ID | JS bundle | Offline tolerant | Relies on |
|---|---|---|---|---|
| `wallet-connect` | required (dev-supplied) | ~255 KB vendored | no | Reown relay + chain RPC |
| `auto-detect` | none | none | no | chain RPC only |
| `manual` (§1.7, §1.8) | none | none | yes (paste later) | user copy/paste |

The three modes surface as three tabs on the cooldown / purchase screens. The default tab at render time is the highest-capability one available for the current build:
- WalletConnect tab visible when the `wallet-connect` feature is compiled in **and** the developer supplied a non-placeholder `wc_project_id`
- Auto-detect tab visible when `onchain-write` is on (always true for tier 3+, which is the only tier that reaches these screens)
- Manual tab always visible

Each tab drives the same two outbound IPC events (`purchase_tx_sent` / `activate_tx_sent`) — the downstream poller/finalize path from §1.7 and §1.8 Phase B is untouched. This keeps auto-detect and WalletConnect as pure front-door improvements rather than new branches in the session pipeline.

### 1.10a — RPC auto-detect `[not started]`

**Rationale.** Many embedded-app developers will never configure WalletConnect — they may not want the relay dependency, may not want to register with Reown, or may be shipping internal / CLI-adjacent tools. Auto-detect gives those deployments a one-click confirm path without adding any JS or external service.

**How it works.**
- Purchase: poll `eth_getLogs` for the ERC-721 `Transfer(0x0, wallet, *)` topic signature (already constant in `rpc.rs` as `ERC721_TRANSFER_SIG`) filtered by `address == contract`, starting from the block the user opened the screen. First match wins → its tx hash feeds the same `purchase_tx_sent` handler as manual.
- Activate: poll `lastActivationBlock(tokenId)` (already in `rpc.rs`); when it advances past the starting block, resolve the block's receipts and pick the one whose `to == contract && from == wallet`. That receipt's tx hash feeds `activate_tx_sent`.
- Poll cadence: 3 s, same as `spawn_tx_poller` / `spawn_purchase_poller`. Total budget configurable, default 120 s (longer than manual because the user is broadcasting the tx in-wallet during this window). Falls back to the Manual tab (pre-populated with helpful copy) on timeout or repeated RPC error.

**Rust additions (`rpc.rs`)**
- `pub fn watch_for_mint(rpc_url, contract, recipient, from_block, deadline) -> Result<String, RpcError>` — polls `eth_getLogs` with the `Transfer(0x0, recipient, *)` filter; returns the tx hash.
- `pub fn watch_for_activate(rpc_url, contract, token_id, from_block, deadline) -> Result<String, RpcError>` — polls `lastActivationBlock`; on delta, resolves the tx hash via `eth_getBlockByNumber` + receipt scan.

**Webview wiring**
- New IPC variants (gated on `onchain-write`): `AutoWatchStart { kind: "mint" | "activate", … }`, `AutoWatchCancel`. `webview.rs` spawns a `thread::spawn` running the watcher; on success the watcher routes its hash through the same internal dispatch as `purchase_tx_sent` / `activate_tx_sent` — no JS round-trip, no duplicated handlers.
- Existing purchase / cooldown / session handlers unchanged.

**HTML**
- Tabs in `#screen-purchase` and `#screen-cooldown`: `[WalletConnect] [Auto-detect] [Manual]`. The auto-detect body is a spinner + "Waiting for your wallet to broadcast the tx…" copy and a "Switch to manual" link.

**Gating.** `onchain-write` (already required by §1.7 / §1.8). No new Cargo feature. Pure additive — tier 3+ builds pick it up automatically.

### 1.10b — WalletConnect v2 `[not started]`

**Scope.** The developer opts in per deployment by supplying a `wc_project_id` (obtained from cloud.reown.com). No single rub3-wide project ID — project IDs are the abuse / rate-limit boundary, and branding (the wallet QR prompt shows the dApp name) should reflect the embedded app, not rub3.

**Rust additions**
- `ActivationContext` (the `main.rs` constants struct) gains `wc_project_id: Option<&'static str>`. Missing or placeholder → WC tab is hidden. Default in the wrapper's own dev builds is `None`, not a shared project ID — `rub3 pack` (§2.5) rejects a distributable that inherits a placeholder value.
- Feature flag `wallet-connect` on the wrapper crate — opt-in because of the vendored JS weight. Composes with `onchain-write`; does not change tier bundle definitions (developer picks `tier-3,wallet-connect` at pack time).
- `webview.rs::show_purchase` / `show_cooldown` include the project id in the `onShowPurchase` / `onShowCooldown` payload when the feature is compiled in; JS decides whether to render the tab based on its presence.

**Assets (`assets/vendor/`)**
- `walletconnect-sign-client.mjs` — Reown SignClient v2 bundle (~250 KB).
- `qrcode.mjs` — ~5 KB QR-from-URI renderer.
- Both served by the same `include_dir!` custom-protocol handler introduced in §5.2 (Preact refactor); if §5.2 has not landed yet, this section creates that handler.

**Assets (`assets/app/`)**
- New `wc.js` — init `SignClient`, open a session via `chains: ["eip155:<chain_id>"]`, render the pairing URI as an inline QR, call `client.request({ method: "eth_sendTransaction", params: [{ to, data, value }] })` to dispatch either the purchase or activate tx. Returns the tx hash through the existing `purchase_tx_sent` / `activate_tx_sent` IPC message — reusing the rest of the pipeline.

**HTML**
- WC tab body: the vendored QR canvas, a "copy pairing URI" fallback, and error copy that suggests falling back to Auto-detect or Manual.

**Gating recap.** `wallet-connect` Cargo feature + developer-supplied project id. Both must be present for the tab to render; either absent → the tab is silently omitted and the user sees a 2-tab (or 1-tab) screen.

**Phase 1 deliverable:** A wrapped binary that requires wallet ownership + session signature to run, with session caching, on-chain cooldown enforcement (tier 3), and automatic re-activation on expiry.

---

## Phase 2: Agent-Native Core

Goal: an agent holding only a funded wallet key can purchase, activate, and launch a wrapped binary in one programmatic pass — and every contract deployed from here on carries the protocol's economics and ownership invariants.

### 2.1 - Headless activation `[complete]`

The agent path: signer in, session out. No webview, no IPC round-trips, no human.
`ensure_headless` runs the whole pipeline in one call and reuses every module
below the front door unchanged - the webview really was the only human-shaped
piece.

**The design tension, and how it is resolved.** Every Phase 1 flow rests on "the
wrapper never holds keys - it encodes calldata and the user broadcasts."
Headless necessarily signs and broadcasts. The capability is contained rather
than spread: it exists only behind the `headless` feature, callers see an
object-safe `Signer` trait whose single primitive is "sign this 32-byte digest"
(so a KMS/enclave backend serves it without releasing a key), and exactly one
type in the crate - `signer::LocalSigner` - ever touches raw key material.

**Front doors are now features** - `webview` and `headless`, composable

`wry`/`tao` were unconditional dependencies, so every tier bundle dragged in the
GUI stack. They are now `optional`, pulled only by the new `webview` feature;
`webview.rs` and its call sites are gated on it. Tier bundles became pure
capability sets that name no front door, so `tier-3,headless` really excludes
the GUI rather than merely not using it.

| Feature | Pulls | Purpose |
|---|---|---|
| `webview` | `wry`, `tao` | Native activation window - the human fallback floor |
| `headless` | `session` + `onchain-read` + `onchain-write` + `cooldown`, `zeroize`, `alloy/signer-local`, `alloy/signer-keystore` | Signer-in / session-out activation |

`default = ["tier-2", "webview"]` preserves the historical default build.
`headless` composes the tier-3 capability set itself (it cannot function
without `activate()` + cooldown + purchase), so `--features headless` and
`--features tier-3,headless` are the same build.

**`signer.rs` (new, gated on `headless`)** - the one auditable place for key material
- `trait Signer: Send + Sync` - `address()`, `sign_prehash(B256)`, `source()`. Object-safe; the flow takes `&dyn Signer`. One signing primitive is the smallest operation every backend supports, so KMS/HSM/enclave impls need expose nothing else
- `personal_sign(signer, preimage)` - applies the EIP-191 prefix and emits `r || s || v` hex with `v ∈ {27,28}`, the exact shape `session::verify_local` already expects, so headless sessions verify through the same code path as webview ones
- `LocalSigner` - holds a `k256::ecdsa::SigningKey`. Constructors: `from_hex` (crate-private), `from_env` (`RUB3_AGENT_KEY`), `from_keystore` (Web3 Secret Storage V3 via `alloy/signer-keystore`)
- `resolve_signer()` - precedence is strict: `RUB3_AGENT_KEY`, then keystore (`RUB3_AGENT_KEYSTORE`, else `~/.rub3/agent-key.json` if present) with the password from `RUB3_AGENT_KEYSTORE_PASSWORD_FILE` (preferred) or `RUB3_AGENT_KEYSTORE_PASSWORD`. A **malformed** env key is a hard error, never a silent fall-through to a keystore - falling through would activate under a different identity
- Leak containment: hand-written `Debug` prints address + source and `<redacted>`; no `Display`, no `Serialize`; `SignerError` variants carry only fixed strings and at most a path - the underlying hex/keystore errors are dropped, not forwarded, because `hex::decode` names the offending character and keystore errors can distinguish a wrong password; no `unwrap`/`expect`/`panic!` anywhere on the production load path; decoded key bytes and the password buffer are zeroized after use

**`tx.rs` (new, gated on `headless`)** - calldata → broadcast, no key material
- `TxPlan { to, value, input }` and `send(rpc_url, signer, plan) -> tx_hash`: reads nonce + chain id + `estimate_eip1559_fees`, estimates gas with a 25% buffer, checks the balance covers `value + gas_limit × max_fee_per_gas`, signs the `TxEip1559` sighash via `Signer::sign_prehash`, and submits the 2718-encoded envelope with `eth_sendRawTransaction`
- `TxError::InsufficientFunds(Option<Shortfall>)` is raised both from the pre-flight balance check (which knows the numbers, and carries them) and by recognising the node's own "insufficient funds" wording (which does not, and carries `None`) - so the CLI returns the dedicated exit code either way, and the `rub3-detail:` line is omitted rather than reporting a shortfall of zero
- `Shortfall` carries a `Covers` discriminator, emitted as `required_covers=price|price_plus_gas`. The pre-flight check fires before `eth_estimateGas` (a wallet that cannot cover the value makes the estimate fail opaquely), so its figure excludes gas and says so rather than being reported under a "price + gas" label an orchestrator would top up against once and fail again

**`activation.rs`** - `ensure_headless` plus the shared fast path
- `pub mod activation` (was private) so `activation::ensure_headless` is the real path
- `ensure_headless(&dyn Signer, &HeadlessContext) -> (Session, HeadlessOutcome)`: cached-session fast path → chain-id guard → `tokens_of_owner` → (empty → `purchase()`) → `cooldown_ready` → `activate()` → `wait_for_receipt` → `draft_from_activation` → `personal_sign` → `verify_local` → `save_session`
- `HeadlessOutcome { Reused | Activated | PurchasedAndActivated { token_id, price_wei } }` - lets an orchestrator tell "launched from cache" from "spent money"
- `try_session_fast_path` gained `require_wallet` + `require_token` parameters and now returns the session rather than a bool. The interactive door passes `None` for both (whoever last activated on this machine is the user); headless passes its signer's address, so an agent never launches on a session belonging to a different key, and passes `--token-id` when given, so an explicit token constrains cache reuse exactly as it constrains purchasing. `require_token` **selects** rather than filters: it loads that token's own `<token_id>.json` instead of taking `load_latest_session`'s newest-across-all-tokens result, so a signer holding several licenses still reuses the requested token's session when another token was activated more recently. The session's signed `token_id` is then checked against the requested one, since the session directory is user-writable and a filename proves nothing
- An unqualified run activates the **lowest** token id the signer holds (`lowest_token`), not `tokens_of_owner`'s first entry: that is OpenZeppelin's ERC721Enumerable owner array, whose swap-and-pop ordering is arbitrary after any transfer out
- A `purchase()` that is broadcast but not confirmed inside the 30s budget exits **21**, not the retryable 14: the price may already have left the wallet, and an orchestrator following 14's "retry" advice would buy a second license while the first tx is still pending. Once the hash exists the funds are committed, so **every** way of failing to confirm it is 21 - a node that answers "not mined yet" until the budget runs out and a node that stops answering at all are equally unresolved. `rpc::wait_for_receipt` returns a typed `ReceiptWaitError` (`Timeout` vs `Transport`) so the message can say which one happened, and tolerates transient poll failures inside the budget rather than abandoning the transaction on the first 502; the purchase path maps both variants through one `unconfirmed()` helper, so no future variant can leak back into 14. `activate()`'s own failures stay 14, because a re-send either lands or hits the cooldown gate. The detail line carries `tx_hash` + `waited_secs` so the pending transaction can be resolved out of band
- `--token-id N` that the signer does not hold is a hard error, never a fallback purchase - buying a different token than the one asked for spends money the caller did not authorise. The same id also narrows the cached-session fast path, so a session for another token cannot stand in for the one that was asked for
- A malformed `CONTRACT` constant routes to `NoContract` (exit 1), the same terminal classification as the zero address: both mean "no usable contract was compiled in", and neither is worth a retry
- Chain-id guard: the node's `eth_chainId` is compared to the build's before anything is signed. A wrapper packed for Base and pointed elsewhere would otherwise broadcast a perfectly valid transaction to the wrong network
- `ActivationError::NoInteractiveFrontDoor` for builds compiled without `webview`; `interactive_slow_path` is cfg-split so the webview types are referenced only when they exist

**Shared, not forked** - the two doors sit on identical machinery
- `rpc::wait_for_receipt` (+ `RECEIPT_POLL_ATTEMPTS`/`RECEIPT_POLL_INTERVAL_SECS`) replaces the private `poll_receipt` that lived in `webview.rs`; both doors now use the same 30s budget
- `session::draft_from_activation(...) -> SessionDraft` lifts the whole post-activation block out of `webview.rs::spawn_tx_poller`: reads `activeSessionId`, resolves the identity model, derives the ERC-6551 TBA for account-model deploys, mints the nonce, computes `expires_at`, and builds the preimage. One producer means the doors can never drift into signing different bytes for the same on-chain facts. The draft also carries the **normalised lower-case wallet string** the preimage commits to, and the webview echoes that back through `onTxConfirmed` so JS returns the exact value that was hashed
- `rpc::chain_id()` - new reader for the pre-signing guard

**CLI (`main.rs`)** - `rub3-wrapper --headless [--token-id N]`
- `--headless` parses on every build; a build without the feature exits **18** rather than a clap usage error, so an orchestrator that picked the wrong binary learns that specifically
- `activation::EXIT_CODE_HELP` is rendered into `--help`, so the contract is discoverable from the binary and not only from the docs. Exit codes are defined unconditionally in `activation.rs` (not inside the gated module) precisely so code 18 exists in non-headless builds
- Failures print one human line plus, when structured, one `rub3-detail: key=value` line - `blocks_remaining=N` for cooldown, `required_wei`/`available_wei` for funds, `supply_cap`/`minted` for sold out

| Code | Meaning |
|---|---|
| 0 | success - session valid, wrapped binary launched |
| 1 | unclassified failure |
| 2 | command-line usage error (clap; reserved, nothing of ours collides) |
| 10 | no usable signer |
| 11 | insufficient funds for purchase + gas |
| 12 | no token held and supply sold out |
| 13 | cooldown active - `blocks_remaining=N` on stderr |
| 14 | `activate()` reverted, or did not confirm in time - retryable |
| 15 | session verification failed |
| 16 | chain RPC / transport failure |
| 17 | session could not be persisted |
| 18 | headless mode not compiled into this build |
| 19 | chain id mismatch between the RPC endpoint and this build |
| 20 | `--token-id` names a token this signer does not hold |
| 21 | purchase broadcast but not confirmed (timeout or receipt-poll transport failure) - terminal, `tx_hash=0x...` on stderr |

**Tests**
- `signer.rs`: 20 tests - hex accepted bare/prefixed/whitespace-padded, rejected for wrong length, non-hex, and out-of-range scalars (zero and above the curve order); `Debug` redaction and error messages asserted **not** to echo the input; `personal_sign` round-trips through `license::recover_address`; `sign_prehash` recovers the signer address; RFC-6979 determinism; env-over-keystore precedence; no fall-through on a malformed env key; keystore decrypt via a keystore written in-test; password-file preferred over the inline var, trailing newline stripped; wrong password opaque and not echoed; missing file/password reported
- `tx.rs`: 7 tests - invalid-URL transport error, the `insufficient funds` classifier (match, case-insensitive, pass-through, and carrying no amounts because it only matched a string), and the amounts appearing in the message when they are known, plus the price-only shortfall stating that it excludes gas
- `activation.rs`: 17 tests - the full exit-code table asserted value-by-value, all classified codes distinct and disjoint from 0/1/2, `machine_detail` contents for cooldown/funds/sold-out, no detail line for unclassified failures or for a node-reported shortfall with unknown amounts, a price-only shortfall labelled as excluding gas on both surfaces, a malformed contract classified as terminal rather than retryable, `lowest_token` picking the minimum of an unsorted owned set, the token-scoped fast path reusing the requested token's own session when another token's is newer and rejecting one whose signed token id disagrees with the file it came from, an unconfirmed purchase mapping to the terminal code 21 with its tx hash on the detail line for **both** receipt-wait outcomes (timeout and transport failure) with the transport reason surfaced in the message, and `TxError → HeadlessError` code mapping
- `rpc.rs`: 5 new tests - a transport-error test for `chain_id()`, plus the receipt poll loop driven over scripted answers (a transient failure does not end the wait, a failure that outlasts the budget is reported as `Transport`, a poll that recovers ends as `Timeout`, and both outcomes report the elapsed budget)
- `tests/headless_e2e.rs` (new, anvil-gated, `#[ignore]`, zero webview involvement - the test binary links neither `wry` nor `tao`): 7 tests, each on a **freshly generated key** resolved through the real `RUB3_AGENT_KEY` path with an isolated `RUB3_SESSION_DIR`. Happy path funds the key, purchases at a non-zero price (0.01 ETH, so the value-transfer and balance paths are actually exercised), activates, and asserts ownership, `nextTokenId`, every session field, `verify_local`, `verify_onchain`, on-disk persistence, then **relaunches and asserts `Reused` with no new mint**. Plus: insufficient funds (11), sold out against a supply cap of 1 (12), cooldown active reporting `blocks_remaining` then succeeding after `anvil_mine` with `session_id` bumped to 2 (13), explicit `--token-id` not owned minting nothing (20), explicit `--token-id` refusing to reuse a cached session minted for a different token (20), and chain-id mismatch refused before signing (19). Skips gracefully when Foundry is absent, matching `session_onchain_e2e.rs`. Runs on port 8549 so both suites can run side by side, and serialises its own tests through a file-level mutex covering the port and the process-global env vars, so no `--test-threads=1` is required; `session_onchain_e2e.rs` was not modified

**Verification**
- `cargo test -p rub3-wrapper --lib` (default tier-2 + webview): 58 pass (up from 57)
- `cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib`: 66 pass (up from 61)
- `cargo test -p rub3-wrapper --no-default-features --features tier-3,headless --lib`: 110 pass
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean, plus `headless` alone, `tier-3,headless` and `tier-2,webview`. CI's matrix covers both front doors: `tier-3,headless` and `tier-2,webview` are blocking jobs, so neither door can break green
- **No GUI in a headless build**: `cargo tree --no-default-features --features tier-3,headless` contains neither `wry` nor `tao`, nor any of the 20 GUI crates the webview build pulls (`cocoa`, `core-graphics`, `objc`, `dpi`, `raw-window-handle`, …). On macOS the release binary links no WebKit, AppKit, QuartzCore, or `libobjc` - only `Security`, `CoreFoundation`, `libiconv`, `libSystem`
- Anvil-gated headless e2e: 7 pass, run without `--test-threads=1`, and now run in CI as a second step of the existing `onchain-e2e` job. Existing `session_verify_onchain_e2e` still passes untouched
- No new clippy warnings in any changed or added file

**Deferred to §2.2**
- USDC via EIP-3009. The headless purchase path is ETH-only today; §2.2 adds `purchaseWithAuthorization` and has headless prefer it when the contract advertises it

### 2.2 — USDC purchase via EIP-3009 `[not started]`

Machine money. Agents hold stablecoins, not ETH; card rails can't onboard them at all.

- Contracts: `purchaseWithAuthorization(...)` — accepts a USDC EIP-3009 `transferWithAuthorization` signature; anyone (developer, facilitator, or the buyer itself) may submit, so the purchase is gasless for the buyer. `priceToken` (USDC address) + `priceAmount` alongside the existing ETH `price`; either path mints identically.
- Same addition to `renew()` for subscriptions.
- Wrapper/CLI: headless purchase (§2.1) prefers the USDC path when the contract advertises it; ETH remains the fallback.
- Compatibility goal: anything that speaks x402 can pay for a rub3 license — this is the prerequisite for Bazaar-style catalog listings (§3.3).

### 2.3 — Rub3Factory + protocol fee `[not started]`

The revenue mechanism, stamped at deploy time and immutable thereafter.

```solidity
contract Rub3Factory {
    uint16  public immutable feeBps;    // 200–300; frozen per factory version
    address public immutable treasury;  // rub3 fee recipient
    mapping(address => bool) public isDeployed;  // registry + marketplace trust only these

    function deployAccess(...) external returns (address);
    function deploySubscription(...) external returns (address);
}
```

- Fee split executes on-chain inside `purchase()` / `renew()`: `feeBps` to `treasury`, remainder to the developer's `withdraw()` balance. **Immutable per contract** — a developer's economics can never change after deploy; rub3 changes its take only by shipping a new factory version, which affects future deploys only.
- Direct (non-factory) deployment of the open-source contracts stays possible — it just isn't listable in the registry or marketplace. The fee buys distribution, verification, and liquidity, priced so routing around it costs more than paying it.
- `rub3 deploy` (§2.5) goes through the factory by default.
- Never charged: deploys, CLI, SDK, wrapper. No token.

### 2.4 — Ownership invariants `[not started]`

"The token is the invariant; everything else is versioned." Encode it in bytecode, not policy.

- **Append-only wrapper hash set** — replace the single rotatable `wrapperHash` slot with `mapping(bytes32 => HashStatus) { Unknown, Valid, Revoked }` + `addWrapperHash` / `revokeWrapperHash(hash, reason)`. Old releases stay verifiable; compromised builds are flagged on-chain with a reason. Revoking a *binary hash* never touches *token validity*.
- **Successor pattern** — owner-settable `successor` address on `Rub3License`. Three hard properties: the old contract validates its tokens forever regardless; migration (snapshot-claim or burn-to-mint on the successor) is holder-initiated, never forced; the wrapper accepts "contract X, or X's successor holding a token claimed from X". Covers contract bugs, paid major versions, and chain migration.
- **Per-token renewal snapshot** — `renewPrice[tokenId]` frozen at mint in `Rub3Subscription`; a developer cannot reprice a held subscription.
- **No-revocation audit** — verify (and test for) the absence of burn, admin transfer, and any pause affecting `ownerOf` / `isValid` / `activate` for issued tokens. Document the resulting mutability table (see architecture.md §North Star) as a machine-checkable claim agents can audit before buying.

### 2.5 — rub3 CLI `[not started]`

Pulled forward from the old Phase 2 — a CLI is the natural agent interface, and every step is already scriptable.

```
rub3 pack --binary ./target/release/myapp --app-id com.example.myapp \
  --contract 0x1234...abcd --chain base --tier cooldown --headless \
  --session-ttl 7 --output ./dist/myapp

rub3 deploy --type access --identity account --tba-implementation 0x... \
  --price-usdc 20 --chain base            # via Rub3Factory by default

rub3 fetch 0x1234...abcd                  # download from contentURI, verify hash on-chain (§3.1)

rub3 register --name myapp --contract 0x1234...abcd
```

- `pack`: single distributable binary (wrapper + embedded app + config); `--headless` selects the no-webview build; cross-platform targets.
- `deploy`: factory-mediated; `--identity` sets `identityModel`; `--price-usdc` configures the EIP-3009 path.
- `fetch`: the agent-side half of distribution (§3.1).
- `register`: registry entry (§3.2).

**Phase 2 deliverable:** `rub3 deploy` → fund a fresh key → `rub3-wrapper --headless` completes purchase → activation → launch with no human present, and the deployed contract carries the fee split and ownership invariants.

---

## Phase 3: Distribution & Discovery

Goal: close the loop — discover → pay → fetch → verify → run — so the contract is a complete, self-describing distribution record, and machines doing integration research find rub3 first.

### 3.1 — Content-addressed distribution `[not started]`

- Contract gains `contentURI` (IPFS/Arweave) next to the wrapper hash set — the on-chain record now says *where* the binary lives and *what* it must hash to.
- `rub3 fetch <contract>` downloads from `contentURI`, verifies against the hash set (rejecting `Revoked`), and reports which release it got.
- `rub3 pack --publish` pins the artifact and writes `contentURI` + hash in one step.
- Hosted pinning is an optional paid convenience (off the enforcement path); any pinning service works.

### 3.2 — Registry `[not started]` *(replaces old §2.4)*

- Deploy `Rub3Registry` on Base: `register(appName, contract)` requires `factory.isDeployed(contract)` **and** contract ownership — only canonical deploys are listable.
- **Discovery, never validity:** delisting removes the badge and the listing; it cannot invalidate a token or a session. This invariant is documented and tested.
- Each entry doubles as an ERC-8004-style agent card: contract address, price(s), payment methods, `contentURI`, hash set, identity model — machine-readable, so agent spend policies can allowlist "verified rub3 contracts" and audit the §2.4 invariants before buying.
- Wrapper ENS handling softens accordingly: resolution to a *different* address → hard fail (attack signature); failure to resolve (lapsed name, dead registry, offline) → warn and proceed. The embedded contract address is the root of trust after purchase.

### 3.3 — Agent-facing surface `[not started]`

Distribution to the machines doing the integration research.

- `llms.txt` + docs served as clean Markdown (the repo's docs are already agent-legible; formalize it).
- Docs MCP server so Claude Code / Cursor pull real method signatures and contract ABIs instead of hallucinating them.
- One-shot quickstart: a single self-contained prompt/script — "paste this into your coding agent and your binary is wallet-gated on Base Sepolia in minutes" — deterministic, testnet-safe, verifiable. Market that fact explicitly.
- Listings: blockchain/MCP server directories, x402-adjacent catalogs (once §2.2 lands), ERC-8004 registries.
- **Beachhead:** wallet-gated MCP servers — ship the example (`examples/hello-mcp/`) and target paid-MCP developers as design partners.

### 3.4 — Concurrent seats `[not started]`

Fleet licensing — the tier the agent economy actually wants.

- Generalize tier 3's single `activeSessionId` into an on-chain semaphore: `maxConcurrentSessions[tokenId] = K` (set at purchase tier / deploy), `activate()` admits up to K live session ids per token, `release()` (or TTL lapse) frees a seat.
- One license NFT = K concurrent fleet instances; buy another token to scale. Cooldown still rate-limits churn.
- Wrapper: seat-aware activation + a clear "fleet exhausted, N seats in use" error for orchestrators.

### 3.5 — rub3 SDK crate `[not started]` *(moved from old §2.3)*

- `rub3::heartbeat()` — panics if wrapper is not alive (Unix socket / named pipe)
- `rub3::session()` — returns `SessionInfo { app_id, token_id, user_id, wallet, identity, expires_at }`
- Application code keys all persistent data on `user_id`, never on `wallet`
- Socket path passed as env var by wrapper; minimal dependency footprint — no `alloy` or `wry`
- Needed early for the MCP-server beachhead (a wrapped server checks its session/heartbeat).

**Phase 3 deliverable:** an agent that has never heard of rub3 can find a wrapped app via the registry/docs surface, buy it in USDC, fetch and verify the binary, and run it — headlessly, end to end.

---

## Phase 4: Machine Economy

Goal: the payment flows only rub3 can host.

### 4.1 — Metered billing (`Rub3Metered`) `[not started]`

- Third billing model: the launch gate requires a micropayment — per launch, per session-hour, or per N launches — settled in USDC (EIP-3009 authorizations batched/settled on-chain).
- The structural moat: x402 meters API calls because the server is a choke point; the wrapper is the only viable choke point for *locally executed* software. Same protocol fee, much higher-frequency flow.
- Pilot with one or two paid-MCP-server design partners before generalizing.

### 4.2 — Facilitator `[not started]`

- Hosted relay that submits EIP-3009 purchase/renew/meter authorizations and fronts gas for buyers holding only stablecoins.
- Bundled into the protocol fee rather than separately priced — its function is making the fee-carrying path also the lowest-friction path.
- Self-hosting the facilitator remains possible (it's a thin relay); the hosted one is a convenience, not a chokehold.

### 4.3 — License marketplace `[trigger-gated]`

- **Do not build speculatively.** Trigger: organic `Transfer` volume on factory contracts (all on-chain — query for the moment resale behavior emerges).
- Purpose-built venue for license resale: queryable by agents, filtered to registry-verified contracts, priced in USDC. 1–2% marketplace fee + ERC-2981 royalty split with the developer.
- This is what makes "licenses as liquid capital assets" real: agents buy for a workload, resell when the job ends.

**Phase 4 deliverable:** revenue flows from all three billing models plus secondary trades, entirely on-chain, with no invoicing and no accounts receivable.

---

## Phase 5: Human Surface *(demoted, not dropped)*

The interactive path stays fully supported — manual tx-hash paste is the floor today and remains reachable forever. Polish lands after the agent path.

### 5.1 — Frictionless tx confirmation *(spec in §1.10 / §1.10a / §1.10b)*
- Auto-detect and WalletConnect tabs on the purchase/cooldown screens, manual paste as the always-available floor. The detailed specs in §1.10a/§1.10b apply unchanged.

### 5.2 — Activation UI refactor to Preact *(was old §2.5)*
- Single reducer over `(phase, ctx)`; vendored `preact.mjs` + `htm.mjs` under `assets/vendor/`; `include_dir!` custom-protocol handler; no Node/bundler. No behavioral changes.

### 5.3 — Tauri integration *(was old Phase 3)*
- `tauri-plugin-rub3`: auto-heartbeat, session renewal in the app's own webview, `invoke('plugin:rub3|session')` JS API, `rub3://session-renewed` event.
- `create-rub3-app` starter template preconfigured against Base Sepolia.

### 5.4 — Polish *(was old Phase 4, minus deferred items)*
- Background session renewal with OS notification before expiry
- Windows support: named pipes for heartbeat IPC, MSVC target, WebView2 testing
- Subscription renewal UI (view expiry, renew from tray/menu)
- Multi-wallet delegation (hardware wallet owns, hot wallet signs sessions — EIP-7702 or delegation registry; exploratory)

---

## Deferred

Cut from the active roadmap with rationale; scaffolds are retained.

- **Tier 4 device binding** (`activateDevice`, `registeredDevice`, Secure Enclave/TPM storage) — device binding treats fleet cloning as an attack, but agent fleets clone VMs as a legitimate pattern; seats (§3.4) are the right concurrency primitive. Human anti-sharing pressure also shrinks when the customer is an agent with a wallet and a spend policy. `device.rs` scaffold stays behind the `device-key` feature.
- **Binary encryption** (AES-256-GCM unwrap, in-memory exec) — large engineering surface against a threat model the agent thesis dissolves; extraction-resistance was never a goal (see ideation.md, "Not DRM"). `decrypt.rs` scaffold stays behind `binary-encryption`.
- **Binary obfuscation** (UPX-style) — same rationale.

---

## Tech Stack

| Component | Technology |
|---|---|
| Wrapper runtime | Rust |
| Crypto (secp256k1) | `k256` crate |
| Ethereum RPC | `alloy` crate |
| Webview (interactive fallback) | `wry` crate — excluded from `headless` builds |
| IPC (wrapper ↔ app) | Unix domain sockets / named pipes |
| Smart contracts | Solidity, OpenZeppelin, Foundry |
| Target chain | Base (primary). Config-abstracted for other EVM L2s |
| Machine payments | USDC via EIP-3009 `transferWithAuthorization` |
| Distribution | Content-addressed storage (IPFS/Arweave), hash + URI on-chain |
| Agent surface | `llms.txt`, Markdown docs, docs MCP server |
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
│       │   ├── identity.rs           # Identity models, ERC-6551 TBA derivation
│       │   ├── store.rs              # Proof persistence (RUB3_LICENSE_DIR override)
│       │   ├── activation.rs         # Activation flow orchestration
│       │   ├── rpc.rs                # On-chain queries (ownerOf, price, cooldown, purchase) via alloy
│       │   ├── webview.rs            # Native activation window (wry/tao), IPC
│       │   ├── supervisor.rs         # Child process lifecycle, signal forwarding
│       │   ├── session.rs            # [feature = "session"] session schema, message, verify_local
│       │   ├── session_store.rs      # [feature = "session"] load/save/load_latest_session
│       │   ├── device.rs             # [scaffold, deferred] device keypair mgmt
│       │   └── decrypt.rs            # [scaffold, deferred] AES-256-GCM binary unwrap
│       ├── assets/
│       │   └── activation.html       # Activation UI
│       └── tests/
├── contracts/                        # Foundry project (§1.5, §1.6)
│   ├── src/
│   │   ├── Rub3License.sol           # Abstract base: ERC-721 + Enumerable + Ownable, activation
│   │   ├── Rub3Access.sol            # One-time purchase license
│   │   └── Rub3Subscription.sol      # Time-bounded license (expiresAt, renew, isValid)
│   ├── test/
│   ├── script/Deploy.s.sol
│   └── contracts.md
├── licenses/com.rub3.example.json
├── scripts/
├── architecture.md
├── implementation.md
├── ideation.md
└── testing.md
```

Planned (not yet created):

```
├── crates/
│   ├── rub3-sdk/                # §3.5 — heartbeat, session info
│   ├── rub3-cli/                # §2.5 — pack, deploy, fetch, register
│   └── tauri-plugin-rub3/       # §5.3
├── contracts/src/
│   ├── Rub3Factory.sol          # §2.3 — fee-stamping deploys
│   ├── Rub3Metered.sol          # §4.1 — per-launch billing
│   └── Rub3Registry.sol         # §3.2 — discovery + agent cards
├── llms.txt                     # §3.3
├── docs-mcp/                    # §3.3 — docs MCP server
└── examples/
    ├── hello-mcp/               # §3.3 beachhead — wallet-gated MCP server
    ├── hello-rust/
    └── hello-subscription/
```
