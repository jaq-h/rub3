//! End-to-end test for headless activation against a live EVM node.
//!
//! The agent path, exactly as an orchestrator would run it: a freshly generated
//! private key, funded from anvil, drives
//! `purchase() → activate() → sign → verify → persist` with **no webview
//! anywhere in the process** - this test binary links neither `wry` nor `tao`,
//! because `--features tier-3,headless` excludes them.
//!
//! Requires the Foundry toolchain (`anvil`, `forge`, `cast`) on PATH.
//! Ignored by default - run with:
//!
//!     cargo test -p rub3-wrapper --no-default-features --features tier-3,headless \
//!         -- --ignored headless
//!
//! The tests share one anvil port and set process-global env vars, so they
//! serialise themselves through [`serial_guard`]. No `--test-threads=1` needed.
//!
//! Each test prints `SKIP: ...` and returns Ok when the toolchain is missing,
//! so it is safe to run in any environment.
//!
//! Modelled on `session_onchain_e2e.rs`; deliberately a separate file with its
//! own anvil port so the two can run side by side.

#![cfg(all(feature = "cooldown", feature = "headless"))]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use rub3_wrapper::activation::{
    ensure_headless, HeadlessContext, HeadlessError, HeadlessOutcome, PaymentRail,
    ENV_MAX_TOKEN_AMOUNT, EXIT_PRICE_ABOVE_POLICY,
};
use rub3_wrapper::rpc;
use rub3_wrapper::signer::{resolve_signer, Signer, ENV_AGENT_KEY};
use rub3_wrapper::{session, session_store};

// Anvil's built-in account #0 - deterministic, documented, holds nothing real.
// Used only as the deployer / faucet; the agent under test uses a fresh key.
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEPLOYER_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// Distinct from `session_onchain_e2e.rs` (8547) so both suites can run at once.
const PORT: u16 = 8549;

/// Every test in this file binds `PORT` and sets `RUB3_AGENT_KEY` +
/// `RUB3_SESSION_DIR`, both of which are process-global. Held for the whole
/// test body so the anvil instance, the key and the session dir belong to one
/// test at a time, rather than depending on the caller passing
/// `--test-threads=1`.
static SERIAL: Mutex<()> = Mutex::new(());

/// Takes the whole-file lock. A panicking test poisons it; that test has
/// already failed, and the next one starts from a fresh anvil and a fresh key,
/// so the poison is cleared rather than cascading.
fn serial_guard() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

const APP_ID: &str = "com.rub3.headless-test";
const CHAIN_ID: u64 = 31337; // anvil's default
const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Price the fixture contract charges, in wei (0.01 ETH). Non-zero on purpose:
/// a free mint would not exercise the value-transfer or balance-check paths.
const PRICE_WEI: &str = "10000000000000000";

/// What the faucet sends the agent when the test wants it solvent.
const FUNDING_ETH: &str = "1ether";

/// Price the fixture charges on the stablecoin rail: 5 USDC at 6 decimals.
const USDC_PRICE: &str = "5000000";

/// What the mock USDC faucet mints an agent: 1000 USDC.
const USDC_FUNDING: &str = "1000000000";

const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";

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
    crate_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("contracts")
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

/// Deploys `Rub3Access` selling for ETH only.
fn deploy_access(price_wei: &str, supply_cap: &str, cooldown_blocks: &str) -> String {
    deploy_access_with_rail(price_wei, ZERO_ADDR, "0", supply_cap, cooldown_blocks)
}

/// Deploys `Rub3Access` with both rails configured.
///
/// Constructor args (10): name, symbol, identityModel, tbaImplementation,
/// wrapperHashes, sale, supplyCap, cooldownBlocks, predecessor, owner.
///
/// `sale` is the `SaleTerms` tuple of contracts §2.2 - `(price, priceToken,
/// priceAmount)` - which `forge create` takes as a parenthesised tuple. A zero
/// `priceToken` advertises no stablecoin rail, which is what the wrapper reads
/// to decide which currency to pay in.
///
/// `wrapperHashes` is the append-only hash set (contracts §2.4), seeded with a
/// single stand-in release hash - the zero hash is rejected on-chain because it
/// is the `Unknown` sentinel. `predecessor` is zero: no migration source.
fn deploy_access_with_rail(
    price_wei: &str,
    price_token: &str,
    price_amount: &str,
    supply_cap: &str,
    cooldown_blocks: &str,
) -> String {
    let wrapper_hashes = "[0x1111111111111111111111111111111111111111111111111111111111111111]";
    let sale = format!("({price_wei},{price_token},{price_amount})");
    forge_create(
        "src/Rub3Access.sol:Rub3Access",
        &[
            "Rub3 Headless Test",
            "RUB3H",
            "0",
            ZERO_ADDR,
            wrapper_hashes,
            &sale,
            supply_cap,
            cooldown_blocks,
            ZERO_ADDR,
            DEPLOYER_ADDR,
        ],
    )
}

/// Deploys the EIP-3009 stand-in for USDC from the Foundry test tree.
///
/// The forge suite justifies the mock over a fork or a deployed token in
/// `contracts/test/mocks/MockEIP3009Token.sol`; the same reasoning applies
/// here, with more force - anvil starts empty, so there is no USDC to buy with
/// unless the test deploys one.
fn deploy_mock_usdc() -> String {
    forge_create("test/mocks/MockEIP3009Token.sol:MockEIP3009Token", &[])
}

/// Deploys an EIP-3009 token that passes the licence contract's constructor
/// probe and holds balances, but exposes no `DOMAIN_SEPARATOR()`.
///
/// The one shape of payment token an authorization cannot be built for
/// off-chain, and therefore the fixture for "a contract-level failure on a
/// token-side read selects ETH rather than ending the run".
fn deploy_mock_without_domain_separator() -> String {
    forge_create(
        "test/mocks/MockEIP3009Token.sol:NoDomainSeparatorEIP3009Token",
        &[],
    )
}

fn forge_create(target: &str, constructor_args: &[&str]) -> String {
    let url = rpc_url();
    let mut args = vec![
        "create",
        target,
        "--broadcast",
        "--private-key",
        DEPLOYER_KEY,
        "--rpc-url",
        &url,
    ];
    if !constructor_args.is_empty() {
        args.push("--constructor-args");
        args.extend_from_slice(constructor_args);
    }

    let output = Command::new("forge")
        .current_dir(contracts_dir())
        .args(&args)
        .output()
        .expect("failed to run forge create");

    if !output.status.success() {
        panic!(
            "forge create {target} failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
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

/// Mints mock USDC to `to` through the token's test faucet.
fn mint_usdc(token: &str, to: Address, amount: &str) {
    let to_hex = format!("0x{}", hex::encode(to.as_slice()));
    let output = Command::new("cast")
        .args([
            "send",
            token,
            "mint(address,uint256)",
            &to_hex,
            amount,
            "--private-key",
            DEPLOYER_KEY,
            "--rpc-url",
            &rpc_url(),
        ])
        .output()
        .expect("failed to run cast send mint");
    assert!(
        output.status.success(),
        "minting USDC to {to_hex} failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Reads a native ETH balance through `cast`, for the same reason as
/// [`usdc_balance`]: the assertions should not depend on the code under test.
fn eth_balance(who: Address) -> u128 {
    let who_hex = format!("0x{}", hex::encode(who.as_slice()));
    let output = Command::new("cast")
        .args(["balance", &who_hex, "--rpc-url", &rpc_url()])
        .output()
        .expect("failed to run cast balance");
    assert!(output.status.success(), "cast balance failed");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("cast balance did not return a number")
}

/// Reads an ERC-20 balance through `cast`, independently of the wrapper's own
/// RPC helpers - so the assertions do not trust the code under test.
fn usdc_balance(token: &str, who: Address) -> u128 {
    let who_hex = format!("0x{}", hex::encode(who.as_slice()));
    let output = Command::new("cast")
        .args([
            "call",
            token,
            "balanceOf(address)(uint256)",
            &who_hex,
            "--rpc-url",
            &rpc_url(),
        ])
        .output()
        .expect("failed to run cast call balanceOf");
    assert!(output.status.success(), "balanceOf failed");
    let raw = String::from_utf8_lossy(&output.stdout);
    raw.split_whitespace()
        .next()
        .expect("empty balanceOf output")
        .parse()
        .expect("balanceOf did not return a number")
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
        .args([
            "rpc",
            "anvil_mine",
            &format!("0x{n:x}"),
            "--rpc-url",
            &rpc_url(),
        ])
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
/// A fresh key per test is the point of the exercise - it proves the flow needs
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
        // The spend ceiling is process-global like the key. Cleared here rather
        // than only on drop, so a test that means to run without one cannot
        // inherit a previous test's.
        std::env::remove_var(ENV_MAX_TOKEN_AMOUNT);

        // Resolve through the production path so the test exercises the real
        // env-var source, not a test-only constructor.
        let signer = resolve_signer().expect("resolve_signer from RUB3_AGENT_KEY");
        Self {
            signer,
            _session_dir: session_dir,
        }
    }

    fn address(&self) -> Address {
        self.signer.address()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        std::env::remove_var(ENV_AGENT_KEY);
        std::env::remove_var("RUB3_SESSION_DIR");
        std::env::remove_var(ENV_MAX_TOKEN_AMOUNT);
    }
}

/// Sets the operator's stablecoin spend ceiling for the current test.
///
/// Required before the stablecoin rail is usable at all: there is no default,
/// because the ceiling is denominated in the payment token's own smallest unit
/// and decimals differ between tokens. Cleared by [`Agent`]'s drop.
fn set_spend_ceiling(amount: &str) {
    std::env::set_var(ENV_MAX_TOKEN_AMOUNT, amount);
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
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    // Cooldown 15 blocks = the contract's enforced floor (MIN_COOLDOWN_BLOCKS).
    let contract = deploy_access(PRICE_WEI, "0", "15");
    let contract_addr: Address = contract
        .parse()
        .expect("forge returned a malformed address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    // Pre-conditions: the agent owns nothing and nothing has been minted.
    assert!(
        rpc::tokens_of_owner(&rpc_url(), contract_addr, agent.address())
            .unwrap()
            .is_empty(),
        "a fresh key must start with no tokens",
    );
    assert_eq!(rpc::next_token_id(&rpc_url(), contract_addr).unwrap(), 0);

    // ── The whole flow, one call ─────────────────────────────────────────────
    let (session, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("headless activation should succeed");

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { token_id, paid } => {
            assert_eq!(*token_id, 0, "first mint should be token id 0");
            assert_eq!(
                paid,
                &PaymentRail::Eth {
                    price_wei: PRICE_WEI.to_string()
                },
                "a contract advertising no stablecoin rail must be paid in ETH",
            );
        }
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    // ── The token really was bought ──────────────────────────────────────────
    assert_eq!(
        rpc::tokens_of_owner(&rpc_url(), contract_addr, agent.address()).unwrap(),
        vec![0],
        "the agent should now hold token 0",
    );
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address()
    );
    assert_eq!(rpc::next_token_id(&rpc_url(), contract_addr).unwrap(), 1);

    // ── The session is well-formed and bound to real chain state ─────────────
    assert_eq!(session.app_id, APP_ID);
    assert_eq!(session.token_id, 0);
    assert_eq!(
        session.identity, "access",
        "fixture deploys the access model"
    );
    assert!(session.tba.is_none(), "access model has no TBA");
    assert!(
        session
            .wallet
            .eq_ignore_ascii_case(&format!("0x{}", hex::encode(agent.address()))),
        "session wallet should be the signer address",
    );
    assert_eq!(
        session.user_id, session.wallet,
        "access model: user_id is the wallet"
    );
    assert_eq!(
        session.session_id,
        Some(1),
        "first activation gets session id 1"
    );
    assert!(session.activation_tx.is_some());
    assert!(session.activation_block.is_some());

    // Locally verifiable - signed by the agent's own key, no wallet involved.
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

    assert_eq!(
        outcome,
        HeadlessOutcome::Reused,
        "relaunch must hit the fast path"
    );
    assert_eq!(
        reused.nonce, session.nonce,
        "the same session should come back"
    );
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
    let _serial = serial_guard();
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
    let _serial = serial_guard();
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
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

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
/// a scheduler knows exactly how long to back off - and must succeed once the
/// window has passed.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_cooldown_active_then_ready_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access("0", "0", "15");
    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    // First run: buys and activates, starting the cooldown.
    let (first, _) =
        ensure_headless(agent.signer.as_ref(), &ctx(&contract, None)).expect("first activation");

    // Wipe the cached session so the flow is forced back on-chain; the token
    // stays owned, so this is the "session expired inside cooldown" case.
    std::fs::remove_file(session_store::session_path(APP_ID, first.token_id).unwrap())
        .expect("remove cached session");

    let err = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect_err("cooldown should block a second activation");

    let remaining = match &err {
        HeadlessError::CooldownActive {
            token_id,
            blocks_remaining,
        } => {
            assert_eq!(*token_id, first.token_id);
            assert!(
                *blocks_remaining > 0,
                "cooldown must report blocks remaining"
            );
            *blocks_remaining
        }
        other => panic!("expected CooldownActive, got {other:?}"),
    };
    assert_eq!(err.exit_code(), 13);
    assert!(err
        .machine_detail()
        .unwrap()
        .contains(&format!("blocks_remaining={remaining}")));

    // Let the window elapse; the same call should now go through.
    mine(remaining as u32 + 1);
    let (second, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("activation should succeed once the cooldown elapses");

    assert_eq!(
        outcome,
        HeadlessOutcome::Activated,
        "token already held - no purchase"
    );
    assert_eq!(second.token_id, first.token_id);
    assert_eq!(
        second.session_id,
        Some(2),
        "a second activate() bumps the session id"
    );
    assert_ne!(
        second.nonce, first.nonce,
        "a re-activation mints a fresh session"
    );
    session::verify_local(&second).expect("re-activated session must verify");
}

/// `--token-id N` must never fall back to buying a different token: an agent
/// asked for one specific license, and quietly spending money on another is
/// worse than failing.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_explicit_token_not_owned_e2e() {
    let _serial = serial_guard();
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

/// A cached session is not a substitute for the license that was asked for.
/// After a plain run has cached a session for token 0, `--token-id 7` must
/// still reach the ownership check and fail, not launch on token 0's session.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_explicit_token_id_does_not_reuse_another_tokens_session_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access("0", "0", "15");
    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    // Run 1: no explicit token, so the flow buys token 0 and caches a session.
    let (cached, _) =
        ensure_headless(agent.signer.as_ref(), &ctx(&contract, None)).expect("first activation");
    assert_eq!(cached.token_id, 0);

    // Run 2: a different token is requested. The cached session for token 0
    // must be a miss, and the run must end on the ownership check.
    let err = ensure_headless(agent.signer.as_ref(), &ctx(&contract, Some(7)))
        .expect_err("a session for token 0 must not satisfy a request for token 7");

    match &err {
        HeadlessError::TokenNotOwned { token_id, .. } => assert_eq!(*token_id, 7),
        other => panic!("expected TokenNotOwned, got {other:?}"),
    }
    assert_eq!(err.exit_code(), 20);

    // And the token the caller did ask for by name still reuses its own cache.
    let (reused, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, Some(0)))
        .expect("token 0 is held and cached");
    assert_eq!(outcome, HeadlessOutcome::Reused);
    assert_eq!(reused.nonce, cached.nonce);
}

/// A wrapper packed for Base and pointed at some other chain must refuse before
/// it signs anything, rather than broadcasting a valid transaction to the wrong
/// network.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_chain_id_mismatch_e2e() {
    let _serial = serial_guard();
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

// ── The stablecoin rail (§2.2) ────────────────────────────────────────────────

/// The §2.2 thesis end to end: a contract that advertises a USDC price is paid
/// in USDC, and the agent's ETH balance is spent on gas alone.
///
/// The agent is funded with ETH deliberately, because it broadcasts its own
/// transaction here and every transaction costs gas. What the test proves is
/// the *price* moved in USDC and not in ETH: the token balance falls by exactly
/// the listed amount, and the ETH spent is far below the ETH price the same
/// contract also lists.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_purchases_on_the_stablecoin_rail_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    let contract = deploy_access_with_rail(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    mint_usdc(&usdc, agent.address(), USDC_FUNDING);
    // Exactly the listed price: the ceiling is inclusive, and a run at the
    // ceiling is the boundary worth exercising on the happy path.
    set_spend_ceiling(USDC_PRICE);

    let eth_before = eth_balance(agent.address());

    let (session, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("headless activation on the stablecoin rail should succeed");

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { token_id, paid } => {
            assert_eq!(*token_id, 0);
            match paid {
                PaymentRail::Erc3009 { token, amount } => {
                    assert!(
                        token.eq_ignore_ascii_case(&usdc),
                        "should name the advertised token, got {token}",
                    );
                    assert_eq!(amount, USDC_PRICE, "should pay the advertised amount");
                }
                other => panic!("expected the stablecoin rail, got {other:?}"),
            }
        }
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    // The price really moved in USDC.
    let spent_usdc: u128 =
        USDC_FUNDING.parse::<u128>().unwrap() - usdc_balance(&usdc, agent.address());
    assert_eq!(
        spent_usdc,
        USDC_PRICE.parse::<u128>().unwrap(),
        "exactly the listed stablecoin price left the wallet",
    );
    assert_eq!(
        usdc_balance(&usdc, contract_addr),
        USDC_PRICE.parse::<u128>().unwrap(),
        "and arrived at the contract",
    );

    // And the ETH price did not: what left the wallet is gas for two
    // transactions, orders of magnitude below the 0.01 ETH listed price.
    let eth_spent = eth_before - eth_balance(agent.address());
    assert!(
        eth_spent < PRICE_WEI.parse::<u128>().unwrap(),
        "ETH spent ({eth_spent}) must be gas only, well under the ETH price",
    );

    // And it is a real licence: owned, activated, verifiable.
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address()
    );
    assert_eq!(session.token_id, 0);
    session::verify_local(&session).expect("locally signed session must verify");
    session::verify_onchain(&session, &rpc_url()).expect("session must verify on-chain");
}

/// The fallback, which is the other half of "prefers USDC when advertised".
///
/// Same contract, same advertised rail - but this agent holds no USDC, so the
/// wrapper pays in ETH rather than broadcasting a transaction that could only
/// revert.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_falls_back_to_eth_without_stablecoin_balance_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    let contract = deploy_access_with_rail(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    set_spend_ceiling(USDC_PRICE);
    // Deliberately no `mint_usdc`: the agent holds none of the listed token, so
    // the balance check is the only thing left that can select ETH.

    let (_, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("headless activation should fall back to ETH");

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { paid, .. } => assert_eq!(
            paid,
            &PaymentRail::Eth {
                price_wei: PRICE_WEI.to_string()
            },
            "an agent holding no USDC must pay in ETH",
        ),
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    assert_eq!(
        usdc_balance(&usdc, contract_addr),
        0,
        "nothing paid in USDC"
    );
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address()
    );
}

/// An unset ceiling makes the stablecoin rail unavailable, not unlimited.
///
/// The agent here can afford the listed USDC price several times over and the
/// contract advertises the rail, so nothing but the missing configuration
/// selects ETH. Proving it buys in ETH rather than failing is the standing
/// requirement that §2.2 breaks nothing: ETH is the fallback, not a deprecated
/// path.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_falls_back_to_eth_without_a_spend_ceiling_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    let contract = deploy_access_with_rail(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    mint_usdc(&usdc, agent.address(), USDC_FUNDING);
    // Deliberately no `set_spend_ceiling`.

    let (_, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("an unconfigured ceiling must fall back to ETH, not fail");

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { paid, .. } => assert_eq!(
            paid,
            &PaymentRail::Eth {
                price_wei: PRICE_WEI.to_string()
            },
            "no configured ceiling means no stablecoin rail",
        ),
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    assert_eq!(
        usdc_balance(&usdc, contract_addr),
        0,
        "no stablecoin may move without an operator-set ceiling",
    );
    assert_eq!(
        usdc_balance(&usdc, agent.address()),
        USDC_FUNDING.parse::<u128>().unwrap(),
        "and the agent's stablecoin balance is untouched",
    );
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address(),
        "the licence was still obtained, in ETH",
    );
}

/// A price above the configured ceiling is a refusal, not a fallback.
///
/// The distinction is the point: an orchestrator must be able to tell "this
/// costs more than my policy allows" from "the network failed", so this exits
/// with its own code, spends nothing on either rail, and mints nothing.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_refuses_a_price_above_the_spend_ceiling_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    let contract = deploy_access_with_rail(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    mint_usdc(&usdc, agent.address(), USDC_FUNDING);
    // One unit under the listed price: affordable, but outside policy.
    let ceiling = USDC_PRICE.parse::<u128>().unwrap() - 1;
    set_spend_ceiling(&ceiling.to_string());

    let err = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect_err("a price above the ceiling must refuse");

    assert_eq!(
        err.exit_code(),
        EXIT_PRICE_ABOVE_POLICY,
        "a policy refusal has its own exit code, got {err}",
    );
    assert!(
        matches!(err, HeadlessError::PriceAbovePolicy { .. }),
        "got {err:?}",
    );

    let detail = err
        .machine_detail()
        .expect("a policy refusal must be machine-readable");
    assert!(detail.contains(&format!("listed={USDC_PRICE}")), "{detail}");
    assert!(detail.contains(&format!("maximum={ceiling}")), "{detail}");
    assert!(
        detail
            .to_ascii_lowercase()
            .contains(&usdc.trim_start_matches("0x").to_ascii_lowercase()),
        "the refused token must be named: {detail}",
    );

    // Nothing was spent and nothing was minted: not in USDC, and not by
    // quietly taking the ETH rail instead.
    assert_eq!(
        usdc_balance(&usdc, agent.address()),
        USDC_FUNDING.parse::<u128>().unwrap(),
        "no stablecoin left the wallet",
    );
    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        0,
        "a refusal must mint nothing",
    );
    assert!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).is_err(),
        "token 0 must not exist",
    );
}

/// A payment token that answers the constructor probe but has no
/// `DOMAIN_SEPARATOR()` selects ETH rather than ending the run.
///
/// EIP-3009 mandates the authorization functions and `authorizationState`, not
/// that getter, so a token like this deploys onto a licence contract happily.
/// An agent that could have bought in ETH must not be stopped by it.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_falls_back_to_eth_when_the_token_has_no_domain_separator_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let token = deploy_mock_without_domain_separator();
    let contract = deploy_access_with_rail(PRICE_WEI, &token, USDC_PRICE, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    // Funded and within policy, so the only thing that can select ETH is the
    // token's missing getter.
    mint_usdc(&token, agent.address(), USDC_FUNDING);
    set_spend_ceiling(USDC_PRICE);

    let (session, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("a token without DOMAIN_SEPARATOR() must not end the activation");

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { paid, .. } => assert_eq!(
            paid,
            &PaymentRail::Eth {
                price_wei: PRICE_WEI.to_string()
            },
            "a token that cannot be signed for must select ETH",
        ),
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    assert_eq!(
        usdc_balance(&token, contract_addr),
        0,
        "nothing paid in a token no authorization could be built for",
    );
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address()
    );
    session::verify_onchain(&session, &rpc_url()).expect("session must verify on-chain");
}

/// The other half of the same rule: a *transport* failure on a token-side read
/// is never a fallback.
///
/// The contract advertises a rail and the agent is funded, so the run would buy
/// in USDC. Pointing it at a dead endpoint must stop it rather than let a
/// blinking node silently change the currency.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_transport_failure_on_a_token_read_is_a_hard_error_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    let contract = deploy_access_with_rail(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    mint_usdc(&usdc, agent.address(), USDC_FUNDING);
    set_spend_ceiling(USDC_PRICE);

    // A port nothing is listening on: every read fails at the socket, which is
    // the one class of failure that must never be read as an answer.
    let mut dead = ctx(&contract, None);
    dead.rpc_url = "http://127.0.0.1:1".to_string();

    let err = ensure_headless(agent.signer.as_ref(), &dead)
        .expect_err("an unreachable node must not be read as a rail decision");
    assert!(
        matches!(err, HeadlessError::Rpc(_)),
        "an unreachable node is a chain error, got {err:?}",
    );

    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        0,
        "and nothing was bought on either rail",
    );
}
