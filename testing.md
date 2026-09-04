# Testing Guide

This file owns the test inventory: what each suite covers, how to run it, and how to set up manual testing. It also owns the on-chain test plan (section 7): the three tiers a run can belong to, what each one may touch, and the rule that keeps a scratch deployment out of the canonical record. Per-test descriptions and suite counts belong here rather than in [implementation.md](implementation.md), which records status and cites the headline numbers. Contract-side operational recipes are in [contracts/contracts.md](contracts/contracts.md), the tier feature bundles the suites compile under are described in [README.md](README.md) and [AGENTS.md](AGENTS.md), and design rationale is in [architecture.md](architecture.md).

## Prerequisites

- Rust toolchain (rustc 1.91+): `rustup update stable`
- Optional: Foundry (`cast`, `anvil`) for manual wallet operations: `curl -L https://foundry.paradigm.xyz | bash && foundryup`
- Optional: Access to Base mainnet RPC (default: `https://mainnet.base.org`) for network tests

## 1. Run all tests

```bash
cargo test -p rub3-wrapper
```

This runs all unit tests, integration tests, and license e2e tests. No external tools required - wallet generation and signing are done natively in Rust via `k256`.

The workspace has two further members whose suites are separate: the docs MCP
server, described in [Docs MCP server](#docs-mcp-server-cratesrub3-docs-mcp), and
the `rub3` CLI, described in [The rub3 CLI](#the-rub3-cli-cratesrub3-cli):
`cargo test -p rub3-docs-mcp` and `cargo test -p rub3-cli`.

The default bundle is `tier-2` + `webview`, so it compiles neither the tier-3
capabilities nor the headless front door. Cargo features are additive, so
`--no-default-features` is mandatory when selecting another bundle:

```bash
# tier-3 (adds onchain-write + cooldown): 164 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3 --lib

# tier-3 + the headless (agent) front door: 228 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless --lib

# tier-3 + the webview (human) front door: 199 lib tests
cargo test -p rub3-wrapper --no-default-features --features tier-3,webview --lib
```

For reference, `--lib` counts per bundle: `tier-0` 58, `tier-1` 88, `tier-2` 128,
`tier-3`/`tier-4` 164, `tier-2,webview` 129, `tier-3,webview` 199,
`tier-3,headless` 228, `tier-0,sdk` 70, `tier-3,sdk` 181. Every count on this
page is the number libtest reports as *running* - passed plus `#[ignore]`d, not
passed alone - so a plain run of `tier-3,webview` prints 193 passed and 6 ignored
against the 199 above. The five `packed` tests are in every bundle, which is why
every bundle is five higher than the one `implementation.md` §3.5 recorded when
it was written; on top of that, `tier-3,webview` carries the registry screen test
§3.2 added, and every bundle with `onchain-write` carries §5.1a's 26 watch tests,
which is +31 at `tier-3`, `tier-4`, `tier-3,headless` and `tier-3,sdk` and +50 at
`tier-3,webview`, whose remaining 18 are the auto-detect front door, the handler
test beside it, and §5.4's two cooldown-countdown payload tests. The ignored
ones are one network test in every bundle, plus the five anvil-gated webview
session-flow tests under `tier-3,webview`.
`tier-1` and `tier-2` diverge because `attest` needs `onchain-read`.
`tier-2,webview` and `tier-3,webview` diverge because the window's purchase
screen, the code attestation guarding it, and the tier-3 session flow all need
`onchain-write` or `cooldown`. The `sdk` bundles diverge from each other by the
five session-projection tests, which need a session model to project.

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
- **`packed::tests`** - what `rub3 pack` baked in (§2.5), compiled in every bundle. The test build asserts itself *not* to be a packed build, which is what gives the other three a fixed subject: the factory is `None`, since it is the one constant with no placeholder and an unpacked build may not invent one; no application is embedded; the placeholder `CONTRACT` is still the zero address the wrapper reads as "no contract configured", which is why a stock build never touches the chain; the numeric constants parse at compile time; `provenance()` names the app id, contract, chain and rpc it will act on and says "development build" where a packed binary names its factory; and `provenance_never_hands_out_the_endpoint_key` drives the rendering with an endpoint carrying a provider key in its path, in a query parameter and as userinfo - `--version` is reachable by every licence holder, so the endpoint goes through the same `rpc::redact_urls` the error surface uses and is reduced to scheme, host and port, with an authority that will not parse dropped whole rather than passed through
- **`rpc::tests`** - provider construction, contract call error paths, `encode_activate_calldata` selector + layout, `get_tx_receipt` / `get_block_number` / `get_code` error paths, ENS stub; the endpoint redaction (§2.8), which is a property of the wrapper's whole RPC error surface rather than of any one screen: a key placed in a path segment, in a query parameter and as userinfo must each be absent from the error's `Display` after construction through `RpcError::transport` and `RpcError::contract` while the host, the port and the failure text survive; a bracketed IPv6 authority keeps its host and loses its key, including when trailing punctuation follows it; an address whose authority will not parse at all is dropped whole rather than half-printed, which is the fail-closed property that a bare `[redacted url]` followed by a verbatim path would violate; plus one drive of `tokens_of_owner` against a dead port because alloy classifies an unreachable node during an `eth_call` as a contract error rather than a transport one; and for the EIP-3009 rail (§2.2) the `ReceiveWithAuthorization` typehash against its literal preimage, the signing digest against a vector computed independently with `cast`, every signed field proving it changes that digest, and the `purchaseWithAuthorization` calldata selector
- **`session::tests`** (requires `session` feature) - message determinism, tier-diffing, expiry edge cases, sign/verify round-trip, wrong-wallet failure; with `cooldown` adds: `verify_onchain` missing-field + bad-URL paths, `should_reverify` distribution sanity
- **`session_store::tests`** (requires `session` feature) - save/load round-trip, missing-session, `load_latest_session` picking the freshest valid session (`load_latest_session_for_wallet` narrows the same scan to one signer, covered from `activation::tests`)
- **`identity::tests`** - `IdentityModel` parsing and wire format, ERC-6551 TBA derivation determinism and sensitivity to each input, `resolve_user_id` for both models
- **`signer::tests`** (requires `headless` feature) - hex key parsing (bare/prefixed/padded, and every rejection: wrong length, non-hex, zero and out-of-curve-order scalars), `Debug` redaction and error messages asserted not to echo the input, `personal_sign` / `sign_prehash` recovery, RFC-6979 determinism, keystore decrypt, password-file precedence, and the strict env-key-over-keystore resolution order with no fall-through on a malformed key
- **`tx::tests`** (requires `headless` feature) - invalid-URL transport error, the node's `insufficient funds` classifier, the shortfall message with and without known amounts, and (§2.8) the endpoint redaction on this side of the wall: `send` driven against a dead port for a key in a path segment, in a query parameter and as userinfo, asserting none of them reaches what the agent door prints. `tx` builds `TxError` from alloy directly and never passes through `RpcError`, so it inherits nothing from that type's constructors and needs its own coverage of the one shared sanitiser
- **`activation::tests`** (requires `headless` feature) - the exit-code table asserted value-by-value, all classified codes distinct and disjoint from 0/1/2, `machine_detail` contents, `lowest_token` selection, the token- and wallet-scoped session fast path, every unconfirmed-purchase outcome mapping to the terminal code 21, and (§2.6) the `NotCanonicalContract` refusal: its message naming the function the pre-filter saw and stating that nothing was signed, its detail line reporting `code_bytes=` and `exposed=none` rather than an empty value, the factory case reporting `sells_licences=false`, and every classified code appearing in the `--help` table; and (§2.9) `the_refusal_line_says_what_the_code_registry_answered`, which pins the three-valued `registry=` field the same line now carries - `not_consulted`, `unknown` and `unavailable` are one refusal but three different things to alarm on, and today's shipped answer is the first of them; and for the spend ceiling (§2.2) `SpendPolicy`: an unset `RUB3_AGENT_MAX_TOKEN_AMOUNT` leaving the rail unavailable rather than unlimited, the ceiling inclusive at the boundary, zero as a real ceiling rather than "unset", the refusal carrying `listed`/`maximum`/`token`, and every malformed value a hard `Config` error naming the variable; and for the pre-flight's authorization disclosure (§2.2) that the two signed copies share one salt - and therefore one single-use nonce - differing only in `validBefore`; and for the ETH ceiling (§2.7) the same properties on `check_eth_wei` plus the ones the default introduces - an unset `RUB3_AGENT_MAX_ETH_WEI` meaning `DEFAULT_MAX_ETH_WEI` rather than either zero or unlimited, an ordinary 0.01 ETH listing still buying under it, `0.05` rejected as a hard error naming wei as the unit, and neither rail's variable moving the other's. The relationship between the two windows themselves is a `const _: () = assert!(..)` beside the constants rather than a test, so a window that stopped being short fails the build in every build that compiles the headless front door - the constants live inside `mod headless`, so of the eight matrix bundles only `tier-3,headless` sees the assertion, and it is the only one where they exist at all
- **`rpc::stub_node_tests`** - the token-side call classifier (§2.2), driven through `stablecoin_rail` and `preflight_purchase_with_authorization` against a local stub endpoint answering one fixed body each (the endpoint itself is `test_support::StubNode`, shared with `webview::tests` and the §5.1a watch suites), rather than asserted about the classifier in isolation: a revert (`code: 3`, and `-32000` with revert wording) or empty return data is a settled contract answer, so the rail reads as absent and the run continues on ETH; a JSON-RPC error body, an execution timeout and an undeserializable body are node failures that propagate instead of silently changing the currency
- **`rpc::tests::receipt_polling`** (requires `onchain-write` or `cooldown`) - the receipt poll loop driven over scripted answers: a transient transport failure does not end the wait, one that outlasts the budget is reported as `Transport`, a recovered poll ends as `Timeout`, a request that can never succeed (an unparseable tx hash) is reported at once instead of consuming the budget, and both outcomes report real wall-clock waiting time rather than the nominal budget
- **`rpc::watch_loop_tests`** (requires `onchain-write`) - the §5.1a watch loop with the network taken out, driven over scripted polls: a match on the first poll returns at once and one after several empty polls still does, a transient failure is absorbed and a single good answer resets that run, five failures in a row end the watch as the failure it is rather than as a timeout, an unretryable failure ends it immediately, an exhausted budget ends it as `Timeout`, and a cancellation raised before the first poll or during the sleep between polls ends it as `Cancelled` inside one poll interval, and a watch held for a cooldown polls nothing until the hold is over, then gets its whole budget, and is cancellable throughout the hold. `rpc::retry_read` runs a single read on those same terms, which is what the two set-up reads of a watch use
- **`rpc::watch_rpc_tests`** (requires `onchain-write`) - both §5.1a watchers against a `StubNode`. Mint side: the filter that goes out pins the contract, the mint and the recipient, and everything that comes back is re-checked term for term, so an endpoint that honours `address` and degrades on `topics` cannot pass off a resale, another wallet's mint, another contract's mint, an unrelated four-topic event or a three-topic ERC-20 transfer as this screen's purchase, while the real mint is still found beside them. Activate side: `lastActivationBlock` moving past the start block is resolved by one block-scoped `eth_getLogs` on the `Activated` topic, asserted on the methods a poll is allowed to ask for, because the receipt-per-transaction scan it replaced was hundreds of sequential requests on a Base block; an activation of another token or from another contract is ignored, a block at or before the start is the old activation rather than a new one, and a refusing node is a retryable failure rather than a quiet chain. A request the endpoint accepts and never answers is abandoned after `rpc::WATCH_REQUEST_TIMEOUT` and reported as retryable, which is what keeps a watch's deadline reachable and its cancel flag readable
- **`attest::tests`** (requires `onchain-read`, so tier-2 and up) - the pre-purchase code check (§2.6). Drift: every fingerprint and immutable range in `contracts/canonical-bytecode.json` is pinned in `attest::CANONICAL` (the failure prints the row to add), the pinned hashes are lowercase hex of 32 bytes, the ranges are sorted, disjoint and one word wide, and `FORBIDDEN_SIGNATURES` is compared against the `string[N]` array in `contracts/test/Rub3Invariants.t.sol` with Solidity comments stripped first. Comparison: a legitimate deploy that chose different immutables still matches, a truncated deploy is refused rather than partially masked, an address with no code says so, and the selector helper is checked against the published `transfer(address,uint256)` vector. The negative case is `a_renamed_seizure_function_passes_the_name_scan_and_fails_the_hash` - an owner-only seizure named `reconcileLedger(uint256,address)` is asserted to pass the blacklist in silence and to fail the masked hash, which is the asymmetry the module exists for. Gate: `only_licence_roles_are_purchase_targets` runs `decide()` over the shipped table and requires exactly the `Role::Licence` rows accepted and every factory and deployer row refused as `NotALicence`, and `the_attest_module_is_reachable_only_from_the_purchase_path` walks the crate's own `src/` recursively and asserts three things: the set of modules referencing the module at all, by any item rather than only by calling the gate, is a subset of the purchase-path allowlist (`activation.rs`, `webview.rs`); each allowlisted purchase path holds exactly one call site (`activation.rs::headless::purchase` and `webview.rs::show_purchase`), since a subset is also satisfied by calling the gate nowhere; and the named human launch entry points inside `webview.rs` (`show_activate`, `show_cooldown`, `finalize_session`) reference it not at all, which is the half a file-granular allowlist cannot speak for, failing loudly rather than vacuously when one of those functions can no longer be found. That is how "fail closed on purchase, fail open on launch" is enforced structurally rather than by a default. It guards source structure, not runtime wiring, and it is not total: a new launch function added to `webview.rs` is unguarded until it is named, and the same file granularity means a reference elsewhere in `activation.rs` is not caught either. The behavioural half is the launch-path e2e below. Registry (§2.9), driven over a scripted `ChainReader` whose call log is half the point: the three-way verdict of the design (a pinned-table hit that asks the registry *nothing*, a release the registry vouches for, and code neither knows); the registry's own code verified before its answer is believed, with `a_registry_whose_own_code_is_not_canonical_is_never_believed` scripting an answer that *would* be accepted if the check were skipped, `canonical_code_that_is_not_a_registry_is_not_believed_either` for the factory at that address, and `an_unverified_registry_is_never_asked_anything` asserting through the log that nothing follows a failed verification; each of the three reads failing in turn; a hostile offset table dropped and never even hashed under; a record declaring ranges other than the table that found it; every candidate table tried, and the two ends of the bound on that: `candidate_offset_tables_are_capped_on_the_purchase_path` publishes one more usable table than the cap allows and hides the answer under the oldest, first-published one - which is the table outside the budget now that the wrapper reads from the newest end - then asserts through the call log both that the cap is what the registry was *asked* for and that exactly the cap's worth of lookups followed, while `the_offset_table_read_is_bounded_before_the_answer_is_decoded` and `a_registry_that_ignores_the_requested_window_still_buys_no_extra_lookups` split the two costs apart against a registry holding four times the cap - the read is requested bounded so a long published set never reaches the shape check, and a node that answers with more than it was asked for still buys no extra round trip; `the_budget_is_spent_on_the_newest_layouts` pins which end that budget goes to, since a registry is consulted only after a pinned-table miss and a miss is by definition about newer code: out of 64 layouts a release published under the newest is found in a single lookup, and one published under the oldest is refused as unknown, which is the trade the cap makes on purpose; a deprecated release bought with a warning that says held licences are unaffected; a registry record for a non-licence role including one this build has no name for; the shape rules for an immutable slot, `PUSH32` included; `registry_table_mirrors_the_deployment_manifest`, which pins `attest::REGISTRIES` to `contracts/deployments.json`; `nothing_is_deployed_so_the_accumulate_only_rule_is_not_live_yet`, which reads that same manifest and asserts every `factory` and every `code_registry` is still `null`, so `CANONICAL`'s statement that its rows may still be corrected in place is a checked fact rather than a comment - the two records are independent deploys, either one going live arms the permanence rule for what it put on chain, and this fails at that moment so the doc gets updated instead of quietly rotting; `an_unknown_role_number_is_never_guessed_at`, since the first variant is a licence and a registry newer than this build can publish a role this one has no name for (the numbering itself is a wire encoding and is held by the anvil suite, not here); `an_empty_address_is_refused_without_asking_the_registry`, because an address holding no code has no release for an authority to recognise and must keep the one refusal a person can usually act on; and `registry_supplied_text_cannot_break_the_line_that_quotes_it`, which publishes a contract name and a version label carrying a space, an `=` and a newline and asserts neither can invent a field in the agent door's `key=value` detail line nor forge a `rub3:` line of the wrapper's own output. The one that matters most is `an_unpublished_registry_changes_nothing_about_the_gate`: no chain carries a registry address, a table miss costs no extra chain read, and the refusal string is unchanged to the sentence, so the whole step is inert until something is deployed
- **`webview::tests`** (requires `webview` + `onchain-write`, so only `tier-3,webview`) - the human purchase gate (§2.8). `show_purchase` driven against a `StubNode` answering with non-canonical code emits exactly one message to the window and it is the refusal, so "it also showed the purchase screen" cannot pass; against an unreachable endpoint the failure reported is the code check rather than the supply read, which is how the ordering claim is tested rather than asserted. The words are covered too: the two refusal causes carry different titles, bodies and next steps, an address holding no code says so instead of talking about a mismatch, every notice names the address and survives its source line continuations as finished prose, and a failed read shows the kind of failure only rather than a network error the buyer is then told to forward. The endpoint redaction itself is tested a layer down, in `rpc`, since it is a property of the error value rather than of this screen; what is tested here is that it holds on the window's *other* error surface too, by driving `handle` with a `connect` message against an endpoint whose URL embeds a key and asserting the "ownership check failed" box carries none of it. The §2.9 additions: the three code-registry outcomes read differently and none is retryable, the registry is named as the wrong address rather than as bad code, a role this build is too old to name is refused in words a person can read, and `a_failed_registry_read_never_puts_the_packed_endpoint_on_screen` covers the second route to the same leak - a registry read's failure reason travels *inside* the refusal rather than beside it, so `Unrecognised::shareable_detail` drops it while keeping the registry's *answer*, which names no endpoint. `a_deprecated_release_advises_the_buyer_rather_than_alarming_them` covers the other direction: a deprecated release has to reach the person, since the premise of this whole screen is that a buyer cannot read bytecode and a buyer does not read stderr either, and it has to reach them as advice - the sentence names the release, says the code is genuine and says the licence stays valid, and it promises no successor, since the record carries none, while a current release and a pinned-table hit say nothing at all. The §3.2 addition is `the_two_registries_are_described_differently_on_the_blocked_screen`: the discovery registry is described as what it is rather than in the code registry's words - those two are the pair a buyer is most likely to have confused already - and no two roles read identically on the blocked screen, so a role added later cannot silently inherit another one's sentence. The §5.1a addition is `the_handler_accepts_the_auto_detect_messages_the_page_posts`, spelled out as the JSON that crosses the seam rather than as the enum: serde answers a tag it does not know by logging and returning, so a rename fails here instead of in front of a person. That the page posts exactly those messages is not assertable from Rust and was covered by driving the rendered page in a browser, recorded in section 6. The §5.4 addition is the pair behind the cooldown screen's countdown: `the_cooldown_screen_is_handed_the_seconds_its_countdown_runs_on` asserts the `onShowCooldown` payload carries a `cooldownSecsRemaining` that agrees with the blocks beside it, since the page turns that one number into the stamp it counts down and nothing later tells it when the hold ended, so a dropped or disagreeing field leaves a person watching a clock that never moves; `a_ready_token_opens_no_countdown` asserts a ready token is handed a zero instead, so no wait is opened on a token the contract would already take a transaction for. The ticking itself is the page's, and is in section 6 with the rest of the browser record
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

The human front door's §1.8 flows: connect, activate, sign, persist, restart,
plus the §5.1a auto-detect door layered on the same screens. Lib
tests rather than an integration suite, because the seam they drive -
`webview::IpcState`, the activation window's IPC handler - is private to
`src/webview.rs`. A `Window` driver wires that handler to channels instead of a `wry`
view, so a test posts the JSON the page posts and reads back the JS the page
would have run.

**What this does not cover**, and is still §1.7's manual testing: the `wry`/`tao`
layer itself, and `assets/activation.html` - the JS that renders each screen,
carries `pendingSessionCtx` across the cooldown → confirm → sign hand-offs, and
posts the messages back. Everything between the two is covered.

Five of them are anvil-gated on port **8551**, so they run alongside
`session_onchain_e2e.rs` (8547) and `headless_e2e.rs` (8549); they are
`#[ignore]`d, print `SKIP:` and pass when Foundry is absent, and serialise
themselves through a file-level mutex plus the crate's `ENV_LOCK`. Each seeds a
licence with `cast` rather than buying one, so none of them depends on the local
build reproducing `contracts/canonical-bytecode.json`.

- `a_connected_wallet_activates_signs_and_the_session_survives_a_restart_e2e` - the whole flow: `onAppInfo`, `connect` → the cooldown screen, the wallet broadcasting that screen's own calldata, the poller's `onTxConfirmed` checked against the receipt's real block hash and normalised owner address, the wallet signing that preimage, `SessionSuccess` verified locally and on-chain, `activation::persist_activation`, then `activation::ensure` returning from the fast path with no window and no second `activate()`
- `a_second_activation_inside_the_cooldown_is_refused_and_the_window_says_how_long_e2e` - the contract refuses with `CooldownActive` and does not move `lastActivationBlock`; the window reports `ready: false` with the blocks the contract is still counting, holds that two blocks out, and clears at the boundary with session id 2. Two blocks rather than one because `cooldownReady` is evaluated at the head while the transaction executes in the next block
- `an_expired_session_is_refused_and_a_fresh_activation_replaces_it_e2e` - a two-second TTL, then `activation::try_session_fast_path` declines the lapsed session and a second pass through the flow issues a fresh one
- `auto_detect_finds_the_activation_and_the_session_completes_e2e` - §5.1a against a real node: the page opens the Auto-detect tab, the wallet broadcasts the screen's own calldata with no paste, and `watch_for_activate` resolves `lastActivationBlock` moving to the `Activated` log that moved it. The hash comparison is what proves it picked the wallet's own transaction rather than merely something in the same block
- `auto_detect_finds_the_mint_and_the_flow_continues_e2e` - the same for `watch_for_mint`: a wallet with no licence buys one, and the token id the flow arrives at is the one the chain minted

The remaining one is not anvil-gated and not gated on `cooldown`, so it runs in
the ordinary matrix under `tier-2,webview` as well:

- `a_zero_contract_build_still_issues_and_serves_a_legacy_licence_proof` - with no contract configured the window issues a `LicenseProof`, and a later `ensure` is served from it against an RPC URL nothing answers on, which is what proves the path reads no chain

**The §5.1a auto-detect front door** (`src/webview/session_flow/auto_detect.rs`)
drives the same seam against a `StubNode` rather than a chain, so it needs no
Foundry toolchain and runs in the ordinary `tier-3,webview` matrix entry. What it
asserts is mostly sameness and stopping, because auto-detect's whole correctness
claim is that nothing downstream of the hash changes:

- `a_found_hash_and_a_pasted_hash_drive_the_same_flow` - both doors against one node, compared call for call, which is what would catch a second finalize path
- `a_watch_stops_when_the_page_switches_tabs`, `a_watch_stops_when_the_page_moves_on`, `starting_a_watch_stops_the_one_before_it` - a leaked watch is observed from outside, by counting what still reaches the endpoint after the screen stopped caring
- `a_watch_cancelled_mid_request_reports_nothing` - a cancel raised while a request is in flight surfaces as that request's failure, and must not write a fallback into a screen the user has already left
- `a_watch_cancelled_as_it_finds_the_hash_drives_nothing` - the same race the other way round: the poll still in flight when the cancel lands comes back holding the match, and the hash it found drives nothing, because the screen that asked for it is gone
- `returning_to_auto_detect_resumes_the_window_the_screen_opened` - the search window belongs to the screen, not to the latest arm: a transaction that lands while the user is on the Manual tab is still found when they switch back, which re-reading the head on every arm would put permanently out of range
- `one_bad_answer_to_the_first_read_does_not_end_the_watch` - the head block and the cooldown are read before the poll loop exists, so they get the loop's own retry policy (`rpc::retry_read`); one bad answer to the first of them must be absorbed the way one mid-poll is, rather than ending auto-detect a second after the screen rendered
- `an_unreachable_node_lands_on_the_manual_tab`, `the_two_ways_of_giving_up_say_different_things` - the fallback copy: "we could not reach the network" and "we watched and saw nothing" call for different next steps, and neither invites a second purchase
- `the_copy_waits_for_a_cooldown_that_has_not_ended` - the fallback wording is composed in Rust rather than in the page, so one test covers the whole rule: on a token still cooling the advice points at the cooldown instead of telling a person to send a transaction the contract would revert, and the purchase screen's specified default sentence is unchanged
- `a_watch_that_fails_inside_a_cooldown_says_to_wait_for_it` - and that wording reaches a real screen: a watch that gives up on the head block it opens with, before it ever reads a cooldown of its own, still says to wait for one, off the `cooldownReady` answer the screen was built from rather than a second chain read
- `an_activation_watch_waits_out_the_cooldown_before_polling` - a cooling token is left alone until the contract would accept an `activate()`, and polled once it would. Both halves are asserted, because a watch that never polls is quiet too and one that ignores the cooldown reaches the endpoint eventually as well

Run the anvil-gated five with:

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

In-process EVM: no network, no `.env`. 255 tests across six files.

- **`test/Rub3Access.t.sol`** (28) - metadata, constructor validation, purchase (including the exact-payment rule: under, over, exact, and a 256-run fuzz proving the listed price is the only accepted amount), supply cap, activation and cooldown, owner gating
- **`test/Rub3Invariants.t.sol`** (39) - the ownership invariants, in four groups: 17 on the append-only hash set (constructor seeding, zero/duplicate rejection, older releases staying valid, revocation status/reason/events, revoked hashes not resurrectable, owner gating), 14 on the successor pattern (opt-in on both sides, one claim per token, the claim following the current holder, survival of renounced ownership, and the trust rule surviving a successor repoint), 4 on mint ordering and the predecessor probe (a `MintCallbackProbe` recipient reading its own `wasClaimed` provenance from inside `onERC721Received` on the claim path, the one mint path that writes per-token state before minting, and `IncompatiblePredecessor` against a codeless and a non-licence predecessor with a well-typed one completing a claim end to end), and 4 on the no-revocation audit (25 forbidden signatures × 3 deployed contracts, the third being the §2.3 `Rub3Factory`, with a positive control proving the scanner finds selectors that do exist - including `feeBps()` and `treasury()`, the getters that exist while every setter for them is absent)
- **`test/Rub3TokenPurchase.t.sol`** (32) - the EIP-3009 stablecoin rail of §2.2. The buyer holds stablecoin and a zero ETH balance in every test, and a separate submitter sends every transaction: replay, front-running (diverting the mint, stripping it by calling the token directly, and calling `receiveWithAuthorization` as a third party), an authorization aimed at the wrong contract, the validity window and cancellation, a price move after the read rejecting on *both* rails (the stablecoin one through the digest, the ETH one through the exact-payment check), the balance-delta check, the constructor probes, both rails minting identically, a recipient callback that finds the money already in and the token already its own, an EIP-1271 smart-contract wallet buying (and a signature it rejects buying nothing), and a token implementing only EIP-3009's `(v, r, s)` form deploying happily and then being unspendable for that reason alone (signed against its own domain, empty revert data, the same fields spent through the split form it does implement, and the same authorization shape minting against the mock)
- **`test/Rub3Factory.t.sol`** (52) - the factory and the protocol fee of §2.3, in eight groups: the factory itself (terms stamped on every deploy, `isDeployed` plus ordered enumeration, owner defaults, the `LicenseDeployed` log, the fee range rejected either side and accepted at both ends, and the two contract-size limits); immutability (a newer factory at a different rate leaving an older deploy untouched in terms *and* money, disjoint per-factory registries, and the contract owner running every power it has without moving the fee); exact ETH arithmetic at the boundaries (1 wei, the 39/40-wei rounding edge at 250 bps, an indivisible amount, 1,000,000 ether, and a 256-run fuzz over amount x rate); the same on the stablecoin rail; `test_bothRails_chargeIdenticallyForTheSameAmount`, which prices one contract at the same number in wei and in the token's smallest unit and asserts the two accruals are equal; direct deployment working, unrecorded, and unpenalised; fee evasion pinned from both sides by `test_eth_feeIsChargedOnTheListedPriceBecauseNothingElseArrives` and `test_eth_zeroPriceListingCannotCollectByOverpaying` - the inverted forms of two tests that used to assert an overpayment was taxed, closing the route at the payment now that neither rail can deliver more than the listed price - and the accrual rationale by `test_accrual_rejectingTreasuryCannotBlockPurchases`, where a treasury that refuses ETH fails only its own sweep while buyers still buy and the developer is still paid in full; and the canonical-predecessor rule (the laundering route reverting `PredecessorNotCanonical` and recording nothing, a canonical predecessor accepted with the migration completing end to end, the zero predecessor, cross-factory acceptance through `previousFactory` and its absence on an unlinked factory, both sides of the `MAX_PREDECESSOR_FACTORY_HOPS` bound, the constructor probe rejecting a non-contract and a half-answering `previousFactory`, and direct deploys and the deployer helper staying unconstrained and unrecorded)
- **`test/Rub3CodeRegistry.t.sol`** (35) - the append-only properties of §2.9's code registry, asserted as behaviour rather than as comments, in six groups: publishing (the record kept whole, an unpublished hash reading as `Unknown`, the block stamped by the contract rather than supplied, the permanent `Published` event, every role carried through unchanged, and an empty offset table accepted as a real answer); append-only (republish and overwrite both reverting and leaving the record untouched, a deprecated hash not republishable either, and `test_audit_noRemovalOrRewriteSurfaceExists` scanning the deployed runtime bytecode for 10 removal, rewrite and un-deprecate signatures with its own positive control - a separate list from the shared 25, which is about tokens and says nothing about a registry); deprecation (the entire record compared either side of one, the reason logged, no repeat, no undo, and no reach into another record); ownership (non-owner writes reverting on both writers, the two-step transfer taking effect only on acceptance, and `renounceOwnership` reverting because ownership here is the right to *add*); offset tables (interned so identical tables are one entry, announced only on first use, rejected at four malformed shapes including the exact EIP-170 boundary either side, and readable bounded - `offsetTableWindow` returns its bound out of a set four times larger, starts where it is asked to, and clamps rather than reverting past either end or at the largest `count` a caller can pass, while `latestOffsetTables` returns the newest layouts newest-first out of a set four times its bound and clamps the same way, which is what lets a purchase path read the bootstrap without paying for however many tables the owner key published and without spending its budget on layouts older than the build asking); and record completeness (the zero hash, a missing source commit, each empty text field, and a reverted publish leaving neither an enumeration entry nor an interned table)
- **`test/Rub3Registry.t.sol`** (69) - §3.2's *discovery* registry, which is not `Rub3CodeRegistry` and whose own suite guards the distinction: `test_neitherRegistryCanStandInForTheOther` asserts neither contract's runtime bytecode carries the other's selectors, with a positive control. Seven groups. Construction (the factory recorded and immutable, the zero and codeless cases, the `HalfFactory` probe that would otherwise deploy fine and reject every registration forever, `renounceOwnership` reverting because abandoning curation would freeze the token list, and the two-step transfer). The register gate (a canonical deploy listed, a directly deployed licence refused, a non-owner refused, authority following a licence-contract ownership transfer with nothing stored here, the zero address, a second registration, an empty `appName` on both writers, an empty `contentURI` accepted as "nothing published yet", both text fields accepted exactly at `MAX_APP_NAME_BYTES` / `MAX_CONTENT_URI_BYTES` and refused one byte over on both writers with the offending length and the limit in the revert, an older generation's deploy still listable through `previousFactory`, and both sides of the `MAX_FACTORY_GENERATION_HOPS` bound built out of a ten-generation chain). The listing lifecycle (delist keeping the record and its `registeredAtBlock` so relisting is not a demotion, relist, non-owner and repeated-state refusals on each, `updateListing` working while delisted, every write refusing an unregistered address, and a suspension that the listing's own owner cannot undo, that carries a reason bounded at `MAX_SUSPENSION_REASON_BYTES` through the same entry gate, that is owner-only, and whose lifting does not override an owner who withdrew in the meantime). **Discovery, never validity** - `test_delisting_cannotTouchAHeldTokenOrALiveSession` pulls every discovery lever at once on a licence with a paid token and a live session, then measures `ownerOf`, `honorsContract`, `activeSessionId`, a fresh activation past the cooldown, a fresh purchase and a transfer; `test_registryWrites_leaveTheLicenseContractUntouched` snapshots nine pieces of licence state across every registry write; and `test_audit_registryHoldsNoStateChangingExternalCall` walks the registry's runtime opcodes, skipping each `PUSH1..PUSH32` immediate, and asserts no `CALL`, `CALLCODE`, `DELEGATECALL`, `CREATE`, `CREATE2` or `SELFDESTRUCT` appears at all - so the invariant is a property of the bytecode rather than a reading of the source, with `test_audit_opcodeWalkFindsACallWhereOneExists` as the positive control on a licence contract that does move money. Ranking (ETH-only recognised, an unrecognised rail last, delisted and suspended entries omitted, the global window clamping rather than reverting at either end, and `test_rank_followsAPostRegistrationTokenPriceChange`, which registers two entries on opposite quotes, has both call `setTokenPrice` to swap, and asserts the order swaps with them - the one test a rank snapshotted at registration fails while passing every other test in the file). `test_priceTokenOf_readsAnAddressThatCannotAnswerAsEthOnly` and `test_priceTokenOf_readsAMalformedAnswerAsABadQuote` hold the "cannot answer at all is read as ETH only" rule against the shapes a `try`/`catch` does not catch - an address with no code, a silent fallback returning nothing, and a full word of high bits. The recognised-token list (enumerable across add and remove, owner-only, no-ops refused, and `address(0)` refused in both directions because the native rail is recognised by rule rather than by membership). **Reads whose cost the caller controls** - `registeredWindow` and `rankedRegistrationWindow` clamping at both ends, the window ranking only inside itself, `test_rankedRegistrationWindow_isNotAPageOfTheGlobalRanking` pinning the tradeoff the NatSpec states (an unrecognised entry in an earlier window still precedes a recognised one in a later window, so paging cannot reconstruct `rankedListings`), a cursor advancing by `count` rather than by page length across delisted entries, and `test_rankedRegistrationWindow_costDoesNotFollowTheSetSize`, which measures gas over a 24-entry registry and asserts both halves: the bounded read costs a fraction of the whole scan, and `rankedListingWindow` still costs the whole scan, which is exactly what its own doc says. And the agent card (every field against a live read, each wrapper hash carrying its own status including a revoked one, an unregistered contract answered rather than refused, the card following a price the contract has since changed, `cards` returning a globally ranked page, `cardWindow` returning a bounded one, and the `MAX_CARD_WRAPPER_HASHES` cap - `test_card_capsTheHashSetAndReportsTheTrueTotal` asserts the newest hashes are the ones kept and that `wrapperHashCount` still reports the true total, and `test_cardWindow_costDoesNotFollowOneListingsHashSet` measures a card at the cap against one eight times past it, which is the griefing vector closed, and `test_cardWindow_costOfAMaximalEntryIsTheStatedConstant` doing the same for the two text fields: a page holding an entry that used every byte it is allowed still carries only the published constants)
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

### The rub3 CLI (`crates/rub3-cli/`)

The `rub3` command of §2.5: `pack` and `deploy`. Its own workspace member, so
its suites are their own invocation and nothing it depends on reaches a shipped
binary:

```bash
cargo test -p rub3-cli
```

**The subject is mostly the refusal.** Nothing is deployed to any public network,
so `contracts/deployments.json` publishes no canonical factory and the path that
succeeds cannot be exercised against a real address. It is exercised against
`tests/fixtures/deployments-populated.json`, a fabricated manifest that publishes
one on `base` and leaves `base_sepolia` null - the two records have independent
lifecycles, and a fixture where everything is populated would not exercise the
refusal that matters.

- **`src/` unit tests** (17) - `deployments::tests` reads the *committed*
  manifest, compiled in with `include_str!`: every entry is asserted still null,
  so the day something is deployed, the tests that assume a refusal are read
  again rather than quietly passing; a null `factory` is refused with the chain's
  name and id and without offering an address; a name resolves to the id the file
  keys it by; an unknown name is refused with the list of the ones that exist; a
  chain id the manifest does not answer for is still addressable but has no
  canonical factory; a zero address written into the file is refused as malformed
  rather than used; and a schema bump is refused rather than read anyway.
  `tier::tests` covers the five bundles and all three spellings of each
  (`cooldown`, `tier-3`, `3`); `deploy::tests` covers the decimal-to-smallest-unit
  conversion on both rails, a price finer than the unit refused rather than
  rounded, and the wrapper-hash shape including the zero hash the contract reads
  as "unknown"; `repo::tests` covers checkout resolution
- **`tests/cli.rs`** (25) - the binary itself, driven against a temporary
  checkout carrying one manifest or the other. Both subcommands exit 2 on a null
  entry with a message naming the chain and containing no address to paste, and
  print no plan while doing it; a published factory is what gets baked in and
  what a deploy goes through; `--factory` is the only other way to get one and
  says on screen that it is not canonical; `--factory 0x0` is refused like every
  other zero address; a deploy without `--broadcast` says it is simulating; a
  `--direct` deploy passes no `FACTORY` at all *and* removes an inherited one;
  `--price-usdc` without `--price-token` is refused with the reason; the account
  model without a TBA implementation is refused; and
  `there_is_no_fetch_and_no_register_subcommand` asserts the two
  unbuilt subcommands are absent from the binary and from `--help`. Four of them
  are about what an exit code and a summary may say: a missing flag, an unknown
  flag and an unknown subcommand all exit 1 while the null-factory refusal still
  exits 2, so exit 2 means only "nothing is deployed yet" (clap's own default for
  a usage error is 2, which is what made this worth pinning); `--help` and
  `--version` exit 0; an `--app-id` carrying a separator or `..` is refused,
  since it names a directory the packed binary extracts into; and a deploy
  summary carrying `--private-key` after `--` prints how many arguments were
  passed through and none of them, because that summary is printed on every
  deploy and not only a `--dry-run`. Two more drive a real `pack` with `CARGO`
  pointed at a stand-in that records the environment it was given: an exported
  `RUB3_PACK_DEVELOPER_ENS` never reaches the build, and one asked for with
  `--developer-ens` does. Three cover the boundary between a rub3 flag and the
  forge passthrough, which begins at `--` and nowhere else: a `--slow` placed
  before the separator is a usage error naming it rather than an argument handed
  to forge, a `--dry-run` written after other flags still runs nothing (with
  `forge` replaced on `PATH` by a stand-in that records having been called, so
  "nothing ran" is observed rather than inferred), and a malformed command line
  on a chain with no canonical factory is reported as itself rather than as the
  factory refusal, which is the signal an orchestrator waits on
- **`tests/pack_build_gate.rs`** (6, `#[ignore]`d) - the wrapper's own gate,
  `crates/rub3-wrapper/build.rs`, which is not reachable from the CLI. Each test
  runs a **real `cargo check`** against a poisoned environment and reads what
  cargo says: a zero factory, a zero contract, a half-configured pack, an app id
  that would escape the cache directory, and the placeholders `null`, `TBD` and
  `0xYourFactory` are each refused. The last is
  the positive control - a complete environment builds - without which the other
  five would pass on a gate that refused everything. They are ignored because
  each one costs a compile, and they serialise on a mutex since they share a
  target directory:

```bash
cargo test -p rub3-cli --test pack_build_gate -- --ignored
```

`pack` and `deploy` have no anvil-gated suite. What one would cover is the
`cargo build` and the `forge script` they drive, both of which are covered where
they live, and neither can be exercised through the canonical path until a
factory exists. The end-to-end run that *was* performed by hand is recorded in
`implementation.md` §2.5.

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

The wrapper's identity lives in `crates/rub3-wrapper/src/packed.rs`. Each constant
has a development placeholder, which is what an ordinary `cargo build` compiles,
and a packed value that `rub3 pack` injects through a `RUB3_PACK_*` environment
variable on the build it runs:

| Constant | Placeholder | Purpose |
|---|---|---|
| `APP_ID` | `com.rub3.example` | Reverse-DNS app identifier |
| `CONTRACT` | `0x0000...0000` | ERC-721 license contract address |
| `CHAIN_ID` | `8453` | EVM chain ID (Base mainnet) |
| `RPC_URL` | `https://mainnet.base.org` | JSON-RPC endpoint |
| `DEVELOPER_ENS` | `None` | Optional ENS name |
| `SESSION_TTL_SECS` | `604800` | Session lifetime, 7 days |
| `FACTORY` | none | The canonical `Rub3Factory`, and the one constant with no placeholder |

The zero `CONTRACT` is what makes a stock build never touch the chain: the wrapper
reads it as "no contract configured" and skips the on-chain ownership check. To
test against a real contract, either edit the placeholder and rebuild, or pack a
binary against it:

```bash
cargo run -p rub3-cli -- pack --binary <app> --app-id com.rub3.example \
  --contract <deployed> --chain 31337 --rpc-url http://127.0.0.1:8545 \
  --tier verified --factory <a factory you deployed> --output ./dist/myapp
```

`--factory` is required there because nothing is deployed to a public network, so
`contracts/deployments.json` publishes no canonical factory for any chain and
`pack` refuses rather than substituting anything. `rub3-wrapper --version` prints
every value above out of the binary that carries it, with the endpoint reduced to
scheme, host and port: `--version` is reachable by every licence holder, and a
provider key lives in the userinfo, the path or the query.

## 6. The activation window's confirmation tabs, driven in a browser

The §5.1a tab strip lives in `crates/rub3-wrapper/assets/activation.html`, and
what that page posts back is the one half of the flow no Rust test reaches:
`webview::tests` owns the wrapper's side of the same message names. This records
the browser run that covered the page's half.

The file was copied out of the crate and opened in a browser with a stub ahead of
the page script:

```html
<script>
  window.ipc = { postMessage: json => console.log(json) };
</script>
```

`window.ipc` is what the webview injects. The page's last statement posts
`{"type":"ready"}`, so without the stub the load ends in an uncaught error there
and every later message the page tries to post fails the same way; logging the
JSON is what makes the outbound messages readable.

The two tabbed screens were then shown through the same entry points the wrapper
calls, with a representative payload:

```js
window.rub3.onShowPurchase({
  ownerAddress: '0x1111111111111111111111111111111111111111',
  contractAddress: '0x2222222222222222222222222222222222222222',
  chainId: 8453, priceWei: '1000000000000000', valueHex: '0x38d7ea4c68000',
  supplyCap: 0, nextTokenId: 7, calldata: '0xefef39a1',
  autoWatchSecs: 120,
});

window.rub3.onShowCooldown({
  tokenId: 7,
  ownerAddress: '0x1111111111111111111111111111111111111111',
  contractAddress: '0x2222222222222222222222222222222222222222',
  chainId: 8453, ready: false, blocksRemaining: 5, cooldownSecsRemaining: 10,
  calldata: '0x9d1b464a', autoWatchSecs: 120,
});
```

Each was driven twice: as above, and again with `autoWatchSecs` omitted, which is
what a build that cannot read the chain sends.

What was looked at, at the window's own 480x640 viewport:

- with `autoWatchSecs`, both screens open on Auto-detect with Manual beside it in
  the strip and reachable at any point, by its tab and by the panel's "Switch to
  manual" link; with it omitted the strip is gone and the manual panel is the
  screen, which is the single-mode fallback
- the auto panel's spinner, and its bar draining over the reported budget; on a
  cooldown screen still cooling the bar holds full until the cooldown ends and
  drains from there, so it empties when the watch gives up rather than earlier
- the cooldown copy differing while cooling: with `ready: false` and blocks
  remaining, the head reads "Waiting for the cooldown to end…" and the sentence
  under it says the contract will accept the transaction once the cooldown ends;
  with `ready: true` both read the broadcast-now wording the purchase screen
  always carries
- `window.rub3.onAutoWatchEnded({ kind: 'mint', reason: 'timeout', detail: '…' })`
  leaving the screen where it was and moving it to a pre-populated Manual tab:
  the wrapper's sentence above the box, and the hash input focused for the paste
- the logged JSON across a screen change: one `auto_watch_cancel` for the screen
  being left, then one `auto_watch_start` for the one arriving, once each and in
  that order

The cooldown screen's countdown (§5.4) was driven in the same run, since it is
the other half of that screen with no Rust-side seam: `webview::tests` covers the
`cooldownSecsRemaining` the payload carries, and the ticking of it is the page's.
Two payloads, the ten-second one above and a realistic `blocksRemaining: 1800`
with `cooldownSecsRemaining: 3600`:

- the clock reads `1:00:00` on the realistic payload and `59:59` a second later,
  so the hours field appears only while there is an hour of wait, and under it
  "Estimated from 1800 blocks remaining on Base" names the chain's own figure the
  estimate came from
- the ten-second payload counted `0:05`, `0:04`, `0:03` and on to zero, at which
  point the red wait box disappears and the head and the sentence under it flip
  to the broadcast-now wording, leaving the screen identical to the one a
  `ready: true` payload renders
- one second per second: showing the cooldown screen twice in quick succession
  and reading the clock 3.1 s later had it 3 s lower, not 6, so the second render
  replaces the timer rather than racing a second one against it
- the clock freezing on `window.rub3.onProcessing('…')` and never resuming, which
  is the timer being cancelled on the way out of the screen the way the auto
  detect watch is
- a `ready: true` payload starting no clock at all: the wait box stays hidden and
  the value under it does not move

## 7. The on-chain test plan

Three tiers, separated by one question: **is what this run deploys written into
[`contracts/deployments.json`](contracts/deployments.json)?** Tier 1 deploys to a
throwaway local chain, tier 2 deploys to a public testnet and records nothing,
and tier 3 is the single recorded canonical set that the quickstart, the packed
wrappers and `attest::REGISTRIES` all point at. Everything about custody,
permanence and blast radius follows from that one distinction, so it is the
first thing to settle about any run.

| Tier | Chain | Recorded in `deployments.json` | What it is for |
|---|---|---|---|
| 1 | local anvil (ports 8547, 8549, 8551, 8553) and the in-process EVM | never | every automated suite; the per-PR gate |
| 2 | Base Sepolia, own factory and own registry | **never** | rehearsing the deploy scripts, the treasury proof, and the real USDC rail |
| 3 | Base Sepolia, one canonical set | **yes, once** | what the quickstart and packed wrappers target |

**The rule, and it is not a preference.** A tier-2 deployment is scratch: its
addresses live in a shell history and a scratch note, never in
`deployments.json`, never in `attest::REGISTRIES`, and never in a binary handed
to anyone else. A tier-3 deployment is canonical: it is written down once, it is
never used for an experiment, and what it puts on chain is permanent. There is
no third state and nothing gets promoted from one to the other: a scratch
factory that turned out to work is still a scratch factory, and the canonical
one is a fresh deploy.

### Tier 1: local Anvil

Everything the automated suites already cover. Nothing here needs a key, a
faucet, an endpoint or a decision, which is why all of it runs on every pull
request.

```bash
scripts/check-deployments.sh                   # the manifest schema gate
(cd contracts && forge test)                   # 255 tests, in-process EVM
(cd contracts && forge fmt --check)

cargo test -p rub3-wrapper --no-default-features --features tier-3 \
    -- --ignored session_verify_onchain_e2e    # 1  test,  port 8547
cargo test -p rub3-wrapper --no-default-features --features tier-3,headless \
    -- --ignored headless                      # 30 tests, port 8549
cargo test -p rub3-wrapper --no-default-features --features tier-3,webview \
    --lib -- --ignored webview::session_flow   # 5  tests, port 8551
cargo test -p rub3-wrapper --no-default-features --features tier-2 \
    --test code_registry_e2e -- --ignored code_registry  # 1 test, port 8553

cargo test -p rub3-cli
cargo test -p rub3-cli --test pack_build_gate -- --ignored   # 6 real cargo checks
```

Section 2 above is the per-suite inventory. All four anvil suites self-skip when
Foundry is absent, so **a pass in 0.00s is a skip**; CI holds each one to the
count in its `EXPECTED_TESTS` through `scripts/assert-e2e-ran.sh` for exactly
that reason.

**What tier 1 proves.** Contract logic and the ownership invariants against a
real EVM; both payment rails, including the EIP-712 domain, the derived nonce,
`msg.sender == to`, replay and front-running; the protocol fee split and both
sweeps; the pre-purchase code gate, including a modified licence that passes the
selector scan; the registry consult over a real ABI, with the pinned
fingerprints checked against contracts actually compiled and deployed; the
headless exit-code table; and session persistence and re-verification.

**What tier 1 cannot prove**, which is the whole reason tiers 2 and 3 exist:

- **Real USDC.** The stablecoin rail runs against
  `contracts/test/mocks/MockEIP3009Token.sol`, whose own header says what it
  deliberately does not model: USDC's blocklist, its pausing, and its upgrade
  proxy. It reproduces the authorization protocol faithfully and it is not
  Circle's code.
- **Real RPC behaviour.** Anvil answers instantly, without rate limits, without
  a log-range cap and without reorgs. The watch loop's retry budget and
  `rpc::WATCH_REQUEST_TIMEOUT` are exercised against scripted stubs
  (`rpc::watch_loop_tests`, `rpc::watch_rpc_tests`) and against anvil, never
  against an endpoint that throttles or lags.
- **Real signing custody.** Every anvil suite uses anvil's own published
  deployer key or a key the test generated in-process. No keystore under a real
  password file, no hardware signer, and no Safe.
- **The deploy scripts.** No automated suite executes
  `contracts/script/Deploy.s.sol` or `contracts/script/DeployFactory.s.sol`
  against any chain. The anvil suites deploy with `forge create`, and
  `tests/cli.rs` asserts the *printed plan* names the script, replacing `forge`
  on `PATH` with a stand-in where it has to prove nothing ran. As section 2
  already notes, `pack` and `deploy` have no anvil-gated suite at all.
- **Anything about a canonical deployment.** `isDeployed` is per factory and per
  chain, and there is no canonical factory anywhere yet.

### Tier 2: unrecorded Base Sepolia scratch deployments

A developer's or a maintainer's own `Rub3Factory`, own `Rub3CodeRegistry` and
own licence contracts on Base Sepolia, deployed directly and **never written into
`contracts/deployments.json`**. This tier rehearses everything tier 1 cannot
reach, and it is not blocked by the treasury decision or by the launch
sequencing, because it records nothing and therefore decides nothing.

**Why the contracts permit this by design**, each point checkable rather than
asserted:

- **A scratch treasury is accepted.** `Rub3Factory`'s constructor rejects only
  `address(0)` and performs no code check, so an EOA is a valid `TREASURY`. See
  [contracts.md](contracts/contracts.md#treasury-custody-and-the-pre-mainnet-proof),
  which states that same fact as the reason the mainnet address needs its own
  check.
- **`isDeployed` only means something at a published canonical address.** It is
  per factory, so a row on a scratch factory is a row nobody agreed on.
  [contracts.md](contracts/contracts.md#the-accepted-position-on-fee-free-deployment)
  is explicit that a verifier must check `isDeployed` on a specific, known
  factory address and must conclude nothing from a matching fingerprint.
- **A scratch factory cannot become a predecessor of anything canonical by
  accident.** `isCanonicalPredecessor` walks only the immutable `previousFactory`
  chain, which is set once in the constructor. The one way a scratch factory
  enters that chain is a later factory deployed with `PREVIOUS_FACTORY` naming
  it, which is a tier-3 act performed on purpose.
- **Nothing in the repository reads an unrecorded Sepolia address as canonical.**
  `rub3 pack` and `rub3 deploy` take the factory from the manifest and refuse
  when it is `null`; `--factory` is the only other route and says so on screen
  (`pack` prints `named with --factory: not a canonical deploy`, `deploy` prints
  `named with --factory: not the canonical factory`). `attest::REGISTRIES` is `None`
  for chain 84532. And the wrapper never reads `isDeployed` at all:
  `packed::FACTORY` is rendered by `--version` and consumed nowhere else.

**What tier 2 therefore proves, and what it does not.** It proves that the
scripts, the recipes and the real rails work against a public chain with real
latency, real gas and real USDC. It proves nothing about canonicity: a licence
deployed here has genuine rub3 *code* and no canonical *deployment*, and those
two are different questions that
[contracts.md](contracts/contracts.md#the-accepted-position-on-fee-free-deployment)
keeps apart on purpose. It also cannot exercise the wrapper's code-registry
consult; see [The code registry has no override](#the-code-registry-has-no-override)
below.

Every command below is either one run against Base Sepolia while writing this
plan (the reads in [Real USDC on Base Sepolia](#real-usdc-on-base-sepolia)) or
one lifted from the recipe that owns it in
[contracts/contracts.md](contracts/contracts.md). Nothing here has been
broadcast from this repository.

#### Prerequisites

- Foundry (`forge`, `cast`, `anvil`) on `PATH`.
- A Base Sepolia endpoint. `https://sepolia.base.org` answers reads;
  [contracts.md](contracts/contracts.md#1-copy-and-fill-env) lists the provider
  options for anything heavier. `cast chain-id --rpc-url https://sepolia.base.org`
  answers `84532`.
- A **throwaway** deployer key, generated for this and nothing else
  (`cast wallet new`), funded from the
  [Base network faucets](https://docs.base.org/get-started/get-funds).
- Test USDC from the [Circle faucet](https://faucet.circle.com), selecting Base
  Sepolia.
- A `BASESCAN_API_KEY` only if you want `--verify` on the deploys or
  `cast interface` for the token check. Neither is required.

#### The procedures

1. **A generation-1 factory.** `forge script script/DeployFactory.s.sol` with
   `FEE_BPS` and a `TREASURY` that is a throwaway EOA, and no `PREVIOUS_FACTORY`.
   The variables are in
   [contracts.md](contracts/contracts.md#environment-variable-reference); the
   broadcast form of the command is in
   [contracts.md](contracts/contracts.md#3-broadcast-and-verify).
2. **A licence through it**, with the `FACTORY` grep guard from
   [contracts.md](contracts/contracts.md#deploying-through-the-factory) kept
   exactly as written: forge reads a `FACTORY` it cannot parse as an unset one,
   so the guard is what stops a typo from becoming a silent direct deploy.
   `rub3 deploy --factory <SCRATCH_FACTORY> --chain base_sepolia` drives the same
   script and prints the plan first; `--dry-run` runs nothing at all.
   Then confirm what was stamped with the three `cast call` reads in that same
   section: `feeBps()`, `treasury()`, `isDeployed(address)`.
3. **A generation-2 factory superseding it.** The same script with
   `PREVIOUS_FACTORY=<the generation-1 factory>`, then
   `cast call <GEN2> "isCanonicalPredecessor(address)(bool)" <LICENCE_FROM_GEN1>`,
   which must answer `true`. This is the only way to observe the
   `previousFactory` walk against a chain, and the pointer is immutable, so a
   factory deployed without it is the failure being rehearsed against.
4. **A scratch code registry.** `forge create` it with a throwaway owner as
   described in [contracts.md](contracts/contracts.md#deploying-one), then
   `publish` a real fingerprint out of `contracts/canonical-bytecode.json` and
   `deprecate` it, both from
   [contracts.md](contracts/contracts.md#publishing-a-release), and read the
   result back with `latestOffsetTables` and `record` from
   [contracts.md](contracts/contracts.md#reading-it-and-the-offsets-bootstrap).
   Throw the owner key away afterwards: on a scratch registry it protects
   nothing, and on a canonical one it is the one thing that must not be lost.
5. **The pre-mainnet treasury proof.** This one has a fixed shape and
   [contracts.md](contracts/contracts.md#treasury-custody-and-the-pre-mainnet-proof)
   owns it: a factory whose `TREASURY` is **a Safe**, a licence through it, one
   purchase on each rail, then `withdrawFees()` and `withdrawTokenFees(<USDC>)`
   with the Safe's balances read before and after. **An EOA does not substitute
   here.** The proposition under test is that a *contract* recipient receives on
   both rails, and an EOA receives unconditionally, so an EOA rehearsal would
   pass while proving nothing. Use a throwaway Safe: the section is explicit
   that a Sepolia Safe is a separate deployment and that an identical CREATE2
   address on mainnet can still be nothing but a counterfactual, so the Sepolia
   Safe's own identity is never what is being established. Note also that both
   sweeps revert `NoFeeConfigured` on a contract with no fee, so this proof needs
   the factory path; a direct deploy cannot perform it.
6. **A purchase on the real USDC rail**, covered next.

### Real USDC on Base Sepolia

Circle publishes the testnet addresses at
[developers.circle.com](https://developers.circle.com/stablecoins/usdc-contract-addresses).
On **Base Sepolia** it is `0x036CbD53842c5426634e7929541eC2318f3dCF7e`. Read off
that address on 2026-09-04: `symbol()` `USDC`, `decimals()` `6`, `name()` `USDC`,
`version()` `2`.

**It exposes the `bytes` overload the licence contracts require.** The check that
[contracts.md](contracts/contracts.md#which-payment-tokens-work) prescribes,
run against it:

```bash
RPC=https://sepolia.base.org
USDC=0x036CbD53842c5426634e7929541eC2318f3dCF7e

IMPL=$(cast call $USDC "implementation()(address)" --rpc-url $RPC)
# 0xd74cc5d436923b8ba2c179b4bCA2841D8A52C5B5

cast code $IMPL --rpc-url $RPC | grep -c 88b7ab63   # 1: the bytes overload
cast code $IMPL --rpc-url $RPC | grep -c ef55bec6   # 1: the (v, r, s) form too
```

`cast implementation` alone does **not** resolve this token: it reads the
EIP-1967 slot and Circle's proxy predates it, so it answers the zero address for
USDC on both Base and Base Sepolia. The full recipe, which tries the EIP-1967
slots, the getter and the pre-EIP-1967 slot before concluding anything, lives in
[contracts.md](contracts/contracts.md#which-payment-tokens-work).

The overload is also observable behaviourally, which settles it beyond a
selector scan. Called with `msg.sender == to` and an empty signature it reverts
`ECRecover: invalid signature length`, which is the `SignatureChecker` path the
EIP-1271 support rests on; called from any other sender it reverts
`FiatTokenV2: caller must be the payee`, which is the `receiveWithAuthorization`
rule itself. A selector the token does not carry reverts with no data at all.

**The EIP-712 domain a buyer signs under**, confirmed against the token's own
`DOMAIN_SEPARATOR()` rather than assumed:

| Field | Value |
|---|---|
| `name` | `USDC` |
| `version` | `2` |
| `chainId` | `84532` |
| `verifyingContract` | `0x036CbD53842c5426634e7929541eC2318f3dCF7e` |

```bash
cast call $USDC "DOMAIN_SEPARATOR()(bytes32)" --rpc-url $RPC
# 0x71f17a3b2ff373b803d70a5a07c046c1a2bc8e89c09ef722fcb047abe94c9818
cast keccak $(cast abi-encode "f(bytes32,bytes32,bytes32,uint256,address)" \
  $(cast keccak "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)") \
  $(cast keccak "USDC") $(cast keccak "2") 84532 $USDC)
# the same digest
```

That is the `domain` block to put in the `auth.json` of
[contracts.md](contracts/contracts.md#buying-with-an-authorization), whose worked
example is written for chain `8453`. The rest of that recipe is unchanged: the
nonce still comes from `purchaseAuthorizationNonce`, `value` is still
`priceAmount()` exactly, and anyone may submit.

Driving the same purchase through the wrapper rather than by hand means packing a
headless binary against the scratch licence and setting the spend ceiling:
`RUB3_AGENT_MAX_TOKEN_AMOUNT` unset leaves the stablecoin rail unavailable rather
than unlimited, so a wrapper that "chose ETH" with the variable missing has
tested nothing about USDC.

```bash
cargo run -p rub3-cli -- pack --binary <app> --app-id com.example.app \
  --contract <SCRATCH_LICENCE> --chain base_sepolia --tier cooldown --headless \
  --factory <SCRATCH_FACTORY> --rpc-url https://sepolia.base.org \
  --output ./dist/scratch --dry-run
```

`--dry-run` prints the resolved plan and builds nothing; drop it to build. The
`--factory` line is mandatory here and says on screen that the result is not a
canonical deploy, which is the whole of tier 2 in one line of output.

### The code registry has no override

**There is no build-time flag, no pack-time flag and no environment variable that
points a wrapper at a code registry.** The address comes from
`attest::REGISTRIES` in `crates/rub3-wrapper/src/attest.rs`, a `pub static`
carrying one entry per chain, and `attest::registry_for` is its only reader. The
purchase path reaches it as `verify_before_purchase` to `decide` to
`registry_for(chain_id)`, with no parameter a caller could supply instead.

- **No environment override.** The wrapper reads no variable naming a registry.
  The `RUB3_*` set it does read is the signer, spend-policy, SDK-channel and
  directory variables, documented in [README.md](README.md) under "Signer
  sources" and "Spend policy".
- **No pack-time override.** `crates/rub3-wrapper/build.rs` lists the `RUB3_PACK_*`
  variables it accepts and none of them is a registry, and `PackArgs` in
  `crates/rub3-cli/src/pack.rs` has `--factory` and no counterpart for the
  registry. `rub3-cli` does parse `code_registry` out of the manifest into
  `deployments::ChainRecord`, and passes it nowhere.
- **The table cannot be edited alone either.**
  `attest::tests::registry_table_mirrors_the_deployment_manifest` fails when
  `REGISTRIES` names an address `contracts/deployments.json` does not publish, in
  every tier-2-and-up bundle.

**So the only way to exercise the registry lookup on Base Sepolia is to build a
wrapper from a modified `REGISTRIES` and a modified `deployments.json` in a
working tree that is never committed.** That is a deliberate hole in tier 2, and
the consequence is worth stating plainly: tier 2 can deploy a scratch registry
and drive `publish`, `deprecate`, `latestOffsetTables` and `record` with `cast`,
and it cannot observe a *wrapper* deciding a purchase on that registry's answer.
That decision is covered by `tests/code_registry_e2e.rs`, which calls
`attest::consult_registry` with an explicit address on anvil, bypassing
`REGISTRIES` the same way. What remains untested anywhere is the resolution step
answering with an address: `registry_for` runs on every purchase today and
always returns `None`, so the wiring from a populated table into the gate's
decision first executes in production on the day chain 84532 is populated.

**A follow-up worth considering, and not built here.** A pack-time
`--code-registry <ADDRESS>` would close that gap, and it is exactly the kind of
knob this design refuses elsewhere. For it: the resolution step would get real
coverage, and the flag would be no more trusted than `--factory` and
`packed::CONTRACT` already are, since all three are build-time constants trusted
because the user chose to run the binary, which is where `REGISTRIES`' own doc
comment says the recursion stops. Against it: the registry is the *second*
authority a wrapper consults when its first one missed, so a flag that redirects
it turns "code neither table knows" into "code an address supplied at pack time
vouched for", and the failure is silent by construction because the outcome is a
successful purchase rather than a refusal. `--factory` is not the same risk: the
wrapper never reads the factory at runtime at all. If it is ever built, it should
print its non-canonicity on `--version` the way `--factory` prints it on the pack
summary, and it should be refused outright unless the binary also declares itself
non-canonical.

### Tier 3: the one recorded canonical Sepolia set

The single `Rub3Factory` and `Rub3CodeRegistry` that
`contracts/deployments.json` will name for chain 84532, that
`attest::REGISTRIES` will mirror, that `rub3 pack` will bake in with no
`--factory` flag, and that the one-shot quickstart of `implementation.md` §3.3
will target. **This tier is never used for an experiment**, and this plan neither
performs nor schedules it: it waits on the treasury decision and on the standing
sequencing in which the factory launches together with the discovery registry,
`Rub3Registry` (`implementation.md` §3.2), which is not the code registry this
tier deploys.

Mainnet discipline applies here, not testnet discipline, because two of the three
things it does are permanent:

- **A Safe as treasury.** Immutable on the factory and on every licence that
  factory will ever deploy, with no setter and no migration that reaches a
  deployed contract, so the pre-mainnet proof in
  [contracts.md](contracts/contracts.md#treasury-custody-and-the-pre-mainnet-proof)
  runs first.
- **A custodied owner key for the code registry.** The key is the whole of the
  registry's authority, rotation is supported and renouncing is refused, so the
  failure to guard against is loss rather than handover;
  [contracts.md](contracts/contracts.md#deploying-one) states the rule and why
  it argues for a custodied key on a canonical deploy.
- **`attest::CANONICAL` becomes accumulate-only.** The rule is stated in that
  table's own doc comment and enforced by
  `attest::tests::nothing_is_deployed_so_the_accumulate_only_rule_is_not_live_yet`,
  which reads `contracts/deployments.json` and asserts every `factory` and every
  `code_registry` is still `null`. Quoting the trigger in the doc comment's own
  words: the two records are separate deploys with separate lifecycles, so
  **the rule goes live for a chain as soon as either of them stops being `null`,
  and it goes live for whichever contracts that deploy actually put on chain**.
  From that moment a row for a deployed contract is only ever added, never
  overwritten and never dropped. Until then the table is corrected in place, and
  the test is what turns the day it changes into a failing build rather than a
  stale comment.

Publishing either address is therefore a one-way step, and the test above fails
in every tier-2-and-up matrix job the moment it is taken, which is the signal to
update `CANONICAL`'s doc comment and the test itself rather than to work around
them.

### Faucets, RPC, keys, and CI

**Tier 2 does not belong in CI.** `.github/workflows/ci.yml` uses no repository
secrets at all: every job is hermetic, and each of the four on-chain steps runs
its own anvil. Putting tier 2 in CI would mean a funded private key in repository
secrets, and it would make every pull request depend on a public testnet's
uptime, an endpoint's rate limit and a faucet balance nobody is watching. The
value tier 2 delivers is a rehearsal before an irreversible act, which is a
launch gate rather than a per-commit gate, and a flaky per-commit gate is the
thing this repository's `assert-e2e-ran.sh` discipline exists to avoid.

What tier 1 needs: a Rust toolchain, Foundry, and nothing else. What tier 2 adds:

| Need | Where it comes from | Note |
|---|---|---|
| Base Sepolia RPC | `https://sepolia.base.org`, or a provider key in `BASE_SEPOLIA_RPC_URL` | the public endpoint answers reads; a broadcast run wants a provider |
| Testnet ETH | [Base network faucets](https://docs.base.org/get-started/get-funds) | for the deployer and for an ETH-rail purchase |
| Testnet USDC | [Circle faucet](https://faucet.circle.com), select Base Sepolia | for the stablecoin rail |
| A deployer key | `cast wallet new`, used for this and discarded | never a key that holds anything |
| A throwaway Safe | deployed on Base Sepolia, owners rotatable | only for the treasury proof; an EOA is enough for every other step |
| `BASESCAN_API_KEY` | [Basescan](https://basescan.org/register) | optional: `--verify` and `cast interface` only |

**Tier 3 needs a decision, not a faucet.** Its prerequisites are the treasury
custody choice and the key-custody arrangement for the registry owner, both of
which sit above this document.
