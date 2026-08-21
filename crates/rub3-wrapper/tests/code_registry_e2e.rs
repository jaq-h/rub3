//! End-to-end test for the code registry against a live EVM node.
//!
//! Every other test of the registry path drives a scripted chain, which proves
//! the decisions and proves nothing about the wire. This one deploys the real
//! `Rub3CodeRegistry` and a real `Rub3Access`, publishes a record through
//! `cast`, and reads it back through the wrapper's own decoder. Two things can
//! only be checked here:
//!
//!   * **the ABI mirror.** `rpc::IRub3CodeRegistry` restates the registry's
//!     `Release` struct field for field, and field order *is* the encoding. A
//!     mirror that has drifted decodes garbage or reverts, and no unit test in
//!     this crate can see that.
//!   * **the pinned fingerprint against a real deploy.** `consult_registry`
//!     believes an answer only after the registry's own runtime code hashes to
//!     the `attest::CANONICAL` row for it. That row is a number in a table until
//!     something compiles the contract, deploys it, and fetches the code back.
//!
//! Requires the Foundry toolchain (`anvil`, `forge`, `cast`) on PATH. Ignored by
//! default - run with:
//!
//!     cargo test -p rub3-wrapper --no-default-features --features tier-2 \
//!         -- --ignored code_registry
//!
//! The test prints `SKIP: ...` and returns when the toolchain is missing, so it
//! is safe to run in any environment. **A pass in 0.00s is a skip**, not a
//! green run.

#![cfg(feature = "onchain-read")]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rub3_wrapper::attest::{
    self, ChainReader, ImmutableRange, RecordStatus, RegistryVerdict, Role, RpcChain,
};

// Anvil's built-in account #0 (deterministic, documented, no real value).
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEPLOYER_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

// Its own port, so this suite and the three other anvil suites (8547, 8549 and
// webview::session_flow's 8551) can run at the same time.
const PORT: u16 = 8553;

const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";

// ── Harness ───────────────────────────────────────────────────────────────────

fn rpc_url() -> String {
    format!("http://127.0.0.1:{PORT}")
}

fn tool_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn contracts_dir() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("contracts")
}

struct AnvilGuard {
    child: Child,
}

impl Drop for AnvilGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// `AnvilGuard::drop` does kill + wait; clippy cannot see through the guard.
#[allow(clippy::zombie_processes)]
fn start_anvil() -> AnvilGuard {
    let child = Command::new("anvil")
        .args(["--port", &PORT.to_string(), "--silent"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn anvil");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let ready = Command::new("cast")
            .args(["block-number", "--rpc-url", &rpc_url()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ready {
            return AnvilGuard { child };
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("anvil did not become ready within 10s");
}

fn forge_create(target: &str, args: &[&str]) -> String {
    let mut cmd = Command::new("forge");
    cmd.current_dir(contracts_dir()).args([
        "create",
        target,
        "--broadcast",
        "--private-key",
        DEPLOYER_KEY,
        "--rpc-url",
        &rpc_url(),
        "--constructor-args",
    ]);
    for arg in args {
        cmd.arg(arg);
    }

    let output = cmd.output().expect("failed to run forge create");
    if !output.status.success() {
        panic!(
            "forge create {target} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Deployed to: ") {
            return rest.trim().to_string();
        }
    }
    panic!("could not find 'Deployed to:' in forge output:\n{stdout}");
}

fn cast_send(contract: &str, sig: &str, args: &[&str]) {
    let mut cmd = Command::new("cast");
    cmd.args([
        "send",
        contract,
        sig,
        "--private-key",
        DEPLOYER_KEY,
        "--rpc-url",
        &rpc_url(),
    ]);
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd.output().expect("failed to run cast send");
    if !output.status.success() {
        panic!(
            "cast send {sig} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A directly deployed `Rub3Access`, priced at zero on the ETH rail with no
/// stablecoin rail and no protocol fee. The constructor is the one
/// `session_onchain_e2e.rs` uses; see it for the tuple arguments.
fn deploy_access() -> String {
    forge_create(
        "src/Rub3Access.sol:Rub3Access",
        &[
            "Rub3 Registry Test",
            "RUB3",
            "(0,0x0000000000000000000000000000000000000000)",
            "[0x1111111111111111111111111111111111111111111111111111111111111111]",
            "(0,0x0000000000000000000000000000000000000000,0)",
            "(0,0x0000000000000000000000000000000000000000)",
            "0",
            // SessionTerms (§3.4): (cooldownBlocks, seatsPerToken,
            // sessionTtlSeconds). One seat, which is the tier-3 licence.
            "(15,1,86400)",
            ZERO_ADDR,
            DEPLOYER_ADDR,
        ],
    )
}

// ── Fixtures built out of the pinned table ────────────────────────────────────

/// The pinned entry for `contract`, so the test publishes what this build
/// already believes rather than a number of its own.
fn pinned(contract: &str) -> &'static attest::CanonicalContract {
    attest::CANONICAL
        .iter()
        .find(|entry| entry.contract == contract)
        .unwrap_or_else(|| panic!("attest::CANONICAL pins no {contract}"))
}

/// `cast`'s literal for a `(uint32,uint32)[]` of byte ranges.
fn ranges_arg(ranges: &[ImmutableRange]) -> String {
    let inner: Vec<String> = ranges
        .iter()
        .map(|r| format!("({},{})", r.start, r.length))
        .collect();
    format!("[{}]", inner.join(","))
}

fn addr(text: &str) -> alloy::primitives::Address {
    text.parse().expect("a deployed address parses")
}

/// A masked code hash no deploy produces, for the record fixtures that are
/// about a record's own fields rather than about any contract's code. `publish`
/// asks only that a hash is non-zero and not already published.
fn role_fixture_hash(index: u8) -> [u8; 32] {
    let mut hash = [0xf0; 32];
    hash[31] = index;
    hash
}

const PUBLISH_SIG: &str = "publish(bytes32,uint8,string,string,bytes32,string,(uint32,uint32)[])";

/// How many candidate offset tables the wrapper reads in one bootstrap. Mirrors
/// `attest::MAX_CANDIDATE_OFFSET_TABLES`, which is crate-private, and only the
/// bounding matters here: the exact number is asserted by the unit tests.
const CANDIDATE_LIMIT: usize = 16;

/// A table no other release shares: one 32-byte range, `index` words in. Shaped
/// the way `publish` requires, and deliberately not describing any real deploy -
/// this fixture is about how many tables a read returns, not about hashing code
/// under them.
fn distinct_table(index: usize) -> Vec<ImmutableRange> {
    vec![ImmutableRange {
        start: 64 + index * 64,
        length: 32,
    }]
}

// ── The test ──────────────────────────────────────────────────────────────────

#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn code_registry_answers_the_wrapper_over_a_real_chain_e2e() {
    for bin in ["anvil", "forge", "cast"] {
        if !tool_available(bin) {
            println!("SKIP: {bin} not found on PATH");
            return;
        }
    }
    let _anvil = start_anvil();

    let registry_addr = forge_create(
        "src/Rub3CodeRegistry.sol:Rub3CodeRegistry",
        &[DEPLOYER_ADDR],
    );
    let licence_addr = deploy_access();
    let registry = addr(&registry_addr);
    let licence = addr(&licence_addr);

    let chain = RpcChain {
        rpc_url: &rpc_url(),
    };
    let licence_code = chain.code(licence).expect("the licence's code is readable");
    assert!(
        !licence_code.is_empty(),
        "the fixture licence deployed no code"
    );

    // ── The pinned fingerprints, against real deploys ────────────────────────
    //
    // Both halves of the trust root, checked against bytecode a compiler
    // actually produced and a chain actually stored. Until something does this,
    // `attest::CANONICAL` is a table of numbers agreeing with another table of
    // numbers.
    let registry_entry = pinned("Rub3CodeRegistry");
    assert_eq!(
        attest::masked_code_hash(
            &chain
                .code(registry)
                .expect("the registry's code is readable"),
            registry_entry.immutable_ranges,
        )
        .as_deref(),
        Some(registry_entry.masked_sha256),
        "the deployed Rub3CodeRegistry does not hash to the row this build pins for it, so no \
         wrapper would ever believe a real registry"
    );

    let licence_entry = pinned("Rub3Access");
    assert_eq!(
        attest::masked_code_hash(&licence_code, licence_entry.immutable_ranges).as_deref(),
        Some(licence_entry.masked_sha256),
        "a live Rub3Access, immutables filled in by its constructor, does not hash to the pinned \
         row once those ranges are zeroed"
    );

    // ── The offsets bootstrap ────────────────────────────────────────────────
    //
    // Published under the licence's real immutable layout, then read back
    // through the wrapper's decoder. A drifted ABI mirror shows up here first.
    let masked = attest::masked_code_hash(&licence_code, licence_entry.immutable_ranges)
        .expect("the pinned ranges fit a live deploy");
    let commit = "0x2222222222222222222222222222222222222222222222222222222222222222";

    cast_send(
        &registry_addr,
        PUBLISH_SIG,
        &[
            &format!("0x{masked}"),
            "0", // Role.Licence
            "Rub3Access",
            "2026-09, a release newer than this binary",
            commit,
            "0.8.28+commit.7893614a",
            &ranges_arg(licence_entry.immutable_ranges),
        ],
    );

    let tables = chain
        .offset_tables(registry, CANDIDATE_LIMIT)
        .expect("the registry answers latestOffsetTables()");
    assert_eq!(tables.len(), 1, "one release, one distinct table");
    assert_eq!(
        tables[0], licence_entry.immutable_ranges,
        "the published ranges did not survive the round trip through the ABI mirror"
    );

    // ── The three-way verdict, over the wire ─────────────────────────────────

    match attest::consult_registry(&chain, registry, &licence_code) {
        RegistryVerdict::Known(record) => {
            assert_eq!(record.contract, "Rub3Access");
            assert_eq!(record.version, "2026-09, a release newer than this binary");
            assert_eq!(record.status, RecordStatus::Active);
            assert_eq!(record.role, Some(Role::Licence));
            assert_eq!(record.offsets, licence_entry.immutable_ranges);
            assert!(
                record.registered_at_block > 0,
                "the block is recorded by the contract, so it is never zero"
            );
        }
        other => panic!("the registry published this code and must vouch for it, got {other:?}"),
    }

    // Code the registry has never seen. The same call, a different answer, and
    // no record means no purchase.
    let unknown_code: Vec<u8> = (0..512u16).map(|i| (i % 251) as u8).collect();
    assert_eq!(
        attest::consult_registry(&chain, registry, &unknown_code),
        RegistryVerdict::Unknown,
        "code nobody published is unknown, not canonical"
    );

    // ── Deprecation advises; it does not invalidate ──────────────────────────

    cast_send(
        &registry_addr,
        "deprecate(bytes32,string)",
        &[&format!("0x{masked}"), "superseded by a later release"],
    );

    match attest::consult_registry(&chain, registry, &licence_code) {
        RegistryVerdict::Known(record) => {
            assert_eq!(
                record.status,
                RecordStatus::Deprecated,
                "the deprecation did not reach the wrapper"
            );
            assert_eq!(
                record.offsets, licence_entry.immutable_ranges,
                "a deprecated release keeps everything a comparator needs"
            );
        }
        other => panic!(
            "a deprecated release is still genuine rub3 code and must still be recognised, got \
             {other:?}. A registry that could stop a purchase would be a revocation surface."
        ),
    }

    // ── The enum numbering, over the wire ────────────────────────────────────
    //
    // `Role` and `Status` cross the ABI as raw `uint8`s, and the numbering is
    // the encoding: renumbering either side silently turns a factory into a
    // licence, which is a purchase target the gate exists to refuse. Names
    // cannot be that encoding, so every value is published through the real
    // contract and decoded back here.
    //
    // Read through `ChainReader::record` directly rather than through
    // `consult_registry`, because a role is a property of a record and not of
    // the code: this asks about one record per role without needing one contract
    // per role.
    for (index, (role, expected)) in [
        ("0", Role::Licence),
        ("1", Role::Factory),
        ("2", Role::Deployer),
        ("3", Role::CodeRegistry),
        ("4", Role::DiscoveryRegistry),
    ]
    .into_iter()
    .enumerate()
    {
        let hash = role_fixture_hash(index as u8);
        cast_send(
            &registry_addr,
            PUBLISH_SIG,
            &[
                &format!("0x{}", hex::encode(hash)),
                role,
                "Rub3Access",
                "a role fixture",
                commit,
                "0.8.28+commit.7893614a",
                &ranges_arg(licence_entry.immutable_ranges),
            ],
        );

        let published = chain
            .record(registry, hash)
            .expect("the registry answers record()")
            .expect("a record published one line ago is there");
        assert_eq!(
            published.role,
            Some(expected),
            "role {role} decoded as {:?}. The numbers are on the wire: append to \
             Rub3CodeRegistry.Role and to attest::Role together, and never renumber.",
            published.role
        );
        assert_eq!(
            published.status,
            RecordStatus::Active,
            "a freshly published record is Active, which is Status = 1 on the wire"
        );
    }

    // The rest of the Status encoding. `Unknown` is the zero value and is not
    // publishable - it *is* the mapping miss - so it is covered as the answer
    // to a hash nobody published, which is the only way a wrapper ever meets
    // it. `Deprecated` is covered by moving one of the fixtures.
    let never_published = role_fixture_hash(99);
    assert_eq!(
        chain
            .record(registry, never_published)
            .expect("the registry answers record() for an unknown hash"),
        None,
        "Status::Unknown is a mapping miss and must decode to no record at all, not to a record \
         with a status a wrapper might act on"
    );

    let deprecated_fixture = role_fixture_hash(0);
    cast_send(
        &registry_addr,
        "deprecate(bytes32,string)",
        &[
            &format!("0x{}", hex::encode(deprecated_fixture)),
            "superseded, for the status round trip",
        ],
    );
    assert_eq!(
        chain
            .record(registry, deprecated_fixture)
            .expect("the registry answers record()")
            .expect("deprecation never removes a record")
            .status,
        RecordStatus::Deprecated,
        "Deprecated is Status = 2 on the wire, and `rpc::code_registry_record` maps that number \
         by hand"
    );

    // ── An address that is not the registry is not an authority ──────────────
    //
    // The licence contract is canonical rub3 code, deployed from this
    // repository, and it is still not a version authority. Driven against real
    // bytecode rather than a fixture, because this is the check the whole
    // fallback rests on.
    match attest::consult_registry(&chain, licence, &licence_code) {
        RegistryVerdict::Unavailable(why) => assert!(
            why.contains("not a code registry"),
            "the reason must name what was wrong, got: {why}"
        ),
        other => {
            panic!("a licence contract must never be believed as the code registry, got {other:?}")
        }
    }

    // ── The bootstrap read is bounded and newest-first, against a real deploy ─
    //
    // The wrapper reads a bounded window of the newest layouts rather than the
    // whole published set: how many tables exist is the registry owner key's to
    // choose, the read sits on the path that spends money, and a registry is
    // consulted only about code newer than the binary asking, so the old end is
    // the end to give up. `latestOffsetTables` is a second ABI mirror, and a
    // drifted one decodes garbage: only a real deploy can say. Reachability and
    // latency only - neither the size nor the end of this read was ever able to
    // produce a wrong verdict.
    //
    // Last in the test, so everything above ran against the single table one
    // release publishes.
    let extra = 3usize;
    for index in 0..extra {
        cast_send(
            &registry_addr,
            PUBLISH_SIG,
            &[
                &format!("0x{}", hex::encode(role_fixture_hash(0x40 + index as u8))),
                "0", // Role.Licence
                "Rub3Access",
                "a distinctly shaped release",
                commit,
                "0.8.28+commit.7893614a",
                &ranges_arg(&distinct_table(index)),
            ],
        );
    }

    let bounded = chain
        .offset_tables(registry, 2)
        .expect("the registry answers a bounded window");
    assert_eq!(
        bounded.len(),
        2,
        "the registry holds more tables than this read asked for, and a window \
         returns the bound rather than the set"
    );
    assert_eq!(
        bounded[0],
        distinct_table(extra - 1),
        "the budget is spent on the newest layouts, because a registry is only \
         ever asked about code newer than the binary asking"
    );
    assert_eq!(
        bounded[1],
        distinct_table(extra - 2),
        "newest first, and the ranges inside a window must survive the ABI \
         mirror too"
    );

    let clamped = chain
        .offset_tables(registry, CANDIDATE_LIMIT)
        .expect("the registry answers a window larger than its set");
    assert_eq!(
        clamped.len(),
        1 + extra,
        "a window past the end is clamped, so a reader needs no count call first"
    );
    assert_eq!(
        clamped[extra], licence_entry.immutable_ranges,
        "the oldest table is last, so it is the first thing a bound gives up"
    );
}
