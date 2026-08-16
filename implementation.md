# rub3 - Implementation Plan

This file owns the roadmap and the build record: what exists, what is next, and what was decided or rejected along the way. Its `[complete]` / `[partial]` / `[not started]` tags are the authority on what is built. Design rationale lives in [architecture.md](architecture.md), contract operations and fee mechanics in [contracts/contracts.md](contracts/contracts.md), the per-suite test inventory in [testing.md](testing.md), positioning in [ideation.md](ideation.md), and orientation in [README.md](README.md).

> **Plan revision - July 2026 (agent-first reorientation).**
> Everything below Phase 1 has been resequenced around a single thesis: agents will do an increasing share of software development, deployment, and purchasing, and they need to buy, verify, run, and resell locally executed software - low cost, high speed, secure payments - with no human in the loop.
>
> What changes:
> - **Headless is the front door.** All session crypto is already native Rust; the webview exists only because humans keep keys in wallet apps. Signer-in/session-out activation (§2.1) becomes the primary mode; the webview is the human fallback floor.
> - **Machine money.** USDC purchases via EIP-3009 signed authorizations join ETH pricing as the default path (§2.2).
> - **Revenue at the network layers.** The rails (wrapper, SDK, CLI, contracts) stay open source and free. Revenue is an immutable 2–3% fee stamped by `Rub3Factory` (§2.3), metered per-launch billing only the wrapper can enforce (§4.1), and - once volume shows - a registry-filtered resale marketplace (§4.3).
> - **The token is the invariant.** No proxies, no revocation surface, no pause on validation. Evolution only changes what is offered going forward, never what was granted (§2.4).
> - **Distribution completes the loop.** `contentURI` on-chain + `rub3 fetch` + hash verification = discover → pay → fetch → verify → run (§3.1).
> - **Seats, not devices.** Fleet concurrency is licensed as K on-chain seats per token (§3.4). Tier-4 device binding and binary encryption move to Deferred.
> - **Human UX polish is demoted, not dropped.** WalletConnect tabs (§5.1), the Preact refactor, and the Tauri plugin move to Phase 5.
>
> Phase 1 sections below are the build record of what exists today. They are corrected in place when a later phase supersedes a fact, with the pointer to the section that superseded it; §2.4 carries the removal record for everything the ownership invariants took out.

## Phase 1: Proof of Concept

Goal: A working wrapper that gates a Rust binary behind wallet ownership, using a cached SIWE-style session.

### 1.1 - Wrapper skeleton `[complete]`
- `rub3-wrapper` Rust project with CLI: `rub3-wrapper --binary <path>` (clap)
- Launches embedded app as child process (`supervisor.rs`)
- SIGTERM forwarding: wrapper forwards signals to child, exits when child exits
- Process supervision proven with integration tests

### 1.2 - License proof + signature verification `[complete]`
- License proof JSON schema (`license.rs`): `app_id`, `token_id`, `wallet_address`, `signature`, `activated_at`, `chain`, `contract`, optional `paid_by`
- Activation message: `SHA-256(app_id || token_id_be_bytes)` - deterministic, fixed-width
- Signature verification: `personal_sign` prefix (keccak256), secp256k1 ECDSA recovery via `k256`, address comparison
- Proof persistence (`store.rs`): save/load to `~/.rub3/licenses/<app_id>.json` or `$RUB3_LICENSE_DIR`
- Static and dynamic integration tests verify the full crypto pipeline natively in Rust (no external tools)
- Result: valid proof → launch app, invalid/missing → trigger activation flow

### 1.3 - Activation flow + webview `[partial]`
- Activation orchestration (`activation.rs`): check cached proof → verify → launch, or open activation window
- Native webview (`wry`/`tao`) with dark-themed activation UI (`assets/activation.html`)
- IPC message protocol: JS ↔ Rust (ready, connect, token_selected, signed, cancel, error)
- Screens: connect (address input) → token-select (when multiple tokens owned) → activate (message + signature input) → processing
- Activate screen surfaces the exact `personal_sign` preimage (hex) so the user knows what to sign in their wallet
- **Done:** manual wallet address input, `tokensOfOwner()` enumeration, multi-token selection UI, activation message display, manual signature paste, proof storage on success
- **Not yet done:** WalletConnect integration - tracked as §5.1b (requires WC v2 JS SDK + developer-supplied project ID)

### 1.4 - On-chain queries `[complete]`
- `rpc.rs`: `ownerOf(tokenId)`, `price()`, `balanceOf(owner)`, `tokenOfOwnerByIndex(owner, index)` via alloy JSON-RPC with minimal ABI (`IRub3License`)
- `tokens_of_owner(rpc_url, contract, owner)` enumerates all tokens held by a wallet via ERC-721Enumerable
- Synchronous wrapper over async alloy calls (`block_on` with single-threaded tokio runtime)
- Ownership check wired into webview `Connect` handler: 0 tokens → error, 1 → auto-proceed to activate, N → token-select screen
- ENS resolution remains a stub (`EnsNotSupported`) - deferred to §1.6 where it is the primary deliverable

### 1.5 - Smart contracts `[complete]`

Branch: `feature/smart-contract`. Foundry project under `contracts/` with OpenZeppelin v5.1.0 and forge-std installed as submodules under `contracts/lib/`.

**Abstract base - `Rub3License.sol`**
- Inherits `ERC721`, `ERC721Enumerable`, `Ownable` (OZ v5)
- Immutable: `identityModel` (0 = access, 1 = account; rejects values > 1), `supplyCap` (0 = uncapped), `cooldownBlocks` (floor `MIN_COOLDOWN_BLOCKS = 15` ≈ 30s on Base)
- Mutable + owner-gated: `price` (`setPrice`). **Superseded by §2.4:** the single rotatable `wrapperHash` is gone, and `setWrapperHash(bytes32)` is now one of the forbidden selectors asserted absent from the runtime bytecode of all four audited targets. Wrapper hashes live in an append-only set with on-chain revocation reasons instead, so rebuilding the wrapper adds a hash rather than replacing one. See §2.4 for the removal record
- `nextTokenId` counter + internal `_mintNext` helper for sequential ids from 0
- `_resolveRecipient(address)` helper: `address(0)` → `msg.sender` (per architecture.md §1)
- `withdraw(address payable)` owner-only sweep
- `_update` / `_increaseBalance` / `supportsInterface` overrides for ERC-721 + Enumerable composition
- **Activation (tier 3)**: `activate(uint256) returns (sessionId)` - owner-only, bumps `activeSessionId[tokenId]` from a monotonic `_sessionCounter`, records `lastActivationBlock`, reverts `CooldownActive(blocksRemaining)` if called again inside the window (first call, `last == 0`, bypasses); `cooldownReady(tokenId) view returns (bool, uint256)` for the wrapper's pre-tx check; `Activated(tokenId, owner, sessionId)` event

**`Rub3Access.sol`** - concrete, one-time purchase:
- `purchase(address recipient) payable returns (uint256 tokenId)` - pays `price`, mints next id
- `Purchased(tokenId, recipient, payer)` event

**`Rub3Subscription.sol`** - concrete, time-bounded:
- Immutable `period`, `mapping(uint256 => uint256) expiresAt`
- `purchase(address recipient) payable` - mints + sets `expiresAt = now + period`
- `renew(uint256 tokenId) payable` - extends from current expiry if still valid, else resets to `now + period`
- `isValid(uint256 tokenId) view` - `expiresAt[tokenId] > block.timestamp`
- `Purchased` + `Renewed` events

**Tests:** 30 forge tests at this point (`forge test`), covering metadata, sequential mint, zero-recipient default, over/underpay, supply cap, enumeration via `tokenOfOwnerByIndex`, owner-gated setters, withdraw, subscription expiry, mid-period renewal, post-expiry renewal, nonexistent-token revert, plus activation: first-call success, session-id increments across tokens, cooldown-window revert, post-cooldown success, non-owner revert, nonexistent-token revert, `cooldownReady` in all three states, constructor floor check (`cooldownBlocks < 15`), and transfer-then-activate (new owner authorized, old owner rejected). The suite has since grown well beyond that; `testing.md` holds the current inventory.

**`script/Deploy.s.sol`** - forge script that deploys either contract from env vars:
- `CONTRACT_TYPE`, `TOKEN_NAME`, `TOKEN_SYMBOL`, `IDENTITY_MODEL`, `WRAPPER_HASH`, `PRICE` required; `SUPPLY_CAP`, `OWNER`, `COOLDOWN_BLOCKS` (default 1800 ≈ 1hr on Base), `PERIOD` optional. Superseded: `WRAPPER_HASH` is now optional and is the single-hash shorthand for `WRAPPER_HASHES` (§2.4), and later phases added `PRICE_TOKEN` / `PRICE_AMOUNT` (§2.2), `PREDECESSOR` (§2.4) and `FACTORY` (§2.3). `contracts/contracts.md` -> "Environment variable reference" is the current reference
- Dry run (no `--broadcast`): simulates deployment, prints summary with all params
- Live: add `--broadcast --verify --etherscan-api-key $BASESCAN_API_KEY`
- Local: run against `anvil` with `--rpc-url http://localhost:8545` and a pre-funded Anvil key - no `.env` needed

**Not yet done:**
- Tier 4: `activateDevice(tokenId, devicePubKey)` + `registeredDevice` mapping - deferred to tier-4 work
- Base Sepolia deployment. Nothing is deployed to any public network yet, and a mainnet deploy waits on the registry: the factory and the registry launch together (§2.3, §3.2)

### 1.6 - Identity model + TBA derivation `[complete]`

**Contract change** - `Rub3License.sol` gains `address public immutable tbaImplementation`. Constructor now validates that account-model deploys supply a non-zero impl and access-model deploys supply `address(0)` (new errors `TbaImplementationRequired` / `TbaImplementationForbidden`). Threaded through `Rub3Access` + `Rub3Subscription` constructors, the `Deploy.s.sol` script (new `TBA_IMPLEMENTATION` env var), and the Foundry test fixtures. Forge test suite: 33 pass, up from 29 (4 new tests covering the two new reverts plus the happy-path account-model construction).

**Wrapper changes**
- `identity.rs` (new, gated on `session`) - `IdentityModel { Access, Account }` with `from_u8` / `as_str`; `derive_tba(implementation, chain_id, contract, token_id)` computes the ERC-6551 TBA via CREATE2 against canonical registry `0x000000006551c19487814612e58FE06813775758` with `salt = 0` and the reference account-proxy init bytecode (pure, no RPC); `resolve_user_id(model, wallet, tba)` returns lower-case 0x-hex; `format_addr(addr)` helper
- `rpc.rs` - `IRub3License` gains `identityModel() -> uint8` + `tbaImplementation() -> address` getters; new `identity_model()` and `tba_implementation()` pub fns
- `session.rs` - `Session` gains `identity: String`, `user_id: String`, `tba: Option<String>`; `session_message()` adds `identity` + `user_id` into the preimage (between `wallet` and the existing fields) so a forger cannot flip an access-model session into account-model without re-signing. Ordering: `app_id, token_id, identity, user_id, wallet, nonce, [expires_at], [activation_block_hash], [session_id], [device_pubkey]`
- `webview.rs::spawn_tx_poller` - after the existing `active_session_id` read, calls `identity_model()`; for account model also calls `tba_implementation()` and derives the TBA locally. Includes the resolved `identity`, `user_id`, and optional `tba` in the signed preimage + `onTxConfirmed` payload. `IpcMessage::SessionSigned` / `FinalizeArgs` carry the three identity fields through back to the final `Session`
- `activation.html` - sign-session screen shows the identity model label, user_id, and (for account model) TBA address. Echoes all three back in the `session_signed` IPC message

**Tests**
- `identity.rs`: 11 tests - `IdentityModel` from_u8 / as_str / rejects-out-of-range; TBA determinism + sensitivity to each of `{implementation, chain_id, contract, token_id}`; `resolve_user_id` for both models + panic on missing TBA
- `session.rs`: 2 new preimage tests - differs by identity (access → account), differs by user_id alone; 1 new verify test - tampered identity fails `verify_local` with `AddressMismatch`; all existing tests updated to the new 10-arg `session_message()` signature
- `rpc.rs`: 2 new transport-error tests for `identity_model()` + `tba_implementation()`
- `tests/session_onchain_e2e.rs`: updated `forge create` to pass the new `tbaImplementation = address(0)` arg; `Session` struct literal updated. Passes against anvil.

**Verification**
- `cargo test -p rub3-wrapper --lib` (default tier-2): 51 pass (up from 35)
- `cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib`: 55 pass (up from 39)
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean
- `forge test` (contracts/): 33 pass
- Anvil-gated e2e (`session_verify_onchain_e2e`): passes with the new 9-arg constructor

### 1.7 - Purchase UI `[complete]`

In-wrapper purchase flow when the connected wallet owns no token. Gated on
`onchain-write` (tier 3+). Wrapper never holds keys - it encodes calldata,
surfaces it to the user, and polls the receipt they paste back.

**RPC additions (`rpc.rs`)**
- `supplyCap()`, `nextTokenId()`, `purchase(address)` added to the `sol!` interface
- `supply_cap()` / `next_token_id()` public readers
- `encode_purchase_calldata(recipient)` - pure, `SolCall::abi_encode` over `purchase(address)`
- `mint_token_id(rpc_url, tx_hash, contract, recipient)` - fetches the receipt and walks `receipt.inner.logs()` for the ERC-721 `Transfer(0x0, recipient, tokenId)` log (topic0 = `0xddf252ad…`), returning the minted id. Constant `ERC721_TRANSFER_SIG` for comparison
- `pub mod rpc` (was private) so integration tests can drive these directly

**Webview wiring (`webview.rs`)**
- New IPC variant `PurchaseTxSent { tx_hash, owner_address }` gated on `onchain-write`
- `Connect` handler's empty-tokens branch now calls `show_purchase` under `onchain-write`; tier 0-2 still surface the legacy "no token" error
- `show_purchase` reads `supplyCap` / `nextTokenId` / `price`, rejects sold-out state, encodes calldata, emits `onShowPurchase({ ownerAddress, contractAddress, chainId, priceWei, valueHex, supplyCap, nextTokenId, calldata })`. Price is serialised as a decimal string + hex string so a full uint256 price survives JSON
- `spawn_purchase_poller` mirrors `spawn_tx_poller`: polls receipt (30s / 10 × 3s), asserts `status == true` and `receipt.to == contract`, then calls `mint_token_id` to recover the id and re-enters `proceed_after_token_selected` - the downstream cooldown/activate flow is reused verbatim

**HTML (`assets/activation.html`)**
- New `#screen-purchase` with price (ETH + wei), supply counter, recipient, send-to / value / calldata boxes, tx-hash input
- `onShowPurchase` callback populates the screen, stores `pendingPurchaseCtx.ownerAddress`
- `formatEth(weiStr)` - BigInt-based wei→ETH formatter with up to 4 fractional digits; 0 renders as "Free"
- `'purchase'` added to the `SCREENS` array so `show('purchase')` hides the others

**Tests**
- 6 new `rpc` unit tests: purchase calldata selector (`0x25b31a97`) + recipient layout + differs-by-recipient; `supply_cap`, `next_token_id`, `mint_token_id` (both bad-URL and bad-hash) transport-error paths
- Anvil e2e (`tests/session_onchain_e2e.rs`) extended with `supply_cap`/`next_token_id` pre- and post-purchase checks and a `mint_token_id` parse against the real `purchase()` receipt - all four assertions pass against a live Rub3Access on anvil

**Deferred**
- Refactor `activation.html` to Preact (vendored `preact.mjs` + `htm.mjs`, custom-protocol handler via `include_dir` - no Node/build step). Tracked as §5.2.
- Replace the "paste your tx hash" box with auto-detect + WalletConnect tabs while keeping manual paste as the fallback floor. Tracked as §5.1.

**Verification**
- `cargo test -p rub3-wrapper --lib` (default tier-2): 57 pass (up from 51)
- `cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib`: 61 pass (up from 55)
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean
- `forge test` (contracts/): 33 pass
- Anvil-gated e2e (`session_verify_onchain_e2e`): passes with the new purchase-path assertions

### 1.8 - On-chain cooldown + session model (tier 3) `[partial]`

Replaces the legacy `LicenseProof` flow with a full session model backed by an on-chain cooldown. An NFT holder can otherwise run a signing oracle to distribute fresh sessions to non-holders; a contract-enforced `activate()` cooldown rate-limits how many sessions a single token can mint. The wrapper reads cooldown state and encodes calldata - it never sends txs or holds keys.

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

**Phase A - foundation modules `[complete]`**
- `session.rs` - `Session` schema; `session_message()` (SHA-256 over tier-appropriate field set, BE integers, optional fields omitted when `None`); `new_nonce()` (32-byte random hex); `verify_local()` (reconstruct message → `personal_sign` recover → compare to `session.wallet` → expiry check); `is_expired()` (RFC3339 parse vs `Utc::now()`; `None` → false for tier 4)
- `session_store.rs` - `session_path()` (`RUB3_SESSION_DIR` override or `~/.rub3/sessions/<app_id>/<token_id>.json`); `load_session()` / `save_session()`; `load_latest_session()` scans app_id dir, filters expired + invalid-signature sessions, returns most-recently-issued valid one
- `personal_sign_hash`, `recover_address`, `public_key_to_address` promoted to `pub(crate)` in `license.rs`
- 15 tests: message determinism + tier diffing, expiry edge cases (future/past/None/unparseable), sign/verify round-trip, wrong-wallet failure, save/load round-trip, load_latest with mixed valid/expired sessions

**Phase B - RPC + IPC wiring `[complete]`**
- `rpc.rs` additions: `cooldown_ready` → `(is_ready, blocks_remaining)`, `last_activation_block`, `cooldown_blocks`, `active_session_id` (post-tx revocation read), `encode_activate_calldata` (pure, `SolCall::abi_encode`), `get_tx_receipt` → `TxReceipt { status, block_number, block_hash, to }`, `get_block_number`
- `webview.rs` new IPC variants (gated on `cooldown` feature): `ActivateTxSent { tx_hash, token_id, owner_address }`, `SessionSigned { signature, ... }` - JS echoes back all state needed to assemble the `Session`, so the Rust handler is stateless across messages. Outbound JS: `onShowCooldown`, `onTxConfirmed`, `onProcessing`, `onError`. Legacy `Signed` path kept for zero-contract fallback.
- `ActivationResult` gains `SessionSuccess { session }` variant (gated); `LegacySuccess { proof }` replaces the old plain `Success`
- Connect handler branches: zero contract → legacy `show_activate`. Non-zero + `cooldown` → `tokens_of_owner` → `proceed_after_token_selected` → `cooldown_ready` + `encode_activate_calldata` → `onShowCooldown`
- ActivateTxSent handler: spawns a background polling thread (10 × 3s; 30s total timeout) calling `get_tx_receipt`; on confirmation asserts `receipt.to == contract` and `status == true`, reads `activeSessionId`, mints a `new_nonce()`, computes `expires_at` from `SESSION_TTL_SECS`, builds the session message, and emits `onTxConfirmed`
- SessionSigned handler: assembles `Session` (tier-3 fields populated from echoed state), calls `verify_local`, sends `ActivationResult::SessionSuccess`
- `activation.rs::ensure` - tries three paths in order: (1) tier-3 session fast path (`load_latest_session` → `verify_local`), (2) legacy proof fast path, (3) webview. Takes a new `session_ttl_secs` param threaded through from `main.rs` (`SESSION_TTL_SECS = 7 days`). On `SessionSuccess` persists via `session_store::save_session`.
- `assets/activation.html` new screens: `cooldown` (shows calldata + tx-hash input with per-block-remaining banner when cooldown is active), `sign-session` (shows tx hash / block / session id / session message, captures signature). JS tracks `pendingSessionCtx` across the cooldown → tx-confirm → sign-session flow and echoes it back in `session_signed`. The tx-hash input is the "manual paste" path today; the richer auto-detect and WalletConnect tabs layered on top are tracked as §5.1.

**Phase C - verification hardening `[complete]`**
- `session::verify_onchain(session, rpc_url)` (gated on `cooldown`) - fetches the activation tx receipt and confirms `status == true`, `receipt.to` matches `session.contract`, `receipt.block_hash` matches `session.activation_block_hash`. Each failure mode has a dedicated `VerifyError` variant (`MissingTxHash`, `MissingBlockHash`, `Rpc`, `ReceiptNotFound`, `TxReverted`, `ContractMismatch`, `BlockHashMismatch`)
- `session::should_reverify()` - Bernoulli gate (`rand::thread_rng().gen_range(0..5) == 0`) amortising the re-verify cost across cold starts
- `activation.rs::try_session_fast_path` now re-verifies tier-3 sessions (session_id present) on ~1 in 5 launches. `Rpc(_)` errors fall open (offline launches still work); verdict-contradicting errors fall closed (forged session → re-activate)
- Tx polling (already in Phase B): 30s total (10 × 3s), revert → user-facing error via the existing `onError` IPC path

**Verification**
- `cargo test` - 35 lib tests pass under default (tier-2); 39 pass under `--no-default-features --features tier-3` (adds 4 new tests: missing tx-hash, missing block-hash, bad-RPC transport, non-constant sampler); integration + license-e2e suites unchanged
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean
- Phase B `rpc` additions covered by pure tests: selector + calldata layout for `encode_activate_calldata(uint256)`, invalid-hash transport errors for `get_tx_receipt` and `get_block_number`
- Phase C anvil-gated integration test (`tests/session_onchain_e2e.rs`, `#[ignore]`): spawns `anvil`, deploys `Rub3Access` via `forge create`, runs `purchase(address)` + `activate(uint256)` via `cast send`, extracts the real block hash, and exercises `verify_onchain` on (a) the happy path, (b) a tampered contract field, (c) a tampered block hash, and (d) a non-existent tx hash. Gracefully skips when the Foundry toolchain is unavailable. Run with `cargo test -p rub3-wrapper --no-default-features --features tier-3 -- --ignored session_verify_onchain_e2e`
- Still to do separately from Phase C: end-to-end against anvil of the full connect → tx → sign → persistence-across-restarts webview flow (that belongs in §1.7's manual testing), cooldown enforcement path, short-TTL expiry re-activation, zero-contract legacy backward-compat test

### 1.9 - Tier scaffold + feature flags `[complete]`

Branch: `feature/tier-scaffold`. The wrapper is a single crate with Cargo features selecting compile-time behavior. Packing a distributable picks one tier bundle; orthogonal add-ons (e.g. binary encryption) compose independently. See `architecture.md` §Security Tiers for tier semantics.

**Tier bundles** (pick exactly one at pack time):

| Feature | Composed capabilities |
|---|---|
| `tier-0` | - |
| `tier-1` | `session` |
| `tier-2` (default) | `session` + `onchain-read` |
| `tier-3` | `session` + `onchain-read` + `onchain-write` + `cooldown` |
| `tier-4` | `tier-3` + `device-key` |

> **Amended by §2.1:** tier bundles are pure capability sets and no longer imply
> a front door. Compose one with `webview` (human) and/or `headless` (agent).
> The default build is `tier-2` + `webview`, unchanged in behaviour.

**Composable capability flags:**
- `session` - session schema + persistence (pulls `rand`)
- `onchain-read` - `ownerOf`, view calls
- `onchain-write` - calldata encoding, tx receipt polling
- `cooldown` - cooldown interval check
- `device-key` - ephemeral secp256k1 device keypair + storage (pulls `keyring`)
- `binary-encryption` - AES-256-GCM ciphertext unwrap + in-memory exec (pulls `aes-gcm`); orthogonal, composes with tier-3+

**Module scaffolds** (all `unimplemented!()` stubs behind `#[cfg(feature = "...")]`):
- `session.rs`, `session_store.rs` - gated on `session`
- `device.rs` - gated on `device-key`; `StorageBackend` = File | Keychain | Enclave
- `decrypt.rs` - gated on `binary-encryption`; KEK derivation, AEK unwrap, AES-256-GCM decrypt, in-memory exec (`memfd_create`/`fexecve` on Linux, `$TMPDIR` 0700 + unlink on macOS, `CreateFileMapping` on Windows)

All five tier bundles + `binary-encryption` composition compile clean. The 15 existing lib tests pass under default features. The scaffold establishes the wiring; tier 3 behavior is implemented in §1.8, tier 4 and binary encryption in later phases.

**Phase 1 deliverable:** A wrapped binary that requires wallet ownership + session signature to run, with session caching, on-chain cooldown enforcement (tier 3), and automatic re-activation on expiry.

---

## Phase 2: Agent-Native Core

Goal: an agent holding only a funded wallet key can purchase, activate, and launch a wrapped binary in one programmatic pass - and every contract deployed from here on carries the protocol's economics and ownership invariants.

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
- Leak containment, child process included: `supervisor::spawn` `env_remove`s every name in `agent_env::AGENT_ENV_VARS` (every `RUB3_AGENT_*` variable that carries or leads to the credential - the key, the keystore path, and both password sources, since a keystore path plus its password file is enough to decrypt the key) before launching the wrapped binary, unconditionally and in every bundle (the strip is not behind the `headless` feature, since what matters is that the child is never handed the credential however the wrapper was built), covered by tests that read the child's own reported environment for each signer source. The new `agent_env` module is the single authoritative list, read by `signer` and stripped by `supervisor`, so a variable added on one side cannot go missing on the other. Containment, not a sandbox: the child runs as the same UID and can still read what that user can read; hand-written `Debug` prints address + source and `<redacted>`; no `Display`, no `Serialize`; `SignerError` variants carry only fixed strings and at most a path - the underlying hex/keystore errors are dropped, not forwarded, because `hex::decode` names the offending character and keystore errors can distinguish a wrong password; no `unwrap`/`expect`/`panic!` anywhere on the production load path; decoded key bytes and the password buffer are zeroized after use

**`tx.rs` (new, gated on `headless`)** - calldata → broadcast, no key material
- `TxPlan { to, value, input }` and `send(rpc_url, signer, plan) -> tx_hash`: reads nonce + chain id + `estimate_eip1559_fees`, estimates gas with a 25% buffer, checks the balance covers `value + gas_limit × max_fee_per_gas`, signs the `TxEip1559` sighash via `Signer::sign_prehash`, and submits the 2718-encoded envelope with `eth_sendRawTransaction`
- `TxError::InsufficientFunds(Option<Shortfall>)` is raised both from the pre-flight balance check (which knows the numbers, and carries them) and by recognising the node's own "insufficient funds" wording (which does not, and carries `None`) - so the CLI returns the dedicated exit code either way, and the `rub3-detail:` line is omitted rather than reporting a shortfall of zero
- `Shortfall` carries a `Covers` discriminator, emitted as `required_covers=price|price_plus_gas`. The pre-flight check fires before `eth_estimateGas` (a wallet that cannot cover the value makes the estimate fail opaquely), so its figure excludes gas and says so rather than being reported under a "price + gas" label an orchestrator would top up against once and fail again

**`activation.rs`** - `ensure_headless` plus the shared fast path
- `pub mod activation` (was private) so `activation::ensure_headless` is the real path
- `ensure_headless(&dyn Signer, &HeadlessContext) -> (Session, HeadlessOutcome)`: cached-session fast path → chain-id guard → `tokens_of_owner` → (empty → `purchase()`) → `cooldown_ready` → `activate()` → `wait_for_receipt` → `draft_from_activation` → `personal_sign` → `verify_local` → `save_session`
- `HeadlessOutcome { Reused | Activated | PurchasedAndActivated { token_id, price_wei } }` - lets an orchestrator tell "launched from cache" from "spent money"
- `try_session_fast_path` gained `require_wallet` + `require_token` parameters and now returns the session rather than a bool. The interactive door passes `None` for both (whoever last activated on this machine is the user); headless passes its signer's address, so an agent never launches on a session belonging to a different key, and passes `--token-id` when given, so an explicit token constrains cache reuse exactly as it constrains purchasing. Both parameters **select** rather than filter. `require_token` loads that token's own `<token_id>.json` instead of taking `load_latest_session`'s newest-across-all-tokens result, so a signer holding several licenses still reuses the requested token's session when another token was activated more recently. `require_wallet` goes through `session_store::load_latest_session_for_wallet`, which narrows the same scan to sessions signed by that key before picking the newest, so a second agent (or a human who activated interactively later) writing a newer session under the same `app_id` cannot push a signer back on-chain for a session it already holds. The session's signed `token_id` is then checked against the requested one, since the session directory is user-writable and a filename proves nothing
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

The full code table - every code, its meaning, and what an orchestrator should do with it - is maintained in one place, `README.md` → "Exit codes", which mirrors `activation::EXIT_CODE_HELP`. §2.1 defined 0, 1, 2 and 10-21; later phases added 22 (§2.2) and 23 (§2.6).

**Tests** - lib tests 62 under the default bundle (up from 58), 72 under tier-3 (up from 62), 117 under `tier-3,headless`; wrapper e2e 7, all new. New unit coverage: `signer.rs` 20, `activation.rs` 18, `tx.rs` 7, `rpc.rs` 7 receipt-poll tests, `supervisor.rs` 3. `tests/headless_e2e.rs` is new (anvil-gated, `#[ignore]`, zero webview involvement - the test binary links neither `wry` nor `tao`), on port 8549 so it runs alongside `session_onchain_e2e.rs`, self-serialising through a file-level mutex so no `--test-threads=1` is required. Per-test inventory: `testing.md` → "Unit tests (in `src/`)" and "Headless (agent) E2E (`tests/headless_e2e.rs`)".

`RpcError` gained `InvalidInput` plus `is_retryable()` in the same pass, so a malformed argument is classified apart from a network failure instead of being labelled `Transport`.

**Verification**
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean, plus `headless` alone, `tier-3,headless` and `tier-2,webview`. CI's matrix covers both front doors as blocking jobs, so neither door can break green
- **No GUI in a headless build**: `cargo tree --no-default-features --features tier-3,headless` contains neither `wry` nor `tao`, nor any of the 20 GUI crates the webview build pulls (`cocoa`, `core-graphics`, `objc`, `dpi`, `raw-window-handle`, …). On macOS the release binary links no WebKit, AppKit, QuartzCore, or `libobjc` - only `Security`, `CoreFoundation`, `libiconv`, `libSystem`
- Anvil-gated headless e2e: 7 pass without `--test-threads=1`, and now run in CI as a second step of the existing `onchain-e2e` job. Existing `session_verify_onchain_e2e` still passes untouched
- `cargo clippy -p rub3-wrapper --all-targets -- -D warnings`: exit 0 on all eight bundles. The six warnings `main` carried are fixed at the root, so CI's lint job now runs clippy as a blocking gate

**Deferred to §2.2**
- USDC via EIP-3009. The headless purchase path shipped here ETH-only; §2.2 below, now complete, adds `purchaseWithAuthorization` and has headless prefer it when the contract advertises it

### 2.2 - USDC purchase via EIP-3009 `[complete]`

Machine money. Agents hold stablecoins, not ETH; card rails can't onboard them at all. Two rails now, and they mint identically: an agent holding only USDC obtains a licence, and the ETH path is byte-for-byte what it was.

**`receiveWithAuthorization`, not `transferWithAuthorization`** - `Rub3License.sol`

The spec named the transfer variant; the implementation uses the receive variant, and the difference is the whole safety story rather than a detail. Both carry the same six signed fields under different typehashes. `transferWithAuthorization` may be submitted by anyone *to the token*, so an attacker watching the mempool could push a buyer's authorization straight at USDC, moving the money into the licence contract **without** the mint - nonce burnt, buyer paid, no licence, no recovery. That is precisely the unrecoverable-funds failure the money path exists to prevent. `receiveWithAuthorization` requires `msg.sender == to`, and `to` is the licence contract, so the authorization is spendable *only* through `purchaseWithAuthorization`, which always mints. Payment and mint become inseparable. EIP-3009 added the variant for this exact attack and USDC implements it, so nothing is given up: anyone may still submit the purchase, which is what keeps it gasless for the buyer.

**The `bytes signature` overload, and the payment tokens that costs us** - `Rub3License.sol`

`PaymentAuthorization` carries an opaque `bytes signature`, and `IERC3009` declares the `receiveWithAuthorization(address,address,uint256,uint256,uint256,bytes32,bytes)` overload rather than the `(uint8 v, bytes32 r, bytes32 s)` form EIP-3009 specifies. **The token rail therefore requires a payment token that exposes Circle's FiatTokenV2_2-style `bytes signature` overload. A spec-conformant EIP-3009 token implementing only the `(v, r, s)` form is NOT supported** - it passes the constructor probe, which reads `authorizationState`, and then reverts for every buyer.

That is a deliberate trade, and the thing bought with it is smart-contract wallets. The `bytes` form validates through a signature checker - ECDSA recovery for a 65-byte EOA signature, falling through to EIP-1271 `isValidSignature` for a contract signer - so an ERC-4337 smart account buys a licence on the same single entry point an EOA uses. The split form can only ever serve an EOA. ERC-4337 accounts are how a growing share of agent wallets hold funds, agents are the buyers this rail exists for, and contract code is frozen at deploy, so shipping EOA-only would have excluded them permanently and pushed them back to the currency §2.2 exists to avoid. Narrower token support was judged the better price.

One entry point, not two: there is no `(v, r, s)` path alongside it, and the contract never inspects the signature, never branches on its length, and never recovers a signer. The widening happens inside the payment token, which is exactly where the priority-2 "one mint, two rails" rule wants it.

**The failure mode when a `priceToken` lacks the overload**

The contract cannot check it at deploy time. A staticcall probe cannot distinguish "no such function" from "bad signature" - both revert - so the probe would reject conforming tokens or accept non-conforming ones, and either mistake is frozen forever. `_setTokenPrice` keeps the `authorizationState` probe unchanged, with a comment saying why nothing should be added beside it.

Detection is the wrapper's, and it happens **before** broadcasting, so a misconfigured token costs no gas and ends no run: `choose_rail` pre-flights the exact `purchaseWithAuthorization` calldata as an `eth_call` from the account that would send it. A bytecode selector scan would not do - USDC sits behind a proxy, so scanning the token's runtime code for the overload's selector reports a false negative on the very token this rail targets. A contract-level failure selects ETH with a printed reason that leads with the revert the chain gave and offers the missing overload only as one possible cause, because the pre-flight executes the whole purchase and a blocklisted buyer or an exhausted supply cap reverts there too; a transport failure propagates as a hard error, as it does for every other token-side read.

The pre-flight needs a real signature, so it necessarily runs **after** the spend ceiling, never before it. That ordering is not negotiable: `purchaseWithAuthorization` is submittable by anyone, so an authorization for a refused amount handed to an RPC endpoint is the payment with extra steps. The accepted consequence is that "otherwise usable" cannot include the overload check - a token missing it but priced *within* the ceiling still falls back to ETH, while one both missing it and priced above the ceiling reports exit 22. `PriceAbovePolicy` therefore states the price and the ceiling and says explicitly that the rail was not exercised, so the code is never read as evidence the rail is healthy.

**Binding what EIP-3009 does not sign** - `Rub3License.sol`

The token signs `from`, `to`, `value`, `validAfter`, `validBefore`, `nonce`. The *mint recipient* is not among them, so left alone a submitter could pass their own address and take the licence with the buyer's money. rub3 derives the nonce instead of accepting it:

- `purchaseAuthorizationNonce(recipient, salt) = keccak256(tag, address(this), recipient, salt)`, and `Rub3Subscription.renewAuthorizationNonce(tokenId, salt)` with a different tag. A changed recipient (or token id) derives a different nonce, which yields a digest the buyer never signed, and the token rejects it
- Distinct domain tags (`rub3.PurchaseAuthorization.v1` / `rub3.RenewAuthorization.v1`) mean a purchase authorization can never be spent as a renewal, or the reverse
- `address(this)` sits in the preimage as well. The token already binds the contract through `to`; this makes the nonce worthless anywhere else regardless
- `value` is **not** a parameter - it is the listed price read at execution time. A buyer therefore cannot be charged more than their signature covers: if the price moved after they signed, the digest stops matching and the purchase reverts rather than overcharging
- Replay is the token's own single-use `(from, nonce)` accounting. On top of it, `_payWithAuthorization` measures the contract's balance before and after and requires it to have risen by the price, so a mint cannot happen unless the money actually arrived - true even against a payment token that fails silently
- `nonReentrant` (OZ `ReentrancyGuardTransient`) on both authorization entry points, because the payment token is an external call the owner configures

**One mint, two rails** - `Rub3Access.sol`, `Rub3Subscription.sol`

Duplicated mint logic is how the two paths would drift. `Rub3Access` gained a private `_mintPurchased(to, payer)` and `Rub3Subscription` a private `_mintSubscription(to, payer)`; `purchase` and `purchaseWithAuthorization` differ only in how payment is taken and who is named as `payer` (`msg.sender` on the ETH rail, `auth.from` on the other, where `msg.sender` may be a facilitator who paid nothing but gas). `renew` and `renewWithAuthorization` share `_extend` the same way. `_resolveAuthorizedRecipient` is deliberately *not* `_resolveRecipient`: a zero recipient means the buyer who signed, never the submitter.

**Sale terms and how the rail is advertised** - `Rub3License.sol`

- `address public priceToken` + `uint256 public priceAmount` alongside `price`, and `setTokenPrice(address,uint256)` mirroring `setPrice` - it moves what is *offered*, never what was granted
- Advertisement is on-chain by construction: the wrapper reads `priceToken()`. Zero, a revert (a contract deployed before §2.2 has no such function and no fallback), or empty return data all mean "ETH only" and are treated identically. A *transport* failure is not one of those and is propagated rather than silently falling back to the wrong currency
- A constructor/setter probe rejects a `priceToken` with no code or one that cannot answer `authorizationState`, so a contract cannot advertise a rail that reverts for every buyer; `IncompatiblePriceToken`. An amount with no token is rejected as `TokenPriceInconsistent`. A token with a zero amount is allowed - a free tier is legitimate and still takes the buyer's signature
- `withdrawToken(address,address)` alongside `withdraw`, or everything paid on the stablecoin rail would be stranded

**How the token-denominated renewal price relates to `renewPrice`** - `Rub3Subscription.sol`

They are **two independent snapshots of two independently listed prices, not a conversion**. The contract holds no oracle and cannot derive a USDC amount from a wei amount, so `renewPriceToken[tokenId]` and `renewPriceAmount[tokenId]` are written once at mint from the listed `priceToken` / `priceAmount`, at the same instant and by the same rule as `renewPrice[tokenId]` is written from `price` - the *listed* prices, not the amounts paid. Consequences, all deliberate:

- §2.4's "renewal terms are frozen per token" now covers both rails. `renewWithAuthorization` charges the snapshot, so a developer cannot reprice a held subscription in either currency
- A token minted while the contract offered no stablecoin rail carries none: `renewWithAuthorization` reverts `TokenPaymentUnavailable`. It renews in ETH, which every token always can, so no holder is ever stranded
- `_afterClaim` carries the predecessor's `expiresAt` and `renewPrice` across as before, and takes the stablecoin rail from *this* contract's listing rather than the predecessor's. `IRub3Predecessor` is the view slice frozen at §2.4 and a pre-§2.2 predecessor cannot answer for a rail it never had, so reading one across would brick the claim for exactly the holders migration exists to serve. Same shape as `period`: the successor's own terms govern what the carried price buys, and the claim is where the holder accepts them
- Events widened once more: `Purchased(tokenId, recipient, payer, expiresAt, renewPrice, renewPriceToken, renewPriceAmount)` and `Renewed(tokenId, expiresAt, priceToken, pricePaid)`, where a zero `priceToken` means the amount is wei

**Constructor: `SaleTerms`** - both concrete contracts

`price`, `priceToken`, and `priceAmount` are one `SaleTerms` struct rather than three loose arguments. Passing them loosely put `Rub3Subscription` one stack slot over solc's limit in the constructor ABI decoder; grouping also names the concept and keeps the two rails visibly parallel. `Rub3Access` stays at 10 constructor arguments and `Rub3Subscription` at 11. `forge create` takes the struct as a parenthesised tuple, `"(<price>,<priceToken>,<priceAmount>)"`. Threaded through `script/Deploy.s.sol` (new optional `PRICE_TOKEN` / `PRICE_AMOUNT`), all three Foundry fixtures, and both wrapper e2e tests.

The same two extra values pushed `Deploy.run()` itself over the stack limit, so the script now reads every input into one `DeployParams` struct and splits reading, deploying, and the summary into their own frames. Same env-var contract, same output; `forge build` compiles the script again.

**Leaving room for §2.3** - a shaping choice, not a framework

`_payEth` and `_payWithAuthorization` are now the only two places in the contracts where a payment is taken; every entry point delegates to one of them and then mints. The protocol fee splits what has just arrived, so §2.3 changes those two functions and nothing else - no entry point, no mint path, and no test fixture has to be restructured a second time.

**Wrapper** - `crates/rub3-wrapper/`

- `rpc.rs`: `stablecoin_rail` (the one `eth_call` that decides the rail), `erc20_balance_of`, `token_domain_separator`, `purchase_authorization_nonce`, the pure `receive_authorization_digest`, `encode_purchase_with_authorization_calldata`, and `preflight_purchase_with_authorization`. The licence's ETH price is `eth_price`, named for its currency so it cannot be reached for where a token amount belongs - the two prices sit side by side on the money path in different units. The EIP-712 domain is **read** from the token's `DOMAIN_SEPARATOR()` rather than rebuilt from name/version, so a signature cannot drift from whichever USDC version is actually deployed; the nonce is read from the licence contract for the same reason
- `activation.rs`: `choose_rail` prefers the stablecoin rail, but only once five things hold, **checked in this order**: the contract advertises one, the wallet holds at least `priceAmount` of it, the payment token's EIP-712 domain is readable, the operator's spend ceiling covers `priceAmount`, and only then - with the authorization now signed - the purchase pre-flights clean as an `eth_call` (which is what catches a token lacking the `bytes signature` overload). The order is load-bearing, not incidental - see the two bullets below. Anything short of all five buys in ETH with a printed reason naming the cause, except the ceiling, which refuses. The rail decision is made in one place: `choose_rail` reads the token's `DOMAIN_SEPARATOR()` itself and hands it back with the price, so nothing downstream can discover a reason the rail is unusable after the ETH path has been passed over. `authorize_purchase` signs the digest through the existing `Signer` trait, so a KMS backend serves the money path with no new capability
- **The spend ceiling, `RUB3_AGENT_MAX_TOKEN_AMOUNT`** - an integer in the payment token's own smallest unit, held on `SpendPolicy` with one `check_token_amount`. It exists because the two rails are independently quoted with no on-chain relation, so nothing bounds what an agent would otherwise authorize on a contract listing `0.001 ether` and `10000e6` USDC. **It must be set before the stablecoin rail is usable at all**: shipping a numeric default is not well defined across payment tokens, since the unit is the token's own and decimals differ (USDC 6, DAI 18), so any fixed number is wrongly scaled for some token; and the headless path already requires explicit `RUB3_AGENT_*` configuration before it can spend anything, so the ceiling joins a surface the operator has already met. Unset means the rail is *unavailable*, not unlimited: the run buys in ETH with a reason naming the variable as the configuration fact it is. A malformed value is a hard configuration error, never a silent zero and never a silent unlimited
- **Above the ceiling is a refusal, not a fallback** - `HeadlessError::PriceAbovePolicy` and **exit code 22**, with `rail=`/`listed=`/`maximum=`/`token=` on the detail line. An orchestrator has to tell "the price exceeded my policy" from "the network failed", so this never silently takes the ETH rail and is never retryable. Named for the general idea rather than for the stablecoin rail, which is what let the ETH ceiling land in §2.7 as another field on `SpendPolicy` and another call site rather than a second mechanism
- **The ceiling is checked after affordability and before anything is signed, and nothing may be moved in front of it.** The guard means "I would have paid this and policy says no". An agent that holds none of the payment token was never going to spend it, so an over-ceiling listing it cannot afford leaves it on the ETH path it was always on rather than ending the run - refusing there would break a run that succeeded before §2.2. But the ceiling must equally come before `authorize_purchase`: an authorization is submittable by anyone, so one that exists for a refused amount has already been spent as far as policy is concerned, whatever the process does next. The two outcomes stay distinct and must not be collapsed: the ceiling refusing the listed price is the loud non-retryable refusal; a rail the agent could not use at all is simply ETH, and its printed reason says which fact put it there without mentioning a spend limit
- **Contract-level failure on a token-side read selects ETH; transport failure stops the run.** `erc20_balance_of`, `token_domain_separator` and the `purchaseWithAuthorization` pre-flight select ETH with a printed cause when the *chain* answers - a payment token with no `DOMAIN_SEPARATOR()` getter is conforming EIP-3009 (the constructor probe only requires `authorizationState`), and an agent that could have bought in ETH must not be stopped by it. A *transport* failure on the same read is a hard error, because a blinking node must never silently change the currency. `purchase_authorization_nonce` stays a hard error on any failure: it lives on the licence contract, so an address that cannot answer it is not a rub3 licence contract at all, which is not a payment-rail question
- The choice is observable: `HeadlessOutcome::PurchasedAndActivated { token_id, paid }` carries `PaymentRail::Eth { price_wei }` or `PaymentRail::Erc3009 { token, amount }`, and `main.rs` already prints the outcome verbatim

**Test token: a mock, not a fork** - `contracts/test/mocks/MockEIP3009Token.sol`

`forge test` and the anvil-gated e2e both run with no network and no `.env`, so a fork test would make two green jobs depend on an RPC endpoint and a pinned block, and a fresh anvil has no USDC to buy with. What needs exercising is the *authorization protocol* - the EIP-712 domain, the canonical typehashes, `msg.sender == to`, single-use nonces - all of which EIP-3009 specifies rather than USDC. The mock implements exactly that surface with the same domain shape and typehash strings as Circle's FiatTokenV2 (each of the four constants checked against `cast keccak`), at 6 decimals, and deliberately models none of USDC's blocklist, pausing, or proxy, which the licence contracts never touch. Its authorization entry points take a `bytes signature` and validate it through OpenZeppelin's `SignatureChecker`, the same way FiatTokenV2_2 does, so both an EOA signature and an EIP-1271 contract signature go through the one path. It ships `transferWithAuthorization` too, so a test can prove that path is *not* usable against a receive-signed authorization. Companions: `SilentEIP3009Token` (answers the probe, moves no money), `NotAToken`, `NoDomainSeparatorEIP3009Token`, `NoSignatureOverloadEIP3009Token` (conforming EIP-3009, split-signature form only - the fixture for the pre-flight), and `SmartWallet`, an EIP-1271 wallet that defers to an owner key and implements `onERC721Received` so `_safeMint` can reach it.

**Ownership invariants (§2.4) swept in the same pass**

- Forbidden-selector list grows 25 → 27: `setRenewPriceToken(uint256,address)` and `setRenewPriceAmount(uint256,uint256)`, the setters that would unfreeze the new per-token snapshots (§2.3 takes it to 29). Every stated count updated together - `test/Rub3Invariants.t.sol`, the copy-pasteable loop in `contracts/contracts.md`, `architecture.md`'s bytecode table, and §2.4 below
- `test_audit_scannerIsSound` gained `purchaseWithAuthorization` as a positive control
- Both "owner does its worst" tests now also exercise `setTokenPrice` and `withdrawToken`; the subscription one deploys with both rails listed and asserts the held token's `renewPriceToken` / `renewPriceAmount` survive the owner repointing and sweeping everything
- No new external function is a revocation surface: none of them can change `ownerOf`, `isValid`, or `activate` for an issued token

**Tests** - 131 forge tests, up from 90; wrapper e2e 18, up from 7

- `test/Rub3TokenPurchase.t.sol` (new): 41 tests on the stablecoin rail, every one of them with the buyer holding stablecoin at a zero ETH balance and a separate submitter sending the transaction
- Wrapper side: 5 new `rpc` tests for the authorization typehash, digest and calldata; 8 `rpc::stub_node_tests` for the rail-detection classifier, driven against a local stub endpoint; 6 `activation` tests for `SpendPolicy`; 11 new anvil tests in `tests/headless_e2e.rs`, two of which relay every call to anvil except one token read whose connection they close unanswered, which is what actually exercises "a blinking node must never silently change the currency"
- All 90 pre-§2.2 forge tests pass with their bodies unchanged; only constructor fixtures moved, to the `SaleTerms` tuple
- Per-test inventory: `testing.md` → "Solidity suite" and "Headless (agent) E2E (`tests/headless_e2e.rs`)"

**Verification**
- `forge test`: 131 pass, 0 failed
- Wrapper matrix, all eight bundles: pass. `cargo clippy --all-targets -- -D warnings`: clean (and a latent `clippy::zombie_processes` failure in `tests/session_onchain_e2e.rs`, which only surfaces under a bundle CI does not run clippy on, fixed in passing)
- Anvil e2e: `headless` 18/18 including every new arm, `session_verify_onchain_e2e` 1/1
- The `cast` recipe now in `contracts/contracts.md` run verbatim against anvil: mock USDC + `Rub3Access` deployed, buyer signs with `cast wallet sign --data`, the *deployer* submits, and the buyer ends holding token 0 having spent 5 USDC and exactly zero wei
- Canonical bytecode fingerprints regenerated with `scripts/canonical-bytecode-hashes.sh update` on the forge version the `bytecode-fingerprints` job pins, and committed here as that gate requires. Both fingerprints moved, which a change to both contracts must; the recorded build block is untouched and the immutable-range counts are unchanged at 13 and 17, so the figures quoted in `contracts/contracts.md` still hold

**Deliberately not done**
- No hardcoded USDC address anywhere. `priceToken` is per-deploy and the wrapper reads it off the contract, so "which USDC deployment" is the developer's choice on any chain, not rub3's
- No `transferWithAuthorization` fallback for x402 clients that sign the other typehash. It would reintroduce the front-running hole and be a second payment path to drift; an x402 facilitator that speaks EIP-3009 can sign `ReceiveWithAuthorization` just as easily
- No fee split, no fee plumbing, no hook interface (§2.3 owns that, and landed on exactly the two functions this section shaped for it)
- The buyer is gasless, but the *wrapper* is not a facilitator: it signs the authorization and submits it itself, so it still needs gas for `purchaseWithAuthorization` and `activate`. What §2.2 removes is the need to hold ETH for the **price**. Fully gasless agents need a third-party submitter, which the contracts already permit and §4.2 builds


### 2.3 - Rub3Factory + protocol fee `[complete]`

The revenue mechanism, stamped at deploy time and immutable thereafter. Contracts only; the wrapper needed no change at all, which is itself the result - it buys from a fee-bearing contract with the same code that buys from a fee-free one.

**The fee lives on the licence contract, not on the factory** - `Rub3License.sol`

`uint16 public immutable feeBps` + `address public immutable treasury`, passed in as one `FeeTerms` struct and frozen at construction. The factory stamps its own values into every contract it deploys; a direct deploy passes `FeeTerms(0, address(0))` and carries no fee.

Reading the fee off a `factory` pointer at payment time was the alternative, and it is worse in both directions: it puts an external call on the money path, and it makes "immutable per contract" depend on the factory staying honest rather than on the deployed bytecode. As immutables, `feeBps()` and `treasury()` are what a buyer reads before purchasing and what that contract charges for as long as it exists. No setter exists on either side, and both setter names are now on the §2.4 forbidden list.

Validation splits by who is being protected. `Rub3License` rejects `feeBps > 10000` (`FeeBpsTooHigh`) - an arithmetic bound, so the fee can never exceed the payment it is taken from - and rejects a fee without a treasury or a treasury without a fee (`FeeTermsInconsistent`), because the first strands every buyer's money unreachably and the second advertises a claim on revenue that does not exist. The protocol's own 200-300 bps range is a rule about what *rub3* charges, so it lives on `Rub3Factory`, where it is checked in the constructor while the rate is still choosable. **The rate itself is a deploy-time decision and is not chosen anywhere in this repository**: `script/DeployFactory.s.sol` requires `FEE_BPS` with no default.

**The split runs on the amount received, not the listed price** - `Rub3License.sol`

`_accrueFee(token, amount)` is called from `_payEth` and `_payWithAuthorization`, the two functions §2.2 shaped for exactly this, and from nowhere else. §2.2's prediction held: no entry point, no mint path, and no test fixture had to be restructured a second time.

The fee is taken on what arrived, which on the ETH rail is the listed price itself - `_payEth` requires `msg.value` to equal it exactly - and on the stablecoin rail is the measured balance delta, with rounding by integer division in the developer's favour. Why those two properties are load-bearing: `architecture.md` → "Why the fee split is shaped this way".

**Accrued, not pushed** - `Rub3License.sol`

`uint256 public feesAccrued` and `mapping(address => uint256) public tokenFeesAccrued`; `withdraw` and `withdrawToken` pay the balance *less* the accrual, and new permissionless `withdrawFees()` / `withdrawTokenFees(address)` sweep the accrual to `treasury`. The two balances are disjoint and neither side can reach the other's, in either withdrawal order.

Transferring the fee inside `purchase()` was the obvious alternative and it is unfixably fragile: `treasury` is immutable, so a recipient that reverts on receipt - or that one day costs more gas than a buyer sent - would break every purchase on that contract forever. Accruing keeps the buyer's money path free of calls out, and a collection failure becomes rub3's problem rather than the buyer's. `test_accrual_rejectingTreasuryCannotBlockPurchases` is that scenario end to end: buyers still buy, the developer is still paid in full, and only rub3's own sweep fails.

**`Rub3Factory`, and why it deploys through two helper contracts** - `Rub3Factory.sol` (new)

`deployAccess(Rub3LicenseParams)` and `deploySubscription(Rub3LicenseParams, uint256 period)`, both recording `isDeployed[license] = true` plus an insertion-ordered `deployments()` list, so the registry (§3.2) and marketplace (§4.3) can enumerate the canonical set without replaying logs. `MIN_FEE_BPS` / `MAX_FEE_BPS` are `constant` (200 / 300) and `feeBps` / `treasury` `immutable`. The factory has no owner, no admin, and no way to touch or un-record anything it deployed - a listing that could be withdrawn would be a revocation surface pointed at the registry.

A single factory **cannot** `new` both licence contracts: a contract's runtime code carries the creation code of everything it deploys, and the two are 16.7 KB + 18.4 KB against a 24,576-byte runtime limit. A `new` reached only from a *constructor* lands in the creation code, which is discarded after deployment, so the factory builds one `Rub3AccessDeployer` and one `Rub3SubscriptionDeployer` in its own constructor and holds their addresses as immutables. Its runtime is then 3.3 KB. Its initcode is 42.0 KB against EIP-3860's 49,152, and `test_factory_initcodeFitsUnderEip3860` guards that margin because the first sign of trouble would otherwise be an undeployable factory on mainnet. The three consequences for auditors and callers are set out in `contracts/contracts.md` → "Why the factory deploys through two helper contracts".

**A factory deploy may only succeed a canonical predecessor.** `deployAccess` / `deploySubscription` revert `PredecessorNotCanonical(address)` unless `params.predecessor` is `address(0)`, this factory's own deployment, or one recorded by a factory reachable through the immutable `previousFactory` chain (bounded at `MAX_PREDECESSOR_FACTORY_HOPS`, 8). Without it, `claimFromPredecessor` being free - as it must be - lets a whole fee-free sale be laundered onto a registry-listed contract. Direct deploys and the deployer helpers are untouched, because neither grants an `isDeployed` row. Full statement, including what a pre-factory contract can and cannot do, in `contracts/contracts.md` -> "A factory deploy may only succeed a canonical predecessor".

**Constructor: `IdentityTerms`, forced by the stack** - both concrete contracts

`FeeTerms` was a twelfth argument on `Rub3Subscription` and put it back over solc's stack limit in the constructor ABI decoder - the exact wall §2.2 hit and solved with `SaleTerms`. `identityModel` and `tbaImplementation` are now one `IdentityTerms` struct, which is the right grouping independently: the constructor already requires a TBA implementation for the account model and forbids one for the access model, so "which model" and "which implementation" were always one decision. `Rub3Access` is back to 10 constructor arguments and `Rub3Subscription` to 11. `forge create` takes it as `"(<identityModel>,<tbaImplementation>)"`, alongside the `SaleTerms` and `FeeTerms` tuples.

Threaded through `script/Deploy.s.sol`, all four Foundry fixtures, and both wrapper e2e tests. The deployer contracts take their params as `memory` rather than `calldata` for the same stack reason.

**`script/Deploy.s.sol` and `script/DeployFactory.s.sol`**

- New optional `FACTORY` on `Deploy.s.sol`: set it and the deploy goes through that factory (fee-stamped and recorded), leave it and the deploy is direct (no fee, unrecorded). The fee is not an input either way - the factory reads it off itself. The summary prints which path was taken and the stamped terms
- New `DeployFactory.s.sol` takes `FEE_BPS` and `TREASURY`, both required. `FEE_BPS` deliberately has no default: it decides rub3's take for every contract that factory will ever deploy

**Ownership invariants (§2.4) swept in the same pass**

- Forbidden-selector list grows 27 -> 29: `setFeeBps(uint16)` and `setTreasury(address)`, the setters that would unfreeze the economics of every contract a factory ever deployed. Every stated count updated together - `test/Rub3Invariants.t.sol`, the copy-pasteable loop in `contracts/contracts.md`, `architecture.md`'s bytecode table, and §2.4 below
- The audit now runs against **four** targets rather than three: `Rub3Factory` joins the two licence contracts and the successor, because the factory is where the terms are chosen and a setter there would unfreeze them just as surely
- `test_audit_scannerIsSound` gained `feeBps()` and `treasury()` as positive controls - the getters exist while every setter for them is absent, which is the shape "immutable per contract" has in the bytecode
- `architecture.md`'s convention table loses a row: "the protocol fee is immutable per deploy" moves from convention to bytecode, which is the whole point of putting the terms on the licence contract

**Tests** - 174 forge tests, up from 131; wrapper e2e 21, up from 18

- `test/Rub3Factory.t.sol` (new): 43 tests in seven groups - the factory itself, immutability across factory versions, exact ETH arithmetic at the boundaries including a 256-run fuzz over amount x rate, the same on the stablecoin rail, `test_bothRails_chargeIdenticallyForTheSameAmount` (the one-rule-two-rails claim as an equation), direct deployment, constructor validation, and the accrual rationale
- Fee evasion is pinned from both sides: `test_eth_feeIsChargedOnWhatArrivedNotOnTheListedPrice` and `test_eth_zeroPriceListingCannotAvoidTheFee`
- `tests/headless_e2e.rs`: 3 new anvil tests - a factory-deployed contract completing a real purchase on each rail with the split settled and nothing stranded, plus the counterweight of a direct deploy selling identically and reporting `isDeployed == false`
- All 131 pre-§2.3 forge tests pass with their bodies unchanged; only constructor fixtures moved, to the `IdentityTerms` and `FeeTerms` tuples
- Per-test inventory: `testing.md` → "Solidity suite" and "Headless (agent) E2E (`tests/headless_e2e.rs`)"

**Verification**
- `forge test`: 174 pass, 0 failed
- **Mutation-tested**: 14 deliberate regressions applied one at a time, each caught by a named test - neither rail taking a fee, either withdrawal ignoring the accrual, the fee charged on the listed price instead of what arrived, rounding flipped to favour the protocol, the fee pushed to the treasury on the money path, the factory's range check removed, the factory not recording its deploys, the factory stamping no fee, both constructor validations dropped, and `feeBps` / `treasury` made mutable with a setter (caught by the audit). No mutation survived
- Wrapper matrix, all eight bundles: pass. `cargo clippy --all-targets -- -D warnings`: clean
- Anvil e2e: `headless` 21/21 including the three new arms, `session_verify_onchain_e2e` 1/1
- `contracts/canonical-bytecode.json` regenerated with `scripts/canonical-bytecode-hashes.sh update`; the blocking `check` gate passes. Five contracts are now fingerprinted, up from two
- The audit snippet in `contracts/contracts.md` run verbatim against a live factory deployment: all 29 forbidden selectors absent, positive control found (the canonical-predecessor rule later took the list to 30)

**Deliberately not done**
- **No fee anywhere but `purchase()` and `renew()`.** Deploys are free (the factory charges nothing and the developer pays only gas), and the CLI, SDK, and wrapper are untouched. There is no token
- No un-record, no delist, no owner on the factory. A listing that could be withdrawn is a revocation surface pointed at the registry, and the factory holding privilege over its deploys would undo the thing the fee's immutability is for
- No fee on secondary transfers. The marketplace (§4.3) is its own mechanism with its own fee; ERC-721 transfer stays untouched, because a royalty hook on `_update` would be a call the holder cannot avoid on a token they already own
- No discount, override, or exemption path. One rate per factory, applied to everything it deploys - anything else is a mutable fee wearing a different name
- **No charge on value that did not arrive through a payment function.** A direct ERC-20 transfer, a `selfdestruct` beneficiary, or a coinbase payout is never accrued against, and `withdraw` / `withdrawToken` release it whole to the developer. Accepted on the economics, not left open by accident; the argument is in `architecture.md` → "Why the fee split is shaped this way" and the scope statement in `contracts/contracts.md` → "What the fee covers, and what it does not", pinned by `test_token_unaccruedBalanceSweepsEntirelyToTheDeveloper`
- **The fee does not go live ahead of the registry.** The registry (§3.2) and marketplace (§4.3) the fee is intended to buy are not built, so today the factory row is a durable canonical record and nothing more. The contracts are not deployed to mainnet or declared ready for use until the registry is ready: the factory and the registry launch together

### 2.4 - Ownership invariants `[complete]`

"The token is the invariant; everything else is versioned" - encoded in bytecode, not policy. Contracts only; no wrapper-side change beyond the e2e fixture.

**Append-only wrapper hash set** - `Rub3License.sol`
- `enum HashStatus { Unknown, Valid, Revoked }` + `mapping(bytes32 => HashStatus) public wrapperHashes` + `mapping(bytes32 => string) public revocationReason`, backed by a private insertion-ordered `_wrapperHashList`
- `addWrapperHash(bytes32)` (onlyOwner) - rejects `bytes32(0)` (`ZeroWrapperHash`; it is the `Unknown` sentinel) and any hash already recorded, valid *or* revoked (`WrapperHashAlreadyKnown`). Status is monotone `Unknown → Valid → Revoked` with `Revoked` terminal, which is what makes the set auditable as append-only; a mistaken revocation is corrected by publishing a fresh build
- `revokeWrapperHash(bytes32, string)` (onlyOwner) - requires status `Valid` (`WrapperHashNotValid`) and a non-empty reason (`RevocationReasonRequired`), so a compromised build is flagged on-chain *with the reason stated*
- Views for pre-purchase audit: `isWrapperHashValid`, `wrapperHashCount`, `wrapperHashAt`, `wrapperHashList`
- **Removed**: `bytes32 public wrapperHash`, `setWrapperHash(bytes32)`, `WrapperHashUpdated`. Rotating a single slot retroactively strips verifiability from every binary already downloaded - and one release ships several binaries anyway, one per platform
- Revoking a *binary hash* is structurally unable to reach *token validity*: `ownerOf`, `isValid`, and `activate` never read the set

**Successor pattern** - `Rub3License.sol`
- `address public immutable predecessor` (frozen at deploy - "whose holders do I honor" is part of what a buyer audits, so it must not move after they have looked) and `address public successor` + `setSuccessor(address)` (onlyOwner, rejects self)
- `claimFromPredecessor(uint256) returns (uint256)` - requires this contract declared a `predecessor`, that the predecessor's owner pointed `successor` here, and that `msg.sender` currently holds the predecessor token. Bookkeeping: `wasClaimed`, `claimedFromTokenId`, `predecessorTokenClaimed` (one claim per predecessor token)
- **Snapshot-claim, not burn-to-mint** - burn-to-mint would require the predecessor to expose a burn, which is exactly the revocation surface that must not exist. The old token is neither destroyed nor moved; the holder ends up with both
- `_afterClaim(tokenId, predecessorTokenId)` virtual hook; `Rub3Subscription` overrides it to carry the migrating holder's `expiresAt` *and* snapshotted `renewPrice` across. Reads go through `IRub3Predecessor`, the view-only slice a successor is allowed to touch, so migration can never disturb the old contract
- **`period` does not carry across.** It is immutable per contract, so the successor's own `period` governs what the carried price buys from then on; a successor declaring a shorter period raises the effective rate without the price moving. Nothing granted is taken, because claiming is opt-in and holder-initiated and the original token keeps validating on the old contract at its original terms forever. The practical protection is that **a holder reads the successor's `period` and `price` before claiming** - the claim is the moment they accept the successor's terms - and a holder who dislikes them does not claim. Documented in `architecture.md` under the successor pattern
- `honorsContract(address configuredContract, uint256 tokenId) view returns (bool)` - the contract-side trust rule a wrapper can evaluate in one `eth_call`, offered by the contract and **not yet consumed by any shipped wrapper** (it is absent from the `sol!` interface in `crates/rub3-wrapper/src/rpc.rs`, and the wrapper still checks ownership only against its single hardcoded `CONTRACT`): true when this contract *is* the configured contract, or is its declared predecessor's successor holding a token claimed from it. A token *bought* on the successor is not a claim, so a wrapper pinned to the old contract rejects it - which is how a paid major version ships (deploy with no `predecessor`, still signpost via `successor`)
- **Migration can duplicate a seat, and that is accepted.** The v1 token stays live and sellable after the claim while the v2 token stays honored, so honored seats are not bounded by either contract's `supplyCap`. Bounding it would need the predecessor to invalidate the old token, which is the revocation surface, so the consequence is documented in `architecture.md` and `contracts/contracts.md` and deliberately not bounded in code
- **Mint ordering is checks-effects-interactions.** `_mintNext` is split into `_reserveNextId()` (supply check + id allocation) and the `_safeMint`. `claimFromPredecessor` and `Rub3Subscription.purchase` write every per-token mapping against the reserved id *before* minting, so a contract recipient's `onERC721Received` can never observe a token that exists with default terms. Not exploitable for value today, but §2.2 and §2.3 rewrite `purchase`/`renew` on exactly this shape
- The predecessor's opt-in is checked once, at claim time, and recorded permanently. Re-reading it on every `honorsContract` call would let a later `setSuccessor` retroactively unmake a claim already made - a grant, revoked
- New events `WrapperHashAdded`, `WrapperHashRevoked`, `SuccessorUpdated`, `Claimed`; new errors `ZeroWrapperHash`, `WrapperHashAlreadyKnown`, `WrapperHashNotValid`, `RevocationReasonRequired`, `SelfReference`, `NoPredecessor`, `SuccessorNotDeclared`, `PredecessorTokenAlreadyClaimed`

**Per-token renewal snapshot** - `Rub3Subscription.sol`
- `mapping(uint256 => uint256) public renewPrice`, written once by `purchase()` from the listed `price`, which on the ETH rail is also the only amount that can have been paid: `_payEth` takes the exact price, so what a buyer sent cannot inflate what they renew at. No setter exists. §2.2 adds `renewPriceToken` / `renewPriceAmount` under the same rule
- `renew()` charges `renewPrice[tokenId]`, never the current `price`, and now calls `_requireOwned` before reading the snapshot
- **Constructor probes the predecessor, in two layers.** `predecessor` is immutable, so an address that cannot answer what the claim path reads would brick every holder's claim forever with redeployment the only remedy. `Rub3License` rejects a non-zero predecessor that has no code or cannot answer `successor()`, the base read slice; each concrete contract then layers on a model check over the same discriminator, `period()`. `Rub3Subscription` requires the predecessor to answer it, plus `expiresAt(0)` and `renewPrice(0)`, the two getters `_afterClaim` actually reads (both are mapping getters, so they answer `0` for any id and need no minted token); `Rub3Access` requires it to *fail*, because an access license carries nothing across in `_afterClaim`, so a subscription predecessor would let any subscriber - including one lapsed years ago - mint a perpetual license for free. Cross-model succession is therefore impossible by construction, in both directions. All layers revert `IncompatiblePredecessor(address)`, declared on the base. The probe deliberately does not read `ownerOf` (it reverts for an unminted id on a valid predecessor) and does not assert the *value* of `successor()` (the predecessor points here only after this deploy; `claimFromPredecessor` still enforces the value at claim time)
- `period` was already immutable; together the two are the whole of "renewal terms are frozen per token". The freeze cuts both ways - a price *cut* does not reach held tokens either. A developer who wants to pass one on deploys a successor and lets holders claim onto it
- Events widened: `Purchased(tokenId, recipient, payer, expiresAt, renewPrice)`, `Renewed(tokenId, expiresAt, pricePaid)`

**Constructor change** - `bytes32 wrapperHash_` becomes `bytes32[] memory wrapperHashes_` (seeds the set; empty is valid), and `address predecessor_` is added before `owner_`. `Rub3Access` now takes 10 args, `Rub3Subscription` 11. Threaded through `script/Deploy.s.sol`, both Foundry test fixtures, and `crates/rub3-wrapper/tests/session_onchain_e2e.rs`.

**`script/Deploy.s.sol`**
- New `WRAPPER_HASHES` (comma-separated, via `vm.envOr(name, delim, default)`); `WRAPPER_HASH` retained as the single-hash shorthand, with a zero or absent hash deploying an empty set. Neither is required any more
- New optional `PREDECESSOR` (default `address(0)` = accepts no migrations)
- Summary prints the seeded hash list and the predecessor

**No-revocation audit** - `test/Rub3Invariants.t.sol`
- `_bytecodeHasSelector` scans deployed runtime code for a selector constant (solc emits each external function's selector as a literal `PUSH4` in the dispatcher, so absence from the code is absence from the ABI). `_assertNoFunction` asserts absence two independent ways: not in the bytecode, *and* a raw call carrying the selector reverts (there is no fallback to swallow it)
- 30 forbidden signatures × 4 deployed contracts (`Rub3Access`, `Rub3Subscription`, a successor `Rub3Access`, and the §2.3 `Rub3Factory`) - 25 at §2.4, plus the two §2.2 added for its own per-token snapshots, the two §2.3 added for the fee terms, and `setPreviousFactory(address)` for the factory chain the canonical-predecessor rule walks: burn, admin transfer / seizure, pause, direct invalidation of a token or its terms (including `setPeriod(uint256)`, whose absence is what keeps the renewal term a held token buys frozen), proxy/upgrade hooks, the removed `setWrapperHash` and any way to rewrite the set, forced migration, `setPredecessor` and `setPreviousFactory`, and the fee setters `setFeeBps` / `setTreasury`
- A positive control (`test_audit_scannerIsSound`) proves the scanner finds selectors that *do* exist and that an unknown selector really reverts - without it the absence assertions prove nothing
- Behavioural companions: the contract owner cannot `transferFrom` / `safeTransferFrom` / `approve` a token it does not hold; and an "owner does its worst" test runs every owner-only function that exists (max out the price, add a hash, revoke every hash, repoint the successor, drain the balance, hand ownership to an attacker, who repeats it and then renounces) and asserts `ownerOf`, `isValid`, `activate`, `renewPrice`, and transfer all survive

**Tests** - 90 forge tests, up from 33

- `test/Rub3Invariants.t.sol` (new): 50 tests in four groups - 18 on the hash set, 16 on succession, 11 on mint ordering and predecessor typing, 5 on the no-revocation audit
- `test/Rub3Subscription.t.sol`: 7 new on the per-token `renewPrice` snapshot, including the reverse proof that a price cut does not reprice a held token
- `test/Rub3Access.t.sol`: 2 rewritten in place - `test_setWrapperHash_onlyOwner` → `test_addWrapperHash_onlyOwner` (same owner-gating intent, plus "appended, not replaced"), and `test_metadata` now asserting hash-set status, count and order. The other 24 unchanged
- Per-test inventory: `testing.md` → "Solidity suite (`contracts/`, run with `forge test` from `contracts/`)"

**Verification**
- `forge test`: 90 pass (26 Access + 14 Subscription + 50 Invariants), 0 failed
- **Mutation-tested**: 13 deliberate regressions applied one at a time to `src/`, each caught by the named test - old contract refusing to validate once a successor is set (G1), the contract owner being allowed to push a migration (G2), the trust rule re-reading the live successor pointer (G3) or honoring unclaimed successor tokens, a resurrectable revoked hash, hash status reaching `isValid`, `renew` charging the current price, reason-less revocation, dropping the `Rub3Access` model probe so a subscription predecessor deploys, and re-adding `setWrapperHash` / `burn` / `pause` / `setPredecessor`. No mutation survived
- Anvil round-trip with `cast`: deploy v1 + v2, claim rejected before `setSuccessor` and accepted after, v1 token still owned + activatable afterwards, `honorsContract` true on both arms and still true after v1 repoints its successor away
- `script/Deploy.s.sol` broadcast against anvil in all three hash configurations (`WRAPPER_HASHES` multi, `WRAPPER_HASH` shorthand, none) plus `PREDECESSOR`
- The audit snippet documented in `contracts/contracts.md` run verbatim against a live deployment: all forbidden selectors absent, positive control found. The snippet, `architecture.md`'s bytecode table and `test_audit_noRevocationSurface` name one identical set
- `crates/rub3-wrapper/tests/session_onchain_e2e.rs`: passes against anvil with the new 10-arg constructor

**Deliberately not added** - each of these would be a revocation surface: an emergency pause, an owner burn, an admin transfer, an un-revoke for the hash set, a settable `predecessor`, a `renewPrice` setter, and any kill switch able to stop a wrapper binary already running. The honest limit stands: hash revocation informs new downloads and activations only.

**Docs** - `architecture.md` North Star mutability table corrected (`supplyCap` is immutable, so supply cannot change at all; added the other immutables) and its Ownership-invariants section rewritten with the successor pattern and a **bytecode vs convention** breakdown naming what an agent can verify before buying and what remains a promise (registry and factory properties, both unbuilt; a revoked binary already running; the developer continuing to publish). `contracts/contracts.md` gains hash-set management, the migration runbook, and the copy-pasteable pre-purchase audit. Both docs also state the duplicate-seat consequence of snapshot-claim plainly, and the audit snippet's conclusion claims only what a selector-name scan can prove: full assurance needs a name-independent comparison of the deployed runtime bytecode against the canonical template. **Built in §2.6** - the wrapper performs exactly that comparison before it buys, and the snippet is now labelled a diagnostic in every doc that carries it. `README.md` moves §2.4 out of "not yet implemented" and matches the corrected mutability table. `AGENTS.md` now points at `.github/workflows/ci.yml` instead of claiming the repo has no CI.

### 2.5 - rub3 CLI `[not started]`

Pulled forward from the old Phase 2 - a CLI is the natural agent interface, and every step is already scriptable.

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

**`pack` compiles `contracts/deployments.json` into the wrapper.** `--chain base` resolves to a chain id, and the canonical `Rub3Factory` for that chain is read out of that file and baked into the packed binary's constants alongside `CONTRACT` and `CHAIN_ID`, so a wrapper can tell a canonical deploy from any other without a network round trip or a hardcoded address in Rust. A `null` entry - which is every entry until launch - is a hard error from `pack`, never a fallback to the zero address: a distributable that claims a canonical factory it cannot name is worse than one that refuses to build. Same rule for `deploy`, which resolves `FACTORY` the same way.

### 2.6 - Pre-purchase contract attestation `[complete]`

"Is this contract the code I think it is?", answered before the agent spends anything. Wrapper only; no Solidity change. Closes §2.4's standing gap - the selector scan is a blacklist of names, and the name-independent comparison it pointed at was unbuilt.

**The comparable quantity is the masked code hash** - `crates/rub3-wrapper/src/attest.rs`
- Plain runtime-bytecode equality does not work: Solidity immutables are written into the runtime code at deploy time, so two deploys of identical source that chose a different `supplyCap` return different code from `eth_getCode`. Zeroing the immutable ranges first makes the comparison a function of compiled semantics alone
- The compiler already emits that form as `deployedBytecode.object`, which is what `contracts/canonical-bytecode.json` fingerprints, so the agent-side computation is "fetch code, zero the published ranges, `sha256`" and the two sides need no separate derivation to agree
- **The masked bytes cannot hide code.** Every immutable range is the immediate operand of a `PUSH32`, and EVM jump-destination analysis excludes bytes inside push immediates, so no control flow can reach one as an instruction. A match is a complete statement about the contract's executable code, not a partial one
- **`SELFDESTRUCT` cannot defeat it.** `evm_version = "cancun"` and Base has been on Cancun since Ecotone, so under EIP-6780 code at an address cannot be destroyed and replaced between the read and the purchase. Pre-Cancun the whole design would have been defeatable

**The pipeline** - `activation.rs`, `headless::purchase`
- One `eth_getCode`. Everything else runs on bytes already in hand, so the common case costs a single round trip and works on a degraded network
- Masked hash against `attest::CANONICAL`, the table compiled into the binary. Hit on a `Role::Licence` entry means canonical, and the run proceeds. The two `Role::Deployer` helpers and `Role::Factory` are pinned too - the table mirrors the manifest exactly - but buying from one is refused as the wrong address, with its own detail line
- Miss means refuse. `HeadlessError::NotCanonicalContract`, exit code 23, nothing signed. An on-chain `Rub3CodeRegistry` lookup for releases newer than the binary's table slots in between the miss and the refusal without restructuring anything; it is not built here
- **It runs first in `purchase`, not merely before `tx::send`.** `choose_rail` signs an EIP-3009 authorization and hands it to the RPC endpoint as pre-flight calldata, and anyone may submit a `purchaseWithAuthorization`, so disclosure is the spend. A refusal arriving after that has already paid. Same ordering rule as the §2.2 spend ceiling, for the same reason

**Failure posture: closed on purchase, open on launch**
- Refusing to spend money on code that could not be verified is correct, so a chain read that fails is also a refusal
- Refusing to *start* a program the user already paid for because a check could not complete would be a de-facto revocation surface, which §2.4 rules out. Nothing on the launch path consults the module - there is no shared helper and no flag, and `the_attest_module_is_reachable_only_from_the_purchase_path` walks `src/` and fails if any module outside the purchase-path allowlist names the module at all, by any of its items. The allowlist is the paths that spend money, `activation.rs` and `webview.rs`; `webview.rs` is named ahead of the work because `show_purchase` still hands `purchase(recipient)` calldata to a human wallet unattested, and gating it later should not have to argue with a test. A second assertion pins `activation.rs` to exactly one call site, since a subset is also satisfied by calling the gate nowhere. A third covers what a file-granular allowlist cannot: `webview.rs` holds the launch path as well as the purchase one, so the named human launch entry points in it (`show_activate`, `show_cooldown`, `finalize_session`) are asserted to reference the module not at all, and the assertion fails loudly rather than vacuously when one of those functions can no longer be found. The guard is honest about its limit rather than total: a new launch function in `webview.rs` is unguarded until somebody names it, and a reference elsewhere in `activation.rs` is not caught either. That test guards source structure, not runtime wiring; the behaviour is pinned by `headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e`, which relaunches a held licence against anvil and asserts it activates without minting

**The selector scan is demoted to a diagnostic**
- `attest::FORBIDDEN_SIGNATURES` mirrors the 30 signatures `test/Rub3Invariants.t.sol` asserts absent, searched over raw bytes rather than hex text so an odd-nibble coincidence cannot invent a finding
- Its only job is the message: `contract exposes seize(uint256)` instead of `unrecognised code`. It proves nothing by its silence, and every doc that described it as assurance now says so. A unit test compares it against the Solidity array so this fifth copy of the list fails loudly instead of rotting

**Drift protection** - the published record has to stay true
- `pinned_table_mirrors_the_canonical_manifest` asserts every fingerprint and every immutable range in `contracts/canonical-bytecode.json` is pinned in `attest::CANONICAL`, and prints the row to add when it is not. The blocking `bytecode-fingerprints` CI job already guarantees the manifest matches the contracts; this extends the chain to the binary, and it runs in every tier-2-and-up matrix job
- **Entries accumulate rather than being overwritten, from the first deploy onwards: once a contract is deployed at a fingerprint, its row is permanent**, because that contract goes on selling and validating its own tokens forever however far its source moves on
- That rule has nothing to protect until then, and nothing is deployed to any public network yet (§1.5): the contracts do not reach mainnet or get declared ready for use until the registry ships, and the factory and the registry launch together (§2.3). A superseded row before the first deploy guards no holder while widening the set of code the wrapper will spend money on, so the table carries exactly one row per contract today - the pre-exact-payment rows of the §2.3 contracts were dropped on that ground while the condition was still false, which is a one-off and not licence to prune the table once a fingerprint is live
- `bytecode_hash = "none"` (already pinned) is what makes any of this reproducible. Under solc's default a stray comment or a renamed source directory moves every fingerprint

**What this does not prove**, stated in `README.md`, `architecture.md` and `contracts/contracts.md` rather than implied away
- Nothing about the masked values. Byte-identical canonical code with `identityModel == 1` and `tbaImplementation` pointing at an attacker's ERC-6551 implementation matches. Reading the getters against a buyer policy is separate and unbuilt
- Nothing about how a canonical contract's owner uses the powers §2.4 deliberately keeps
- Nothing without an honest RPC. A single endpoint that lies returns canonical code for a hostile contract; the claim is "an honest view of chain state implies canonical code" and no more. A read quorum would close it and is not built
- A miss is not an accusation: a contract from a newer template release than the binary was packed with presents identically

**Files** - `attest.rs` (new), `lib.rs` (`pub mod attest;` gated on `onchain-read`), `rpc.rs` (`get_code` via `provider.get_code_at`), `activation.rs` (the call site, the error variant, exit code 23 and its help text)

**Tests** - 15 unit tests in `attest`, 4 in `activation`, 1 in `rpc`, 2 anvil-gated e2e
- The negative case is executable: a copy carrying `reconcileLedger(uint256,address)` - an owner-only seizure under an innocuous name - is asserted to pass the selector scan in silence and to fail the hash. That asymmetry is the whole justification for the work
- Also covered: a legitimate deploy that chose different immutables still matching, a truncated deploy refused rather than partially masked, an address with no code, the refusal naming what the pre-filter saw, the role check, and the pinned table's shape
- `headless_refuses_a_contract_whose_code_is_not_canonical_e2e` drives the refusal against a real deployed non-rub3 contract on anvil and asserts the signer's nonce and ETH balance are both unchanged - the executable form of "no transaction was sent"
- `headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e` is the other half of the posture: buy once through the gate, wipe the cached session so the fast path cannot answer, mine past the cooldown, and relaunch → `Activated` rather than `PurchasedAndActivated`, with `nextTokenId` unchanged read through `cast`
- Full 8-bundle matrix green. `--lib` counts move to `tier-0` 46, `tier-1` 76, `tier-2` 91, `tier-3`/`tier-4` 101, `tier-3,headless` 156. `tier-0` and `tier-1` gain only the one `rpc::get_code` error-path test and none of `attest`'s fifteen, which is the module compiling away as required

**Fixed in passing** - `tests/license_e2e.rs` had a real flake, roughly one run in sixty: `static_license_loads_and_verifies` and `dynamic_license_round_trips` both set and cleared the process-global `RUB3_LICENSE_DIR`, so one test's `remove_var` could land between the other's `set_var` and its read, failing it with an unrelated `NotFound`. Both now load through one helper holding a file-level lock across the whole set -> read -> unset window. 40 consecutive runs clean.

**Deliberately not built here** - the on-chain `Rub3CodeRegistry` (a separate deploy, and the answer to "several legitimate releases live at once"), a signed release manifest fetched over HTTP (would add an HTTP client and a signature dependency to a crate with neither, to avoid a chain the agent is about to transact on anyway), and the immutables-versus-policy check, which is the one gap a fingerprint structurally cannot close.

One test gap is left open for the same reason. "A launch still works when the check *cannot complete*" has no executable coverage: constructing it needs a licence contract whose code is deliberately not canonical, which is a Solidity change, and `contracts/` was left alone here because a separate lane is editing it. What is covered is the structural half - a held licence relaunches without entering the purchase path at all - and the unbuilt half is follow-up work for whenever a non-canonical licence fixture can land alongside it.

### 2.7 - ETH spend ceiling `[complete]`

The other half of §2.2's spend policy. Wrapper only; no Solidity change. `SpendPolicy` held one field, checked on one rail, so an agent that fell back to ETH - which is what an operator who configures nothing does - was on the unbounded path, and the printed fallback reason named `RUB3_AGENT_MAX_TOKEN_AMOUNT` as though that were the careful outcome.

**Not the overpayment hazard.** The item was filed when an agent reading a pre-cut price would silently overpay and the contract would keep the excess. §2.4's `_payEth` closed that: the ETH rail requires exact payment, so a price that moves in either direction between the read and the send reverts on-chain. What was left is the exposure that remains once overpayment is impossible - **there was no absolute ceiling at all**, so the agent would pay any price a contract listed, however large, as long as it still matched at execution - and the cost of a price move, which is a failed transaction and an on-chain error rather than a clean local refusal.

**One mechanism, two rails** - `crates/rub3-wrapper/src/activation.rs`
- `RUB3_AGENT_MAX_ETH_WEI`, an integer in wei, is a second field on `SpendPolicy` with a second `check_eth_wei` sibling to `check_token_amount`. Same `HeadlessError::PriceAbovePolicy`, same exit code 22, same `rail=`/`listed=`/`maximum=` detail line, minus `token=` because ETH's currency is not a contract. The variant now carries the variable that raises *its* rail's ceiling, so a refusal cannot tell an operator to raise the other one
- The call site is in `headless::purchase`, between reading `price()` and `tx::send`. That ordering is the guarantee: a refusal is local and costs no gas, where letting the transaction go would burn gas to learn the same thing from a revert. It is the ETH analogue of the stablecoin rule that nothing may be moved in front of the ceiling, with a different thing that must not exist - there, a signed authorization; here, a broadcast transaction
- The unit is named in the variable rather than left to convention, because the two plausible readings differ by 10^18. `0.05` does not parse, so an operator who writes ether gets a hard configuration error naming the unit instead of a purchase 18 orders of magnitude from the intent

**The default, 0.1 ETH, and why this rail may have one**

`RUB3_AGENT_MAX_TOKEN_AMOUNT` deliberately has none: it is denominated in whichever token a contract lists, decimals differ (USDC 6, DAI 18), and a fixed number is therefore wrongly scaled for some token. **That argument is about a unit this crate cannot know, and it does not transfer to ETH.** Wei is fixed on every contract on every chain the wrapper targets, so one number here is exactly as well defined as an operator's own.

The stablecoin rail's "unset means the rail is unavailable" is therefore not copied, and copying it was the one option ruled out rather than guessed: applied to ETH it would leave a wrapper that can buy nothing until configured, changing what every existing build does, and there is no rail to fall back to. A default is available precisely because the unit is knowable, and it closes the case the stablecoin rule cannot reach - an operator who configures nothing gets a bounded rail instead of an unlimited one. Neither rail is ever unbounded now.

The number is not a claim about what a licence is worth; ETH's value moves and no constant compiled into a binary tracks it. It is the blast radius of one unattended purchase: above ordinary licence prices (this repository's fixtures and worked examples sit near 0.001-0.01 ETH, so no existing operator's purchase changes), below what a funded agent holds. A real price above it is one variable away, with an exact named error until then - which is the outcome a silent unlimited denies.

`SpendPolicy::default()` is written out rather than derived for the same reason: a derived `U256::ZERO` would silently mean "refuse every ETH purchase", a plausible reading of an empty policy and the wrong one.

**One behaviour change beyond the ceiling itself.** `SpendPolicy::from_env` now parses both variables, and the ETH call site reads it on every purchase, so a *malformed* `RUB3_AGENT_MAX_TOKEN_AMOUNT` stops a run that would previously have bought in ETH without ever looking at it. That is what the variable's documented contract already said - a malformed value is a hard configuration error, never a silent zero and never a silent unlimited - applied on the path that reaches it. A well-formed value on either rail changes nothing about the other.

**Files** - `activation.rs` (the constant, the field, `check_eth_wei`, the shared `parse_ceiling`, the `var` field on `PriceAbovePolicy`, the call site, and the `--help` text), `README.md`, `architecture.md`, `testing.md`

**Tests** - 6 new `activation` unit tests, 2 new anvil-gated e2e, `SpendPolicy`'s existing 6 unchanged in behaviour
- Unit: the rail bounded with nothing configured (the default is neither zero nor unlimited, and an ordinary 0.01 ETH listing still buys), the boundary inclusive at exactly the ceiling and refused one wei above, the refusal's detail line and its variable, every malformed value a hard error naming the unit, zero as a real ceiling now that an unset variable can no longer express it, and the two ceilings not reaching into each other
- `headless_refuses_an_eth_price_above_the_spend_ceiling_before_sending_e2e` drives the refusal against anvil with four independent witnesses that nothing was broadcast: a `CountingSigner` never asked to sign, an unmoved nonce, an unmoved balance, and `nextTokenId` unchanged
- `headless_buys_at_exactly_the_eth_ceiling_and_under_the_default_e2e` checks the inclusive boundary against a real chain, then buys the same listing with a second agent that has no ceiling variable set at all - the property that keeps the default from being a breaking change
- Full 8-bundle matrix green, `cargo fmt --check` and `cargo clippy -- -D warnings` clean, and all 25 headless e2e tests pass against anvil. Only the `--lib` count for `tier-3,headless` moves, 156 → 162: `SpendPolicy` is behind the `headless` feature, so every other bundle is untouched at `tier-0` 46, `tier-1` 76, `tier-2` 91, `tier-3`/`tier-4` 101

**Left alone deliberately** - `contracts/contracts.md` describes the stablecoin ceiling in its purchase recipe and does not mention this one. Nothing it says became false, and `contracts/` was off limits here because a separate lane is editing it; adding the ETH sentence there is follow-up work.

**Phase 2 deliverable:** `rub3 deploy` → fund a fresh key → `rub3-wrapper --headless` completes purchase → activation → launch with no human present, and the deployed contract carries the fee split and ownership invariants.

---

## Phase 3: Distribution & Discovery

Goal: close the loop - discover → pay → fetch → verify → run - so the contract is a complete, self-describing distribution record, and machines doing integration research find rub3 first.

### 3.1 - Content-addressed distribution `[not started]`

- Contract gains `contentURI` (IPFS/Arweave) next to the wrapper hash set - the on-chain record now says *where* the binary lives and *what* it must hash to.
- `rub3 fetch <contract>` downloads from `contentURI`, verifies against the hash set (rejecting `Revoked`), and reports which release it got.
- `rub3 pack --publish` pins the artifact and writes `contentURI` + hash in one step.
- Hosted pinning is an optional paid convenience (off the enforcement path); any pinning service works.

### 3.2 - Registry `[not started]` *(replaces old §2.4)*

- Deploy `Rub3Registry` on Base: `register(appName, contract)` requires `factory.isDeployed(contract)` **and** contract ownership - only canonical deploys are listable.
- **Discovery, never validity:** delisting removes the badge and the listing; it cannot invalidate a token or a session. This invariant is documented and tested.
- Each entry doubles as an ERC-8004-style agent card: contract address, price(s), payment methods, `contentURI`, hash set, identity model - machine-readable, so agent spend policies can allowlist "verified rub3 contracts" and audit the §2.4 invariants before buying.
- Wrapper ENS handling softens accordingly: resolution to a *different* address → hard fail (attack signature); failure to resolve (lapsed name, dead registry, offline) → warn and proceed. The embedded contract address is the root of trust after purchase.
- **The registry reads `contracts/deployments.json` for the factory it trusts.** `register` gates on `factory.isDeployed(contract)`, so "the factory" needs a single committed referent per chain rather than an address baked into registry tooling; that file is it, keyed by chain id and carrying the deploy block an indexer starts from and the generation in the `previousFactory` chain. A registry that must honour an older generation's deploys walks `previousFactory` from the entry rather than keeping a second list. Every entry is null until launch, which is consistent with the factory and the registry launching together.

### 3.3 - Agent-facing surface `[not started]`

Distribution to the machines doing the integration research.

- `llms.txt` + docs served as clean Markdown (the repo's docs are already agent-legible; formalize it).
- Docs MCP server so Claude Code / Cursor pull real method signatures and contract ABIs instead of hallucinating them.
- One-shot quickstart: a single self-contained prompt/script - "paste this into your coding agent and your binary is wallet-gated on Base Sepolia in minutes" - deterministic, testnet-safe, verifiable. Market that fact explicitly.
- Listings: blockchain/MCP server directories, x402-adjacent catalogs (once §2.2 lands), ERC-8004 registries.
- **Beachhead:** wallet-gated MCP servers - ship the example (`examples/hello-mcp/`) and target paid-MCP developers as design partners.

### 3.4 - Concurrent seats `[not started]`

Fleet licensing - the tier the agent economy actually wants.

- Generalize tier 3's single `activeSessionId` into an on-chain semaphore: `maxConcurrentSessions[tokenId] = K` (set at purchase tier / deploy), `activate()` admits up to K live session ids per token, `release()` (or TTL lapse) frees a seat.
- One license NFT = K concurrent fleet instances; buy another token to scale. Cooldown still rate-limits churn.
- Wrapper: seat-aware activation + a clear "fleet exhausted, N seats in use" error for orchestrators.

### 3.5 - rub3 SDK crate `[not started]` *(moved from old §2.3)*

- `rub3::heartbeat()` - panics if wrapper is not alive (Unix socket / named pipe)
- `rub3::session()` - returns `SessionInfo { app_id, token_id, user_id, wallet, identity, expires_at }`
- Application code keys all persistent data on `user_id`, never on `wallet`
- Socket path passed as env var by wrapper; minimal dependency footprint - no `alloy` or `wry`
- Needed early for the MCP-server beachhead (a wrapped server checks its session/heartbeat).

**Phase 3 deliverable:** an agent that has never heard of rub3 can find a wrapped app via the registry/docs surface, buy it in USDC, fetch and verify the binary, and run it - headlessly, end to end.

---

## Phase 4: Machine Economy

Goal: the payment flows only rub3 can host.

### 4.1 - Metered billing (`Rub3Metered`) `[not started]`

- Third billing model: the launch gate requires a micropayment - per launch, per session-hour, or per N launches - settled in USDC (EIP-3009 authorizations batched/settled on-chain).
- The structural moat: x402 meters API calls because the server is a choke point; the wrapper is the only viable choke point for *locally executed* software. Same protocol fee, much higher-frequency flow.
- Pilot with one or two paid-MCP-server design partners before generalizing.

### 4.2 - Facilitator `[not started]`

- Hosted relay that submits EIP-3009 purchase/renew/meter authorizations and fronts gas for buyers holding only stablecoins.
- Bundled into the protocol fee rather than separately priced - its function is making the fee-carrying path also the lowest-friction path.
- Self-hosting the facilitator remains possible (it's a thin relay); the hosted one is a convenience, not a chokehold.

### 4.3 - License marketplace `[not started]` *(trigger-gated: do not build speculatively)*

- **Do not build speculatively.** Trigger: organic `Transfer` volume on factory contracts (all on-chain - query for the moment resale behavior emerges).
- Purpose-built venue for license resale: queryable by agents, filtered to registry-verified contracts, priced in USDC. 1–2% marketplace fee + ERC-2981 royalty split with the developer.
- This is what makes "licenses as liquid capital assets" real: agents buy for a workload, resell when the job ends.

**Phase 4 deliverable:** revenue flows from all three billing models plus secondary trades, entirely on-chain, with no invoicing and no accounts receivable.

---

## Phase 5: Human Surface *(demoted, not dropped)*

The interactive path stays fully supported - manual tx-hash paste is the floor today and remains reachable forever. Polish lands after the agent path.

### 5.1 - Frictionless tx confirmation `[not started]`

Demoted from Phase 1, where it was specified as §1.10 before the agent-first revision; the spec below applies unchanged when picked up. The manual-paste floor already works and stays reachable forever, so richer confirmation modes are human-surface polish rather than a gap.

The purchase (§1.7) and activate (§1.8) flows currently ask the user to paste a transaction hash back into the webview after sending from their wallet. That manual-paste path is our robust fallback - it works with any wallet / any tool / any chain, requires no JS dependencies, and has no external points of failure. But it is not the UX we want people to see first. This section layers two richer confirmation modes on top, while leaving manual paste as the always-available floor.

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

Each tab drives the same two outbound IPC events (`purchase_tx_sent` / `activate_tx_sent`) - the downstream poller/finalize path from §1.7 and §1.8 Phase B is untouched. This keeps auto-detect and WalletConnect as pure front-door improvements rather than new branches in the session pipeline.

#### 5.1a - RPC auto-detect `[not started]`

**Rationale.** Many embedded-app developers will never configure WalletConnect - they may not want the relay dependency, may not want to register with Reown, or may be shipping internal / CLI-adjacent tools. Auto-detect gives those deployments a one-click confirm path without adding any JS or external service.

**How it works.**
- Purchase: poll `eth_getLogs` for the ERC-721 `Transfer(0x0, wallet, *)` topic signature (already constant in `rpc.rs` as `ERC721_TRANSFER_SIG`) filtered by `address == contract`, starting from the block the user opened the screen. First match wins → its tx hash feeds the same `purchase_tx_sent` handler as manual.
- Activate: poll `lastActivationBlock(tokenId)` (already in `rpc.rs`); when it advances past the starting block, resolve the block's receipts and pick the one whose `to == contract && from == wallet`. That receipt's tx hash feeds `activate_tx_sent`.
- Poll cadence: 3 s, same as `spawn_tx_poller` / `spawn_purchase_poller`. Total budget configurable, default 120 s (longer than manual because the user is broadcasting the tx in-wallet during this window). Falls back to the Manual tab (pre-populated with helpful copy) on timeout or repeated RPC error.

**Rust additions (`rpc.rs`)**
- `pub fn watch_for_mint(rpc_url, contract, recipient, from_block, deadline) -> Result<String, RpcError>` - polls `eth_getLogs` with the `Transfer(0x0, recipient, *)` filter; returns the tx hash.
- `pub fn watch_for_activate(rpc_url, contract, token_id, from_block, deadline) -> Result<String, RpcError>` - polls `lastActivationBlock`; on delta, resolves the tx hash via `eth_getBlockByNumber` + receipt scan.

**Webview wiring**
- New IPC variants (gated on `onchain-write`): `AutoWatchStart { kind: "mint" | "activate", … }`, `AutoWatchCancel`. `webview.rs` spawns a `thread::spawn` running the watcher; on success the watcher routes its hash through the same internal dispatch as `purchase_tx_sent` / `activate_tx_sent` - no JS round-trip, no duplicated handlers.
- Existing purchase / cooldown / session handlers unchanged.

**HTML**
- Tabs in `#screen-purchase` and `#screen-cooldown`: `[WalletConnect] [Auto-detect] [Manual]`. The auto-detect body is a spinner + "Waiting for your wallet to broadcast the tx…" copy and a "Switch to manual" link.

**Gating.** `onchain-write` (already required by §1.7 / §1.8). No new Cargo feature. Pure additive - tier 3+ builds pick it up automatically.

#### 5.1b - WalletConnect v2 `[not started]`

**Scope.** The developer opts in per deployment by supplying a `wc_project_id` (obtained from cloud.reown.com). No single rub3-wide project ID - project IDs are the abuse / rate-limit boundary, and branding (the wallet QR prompt shows the dApp name) should reflect the embedded app, not rub3.

**Rust additions**
- `ActivationContext` (the `main.rs` constants struct) gains `wc_project_id: Option<&'static str>`. Missing or placeholder → WC tab is hidden. Default in the wrapper's own dev builds is `None`, not a shared project ID - `rub3 pack` (§2.5) rejects a distributable that inherits a placeholder value.
- Feature flag `wallet-connect` on the wrapper crate - opt-in because of the vendored JS weight. Composes with `onchain-write`; does not change tier bundle definitions (developer picks `tier-3,wallet-connect` at pack time).
- `webview.rs::show_purchase` / `show_cooldown` include the project id in the `onShowPurchase` / `onShowCooldown` payload when the feature is compiled in; JS decides whether to render the tab based on its presence.

**Assets (`assets/vendor/`)**
- `walletconnect-sign-client.mjs` - Reown SignClient v2 bundle (~250 KB).
- `qrcode.mjs` - ~5 KB QR-from-URI renderer.
- Both served by the same `include_dir!` custom-protocol handler introduced in §5.2 (Preact refactor); if §5.2 has not landed yet, this section creates that handler.

**Assets (`assets/app/`)**
- New `wc.js` - init `SignClient`, open a session via `chains: ["eip155:<chain_id>"]`, render the pairing URI as an inline QR, call `client.request({ method: "eth_sendTransaction", params: [{ to, data, value }] })` to dispatch either the purchase or activate tx. Returns the tx hash through the existing `purchase_tx_sent` / `activate_tx_sent` IPC message - reusing the rest of the pipeline.

**HTML**
- WC tab body: the vendored QR canvas, a "copy pairing URI" fallback, and error copy that suggests falling back to Auto-detect or Manual.

**Gating recap.** `wallet-connect` Cargo feature + developer-supplied project id. Both must be present for the tab to render; either absent → the tab is silently omitted and the user sees a 2-tab (or 1-tab) screen.

### 5.2 - Activation UI refactor to Preact `[not started]` *(was old §2.5)*
- Single reducer over `(phase, ctx)`; vendored `preact.mjs` + `htm.mjs` under `assets/vendor/`; `include_dir!` custom-protocol handler; no Node/bundler. No behavioral changes.

### 5.3 - Tauri integration `[not started]` *(was old Phase 3)*
- `tauri-plugin-rub3`: auto-heartbeat, session renewal in the app's own webview, `invoke('plugin:rub3|session')` JS API, `rub3://session-renewed` event.
- `create-rub3-app` starter template preconfigured against Base Sepolia.

### 5.4 - Polish `[not started]` *(was old Phase 4, minus deferred items)*
- Background session renewal with OS notification before expiry
- Windows support: named pipes for heartbeat IPC, MSVC target, WebView2 testing
- Subscription renewal UI (view expiry, renew from tray/menu)
- Multi-wallet delegation (hardware wallet owns, hot wallet signs sessions - EIP-7702 or delegation registry; exploratory)

---

## Deferred

Cut from the active roadmap with rationale; scaffolds are retained.

- **Tier 4 device binding** (`activateDevice`, `registeredDevice`, Secure Enclave/TPM storage) - device binding treats fleet cloning as an attack, but agent fleets clone VMs as a legitimate pattern; seats (§3.4) are the right concurrency primitive. Human anti-sharing pressure also shrinks when the customer is an agent with a wallet and a spend policy. `device.rs` scaffold stays behind the `device-key` feature.
- **Binary encryption** (AES-256-GCM unwrap, in-memory exec) - large engineering surface against a threat model the agent thesis dissolves; extraction-resistance was never a goal (see ideation.md, "Not DRM"). `decrypt.rs` scaffold stays behind `binary-encryption`.
- **Binary obfuscation** (UPX-style) - same rationale.

---

## Tech Stack

| Component | Technology |
|---|---|
| Wrapper runtime | Rust |
| Crypto (secp256k1) | `k256` crate |
| Ethereum RPC | `alloy` crate |
| Webview (interactive fallback) | `wry` crate - excluded from `headless` builds |
| IPC (wrapper ↔ app) | Unix domain sockets / named pipes |
| Smart contracts | Solidity, OpenZeppelin, Foundry |
| Target chain | Base (primary). Config-abstracted for other EVM L2s |
| Machine payments | USDC via EIP-3009 `receiveWithAuthorization` (see §2.2 for why not `transferWithAuthorization`) |
| Distribution | Content-addressed storage (IPFS/Arweave), hash + URI on-chain |
| Agent surface | `llms.txt`, Markdown docs, docs MCP server |
| CLI | `clap` crate |
| Packaging | `include_bytes!` embedding or custom bundler |

---

## Directory Structure

Current (implemented). The per-module map is not repeated here: README.md →
"Project structure" names every wrapper module, including the deferred
`device.rs` / `decrypt.rs` scaffolds.

```
rub3/
├── crates/
│   └── rub3-wrapper/                 # Wrapper runtime (src/, assets/activation.html, tests/)
├── contracts/                        # Foundry project (§1.5, §1.6)
│   ├── src/
│   │   ├── Rub3License.sol           # Abstract base: ERC-721 + Enumerable + Ownable, activation
│   │   ├── Rub3Access.sol            # One-time purchase license
│   │   ├── Rub3Subscription.sol      # Time-bounded license (expiresAt, renew, isValid)
│   │   └── Rub3Factory.sol           # §2.3 - fee-stamping deploys + isDeployed
│   ├── test/
│   ├── script/                       # Deploy.s.sol, DeployFactory.s.sol
│   └── contracts.md
├── licenses/com.rub3.example.json
├── scripts/
├── architecture.md
├── implementation.md
├── ideation.md
├── testing.md
└── README.md
```

Planned (not yet created):

```
├── crates/
│   ├── rub3-sdk/                # §3.5 - heartbeat, session info
│   ├── rub3-cli/                # §2.5 - pack, deploy, fetch, register
│   └── tauri-plugin-rub3/       # §5.3
├── contracts/src/
│   ├── Rub3Metered.sol          # §4.1 - per-launch billing
│   └── Rub3Registry.sol         # §3.2 - discovery + agent cards
├── llms.txt                     # §3.3
├── docs-mcp/                    # §3.3 - docs MCP server
└── examples/
    ├── hello-mcp/               # §3.3 beachhead - wallet-gated MCP server
    ├── hello-rust/
    └── hello-subscription/
```
