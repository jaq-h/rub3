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

The workspace has a further member, the docs MCP server, whose suites are
separate and described in [Docs MCP server](#docs-mcp-server-cratesrub3-docs-mcp)
below: `cargo test -p rub3-docs-mcp`.

The default bundle is `tier-2` + `webview`, so it compiles neither the tier-3
capabilities nor the headless front door. Cargo features are additive, so
`--no-default-features` is mandatory when selecting another bundle:

```bash
# tier-3 (adds onchain-write + cooldown): 133 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib

# tier-3 + the headless (agent) front door: 197 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless --lib

# tier-3 + the webview (human) front door: 150 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3,webview --lib
```

For reference, `--lib` counts per bundle: `tier-0` 53, `tier-1` 83, `tier-2` 123,
`tier-3`/`tier-4` 133, `tier-2,webview` 124, `tier-3,webview` 150,
`tier-3,headless` 197, `tier-0,sdk` 65, `tier-3,sdk` 150. Every count on this
page, and in `implementation.md` §3.5, is the number libtest reports as
*running* - passed plus `#[ignore]`d, not passed alone - so a plain run of
`tier-3,webview` prints 146 passed and 4 ignored against the 150 above. The
ignored ones are one network test in every bundle, plus the three anvil-gated
webview session-flow tests under `tier-3,webview`. `tier-1` and `tier-2` diverge because
`attest` needs `onchain-read`. `tier-2,webview` and `tier-3,webview` diverge
because the window's purchase screen, the code attestation guarding it, and the
tier-3 session flow all need `onchain-write` or `cooldown`. The `sdk` bundles
diverge from each other by the five session-projection tests, which need a
session model to project.

The `rub3` SDK crate is its own package, so it is its own invocation:

```bash
cargo test -p rub3     # 16 unit tests + 3 doctests
```

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
- **`rpc::tests`** - provider construction, contract call error paths, `encode_activate_calldata` selector + layout, `get_tx_receipt` / `get_block_number` / `get_code` error paths, ENS stub; the endpoint redaction (§2.8), which is a property of the wrapper's whole RPC error surface rather than of any one screen: a key placed in a path segment, in a query parameter and as userinfo must each be absent from the error's `Display` after construction through `RpcError::transport` and `RpcError::contract` while the host, the port and the failure text survive; a bracketed IPv6 authority keeps its host and loses its key, including when trailing punctuation follows it; an address whose authority will not parse at all is dropped whole rather than half-printed, which is the fail-closed property that a bare `[redacted url]` followed by a verbatim path would violate; plus one drive of `tokens_of_owner` against a dead port because alloy classifies an unreachable node during an `eth_call` as a contract error rather than a transport one; and for the EIP-3009 rail (§2.2) the `ReceiveWithAuthorization` typehash against its literal preimage, the signing digest against a vector computed independently with `cast`, every signed field proving it changes that digest, and the `purchaseWithAuthorization` calldata selector
- **`session::tests`** (requires `session` feature) - message determinism, tier-diffing, expiry edge cases, sign/verify round-trip, wrong-wallet failure; with `cooldown` adds: `verify_onchain` missing-field + bad-URL paths, `should_reverify` distribution sanity
- **`session_store::tests`** (requires `session` feature) - save/load round-trip, missing-session, `load_latest_session` picking the freshest valid session (`load_latest_session_for_wallet` narrows the same scan to one signer, covered from `activation::tests`)
- **`identity::tests`** - `IdentityModel` parsing and wire format, ERC-6551 TBA derivation determinism and sensitivity to each input, `resolve_user_id` for both models
- **`signer::tests`** (requires `headless` feature) - hex key parsing (bare/prefixed/padded, and every rejection: wrong length, non-hex, zero and out-of-curve-order scalars), `Debug` redaction and error messages asserted not to echo the input, `personal_sign` / `sign_prehash` recovery, RFC-6979 determinism, keystore decrypt, password-file precedence, and the strict env-key-over-keystore resolution order with no fall-through on a malformed key
- **`tx::tests`** (requires `headless` feature) - invalid-URL transport error, the node's `insufficient funds` classifier, the shortfall message with and without known amounts, and (§2.8) the endpoint redaction on this side of the wall: `send` driven against a dead port for a key in a path segment, in a query parameter and as userinfo, asserting none of them reaches what the agent door prints. `tx` builds `TxError` from alloy directly and never passes through `RpcError`, so it inherits nothing from that type's constructors and needs its own coverage of the one shared sanitiser
- **`activation::tests`** (requires `headless` feature) - the exit-code table asserted value-by-value, all classified codes distinct and disjoint from 0/1/2, `machine_detail` contents, `lowest_token` selection, the token- and wallet-scoped session fast path, every unconfirmed-purchase outcome mapping to the terminal code 21, and (§2.6) the `NotCanonicalContract` refusal: its message naming the function the pre-filter saw and stating that nothing was signed, its detail line reporting `code_bytes=` and `exposed=none` rather than an empty value, the factory case reporting `sells_licences=false`, and every classified code appearing in the `--help` table; and (§2.9) `the_refusal_line_says_what_the_code_registry_answered`, which pins the three-valued `registry=` field the same line now carries - `not_consulted`, `unknown` and `unavailable` are one refusal but three different things to alarm on, and today's shipped answer is the first of them; and for the spend ceiling (§2.2) `SpendPolicy`: an unset `RUB3_AGENT_MAX_TOKEN_AMOUNT` leaving the rail unavailable rather than unlimited, the ceiling inclusive at the boundary, zero as a real ceiling rather than "unset", the refusal carrying `listed`/`maximum`/`token`, and every malformed value a hard `Config` error naming the variable; and for the pre-flight's authorization disclosure (§2.2) that the two signed copies share one salt - and therefore one single-use nonce - differing only in `validBefore`; and for the ETH ceiling (§2.7) the same properties on `check_eth_wei` plus the ones the default introduces - an unset `RUB3_AGENT_MAX_ETH_WEI` meaning `DEFAULT_MAX_ETH_WEI` rather than either zero or unlimited, an ordinary 0.01 ETH listing still buying under it, `0.05` rejected as a hard error naming wei as the unit, and neither rail's variable moving the other's. The relationship between the two windows themselves is a `const _: () = assert!(..)` beside the constants rather than a test, so a window that stopped being short fails the build in every build that compiles the headless front door - the constants live inside `mod headless`, so of the eight matrix bundles only `tier-3,headless` sees the assertion, and it is the only one where they exist at all
- **`rpc::stub_node_tests`** - the token-side call classifier (§2.2), driven through `stablecoin_rail` and `preflight_purchase_with_authorization` against a local stub endpoint answering one fixed body each (the endpoint itself is `test_support::StubNode`, shared with `webview::tests`), rather than asserted about the classifier in isolation: a revert (`code: 3`, and `-32000` with revert wording) or empty return data is a settled contract answer, so the rail reads as absent and the run continues on ETH; a JSON-RPC error body, an execution timeout and an undeserializable body are node failures that propagate instead of silently changing the currency
- **`rpc::tests::receipt_polling`** (requires `onchain-write` or `cooldown`) - the receipt poll loop driven over scripted answers: a transient transport failure does not end the wait, one that outlasts the budget is reported as `Transport`, a recovered poll ends as `Timeout`, a request that can never succeed (an unparseable tx hash) is reported at once instead of consuming the budget, and both outcomes report real wall-clock waiting time rather than the nominal budget
- **`attest::tests`** (requires `onchain-read`, so tier-2 and up) - the pre-purchase code check (§2.6). Drift: every fingerprint and immutable range in `contracts/canonical-bytecode.json` is pinned in `attest::CANONICAL` (the failure prints the row to add), the pinned hashes are lowercase hex of 32 bytes, the ranges are sorted, disjoint and one word wide, and `FORBIDDEN_SIGNATURES` is compared against the `string[N]` array in `contracts/test/Rub3Invariants.t.sol` with Solidity comments stripped first. Comparison: a legitimate deploy that chose different immutables still matches, a truncated deploy is refused rather than partially masked, an address with no code says so, and the selector helper is checked against the published `transfer(address,uint256)` vector. The negative case is `a_renamed_seizure_function_passes_the_name_scan_and_fails_the_hash` - an owner-only seizure named `reconcileLedger(uint256,address)` is asserted to pass the blacklist in silence and to fail the masked hash, which is the asymmetry the module exists for. Gate: `only_licence_roles_are_purchase_targets` runs `decide()` over the shipped table and requires exactly the `Role::Licence` rows accepted and every factory and deployer row refused as `NotALicence`, and `the_attest_module_is_reachable_only_from_the_purchase_path` walks the crate's own `src/` recursively and asserts three things: the set of modules referencing the module at all, by any item rather than only by calling the gate, is a subset of the purchase-path allowlist (`activation.rs`, `webview.rs`); each allowlisted purchase path holds exactly one call site (`activation.rs::headless::purchase` and `webview.rs::show_purchase`), since a subset is also satisfied by calling the gate nowhere; and the named human launch entry points inside `webview.rs` (`show_activate`, `show_cooldown`, `finalize_session`) reference it not at all, which is the half a file-granular allowlist cannot speak for, failing loudly rather than vacuously when one of those functions can no longer be found. That is how "fail closed on purchase, fail open on launch" is enforced structurally rather than by a default. It guards source structure, not runtime wiring, and it is not total: a new launch function added to `webview.rs` is unguarded until it is named, and the same file granularity means a reference elsewhere in `activation.rs` is not caught either. The behavioural half is the launch-path e2e below. Registry (§2.9), driven over a scripted `ChainReader` whose call log is half the point: the three-way verdict of the design (a pinned-table hit that asks the registry *nothing*, a release the registry vouches for, and code neither knows); the registry's own code verified before its answer is believed, with `a_registry_whose_own_code_is_not_canonical_is_never_believed` scripting an answer that *would* be accepted if the check were skipped, `canonical_code_that_is_not_a_registry_is_not_believed_either` for the factory at that address, and `an_unverified_registry_is_never_asked_anything` asserting through the log that nothing follows a failed verification; each of the three reads failing in turn; a hostile offset table dropped and never even hashed under; a record declaring ranges other than the table that found it; every candidate table tried, and the two ends of the bound on that: `candidate_offset_tables_are_capped_on_the_purchase_path` publishes one more usable table than the cap allows and hides the answer under the oldest, first-published one - which is the table outside the budget now that the wrapper reads from the newest end - then asserts through the call log both that the cap is what the registry was *asked* for and that exactly the cap's worth of lookups followed, while `the_offset_table_read_is_bounded_before_the_answer_is_decoded` and `a_registry_that_ignores_the_requested_window_still_buys_no_extra_lookups` split the two costs apart against a registry holding four times the cap - the read is requested bounded so a long published set never reaches the shape check, and a node that answers with more than it was asked for still buys no extra round trip; `the_budget_is_spent_on_the_newest_layouts` pins which end that budget goes to, since a registry is consulted only after a pinned-table miss and a miss is by definition about newer code: out of 64 layouts a release published under the newest is found in a single lookup, and one published under the oldest is refused as unknown, which is the trade the cap makes on purpose; a deprecated release bought with a warning that says held licences are unaffected; a registry record for a non-licence role including one this build has no name for; the shape rules for an immutable slot, `PUSH32` included; `registry_table_mirrors_the_deployment_manifest`, which pins `attest::REGISTRIES` to `contracts/deployments.json`; `nothing_is_deployed_so_the_accumulate_only_rule_is_not_live_yet`, which reads that same manifest and asserts every `factory` and every `code_registry` is still `null`, so `CANONICAL`'s statement that its rows may still be corrected in place is a checked fact rather than a comment - the two records are independent deploys, either one going live arms the permanence rule for what it put on chain, and this fails at that moment so the doc gets updated instead of quietly rotting; `an_unknown_role_number_is_never_guessed_at`, since the first variant is a licence and a registry newer than this build can publish a role this one has no name for (the numbering itself is a wire encoding and is held by the anvil suite, not here); `an_empty_address_is_refused_without_asking_the_registry`, because an address holding no code has no release for an authority to recognise and must keep the one refusal a person can usually act on; and `registry_supplied_text_cannot_break_the_line_that_quotes_it`, which publishes a contract name and a version label carrying a space, an `=` and a newline and asserts neither can invent a field in the agent door's `key=value` detail line nor forge a `rub3:` line of the wrapper's own output. The one that matters most is `an_unpublished_registry_changes_nothing_about_the_gate`: no chain carries a registry address, a table miss costs no extra chain read, and the refusal string is unchanged to the sentence, so the whole step is inert until something is deployed
- **`webview::tests`** (requires `webview` + `onchain-write`, so only `tier-3,webview`) - the human purchase gate (§2.8). `show_purchase` driven against a `StubNode` answering with non-canonical code emits exactly one message to the window and it is the refusal, so "it also showed the purchase screen" cannot pass; against an unreachable endpoint the failure reported is the code check rather than the supply read, which is how the ordering claim is tested rather than asserted. The words are covered too: the two refusal causes carry different titles, bodies and next steps, an address holding no code says so instead of talking about a mismatch, every notice names the address and survives its source line continuations as finished prose, and a failed read shows the kind of failure only rather than a network error the buyer is then told to forward. The endpoint redaction itself is tested a layer down, in `rpc`, since it is a property of the error value rather than of this screen; what is tested here is that it holds on the window's *other* error surface too, by driving `handle` with a `connect` message against an endpoint whose URL embeds a key and asserting the "ownership check failed" box carries none of it. The §2.9 additions: the three code-registry outcomes read differently and none is retryable, the registry is named as the wrong address rather than as bad code, a role this build is too old to name is refused in words a person can read, and `a_failed_registry_read_never_puts_the_packed_endpoint_on_screen` covers the second route to the same leak - a registry read's failure reason travels *inside* the refusal rather than beside it, so `Unrecognised::shareable_detail` drops it while keeping the registry's *answer*, which names no endpoint. `a_deprecated_release_advises_the_buyer_rather_than_alarming_them` covers the other direction: a deprecated release has to reach the person, since the premise of this whole screen is that a buyer cannot read bytecode and a buyer does not read stderr either, and it has to reach them as advice - the sentence names the release, says the code is genuine and says the licence stays valid, and it promises no successor, since the record carries none, while a current release and a pinned-table hit say nothing at all. The §3.2 addition is `the_two_registries_are_described_differently_on_the_blocked_screen`: the discovery registry is described as what it is rather than in the code registry's words - those two are the pair a buyer is most likely to have confused already - and no two roles read identically on the blocked screen, so a role added later cannot silently inherit another one's sentence
- **`supervisor::tests`** (Unix) - the wrapped binary's own reported environment carries none of the `RUB3_AGENT_*` credential variables (the list is `agent_env::AGENT_ENV_VARS`; `RUB3_AGENT_MAX_TOKEN_AMOUNT` and `RUB3_AGENT_MAX_ETH_WEI` are spend policy rather than credentials and are deliberately not on it), covered for the raw-key source, the keystore-plus-password-file source (the documented preferred setup) and all sources at once. `the_wrapped_binary_does_not_inherit_a_stale_sdk_channel_address` covers the channel address the same way: `supervisor::SDK_ADDRESS_ENV` exported into the wrapper's own environment is absent from what the child dumped, and what the child got instead is this launch's own value. `a_launch_serving_no_channel_still_tells_the_child_a_wrapper_launched_it` is the other half of that - a build without `sdk`, or a channel that failed to start, publishes `SDK_ADDRESS_NO_CHANNEL` rather than nothing, so the application reports a wrapper serving no channel instead of no wrapper at all, whose advice is to do what the developer already did. This module compiles in every bundle, which is the point - neither behaviour is feature-gated, and the `sdk_e2e` tests of the same invariants only exist in a build that serves a channel
- **`sdk::tests`** (requires `sdk`) - the wrapper's half of the SDK channel (§3.5). The request handler is driven over a loopback stream that shares its read and write ends the way a duplicated socket handle does: several requests answered on one connection, an unrecognised operation answered as an error rather than a dropped connection, a foreign protocol version told which two versions are in play rather than complained about as JSON, and a malformed line answered once before the connection ends, asserted by the second request on that stream *not* being read. Three tests use a real socket instead, because a scripted in-memory stream always has the next request already waiting and so can never exercise a read that finds nothing there yet: `a_served_channel_answers_the_sdk_client_over_a_real_endpoint` drives the SDK's own client, asserting a live channel answers, a tier-0 channel reports no session, and a dropped channel stops answering and leaves no endpoint behind - which is what makes a dead wrapper detectable; `two_requests_on_one_idle_real_connection_are_both_answered` holds one connection open and idles before each of two requests, which is the keep-alive contract `serve_connection` documents and the one that catches an accepted socket left non-blocking (macOS and the BSDs inherit the listener's `O_NONBLOCK` across `accept`, Linux does not, and an `EAGAIN` on that read is otherwise answered as a malformed request before the connection closes); and `several_channels_served_from_one_process_get_distinct_live_endpoints` serves eight at once and asserts each is published at its own address and is listening there, which an endpoint name unique only per process cannot deliver. `the_published_variable_and_sentinel_match_the_sdk_crates_constants` pins `supervisor::SDK_ADDRESS_ENV` and `SDK_ADDRESS_NO_CHANNEL` against `rub3::wire::ADDRESS_ENV` and `ADDRESS_NO_CHANNEL`, the second copies that exist because a build without this feature cannot name the first pair. Three more own the endpoint's directory, which is a 0700 directory under `TMPDIR` that only a `Channel`'s `Drop` would otherwise remove: `an_endpoint_directory_outlives_its_guard_only_once_a_channel_owns_it` drives the guard both ways, since every fallible step in `serve` after the directory exists - the bind, the listener's mode, the accept thread - returns before a `Channel` is built; `the_sweep_collects_dead_endpoints_and_never_a_live_or_foreign_one` is the one that matters, because the sweep deletes directories on a shared temp path - a dead pid's endpoint (a real child, waited for) is collected, while *another live process's* endpoint, this process's own, and a name the module never wrote all survive, so removing the liveness check fails the test rather than passing it quietly; and `a_sweep_of_a_directory_that_is_not_there_is_harmless` pins the fail-open posture, since a sweep must never be the reason a paid-for launch does not happen. With `session` a further five cover the projection onto the six fields an application may see: the account model reporting an identity distinct from its signer, the access model reporting them equal, a tier-4 session with no TTL reading as "does not expire" rather than "expired", an identity model this build has never heard of carried through verbatim rather than mapped onto a known one, and an unreadable `expires_at` refusing to report the session at all rather than serving it with the TTL quietly dropped

### Integration tests (`tests/integration.rs`)

Binary-level tests that spawn the wrapper process:

- `runs_child_and_exits_zero` - wrapper exits 0 when child succeeds
- `propagates_nonzero_exit_code` - wrapper forwards child's exit code
- `passes_args_to_child` - `--` separator passes trailing args to child
- `errors_on_missing_binary` - wrapper rejects nonexistent binary path

Each test provisions a valid license proof in a temp directory via `RUB3_LICENSE_DIR`.

### SDK channel E2E (`tests/sdk_e2e.rs`)

Requires the `sdk` feature; no network and no Foundry, so it runs in the ordinary
suite rather than behind `#[ignore]`. A **real wrapper process** launches a **real
application linking the `rub3` crate** - `rub3-sdk-probe`, which prints one
`key=value` line per field - and every assertion is on what that application
printed. Nothing reaches into the wrapper's internals, because §3.5 makes no claim
about them.

- `an_application_launched_by_the_wrapper_sees_a_live_heartbeat` - the positive case
- `an_application_launched_without_a_wrapper_panics_with_the_documented_failure` - the negative case, run rather than asserted: exit 101, and the panic text is required to name `RUB3_SDK_SOCKET` and to say `rub3-wrapper --binary`, since a developer who has just run a wrapped binary directly is its whole audience
- `without_a_wrapper_the_reported_failure_is_not_wrapped` - the same absence through `try_heartbeat`
- `an_address_pointing_at_nothing_reports_a_dead_wrapper_rather_than_no_wrapper` - the two failures must be distinguishable: the first is a wrapper that died, the second a binary run directly
- `a_launch_served_from_a_legacy_proof_reports_no_session` - a heartbeat and no session, with no `user_id` reported at all. The legacy proof predates the identity model, and synthesising one from the wallet would invent an identity claim the wrapper never verified. Every launch here points `RUB3_SESSION_DIR` at an empty temp directory as well as `RUB3_LICENSE_DIR`: the session fast path runs first in any bundle that compiles it, so without that a session left in the developer's own `~/.rub3/sessions` would serve the launch and the test would stop testing what it names
- `the_endpoint_is_removed_when_the_wrapper_exits` (Unix) - the socket and its directory are both gone afterwards, so a later launch cannot be answered by a stale endpoint
- `the_endpoint_directory_is_private_to_this_user` (Unix) - asserted `drwx------` rather than assumed. That mode is the access control on the channel; the name is unguessable only by accident
- `a_stale_address_in_the_wrappers_environment_never_reaches_the_child` - an address exported into the wrapper's own environment is scrubbed, because a child that inherited it would talk to somebody else's channel and be answered
- `a_channel_that_fails_to_start_reports_a_wrapper_without_a_channel` (Unix) - the launch is not gated and the application reports `no_channel` rather than `not_wrapped`, whose advice is the launch the developer just performed. The failure is real rather than simulated: `TMPDIR` points at a directory that does not exist, which fails the endpoint directory's `mkdir` the way an unwritable temp directory does. The path is kept short on purpose - `endpoint_dir` falls back to `/tmp` when the socket path would exceed `sockaddr_un`, and that fallback would start a channel and quietly stop the test testing what it names
- `a_wrapped_application_sees_the_session_the_wrapper_launched_on` (requires `cooldown`) - both entry points on one launch, field for field, with an account-model session whose `user_id` is asserted to differ from its `wallet`
- `the_application_never_receives_the_session_signature_or_nonce` (requires `cooldown`) - the seeded session's signature and nonce are asserted absent from what the application printed, along with `contract`, `chain`, `issued_at`, the activation fields and the device key. An application that could read the signature could replay the session somewhere the wrapper never launched it

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

**The four anvil-gated suites below are count-checked in CI.** Each `onchain-e2e`
step pipes cargo's output through `scripts/assert-e2e-ran.sh`, which fails the
step unless exactly the number of tests that step's `EXPECTED_TESTS` names
passed: cargo exits 0 both for a filter that selects nothing and for a suite
that self-skips on a missing toolchain, so neither the exit code nor a green
step means anything on its own. Adding or removing a test in any of the four
means updating that count in `.github/workflows/ci.yml`.

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

### Webview session flow (`src/webview/session_flow.rs`)

The human front door's §1.8 flows: connect, activate, sign, persist, restart. Lib
tests rather than an integration suite, because the seam they drive -
`webview::IpcState`, the activation window's IPC handler - is private to
`src/webview.rs`. A `Window` driver wires that handler to channels instead of a `wry`
view, so a test posts the JSON the page posts and reads back the JS the page
would have run.

**What this does not cover**, and is still §1.7's manual testing: the `wry`/`tao`
layer itself, and `assets/activation.html` - the JS that renders each screen,
carries `pendingSessionCtx` across the cooldown → confirm → sign hand-offs, and
posts the messages back. Everything between the two is covered.

Three of the four are anvil-gated on port **8551**, so they run alongside
`session_onchain_e2e.rs` (8547) and `headless_e2e.rs` (8549); they are
`#[ignore]`d, print `SKIP:` and pass when Foundry is absent, and serialise
themselves through a file-level mutex plus the crate's `ENV_LOCK`. Each seeds a
licence with `cast` rather than buying one, so none of them depends on the local
build reproducing `contracts/canonical-bytecode.json`.

- `a_connected_wallet_activates_signs_and_the_session_survives_a_restart_e2e` - the whole flow: `onAppInfo`, `connect` → the cooldown screen, the wallet broadcasting that screen's own calldata, the poller's `onTxConfirmed` checked against the receipt's real block hash and normalised owner address, the wallet signing that preimage, `SessionSuccess` verified locally and on-chain, `activation::persist_activation`, then `activation::ensure` returning from the fast path with no window and no second `activate()`
- `a_second_activation_inside_the_cooldown_is_refused_and_the_window_says_how_long_e2e` - the contract refuses with `CooldownActive` and does not move `lastActivationBlock`; the window reports `ready: false` with the blocks the contract is still counting, holds that two blocks out, and clears at the boundary with session id 2. Two blocks rather than one because `cooldownReady` is evaluated at the head while the transaction executes in the next block
- `an_expired_session_is_refused_and_a_fresh_activation_replaces_it_e2e` - a two-second TTL, then `activation::try_session_fast_path` declines the lapsed session and a second pass through the flow issues a fresh one

The fourth is not anvil-gated and not gated on `cooldown`, so it runs in the
ordinary matrix under `tier-2,webview` as well:

- `a_zero_contract_build_still_issues_and_serves_a_legacy_licence_proof` - with no contract configured the window issues a `LicenseProof`, and a later `ensure` is served from it against an RPC URL nothing answers on, which is what proves the path reads no chain

Run the anvil-gated three with:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-3,webview \
    --lib -- --ignored webview::session_flow
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
- `headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e` (§2.6) - the launch half of "fail closed on purchase, fail open on launch": buys once through the gate, removes the cached session so the fast path cannot answer from disk, mines past the cooldown, and relaunches → `Activated` rather than `PurchasedAndActivated`, the same token id back, and `nextTokenId` unchanged read through `cast`. It pins the structural claim - a launch is a different code path that never reaches the gate - against a contract that is canonical either way, so it cannot tell "never runs the gate" from "runs it and passes". The two tests below do
- `headless_refuses_a_modified_licence_that_passes_the_selector_scan_e2e` (§2.6) - the case the fingerprint check exists for, which the non-rub3 impostor above cannot reach. Deploys `NonCanonicalRub3Access` (the deliberately non-canonical fixture in the mocks file) beside a canonical `Rub3Access` built from the same constructor arguments, asserts the two agree on every getter an agent reads, then drives the purchase door at the fixture → `NotCanonicalContract` / exit 23 with `exposed=none` on the detail line, so it is the masked hash and not the selector blacklist that caught it. The fixture advertises a stablecoin rail the agent holds and can afford, so the witnesses are the ones that rail needs: a `CountingSigner` never asked to sign - no EIP-3009 authorization was ever produced, and disclosure is the spend - plus an unmoved nonce, ETH balance, token balance and `nextTokenId`
- `headless_launch_of_an_already_paid_licence_survives_a_contract_the_gate_refuses_e2e` (§2.6) - the fail-open half observed rather than inferred. Seeds a licence to the agent outside the wrapper (`purchase(address)` is callable by anyone, so a licence can be put in an agent's hands on a contract the wrapper would refuse to buy from), asserts no cached session exists, mines past the cooldown, and launches → `Activated`, the session verifying locally, `nextTokenId` unchanged. **Both outcomes come from the same deployed address**: a second agent holding nothing is then refused at the gate on that identical contract. Refusing to start a program somebody already paid for would be the revocation surface §2.4 rules out
- `headless_launch_survives_a_node_that_will_not_answer_a_code_read_e2e` (§2.6) - the other way verification fails to complete, on a canonical contract so the read is the only variable. A `RecordingProxy` relays every call to anvil except `eth_getCode`, whose connection it closes unanswered → the launch reaches `Activated`, and the recorded request log contains no `eth_getCode` at all, which is the stronger statement: the launch did not survive the missing answer, it never asked. The same proxy then drives the purchase door as a control and it stops at the gate with `HeadlessError::Rpc`, so the launch arm is a fact about the launch path rather than about the proxy
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

In-process EVM: no network, no `.env`. 293 tests across seven files.

- **`test/Rub3Access.t.sol`** (28) - metadata, constructor validation, purchase (including the exact-payment rule: under, over, exact, and a 256-run fuzz proving the listed price is the only accepted amount), supply cap, activation and cooldown, owner gating
- **`test/Rub3Subscription.t.sol`** (15) - expiry, renewal (exact payment on both under and over), `isValid`, and the per-token `renewPrice` snapshot of §2.4
- **`test/Rub3Invariants.t.sol`** (50) - the ownership invariants, in four groups: 18 on the append-only hash set (constructor seeding, zero/duplicate rejection, older releases staying valid, revocation status/reason/events, revoked hashes not resurrectable, owner gating), 16 on the successor pattern (opt-in on both sides, one claim per token, the claim following the current holder, survival of renounced ownership, frozen subscription terms carried across a successor declaring a different `period`, and the trust rule surviving a successor repoint), 11 on mint ordering and predecessor typing (a `MintCallbackProbe` recipient reading a token's already-frozen terms from inside `onERC721Received` on both the purchase and the claim path, and `IncompatiblePredecessor` from each probe against every mistyped or truncated predecessor), and 5 on the no-revocation audit (30 forbidden signatures × 4 deployed contracts, the fourth being the §2.3 `Rub3Factory`, with a positive control proving the scanner finds selectors that do exist - including `feeBps()` and `treasury()`, the getters that exist while every setter for them is absent)
- **`test/Rub3TokenPurchase.t.sol`** (42) - the EIP-3009 stablecoin rail of §2.2. The buyer holds stablecoin and a zero ETH balance in every test, and a separate submitter sends every transaction: replay, front-running (diverting the mint, stripping it by calling the token directly, and calling `receiveWithAuthorization` as a third party), authorizations aimed at the wrong contract / wrong intent / wrong token id, the validity window and cancellation, a price move after the read rejecting on *both* rails (the stablecoin one through the digest, the ETH one through the exact-payment check), the balance-delta check, the constructor probes, both rails minting identically, subscription renewal terms frozen on both rails, an EIP-1271 smart-contract wallet buying and renewing (and a signature it rejects buying nothing), and a token implementing only EIP-3009's `(v, r, s)` form deploying happily and then being unspendable for that reason alone (signed against its own domain, empty revert data, the same fields spent through the split form it does implement, and the same authorization shape minting against the mock)
- **`test/Rub3Factory.t.sol`** (54) - the factory and the protocol fee of §2.3, in eight groups: the factory itself (terms stamped on both models, `isDeployed` plus ordered enumeration, owner defaults, the `LicenseDeployed` log, the fee range rejected either side and accepted at both ends, and the two contract-size limits); immutability (a newer factory at a different rate leaving an older deploy untouched in terms *and* money, disjoint per-factory registries, and the contract owner running every power it has without moving the fee); exact ETH arithmetic at the boundaries (1 wei, the 39/40-wei rounding edge at 250 bps, an indivisible amount, 1,000,000 ether, and a 256-run fuzz over amount x rate); the same on the stablecoin rail; `test_bothRails_chargeIdenticallyForTheSameAmount`, which prices one contract at the same number in wei and in the token's smallest unit and asserts the two accruals are equal; direct deployment working, unrecorded, and unpenalised; fee evasion pinned from both sides by `test_eth_feeIsChargedOnTheListedPriceBecauseNothingElseArrives` and `test_eth_zeroPriceListingCannotCollectByOverpaying` - the inverted forms of two tests that used to assert an overpayment was taxed, closing the route at the payment now that neither rail can deliver more than the listed price - and the accrual rationale by `test_accrual_rejectingTreasuryCannotBlockPurchases`, where a treasury that refuses ETH fails only its own sweep while buyers still buy and the developer is still paid in full; and the canonical-predecessor rule (the laundering route reverting `PredecessorNotCanonical` and recording nothing, a canonical predecessor accepted with the migration completing end to end, the zero predecessor, the subscription path, cross-factory acceptance through `previousFactory` and its absence on an unlinked factory, both sides of the `MAX_PREDECESSOR_FACTORY_HOPS` bound, the constructor probe rejecting a non-contract and a half-answering `previousFactory`, and direct deploys and the deployer helpers staying unconstrained and unrecorded)
- **`test/Rub3CodeRegistry.t.sol`** (35) - the append-only properties of §2.9's code registry, asserted as behaviour rather than as comments, in six groups: publishing (the record kept whole, an unpublished hash reading as `Unknown`, the block stamped by the contract rather than supplied, the permanent `Published` event, every role carried through unchanged, and an empty offset table accepted as a real answer); append-only (republish and overwrite both reverting and leaving the record untouched, a deprecated hash not republishable either, and `test_audit_noRemovalOrRewriteSurfaceExists` scanning the deployed runtime bytecode for 10 removal, rewrite and un-deprecate signatures with its own positive control - a separate list from the shared 30, which is about tokens and says nothing about a registry); deprecation (the entire record compared either side of one, the reason logged, no repeat, no undo, and no reach into another record); ownership (non-owner writes reverting on both writers, the two-step transfer taking effect only on acceptance, and `renounceOwnership` reverting because ownership here is the right to *add*); offset tables (interned so identical tables are one entry, announced only on first use, rejected at four malformed shapes including the exact EIP-170 boundary either side, and readable bounded - `offsetTableWindow` returns its bound out of a set four times larger, starts where it is asked to, and clamps rather than reverting past either end or at the largest `count` a caller can pass, while `latestOffsetTables` returns the newest layouts newest-first out of a set four times its bound and clamps the same way, which is what lets a purchase path read the bootstrap without paying for however many tables the owner key published and without spending its budget on layouts older than the build asking); and record completeness (the zero hash, a missing source commit, each empty text field, and a reverted publish leaving neither an enumeration entry nor an interned table)
- **`test/Rub3Registry.t.sol`** (69) - §3.2's *discovery* registry, which is not `Rub3CodeRegistry` and whose own suite guards the distinction: `test_neitherRegistryCanStandInForTheOther` asserts neither contract's runtime bytecode carries the other's selectors, with a positive control. Seven groups. Construction (the factory recorded and immutable, the zero and codeless cases, the `HalfFactory` probe that would otherwise deploy fine and reject every registration forever, `renounceOwnership` reverting because abandoning curation would freeze the token list, and the two-step transfer). The register gate (a canonical deploy listed, a directly deployed licence refused, a non-owner refused, authority following a licence-contract ownership transfer with nothing stored here, the zero address, a second registration, an empty `appName` on both writers, an empty `contentURI` accepted as "nothing published yet", both text fields accepted exactly at `MAX_APP_NAME_BYTES` / `MAX_CONTENT_URI_BYTES` and refused one byte over on both writers with the offending length and the limit in the revert, an older generation's deploy still listable through `previousFactory`, and both sides of the `MAX_FACTORY_GENERATION_HOPS` bound built out of a ten-generation chain). The listing lifecycle (delist keeping the record and its `registeredAtBlock` so relisting is not a demotion, relist, non-owner and repeated-state refusals on each, `updateListing` working while delisted, every write refusing an unregistered address, and a suspension that the listing's own owner cannot undo, that carries a reason bounded at `MAX_SUSPENSION_REASON_BYTES` through the same entry gate, that is owner-only, and whose lifting does not override an owner who withdrew in the meantime). **Discovery, never validity** - `test_delisting_cannotTouchAHeldTokenOrALiveSession` pulls every discovery lever at once on a subscription with a paid token and a live session, then measures `ownerOf`, `isValid`, `activeSessionId`, `expiresAt`, a fresh activation past the cooldown, a fresh purchase and a renewal; `test_registryWrites_leaveTheLicenseContractUntouched` snapshots nine pieces of licence state across every registry write; and `test_audit_registryHoldsNoStateChangingExternalCall` walks the registry's runtime opcodes, skipping each `PUSH1..PUSH32` immediate, and asserts no `CALL`, `CALLCODE`, `DELEGATECALL`, `CREATE`, `CREATE2` or `SELFDESTRUCT` appears at all - so the invariant is a property of the bytecode rather than a reading of the source, with `test_audit_opcodeWalkFindsACallWhereOneExists` as the positive control on a licence contract that does move money. Ranking (ETH-only recognised, an unrecognised rail last, delisted and suspended entries omitted, the global window clamping rather than reverting at either end, and `test_rank_followsAPostRegistrationTokenPriceChange`, which registers two entries on opposite quotes, has both call `setTokenPrice` to swap, and asserts the order swaps with them - the one test a rank snapshotted at registration fails while passing every other test in the file). `test_priceTokenOf_readsAnAddressThatCannotAnswerAsEthOnly` and `test_priceTokenOf_readsAMalformedAnswerAsABadQuote` hold the "cannot answer at all is read as ETH only" rule against the shapes a `try`/`catch` does not catch - an address with no code, a silent fallback returning nothing, and a full word of high bits. The recognised-token list (enumerable across add and remove, owner-only, no-ops refused, and `address(0)` refused in both directions because the native rail is recognised by rule rather than by membership). **Reads whose cost the caller controls** - `registeredWindow` and `rankedRegistrationWindow` clamping at both ends, the window ranking only inside itself, `test_rankedRegistrationWindow_isNotAPageOfTheGlobalRanking` pinning the tradeoff the NatSpec states (an unrecognised entry in an earlier window still precedes a recognised one in a later window, so paging cannot reconstruct `rankedListings`), a cursor advancing by `count` rather than by page length across delisted entries, and `test_rankedRegistrationWindow_costDoesNotFollowTheSetSize`, which measures gas over a 24-entry registry and asserts both halves: the bounded read costs a fraction of the whole scan, and `rankedListingWindow` still costs the whole scan, which is exactly what its own doc says. And the agent card (every field against a live read, each wrapper hash carrying its own status including a revoked one, an unregistered contract answered rather than refused, the card following a price the contract has since changed, `cards` returning a globally ranked page, `cardWindow` returning a bounded one, and the `MAX_CARD_WRAPPER_HASHES` cap - `test_card_capsTheHashSetAndReportsTheTrueTotal` asserts the newest hashes are the ones kept and that `wrapperHashCount` still reports the true total, and `test_cardWindow_costDoesNotFollowOneListingsHashSet` measures a card at the cap against one eight times past it, which is the griefing vector closed, and `test_cardWindow_costOfAMaximalEntryIsTheStatedConstant` doing the same for the two text fields: a page holding an entry that used every byte it is allowed still carries only the published constants)
- **`test/mocks/MockEIP3009Token.sol`** - a faithful minimal EIP-3009 token standing in for USDC, validating signatures through OpenZeppelin's `SignatureChecker` exactly as Circle's FiatTokenV2_2 does, plus a silent token, a non-token, a token with no `DOMAIN_SEPARATOR()`, a token with only the split-signature form, and a `SmartWallet` EIP-1271 buyer. Why a mock rather than a fork or a deployed token is argued in the file's own header
- **`test/mocks/NonCanonicalRub3Access.sol`** - a deliberately non-canonical licence fixture, used only by the anvil-gated headless suite and by no `forge test`. It inherits the whole of `Rub3Access` and adds one owner-only seizure, `reconcileLedger(uint256,address)`, so it is a working licence in every observable respect and differs only in compiled semantics: it passes `attest::FORBIDDEN_SIGNATURES` in silence and fails the masked code hash. **It must never move to `contracts/src/`** - `scripts/canonical-bytecode-hashes.sh` fingerprints everything under the resolved source directory, so a copy there would be published into `contracts/canonical-bytecode.json` as canonical rub3 code and the wrapper would come to accept it. The file's own header says all of this at length

### Code registry E2E (`tests/code_registry_e2e.rs`)

Deploys `Rub3CodeRegistry` and `Rub3Access` on a real EVM and drives the registry half of §2.9 over the wire. Same gating as the three suites above (`#[ignore]`, prints `SKIP:` and returns when Foundry is absent), on port **8553** so all four can run at once. Gated on `onchain-read`, so `tier-2` is the lowest bundle that compiles it, which is also what CI runs it at. **A pass in 0.00s is a skip.**

Three things only this suite can check, and they are why it exists:

- **The ABI mirror.** `rpc::IRub3CodeRegistry` restates the registry's `Release` struct field for field, and field order *is* the ABI encoding, so a drifted mirror decodes garbage or reverts and no unit test in the crate can see it.
- **The pinned fingerprints against real deploys.** `attest::CANONICAL` is a table of numbers agreeing with another table of numbers until something compiles a contract, deploys it, and fetches the code back. This asserts the deployed `Rub3CodeRegistry` hashes to its pinned row (without which no wrapper would ever believe a real registry) and that a live `Rub3Access`, immutables filled in by its constructor, hashes to its pinned row once those ranges are zeroed.
- **The enum numbering.** `Role` and `Status` cross the ABI as raw `uint8`s and the numbering *is* the encoding, so renumbering either side silently turns a factory into a licence. Every `Role` value is published through the real contract and decoded back, `Active` and `Deprecated` are both driven over the wire, and `Status.Unknown` is covered as the mapping miss it is - it is the zero value and cannot be published. Variant *names* are not the encoding and are deliberately not asserted anywhere.

- `code_registry_answers_the_wrapper_over_a_real_chain_e2e` - publishes a release through `cast send` using the masked hash and immutable ranges out of `attest::CANONICAL` itself, then asserts the bounded `latestOffsetTables` read round-trips those ranges exactly, `consult_registry` returns the record with every field intact, code nobody published is `Unknown`, every `Role` and every reachable `Status` round-trips through the deployed contract, a hash nobody published decodes to no record at all, a `deprecate` reaches the wrapper as `Deprecated` with the record still whole, a bounded read against a registry holding more tables than it asked for returns the bound newest-first and clamps when asked for more than exists, and a licence contract at the registry's place is never believed as the registry.

Run with:

```bash
cargo test -p rub3-wrapper --no-default-features --features tier-2 \
    --test code_registry_e2e -- --ignored code_registry
```

### The SDK crate (`crates/rub3-sdk/`, run with `cargo test -p rub3`)

16 unit tests plus 3 doctests, none of which need a wrapper:

- **The wire format** - `session_info_carries_exactly_the_six_specified_fields` compares the serialized key set against the literal list from §3.5, so widening `SessionInfo` fails here; a round trip; a session with no TTL never expiring and a past TTL expiring; an identity model this build does not know surviving deserialization *and* a round trip unchanged, since the wrapper and the application are packaged separately and can be built from different revisions; the envelope carrying the protocol version on both a request and a response, asserted against the literal JSON
- **Framing** - a line past `MAX_LINE_BYTES` reported as exceeding the ceiling rather than as broken JSON, because a reader that truncated silently would blame the parser; a cleanly closed stream reading as "no message" rather than an error
- **The documented failure with no wrapper** - an absent address, an empty one (the shape a shell leaves behind, and it means the same thing), an address nothing listens on, and the `rub3:no-channel` sentinel a wrapper publishes when it serves none, each mapping to its own error; the panic text naming the variable and the way out, and the sentinel's saying to repack the wrapper rather than to launch through one; every error asserted to read as finished prose and to produce a two-line panic
- **The version before the body** - `exchange` is driven against an in-memory peer answering a shape this build has never seen: at protocol 2 that is a version mismatch, at protocol 1 it is a protocol error, so a future wrapper is reported as a mismatched pair rather than as a broken connection
- **Doctests** - a `no_run` example of the API as an application writes it, a `Display` example for `Wallet`, and a `compile_fail` one proving `Wallet` cannot be a `HashMap` key. That last one is the `user_id`-not-`wallet` rule made executable rather than documented

### Test helpers (`tests/helpers/mod.rs`)

Shared utilities available to all integration test files:

- `generate_wallet()` - random secp256k1 keypair, returns `(SigningKey, address_hex)`
- `sign_activation(key, app_id, token_id)` - compute activation message, personal_sign, return hex signature
- `create_license_json(dir, ...)` - write a valid `LicenseProof` JSON file
- `create_session_json(dir, ...)` (requires `session`) - write a signed `Session` where the launch fast path will look for it, and return it. The signature is over `session::session_message`, the same preimage the wrapper verifies, so the seeded session passes `verify_local` rather than merely deserializing
- `wrapper_bin()` - path to the compiled wrapper binary
- `verifying_key_to_address(key)` - derive Ethereum address from public key

### Docs MCP server (`crates/rub3-docs-mcp/`)

The developer-facing docs server of §3.3, and the machine-legibility gate over
the documents themselves. A separate workspace member, so it runs on its own and
adds nothing to the wrapper's dependency path:

```bash
cargo test -p rub3-docs-mcp
```

Four suites. Their subject is not behaviour so much as *provenance*: the server
must be unable to answer from a stored copy of anything.

- **`src/` unit tests** (14) - checkout resolution and the read guard (the walk up
  from a subdirectory, a named directory that is not a checkout refused, that
  same refusal held through `resolve_from_env` so a stale `RUB3_REPO_ROOT` never
  falls back to serving the compiled-in tree instead, a relative path refused for
  trying to leave the root, and `CLAUDE.md` still readable through its symlink),
  the Markdown parse (a `#` comment inside a shell fence not counted as a
  heading, a section carrying its subsections and stopping at its peer,
  resolution by anchor, and an untagged fence reporting an empty language), a
  search that finds exactly as many hits as its limit reporting itself
  untruncated while one more hit past the limit sets the flag, and the `cfg`
  predicate that decides what is test scaffolding: `test`, `any(test, ..)` and
  `all(test, ..)` are dropped from the served surface, `not(test)` and a plain
  feature gate are not
- **`tests/derivation.rs`** (27) - the provenance proofs. Seven of them build a
  throwaway checkout, ask a question, edit the file the answer came from, and ask
  again *through the same server*: a renamed function, a feature gate added to a
  crate root, a declaration re-exported out of a private module, an edited ABI
  entry, a selector removed from an artifact, a moved fingerprint, and a document
  added after startup. An answer that survives its own source being changed is
  either hardcoded or cached, and both are the defect the suite exists to catch.
  The rest run against this repository:
  `every_served_rust_signature_is_a_verbatim_slice_of_its_file` walks the whole
  workspace and asserts each served signature appears byte for byte in the file
  it is attributed to, and
  `every_rendered_function_signature_matches_the_artifacts_own_selector_map`
  cross-checks every rendered contract signature against `methodIdentifiers`,
  which is how a mis-expanded tuple parameter is caught rather than shipped with
  a plausible-looking wrong selector. A third asserts that no served signature
  carries an attribute or a doc comment, since `syn` spans cover both and a
  signature a caller cannot paste is only half derived; its fixture companion,
  `a_braced_variant_is_cut_at_its_brace_and_serves_its_fields`, pins the cut a
  braced enum variant needs to satisfy it, because a variant's body carries doc
  comments of its own and its fields are served as members instead. A fourth,
  `the_wrappers_reexported_front_doors_carry_their_declarations`, holds
  `supervisor_run` and `ensure_headless` to answering with the `pub fn` line
  their private modules declare, at the file and line the answer names, since a
  `pub use` statement tells an agent the name exists and nothing about how to
  call it. A fifth, `a_second_derivation_agrees_with_the_first`, derives the
  workspace twice and holds the second answer to the same verbatim slices as the
  first: `rustapi::workspace` drops the spans of every earlier derivation before
  it starts, and dropping them any later would silently start slicing the right
  shape out of the wrong file. Three refusals are in here too, for the questions
  where an empty answer would be read as a fact: a contract question with no
  artifacts names `forge build`, a module name that no crate declares names the
  modules that do exist, and an item name nothing matches names the items that
  do, rather than returning an empty surface an agent would read as "nothing
  public lives there". A fourth holds the shape of those refusals: a scope with
  no public names in it must say so rather than print a colon and stop, since
  the list is the whole reason the refusal exists. Four more hold the single
  rule about modules that expose nothing, which are answered rather than
  refused: such a module keeps its `//!` unfiltered and answers the same way
  when asked for by name, only an unknown module is refused, an unfiltered
  `rust_api` is capped at `limit` items and reports `truncated` with the capped
  answer a prefix of the whole one, and the cap drops a crate it emptied while
  keeping one that had nothing to cut. All of them build the shape they need in
  the fixture, since no module of this workspace has it. Two tolerances are here
  for the same reason inverted: a document that is not readable UTF-8 costs that
  document and not the other two document tools, which share the inventory with
  it, and a missing
  `contracts/canonical-bytecode.json` costs the fingerprints and not the whole
  contract surface, which is derived from the artifacts instead
- **`tests/docs_legibility.rs`** (8) - the gate: every code fence declares a
  language, every relative cross-reference resolves to a file *and* to a heading
  that exists, one title per document with no skipped heading level, a stated
  purpose under each title, no em dashes, and `llms.txt` conforming to the
  specification, linking only files that exist, and covering every document in
  the inventory. The gate holds the git-tracked Markdown, not the working tree
  the server walks, so an uncommitted scratch file is not held to the
  repository's documentation standard
- **`tests/mcp_stdio.rs`** (1) - spawns the built binary and speaks
  newline-delimited JSON-RPC to it: `initialize` at protocol revision
  `2025-11-25`, `tools/list`, three `tools/call`s and one refusal. It is the only
  test that can see stdout being clean, which the protocol depends on: a stray
  `println!` anywhere in the crate would corrupt every message after it

The two contract suites need forge artifacts, which are not checked in. Without
them the contract tests print `SKIP:` and pass, the same convention as the
anvil-gated suites. CI builds them first and sets
`RUB3_DOCS_MCP_REQUIRE_ARTIFACTS=1`, which turns that skip into a failure, so a
run that silently tested nothing cannot report green:

```bash
(cd contracts && forge build)
RUB3_DOCS_MCP_REQUIRE_ARTIFACTS=1 cargo test -p rub3-docs-mcp
```

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
