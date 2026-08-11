//! End-to-end test for headless activation against a live EVM node.
//!
//! The agent path, exactly as an orchestrator would run it: a freshly generated
//! private key, funded from anvil, drives
//! `purchase() → activate() → sign → verify → persist` with **no webview
//! anywhere in the process** — this test binary links neither `wry` nor `tao`,
//! because `--features tier-3,headless` excludes them.
//!
//! Requires the Foundry toolchain (`anvil`, `forge`, `cast`) on PATH.
//! Ignored by default — run with:
//!
//!     cargo test -p rub3-wrapper --no-default-features --features tier-3,headless \
//!         -- --ignored --test-threads=1 headless
//!
//! Each test prints `SKIP: ...` and returns Ok when the toolchain is missing,
//! so it is safe to run in any environment.
//!
//! Modelled on `session_onchain_e2e.rs`; deliberately a separate file with its
//! own anvil port so the two can run side by side.

#![cfg(all(feature = "cooldown", feature = "headless"))]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use rub3_wrapper::activation::{ensure_headless, HeadlessContext, HeadlessError, HeadlessOutcome};
use rub3_wrapper::rpc;
use rub3_wrapper::signer::{resolve_signer, Signer, ENV_AGENT_KEY};
use rub3_wrapper::{session, session_store};

// Anvil's built-in account #0 — deterministic, documented, holds nothing real.
// Used only as the deployer / faucet; the agent under test uses a fresh key.
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEPLOYER_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// Distinct from `session_onchain_e2e.rs` (8547) so both suites can run at once.
const PORT: u16 = 8549;

const APP_ID: &str = "com.rub3.headless-test";
const CHAIN_ID: u64 = 31337; // anvil's default
const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Price the fixture contract charges, in wei (0.01 ETH). Non-zero on purpose:
/// a free mint would not exercise the value-transfer or balance-check paths.
const PRICE_WEI: &str = "10000000000000000";

/// What the faucet sends the agent when the test wants it solvent.
const FUNDING_ETH: &str = "1ether";

// ── Tool availability ─────────────────────────────────────────────────────────

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

/// Returns `false` (and prints a SKIP line) when Foundry is not installed.
fn toolchain_ready() -> bool {
    for bin in ["anvil", "forge", "cast"] {
        if !tool_available(bin) {
            eprintln!("SKIP: {bin} not found on PATH");
            return false;
        }
    }
    true
}

fn contracts_dir() -> PathBuf {
    // tests/ → crates/rub3-wrapper → crates → workspace root → contracts
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir.parent().unwrap().parent().unwrap().join("contracts")
}

// ── Anvil lifecycle ───────────────────────────────────────────────────────────

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

// ── Subprocess helpers ────────────────────────────────────────────────────────

/// Deploys `Rub3Access` with the given price, supply cap, and cooldown.
///
/// Constructor args (9): name, symbol, identityModel, tbaImplementation,
/// wrapperHash, price, supplyCap, cooldownBlocks, owner.
fn deploy_access(price_wei: &str, supply_cap: &str, cooldown_blocks: &str) -> String {
    let zero_hash = "0x0000000000000000000000000000000000000000000000000000000000000000";
    let zero_addr = "0x0000000000000000000000000000000000000000";
    let output = Command::new("forge")
        .current_dir(contracts_dir())
        .args([
            "create",
            "src/Rub3Access.sol:Rub3Access",
            "--broadcast",
            "--private-key",
            DEPLOYER_KEY,
            "--rpc-url",
            &rpc_url(),
            "--constructor-args",
            "Rub3 Headless Test",
            "RUB3H",
            "0",
            zero_addr,
            zero_hash,
            price_wei,
            supply_cap,
            cooldown_blocks,
            DEPLOYER_ADDR,
        ])
        .output()
        .expect("failed to run forge create");

    if !output.status.success() {
        panic!(
            "forge create failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
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

/// Sends ETH from anvil's faucet account to `to`.
fn fund(to: Address, amount: &str) {
    let to_hex = format!("0x{}", hex::encode(to.as_slice()));
    let output = Command::new("cast")
        .args([
            "send",
            &to_hex,
            "--value",
            amount,
            "--private-key",
            DEPLOYER_KEY,
            "--rpc-url",
            &rpc_url(),
        ])
        .output()
        .expect("failed to run cast send");
    assert!(
        output.status.success(),
        "funding {to_hex} failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Mines `n` blocks so a cooldown window can elapse without waiting.
fn mine(n: u32) {
    let output = Command::new("cast")
        .args(["rpc", "anvil_mine", &format!("0x{n:x}"), "--rpc-url", &rpc_url()])
        .output()
        .expect("failed to run cast rpc anvil_mine");
    assert!(
        output.status.success(),
        "anvil_mine failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

// ── Test fixture ──────────────────────────────────────────────────────────────

/// One isolated agent run: a fresh key in `RUB3_AGENT_KEY` and a tmpdir for
/// `RUB3_SESSION_DIR`, both torn down on drop.
///
/// A fresh key per test is the point of the exercise — it proves the flow needs
/// nothing but a funded keypair, with no pre-existing token, session, or state.
struct Agent {
    signer: Box<dyn Signer>,
    _session_dir: tempfile::TempDir,
}

impl Agent {
    fn new() -> Self {
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let key = SigningKey::random(&mut OsRng);
        let key_hex = format!("0x{}", hex::encode(key.to_bytes()));

        let session_dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var(ENV_AGENT_KEY, &key_hex);
        std::env::set_var("RUB3_SESSION_DIR", session_dir.path());

        // Resolve through the production path so the test exercises the real
        // env-var source, not a test-only constructor.
        let signer = resolve_signer().expect("resolve_signer from RUB3_AGENT_KEY");
        Self { signer, _session_dir: session_dir }
    }

    fn address(&self) -> Address {
        self.signer.address()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        std::env::remove_var(ENV_AGENT_KEY);
        std::env::remove_var("RUB3_SESSION_DIR");
    }
}

fn ctx(contract: &str, token_id: Option<u64>) -> HeadlessContext {
    HeadlessContext {
        app_id: APP_ID.to_string(),
        contract: contract.to_string(),
        chain_id: CHAIN_ID,
        rpc_url: rpc_url(),
        session_ttl_secs: SESSION_TTL_SECS,
        token_id,
    }
}

// ── The happy path ────────────────────────────────────────────────────────────

/// Fresh key → funded → purchase → activate → session persisted → relaunch
/// hits the fast path. The whole §2.1 thesis in one test.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_purchase_activate_persist_e2e() {
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    // Cooldown 15 blocks = the contract's enforced floor (MIN_COOLDOWN_BLOCKS).
    let contract = deploy_access(PRICE_WEI, "0", "15");
    let contract_addr: Address = contract.parse().expect("forge returned a malformed address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    // Pre-conditions: the agent owns nothing and nothing has been minted.
    assert!(
        rpc::tokens_of_owner(&rpc_url(), contract_addr, agent.address()).unwrap().is_empty(),
        "a fresh key must start with no tokens",
    );
    assert_eq!(rpc::next_token_id(&rpc_url(), contract_addr).unwrap(), 0);

    // ── The whole flow, one call ─────────────────────────────────────────────
    let (session, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("headless activation should succeed");

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { token_id, price_wei } => {
            assert_eq!(*token_id, 0, "first mint should be token id 0");
            assert_eq!(price_wei, PRICE_WEI, "should have paid the advertised price");
        }
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    // ── The token really was bought ──────────────────────────────────────────
    assert_eq!(
        rpc::tokens_of_owner(&rpc_url(), contract_addr, agent.address()).unwrap(),
        vec![0],
        "the agent should now hold token 0",
    );
    assert_eq!(rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(), agent.address());
    assert_eq!(rpc::next_token_id(&rpc_url(), contract_addr).unwrap(), 1);

    // ── The session is well-formed and bound to real chain state ─────────────
    assert_eq!(session.app_id, APP_ID);
    assert_eq!(session.token_id, 0);
    assert_eq!(session.identity, "access", "fixture deploys the access model");
    assert!(session.tba.is_none(), "access model has no TBA");
    assert!(
        session.wallet.eq_ignore_ascii_case(&format!("0x{}", hex::encode(agent.address()))),
        "session wallet should be the signer address",
    );
    assert_eq!(session.user_id, session.wallet, "access model: user_id is the wallet");
    assert_eq!(session.session_id, Some(1), "first activation gets session id 1");
    assert!(session.activation_tx.is_some());
    assert!(session.activation_block.is_some());

    // Locally verifiable — signed by the agent's own key, no wallet involved.
    session::verify_local(&session).expect("locally signed session must verify");
    // And it matches the chain: right tx, right contract, right block.
    session::verify_onchain(&session, &rpc_url()).expect("session must verify on-chain");

    // ── Persisted where the fast path will look ──────────────────────────────
    let stored = session_store::load_session(APP_ID, 0).expect("session should be on disk");
    assert_eq!(stored.signature, session.signature);
    assert_eq!(stored.nonce, session.nonce);

    // ── Relaunch: no purchase, no tx, straight to the cached session ─────────
    let minted_before = rpc::next_token_id(&rpc_url(), contract_addr).unwrap();
    let (reused, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("relaunch should succeed from cache");

    assert_eq!(outcome, HeadlessOutcome::Reused, "relaunch must hit the fast path");
    assert_eq!(reused.nonce, session.nonce, "the same session should come back");
    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        minted_before,
        "the fast path must not mint anything",
    );
}

// ── Classified failures ───────────────────────────────────────────────────────

/// An unfunded agent cannot buy, and must say so with the exit code that tells
/// an orchestrator to top up the wallet rather than retry.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_insufficient_funds_e2e() {
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access(PRICE_WEI, "0", "15");
    let agent = Agent::new(); // deliberately not funded

    let err = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect_err("an empty wallet cannot purchase");

    match &err {
        HeadlessError::InsufficientFunds { .. } => {}
        other => panic!("expected InsufficientFunds, got {other:?}"),
    }
    assert_eq!(err.exit_code(), 11);
}

/// Supply cap reached and the agent holds nothing: no amount of retrying or
/// funding helps, so this gets its own terminal exit code.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_sold_out_e2e() {
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    // Cap of 1, and the deployer takes it.
    let contract = deploy_access("0", "1", "15");
    let output = Command::new("cast")
        .args([
            "send",
            &contract,
            "purchase(address)",
            DEPLOYER_ADDR,
            "--private-key",
            DEPLOYER_KEY,
            "--rpc-url",
            &rpc_url(),
        ])
        .output()
        .expect("cast send purchase");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    let err = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect_err("no supply left to buy");

    match &err {
        HeadlessError::SoldOut { supply_cap, minted } => {
            assert_eq!(*supply_cap, 1);
            assert_eq!(*minted, 1);
        }
        other => panic!("expected SoldOut, got {other:?}"),
    }
    assert_eq!(err.exit_code(), 12);
    assert!(err.machine_detail().unwrap().contains("supply_cap=1"));
}

/// Re-activating inside the cooldown window must report the remaining blocks so
/// a scheduler knows exactly how long to back off — and must succeed once the
/// window has passed.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_cooldown_active_then_ready_e2e() {
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access("0", "0", "15");
    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    // First run: buys and activates, starting the cooldown.
    let (first, _) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("first activation");

    // Wipe the cached session so the flow is forced back on-chain; the token
    // stays owned, so this is the "session expired inside cooldown" case.
    std::fs::remove_file(session_store::session_path(APP_ID, first.token_id).unwrap())
        .expect("remove cached session");

    let err = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect_err("cooldown should block a second activation");

    let remaining = match &err {
        HeadlessError::CooldownActive { token_id, blocks_remaining } => {
            assert_eq!(*token_id, first.token_id);
            assert!(*blocks_remaining > 0, "cooldown must report blocks remaining");
            *blocks_remaining
        }
        other => panic!("expected CooldownActive, got {other:?}"),
    };
    assert_eq!(err.exit_code(), 13);
    assert!(err.machine_detail().unwrap().contains(&format!("blocks_remaining={remaining}")));

    // Let the window elapse; the same call should now go through.
    mine(remaining as u32 + 1);
    let (second, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("activation should succeed once the cooldown elapses");

    assert_eq!(outcome, HeadlessOutcome::Activated, "token already held — no purchase");
    assert_eq!(second.token_id, first.token_id);
    assert_eq!(second.session_id, Some(2), "a second activate() bumps the session id");
    assert_ne!(second.nonce, first.nonce, "a re-activation mints a fresh session");
    session::verify_local(&second).expect("re-activated session must verify");
}

/// `--token-id N` must never fall back to buying a different token: an agent
/// asked for one specific license, and quietly spending money on another is
/// worse than failing.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_explicit_token_not_owned_e2e() {
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access("0", "0", "15");
    let contract_addr: Address = contract.parse().unwrap();
    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    let err = ensure_headless(agent.signer.as_ref(), &ctx(&contract, Some(7)))
        .expect_err("token 7 is not held");

    match &err {
        HeadlessError::TokenNotOwned { token_id, .. } => assert_eq!(*token_id, 7),
        other => panic!("expected TokenNotOwned, got {other:?}"),
    }
    assert_eq!(err.exit_code(), 20);
    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        0,
        "nothing may be minted when an explicit token is missing",
    );
}

/// A wrapper packed for Base and pointed at some other chain must refuse before
/// it signs anything, rather than broadcasting a valid transaction to the wrong
/// network.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_chain_id_mismatch_e2e() {
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access("0", "0", "15");
    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    let mut wrong = ctx(&contract, None);
    wrong.chain_id = 8453; // Base mainnet, but we are talking to anvil

    let err = ensure_headless(agent.signer.as_ref(), &wrong)
        .expect_err("chain id mismatch must be refused");

    match &err {
        HeadlessError::ChainIdMismatch { expected, actual } => {
            assert_eq!(*expected, 8453);
            assert_eq!(*actual, CHAIN_ID);
        }
        other => panic!("expected ChainIdMismatch, got {other:?}"),
    }
    assert_eq!(err.exit_code(), 19);
}
