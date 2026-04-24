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
- **Activation (tier 4)**: `activateDevice(uint256 tokenId, bytes32 devicePubKey) returns (sessionId)` — shares the owner + cooldown check with `activate()` via a shared `_activate` internal helper; additionally writes `registeredDevice[tokenId] = devicePubKey` (overwriting any previous binding) and emits `DeviceRegistered(tokenId, devicePubKey)`. Rejects a zero fingerprint (`InvalidDevicePubKey`) because `bytes32(0)` is reserved for "no device bound". Plain `activate()` does not touch `registeredDevice`, so tier 3 deploys continue to work unchanged.

**`Rub3Access.sol`** — concrete, one-time purchase:
- `purchase(address recipient) payable returns (uint256 tokenId)` — pays `price`, mints next id
- `Purchased(tokenId, recipient, payer)` event

**`Rub3Subscription.sol`** — concrete, time-bounded:
- Immutable `period`, `mapping(uint256 => uint256) expiresAt`
- `purchase(address recipient) payable` — mints + sets `expiresAt = now + period`
- `renew(uint256 tokenId) payable` — extends from current expiry if still valid, else resets to `now + period`
- `isValid(uint256 tokenId) view` — `expiresAt[tokenId] > block.timestamp`
- `Purchased` + `Renewed` events

**Tests:** 43 forge tests (`forge test`) covering metadata, sequential mint, zero-recipient default, over/underpay, supply cap, enumeration via `tokenOfOwnerByIndex`, owner-gated setters, withdraw, subscription expiry, mid-period renewal, post-expiry renewal, nonexistent-token revert, plus activation: first-call success, session-id increments across tokens, cooldown-window revert, post-cooldown success, non-owner revert, nonexistent-token revert, `cooldownReady` in all three states, constructor floor check (`cooldownBlocks < 15`), transfer-then-activate (new owner authorized, old owner rejected), and 10 tier-4 tests covering `activateDevice`: initial zero `registeredDevice`, first-call records key + session, zero-key revert, non-owner revert, cooldown revert with device untouched, post-cooldown overwrite, tier-3→4 upgrade mid-flow, plain `activate` leaves binding intact, shared session counter across `activate`/`activateDevice`, post-transfer replacement.

**`script/Deploy.s.sol`** — forge script that deploys either contract from env vars:
- `CONTRACT_TYPE`, `TOKEN_NAME`, `TOKEN_SYMBOL`, `IDENTITY_MODEL`, `WRAPPER_HASH`, `PRICE` required; `SUPPLY_CAP`, `OWNER`, `COOLDOWN_BLOCKS` (default 1800 ≈ 1hr on Base), `PERIOD` optional
- Dry run (no `--broadcast`): simulates deployment, prints summary with all params
- Live: add `--broadcast --verify --etherscan-api-key $BASESCAN_API_KEY`
- Local: run against `anvil` with `--rpc-url http://localhost:8545` and a pre-funded Anvil key — no `.env` needed

**Not yet done:**
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
- `identity.rs`: 10 tests — `IdentityModel` from_u8 / as_str / rejects-out-of-range; TBA determinism + sensitivity to each of `{implementation, chain_id, contract, token_id}`; `resolve_user_id` for both models + panic on missing TBA
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
- Refactor `activation.html` to Preact (vendored `preact.mjs` + `htm.mjs`, custom-protocol handler via `include_dir` — no Node/build step). Tracked in Phase 2 as §2.5.
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

### 1.10 — Frictionless tx confirmation `[not started]`

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
- `ActivationContext` (the `main.rs` constants struct) gains `wc_project_id: Option<&'static str>`. Missing or placeholder → WC tab is hidden. Default in the wrapper's own dev builds is `None`, not a shared project ID — `rub3 pack` (§2.1) rejects a distributable that inherits a placeholder value.
- Feature flag `wallet-connect` on the wrapper crate — opt-in because of the vendored JS weight. Composes with `onchain-write`; does not change tier bundle definitions (developer picks `tier-3,wallet-connect` at pack time).
- `webview.rs::show_purchase` / `show_cooldown` include the project id in the `onShowPurchase` / `onShowCooldown` payload when the feature is compiled in; JS decides whether to render the tab based on its presence.

**Assets (`assets/vendor/`)**
- `walletconnect-sign-client.mjs` — Reown SignClient v2 bundle (~250 KB).
- `qrcode.mjs` — ~5 KB QR-from-URI renderer.
- Both served by the same `include_dir!` custom-protocol handler introduced in §2.5; if §2.5 has not landed yet, this section creates that handler.

**Assets (`assets/app/`)**
- New `wc.js` — init `SignClient`, open a session via `chains: ["eip155:<chain_id>"]`, render the pairing URI as an inline QR, call `client.request({ method: "eth_sendTransaction", params: [{ to, data, value }] })` to dispatch either the purchase or activate tx. Returns the tx hash through the existing `purchase_tx_sent` / `activate_tx_sent` IPC message — reusing the rest of the pipeline.

**HTML**
- WC tab body: the vendored QR canvas, a "copy pairing URI" fallback, and error copy that suggests falling back to Auto-detect or Manual.

**Gating recap.** `wallet-connect` Cargo feature + developer-supplied project id. Both must be present for the tab to render; either absent → the tab is silently omitted and the user sees a 2-tab (or 1-tab) screen.

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

### 2.5 — Activation UI refactor to Preact `[not started]`

The current `assets/activation.html` is a single 700-line file of vanilla JS
with hand-rolled DOM manipulation and module-scoped `pending*Ctx` state
variables. Each screen added (§1.7's purchase screen is the 7th) makes the
state flow harder to follow.

Goals
- Replace DOM id lookups with a component tree driven by a single reducer
  (`phase`, `ctx`) — one reducer action per inbound IPC callback.
- Keep the asset pipeline build-free: commit `preact.mjs` + `htm.mjs` under
  `assets/vendor/`, switch the webview from `WebViewBuilder::with_html` to a
  `with_custom_protocol` handler that serves files from
  `include_dir!("assets")`. No Node / no bundler in CI.
- No behavioral changes — the Preact version must drive the same IPC
  surface, screens, and error paths as today's vanilla version.

Out of scope here (dedicated sub-issues)
- ENS lookups on the purchase screen (not present today either).
- USD price conversion.

**Deliverable:** `activation.html` becomes a ~30-line shell; each screen is a
component in `assets/app/screens/`. No change to Rust-side IPC types.

---

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
- WalletConnect webview (§1.10b) tested on Windows WebView2

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
│       │   ├── session.rs            # [feature = "session"] session schema, message, verify_local, is_expired
│       │   ├── session_store.rs      # [feature = "session"] load/save/load_latest_session
│       │   ├── device.rs             # [scaffold, feature = "device-key"] device keypair mgmt (tier 4)
│       │   └── decrypt.rs            # [scaffold, feature = "binary-encryption"] AES-256-GCM binary unwrap
│       ├── assets/
│       │   └── activation.html       # Activation UI
│       └── tests/
│           ├── helpers/mod.rs        # Wallet gen, signing, license creation
│           ├── integration.rs        # Wrapper binary tests
│           └── license_e2e.rs        # Static + dynamic license verification tests
├── contracts/                        # Foundry project (§1.5)
│   ├── src/
│   │   ├── Rub3License.sol           # Abstract base: ERC-721 + Enumerable + Ownable, metadata, mint helper
│   │   ├── Rub3Access.sol            # One-time purchase license
│   │   └── Rub3Subscription.sol      # Time-bounded license (expiresAt, renew, isValid)
│   ├── test/
│   │   ├── Rub3Access.t.sol
│   │   └── Rub3Subscription.t.sol
│   ├── script/
│   │   └── Deploy.s.sol              # Deploys either contract from env vars; supports Anvil + Base Sepolia
│   ├── lib/                          # Git submodules: openzeppelin-contracts@v5.1.0, forge-std
│   ├── foundry.toml
│   ├── remappings.txt
│   ├── .env.example                  # Template for RPC URLs, keys, deploy params
│   └── contracts.md                  # Local (Anvil) + on-chain (Base Sepolia) setup guide
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
│   └── src/
│       └── Rub3Registry.sol # §2.4 — ENS subdomain registry
└── examples/
    ├── hello-rust/
    ├── hello-subscription/
    └── hello-tauri/
```
