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

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use rub3_wrapper::activation::{
    ensure_headless, HeadlessContext, HeadlessError, HeadlessOutcome, PaymentRail,
    ENV_MAX_TOKEN_AMOUNT, EXIT_PRICE_ABOVE_POLICY,
};
use rub3_wrapper::rpc;
use rub3_wrapper::signer::{resolve_signer, Signer, SignerError, ENV_AGENT_KEY};
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
/// Constructor args (10): name, symbol, identity, wrapperHashes, sale, fee,
/// supplyCap, cooldownBlocks, predecessor, owner.
///
/// Three are tuples, which `forge create` takes parenthesised. `identity` is
/// `(identityModel, tbaImplementation)`. `sale` is the `SaleTerms` of contracts
/// §2.2 - `(price, priceToken, priceAmount)` - where a zero `priceToken`
/// advertises no stablecoin rail, which is what the wrapper reads to decide
/// which currency to pay in. `fee` is the `FeeTerms` of §2.3 -
/// `(feeBps, treasury)` - and `(0, 0x0)` is a direct deploy carrying no
/// protocol fee, which is what every arm here except the factory ones uses.
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
    let sale = format!("({price_wei},{price_token},{price_amount})");
    forge_create(
        "src/Rub3Access.sol:Rub3Access",
        &[
            "Rub3 Headless Test",
            "RUB3H",
            NO_TBA_IDENTITY,
            WRAPPER_HASHES,
            &sale,
            NO_FEE,
            supply_cap,
            cooldown_blocks,
            ZERO_ADDR,
            DEPLOYER_ADDR,
        ],
    )
}

/// The `IdentityTerms` tuple for the access model: model 0, no TBA
/// implementation (the constructor forbids one for that model).
const NO_TBA_IDENTITY: &str = "(0,0x0000000000000000000000000000000000000000)";

/// The `FeeTerms` tuple a direct (non-factory) deploy carries: no fee, no
/// treasury. The constructor rejects one without the other.
const NO_FEE: &str = "(0,0x0000000000000000000000000000000000000000)";

/// The append-only wrapper hash set, seeded with one stand-in release hash.
const WRAPPER_HASHES: &str = "[0x1111111111111111111111111111111111111111111111111111111111111111]";

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

/// Deploys a spec-conformant EIP-3009 token that implements only the
/// `(uint8 v, bytes32 r, bytes32 s)` form of `receiveWithAuthorization`.
///
/// The licence contracts call the `bytes signature` overload, the FiatTokenV2_2
/// form that also admits EIP-1271 smart-wallet signatures, so a token like this
/// deploys onto a licence contract happily - the constructor probe only reads
/// `authorizationState` - and then reverts for every buyer. It is the fixture
/// for the wrapper's pre-flight, which is where that is caught.
fn deploy_mock_without_signature_overload() -> String {
    forge_create(
        "test/mocks/MockEIP3009Token.sol:NoSignatureOverloadEIP3009Token",
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

// ── The protocol fee (§2.3) ───────────────────────────────────────────────────

/// Protocol fee the factory fixtures charge, in basis points (2.50%).
///
/// Inside the range `Rub3Factory` enforces and deliberately not either end of
/// it: the rate is chosen per factory deploy, and a test naming 200 or 300
/// would read as if one of them were settled.
const FEE_BPS: u128 = 250;

/// Where the fee accrues in the factory fixtures. Any address will do; it only
/// has to be able to receive ETH, which a plain EOA can.
const TREASURY_ADDR: &str = "0x00000000000000000000000000000000000EA5E7";

/// Deploys a `Rub3Factory` at [`FEE_BPS`] paying [`TREASURY_ADDR`].
///
/// The third constructor argument is `previousFactory`, zero here because this
/// is a first-generation factory: its own deployments are then the only
/// canonical predecessors it accepts.
fn deploy_factory() -> String {
    forge_create(
        "src/Rub3Factory.sol:Rub3Factory",
        &[&FEE_BPS.to_string(), TREASURY_ADDR, ZERO_ADDR],
    )
}

/// Deploys a `Rub3Access` *through* the factory, which is what stamps the fee.
///
/// `deployAccess` returns the address, but a transaction's return value is not
/// in its receipt, so the address is read back off the factory's own
/// insertion-ordered `deploymentAt(0)`. That also exercises the enumeration the
/// registry will read.
///
/// The `Rub3LicenseParams` tuple mirrors the struct field for field, including
/// the two nested tuples (`identity`, `sale`). `owner` is passed as the zero
/// address, which the factory resolves to the caller.
fn factory_deploy_access(
    factory: &str,
    price_wei: &str,
    price_token: &str,
    price_amount: &str,
) -> String {
    let params = format!(
        "(\"Rub3 Headless Test\",\"RUB3H\",{NO_TBA_IDENTITY},{WRAPPER_HASHES},({price_wei},{price_token},{price_amount}),0,15,{ZERO_ADDR},{ZERO_ADDR})"
    );
    let output = Command::new("cast")
        .args([
            "send",
            factory,
            "deployAccess((string,string,(uint8,address),bytes32[],(uint256,address,uint256),uint256,uint256,address,address))",
            &params,
            "--private-key",
            DEPLOYER_KEY,
            "--rpc-url",
            &rpc_url(),
        ])
        .output()
        .expect("failed to run cast send deployAccess");
    assert!(
        output.status.success(),
        "deployAccess failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    cast_call(factory, "deploymentAt(uint256)(address)", &["0"])
}

/// Reads a `uint256` getter through `cast`, so the fee assertions do not go
/// through the code under test.
fn cast_call_uint(contract: &str, sig: &str, args: &[&str]) -> u128 {
    cast_call(contract, sig, args)
        .parse()
        .expect("getter did not return a number")
}

/// One `cast call`, returning the first whitespace-separated word of the output
/// - which for a single-value return is the value.
fn cast_call(contract: &str, sig: &str, args: &[&str]) -> String {
    let url = rpc_url();
    let mut argv = vec!["call", contract, sig];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--rpc-url", &url]);

    let output = Command::new("cast")
        .args(&argv)
        .output()
        .expect("failed to run cast call");
    assert!(
        output.status.success(),
        "cast call {sig} failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("empty cast call output")
        .to_string()
}

/// Sends a transaction from the deployer key, used to settle the two halves of
/// a split after the agent has bought.
fn cast_send(contract: &str, sig: &str, args: &[&str]) {
    let url = rpc_url();
    let mut argv = vec!["send", contract, sig];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--private-key", DEPLOYER_KEY, "--rpc-url", &url]);

    let output = Command::new("cast")
        .args(&argv)
        .output()
        .expect("failed to run cast send");
    assert!(
        output.status.success(),
        "cast send {sig} failed:\n{}",
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

/// A [`Signer`] that counts how many times it was asked to sign, and otherwise
/// delegates unchanged.
///
/// The spend ceiling's guarantee is that no authorization for a refused amount
/// is ever *produced*, not merely that the run exits non-zero. An exit code
/// cannot show that: it reads the same whether the refusal came before or after
/// a valid 900-second authorization was signed and handed to an RPC endpoint
/// that could broadcast it. Counting the signing calls is what distinguishes
/// the two.
struct CountingSigner<'a> {
    inner: &'a dyn Signer,
    calls: AtomicUsize,
}

impl<'a> CountingSigner<'a> {
    fn wrapping(inner: &'a dyn Signer) -> Self {
        Self {
            inner,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Signer for CountingSigner<'_> {
    fn address(&self) -> Address {
        self.inner.address()
    }

    fn sign_prehash(&self, hash: B256) -> Result<alloy::primitives::Signature, SignerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.sign_prehash(hash)
    }

    fn source(&self) -> &'static str {
        self.inner.source()
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

// ── The protocol fee, end to end (§2.3) ───────────────────────────────────────

/// A factory-deployed contract completing a real purchase on the **ETH rail**,
/// with the fee landing in the right two places.
///
/// The forge suite proves the arithmetic; this proves the whole path holds
/// against a real chain and a real agent: the wrapper, which knows nothing about
/// the fee, buys exactly as it does from a fee-free contract, and afterwards the
/// treasury and the developer between them hold the entire price with nothing
/// stranded in the contract.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_factory_deploy_splits_the_eth_payment_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let factory = deploy_factory();
    let contract = factory_deploy_access(&factory, PRICE_WEI, ZERO_ADDR, "0");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    // The terms really are stamped on the deployed contract, and the factory
    // really did record it.
    assert_eq!(cast_call_uint(&contract, "feeBps()(uint16)", &[]), FEE_BPS);
    assert_eq!(
        cast_call(&factory, "isDeployed(address)(bool)", &[&contract]),
        "true",
    );

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    let (session, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("headless activation against a factory deploy should succeed");
    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { token_id, paid } => {
            assert_eq!(*token_id, 0);
            assert!(
                matches!(paid, PaymentRail::Eth { price_wei } if *price_wei == PRICE_WEI),
                "expected the ETH rail at the listed price, got {paid:?}",
            );
        }
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    let price: u128 = PRICE_WEI.parse().unwrap();
    let expected_fee = price * FEE_BPS / 10_000;

    assert_eq!(
        cast_call_uint(&contract, "feesAccrued()(uint256)", &[]),
        expected_fee,
        "the protocol's share is accrued against the payment",
    );
    assert_eq!(
        eth_balance(contract_addr),
        price,
        "and nothing has left yet"
    );

    // Settle both halves and check they add up to the payment exactly.
    let treasury: Address = TREASURY_ADDR.parse().expect("malformed treasury address");
    let developer: Address = DEPLOYER_ADDR.parse().expect("malformed deployer address");
    let treasury_before = eth_balance(treasury);
    let developer_before = eth_balance(developer);

    cast_send(&contract, "withdrawFees()", &[]);
    cast_send(&contract, "withdraw(address)", &[DEPLOYER_ADDR]);

    assert_eq!(
        eth_balance(treasury) - treasury_before,
        expected_fee,
        "the fee reached the treasury",
    );
    // The developer pays gas out of the same account, so their share is checked
    // as "at least" - gas can only reduce it, never inflate it.
    assert!(
        eth_balance(developer) > developer_before,
        "the developer's share reached them",
    );
    assert_eq!(
        eth_balance(contract_addr),
        0,
        "nothing is stranded: the two shares are the whole payment",
    );

    // And it is a real licence, indistinguishable from a fee-free one.
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address()
    );
    session::verify_onchain(&session, &rpc_url()).expect("session must verify on-chain");
}

/// The same, on the **stablecoin rail**. The fee applies identically on both,
/// and this is the arm that proves it against a chain rather than in the EVM
/// test harness.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_factory_deploy_splits_the_stablecoin_payment_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    let factory = deploy_factory();
    let contract = factory_deploy_access(&factory, PRICE_WEI, &usdc, USDC_PRICE);
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    mint_usdc(&usdc, agent.address(), USDC_FUNDING);
    set_spend_ceiling(USDC_PRICE);

    let (_session, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("headless activation on the stablecoin rail should succeed");
    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { paid, .. } => match paid {
            PaymentRail::Erc3009 { token, amount } => {
                assert!(token.eq_ignore_ascii_case(&usdc));
                assert_eq!(amount, USDC_PRICE);
            }
            other => panic!("expected the stablecoin rail, got {other:?}"),
        },
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    let amount: u128 = USDC_PRICE.parse().unwrap();
    let expected_fee = amount * FEE_BPS / 10_000;

    assert_eq!(
        cast_call_uint(&contract, "tokenFeesAccrued(address)(uint256)", &[&usdc]),
        expected_fee,
        "the protocol's share is accrued in the payment token",
    );
    assert_eq!(usdc_balance(&usdc, contract_addr), amount);

    let treasury: Address = TREASURY_ADDR.parse().expect("malformed treasury address");
    let developer: Address = DEPLOYER_ADDR.parse().expect("malformed deployer address");

    cast_send(&contract, "withdrawTokenFees(address)", &[&usdc]);
    cast_send(
        &contract,
        "withdrawToken(address,address)",
        &[&usdc, DEPLOYER_ADDR],
    );

    assert_eq!(usdc_balance(&usdc, treasury), expected_fee);
    assert_eq!(usdc_balance(&usdc, developer), amount - expected_fee);
    assert_eq!(
        usdc_balance(&usdc, contract_addr),
        0,
        "nothing is stranded on this rail either",
    );
    assert_eq!(
        usdc_balance(&usdc, treasury) + usdc_balance(&usdc, developer),
        amount,
        "the two shares are the whole payment",
    );
}

/// Direct deployment stays possible, is not penalised, and is simply
/// unrecorded. The counterweight to the two arms above: the wrapper's behaviour
/// is identical, no fee is taken, and the factory does not know the contract.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_direct_deploy_pays_no_fee_and_is_unrecorded_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let factory = deploy_factory();
    let contract = deploy_access(PRICE_WEI, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    assert_eq!(cast_call_uint(&contract, "feeBps()(uint16)", &[]), 0);
    assert_eq!(
        cast_call(&factory, "isDeployed(address)(bool)", &[&contract]),
        "false",
        "a direct deploy is not listable, which is the whole difference",
    );

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("a direct deploy must still sell licences");

    assert_eq!(cast_call_uint(&contract, "feesAccrued()(uint256)", &[]), 0);
    assert_eq!(
        eth_balance(contract_addr),
        PRICE_WEI.parse::<u128>().unwrap(),
        "the whole price is the developer's",
    );
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
    // One unit under the listed price: the rail is otherwise fully usable -
    // advertised, affordable, the token's domain is readable, and the purchase
    // pre-flights clean - so the ceiling is the only thing that can stop it.
    let ceiling = USDC_PRICE.parse::<u128>().unwrap() - 1;
    set_spend_ceiling(&ceiling.to_string());

    let eth_before = eth_balance(agent.address());

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
    // Not one wei moved either, so the refusal cannot have quietly become an
    // ETH purchase: no transaction was broadcast at all, not even for gas.
    assert_eq!(
        eth_balance(agent.address()),
        eth_before,
        "a refusal must not fall back to the ETH rail",
    );
}

/// The other side of the same rule, and the regression case: the ceiling may
/// only refuse a purchase the agent would otherwise have made.
///
/// Same contract and the same over-ceiling listing as above, but this agent
/// holds none of the payment token. It could never have spent a unit of it, so
/// ETH is not a fallback from a policy refusal - it is the path this agent was
/// always on, exactly as before §2.2. Refusing here would break a run that used
/// to succeed.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_buys_in_eth_when_it_holds_none_of_an_over_ceiling_token_e2e() {
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
    // Deliberately no `mint_usdc`, with the same ceiling the refusal test uses.
    let ceiling = USDC_PRICE.parse::<u128>().unwrap() - 1;
    set_spend_ceiling(&ceiling.to_string());

    let (_, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .unwrap_or_else(|err| {
            panic!(
                "an agent holding none of the payment token must stay on the ETH path, not be \
                 refused over money it could not have spent: {err} (exit {})",
                err.exit_code()
            )
        });

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { paid, .. } => assert_eq!(
            paid,
            &PaymentRail::Eth {
                price_wei: PRICE_WEI.to_string()
            },
            "an unaffordable rail is not a policy question",
        ),
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address(),
        "the licence was obtained, in ETH",
    );
    assert_eq!(
        usdc_balance(&usdc, contract_addr),
        0,
        "and nothing moved on the rail policy would have refused",
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

/// A refused price must leave no signed authorization behind anywhere.
///
/// The regression this pins: the ceiling once ran *after* the purchase was
/// signed and pre-flighted, so an above-ceiling run had already produced a
/// valid authorization for the full listed amount and shipped it to the RPC
/// endpoint as `eth_call` calldata. `purchaseWithAuthorization` is submittable
/// by anyone by design, so anything in that request path could have broadcast
/// it and moved more than the ceiling allowed. The exit code was 22 either way,
/// which is exactly why this asserts on the signing itself: zero calls, because
/// nothing on the path to a refusal signs anything.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_signs_nothing_when_the_price_is_above_the_spend_ceiling_e2e() {
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
    // Advertised, affordable and signable, with a ceiling one unit under the
    // listed price: the rail is refused on price alone.
    let ceiling = USDC_PRICE.parse::<u128>().unwrap() - 1;
    set_spend_ceiling(&ceiling.to_string());

    let counting = CountingSigner::wrapping(agent.signer.as_ref());
    let err = ensure_headless(&counting, &ctx(&contract, None))
        .expect_err("a price above the ceiling must refuse");

    assert!(
        matches!(err, HeadlessError::PriceAbovePolicy { .. }),
        "got {err:?}",
    );
    assert_eq!(err.exit_code(), EXIT_PRICE_ABOVE_POLICY, "{err}");

    assert_eq!(
        counting.calls(),
        0,
        "a refused purchase must not sign anything: an authorization the policy \
         refuses is spendable by whoever sees it",
    );
    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        0,
        "and nothing was broadcast on either rail",
    );
}

/// A payment token that answers every read but cannot actually be paid with
/// selects ETH, without spending a wei of gas finding out.
///
/// `NoSignatureOverloadEIP3009Token` implements EIP-3009 exactly as written -
/// the `(v, r, s)` form - so it is conforming, it holds real balances, and the
/// licence contract's constructor probe accepts it. What it lacks is the
/// `bytes signature` overload the licence contract calls. Nothing the wrapper
/// can read off either contract says so, which is why the rail decision
/// pre-flights the real `purchaseWithAuthorization` call: the agent buys in ETH
/// and the run completes, rather than broadcasting a transaction that could
/// only revert.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_falls_back_to_eth_when_the_token_lacks_the_signature_overload_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let token = deploy_mock_without_signature_overload();
    let contract = deploy_access_with_rail(PRICE_WEI, &token, USDC_PRICE, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    // Advertised, affordable, signable, and priced exactly at the ceiling so it
    // is within policy: the run reaches the pre-flight, and the token refusing
    // the call is the only thing that can select ETH. A ceiling below the price
    // here would refuse on price and stop proving the fallback.
    mint_usdc(&token, agent.address(), USDC_FUNDING);
    set_spend_ceiling(USDC_PRICE);

    let (session, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("a token missing the overload must not end the activation");

    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { paid, .. } => assert_eq!(
            paid,
            &PaymentRail::Eth {
                price_wei: PRICE_WEI.to_string()
            },
            "a token that cannot be paid with must select ETH",
        ),
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    assert_eq!(
        usdc_balance(&token, contract_addr),
        0,
        "nothing paid in a token that cannot take the payment",
    );
    assert_eq!(
        usdc_balance(&token, agent.address()),
        USDC_FUNDING.parse::<u128>().unwrap(),
        "and the agent still holds every unit of it",
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

    // The run above stops at chain-id resolution, so it never reaches the
    // token-side reads. These do: both must classify a dead socket as
    // transport, because that is the classification the rail decision branches
    // on when it refuses to treat a silent node as an answer.
    let usdc_addr: Address = usdc.parse().expect("malformed token address");
    assert!(
        rpc::erc20_balance_of(&dead.rpc_url, usdc_addr, agent.address())
            .expect_err("a dead endpoint cannot report a balance")
            .is_transport(),
        "balanceOf against a dead node must be transport, never a contract answer",
    );
    assert!(
        rpc::token_domain_separator(&dead.rpc_url, usdc_addr)
            .expect_err("a dead endpoint cannot report a domain separator")
            .is_transport(),
        "DOMAIN_SEPARATOR() against a dead node must be transport, never a contract answer",
    );
}

// ── A node that goes away on one call and answers every other ────────────────

/// Forwards JSON-RPC to anvil, except for one call, whose connection it closes
/// with no reply.
///
/// The wrapper reaches the chain through a single URL, so this is the only way
/// to fail exactly one of the token-side reads at the socket while the rest of
/// the run proceeds normally. Dropping the connection is what a node going away
/// mid-call looks like from the client, and it is the failure the rail decision
/// must never read as "this contract has no stablecoin rail".
struct BlockingProxy {
    url: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl BlockingProxy {
    /// Blocks any `eth_call` whose body names both `token` and `selector`, the
    /// 4-byte function selector as lowercase hex without `0x`. Together those
    /// two identify one function on one contract: the licence contract's own
    /// `balanceOf` never carries the payment token's address.
    fn blocking(token: &str, selector: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let url = format!("http://{}", listener.local_addr().expect("proxy addr"));
        // The accept loop polls a shutdown flag, so it must not park in
        // `accept`. `relay` undoes this on each accepted stream; see there.
        listener.set_nonblocking(true).expect("proxy non-blocking");

        let upstream = format!("127.0.0.1:{PORT}");
        let needle_token = token.trim_start_matches("0x").to_ascii_lowercase();
        let needle_selector = selector.trim_start_matches("0x").to_ascii_lowercase();

        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let upstream = upstream.clone();
                        let token = needle_token.clone();
                        let selector = needle_selector.clone();
                        std::thread::spawn(move || relay(client, &upstream, &token, &selector));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            url,
            shutdown,
            handle: Some(handle),
        }
    }
}

impl Drop for BlockingProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Relays one client connection, request by request, until it blocks a call or
/// either side goes quiet.
fn relay(mut client: TcpStream, upstream_addr: &str, token: &str, selector: &str) {
    // `accept` on the BSD socket layer returns a stream carrying the
    // listener's non-blocking flag. Left set, the first read below returns
    // `WouldBlock` before the request has arrived and this proxy would drop
    // every connection instead of the one call it means to block.
    if client.set_nonblocking(false).is_err() {
        return;
    }
    let _ = client.set_read_timeout(Some(Duration::from_secs(10)));
    let mut upstream: Option<TcpStream> = None;

    loop {
        let Some(request) = read_http_message(&mut client) else {
            return;
        };
        let body = String::from_utf8_lossy(&request).to_ascii_lowercase();
        if body.contains(token) && body.contains(selector) {
            return;
        }

        if upstream.is_none() {
            match TcpStream::connect(upstream_addr) {
                Ok(stream) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    upstream = Some(stream);
                }
                Err(_) => return,
            }
        }
        let stream = upstream.as_mut().expect("upstream connected above");

        if stream.write_all(&request).is_err() {
            return;
        }
        let Some(response) = read_http_message(stream) else {
            return;
        };
        if client.write_all(&response).is_err() {
            return;
        }
    }
}

/// Reads one complete HTTP message: headers, then exactly `Content-Length`
/// bytes. Both anvil and the wrapper's client send framed messages, never
/// chunked, so a missing length means a message this proxy cannot relay and
/// the caller gives up rather than forwarding a truncated one.
fn read_http_message(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];

    loop {
        if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())?;
            if buf.len() >= head_end + 4 + length {
                return Some(buf);
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// A node that answers everything except the payment token's `balanceOf`.
///
/// The run gets far enough to read the rail off the contract, then the balance
/// read fails at the socket. Selecting ETH there would be the wrapper deciding
/// the currency on a node's silence.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_transport_failure_on_the_token_balance_read_is_a_hard_error_e2e() {
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

    let proxy = BlockingProxy::blocking(&usdc, "70a08231");
    let mut blinking = ctx(&contract, None);
    blinking.rpc_url = proxy.url.clone();

    let err = ensure_headless(agent.signer.as_ref(), &blinking)
        .expect_err("a node that goes away on balanceOf must not select a currency");
    assert!(
        matches!(err, HeadlessError::Rpc(_)),
        "a silent node is a chain error, got {err:?}",
    );

    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        0,
        "and nothing was bought, least of all in ETH",
    );
}

/// The same, one read later: everything answers except the payment token's
/// `DOMAIN_SEPARATOR()`.
///
/// A token that *answers* that call with a revert selects ETH, because that is
/// a settled fact about the token. A node that fails to answer it is not, and
/// must stop the run.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_transport_failure_on_the_domain_separator_read_is_a_hard_error_e2e() {
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

    let proxy = BlockingProxy::blocking(&usdc, "3644e515");
    let mut blinking = ctx(&contract, None);
    blinking.rpc_url = proxy.url.clone();

    let err = ensure_headless(agent.signer.as_ref(), &blinking)
        .expect_err("a node that goes away on DOMAIN_SEPARATOR() must not select a currency");
    assert!(
        matches!(err, HeadlessError::Rpc(_)),
        "a silent node is a chain error, got {err:?}",
    );

    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        0,
        "and nothing was bought, least of all in ETH",
    );
}
