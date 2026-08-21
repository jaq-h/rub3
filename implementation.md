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
- Replace the "paste your tx hash" box with auto-detect + WalletConnect tabs while keeping manual paste as the fallback floor. Tracked as §5.1, which carries the per-mode status: auto-detect shipped there, WalletConnect is still outstanding.

**Verification**
- `cargo test -p rub3-wrapper --lib` (default tier-2): 57 pass (up from 51)
- `cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib`: 61 pass (up from 55)
- All five tier bundles (`tier-0`/`1`/`2`/`3`/`4`) compile clean
- `forge test` (contracts/): 33 pass
- Anvil-gated e2e (`session_verify_onchain_e2e`): passes with the new purchase-path assertions

### 1.8 - On-chain cooldown + session model (tier 3) `[complete]`

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
- `assets/activation.html` new screens: `cooldown` (shows calldata + tx-hash input with per-block-remaining banner when cooldown is active), `sign-session` (shows tx hash / block / session id / session message, captures signature). JS tracks `pendingSessionCtx` across the cooldown → tx-confirm → sign-session flow and echoes it back in `session_signed`. The tx-hash input is the "manual paste" path, and it remains the floor; the richer tabs layered on top are tracked as §5.1.

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
- Everything Phase C left open is now covered by Phase D below

**Phase D - the deferred regression net `[complete]`**

This is what the section was `[partial]` for. Phases A, B and C were each complete on their own terms; what was outstanding was that four behaviours the flow depends on had no test that would go red if they broke. They do now, so the section is complete.

The four behaviours Phase C listed as "still to do" have named tests in `crates/rub3-wrapper/src/webview/session_flow.rs`. They live there rather than under `tests/` because the seam they drive is `webview::IpcState` - the window's IPC handler - which is private to `src/webview.rs` and out of reach of an integration test binary. `activation::persist_activation` was split out of `interactive_slow_path` in the same pass, so a test writes the record through the production call instead of a copy of it, and `try_session_fast_path` became `pub(crate)` so the expiry test asserts against the launch path itself.

- The full session flow, connect → activate tx → session signing → the session surviving a restart: `a_connected_wallet_activates_signs_and_the_session_survives_a_restart_e2e`
- Cooldown enforcement: `a_second_activation_inside_the_cooldown_is_refused_and_the_window_says_how_long_e2e`
- Short-TTL expiry re-activation: `an_expired_session_is_refused_and_a_fresh_activation_replaces_it_e2e`
- Zero-contract legacy `LicenseProof` backward compatibility: `a_zero_contract_build_still_issues_and_serves_a_legacy_licence_proof`
- Per-test inventory: `testing.md` → "Webview session flow (`src/webview/session_flow.rs`)"

The first three are anvil-gated and need `tier-3,webview`; the fourth is gated on neither anvil nor `cooldown`, so it runs in the ordinary matrix from `tier-2,webview` up.

**The first behaviour is covered only at the `webview::IpcState` seam**, and the claim goes no further: connect → activate tx → session signing → persistence across restarts is driven by posting the IPC messages the page would post, not by a real browser or a genuine end-to-end webview drive. The browser layer itself - the `wry`/`tao` view and `assets/activation.html` - is untouched by these tests and remains §1.7's manual testing. Everything on the Rust side of that seam is covered.

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

Detection is the wrapper's, and it happens **before** broadcasting, so a misconfigured token costs no gas and ends no run: `choose_rail` pre-flights the `purchaseWithAuthorization` calldata as an `eth_call` from the account that would send it. A bytecode selector scan would not do - USDC sits behind a proxy, so scanning the token's runtime code for the overload's selector reports a false negative on the very token this rail targets. A contract-level failure selects ETH with a printed reason that leads with the revert the chain gave and offers the missing overload only as one possible cause, because the pre-flight executes the whole purchase and a blocklisted buyer or an exhausted supply cap reverts there too; a transport failure propagates as a hard error, as it does for every other token-side read.

**The pre-flight discloses an authorization, so it discloses a payment.** The `eth_call` carries a valid signature to a third-party RPC endpoint, and `purchaseWithAuthorization` is submittable by anyone: an endpoint that answers "reverted" - through a transient fault, or by inventing one - sends the run down the ETH rail and keeps a live payment instrument for the licence the buyer is about to pay for in ETH. Left alone, that is one licence bought twice, in two currencies.

Two things bound it, both in `activation.rs` where the constants are. The copy handed to the endpoint is signed with its own short window, `PREFLIGHT_AUTHORIZATION_TTL_SECS` (30 seconds - one JSON-RPC round trip plus clock error, against the 900 seconds a *broadcast* copy needs to be mined under congestion); the copy that is broadcast is signed only once the pre-flight has passed, so no *long-lived* authorization is ever created on the path that ends in an ETH purchase (the short-lived copy is created there, and is a fully valid payment instrument until it expires). Both carry the same salt, and the licence contract derives the EIP-3009 nonce from it, so the two are alternatives rather than two payments: whichever reaches the chain first voids the other. **That last property covers the submission path only, and not the fallback path this whole arrangement exists for.** When the pre-flight passes, the wrapper's own submission is `purchaseWithAuthorization` over the shared nonce, so a leaked copy spent inside its window makes the wrapper's submission revert rather than pay twice. When the pre-flight fails, no second copy is ever signed and the wrapper pays through the ETH branch of `purchase`, which never reads or writes `purchaseAuthorizationNonce`: nothing there can void the disclosed copy, so on that path the shared nonce bounds nothing at all and `PREFLIGHT_AUTHORIZATION_TTL_SECS` is the only thing standing between a leaked authorization and a second payment in a second currency. That is what the size of the number is buying. The rejected alternative was calling `cancelAuthorization` on the fallback path, which spends a transaction and gas on a rail the buyer never chose and can itself fail.

*Possible future direction, not built:* pre-flight against a **local fork** - an in-process EVM seeded from the live chain - so the authorization is never disclosed to anyone at all and the window question disappears. It is the clean answer in principle. It needs infrastructure the wrapper does not have - a forking EVM inside the binary, and an endpoint that will serve the state it forks from - and it would put a live endpoint and a pinned block behind the purchase path, which is the same cost that keeps the contract suite off fork tests; see the mock's rationale below.

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
- `activation.rs`: `choose_rail` prefers the stablecoin rail, but only once five things hold, **checked in this order**: the contract advertises one, the wallet holds at least `priceAmount` of it, the payment token's EIP-712 domain is readable, the operator's spend ceiling covers `priceAmount`, and only then - with a short-lived authorization now signed - the purchase pre-flights clean as an `eth_call` (which is what catches a token lacking the `bytes signature` overload), after which the copy that is actually broadcast is signed. The order is load-bearing, not incidental - see the two bullets below. Anything short of all five buys in ETH with a printed reason naming the cause, except the ceiling, which refuses. The rail decision is made in one place: `choose_rail` reads the token's `DOMAIN_SEPARATOR()` itself and hands it back with the price, so nothing downstream can discover a reason the rail is unusable after the ETH path has been passed over. `authorize_purchase` signs the digest through the existing `Signer` trait, so a KMS backend serves the money path with no new capability
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
- **No fee on secondary transfers, and no royalty hook. Reconfirmed as the accepted position.** ERC-721 transfer stays untouched: a royalty hook on `_update` would be a call the holder cannot avoid on a token they already own, which is a claim against a granted entitlement rather than against a new sale, and it would tax gifts, wallet moves, and treasury rotations identically to sales because the transfer path cannot tell them apart. It would also sit in immutable bytecode, so the rate could never move. Protocol revenue on the resale leg therefore comes from the rub3 marketplace (§4.3) when it is built - a venue takes its cut from a sale it actually facilitates, and a seller who transacts elsewhere pays nothing, which is the same economic-not-technical shape the deploy-time fee has. §4.3's ERC-2981-style developer split is that same venue mechanism, honoured by the marketplace on a sale it settles, not a charge the token levies on its own transfers. The consequence, accepted: until §4.3 ships, a resale earns rub3 nothing, and §4.3 is trigger-gated on organic transfer volume rather than built speculatively
- No discount, override, or exemption path. One rate per factory, applied to everything it deploys - anything else is a mutable fee wearing a different name
- **No charge on value that did not arrive through a payment function.** A direct ERC-20 transfer, a `selfdestruct` beneficiary, or a coinbase payout is never accrued against, and `withdraw` / `withdrawToken` release it whole to the developer. Accepted on the economics, not left open by accident; the argument is in `architecture.md` → "Why the fee split is shaped this way" and the scope statement in `contracts/contracts.md` → "What the fee covers, and what it does not", pinned by `test_token_unaccruedBalanceSweepsEntirelyToTheDeveloper`
- **The fee does not go live ahead of the registry.** Of the two things the fee is intended to buy, the registry (§3.2) is built and deployed nowhere and the marketplace (§4.3) is not built, so today the factory row is a durable canonical record and nothing more. The contracts are not deployed to mainnet or declared ready for use until the registry is ready: the factory and the registry launch together

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

### 2.5 - rub3 CLI `[partial]`

Pulled forward from the old Phase 2 - a CLI is the natural agent interface, and every step is already scriptable. New workspace member `crates/rub3-cli/`, package `rub3-cli` and binary `rub3`, off the wrapper's dependency path exactly as §3.3's docs server is.

```bash
rub3 pack --binary ./target/release/myapp --app-id com.example.myapp \
  --contract 0x1234...abcd --chain base --tier cooldown --headless \
  --session-ttl 7 --output ./dist/myapp

rub3 deploy --type access --name "My App License" --symbol MAL \
  --identity account --tba-implementation 0x... \
  --price-usdc 20 --price-token 0x... --chain base \
  --broadcast -- --private-key $DEPLOYER_KEY   # via Rub3Factory by default
```

- `pack` `[complete]`: single distributable binary (wrapper + embedded app + config); `--headless` selects the no-webview build; cross-platform targets via `--target`.
- `deploy` `[complete]`: factory-mediated; `--identity` sets `identityModel`; `--price-usdc` configures the EIP-3009 path.
- `fetch` `[not started]`: the agent-side half of distribution (§3.1).
- `register` `[not started]`: registry entry (§3.2).

**`fetch` and `register` are deliberately absent rather than stubbed.** Both are the agent-side halves of sections that do not exist: there is no content-addressed distribution to fetch from (§3.1) and no registry to register with (§3.2), so neither subcommand has anything to talk to. A subcommand that cannot work is worse than an absent one - it is a promise in `--help` that fails at the moment somebody depends on it - so `rub3 fetch` and `rub3 register` are not present in any form, and a test asserts they are not. They land with the sections that give them something to do.

**`pack` compiles `contracts/deployments.json` into the wrapper.** `--chain base` resolves to a chain id, and the canonical `Rub3Factory` for that chain is read out of that file and baked into the packed binary's constants alongside `CONTRACT` and `CHAIN_ID`, so a wrapper can tell a canonical deploy from any other without a network round trip or a hardcoded address in Rust. A `null` entry - which is every entry until launch - is a hard error from `pack`, never a fallback to the zero address: a distributable that claims a canonical factory it cannot name is worse than one that refuses to build. Same rule for `deploy`, which resolves `FACTORY` the same way.

**The refusal is the feature, and it has its own exit code.** `pack` and `deploy` both exit 2 on a `null` entry, so an orchestrator tells "nothing is deployed yet" from a typo (1) or a failed toolchain (3) without parsing English. That only holds while nothing else exits 2, and clap's own default is to exit 2 on every usage error, so the command line is parsed with `try_parse` and a missing flag, an unknown flag or an unknown subcommand is reported as 1 like every other impossible command line. The message names the chain and its id, quotes the manifest's own rule, and offers only choices that are choices: `--factory <address>` names one you deployed yourself, and `deploy --direct` deploys through none at all. Neither is reachable by forgetting a flag, and no code path anywhere substitutes the zero address. The manifest's `note` already said a consumer must stop rather than guess; this is the first consumer that had to.

**Three gates, not one, because the value crosses three boundaries** - `crates/rub3-cli/src/deployments.rs`, `crates/rub3-wrapper/build.rs`, `crates/rub3-wrapper/src/packed.rs`
- The CLI refuses a `null` entry, a placeholder, a zero address and an unknown chain *name*, and it validates a published address rather than trusting the file it came from
- The wrapper's new `build.rs` refuses the same values again, because it is the last thing between a pack input and a binary somebody else runs, and it is reachable without the CLI. It also refuses a *half* configured pack: any `RUB3_PACK_*` variable requires the whole set, so a forgotten one cannot leave a placeholder from `packed.rs` in a shipped binary
- `packed.rs` parses the numeric constants in a `const fn`, so a malformed chain id is a compile error rather than a runtime surprise
- Both the CLI and `build.rs` require `--app-id` to be one plain path component. It names the directory the packed binary extracts its application into and the file its licence proof is stored under, so a separator or `..` would put both outside the rub3 cache directory; `build.rs` holds the same rule for `RUB3_PACK_APP_NAME` and now shares one predicate with it
- The zero address is refused at every one of them, on `CONTRACT` as well as on `FACTORY`. It is a legitimate development value - the wrapper reads a zero `CONTRACT` as "no contract configured" and skips the ownership check - which is exactly why a distributable must never carry it: it would gate on nothing while looking configured

**A chain name is resolved, never guessed** - a name means nothing outside `contracts/deployments.json`, so a name it does not publish is refused with the list of the ones it does. A bare chain id is taken at face value, because it is already unambiguous: a local anvil is addressable, it simply has no canonical factory, and that refusal comes from the factory lookup rather than from being unable to name the chain. That is what keeps a local end-to-end run possible while nothing is deployed.

**What `pack` actually produces** - `crates/rub3-wrapper/src/packed.rs` (new), `build.rs` (new), `main.rs`
- One `cargo build` of `rub3-wrapper` with `--no-default-features --features <tier>,<front door>` and the identity in the environment. `--locked` is passed too: a packed binary's sha-256 becomes a wrapper hash on-chain, so a dependency that resolved differently between two packs would move it
- The application is embedded with `include_bytes!` and extracted on launch to `{data_dir}/rub3/apps/{app_id}/{sha256}/{name}`, staged and renamed into place so a half-written executable is never reachable, and keyed by content so a new version lands beside the old one rather than over a copy another process is running
- **Extraction happens after activation and never before it.** A failed launch leaves nothing on disk to run directly, which is the whole point of embedding rather than shipping two files
- A packed build refuses `--binary`. The point of packing is one file, and a wrapper that would launch a different application on request is a licence gate wrapped around whatever the caller felt like running
- The artifact path comes from cargo's own JSON output rather than from guessing at `target/release/`, so `CARGO_TARGET_DIR` and `--target` do not silently produce a distributable with somebody else's configuration in it
- Every `RUB3_PACK_*` variable the plan did not set is **removed** from the build's environment rather than inherited, for the same reason `deploy` clears the script's. `RUB3_PACK_DEVELOPER_ENS` is the one that makes it load-bearing: it is optional, so the wrapper's gate lets a build through without it, and a stale one exported from an earlier pack would be compiled in and shown to every licence holder during activation as this app's developer
- `pack` prints the sha-256 of what it wrote, which is the wrapper hash that seeds the licence contract's append-only set - `rub3 deploy --wrapper-hash` takes it directly
- `rub3-wrapper --version` now answers which licence the binary gates on, on which chain, through which factory, at which tier and through which front doors. A distributable is run by somebody who did not build it, and all of that is compiled in, so it costs no network call. The endpoint is the one field reduced rather than reported: it goes through `rpc::redact_urls`, the same helper and the same rule the error surface holds to, so `--version` prints scheme, host and port and not the provider key that lives in the userinfo, the path or the query

**`deploy` drives `contracts/script/Deploy.s.sol` rather than building the transaction** - that script is the deploy path the contract tests, `contracts/contracts.md` and CI all exercise, and a second implementation in Rust would be a second thing to keep correct about constructor argument order, the identity model's conditions and which rail a price belongs to. What the CLI adds is the part a `forge script` invocation cannot do for itself
- It resolves the canonical factory, and refuses when there is none
- It **clears** every variable the script reads that this invocation did not set. `vm.envOr` reads a value it cannot parse exactly as it reads an unset one, so an inherited `FACTORY` from an earlier `source .env` would not fail the deploy - it would produce a direct, unrecorded contract - and an inherited `PRICE` would be a listing nobody typed
- It refuses to broadcast unless asked. Without `--broadcast` forge simulates, and `--dry-run` prints the resolved plan without running forge at all
- `--price-usdc 20` sets the amount in USDC's six decimals and **requires** `--price-token`. rub3 publishes no per-chain stablecoin address anywhere, and a payment token is exactly the kind of address that must never be guessed; a price finer than the token's smallest unit is refused rather than rounded
- The signer is not a flag. Everything after `--` goes straight to `forge script`, and the passthrough begins at that separator and nowhere else (clap's `last`, not `trailing_var_arg`): a rub3 flag written after the deploy's other flags is still a rub3 flag, and an unrecognised one is a usage error naming it rather than an argument quietly handed to forge. `--private-key`, `--account`, `--ledger` and `--verify` work as `contracts/contracts.md` documents them, and no key material passes through an argument this CLI parses. The separator is load-bearing rather than cosmetic: a `--dry-run` collected into the passthrough would reach forge as an argument and leave the CLI thinking it was never asked to simulate, which broadcasts. The plan the command prints before it runs forge reports the invocation the CLI chose - the script, the endpoint, `--broadcast` - and says only *how many* arguments were passed through: that summary is printed on every deploy and not only a `--dry-run`, so echoing the tail would put an expanded `--private-key` in terminal scrollback and in any CI log that captures the step

**`--headless` below tier 3 is refused.** The headless front door enables `session`, `onchain-read`, `onchain-write` and `cooldown` on top of whatever bundle is named, and cargo features are additive, so `--tier offline --headless` would silently produce a tier-3 binary. Refusing beats shipping a binary whose tier is not the one asked for.

**Files** - `crates/rub3-cli/` (`main.rs`, `lib.rs`, `deployments.rs`, `repo.rs`, `tier.rs`, `pack.rs`, `deploy.rs`, `tests/cli.rs`, `tests/pack_build_gate.rs`, `tests/fixtures/deployments-populated.json`), `crates/rub3-wrapper/build.rs` (new), `crates/rub3-wrapper/src/packed.rs` (new), `crates/rub3-wrapper/src/main.rs` (constants moved to `packed`, `--binary` optional, `--version` provenance), `crates/rub3-wrapper/src/lib.rs` (`pub mod packed`), root `Cargo.toml` (the new member), `.github/workflows/ci.yml` (the `cli` job)

**Tests** - 17 unit and 26 end-to-end in the CLI, 5 more in `packed`, plus 6 `#[ignore]`d build-gate tests
- The canonical path cannot be exercised against a real address today, so it is exercised against a **fixture checkout** whose manifest publishes one, alongside the committed manifest that publishes none. A unit test asserts the committed one is still all null, so the day that changes, the tests that assume a refusal are read again rather than quietly passing
- The fixture publishes a factory on one chain and leaves the other null on purpose: the two records have independent lifecycles, and a fixture where everything is populated would not exercise the refusal that matters
- `tests/pack_build_gate.rs` runs a **real `cargo check`** against a poisoned environment, because the wrapper's build script is not reachable from the CLI and asserting on it any other way would be asserting on a copy. A zero factory, a zero contract, a half-configured pack, an app id that would escape the cache directory, and the placeholders `null`, `TBD` and `0xYourFactory` are each refused, and a complete environment builds - the positive control, without which every other assertion would pass on a gate that refused everything. `#[ignore]`d because each one costs a compile; CI runs them as their own step
- Verified end to end by hand on a local anvil: `rub3 deploy --direct` deployed a `Rub3Access` through the real forge script, a token was purchased on it, `rub3 pack --tier verified` produced a 7.4 MB distributable, and running that single file verified ownership on-chain, extracted the application and ran it with its arguments. The same run confirmed the extraction ordering: with activation failing, nothing was written to the cache directory at all

**Deliberately not built here**
- **No `rub3 fetch` and no `rub3 register`**, for the reason above
- **No pack-time signing or notarization.** Producing a distributable is one thing; making the operating system accept it from a stranger is a distribution concern that belongs with §3.1, not a flag on `pack`
- **No `--encrypt-binary`.** `binary-encryption` composes with tier-3 and up, but `src/decrypt.rs` is a deferred scaffold, so a flag for it would be a flag that does not work
- **No factory deploy.** `rub3 deploy` deploys licence contracts. `contracts/script/DeployFactory.s.sol` deploys factories, it is run once per chain per generation by whoever operates rub3, and its most consequential argument is the immutable treasury - see `contracts/contracts.md` -> "Treasury custody, and the pre-mainnet proof"

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
- Miss means refuse. `HeadlessError::NotCanonicalContract`, exit code 23, nothing signed. An on-chain `Rub3CodeRegistry` lookup for releases newer than the binary's table slots in between the miss and the refusal without restructuring anything; §2.9 built it there, and it did not restructure anything
- **It runs first in `purchase`, not merely before `tx::send`.** `choose_rail` signs an EIP-3009 authorization and hands it to the RPC endpoint as pre-flight calldata, and anyone may submit a `purchaseWithAuthorization`, so disclosure is the spend. A refusal arriving after that has already paid. Same ordering rule as the §2.2 spend ceiling, for the same reason

**Failure posture: closed on purchase, open on launch**
- Refusing to spend money on code that could not be verified is correct, so a chain read that fails is also a refusal
- Refusing to *start* a program the user already paid for because a check could not complete would be a de-facto revocation surface, which §2.4 rules out. Nothing on the launch path consults the module - there is no shared helper and no flag, and `the_attest_module_is_reachable_only_from_the_purchase_path` walks `src/` and fails if any module outside the purchase-path allowlist names the module at all, by any of its items. The allowlist is the paths that spend money, `activation.rs` and `webview.rs`; when this section shipped, `webview.rs` was named ahead of the work, because `show_purchase` still handed `purchase(recipient)` calldata to a human wallet unattested and gating it later should not have to argue with a test. §2.8 makes it real. A second assertion pins each allowlisted path to exactly one call site, since a subset is also satisfied by calling the gate nowhere. A third covers what a file-granular allowlist cannot: `webview.rs` holds the launch path as well as the purchase one, so the named human launch entry points in it (`show_activate`, `show_cooldown`, `finalize_session`) are asserted to reference the module not at all, and the assertion fails loudly rather than vacuously when one of those functions can no longer be found. The guard is honest about its limit rather than total: a new launch function in `webview.rs` is unguarded until somebody names it, and a reference elsewhere in `activation.rs` is not caught either. That test guards source structure, not runtime wiring; the behaviour is pinned by `headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e`, which relaunches a held licence against anvil and asserts it activates without minting, and - once the non-canonical fixture landed - by the two launch tests at the end of this section, which observe the same launch succeeding on a contract the gate refuses and against a node that will not answer the code read

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
- A miss is not an accusation: a contract from a newer template release than the binary was packed with presents identically. §2.9's registry is what tells the two apart once one is deployed

**Files** - `attest.rs` (new), `lib.rs` (`pub mod attest;` gated on `onchain-read`), `rpc.rs` (`get_code` via `provider.get_code_at`), `activation.rs` (the call site, the error variant, exit code 23 and its help text)

**Tests** - 15 unit tests in `attest`, 4 in `activation`, 1 in `rpc`, 5 anvil-gated e2e (three of them added once the non-canonical licence fixture could land - see the end of this section)
- The negative case is executable: a copy carrying `reconcileLedger(uint256,address)` - an owner-only seizure under an innocuous name - is asserted to pass the selector scan in silence and to fail the hash. That asymmetry is the whole justification for the work
- Also covered: a legitimate deploy that chose different immutables still matching, a truncated deploy refused rather than partially masked, an address with no code, the refusal naming what the pre-filter saw, the role check, and the pinned table's shape
- `headless_refuses_a_contract_whose_code_is_not_canonical_e2e` drives the refusal against a real deployed non-rub3 contract on anvil and asserts the signer's nonce and ETH balance are both unchanged - the executable form of "no transaction was sent"
- `headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e` is the other half of the posture: buy once through the gate, wipe the cached session so the fast path cannot answer, mine past the cooldown, and relaunch → `Activated` rather than `PurchasedAndActivated`, with `nextTokenId` unchanged read through `cast`
- Full 8-bundle matrix green. `--lib` counts move to `tier-0` 46, `tier-1` 76, `tier-2` 91, `tier-3`/`tier-4` 101, `tier-3,headless` 156. `tier-0` and `tier-1` gain only the one `rpc::get_code` error-path test and none of `attest`'s fifteen, which is the module compiling away as required

**Fixed in passing** - `tests/license_e2e.rs` had a real flake, roughly one run in sixty: `static_license_loads_and_verifies` and `dynamic_license_round_trips` both set and cleared the process-global `RUB3_LICENSE_DIR`, so one test's `remove_var` could land between the other's `set_var` and its read, failing it with an unrelated `NotFound`. Both now load through one helper holding a file-level lock across the whole set -> read -> unset window. 40 consecutive runs clean.

**Deliberately not built here** - the on-chain `Rub3CodeRegistry` (a separate deploy, and the answer to "several legitimate releases live at once"; §2.9), a signed release manifest fetched over HTTP (would add an HTTP client and a signature dependency to a crate with neither, to avoid a chain the agent is about to transact on anyway), and the immutables-versus-policy check, which is the one gap a fingerprint structurally cannot close.

**The test gap this section left open is closed.** It was open because the fixture it needed - a licence contract whose code is deliberately not canonical - is a Solidity change, and `contracts/` was being edited by another lane at the time. That fixture is now `contracts/test/mocks/NonCanonicalRub3Access.sol`: the whole of `Rub3Access` by inheritance, plus one owner-only seizure named `reconcileLedger(uint256,address)`, so it answers every read an agent makes exactly as a canonical deploy of the same arguments does, passes the selector scan in silence, and fails the masked hash. It lives under `test/` and must never move to `src/` - `scripts/canonical-bytecode-hashes.sh` fingerprints everything under the resolved source directory, so a copy there would be published as canonical rub3 code and `pinned_table_mirrors_the_canonical_manifest` would then demand a row for it in `attest::CANONICAL`, making the wrapper accept the contract these tests prove it refuses. The fixture's own header says so at length.

Three anvil-gated tests use it, and between them they pin both halves of the posture against a deployed contract rather than against synthetic bytes:
- `headless_refuses_a_modified_licence_that_passes_the_selector_scan_e2e` is the asymmetry itself, made executable end to end. It first asserts the fixture and a canonical deploy of the same constructor arguments agree on every getter an agent reads, then drives the purchase door at it: `NotCanonicalContract` / exit 23, and `exposed=none` on the detail line, so it is the hash and not the blacklist that caught it. The fixture advertises a stablecoin rail the agent holds and can afford, so the witnesses are chosen for that rail - a `CountingSigner` never asked to sign is what says no EIP-3009 authorization was produced, alongside an unmoved nonce, ETH balance, token balance and `nextTokenId`. Disclosure is the spend, so the nonce alone would not have been enough
- `headless_launch_of_an_already_paid_licence_survives_a_contract_the_gate_refuses_e2e` is the fail-open half. A licence is put in the agent's hands outside the wrapper (`purchase(address)` is callable by anyone, which is what makes seeding possible on a contract the wrapper would refuse to buy from), the session directory is asserted empty so the fast path cannot answer, the cooldown is mined past, and the launch reaches `Activated` with `nextTokenId` unchanged. **Both outcomes are driven against the same deployed address**: a second agent holding nothing is refused at the gate on that identical contract, immediately after it served the launch
- `headless_launch_survives_a_node_that_will_not_answer_a_code_read_e2e` covers the other way verification fails to complete, on a canonical contract so the read is the only variable. A `RecordingProxy` relays faithfully except for `eth_getCode`, whose connection it closes unanswered: the launch completes, and the recorded request log shows it never asked - the stronger of the two statements. The same proxy and the same run then drive the purchase door as a control, which stops at the gate with `HeadlessError::Rpc`, so the launch arm is a fact about the launch path and not about the proxy

Each was verified to fail against the behaviour it pins. The human door is unchanged and remains unit-covered: §2.8's `show_purchase` gate is exercised against a stub node, and driving a webview against anvil needs a front-door e2e harness that does not exist and was not worth inventing for this.

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

**Phase 2 deliverable:** `rub3 deploy` → fund a fresh key → `rub3-wrapper --headless` completes purchase → activation → launch with no human present, and the deployed contract carries the fee split and ownership invariants.

---

### 2.8 - The same gate on the human purchase path `[complete]`

§2.6 gated the agent and left the person ungated, which is the protection inverted with respect to who needs it: an autonomous buyer refused to spend on code the project does not vouch for, while a human buying through the wrapper's own window got no check at all. The human is the one who cannot read bytecode. Wrapper only; no Solidity change, and no second verification mechanism - the same `attest::verify_before_purchase`, called from the other door.

**The gate** - `webview.rs`, `show_purchase`
- First statement in the function, before supply, price or calldata are read. `show_purchase` builds the entire apparatus of the ask - the address to send to, the value, the calldata - so presenting that screen at all is the wrapper vouching for the address. A refusal that arrives beside it is not a refusal: the wallet already has everything it needs
- Fails closed, including on a chain read that did not complete, exactly as the agent door does
- On a match it prints the same one line naming what the money is about to go to, so a wrapper started from a terminal leaves the same trace either way

**The refusal is a screen, not an exit code**
- The agent door answers with exit 23 and a machine detail line, which is the right answer for an orchestrator and no answer at all for a person. `refusal_notice` turns a `GateError` into a title, an explanation, one next step and the finding; `window.rub3.onPurchaseBlocked` renders it in place of the purchase screen. The verification is shared and only the wording differs, which is where it should differ
- Three explanations, because the causes need different actions. **Unrecognised code**: the app does not know this contract, which is equally a newer template release and a modified copy - both are said, and the buyer is told to confirm the address with the publisher. **Canonical code that is not a licence** (the factory or a deployer helper, joined by the code registry in §2.9): the code is genuine and the *address* is the mistake, so nobody goes hunting for a compromise that is not there. **An address holding no code**: almost always the wrong network or a mistyped address, and saying "the code does not match" would send them looking for a contract that is not there
- Only a failed chain read offers `Try Again`, and it returns to the connect screen rather than re-running the check in place. A refused address is a settled answer, and a retry button on it invites clicking until the check passes
- A refusal shows its finding verbatim; a failed read shows only the kind of failure. The screen is telling the buyer to forward what it shows, and a network error is not what they should be forwarding. `retryable` is read off `RpcError::is_retryable` rather than restated here, so a settled failure can never acquire a `Try Again` button

**The packed endpoint is redacted across the wrapper's whole RPC error surface, not at a screen.** `RPC_URL` can carry a provider API key in its userinfo, its path or its query, and alloy builds its error messages from the request, so the key rides inside the error value itself. One sanitiser, `rpc::redact_urls`, rewrites every URL in a message to `scheme://host[:port]`, and every error built from a network or contract failure goes through it at construction: `RpcError::transport` and `RpcError::contract` in `rpc.rs`, and all seven `TxError::Rpc` / `classify` sites in `tx.rs`, which builds its own error type and never touches `RpcError`. Sanitizing at construction rather than at a display helper is the point: the same string reaches the window's error box, the agent door's printed `HeadlessError::Rpc` (from both `RpcError` and `TxError`), and `show_purchase`'s stderr line, and only the constructor is upstream of all of them. Both `RpcError` constructors were needed, not just the transport one - alloy reports an unreachable node during an `eth_call` as a *contract* error, which is how the window's "ownership check failed" box, the first error surface any buyer meets, was the most-travelled leak. The host and port stay: an operator chasing a dead endpoint needs to know which one failed, and neither is the secret.

**The redaction fails closed.** A bracketed IPv6 authority is the shape that proved it has to: reading `[` and `]` as delimiters cut the token to `scheme://`, which does not parse, so the placeholder was emitted and the entire authority, path and query were then appended verbatim as prose - destroying the host it meant to keep while publishing the key it meant to strip, which is worse than no redaction at all. The scan now skips to the matching `]` before looking for a terminator, and any address whose authority still will not parse is replaced by `[redacted url]` and consumed to the next whitespace rather than left to trail.
- The refusal never claims to know it found an attack. A miss is what a legitimate contract released after the wrapper was packed looks like too, and the words say so

**The launch path is untouched**, which is the constraint the whole design turns on. `show_activate`, `show_cooldown` and `finalize_session` serve a licence already paid for and still reference the module not at all; the guard that asserts it passed unchanged.

**Files** - `webview.rs` (the gate, `RefusalNotice`, `refusal_notice`, the tests), `assets/activation.html` (the `screen-blocked` screen and its handler), `attest.rs` (the allowlist guard now pins *each* purchase path to one call site, since `webview.rs` has a real one), `lib.rs` + `test_support.rs` (`StubNode` lifted out of `rpc.rs`'s test module so both can drive a canned node), `.github/workflows/ci.yml` and `AGENTS.md` (the `tier-3,webview` bundle)

**A ninth matrix bundle, because the code had nowhere to be compiled.** `show_purchase` is gated on `onchain-write`, which `tier-2` does not enable, so `tier-2,webview` builds the window without the one screen in it that spends money, and no tier bundle names a front door. `tier-3,webview` is the only entry that compiles this section at all, and without it CI would have carried a security gate it never built.

**Tests** - 7 unit tests in `webview`, compiled only under a bundle where a purchase can be made from the window, plus five in `rpc` and one in `tx` for the redaction, which is not gated on a front door
- `unrecognised_code_never_reaches_the_purchase_screen` drives `show_purchase` against a stub node answering with real non-canonical code and asserts the window was told exactly one thing, and that it was the refusal - "it also showed the purchase screen" cannot pass
- `the_code_is_checked_before_supply_and_price_are_read` is the ordering claim. A node that answers nothing fails both the code read and the supply read, so the two orderings are told apart by *which* failure reaches the window; "supply cap read failed" here would mean the contract was priced before it was verified
- `the_two_refusal_causes_read_differently`, `an_empty_address_says_the_address_is_empty` and `every_notice_reads_as_finished_prose` cover the words themselves - distinct titles, bodies and next steps per cause, the address always named, and no source line-continuation artefacts surviving into prose a person reads
- `a_failed_read_never_puts_the_rpc_endpoint_on_screen` builds a `Fetch` notice from a transport error carrying an RPC URL with an API key in it and asserts that no field the screen renders holds the host, the key or the scheme. The blocked screen tells the buyer to send `detail` to whoever published the software, so anything in it is as good as published, and the URL a build was packed with is not the buyer's to publish
- `the_ownership_check_error_box_never_shows_the_packed_endpoint` drives `handle` with a `connect` message against an unreachable endpoint whose URL embeds a key, and asserts the window's error box carries none of it. That arm runs before a purchase screen exists in the flow and is where the blocked screen's `Try Again` returns to, so it is the surface a buyer meets first
- In `rpc`: `a_key_in_the_endpoint_url_never_survives_into_the_error` covers a key in a path segment, in a query parameter and as userinfo, through both constructors, and asserts the host and the failure text survive; `redaction_keeps_the_origin_and_the_words_around_it` pins the rewrite itself, including the port and the prose either side; `a_bracketed_ipv6_endpoint_keeps_its_host_and_loses_its_key` covers the IPv6 shape that used to fail open, including a bracketed host followed by trailing punctuation; `an_unreadable_url_is_dropped_whole_rather_than_half_printed` asserts the fail-closed property directly against an unterminated bracket and a hostless URL; `an_unreachable_node_leaks_no_key_through_the_ownership_check` drives `tokens_of_owner` against a dead port so the contract-error classification is observed rather than assumed
- In `tx`: `a_failed_send_never_carries_the_packed_endpoints_key` drives `send` against a dead port for all three key placements, since `TxError` reaches an operator through the agent door verbatim and never passes through `RpcError`
- Full 9-bundle matrix green. `--lib` counts: `tier-0` 51, `tier-1` 81, `tier-2` 96, `tier-3`/`tier-4` 106, `tier-2,webview` 96, `tier-3,webview` 113, `tier-3,headless` 169. Every bundle gains the five `rpc` redaction tests, since the sanitiser is not feature-gated; `tier-3,webview` alone gains the two window tests and `tier-3,headless` alone the `tx` one, which is the gate and the agent door each compiling away where they cannot be reached

**Fixed in passing** - the activation window is a fixed 480x640 and several of its screens are taller than that, including the purchase screen at 885px. `body` centred the card with `align-items: center`, which centres by overflowing equally in both directions, so the top of a tall card sat above the viewport where nothing could scroll to it and the last button sat below it unreachable. The card now centres with auto margins, which collapse to zero once it overflows, and `body` scrolls. Also `proceed_after_token_selected` carried a `return` that is dead under every bundle compiling it - a clippy error that only `tier-3,webview` was ever going to see.

**Deliberately not built here** - reading the immutables behind a canonical fingerprint against a buyer policy, which is §2.6's open gap and belongs to both doors at once, not to this one.

---

### 2.9 - `Rub3CodeRegistry`: the version authority behind the pinned table `[complete]`

The other half of §2.6. A table compiled into a binary answers "is this the code I was built against" perfectly and "is this a genuine rub3 release" not at all, so **a contract deployed from a template release newer than the binary is indistinguishable from a modified copy** - both miss the table and both are refused. That is safe and it is wrong, and it does not stay a corner case: §2.3's factory, §3.1's content-addressed distribution, §3.2's registry and §4.1's metered contracts each put another legitimate release in the field while every fielded binary keeps the table it was packed with. This section is what tells the two apart. Contracts plus wrapper.

**Nothing is deployed, and the step is inert until something is.** `contracts/deployments.json` gains `code_registry` and `code_registry_deploy_block`, both `null` on both chains, and `attest::REGISTRIES` mirrors that file with `None` for every chain. On a table miss with no registry published, the gate does exactly what it did before this section: refuses, in the same words, having made the same single `eth_getCode`. `an_unpublished_registry_changes_nothing_about_the_gate` asserts all three of those - no chain carries an address, no extra chain read happens, and the refusal string is unchanged to the sentence - so the inertness is a test and not a claim.

**Append-only, and it can only ever add** - `contracts/src/Rub3CodeRegistry.sol` (new)

- `publish(mch, role, contractName, version, sourceCommit, solcVersion, offsets)` and `deprecate(mch, reason)`, both `onlyOwner`. There is no removal, no overwrite, no proxy, and no way back from `Deprecated` to `Active`. Republishing an existing hash reverts rather than replacing it, deprecated included, so no sequence of owner calls can change what the registry said about code an agent has already acted on. A compromised owner key is bounded to *additions*, each a permanent `Published` event a watcher can alarm on
- The prohibitions are absent from the bytecode rather than merely unused, and `test_audit_noRemovalOrRewriteSurfaceExists` asserts it against the deployed runtime code with the same scanner `Rub3Invariants.t.sol` uses, plus its own positive control. **This is a separate 10-name list, not the shared 30.** That list is about tokens - burn, seize, pause, upgrade - and says nothing about a registry; what would undo *this* contract's guarantee is a remove, a rewrite, or a status moving backwards. No count in the shared list moved, because no external function was added to a licence contract or to `Rub3Factory`
- **`Deprecated` never means "stop honouring".** It means "not recommended for new purchases": the record stays whole, offsets included, and an agent that meets it warns and buys. A deprecation that could strand a paid licence would be §2.4's revocation surface reached by a different door, and it is unreachable twice over - nothing on the launch path reads this contract, and the purchase path treats the status as advice. `test_deprecate_invalidatesNothing` compares the whole record either side of a deprecation
- The one-way status has a real cost, accepted: a deprecation made in error cannot be undone, because an un-deprecate is a second writable transition on a record meant to have exactly one. The cost is a warning that keeps appearing beside purchases that still complete
- **The MCH is the version identity.** `version` is a label for a log to quote and a human to read; nothing branches on it. `sourceCommit` and `solcVersion` are required and non-empty, because a record is permanent and a fingerprint nobody can reproduce is a fingerprint nobody can check
- `Ownable2Step`, and `renounceOwnership` reverts. Ownership here is the right to *add*; handing it to a mistyped address, or to nobody, would freeze the answer to "is this release newer than my binary" for every future release with no recovery. **Who the owner is and how that key is custodied is a deploy-time decision and is chosen nowhere in this repository**, exactly as `FEE_BPS` is not

**The offsets bootstrap** - `latestOffsetTables(count)`, `offsetTableWindow(start, count)`, `offsetTables()`

Computing a masked hash needs the immutable ranges; finding the record needs the hash. The registry interns the *distinct* tables its releases use and returns them in one call, so an agent fetches the short candidate list first and computes a hash under each - up to a cap, since each surviving candidate costs a round trip on a path that spends money. Today's canonical set spans five of them: one each for `Rub3Access`, `Rub3Subscription`, `Rub3Factory` and `Rub3Registry`, plus the empty one the two deployer helpers and `Rub3CodeRegistry` itself share. `publish` bounds what may go in one: each range is exactly one 32-byte word, ranges are sorted and disjoint, and none may fall outside EIP-170's 24,576-byte cap - which together bound a table to 768 ranges without inventing a limit of its own.

**The read is bounded at the contract, because a cap the caller applies afterwards is a cap it has already paid for.** `offsetTableWindow(start, count)` returns at most `count` tables from `start` in first-use order, clamped so a `count` past the end yields what is left and no reader needs a count call first; `offsetTables()` stays for a watcher or an indexer that wants the whole set and has no deadline. A published set the owner key grew to any length is therefore never transferred, decoded or shape-checked in full by anyone on a deadline. The cap is then applied again on the loop, since a node is not obliged to honour the window it was asked for. This is a latency bound only: reading the whole set could never produce a wrong verdict, only a slow one.

**And the wrapper reads the newest end, `latestOffsetTables(count)`, because the old end is the wrong one to protect.** A registry is consulted only when the pinned table missed, and a pinned-table miss is by definition a question about code *newer* than the binary asking. A budget spent oldest-first would mean that the moment a seventeenth distinct layout was interned, every release published under it became unreadable to every fielded binary carrying this cap, while the layouts of the very first releases stayed readable forever - silently blinding fielded binaries to exactly the releases the registry exists to vouch for. `latestOffsetTables(count)` returns the newest `count` tables, newest first, in one call, clamped the same way; newest first also means the likeliest candidate is the first lookup. Still not correctness: a layout never read is a release refused as unknown, which is what a build with no registry at all does.

Adding both now rather than later is the cheap moment and the reason is worth stating plainly - nothing is deployed to any public network, so the registry's fingerprint is still free to move. Once it is live, §2.4's accumulate-only rule makes that fingerprint permanent, and the same change would cost a fresh deploy plus a `CANONICAL` row that can never be removed.

**The wrapper asks only on a miss, and only after verifying the answerer** - `crates/rub3-wrapper/src/attest.rs`, `rpc.rs`

- `Role` gains `CodeRegistry`, and the enum's discriminants now mirror `Rub3CodeRegistry.Role` because a record carries the role as the raw `uint8`. Buying from the registry address is refused as the wrong address with its own detail line, the way `Role::Factory` and `Role::Deployer` already are: canonical rub3 code that sells nothing. The numbering is a wire encoding, so it is held by the wire: the anvil suite publishes every `Role` and reads it back, covers `Active` and `Deprecated`, and covers `Status.Unknown` as the mapping miss it is, since renumbering either enum silently turns a factory into a purchase target. A role number this build has no name for stays unnamed, which a unit test pins
- **The registry's own code is verified before its answer is believed.** One address (pinned per chain) and one masked hash (an ordinary `attest::CANONICAL` row) frozen into the binary, and the deployer trusted for nothing. That is what terminates the trust recursion, and it is the first read `consult_registry` makes: `an_unverified_registry_is_never_asked_anything` asserts through a call log that nothing follows a failed verification
- **Registry-supplied offsets are checked against the fetched code before anything is masked with them.** A masked byte is a byte the comparison never looks at, so a table wide enough or placed freely enough is a blind spot somebody else chose. Each range must be one 32-byte word, sorted, disjoint, inside the code, and **preceded by the `PUSH32` opcode** - which in compiler output means the range is that instruction's immediate operand, and jump-destination analysis excludes bytes inside push immediates, so the masked bytes cannot execute. Measured on all four of this repository's contracts that have immutables (`Rub3Access`, `Rub3Subscription`, `Rub3Factory` and `Rub3Registry`; the two deployer helpers and `Rub3CodeRegistry` have none), not assumed. What the one-byte lookback does not establish is that the `PUSH32` byte is itself an instruction rather than data inside an earlier push's immediate, so it bounds a careless or drifted table rather than a hostile authority: code and table shaped together could mask bytes that do execute, which needs the registry's owner key, and that key has the shorter route of publishing an empty table. The full "these bytes cannot execute" guarantee is the pinned table's, whose ranges arrive with the binary from solc. Dropping every candidate is `Unknown`, not an error: it says no published release is shaped like this code. The pinned table is deliberately *not* put through this check - its ranges arrive with the binary and are already covered by the drift tests, and a bytecode-shape check against them would let a chain read refuse the contracts the build was packed to buy from
- A record whose own declared offsets differ from the table its hash was computed under is refused: that is a registry describing one layout while answering about another
- **The candidate list is capped at both ends, which changes what the money path does.** Every candidate table that survives the shape check costs its own sequential `record` call, so an unbounded list let whoever holds the owner key turn each purchase into arbitrarily many round trips. The bound this project publishes on that key is that the damage is limited to *additions*, each a permanent public event a watcher can alarm on - and a purchase that takes minutes is not an addition anyone would alarm on, so the published bound did not cover it. `attest::MAX_CANDIDATE_OFFSET_TABLES` closes that gap at 16, and it closes it at the read as well as at the loop: the wrapper asks the registry for exactly that many tables through `latestOffsetTables`, then holds the same number over the lookups because a node may answer with more entries than it was asked for. So the published bound on a compromised owner key is now true without an asterisk: neither the response the wrapper pays to transfer and decode nor the round trips it makes grows with what that key publishes. **The budget is spent from the newest end**, since a lookup only happens after a pinned-table miss and a miss is a question about newer code; oldest-first would have made a seventeenth layout the point at which fielded binaries stopped being able to recognise new releases at all. Candidates past the cap are never read or never tried and, like a dropped one, contribute nothing, so the verdict is the same `Unknown` rather than a new outcome. This is a latency bound plus a reachability property and nothing more: neither an uncapped read, nor an uncapped loop, nor a read of the wrong end could ever produce a wrong verdict. The number is far above legitimate use on purpose - a table is per *code layout*, not per contract or per release, and today five exist across seven fingerprinted contracts - so it is raised only when a real deployment needs more, never to accommodate a registry publishing tables nothing was deployed under
- Chain access goes through a `ChainReader` seam, with `RpcChain` the only implementation a shipped binary builds. The interesting cases here are a registry that is unreachable, one answering from the wrong code, and one publishing a table that does not describe the contract, and no live endpoint can be asked to produce those on demand

**What a refusal now says, and what it deliberately still says** - `activation.rs`, `webview.rs`

- `Unrecognised` carries a three-valued `RegistryOutcome`: `not_consulted` (this build knows no registry on this chain - every chain today), `unknown` (asked, no record), `unavailable` (could not be asked). An orchestrator retries none of them, but only the last means the question went unanswered. It rides in the agent door's detail line **in front of `exposed=`**, which stays last because its values carry commas and parentheses and are terminated by the end of the line
- The window says the same thing in a person's words: with no registry there is no extra sentence at all, `unknown` adds that the on-chain record of genuine releases has no entry either, and `unavailable` says the app could not reach it and so could not tell a newer release from a modified copy. None of the three is retryable - a refused address is a settled answer for that attempt, and a Try Again button beside one invites clicking until the check passes
- **The registry's failure reason does not reach the screen.** §2.8 settled that a failed chain read shows a person the *kind* of failure and not the error, because the blocked screen asks the buyer to forward what it shows and the packed `RPC_URL`'s host is not theirs to publish. §2.9 added a second chain read whose failure travels *inside* the refusal rather than beside it, which is a different route to the same leak, so `Unrecognised::shareable_detail` drops an `Unavailable` reason while keeping an `Unknown` verdict - which names no endpoint and is half of what the refusal means. The agent door still prints the reason in full: an operator who ran the wrapper already knows the endpoint and needs to know which read failed
- The gate's success value is now an owned `Attestation` naming which authority vouched, because it can no longer be a pointer into the binary's table. A registry hit prints the block the record was published in; `Attestation::advisory()` returns the deprecation warning and the two doors word it themselves
- **The deprecation advisory reaches the person, not only the terminal.** §2.8's founding premise is that the buyer is the one who cannot read bytecode, and a buyer does not read stderr either, so an advisory only the agent door showed would repeat the inversion §2.8 exists to close. `onShowPurchase` carries an `advisory` field and the purchase screen renders it above the price, in amber rather than the refusal's red and beside the purchase rather than in place of it: the sentence names the release, says the code is genuine, says the licence stays valid, and offers cancelling as a choice. It claims no more than the record carries: `Deprecated` has no reason field and no successor pointer, so neither door promises a newer version nor sends the buyer to fetch one, which a deprecation issued for a defect with no fix yet would make a wild goose chase. It is advice, and the screen must never read as a block
- **Registry-supplied text is reduced before it is repeated.** `publish` requires `contractName` and `version` to be non-empty and requires nothing else of them, and both reach surfaces with a grammar: the agent door's detail line carries the name as `canonical=<name>` in space-separated `key=value` pairs, and both doors print the attestation as one `rub3:` line. A published space or `=` would invent a field an orchestrator reads; a published newline would forge a line of the wrapper's own output. The reduction happens once, where the registry's answer enters the decision - the name to one token, the label to bounded printable ASCII - so no surface has to remember the rule. The pinned table never needed it: its names are Solidity identifiers fixed at build time
- **An address holding no code never becomes a version question.** There is no release for an authority to recognise, so the registry is not consulted at all and the refusal stays the one sentence a person can usually act on, rather than growing "; the on-chain code registry has no record of it either" about something that is not there

**Failure posture unchanged, in both directions.** Closed on purchase: a registry that cannot be read leaves the refusal standing, since a chain read that did not complete is not permission to spend. Open on launch: nothing on the launch path reads any of this, and `the_attest_module_is_reachable_only_from_the_purchase_path` passed unchanged. And the registry cannot fail *open* against a buyer either - `Deprecated` is the strongest thing it can say against a release, and it is advice.

**Files** - `contracts/src/Rub3CodeRegistry.sol` (new), `contracts/test/Rub3CodeRegistry.t.sol` (new), `contracts/canonical-bytecode.json` (regenerated in the same commit; only the registry's own row moved, so no existing fingerprint changed and `RELEASE` did not move), `contracts/deployments.json` + `scripts/check-deployments.sh` (the two new fields, their own all-or-nothing group, and one shared address rule so the factory and the registry cannot be held to different standards), `attest.rs`, `rpc.rs` (the ABI mirror and two reads), `activation.rs`, `webview.rs`, `assets/activation.html` (the advisory on the purchase screen), `tests/code_registry_e2e.rs` (new), `.github/workflows/ci.yml`

**Tests** - 35 forge tests in `Rub3CodeRegistry.t.sol`, 25 new unit tests in `attest`, 5 in `webview`, 1 in `activation`, 1 new anvil-gated suite

- Solidity, as behaviour rather than as comments: publish records what it was told and stamps its own block; republish and overwrite both revert and leave the record untouched; a deprecated hash cannot be republished either; removal and rewrite surfaces are absent from the runtime bytecode; deprecate preserves the entire record and touches no other; non-owner calls revert on both writers; ownership transfers only on acceptance and cannot be renounced; offset tables intern, and malformed ones are rejected at four shapes including the exact EIP-170 boundary; a bounded window read returns its bound out of a set four times larger, starts where it is asked to, and clamps rather than reverting at either end and at the largest `count` a caller can pass; the newest-end read returns the newest layouts newest-first out of a set four times its bound and clamps the same way; a reverted publish leaves neither an enumeration entry nor an interned table
- Wrapper, the three-way verdict of the design's own table: canonical from the pinned table (and asking the registry nothing, asserted through the call log), canonical-newer from the registry, not-canonical from neither. Plus: registry unreachable at each of its three reads, a registry whose own MCH does not verify, a registry answering from canonical-but-wrong code, a hostile offset table never even hashed under, an inconsistent record, every candidate table tried, the candidate list capped with the answer hidden one table past the cap (`Unknown`, and exactly the cap's worth of lookups in the call log), the bootstrap read itself asked for bounded against a registry holding four times the cap, the budget spent on the newest layouts so a release published under the newest of 64 is found in one lookup while one published under the oldest is refused as unknown, and a node that ignores the window it was asked for still buying no extra lookups, a deprecated release bought with a warning that says held licences are unaffected, and a registry record for a non-licence role - including a role this build has no name for, which is refused rather than guessed at as the first variant
- The window's four: the three registry outcomes reading differently and none of them retryable, the registry named as the wrong address rather than as bad code, a role this build is too old to name, and `a_failed_registry_read_never_puts_the_packed_endpoint_on_screen`, which asserts neither the host nor the key of a packed endpoint survives into any field the blocked screen renders while the buyer is still told the check was incomplete
- `code_registry_e2e.rs` deploys `Rub3CodeRegistry` and `Rub3Access` on anvil, publishes a release through `cast`, and reads it back through the wrapper's own decoder. Two things only it can check: **the ABI mirror**, since field order is the encoding and a drifted mirror decodes garbage, and **the pinned fingerprints against real deploys** - the registry's own row, and a live `Rub3Access` whose constructor filled its immutables in, hashing to the pinned value once those ranges are zeroed. It also drives deprecation over the wire, reads a bounded newest-first window out of a registry holding more tables than the window asked for, and asserts a licence contract is never believed as the registry. Wired into the existing `onchain-e2e` CI job at `tier-2`, the lowest bundle that compiles the path
- Full 9-bundle matrix green, `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean on every bundle, `forge test` 224 passing (up from 189), `scripts/check-deployments.sh` and `scripts/canonical-bytecode-hashes.sh check` green, and all four anvil suites pass (30 headless, 1 session, 3 webview session flow, 1 code registry). `--lib` counts, each the run's own `passed` plus `ignored`: `tier-0` 51, `tier-1` 81, `tier-2` 121, `tier-3`/`tier-4` 131, `tier-3,binary-encryption` 131, `tier-2,webview` 122, `tier-3,webview` 147, `tier-3,headless` 195. `tier-0` and `tier-1` are unmoved, which is the registry path compiling away as §2.6's rule requires

**Deliberately not built here**

- **No deploy, and no decision that implies one.** No address is invented, placeholdered, or defaulted anywhere; `null` and `None` are the only markers, and `scripts/check-deployments.sh` rejects anything else. Who owns the publish key, how it is custodied, and when the registry ships are the captain's calls and are made nowhere in this repository
- **No publishing tooling.** A release is published with one `cast send`, and a script that automated it would be a script that decides what a release is. `contracts/contracts.md` carries the recipe
- **No watcher.** What bounds a compromised registry owner key is that it can only ever add records, each one a permanent public event - and that bound is worth only as much as somebody alarming on `Published`; the events and the `deploy_block` to start from are here, and the alarm is not
- **No read quorum.** One dishonest RPC endpoint still defeats the whole scheme, and now defeats both the code read and the registry read at once rather than being diluted by the second authority. The claim stays "an honest view of chain state implies canonical code" and no stronger. `architecture.md` and `contracts/contracts.md` say so beside every place they describe what attestation answers
- **Still nothing about the immutables behind the mask.** §2.6's open gap is unchanged: a fingerprint structurally cannot cover the values it zeroes, and a registry record does not change that

## Phase 3: Distribution & Discovery

Goal: close the loop - discover → pay → fetch → verify → run - so the contract is a complete, self-describing distribution record, and machines doing integration research find rub3 first.

### 3.1 - Content-addressed distribution `[not started]`

- Contract gains `contentURI` (IPFS/Arweave) next to the wrapper hash set - the on-chain record now says *where* the binary lives and *what* it must hash to.
- `rub3 fetch <contract>` downloads from `contentURI`, verifies against the hash set (rejecting `Revoked`), and reports which release it got.
- `rub3 pack --publish` pins the artifact and writes `contentURI` + hash in one step.
- Hosted pinning is an optional paid convenience (off the enforcement path); any pinning service works.

### 3.2 - Registry `[partial]` *(replaces old §2.4)*

`Rub3Registry` is built and tested; nothing is deployed, and the ENS half is unbuilt. Contracts only.

- Deploy `Rub3Registry` on Base: `register(appName, contract)` requires `factory.isDeployed(contract)` **and** contract ownership - only canonical deploys are listable. **Built**, as `register(address,string,string)`, with the factory reference read live and the ownership check read live.
- **Discovery, never validity:** delisting removes the badge and the listing; it cannot invalidate a token or a session. This invariant is documented and tested. **Built**, and proved from the bytecode rather than documented as a commitment - see below.
- Each entry doubles as an ERC-8004-style agent card: contract address, price(s), payment methods, `contentURI`, hash set, identity model - machine-readable, so agent spend policies can allowlist "verified rub3 contracts" and audit the §2.4 invariants before buying. **Built**, as `card(address)`, `cards(start,count)` and the bounded `cardWindow(start,count)`.
- The recognised-token list and the live ranking. **Built.**
- Wrapper ENS handling softens accordingly: resolution to a *different* address → hard fail (attack signature); failure to resolve (lapsed name, dead registry, offline) → warn and proceed. **Not built**, and not a change made here: the wrapper resolves no ENS name today and carries no name to resolve, so this is a design in `architecture.md` → "ENS Trust Layer" waiting on the resolver work rather than a behaviour that softened. The registry deliberately holds no ENS record either - see "Deliberately not built here" below.

**This is not `Rub3CodeRegistry`, and every artifact here says so.** Two contracts in this project have "registry" in the name and they answer different questions: §2.9's code registry answers "is this bytecode a genuine rub3 release", keyed by masked code hash, read on the purchase path; this one answers "which apps exist and which are listable", keyed by licence contract address, read by a shopper before it has an address to verify. Neither is evidence for the other's question, and the confusion is cheap to create and expensive to find, so it is guarded rather than described: `test_neitherRegistryCanStandInForTheOther` asserts neither contract's runtime bytecode carries the other's selectors, with a positive control that the scan finds selectors that are there. The two also now carry different `Rub3CodeRegistry.Role` values, so a buyer's agent pointed at either is refused by name.

**Discovery, never validity - proved from the opcodes.** The strongest available form of this invariant is not a test that delisting leaves a token alone; it is that the registry has no instruction that could touch one. Every external call it makes is to a `view`, which solc compiles to `STATICCALL`, so its runtime code contains no `CALL`, `CALLCODE`, `DELEGATECALL`, `CREATE`, `CREATE2` or `SELFDESTRUCT` at all. `test_audit_registryHoldsNoStateChangingExternalCall` walks the opcodes, skipping each `PUSH1..PUSH32` immediate so a `0xF1` inside a push operand is read as the data it is, and `test_audit_opcodeWalkFindsACallWhereOneExists` is the positive control on a licence contract, which does move money and does contain a `CALL`. That is what moved "registry delisting never invalidates a token" out of `architecture.md`'s **convention** table and into its **bytecode** table, where every other ownership invariant lives.

The behavioural tests sit underneath it rather than instead of it. `test_delisting_cannotTouchAHeldTokenOrALiveSession` buys a subscription, activates a session, then pulls every discovery lever at once - the owner delists, the registry suspends, the payment token stops being recognised - and measures `ownerOf`, `isValid`, `activeSessionId`, `expiresAt`, a fresh activation past the cooldown, a fresh purchase and a renewal. `test_registryWrites_leaveTheLicenseContractUntouched` snapshots nine pieces of licence state across every registry write.

**The ranking reads the quote live, and that is the whole of the difficulty.** `setTokenPrice(address,uint256)` stays owner-callable on a licence contract for its life, so a contract registered while priced in a recognised token can switch the block afterwards. A rank snapshotted at registration would keep advertising it on a quote it no longer honours and emit nothing to say so, so `priceToken` is read from chain on every ranking call, one `eth_call` per entry inside a `view`. `test_rank_followsAPostRegistrationTokenPriceChange` is the test written against exactly that failure: two entries registered on opposite quotes both call `setTokenPrice` to swap, and the order has to swap with them. A snapshot implementation passes every other test in the file and fails that one, which is the property a test of this kind needs.

The native rail is recognised by rule rather than by membership. An ETH-only contract quotes no token at all (`priceToken == address(0)`) and its fee accrues in ETH, so only a contract quoting a token rail in an unrecognised asset ranks below, and `setTokenRecognised` refuses `address(0)` in both directions - allowing it as a key would put the entire ETH-only population one owner transaction away from the bottom of the list. `isRecognisedToken` is a function rather than a public mapping for the same reason: a `mapping(address => bool) public` would answer `false` for the native rail while every other read treated it as recognised, and the disagreement would surface only in whichever caller happened to ask the mapping.

**The generation walk, and why it is not `isCanonicalPredecessor`.** `isCanonicalDeploy` checks `factory.isDeployed`, then walks `previousFactory` for up to `MAX_FACTORY_GENERATION_HOPS` (8) further generations, so the contracts an earlier factory recorded stay listable when rub3 ships a new one. `Rub3Factory.isCanonicalPredecessor` performs the same steps and is deliberately not called: it answers "may this be named as the predecessor of a new deploy", it returns true for `address(0)`, and its rule belongs to the deploy path. Binding discovery to it would mean a future factory tightening its predecessor rule silently delisted nine generations of applications, which is a validity decision reaching into discovery by the back door. The two bounds are separate constants for the same reason.

The factory a registry trusts is immutable and comes from `contracts/deployments.json`, keyed by chain id, which already carries the deploy block an indexer starts at and the generation in the chain. No field was added to that file: the registry reads the factory record, and its own address gets a record there when it is deployed, alongside the factory it launches with. The constructor probes both views the walk reads, exactly as `Rub3Factory` probes its own `previousFactory`, because the pointer is immutable and an address that cannot answer would reject every registration forever with no way to correct it. That probe is also what lets the walk run without a `try` at every hop: each factory validated its own link when it was built, so the chain is well-formed by induction.

**Every read that would scan the whole set has a bounded counterpart, and the misleading middle case is labelled.** Registration is permissionless for anyone holding a factory deploy and nothing is ever removed - `delist` and `suspend` change flags and leave the entry in `registered()` - so the set only grows at a rate strangers decide, and this contract cannot be redeployed to fix that later. `registeredWindow`, `rankedRegistrationWindow` and `cardWindow` take their cursor over registration order, scan at most `count` entries, and make at most one `priceToken()` read per listed entry among them, so nothing they cost follows how large the registry has grown. All of them clamp at both ends like `Rub3CodeRegistry.offsetTableWindow`, so one call is enough.

The price is stated on the functions themselves rather than only here: **a bounded page is ranked within its window, not globally**, and paging through cannot reconstruct `rankedListings()` - an unrecognised entry in an early window still precedes a recognised one in a later window, because no window can know what the others hold without reading them. A caller that needs the global order either pays for it or collects the windows and ranks them off-chain, where `isRecognisedRail` is the same input the contract uses. `rankedListingWindow` and `cards` are the case worth labelling: they take a `start` and a `count` and look bounded, but those index into the global ranking, so they cost the whole scan however small a page is asked for. Two functions that look bounded and are not is the state this avoids, so each says so in its own NatSpec.

**The card's hash set is capped, and the cap reports itself.** `Rub3License.addWrapperHash` is append-only and uncapped, so an unbounded card let one licence owner decide what reading their listing cost, and therefore what any page containing them cost - a reach into unrelated listings' discoverability that no listing owner gets to have. `card` takes the newest `MAX_CARD_WRAPPER_HASHES` (32) and reports `wrapperHashCount`, the true total, beside them with a `wrapperHashesTruncated` flag, so a partial answer is never mistaken for a complete one. Nothing is capped on the licence contract, which still publishes the whole set through `wrapperHashCount()` and `wrapperHashAt(index)`. The newest end is kept for the reason `latestOffsetTables` spends its budget there: a buyer checking the build it just downloaded is asking about the most recently published hash.

**Every text field is bounded at the point it enters the contract.** `appName` is capped at `MAX_APP_NAME_BYTES` (128), `contentURI` at `MAX_CONTENT_URI_BYTES` (512) and the `suspend` reason at `MAX_SUSPENSION_REASON_BYTES` (512), all through the one helper both writers already route through. Two limits rather than one because a name and a locator are not the same kind of value: a CIDv1 base32 `ipfs://` URI already runs to about 66 bytes before any path. The bound is at entry and never on the read path, so a field added later is bounded by being written at all rather than by somebody remembering to cap it while assembling a card - and without it the string bound would have been the hole the `MAX_CARD_WRAPPER_HASHES` cap above was closing, since `card` copies both strings and registration is permissionless for anyone holding a factory deploy. `contentURI` stays optional: it is bounded through a length-only sibling of the required-text helper, so an empty value still means "nothing published yet". Over-length input reverts `TextTooLong(field, length, limit)`, which says what to shorten and to what.

**`priceTokenOf` reads a contract that cannot answer, rather than reverting on it.** The rule the ranking documents - a licence contract that cannot answer `priceToken()` is read as ETH only, matching the wrapper - was written with `try`/`catch`, which does not deliver it: solc decodes a returning call's response in the *calling* frame and skips its `extcodesize` check when a return value is expected, so an address with no code or a contract with a silent fallback reverted the reader instead of entering the `catch`. It is now a code-length check plus a low-level `staticcall` whose response width is checked before it is decoded, which is the same shape the constructor already used to probe the factory. That keeps one unreachable entry from reverting a whole `rankedListings` page, which was the guarantee's only purpose.

**What the registry owner may do, and what bounds it.** Curation is the whole of the owner's power: maintain the recognised-token list, and withhold the badge from a listing with a logged reason. The badge is registry-granted, so a badge only its holder could remove would not be one; and the owner already moves entries between ranking groups by token policy, so refusing an outright delisting while allowing demotion would be incoherent. What bounds it is the invariant above rather than a restriction on the surface: a compromise of the owner key makes the discovery surface wrong until it is fixed and cannot reach a token, a session, a renewal, or a price. Suspension is independent of the listing owner's own flag, so lifting one never overrides an owner who withdrew in the meantime, and `renounceOwnership` reverts because abandoning curation would freeze the token list at whatever it happened to say on a chain where the assets it names can be deprecated or migrated.

**A contract change is a wrapper change, and this one moved two fingerprints.** `Rub3CodeRegistry.Role` gained `DiscoveryRegistry` so that a buyer's agent pointed at the discovery registry is refused by name rather than mis-typed as a code registry, which moves solc's enum decoder bound and therefore `Rub3CodeRegistry`'s masked code hash. Nothing is deployed on any public network, so both rows in `attest::CANONICAL` were corrected in place rather than accumulated - the pre-deploy case its doc comment describes. `attest::Role` gained the matching variant and `tests/code_registry_e2e.rs` now publishes all five role values through a deployed registry and decodes them back, which is the only place the numbering can actually be settled. The canonical set now spans five distinct offset tables rather than four, well inside the wrapper's candidate budget of sixteen.

**Files** - `contracts/src/Rub3Registry.sol` (new), `contracts/test/Rub3Registry.t.sol` (new), `contracts/src/Rub3CodeRegistry.sol` (the new `Role` variant), `contracts/canonical-bytecode.json`, `crates/rub3-wrapper/src/attest.rs`, `crates/rub3-wrapper/tests/code_registry_e2e.rs`, `contracts/contracts.md`, `architecture.md`, `README.md`, `testing.md`.

**Tests** - 69 new in `test/Rub3Registry.t.sol`, `forge test` 293 passing (up from 224). `scripts/canonical-bytecode-hashes.sh check` and `scripts/check-deployments.sh` green. The anvil-gated `code_registry_e2e` passes against a real chain with the fifth role over the wire.

**Deliberately not built here, and why each one waits.**
- **Deployment, and anything that would decide one.** The factory and the registry launch together (§2.3) and every entry in `contracts/deployments.json` is still `null`. No address was invented, no field was added for one, and key custody and the deploy plan are held open as a separate decision.
- **The ENS records.** `appName.rub3.eth → contract` is a name-service write, not a registry write, and the wrapper resolves no name today. Putting an ENS record behind `register` would add an owner-key dependency on a resolver to a contract whose whole bound is that it can only affect discovery, and it would do so before anything reads it.
- **`contentURI` on the licence contract.** That is §3.1. The registry holds the field so a listing can quote it in the meantime, and it is the only card field other than `appName` that is not read off the licence contract.
- **An indexer, a front end, and any listing taxonomy.** Categories, search and editorial ranking beyond the recognised-token rule are product decisions, and the recognised-token list is deliberately the only judgement this contract holds.

### 3.3 - Agent-facing surface `[partial]`

Distribution to the machines doing the integration research.

- `llms.txt` + docs served as clean Markdown (the repo's docs are already agent-legible; formalize it). **Built.**
- Docs MCP server so Claude Code / Cursor pull real method signatures and contract ABIs instead of hallucinating them. **Built.**
- One-shot quickstart: a single self-contained prompt/script - "paste this into your coding agent and your binary is wallet-gated on Base Sepolia in minutes" - deterministic, testnet-safe, verifiable. Market that fact explicitly. **Not built; blocked on a testnet deploy, see below.**
- Listings: blockchain/MCP server directories, x402-adjacent catalogs (once §2.2 lands), ERC-8004 registries. **Not built; not an engineering call.**
- **Beachhead:** wallet-gated MCP servers - ship the example (`examples/hello-mcp/`) and target paid-MCP developers as design partners. **Not built; deferred with the quickstart it illustrates.**

**`llms.txt`, and agent-legibility as a checked property rather than a habit.** The documents were already written to be read by a machine, so this was never a rewrite: the whole doc-side diff is eighteen code fences that declared no language and now declare `text` or `bash`. What was missing was the *check*. Nothing failed when a fence lost its tag, when a cross-reference pointed at a heading that had since been renamed, or when a new document arrived without the first-paragraph statement of what it owns that makes every other document in this repository quotable on its own. Those are now assertions, stated as properties rather than as lists of known-good files, so a document a later branch adds is held to them too. Measured before writing them: every relative link and anchor in the six subject documents already resolved, no document skipped a heading level, and the em dash sweep of #42 had not regressed - the gate found nothing but the fences, which is the expected result for a check added to work that was already correct.

`llms.txt` itself follows llmstxt.org **v2** (last modified 2026-08-10), read rather than remembered, and the conformance test says so in its own doc comment: title, summary blockquote, heading-free details, then H2-delimited link lists. v2 dropped the `llms_txt2ctx` expansion tooling and the special meaning of the `Optional` section, so nothing here depends on either; the section is used for its conventional meaning only. Links are absolute `raw.githubusercontent.com` URLs, which is the clean-Markdown representation the specification asks for, and absolute because this file is routinely read detached from the tree it describes. The details section carries the three things an integrating agent most needs before it reads anything else: what the contracts guarantee, that those guarantees are absences from bytecode rather than promises, and that **nothing is deployed to any public network yet**.

Two parts of the specification do not apply yet, and are worth naming so that neither reads later as an oversight. v2's discoverability mechanism is HTML link relations (`rel="alternate" type="text/markdown"`, `rel="describedby"`) or the equivalent HTTP headers, and there is no rub3 website to carry either; when there is one, the file also belongs at its `/llms.txt`. And the "clean Markdown version of each page" the specification asks for is already what a repository serves, since the documents are Markdown to begin with - which is why the links point at raw rather than at the `blob` viewer.

**The docs MCP server** - `crates/rub3-docs-mcp`, a second workspace member. Six tools: `list_documents` (inventory, purposes, full heading outlines), `read_document` (whole document or one section by heading text or anchor, subsections included), `search_documents`, `list_contracts`, `contract_abi` and `rust_api`. It runs on stdio, resolves the checkout from `--repo-root`, then `RUB3_REPO_ROOT`, then a walk up from the working directory, then the tree it was compiled in - the last because an MCP client starts a server with whatever working directory it happens to have, often `/`.

**Everything it serves is derived, and that is the tested claim rather than an intention.** Contract facts come from the artifacts `forge build` wrote, discovered the way `scripts/canonical-bytecode-hashes.sh` discovers them - by reading each artifact's own `compilationTarget` - so no Solidity is parsed and no signature is written down anywhere in the crate. Selectors are read out of the artifact's `methodIdentifiers` map rather than hashed here, which keeps a keccak implementation whose agreement with solc would need proving out of the crate entirely. Rust signatures are *byte ranges of the source file*: `syn` locates the item and the text handed back is `source[span]`, so a served signature that is not in the file is a slicing bug the suite catches, not a stale copy nobody notices. Each item carries the `#[cfg(...)]` that governs it and each module the gate the crate root puts on it, because an API listing of this workspace without the feature gate beside it is misleading - whole modules do not exist under the lower tier bundles.

A `pub use` is resolved to the declaration it points at, within the same crate and under the same rule: the target is located with `syn` and the text handed back is the slice of the file that declares it, which the answer names. The wrapper's two front doors are why. `supervisor_run` is one hop out of a private file module and `ensure_headless` is two, `lib.rs` re-exporting what `activation.rs` re-exports out of its private `headless` module, so a listing that stopped at the statement carried no parameter list for either - and those are the calls an integrating agent makes. A target already described under its own `pub mod` is left as the statement rather than repeated, decided on where the declaration is rather than on how the module is spelled. A leaf that does not resolve - an external crate, a glob, a `super::` path - contributes nothing, because a re-export whose target cannot be found is a fact this crate does not have.

**One derivation reads each file once, and keeps nothing between derivations.** `syn` records every parse in a `proc_macro2` thread-local source map that grows and never shrinks by itself, and its positions are a `u32`, so a server answering `rust_api` for the length of an agent session would eventually wrap them and slice the wrong bytes out of the right files. Two things bound it: each crate's derivation reads and parses a file at most once, which matters because a `pub use` is resolved a leaf at a time and the wrapper re-exports fourteen names out of its largest file, and `workspace` calls `proc_macro2::extra::invalidate_current_thread_spans` before it starts, which is the API `proc_macro2` documents for exactly this workload. Neither is a cache of answers: the parse cache lives for one call, because a docs server answering from a snapshot is the defect the whole crate is built against.

**A record that is absent is not a failure, but a fact that cannot be derived still is.** Three refusals name what does exist rather than answering emptily - an unknown crate, an unknown module, and an item name nothing matches - because `{"crates": []}` reads to an agent as "there is no such API" and sends it back to inventing the signature. Each of them names what is in scope instead, and says so plainly when nothing is, since a colon followed by nothing reads as the server having lost the answer. What is *not* refused is a module that exists and exposes nothing: one rule holds at every filter, that a module doc is derived content this tool serves, so such a module is answered with its `//!` whether it is reached unfiltered, by `module`, by `name` or through a truncated page. A crate root of `//! What this is.` over nothing but `pub mod` declarations is an ordinary shape and that documentation has nowhere else to surface, since the walk describes no `mod` item. Only what a filter or the cap *emptied* is dropped, module and crate alike: that is an artifact of the query rather than an answer. Against that, a document that is not readable UTF-8 costs that document and not `list_documents`, `read_document` and `search_documents` together, and a missing `contracts/canonical-bytecode.json` costs the published fingerprints and not the contract surface, which is derived from the artifacts and does not consult the manifest for anything else.

**What it returns is bounded, and the bound is reported.** `rust_api {}` is a legitimate first call and the whole surface is on the order of a hundred kilobytes of JSON with the doc comments in it, so it is capped at `limit` items, 100 by default and 500 at most, and the response carries `truncated`. That is the bound `search_documents` already puts on hits rather than a second style, and reporting it is what lets a caller tell a first page from the whole API instead of believing the page. It is a count of top-level items and not of bytes, so it bounds how much of the surface comes back rather than the size of the answer: each item still carries its members and its doc comment, and the doc comments are most of the weight. A filter is what makes a small answer; the cap is what stops an unfiltered one running away.

The corollary is the part worth defending: **a fact that cannot be derived is not served.** `contracts/out/` is not checked in, so the two contract tools return an error naming `cd contracts && forge build` instead of answering from a remembered ABI. That is the failure mode `attest`'s manifest mirror test and the fingerprint gate were both built to prevent, and a docs server is the place it would rot fastest, because nothing downstream would notice a wrong signature until an agent had written calldata against it.

**Why the official SDK rather than a hand-rolled JSON-RPC loop.** The protocol moved twice in the year to August 2026, and the `2026-07-28` revision removes the `initialize` handshake outright in favour of a stateless `server/discover`, adds a required `resultType` to every result and cache hints to every list, while clients in the field still speak `2025-11-25` and older. Version negotiation is therefore real work with a real failure mode, and none of it is rub3's work: `rmcp` 3.1.2 implements all four revisions. What this crate owns is the derivation.

**It stays off the wrapper's dependency path**, which was the constraint on where it could live. A separate member that the wrapper does not depend on adds nothing to a shipped binary: `cargo tree -p rub3-wrapper` lists no `rmcp`, `pulldown-cmark`, `schemars` or `toml`, on the default bundle and on `tier-3,headless`. That is a CI step in the docs job rather than a sentence here, on the rule the fingerprint gate and `attest`'s mirror test already set: an invariant this load-bearing is checked by its real consumer, because prose cannot fail. Its version requirements are the workspace's loose ones (`tokio = "1"`, `clap = "4"`) rather than precise minimums, for the second half of the same constraint: Cargo unifies semver-compatible versions across a workspace, so a developer-only member pinning a floor moves the shipped crate's locked versions with it. Adding this member leaves every dependency the shipped binary links at the version it was already locked to. One lockfile entry did move, `toml_parser` 1.1.2 to 1.1.3, pulled by the new `toml` requirement; it is reached only through the proc-macro build graph, whose features resolver 2 does not unify with normal dependencies, so it is not on the shipped path this paragraph is about. The workspace-wide `cargo clippy --workspace --all-targets -- -D warnings` in CI already covers the new crate; its tests needed a job, which also builds the contracts first and sets `RUB3_DOCS_MCP_REQUIRE_ARTIFACTS=1` so that the artifact-dependent tests cannot silently skip and report green.

**Files** - `crates/rub3-docs-mcp/` (`main.rs`, `lib.rs`, `repo.rs`, `docs.rs`, `solidity.rs`, `rustapi.rs`, `server.rs`, and three test suites), `llms.txt`, `Cargo.toml` (the new member), `README.md` (the per-module map, a "Docs MCP server" section, and the dependency table's scope), `testing.md` (the suite inventory), `.github/workflows/ci.yml` (the `docs` job), `AGENTS.md`, and eighteen code-fence tags across `README.md`, `architecture.md`, `implementation.md`, `ideation.md` and `contracts/contracts.md`.

**Tests** - 50 in the new crate: 14 unit, 27 in `derivation.rs`, 8 in `docs_legibility.rs`, 1 in `mcp_stdio.rs`.
- Seven of the derivation tests build a throwaway checkout, ask a question, edit the file the answer came from, and ask again *through the same server instance*. An answer that survives its own source being edited is either a hardcoded string or a cache, and for a docs server those are the same defect. Covered that way: a renamed function, a feature gate added to a crate root, a declaration re-exported out of a private module, an edited ABI entry, a selector removed from an artifact, a moved canonical fingerprint, and a document added after startup
- `the_wrappers_reexported_front_doors_carry_their_declarations` holds the resolution against this repository rather than against the fixture: `supervisor_run` and `ensure_headless` must each answer with a `pub fn` line that is in the file the answer names, at the line it names. It fails if either declaration moves and the served answer does not, which is the whole point of resolving them rather than serving the `pub use`
- `a_second_derivation_agrees_with_the_first` derives the workspace twice and holds the second answer to the first, byte for byte. `rustapi::workspace` drops the spans of every earlier derivation before it starts anything, and that placement is load-bearing: dropping them any later invalidates positions a derivation in progress still holds, and the result is not an error but a signature of the right shape sliced out of the wrong file
- `every_served_rust_signature_is_a_verbatim_slice_of_its_file` walks the whole workspace and asserts every served signature, member signature and `cfg` appears byte for byte in the file it is attributed to, at the line it is attributed to. This is the one that would fail the first time a signature was assembled by re-rendering an AST instead of sliced
- `every_rendered_function_signature_matches_the_artifacts_own_selector_map` cross-checks two independent fields of the same artifact: every rendered canonical signature must be a key of `methodIdentifiers` with the selector to match, and every function solc emitted a selector for must be served. A tuple parameter expanded wrongly - `tuple[]` over two `uint256` members is `(uint256,uint256)[]` - changes the selector while still reading plausibly, and this is what catches it rather than an agent's failed transaction
- `no_served_signature_carries_an_attribute_or_a_doc_comment` is the other half of "verbatim": a `syn` span covers an item's attributes, so slicing whole spans hands back a doc comment as part of the signature. It caught exactly that on enum variants and impl headers, which is why each kind is now sliced from the token that begins the declaration
- `mcp_stdio.rs` spawns the built binary and speaks newline-delimited JSON-RPC to it. It is the only test that can observe stdout being clean, which the protocol depends on absolutely: one stray `println!` anywhere in the crate corrupts every message after it. It also holds the server to identifying itself as `rub3-docs-mcp` - `Implementation::from_build_env()` reports the SDK's own name, which is a real confusion in a client listing several rmcp-based servers
- The gate suite was verified to go red by breaking each property in turn (an untagged fence, an em dash, a dangling anchor, a document removed from `llms.txt`) rather than only observed to pass
- Full 9-bundle wrapper matrix green and unaffected, as expected for a change that touches no wrapper file. `--lib` counts: `tier-0` 51, `tier-1` 81, `tier-2` 96, `tier-3`/`tier-4`/`tier-3,binary-encryption` 106, `tier-2,webview` 97, `tier-3,webview` 117, `tier-3,headless` 169

**Deliberately not built here, and why each one waits.**
- **The one-shot quickstart.** It cannot be honest yet. Nothing is deployed to any public network - every entry in `contracts/deployments.json` is `null` on purpose, and §2.3 has the factory and the registry launching together - so a prompt promising a wallet-gated binary on Base Sepolia in minutes would either be untrue or would hardcode an address that does not exist. Filed separately; it unblocks the moment a testnet deploy exists, and not before
- **`examples/hello-mcp/`.** It exists to illustrate that quickstart, so it is deferred with it rather than shipped orphaned pointing at nothing deployed
- **Directory and catalog listings, and any positioning toward paid-MCP developers.** How rub3 is presented is not an engineering decision, and no marketing copy was written here

### 3.4 - Concurrent seats `[not started]`

Fleet licensing - the tier the agent economy actually wants.

- Generalize tier 3's single `activeSessionId` into an on-chain semaphore: `maxConcurrentSessions[tokenId] = K` (set at purchase tier / deploy), `activate()` admits up to K live session ids per token, `release()` (or TTL lapse) frees a seat.
- One license NFT = K concurrent fleet instances; buy another token to scale. Cooldown still rate-limits churn.
- Wrapper: seat-aware activation + a clear "fleet exhausted, N seats in use" error for orchestrators.

### 3.5 - rub3 SDK crate `[complete]` *(moved from old §2.3)*

The library a wrapped application links so it can ask the wrapper who is running it. Two calls, `rub3::heartbeat()` and `rub3::session()`, over a per-launch local endpoint the wrapper publishes in one environment variable. New workspace member `crates/rub3-sdk/`, package `rub3` so the call reads `rub3::session()`; the wrapper's half is `crates/rub3-wrapper/src/sdk.rs` behind a new `sdk` feature.

**The threat-model conclusion, settled before the transport was chosen: `heartbeat()` is an honest-integration and liveness aid, and nothing more.** It is stated that way in the crate's own documentation, in `architecture.md`, and in `sdk.rs`'s header, rather than implied away.
- The wrapper enforces licensing *before* it launches anything, and runs the application as its own child. By the time the endpoint exists the gate has already run, so what a live heartbeat establishes is that a wrapper is there and answering - not that anything it decided was correct
- The failures it does catch are the common ones and worth catching: an application run directly instead of through its wrapper, a wrapper that died mid-run, a stale address, a wrapper and an application built against different protocol versions
- **The other reading, a defence against a determined local attacker, a local socket cannot deliver, and building toward it would have been theatre.** Anyone who can run the wrapped binary outside its wrapper controls the machine: they can publish a socket of their own and answer every request however they like. Any credential this channel could demand would have to ship inside the binary they already control. So the channel carries no authentication, deliberately, and the documentation says why rather than leaving the absence to be read as an oversight
- The panic-on-absence contract survives that conclusion unchanged, which is why this shipped rather than coming back as a product question. A panic is what an assertion about a broken integration should do; it is not being sold as enforcement. `try_heartbeat()` and `try_session()` are the same checks returning an `Error`, for an application that would rather degrade than die

**The `user_id` rule is enforced by the type system, not by a comment** - `crates/rub3-sdk/src/info.rs`
- `SessionInfo::user_id` is a `UserId` implementing `Hash`, `Eq`, `Ord`, `Display` and `AsRef<str>`, so a `HashMap` key, a `BTreeMap` key, a sort or a path segment is the path of least resistance
- `SessionInfo::wallet` is a `Wallet` implementing `Display` and nothing else. Keying anything on it does not compile, and a `compile_fail` doctest asserts exactly that, so the guarantee is executable rather than asserted. What is left is what a wallet is legitimately for: showing it to a human and naming an address to the chain
- A licence can be sold or moved to a fresh key, so the wallet on the next session is a different address for the same licence and the same user. `user_id` is the wallet under the access model and the ERC-6551 account address under the account model, and the wrapper resolves that difference before the application sees it - application code never branches on the model

**`SessionInfo` carries exactly the six fields this section names, and a test pins that.** The signature, the nonce, the activation transaction and the device key stay on the wrapper's side: an application that could read the session signature could replay the session somewhere the wrapper never launched it, and one that never receives it cannot leak it either. `session_info_carries_exactly_the_six_specified_fields` compares the serialized key set against the literal list, and the e2e asserts the signature and nonce are absent from what a real wrapped process actually printed.

**Transport** - line-delimited JSON, one request per line, either side may send several over one connection
- Both halves read the envelope's protocol version *before* its body. A future version may change the body's shape entirely, and a peer speaking one deserves "we speak different versions" - which says repack the wrapper - rather than a JSON complaint, which says go looking for a broken connection
- Unix domain socket in a 0700 directory under `TMPDIR`, falling back to `/tmp` when `sockaddr_un`'s 104-byte path limit would be exceeded. The directory's mode is the access control; the name only has to be unique per launch
- Windows named pipe, written in two halves that are verified to two different degrees. **The SDK crate's client half type-checks for `x86_64-pc-windows-msvc`**: `cargo check -p rub3 --target x86_64-pc-windows-msvc` is clean, and it needs no `windows-sys` at all, because a named-pipe client is an ordinary file handle. **The wrapper's half, the named-pipe server in `sdk.rs`, is written but has never been compiled on a Windows-capable host in this project**, and nothing here can compile it: cross-checking `rub3-wrapper` for that target dies in `aws-lc-sys` (`rustls` <- `reqwest` <- `alloy`, an unconditional wrapper dependency), which compiles C against the Windows SDK and stops at `fatal error: 'windows.h' file not found` before `rub3-wrapper` itself is reached, and CI is macOS-only, so nothing else compiles it either. That server was written from the documented Win32 contract, and everything only a compiler would catch - its `windows-sys` feature selection, its import paths, its call signatures - is unverified. Neither half has been executed: no test here has run one of those calls. What closes it is a host carrying the Windows SDK, or a Windows CI job, where `cargo test -p rub3-wrapper --no-default-features --features tier-3,sdk` compiles the server before it runs it
- The unix client sets a 2-second read and write timeout. The Windows client sets none, and says so: a byte-mode pipe `File` has no equivalent, and overlapped I/O or a watchdog thread per call would each buy a bound on one failure mode at a real cost in machinery
- `RUB3_SDK_SOCKET` carries the address, and the wrapper *overwrites* it on every child in every bundle - with this launch's address, or with the `rub3:no-channel` sentinel when it serves none. An address inherited from the wrapper's own environment would point the application at somebody else's channel, and it would be answered. Because it is never left unset, the variable's absence means exactly one thing, which is what lets the SDK report the three states apart: absent is `NotWrapped`, the sentinel is `NoChannel`, an address nothing answers on is `Unreachable`. Without the sentinel a wrapper built without `sdk`, or one whose channel failed to start, reached the developer as "you did not launch this through a wrapper" - advice they had already followed, with the wrapper's own stderr warning the only correction and a child's panic printed under it. Both the variable's name and the sentinel are second copies of `rub3::wire::ADDRESS_ENV` and `ADDRESS_NO_CHANNEL`, in `supervisor.rs`, because the builds that publish them are exactly the ones that cannot name them; a unit test fails when either pair disagrees. The behaviour is covered on both sides of the feature: end to end in `tests/sdk_e2e.rs` for a build that serves a channel - including one whose channel is made to fail for real, by pointing `TMPDIR` at a directory that does not exist - and in `supervisor::tests`, which compiles in every bundle, for the builds that serve none
- The endpoint leaves nothing behind on any exit it can see. The 0700 directory goes with the `Channel`, a `serve` that fails after creating it removes it through a guard rather than through each fallible step remembering to, and an endpoint that outlived its wrapper - Ctrl-C is a SIGINT to the foreground group, which skips every destructor, as do SIGHUP, SIGKILL and a crash - is collected by the next `serve` on the machine. That sweep deletes directories on a shared temp path, so it removes only a name it can parse as one it wrote *and* whose pid is gone: another wrapper running at once is ordinary here, and its endpoint is a live channel. It is non-fatal throughout, for the same reason nothing else here gates a launch
- Minimal dependency footprint as specified: the SDK crate's whole tree is `serde`, `serde_json` and `chrono`. No `alloy`, no `wry`, no async runtime, no HTTP client. The wrapper reads the wire types from the SDK crate rather than keeping a second copy to drift

**`sdk` is an orthogonal feature, named by no tier bundle** - what an application may ask about itself does not depend on how hard the launch was gated, so it composes with any tier exactly as `binary-encryption` does. Tier-0 gets heartbeats and no session, which is honest: there is no session model compiled in to report. A launch served from the legacy `LicenseProof` also reports no session, because that record predates the identity model and has no `user_id`; synthesising one from the wallet would invent a second identity notion next to the one `session.rs` already owns properly. In practice a session is reportable from tier-3 up, since `ensure`'s session fast path and the window's `SessionSuccess` are both gated on `cooldown`.

**The channel never gates a launch.** A channel that fails to start is two warning lines on stderr, the no-channel sentinel in the child's environment, and the wrapped binary runs anyway. Refusing to start a program the user has already paid for because a socket could not be created would be a de-facto revocation surface, which §2.4 rules out - the same fail-open-on-launch posture as §2.6's attestation. Whether the application requires the channel is the developer's call, expressed by calling `heartbeat()` or not.

**Plumbing** - `activation::ensure` now returns a `Launch` instead of `()`, carrying whatever authorised the launch, and `supervisor::run` takes it. That is what stops the channel inventing a second session notion: it reports the session the launch was actually served from rather than re-reading the store and possibly picking a different one. `Launch` is empty in a bundle without `session`, so the launch path reads the same in every tier.

**Files** - `crates/rub3-sdk/` (`lib.rs`, `info.rs`, `wire.rs`, `transport.rs`), `crates/rub3-wrapper/src/sdk.rs` (new), `supervisor.rs` (`Launch`, `SDK_ADDRESS_ENV`, `SDK_ADDRESS_NO_CHANNEL`, the channel's lifetime), `activation.rs` + `main.rs` (the `Launch` return), `lib.rs` (`pub mod sdk` gated on `sdk`), `src/bin/rub3-sdk-probe.rs` (new), `tests/sdk_e2e.rs` (new), `tests/helpers/mod.rs` (`create_session_json`), root `Cargo.toml` (the new member)

**Tests** - 16 in the SDK crate plus 3 doctests, 17 in `sdk`, 11 end-to-end
- The e2e suite launches a **real wrapper process** whose child is a **real application linking the SDK**, and asserts only what that application printed: a live heartbeat, and the session field for field including an account-model `user_id` that differs from its signer
- The negative case is executable and was run: the same probe binary with no wrapper exits 101, and the panic text is asserted to name `RUB3_SDK_SOCKET` and to say `rub3-wrapper --binary`. The three ways a call can fail before it is answered are asserted apart, because each carries different advice: an address pointing at nothing reports a *dead* wrapper, no address at all reports a binary run directly, and a wrapper serving no channel reports itself as that - the last one driven by forcing a real bind failure (`TMPDIR` pointing at a directory that does not exist), not by simulating one
- Also covered end to end: the endpoint and its directory gone once the wrapper exits, the directory's mode asserted `drwx------` rather than assumed, a stale `RUB3_SDK_SOCKET` in the wrapper's own environment never reaching the child, and a legacy-proof launch reporting a heartbeat and no session
- `sdk`'s unit tests drive the request handler over a loopback stream: several requests on one connection, an unknown operation answered as an error rather than a dropped connection, a foreign protocol version told which two versions are in play, and a malformed line answered once before the connection ends. Three more use a real socket rather than that loopback, because the loopback always has the next request already buffered and so cannot see a read that finds nothing there yet: one asserts a dropped channel stops answering, one holds a single connection open and idles between two requests, which is the keep-alive contract and the one that catches an accepted socket left non-blocking (macOS and the BSDs inherit the listener's `O_NONBLOCK` across `accept`, Linux does not, and an `EAGAIN` there reads as a malformed request), and one serves several channels from a single process at once, which is what an endpoint name unique only per process fails
- Full 11-bundle matrix green, clippy and fmt clean across it. `--lib` counts as this phase left them, with `testing.md` carrying the current ones: `tier-0` 53, `tier-1` 83, `tier-2` 123, `tier-3`/`tier-4` 133, `tier-2,webview` 124, `tier-3,webview` 149, `tier-3,headless` 197, and the two new entries `tier-0,sdk` 65 and `tier-3,sdk` 150. Every bundle gains two tests against the counts `testing.md` carried before, and both are the new `supervisor` ones, which are not feature-gated: the unconditional address publish, and the no-channel sentinel a build serving no channel puts there instead. The sibling scrub test they sit beside was tightened rather than added. The `sdk` module's own 17 land only in the two bundles that compile it - a delta of +12 at tier-0 and +17 at tier-3, the difference being the five session-projection tests that need a session model to project

**Deliberately not built here**
- **No periodic push from the wrapper.** `architecture.md` sketched a 5-second heartbeat pushed at the application, which would have needed a policy for what the wrapper does when the application stops answering - and the only honest answers are "nothing" or "kill a paid-for launch". The application asks when it wants to know; that doc's runtime block now describes what exists
- **No session renewal over the channel.** An application cannot ask the wrapper to re-activate. That is a front-door decision and both doors already own it
- **No revocation channel.** Nothing on this channel can invalidate a running launch, by construction rather than by omission
- **Nothing for the MCP beachhead itself** (§3.3): this section is the dependency that section named, not that section

**Fixed in passing** - `architecture.md`'s defence-layer summary listed the heartbeat as "app cannot run without wrapper", which is precisely the claim the transport cannot support and the reason this section wrote its threat model down first. It now says what the channel proves.

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
- Purpose-built venue for license resale: queryable by agents, filtered to registry-verified contracts, priced in USDC. 1–2% marketplace fee + ERC-2981-style royalty split with the developer, venue-side: honoured by this marketplace on a sale it settles, not levied by the token. No contract under `contracts/src/` exposes `royaltyInfo`, and the licence contracts are immutable, so a deployed token can never advertise a royalty to a third-party marketplace.
- This is what makes "licenses as liquid capital assets" real: agents buy for a workload, resell when the job ends.

**Phase 4 deliverable:** revenue flows from all three billing models plus secondary trades, entirely on-chain, with no invoicing and no accounts receivable.

---

## Phase 5: Human Surface *(demoted, not dropped)*

The interactive path stays fully supported - manual tx-hash paste is the floor today and remains reachable forever. Polish lands after the agent path.

### 5.1 - Frictionless tx confirmation `[partial]`

Demoted from Phase 1, where it was specified as §1.10 before the agent-first revision; the spec below is the one that was picked up, with the four places auto-detect had to depart from it recorded under §5.1a "As built". The manual-paste floor already works and stays reachable forever, so richer confirmation modes are human-surface polish rather than a gap.

The purchase (§1.7) and activate (§1.8) flows originally asked the user to paste a transaction hash back into the webview after sending from their wallet, and still do whenever the Manual tab is the one in use. That manual-paste path is our robust fallback - it works with any wallet / any tool / any chain, requires no JS dependencies, and has no external points of failure. But it is not the UX we want people to see first. This section layers two richer confirmation modes on top, while leaving manual paste as the always-available floor.

**Three modes, in order of preference:**

| Mode | Status | Project ID | JS bundle | Offline tolerant | Relies on |
|---|---|---|---|---|---|
| `wallet-connect` | `[not started]` (§5.1b) | required (dev-supplied) | ~255 KB vendored | no | Reown relay + chain RPC |
| `auto-detect` | `[complete]` (§5.1a) | none | none | no | chain RPC only |
| `manual` (§1.7, §1.8) | `[complete]` | none | none | yes (paste later) | user copy/paste |

The three modes surface as three tabs on the cooldown / purchase screens. Two of the three are built, so a tier-3 build shows a two-tab strip today. The default tab at render time is the highest-capability one available for the current build:
- WalletConnect tab visible when the `wallet-connect` feature is compiled in **and** the developer supplied a non-placeholder `wc_project_id`. Neither exists yet: no tab, no vendored bundle, no project id anywhere in the tree.
- Auto-detect tab visible when `onchain-write` is on (always true for tier 3+, which is the only tier that reaches these screens). The screen payload carries `autoWatchSecs` only where a watch can run, and the page reads its presence as the tab's availability test - the same mechanism the WalletConnect tab will use for `wcProjectId`.
- Manual tab always visible. The strip itself is hidden when only one mode is available, because a strip offering no choice is decoration; the manual panel is shown either way.

Each tab drives the same two outbound IPC events (`purchase_tx_sent` / `activate_tx_sent`) - the downstream poller/finalize path from §1.7 and §1.8 Phase B is untouched. This keeps auto-detect and WalletConnect as pure front-door improvements rather than new branches in the session pipeline.

#### 5.1a - RPC auto-detect `[complete]`

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

**As built.** Four details differ from the sketch above, each because building it exposed something the sketch could not have known:

- `deadline` is an `rpc::Deadline`, which carries the budget *and* an `rpc::Cancel` flag. Cancellation and expiry are the same question asked of a person and of a clock, and a watch consulting only one of them would either outlive its screen or ignore its budget. Bundling them means no call site can pass a budget and forget the flag. `Cancel` is checked in 50 ms slices between polls, so a cancelled watch stops well inside the 3 s cadence rather than leaving a request in flight.
- The two ways a watch ends without a match are `RpcError::WatchEnded(WatchEnd::Timeout | WatchEnd::Cancelled)`. Nothing failed in either case, which is why the screen falls back rather than reporting an error, and why they are distinguishable from an endpoint that stopped answering. Beyond the budget, five consecutive retryable failures also end a watch: fifteen seconds of silence is an endpoint that will not answer, and spending the remaining budget on it only delays the paste that would have worked. The two reads a watch has to make before it can start - the head block it counts from, and the cooldown it may have to wait out - go through the same policy (`rpc::retry_read`) rather than failing on their first bad answer, because one 429 on the first request of the flow would otherwise end auto-detect a second after the screen rendered while the identical 429 one poll later is absorbed in silence.
- **`watch_for_activate` does not use the `eth_getBlockByNumber` + receipt scan specified above. That mechanism was abandoned during the build because it cannot run on Base at all.** Reading a transaction's `to` without fetching a receipt for it means asking for the block with its transactions inlined, and a Base block carries the OP-stack deposit transaction, whose `0x7e` type the wrapper's Ethereum transaction types refuse to decode; there is no `op-alloy` dependency in the workspace, so the block does not deserialize and the scan never reaches a `to` to compare. Falling back to one `eth_getTransactionReceipt` per transaction is hundreds of sequential requests per poll on a Base block, with one failure among them restarting the whole scan against an endpoint that is likely already rate limiting. What is used instead is the `Activated(uint256 indexed tokenId, ...)` log the contract already emits, fetched with one `eth_getLogs` pinned to the single block `lastActivationBlock` named - one request on any chain, and the same shape `watch_for_mint` uses. Of the two receipt fields the sketch matches on: `from == wallet` has no counterpart in the signature §5.1a gives this function, which carries no wallet, and the indexed token id it does carry is the sharper discriminator, naming the token this screen waits on rather than the account that paid the gas; `to == contract` is not dropped but moved into the filter, which pins `address == contract`, so a match is an event this contract emitted, a stronger statement than a transaction merely addressed to it. A reverted activation emits no log, so the revert check comes for free.
- An activation watch **holds until the cooldown ends** before it spends a second of its budget (`rpc::Deadline::starting_in`). The sketch has the budget start when the screen renders, which works only where the cooldown is shorter than the budget; the default is 1800 blocks, roughly an hour on Base, so an armed-immediately watch would poll for two minutes and hand back to the manual tab fifty-eight minutes before the contract would accept an `activate()` at all - auto-detect could never succeed on that screen. Holding loses nothing, because a watch reads state rather than events and still sees a transaction broadcast during the hold on its first poll, and cancellation is honoured throughout it. The wait is estimated once, in `webview::cooldown_wait`, and the same estimate drives the screen's own sentence and the bar that drains beside it.

**Where it is tested.** `rpc::watch_loop_tests` drives the loop with the network taken out (budget, cancellation, the retry threshold); `rpc::watch_rpc_tests` drives both watchers against a mocked node (the filter that goes out, the decoding that comes back); `webview::session_flow::auto_detect` proves at the IPC seam that a found hash and a pasted hash produce call-for-call the same flow, and that a watch stops when its screen does; and two `#[ignore]`d arms in `webview::session_flow::onchain` run both watchers against a live anvil.

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
- Windows support: MSVC target and WebView2 testing. The SDK channel's named-pipe half is written, and half of it is verified (§3.5): the SDK crate's client type-checks for `x86_64-pc-windows-msvc`, the wrapper's server has never been compiled anywhere in this project, and neither half has been executed - a Windows host or runner is what both are waiting for
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
| Packaging | `include_bytes!` embedding, driven by `rub3 pack` (§2.5) |

---

## Directory Structure

Current (implemented). The per-module map is not repeated here: README.md →
"Project structure" names every wrapper module, including the deferred
`device.rs` / `decrypt.rs` scaffolds.

```text
rub3/
├── crates/
│   ├── rub3-wrapper/                 # Wrapper runtime (src/, assets/activation.html, tests/)
│   ├── rub3-sdk/                     # §3.5 - the `rub3` crate a wrapped app links (heartbeat, session)
│   ├── rub3-cli/                     # §2.5 - the `rub3` command: pack, deploy
│   └── rub3-docs-mcp/                # §3.3 - docs MCP server, off the wrapper's dependency path
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
├── llms.txt                          # §3.3 - the agent's entry point to the documents
└── README.md
```

Planned (not yet created):

```text
├── crates/
│   └── tauri-plugin-rub3/       # §5.3
├── contracts/src/
│   ├── Rub3Metered.sol          # §4.1 - per-launch billing
│   └── Rub3Registry.sol         # §3.2 - discovery + agent cards
└── examples/
    ├── hello-mcp/               # §3.3 beachhead - wallet-gated MCP server
    ├── hello-rust/
    └── hello-subscription/
```
