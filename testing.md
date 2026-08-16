# Testing Guide

This file owns the test inventory: what each suite covers, how to run it, and how to set up manual testing. Per-test descriptions and suite counts belong here rather than in [implementation.md](implementation.md), which records status and cites the headline numbers. Contract-side operational recipes are in [contracts/contracts.md](contracts/contracts.md), the tier feature bundles the suites compile under are described in [README.md](README.md) and [AGENTS.md](AGENTS.md), and design rationale is in [architecture.md](architecture.md).

## Prerequisites

- Rust toolchain (rustc 1.91+): `rustup update stable`
- Optional: Foundry (`cast`, `anvil`) for manual wallet operations: `curl -L https://foundry.paradigm.xyz | bash && foundryup`
- Optional: Access to Base mainnet RPC (default: `https://mainnet.base.org`) for network tests

## 1. Run all tests

```bash
cargo test -p rub3-wrapper
```

This runs all unit tests, integration tests, and license e2e tests. No external tools required - wallet generation and signing are done natively in Rust via `k256`.

The default bundle is `tier-2` + `webview`, so it compiles neither the tier-3
capabilities nor the headless front door. Cargo features are additive, so
`--no-default-features` is mandatory when selecting another bundle:

```bash
# tier-3 (adds onchain-write + cooldown): 101 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib

# tier-3 + the headless (agent) front door: 162 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless --lib
```

For reference, `--lib` counts per bundle: `tier-0` 46, `tier-1` 76, `tier-2` 91,
`tier-3`/`tier-4` 101, `tier-3,headless` 162. Each total includes the one
`#[ignore]`d network test, which a plain run skips, so a bundle reports one
fewer as passed. `tier-1` and `tier-2` diverge because `attest` needs
`onchain-read`.

Network-dependent tests (requires internet). `--ignored` runs *only* the ignored tests, so this replaces the suite above rather than adding to it:

```bash
cargo test -p rub3-wrapper -- --ignored
```

Or use the convenience script:

```bash
scripts/test-e2e.sh
```

## 2. Test suites

### Unit tests (in `src/`)

- **`license::tests`** - activation message hashing, personal_sign prefix, proof serialization round-trips
- **`store::tests`** - proof save/load, directory creation, overwrite, missing file handling
- **`rpc::tests`** - provider construction, contract call error paths, `encode_activate_calldata` selector + layout, `get_tx_receipt` / `get_block_number` / `get_code` error paths, ENS stub; and for the EIP-3009 rail (§2.2) the `ReceiveWithAuthorization` typehash against its literal preimage, the signing digest against a vector computed independently with `cast`, every signed field proving it changes that digest, and the `purchaseWithAuthorization` calldata selector
- **`session::tests`** (requires `session` feature) - message determinism, tier-diffing, expiry edge cases, sign/verify round-trip, wrong-wallet failure; with `cooldown` adds: `verify_onchain` missing-field + bad-URL paths, `should_reverify` distribution sanity
- **`session_store::tests`** (requires `session` feature) - save/load round-trip, missing-session, `load_latest_session` picking the freshest valid session (`load_latest_session_for_wallet` narrows the same scan to one signer, covered from `activation::tests`)
- **`identity::tests`** - `IdentityModel` parsing and wire format, ERC-6551 TBA derivation determinism and sensitivity to each input, `resolve_user_id` for both models
- **`signer::tests`** (requires `headless` feature) - hex key parsing (bare/prefixed/padded, and every rejection: wrong length, non-hex, zero and out-of-curve-order scalars), `Debug` redaction and error messages asserted not to echo the input, `personal_sign` / `sign_prehash` recovery, RFC-6979 determinism, keystore decrypt, password-file precedence, and the strict env-key-over-keystore resolution order with no fall-through on a malformed key
- **`tx::tests`** (requires `headless` feature) - invalid-URL transport error, the node's `insufficient funds` classifier, and the shortfall message with and without known amounts
- **`activation::tests`** (requires `headless` feature) - the exit-code table asserted value-by-value, all classified codes distinct and disjoint from 0/1/2, `machine_detail` contents, `lowest_token` selection, the token- and wallet-scoped session fast path, every unconfirmed-purchase outcome mapping to the terminal code 21, and (§2.6) the `NotCanonicalContract` refusal: its message naming the function the pre-filter saw and stating that nothing was signed, its detail line reporting `code_bytes=` and `exposed=none` rather than an empty value, the factory case reporting `sells_licences=false`, and every classified code appearing in the `--help` table; and for the spend ceiling (§2.2) `SpendPolicy`: an unset `RUB3_AGENT_MAX_TOKEN_AMOUNT` leaving the rail unavailable rather than unlimited, the ceiling inclusive at the boundary, zero as a real ceiling rather than "unset", the refusal carrying `listed`/`maximum`/`token`, and every malformed value a hard `Config` error naming the variable; and for the pre-flight's authorization disclosure (§2.2) that the two signed copies share one salt - and therefore one single-use nonce - differing only in `validBefore`; and for the ETH ceiling (§2.7) the same properties on `check_eth_wei` plus the ones the default introduces - an unset `RUB3_AGENT_MAX_ETH_WEI` meaning `DEFAULT_MAX_ETH_WEI` rather than either zero or unlimited, an ordinary 0.01 ETH listing still buying under it, `0.05` rejected as a hard error naming wei as the unit, and neither rail's variable moving the other's. The relationship between the two windows themselves is a `const _: () = assert!(..)` beside the constants rather than a test, so a window that stopped being short fails the build in every build that compiles the headless front door - the constants live inside `mod headless`, so of the eight matrix bundles only `tier-3,headless` sees the assertion, and it is the only one where they exist at all
- **`rpc::stub_node_tests`** - the token-side call classifier (§2.2), driven through `stablecoin_rail` and `preflight_purchase_with_authorization` against a local stub endpoint answering one fixed body each, rather than asserted about the classifier in isolation: a revert (`code: 3`, and `-32000` with revert wording) or empty return data is a settled contract answer, so the rail reads as absent and the run continues on ETH; a JSON-RPC error body, an execution timeout and an undeserializable body are node failures that propagate instead of silently changing the currency
- **`rpc::tests::receipt_polling`** (requires `onchain-write` or `cooldown`) - the receipt poll loop driven over scripted answers: a transient transport failure does not end the wait, one that outlasts the budget is reported as `Transport`, a recovered poll ends as `Timeout`, a request that can never succeed (an unparseable tx hash) is reported at once instead of consuming the budget, and both outcomes report real wall-clock waiting time rather than the nominal budget
- **`attest::tests`** (requires `onchain-read`, so tier-2 and up) - the pre-purchase code check (§2.6). Drift: every fingerprint and immutable range in `contracts/canonical-bytecode.json` is pinned in `attest::CANONICAL` (the failure prints the row to add), the pinned hashes are lowercase hex of 32 bytes, the ranges are sorted, disjoint and one word wide, and `FORBIDDEN_SIGNATURES` is compared against the `string[N]` array in `contracts/test/Rub3Invariants.t.sol` with Solidity comments stripped first. Comparison: a legitimate deploy that chose different immutables still matches, a truncated deploy is refused rather than partially masked, an address with no code says so, and the selector helper is checked against the published `transfer(address,uint256)` vector. The negative case is `a_renamed_seizure_function_passes_the_name_scan_and_fails_the_hash` - an owner-only seizure named `reconcileLedger(uint256,address)` is asserted to pass the blacklist in silence and to fail the masked hash, which is the asymmetry the module exists for. Gate: `only_licence_roles_are_purchase_targets` runs `decide()` over the shipped table and requires exactly the `Role::Licence` rows accepted and every factory and deployer row refused as `NotALicence`, and `the_attest_module_is_reachable_only_from_the_purchase_path` walks the crate's own `src/` recursively and asserts three things: the set of modules referencing the module at all, by any item rather than only by calling the gate, is a subset of the purchase-path allowlist (`activation.rs`, `webview.rs`); `activation.rs` holds exactly one call site, since a subset is also satisfied by calling the gate nowhere; and the named human launch entry points inside `webview.rs` (`show_activate`, `show_cooldown`, `finalize_session`) reference it not at all, which is the half a file-granular allowlist cannot speak for, failing loudly rather than vacuously when one of those functions can no longer be found. That is how "fail closed on purchase, fail open on launch" is enforced structurally rather than by a default. It guards source structure, not runtime wiring, and it is not total: a new launch function added to `webview.rs` is unguarded until it is named, and the same file granularity means a reference elsewhere in `activation.rs` is not caught either. The behavioural half is the launch-path e2e below
- **`supervisor::tests`** (Unix) - the wrapped binary's own reported environment carries none of the `RUB3_AGENT_*` credential variables (the list is `agent_env::AGENT_ENV_VARS`; `RUB3_AGENT_MAX_TOKEN_AMOUNT` and `RUB3_AGENT_MAX_ETH_WEI` are spend policy rather than credentials and are deliberately not on it), covered for the raw-key source, the keystore-plus-password-file source (the documented preferred setup) and all sources at once

### Integration tests (`tests/integration.rs`)

Binary-level tests that spawn the wrapper process:

- `runs_child_and_exits_zero` - wrapper exits 0 when child succeeds
- `propagates_nonzero_exit_code` - wrapper forwards child's exit code
- `passes_args_to_child` - `--` separator passes trailing args to child
- `errors_on_missing_binary` - wrapper rejects nonexistent binary path

Each test provisions a valid license proof in a temp directory via `RUB3_LICENSE_DIR`.

### License E2E tests (`tests/license_e2e.rs`)

**Static tests** - use a deterministic test keypair (hardcoded private key `0xac0974...`). Fully reproducible:

- `static_license_verifies` - construct proof, verify signature recovery matches wallet address
- `static_license_loads_and_verifies` - write proof to disk, load it back, verify
- `static_wrapper_runs_with_valid_license` - run wrapper binary with a valid proof, assert child executes

**Dynamic tests** - generate a random wallet each run via `k256::ecdsa::SigningKey::random()`:

- `dynamic_wallet_generates_valid_signature` - prove the full crypto pipeline works with random keys
- `dynamic_license_round_trips` - generate, save, load, verify with fresh keypair
- `dynamic_wrapper_runs_with_fresh_license` - run wrapper with ephemeral license

**Signal handling:**

- `wrapper_forwards_sigterm` - spawn wrapper with `/bin/sleep`, send SIGTERM, assert clean exit

### Tier-3 on-chain session E2E (`tests/session_onchain_e2e.rs`)

Exercises `session::verify_onchain` against a live EVM node. Requires the Foundry toolchain (`anvil`, `forge`, `cast`) on PATH; gracefully prints `SKIP:` and returns when any of those are missing. Marked `#[ignore]` so default `cargo test` runs skip it.

- `session_verify_onchain_e2e` - spawns `anvil` on port 8547, deploys `Rub3Access` via `forge create`, runs `purchase(address)` + `activate(uint256)` via `cast send`, extracts the receipt's block hash, then:
  - asserts `verify_onchain` succeeds for a correctly-populated session,
  - tampers the `contract` field → `VerifyError::ContractMismatch`,
  - tampers the `activation_block_hash` → `VerifyError::BlockHashMismatch`,
  - points `activation_tx` at a non-existent hash → `VerifyError::ReceiptNotFound`.

Run with:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3 \
    -- --ignored session_verify_onchain_e2e
```

### Headless (agent) E2E (`tests/headless_e2e.rs`)

Drives `activation::ensure_headless` end to end with no webview involved: the test binary links neither `wry` nor `tao`. Same gating as the suite above (`#[ignore]`, prints `SKIP:` and passes when Foundry is absent). Runs anvil on port **8549** so it can run alongside `session_onchain_e2e.rs` (8547), and serialises its own tests through a file-level mutex covering the port and the process-global env vars, so no `--test-threads=1` is needed. Every test generates a fresh key and resolves it through the real `RUB3_AGENT_KEY` path with an isolated `RUB3_SESSION_DIR`.

- `headless_purchase_activate_persist_e2e` - funds a fresh key, purchases at 0.01 ETH, activates, asserts ownership / `nextTokenId` / every session field / `verify_local` / `verify_onchain` / on-disk persistence, then relaunches and asserts `Reused` with no new mint
- `headless_insufficient_funds_e2e` - unfunded key → exit code 11
- `headless_sold_out_e2e` - supply cap of 1, already minted → exit code 12
- `headless_cooldown_active_then_ready_e2e` - exit code 13 reporting `blocks_remaining`, then succeeds after `anvil_mine` with `session_id` bumped to 2
- `headless_explicit_token_not_owned_e2e` - `--token-id` the signer does not hold → exit code 20, minting nothing
- `headless_explicit_token_id_does_not_reuse_another_tokens_session_e2e` - a cached session for a different token cannot satisfy `--token-id` → exit code 20
- `headless_chain_id_mismatch_e2e` - endpoint chain id ≠ build chain id → exit code 19, refused before anything is signed
- `headless_purchases_on_the_stablecoin_rail_e2e` (§2.2) - deploys the EIP-3009 mock from `contracts/test/mocks/`, deploys a contract listing both rails, mints USDC to the agent, and asserts the outcome names `PaymentRail::Erc3009`, that exactly the listed stablecoin amount left the agent and arrived at the contract, and that the ETH spent is gas only - far under the ETH price the same contract lists. Balances are read with `cast`, not through the code under test
- `headless_falls_back_to_eth_without_stablecoin_balance_e2e` (§2.2) - same contract, an agent holding no USDC → `PaymentRail::Eth`, with nothing paid in USDC
- `headless_falls_back_to_eth_without_a_spend_ceiling_e2e` (§2.2) - a funded agent against an advertised rail with `RUB3_AGENT_MAX_TOKEN_AMOUNT` unset → `PaymentRail::Eth`, the agent's stablecoin balance untouched and the licence still obtained. An unset ceiling makes the rail unavailable, not unlimited
- `headless_refuses_a_price_above_the_spend_ceiling_e2e` (§2.2) - a rail that is advertised, affordable and domain readable, with a ceiling one unit under the listed price → exit code 22, `listed=`/`maximum=`/`token=` on the detail line, `nextTokenId` still 0 and the agent's ETH balance unchanged to the wei, so the refusal cannot have become an ETH purchase. A refusal, not a fallback
- `headless_signs_nothing_when_the_price_is_above_the_spend_ceiling_e2e` (§2.2) - the same refusal driven through a `CountingSigner` delegate that wraps the resolved signer and counts `sign_prehash` calls → exactly zero calls, alongside `PriceAbovePolicy` / exit 22 and an unchanged `nextTokenId`. The exit code alone cannot tell a refusal that signed nothing from one that signed a valid authorization for the refused amount and shipped it to the RPC endpoint as pre-flight calldata; since anyone may submit a `purchaseWithAuthorization`, that disclosure is the spend. This is the assertion that pins the ordering
- `headless_buys_in_eth_when_it_holds_none_of_an_over_ceiling_token_e2e` (§2.2) - the same contract and the same ceiling, but a wallet holding none of the payment token → succeeds on `PaymentRail::Eth`. The ceiling is weighed after affordability, so an agent that could not have spent the token is not refused over it; this is the regression case for "nothing that bought a licence before §2.2 starts failing". Paired deliberately with the test above, because a future reordering is most likely to collapse the two
- `headless_falls_back_to_eth_when_the_token_has_no_domain_separator_e2e` (§2.2) - a payment token that passes the licence contract's constructor probe but exposes no `DOMAIN_SEPARATOR()` (`NoDomainSeparatorEIP3009Token` in the mocks file) → `PaymentRail::Eth`, not an aborted activation
- `headless_falls_back_to_eth_when_the_token_lacks_the_signature_overload_e2e` (§2.2) - a conforming EIP-3009 token that implements only the `(v, r, s)` form (`NoSignatureOverloadEIP3009Token`), advertised, affordable, signable and priced *exactly at* the ceiling so it clears policy and the run reaches the pre-flight, which is the only check that can catch it. The `eth_call` of the real `purchaseWithAuthorization` does → `PaymentRail::Eth`, the agent's whole token balance intact, the licence still obtained and the session verified on-chain. A ceiling below this price would refuse on price instead and stop exercising the fallback
- `headless_disclosed_authorization_expires_before_the_endpoint_can_spend_it_e2e` (§2.2) - the fallback path's own hazard, driven through a `RecordingProxy` that relays every call to anvil but answers the `purchaseWithAuthorization` pre-flight with `execution reverted` and keeps the calldata. The token is the *working* mock, so the authorization it holds is a live payment instrument. The run falls back → `PaymentRail::Eth`, then the test reads `validBefore` out of the bytes that actually left the machine (seconds, not minutes), then runs that captured calldata as a `cast call` before warping - a simulation that moves no money, and a positive control: it must *succeed*, proving the blob is well-formed, correctly extracted and a genuinely live payment instrument at that moment, so the test cannot pass on a truncated or mis-extracted one. Only then does it warp the chain past `validBefore` with `evm_increaseTime` and replay the calldata verbatim from a third-party key: the replay must fail, and the refusal must name the expiry (the mock's `AuthorizationExpired()`, matched as either the decoded name or the bare `0f05f5bf` selector `cast` prints from raw calldata) rather than being any failure at all, with the agent's stablecoin balance untouched and `nextTokenId` 1. Against the pre-fix 900-second window that replay succeeds and the buyer has paid for one licence in two currencies
- `headless_broadcasts_a_longer_window_than_it_discloses_e2e` (§2.2) - the same proxy relaying faithfully through a successful stablecoin purchase, reading both copies off the wire: the `eth_call` copy expires in seconds, the `eth_sendRawTransaction` copy has room to be mined. The two windows solve opposite problems, and this is what stops the short one from silently becoming the long one
- `headless_transport_failure_on_a_token_read_is_a_hard_error_e2e` (§2.2) - the same funded, within-policy setup pointed at a dead endpoint → `HeadlessError::Rpc` and nothing bought, plus direct assertions that `rpc::erc20_balance_of` and `rpc::token_domain_separator` each classify a dead socket as transport rather than as a contract answer
- `headless_transport_failure_on_the_token_balance_read_is_a_hard_error_e2e` and `headless_transport_failure_on_the_domain_separator_read_is_a_hard_error_e2e` (§2.2) - a proxy relays every call to anvil except one, the payment token's `balanceOf` and then its `DOMAIN_SEPARATOR()`, whose connection it closes unanswered → `HeadlessError::Rpc` and `nextTokenId` still 0. These are what actually exercise "a blinking node must never silently change the currency": delete either `is_transport` arm from `choose_rail` and the run selects ETH and succeeds, so both fail

- `headless_factory_deploy_splits_the_eth_payment_e2e` (§2.3) - deploys a `Rub3Factory`, deploys a licence through it, and runs a real agent purchase on the ETH rail. Asserts the terms were stamped (`feeBps()`), the factory recorded the contract (`isDeployed`), the accrual matches the rate, and that after `withdrawFees()` + `withdraw(address)` the treasury and the developer between them hold the whole payment with the contract at zero. Balances are read with `cast`, not through the code under test
- `headless_factory_deploy_splits_the_stablecoin_payment_e2e` (§2.3) - the same on the stablecoin rail, against `tokenFeesAccrued` / `withdrawTokenFees` / `withdrawToken`. Together these are the "identical on both rails" claim checked against a chain rather than in the EVM harness
- `headless_refuses_a_contract_whose_code_is_not_canonical_e2e` (§2.6) - points the flow at a real, working, fully deployed contract that is not a rub3 licence (the EIP-3009 mock) → `NotCanonicalContract` / exit 23 naming the refused address, with the signer's nonce *and* ETH balance unchanged read through `cast`. The nonce is the executable form of "no transaction was sent": a refusal that has already broadcast something moves it whatever else it reports
- `headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e` (§2.6) - the launch half of "fail closed on purchase, fail open on launch": buys once through the gate, removes the cached session so the fast path cannot answer from disk, mines past the cooldown, and relaunches → `Activated` rather than `PurchasedAndActivated`, the same token id back, and `nextTokenId` unchanged read through `cast`. It pins the structural claim - a launch is a different code path that never reaches the gate - and deliberately not the behaviour when the check cannot complete, which needs a non-canonical licence fixture in `contracts/`
- `headless_refuses_an_eth_price_above_the_spend_ceiling_before_sending_e2e` (§2.7) - an ETH-only contract with `RUB3_AGENT_MAX_ETH_WEI` one wei under the listed price → exit code 22 and `rail=eth listed= maximum=` with no `token=` key, with four independent witnesses that nothing was broadcast: a `CountingSigner` never asked to sign, an unmoved nonce, an unmoved ETH balance, and `nextTokenId` still 0. The contract requires exact payment, so an over-ceiling purchase would revert on-chain anyway; what this pins is that it is refused *locally*, before `tx::send`, so it costs no gas and arrives as a policy answer rather than a chain error
- `headless_buys_at_exactly_the_eth_ceiling_and_under_the_default_e2e` (§2.7) - the inclusive boundary against a real chain (a ceiling set to exactly the listed price buys), then the same listing bought by a second agent with no ceiling variable set at all, under the built-in default. The second half is the regression case for "the default is not a breaking change": an operator who configures nothing still buys
- `headless_direct_deploy_pays_no_fee_and_is_unrecorded_e2e` (§2.3) - the counterweight: a directly deployed contract sells identically, accrues nothing, keeps the whole price for the developer, and reports `isDeployed == false`. Direct deployment is unrecorded, not penalised

Every test that means to use the stablecoin rail sets `RUB3_AGENT_MAX_TOKEN_AMOUNT` explicitly; the `Agent` fixture clears it, and `RUB3_AGENT_MAX_ETH_WEI` with it, on construction and on drop, so no test inherits another's ceiling. Clearing the ETH one restores the built-in default rather than removing the ceiling - that rail always has one - which is why every other ETH purchase in the file runs without setting it.

Run with:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless \
    -- --ignored headless
```

### Solidity suite (`contracts/`, run with `forge test` from `contracts/`)

In-process EVM: no network, no `.env`. 189 tests across five files.

- **`test/Rub3Access.t.sol`** (28) - metadata, constructor validation, purchase (including the exact-payment rule: under, over, exact, and a 256-run fuzz proving the listed price is the only accepted amount), supply cap, activation and cooldown, owner gating
- **`test/Rub3Subscription.t.sol`** (15) - expiry, renewal (exact payment on both under and over), `isValid`, and the per-token `renewPrice` snapshot of §2.4
- **`test/Rub3Invariants.t.sol`** (50) - the ownership invariants, in four groups: 18 on the append-only hash set (constructor seeding, zero/duplicate rejection, older releases staying valid, revocation status/reason/events, revoked hashes not resurrectable, owner gating), 16 on the successor pattern (opt-in on both sides, one claim per token, the claim following the current holder, survival of renounced ownership, frozen subscription terms carried across a successor declaring a different `period`, and the trust rule surviving a successor repoint), 11 on mint ordering and predecessor typing (a `MintCallbackProbe` recipient reading a token's already-frozen terms from inside `onERC721Received` on both the purchase and the claim path, and `IncompatiblePredecessor` from each probe against every mistyped or truncated predecessor), and 5 on the no-revocation audit (30 forbidden signatures × 4 deployed contracts, the fourth being the §2.3 `Rub3Factory`, with a positive control proving the scanner finds selectors that do exist - including `feeBps()` and `treasury()`, the getters that exist while every setter for them is absent)
- **`test/Rub3TokenPurchase.t.sol`** (42) - the EIP-3009 stablecoin rail of §2.2. The buyer holds stablecoin and a zero ETH balance in every test, and a separate submitter sends every transaction: replay, front-running (diverting the mint, stripping it by calling the token directly, and calling `receiveWithAuthorization` as a third party), authorizations aimed at the wrong contract / wrong intent / wrong token id, the validity window and cancellation, a price move after the read rejecting on *both* rails (the stablecoin one through the digest, the ETH one through the exact-payment check), the balance-delta check, the constructor probes, both rails minting identically, subscription renewal terms frozen on both rails, an EIP-1271 smart-contract wallet buying and renewing (and a signature it rejects buying nothing), and a token implementing only EIP-3009's `(v, r, s)` form deploying happily and then being unspendable for that reason alone (signed against its own domain, empty revert data, the same fields spent through the split form it does implement, and the same authorization shape minting against the mock)
- **`test/Rub3Factory.t.sol`** (54) - the factory and the protocol fee of §2.3, in eight groups: the factory itself (terms stamped on both models, `isDeployed` plus ordered enumeration, owner defaults, the `LicenseDeployed` log, the fee range rejected either side and accepted at both ends, and the two contract-size limits); immutability (a newer factory at a different rate leaving an older deploy untouched in terms *and* money, disjoint per-factory registries, and the contract owner running every power it has without moving the fee); exact ETH arithmetic at the boundaries (1 wei, the 39/40-wei rounding edge at 250 bps, an indivisible amount, 1,000,000 ether, and a 256-run fuzz over amount x rate); the same on the stablecoin rail; `test_bothRails_chargeIdenticallyForTheSameAmount`, which prices one contract at the same number in wei and in the token's smallest unit and asserts the two accruals are equal; direct deployment working, unrecorded, and unpenalised; fee evasion pinned from both sides by `test_eth_feeIsChargedOnTheListedPriceBecauseNothingElseArrives` and `test_eth_zeroPriceListingCannotCollectByOverpaying` - the inverted forms of two tests that used to assert an overpayment was taxed, closing the route at the payment now that neither rail can deliver more than the listed price - and the accrual rationale by `test_accrual_rejectingTreasuryCannotBlockPurchases`, where a treasury that refuses ETH fails only its own sweep while buyers still buy and the developer is still paid in full; and the canonical-predecessor rule (the laundering route reverting `PredecessorNotCanonical` and recording nothing, a canonical predecessor accepted with the migration completing end to end, the zero predecessor, the subscription path, cross-factory acceptance through `previousFactory` and its absence on an unlinked factory, both sides of the `MAX_PREDECESSOR_FACTORY_HOPS` bound, the constructor probe rejecting a non-contract and a half-answering `previousFactory`, and direct deploys and the deployer helpers staying unconstrained and unrecorded)
- **`test/mocks/MockEIP3009Token.sol`** - a faithful minimal EIP-3009 token standing in for USDC, validating signatures through OpenZeppelin's `SignatureChecker` exactly as Circle's FiatTokenV2_2 does, plus a silent token, a non-token, a token with no `DOMAIN_SEPARATOR()`, a token with only the split-signature form, and a `SmartWallet` EIP-1271 buyer. Why a mock rather than a fork or a deployed token is argued in the file's own header

### Test helpers (`tests/helpers/mod.rs`)

Shared utilities available to all integration test files:

- `generate_wallet()` - random secp256k1 keypair, returns `(SigningKey, address_hex)`
- `sign_activation(key, app_id, token_id)` - compute activation message, personal_sign, return hex signature
- `create_license_json(dir, ...)` - write a valid `LicenseProof` JSON file
- `wrapper_bin()` - path to the compiled wrapper binary
- `verifying_key_to_address(key)` - derive Ethereum address from public key

## 3. Seed a license proof for manual testing

The `seed-license.sh` script generates a valid license proof so the wrapper skips the activation window. Requires Foundry (`cast`).

```bash
./scripts/seed-license.sh
```

This writes a proof to `/tmp/rub3-test/com.rub3.example.json` signed by anvil's default account 0. Then run the wrapper with:

```bash
RUB3_LICENSE_DIR=/tmp/rub3-test cargo run -p rub3-wrapper -- --binary /path/to/your/binary
```

The wrapper will verify the proof's signature, skip activation, and launch the binary directly.

To reset and force re-activation:

```bash
rm -rf /tmp/rub3-test
```

## 4. Manual wallet operations with `cast`

For ad-hoc wallet operations (not required for automated tests):

```bash
# Create a wallet
cast wallet new

# Check balance on Base
cast balance <ADDRESS> --rpc-url https://mainnet.base.org

# Query a license contract
cast call <CONTRACT_ADDRESS> "ownerOf(uint256)" 1 --rpc-url https://mainnet.base.org
cast call <CONTRACT_ADDRESS> "price()" --rpc-url https://mainnet.base.org

# Sign an activation message
cast wallet sign --private-key <KEY> <MESSAGE_HASH>

# Use a local fork
anvil --fork-url https://mainnet.base.org
cast call <CONTRACT_ADDRESS> "ownerOf(uint256)" 1 --rpc-url http://127.0.0.1:8545
```

## 5. App constants

The wrapper's identity is controlled by constants in `crates/rub3-wrapper/src/main.rs`:

| Constant | Default | Purpose |
|---|---|---|
| `APP_ID` | `com.rub3.example` | Reverse-DNS app identifier |
| `CONTRACT` | `0x0000...0000` | ERC-721 license contract address |
| `CHAIN_ID` | `8453` | EVM chain ID (Base mainnet) |
| `RPC_URL` | `https://mainnet.base.org` | JSON-RPC endpoint |
| `DEVELOPER_ENS` | `None` | Optional ENS name |

To test against a real contract, update `CONTRACT` to your deployed ERC-721 address and rebuild.
