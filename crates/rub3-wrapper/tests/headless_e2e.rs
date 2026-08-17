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
//!
//! **Every purchase here runs the §2.6 pre-purchase gate**, so every test that
//! buys depends on the locally compiled `Rub3Access` reproducing
//! `contracts/canonical-bytecode.json` byte for byte. If your Foundry resolves
//! a different solc, the whole suite fails as `NotCanonicalContract` / exit 23
//! rather than as the build mismatch it actually is. `contracts/foundry.toml`
//! pins `solc_version`, `optimizer_runs`, `evm_version` and `bytecode_hash`;
//! `scripts/canonical-bytecode-hashes.sh check` is what confirms the local
//! build still matches.

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
    DEFAULT_MAX_ETH_WEI, ENV_MAX_ETH_WEI, ENV_MAX_TOKEN_AMOUNT, EXIT_PRICE_ABOVE_POLICY,
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

/// Deploys the deliberately non-canonical licence fixture,
/// `test/mocks/NonCanonicalRub3Access.sol`.
///
/// Constructor args are `Rub3Access`'s, forwarded unchanged, so a fixture
/// deploy and a canonical deploy are configured identically and differ only in
/// compiled semantics: the fixture carries one extra owner-only function,
/// `reconcileLedger(uint256,address)`, an admin seizure under an accounting
/// name that `attest::FORBIDDEN_SIGNATURES` does not list.
///
/// It lives under `test/`, never `src/`, and the fixture's own header says why
/// at length: `scripts/canonical-bytecode-hashes.sh` fingerprints everything
/// under the resolved source directory, so a copy in `src/` would be published
/// as canonical rub3 code and the wrapper would come to accept the very
/// contract these tests prove it refuses.
fn deploy_non_canonical_access(
    price_wei: &str,
    price_token: &str,
    price_amount: &str,
    supply_cap: &str,
    cooldown_blocks: &str,
) -> String {
    let sale = format!("({price_wei},{price_token},{price_amount})");
    forge_create(
        "test/mocks/NonCanonicalRub3Access.sol:NonCanonicalRub3Access",
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
            let address = rest.trim().to_string();
            // The address is parsed out of forge's own report, so re-read the
            // chain to prove code is actually there: a deploy whose creation
            // transaction reverted would otherwise be handed on as a live
            // contract, and every read against it would fail as if the wrapper
            // were at fault.
            let code = cast_call_raw_code(&address);
            assert!(
                code.len() > 2,
                "test setup failed: deploying {target} reported {address}, but that \
                 address holds no code - the deployment did not land",
            );
            return address;
        }
    }
    panic!("could not find 'Deployed to:' in forge output:\n{stdout}");
}

/// Reads the deployed code at `address` through `cast`, used to confirm a
/// deployment actually landed.
fn cast_call_raw_code(address: &str) -> String {
    let output = Command::new("cast")
        .args(["code", address, "--rpc-url", &rpc_url()])
        .output()
        .expect("failed to run cast code");
    assert!(output.status.success(), "cast code failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Mints mock USDC to `to` through the token's test faucet.
fn mint_usdc(token: &str, to: Address, amount: &str) {
    let to_hex = format!("0x{}", hex::encode(to.as_slice()));
    cast_send_from_deployer(
        &format!("minting mock USDC to {to_hex}"),
        &[token, "mint(address,uint256)", &to_hex, amount],
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

/// The number of transactions `who` has ever sent, read through `cast`.
///
/// The measure of "no transaction was sent": a refusal that has already
/// broadcast something moves this, whatever else it reports.
fn tx_count(who: Address) -> u64 {
    let who_hex = format!("0x{}", hex::encode(who.as_slice()));
    let output = Command::new("cast")
        .args(["nonce", &who_hex, "--rpc-url", &rpc_url()])
        .output()
        .expect("failed to run cast nonce");
    assert!(output.status.success(), "cast nonce failed");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("cast nonce did not return a number")
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
    cast_send_from_deployer(&format!("funding {to_hex}"), &[&to_hex, "--value", amount]);
}

/// Buys a licence for `recipient` on the ETH rail without going through the
/// wrapper, paying from the deployer key.
///
/// `purchase(address recipient)` is callable by anyone and mints to whoever is
/// named, which is what makes this possible: a licence can be put in an
/// agent's hands on a contract the wrapper itself would refuse to buy from, so
/// the launch path can then be driven against it. Returns the id minted, read
/// off `nextTokenId` rather than out of the receipt.
fn seed_licence(contract: &str, recipient: Address, price_wei: &str) -> u64 {
    let minted_before = cast_call_uint(contract, "nextTokenId()(uint256)", &[]);
    let to_hex = format!("0x{}", hex::encode(recipient.as_slice()));
    let step = format!("seeding a licence for {to_hex}");
    cast_send_from_deployer(
        &step,
        &[contract, "purchase(address)", &to_hex, "--value", price_wei],
    );

    // The receipt status alone proves *a* transaction succeeded; this proves
    // the mint this helper promises actually happened, and that the id it is
    // about to hand back is the one that was minted.
    let minted_after = cast_call_uint(contract, "nextTokenId()(uint256)", &[]);
    assert_eq!(
        minted_after,
        minted_before + 1,
        "test setup failed: {step} did not mint - nextTokenId went {minted_before} -> \
         {minted_after}, so token id {minted_before} was never minted and every \
         assertion after this point would be testing the wrapper against a licence \
         that does not exist",
    );
    minted_before as u64
}

/// Mines `n` blocks so a cooldown window can elapse without waiting.
fn mine(n: u32) {
    let before = block_number();
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
        "test setup failed: anvil_mine could not be sent:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    // `anvil_mine` answers with a bare `null` on success, so the only proof the
    // blocks exist is the height. A cooldown window that silently did not
    // elapse would otherwise fail later as a wrapper bug.
    let after = block_number();
    assert_eq!(
        after,
        before + u64::from(n),
        "test setup failed: mining {n} blocks left the chain at height {after}, up from \
         {before} - any cooldown this was meant to clear has not elapsed",
    );
}

/// The current block height, read through `cast`.
fn block_number() -> u64 {
    let output = Command::new("cast")
        .args(["block-number", "--rpc-url", &rpc_url()])
        .output()
        .expect("failed to run cast block-number");
    assert!(output.status.success(), "cast block-number failed");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("cast block-number did not return a number")
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
    cast_send_from_deployer(
        "deploying a Rub3Access through the factory",
        &[
            factory,
            "deployAccess((string,string,(uint8,address),bytes32[],(uint256,address,uint256),uint256,uint256,address,address))",
            &params,
        ],
    );

    cast_call(factory, "deploymentAt(uint256)(address)", &["0"])
}

/// Runs one `cast send` from the deployer key and **proves the transaction
/// landed**, returning its receipt.
///
/// `cast send` exits zero for a transaction that reverted on-chain: the
/// process succeeded, the transaction did not. Trusting the exit code lets a
/// failed setup step run silently and surface much later as a failure of the
/// code under test, which is exactly the misdiagnosis these suites must not
/// produce. So every send here goes through `--json` and asserts the receipt
/// status is `0x1`, and every failure names the step that did not land.
fn cast_send_from_deployer(step: &str, args: &[&str]) -> serde_json::Value {
    let url = rpc_url();
    let mut argv = vec!["send"];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--private-key", DEPLOYER_KEY, "--rpc-url", &url, "--json"]);

    let output = Command::new("cast")
        .args(&argv)
        .output()
        .expect("failed to run cast send");
    assert!(
        output.status.success(),
        "test setup failed: `cast send` for {step} could not be sent:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("test setup failed: {step} receipt was not JSON ({e}):\n{stdout}")
    });
    let status = receipt["status"].as_str().unwrap_or_default();
    assert_eq!(
        status, "0x1",
        "test setup failed: the transaction for {step} reverted on-chain \
         (receipt status {status}); `cast send` still exited zero, so nothing \
         downstream of this step ran against the state it was meant to create.\n\
         receipt:\n{receipt:#}",
    );
    receipt
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
    let mut argv = vec![contract, sig];
    argv.extend_from_slice(args);
    cast_send_from_deployer(&format!("`{sig}` on {contract}"), &argv);
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
        // The spend ceilings are process-global like the key. Cleared here
        // rather than only on drop, so a test that means to run without one
        // cannot inherit a previous test's. Clearing the ETH one restores the
        // built-in default rather than removing the ceiling: that rail always
        // has one.
        std::env::remove_var(ENV_MAX_TOKEN_AMOUNT);
        std::env::remove_var(ENV_MAX_ETH_WEI);

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
        std::env::remove_var(ENV_MAX_ETH_WEI);
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

/// Sets the operator's ETH spend ceiling, in wei, for the current test.
///
/// Never required to make the ETH rail work: unlike the stablecoin ceiling
/// this one has a built-in default, so every other test in this file buys in
/// ETH under [`DEFAULT_MAX_ETH_WEI`] without calling this. Cleared by
/// [`Agent`]'s drop.
fn set_eth_spend_ceiling(wei: &str) {
    std::env::set_var(ENV_MAX_ETH_WEI, wei);
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
    cast_send_from_deployer(
        "taking the only licence in supply, to sell the contract out",
        &[&contract, "purchase(address)", DEPLOYER_ADDR],
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

// ── Pre-purchase code attestation ─────────────────────────────────────────────

/// A contract whose deployed code is not the rub3 template is refused before
/// anything is signed, and no transaction leaves the wrapper.
///
/// The fixture is a real, working, fully deployed contract that simply is not a
/// rub3 licence - which is the shape of the actual threat. A modified copy of
/// the templates would present the same way: it answers the reads, it looks
/// like a licence contract, and its masked code hash is in no pinned table.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_refuses_a_contract_whose_code_is_not_canonical_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    // Real deployed code that is not a rub3 licence contract.
    let impostor = deploy_mock_usdc();
    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    let nonce_before = tx_count(agent.address());
    let balance_before = eth_balance(agent.address());

    let err = ensure_headless(agent.signer.as_ref(), &ctx(&impostor, None))
        .expect_err("code that matches no canonical fingerprint must not be bought from");

    match &err {
        HeadlessError::NotCanonicalContract { contract, .. } => {
            assert_eq!(
                contract.to_lowercase(),
                impostor.to_lowercase(),
                "the refusal must name the address it refused"
            );
        }
        other => panic!("expected NotCanonicalContract, got {other:?}"),
    }
    assert_eq!(err.exit_code(), 23);

    let detail = err
        .machine_detail()
        .expect("a refusal carries a detail line");
    assert!(
        detail.contains("code_bytes="),
        "the detail line must say how much code was there: {detail}"
    );

    // The whole point: the gate runs before the first signature, so nothing was
    // broadcast and no gas was burned.
    assert_eq!(
        tx_count(agent.address()),
        nonce_before,
        "a refusal must not send a transaction"
    );
    assert_eq!(
        eth_balance(agent.address()),
        balance_before,
        "a refusal must not spend gas"
    );
}

/// Launching a licence the agent already holds does not enter the purchase
/// path, so it never runs the pre-purchase gate.
///
/// The posture the gate depends on is "fail closed on purchase, fail open on
/// launch", built as two code paths rather than one helper with a flag. This is
/// the launch half, driven for real: buy once (through the gate), wipe the
/// cached session so the fast path cannot short-circuit anything, let the
/// cooldown elapse, and run again. The second run reaches `activate()` with the
/// token already held, reports `Activated` rather than `PurchasedAndActivated`,
/// and mints nothing - `nextTokenId` is read through `cast`, not through the
/// code under test.
///
/// **What this does not prove**: behaviour when the check *cannot complete*.
/// The fixture is canonical either way, so this cannot separate "the launch
/// path never runs the gate" from "it runs it and passes". Both halves are
/// covered now, against a licence contract that is deliberately not canonical:
/// [`headless_launch_of_an_already_paid_licence_survives_a_contract_the_gate_refuses_e2e`]
/// for code that would fail the comparison, and
/// [`headless_launch_survives_a_node_that_will_not_answer_a_code_read_e2e`] for
/// a read that never returns.
///
/// It knowingly repeats the setup of the second half of
/// [`headless_cooldown_active_then_ready_e2e`] - the overlap is the setup, not
/// the assertions, which are the two `nextTokenId` reads. Kept separate on
/// purpose: the fail-open property deserves its own name and its own failure,
/// so it cannot be dropped silently when someone edits the cooldown test for an
/// unrelated reason. The extra anvil spin-up is the accepted cost.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    // Cooldown 15 blocks = the contract's enforced floor (MIN_COOLDOWN_BLOCKS).
    let contract = deploy_access(PRICE_WEI, "0", "15");
    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);

    // First run: the purchase path, which is the one that runs the gate.
    let (bought, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("a canonical contract must be buyable");
    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { .. } => {}
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    let minted = cast_call_uint(&contract, "nextTokenId()(uint256)", &[]);
    assert_eq!(
        minted, 1,
        "the first run should have minted exactly one token"
    );

    // Wipe the cached session so the second run cannot answer from disk: it has
    // to go back on-chain, find the token already held, and activate it.
    std::fs::remove_file(session_store::session_path(APP_ID, bought.token_id).unwrap())
        .expect("remove cached session");

    // The first activation started the cooldown; step past it.
    mine(16);

    let (relaunched, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("a held licence must launch without going near the purchase path");

    assert_eq!(
        outcome,
        HeadlessOutcome::Activated,
        "the token is already held, so no purchase may run"
    );
    assert_eq!(
        relaunched.token_id, bought.token_id,
        "the same licence should be re-activated"
    );
    session::verify_local(&relaunched).expect("re-activated session must verify");
    assert_eq!(
        cast_call_uint(&contract, "nextTokenId()(uint256)", &[]),
        minted,
        "a launch must not mint a second token",
    );
}

// ── The modified licence: refused on purchase, served on launch ──────────────

/// A contract that *is* a rub3 licence in every observable respect, and differs
/// only in compiled semantics, is refused - and nothing is signed, broadcast or
/// disclosed on either rail.
///
/// This is the case the fingerprint check exists for, and the one
/// [`headless_refuses_a_contract_whose_code_is_not_canonical_e2e`] cannot
/// reach: that fixture is an unrelated contract, so refusing it would also
/// follow from a much weaker check. `NonCanonicalRub3Access` inherits the whole
/// of `Rub3Access`, so it answers every read the wrapper makes with the same
/// values a canonical deploy of the same arguments would - asserted below
/// rather than asserted about - and it carries one extra owner-only seizure
/// under a name no blacklist guessed. **The selector scan therefore passes it
/// in silence and the masked hash catches it**, which is the asymmetry
/// `implementation.md` §2.6 calls the whole justification for the work, pinned
/// here against a deployed contract instead of synthetic bytes.
///
/// The witnesses are chosen for the stablecoin rail, not just the ETH one. The
/// fixture advertises a `priceToken` the agent holds and can afford, so a run
/// that got past the gate would sign an EIP-3009 authorization and hand it to
/// the RPC endpoint as pre-flight calldata before any transaction existed.
/// **Disclosure is the spend** (§2.6), so an unmoved nonce proves nothing on
/// its own: the `CountingSigner` is what says no authorization was ever
/// produced.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_refuses_a_modified_licence_that_passes_the_selector_scan_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    // The same constructor arguments deployed twice: once as the fixture, once
    // as the real thing. Everything an agent can read off the two agrees.
    let modified = deploy_non_canonical_access(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");
    let canonical = deploy_access_with_rail(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");
    for getter in [
        "price()(uint256)",
        "priceAmount()(uint256)",
        "supplyCap()(uint256)",
        "cooldownBlocks()(uint256)",
        "nextTokenId()(uint256)",
        "priceToken()(address)",
        "owner()(address)",
        "identityModel()(uint8)",
    ] {
        assert_eq!(
            cast_call(&modified, getter, &[]),
            cast_call(&canonical, getter, &[]),
            "the fixture must be indistinguishable from a canonical deploy on {getter}",
        );
    }

    let modified_addr: Address = modified.parse().expect("malformed fixture address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    mint_usdc(&usdc, agent.address(), USDC_FUNDING);
    set_spend_ceiling(USDC_PRICE);

    let nonce_before = tx_count(agent.address());
    let eth_before = eth_balance(agent.address());
    let usdc_before = usdc_balance(&usdc, agent.address());

    let counting = CountingSigner::wrapping(agent.signer.as_ref());
    let err = ensure_headless(&counting, &ctx(&modified, None))
        .expect_err("a modified copy of the licence template must not be bought from");

    match &err {
        HeadlessError::NotCanonicalContract { contract, .. } => assert_eq!(
            contract.to_lowercase(),
            modified.to_lowercase(),
            "the refusal must name the address it refused",
        ),
        other => panic!("expected NotCanonicalContract, got {other:?}"),
    }
    assert_eq!(err.exit_code(), 23);

    let detail = err
        .machine_detail()
        .expect("a refusal carries a detail line");
    assert!(
        detail.contains("exposed=none"),
        "the selector scan must pass this contract in silence - it is the hash \
         that catches it, and a fixture the blacklist happened to name would \
         prove the weaker claim instead: {detail}",
    );
    let code_bytes: usize = detail
        .split("code_bytes=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no code_bytes in the detail line: {detail}"));
    assert!(
        code_bytes > 0,
        "the refusal is about code that is there, not about an empty address: {detail}",
    );

    // ── Nothing was spent, on either rail ────────────────────────────────────
    assert_eq!(
        counting.calls(),
        0,
        "a refused purchase must sign nothing: an EIP-3009 authorization is \
         spendable by whoever receives it, so disclosing one is the spend",
    );
    assert_eq!(
        tx_count(agent.address()),
        nonce_before,
        "a refusal must not send a transaction",
    );
    assert_eq!(
        eth_balance(agent.address()),
        eth_before,
        "a refusal must not spend gas",
    );
    assert_eq!(
        usdc_balance(&usdc, agent.address()),
        usdc_before,
        "and must not move a unit of the payment token",
    );
    assert_eq!(
        rpc::next_token_id(&rpc_url(), modified_addr).unwrap(),
        0,
        "nothing was minted",
    );
}

/// A licence the agent already holds still launches on a contract the purchase
/// gate refuses. The fail-open half of the posture, observed rather than
/// inferred.
///
/// §2.4 rules out a revocation surface, and refusing to *start* a program
/// somebody already paid for because an integrity check could not complete
/// would be one. Both outcomes are driven against **the same deployed
/// address**: the held licence activates, and a second agent holding nothing is
/// refused at the gate on that identical contract. One contract, two answers,
/// which is the whole design.
///
/// [`headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e`] is
/// the sibling and not a duplicate: there the contract is canonical, so it
/// cannot tell "the launch path never attests" from "it attests and passes".
/// Here the contract would fail attestation, so a launch path that consulted
/// the module at all could not reach `Activated`.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_launch_of_an_already_paid_licence_survives_a_contract_the_gate_refuses_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    // Cooldown 15 blocks = the contract's enforced floor (MIN_COOLDOWN_BLOCKS).
    let contract = deploy_non_canonical_access(PRICE_WEI, ZERO_ADDR, "0", "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed fixture address");

    let holder = Agent::new();
    fund(holder.address(), FUNDING_ETH);

    // Paid for outside the wrapper, because the wrapper would refuse to buy it
    // - which is precisely the state this test is about.
    let token_id = seed_licence(&contract, holder.address(), PRICE_WEI);
    assert_eq!(
        rpc::tokens_of_owner(&rpc_url(), contract_addr, holder.address()).unwrap(),
        vec![token_id],
        "the agent must start already holding the licence",
    );
    let minted = cast_call_uint(&contract, "nextTokenId()(uint256)", &[]);

    // Nothing cached: `Agent::new` gives a fresh `RUB3_SESSION_DIR`, so the
    // fast path cannot answer and the run has to go back on-chain.
    assert!(
        !session_store::session_path(APP_ID, token_id)
            .unwrap()
            .exists(),
        "the fast path must not be able to short-circuit this launch",
    );
    // The seeding transaction did not activate, so no cooldown is running; step
    // past one anyway, so the assertion below is about attestation and not
    // about timing.
    mine(16);

    let (session, outcome) = ensure_headless(holder.signer.as_ref(), &ctx(&contract, None))
        .expect("a licence already paid for must launch, whatever its code hashes to");

    assert_eq!(
        outcome,
        HeadlessOutcome::Activated,
        "the token is already held, so no purchase - and therefore no gate - may run",
    );
    assert_eq!(session.token_id, token_id);
    session::verify_local(&session).expect("the activated session must verify");
    assert_eq!(
        cast_call_uint(&contract, "nextTokenId()(uint256)", &[]),
        minted,
        "a launch must not mint",
    );

    // ── The same address, the other door ─────────────────────────────────────
    drop(holder);
    let buyer = Agent::new();
    fund(buyer.address(), FUNDING_ETH);
    let err = ensure_headless(buyer.signer.as_ref(), &ctx(&contract, None))
        .expect_err("the contract that just served a launch must still refuse a purchase");
    assert!(
        matches!(err, HeadlessError::NotCanonicalContract { .. }),
        "expected NotCanonicalContract on the purchase door, got {err:?}",
    );
    assert_eq!(err.exit_code(), 23);
    assert_eq!(
        cast_call_uint(&contract, "nextTokenId()(uint256)", &[]),
        minted,
        "and still minted nothing",
    );
}

/// A launch completes against a node that will not answer `eth_getCode`, and a
/// purchase against that same node does not.
///
/// The other way verification fails to complete: not code that hashes wrong,
/// but a chain read that never returns. §2.6 makes a failed read a refusal on
/// purchase and forbids it being one on launch, and this is that sentence made
/// executable in both directions at once.
///
/// The launch arm is not vacuous, which is what the purchase arm is here to
/// show: the same proxy, the same dead method, and the purchase stops at the
/// gate. The recorded request log is the stronger statement of the two - the
/// launch did not merely survive the missing answer, it never asked the
/// question.
///
/// The contract is canonical on purpose. This arm is about the *read*, so a
/// fixture whose code would also fail the comparison would leave two reasons a
/// refusal could have been avoided.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_launch_survives_a_node_that_will_not_answer_a_code_read_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access(PRICE_WEI, "0", "15");
    let holder = Agent::new();
    fund(holder.address(), FUNDING_ETH);
    let token_id = seed_licence(&contract, holder.address(), PRICE_WEI);
    let minted = cast_call_uint(&contract, "nextTokenId()(uint256)", &[]);
    mine(16);

    let proxy = RecordingProxy::refusing_method("eth_getCode");
    let mut through_proxy = ctx(&contract, None);
    through_proxy.rpc_url = proxy.url.clone();

    let (session, outcome) = ensure_headless(holder.signer.as_ref(), &through_proxy)
        .expect("a launch must not depend on a code read the node will not answer");
    assert_eq!(outcome, HeadlessOutcome::Activated);
    assert_eq!(session.token_id, token_id);
    session::verify_local(&session).expect("the activated session must verify");

    let asked_for_code = proxy
        .requests()
        .iter()
        .any(|body| body.to_ascii_lowercase().contains("eth_getcode"));
    assert!(
        !asked_for_code,
        "the launch path must not read the contract's code at all - an \
         integrity check a launch can fail is a revocation surface",
    );

    // ── The control: the same node, the purchase door ────────────────────────
    drop(holder);
    let buyer = Agent::new();
    fund(buyer.address(), FUNDING_ETH);
    let err = ensure_headless(buyer.signer.as_ref(), &through_proxy)
        .expect_err("a purchase must not proceed on a code read that did not complete");
    assert!(
        matches!(err, HeadlessError::Rpc(_)),
        "a read that never returned is a chain error, not a verdict: got {err:?}",
    );
    assert!(
        proxy
            .requests()
            .iter()
            .any(|body| body.to_ascii_lowercase().contains("eth_getcode")),
        "the purchase door really did try the read this proxy refuses, so the \
         launch arm above is a fact about the launch path and not about the proxy",
    );
    assert_eq!(
        cast_call_uint(&contract, "nextTokenId()(uint256)", &[]),
        minted,
        "and bought nothing",
    );
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

/// An ETH price above the ceiling is refused locally, before the transaction
/// exists.
///
/// This is the ETH rail's half of the same guarantee, and the ordering is what
/// it asserts. The contract requires exact payment, so a listing this agent
/// will not pay for would revert on-chain anyway - but reverting costs gas and
/// arrives as a chain error, while a ceiling weighed before `tx::send` costs
/// nothing and arrives as a policy answer. Four independent witnesses that
/// nothing was broadcast: the signer was never asked (so no transaction was
/// ever signed), the nonce did not move, the balance did not move, and nothing
/// was minted.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_refuses_an_eth_price_above_the_spend_ceiling_before_sending_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    // ETH only: no stablecoin rail is advertised, so the ETH ceiling is the
    // only thing that can stop this purchase.
    let contract = deploy_access(PRICE_WEI, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    // One wei under the listed price: the agent is funded, the contract is
    // canonical, and supply is open, so price is the only refusal available.
    let ceiling = PRICE_WEI.parse::<u128>().unwrap() - 1;
    set_eth_spend_ceiling(&ceiling.to_string());

    let nonce_before = tx_count(agent.address());
    let balance_before = eth_balance(agent.address());

    let counting = CountingSigner::wrapping(agent.signer.as_ref());
    let err = ensure_headless(&counting, &ctx(&contract, None))
        .expect_err("an ETH price above the ceiling must refuse");

    assert!(
        matches!(err, HeadlessError::PriceAbovePolicy { .. }),
        "got {err:?}",
    );
    assert_eq!(err.exit_code(), EXIT_PRICE_ABOVE_POLICY, "{err}");

    let detail = err
        .machine_detail()
        .expect("a policy refusal must be machine-readable");
    assert_eq!(
        detail,
        format!("rail=eth listed={PRICE_WEI} maximum={ceiling}"),
        "the ETH refusal names its rail and its amounts, and has no payment token",
    );
    assert!(
        err.to_string().contains(ENV_MAX_ETH_WEI),
        "the message must name the variable that raises this rail's ceiling: {err}",
    );

    assert_eq!(
        counting.calls(),
        0,
        "the refusal must come before anything is signed, so no transaction was built",
    );
    assert_eq!(
        tx_count(agent.address()),
        nonce_before,
        "a refusal must not send a transaction",
    );
    assert_eq!(
        eth_balance(agent.address()),
        balance_before,
        "a refusal must cost no gas",
    );
    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        0,
        "a refusal must mint nothing",
    );
}

/// A price at exactly the ETH ceiling buys, and one under it buys under the
/// built-in default with nothing configured at all.
///
/// The boundary is inclusive, and it is checked here against a real chain
/// rather than only in the unit test, because an off-by-one in the wrong
/// direction would make a correctly configured agent refuse the price it was
/// configured for. The second half is the property that keeps the default from
/// being a breaking change: an operator who sets nothing still buys.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_buys_at_exactly_the_eth_ceiling_and_under_the_default_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access(PRICE_WEI, "0", "15");
    let contract_addr: Address = contract.parse().expect("malformed contract address");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    set_eth_spend_ceiling(PRICE_WEI);

    let (_, outcome) = ensure_headless(agent.signer.as_ref(), &ctx(&contract, None))
        .expect("a price at exactly the ceiling is within policy");
    match outcome {
        HeadlessOutcome::PurchasedAndActivated {
            paid: PaymentRail::Eth { price_wei },
            ..
        } => assert_eq!(price_wei, PRICE_WEI, "the listed price is what was paid"),
        other => panic!("expected an ETH purchase, got {other:?}"),
    }
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 0).unwrap(),
        agent.address(),
        "the licence was obtained",
    );

    // And with nothing configured: a second agent, no ceiling variable set,
    // buying the same listing under the built-in default.
    assert!(
        rpc::eth_price(&rpc_url(), contract_addr).unwrap() <= DEFAULT_MAX_ETH_WEI,
        "the fixture price must sit under the default, or this proves nothing",
    );
    let unconfigured = Agent::new();
    fund(unconfigured.address(), FUNDING_ETH);

    let (_, outcome) = ensure_headless(unconfigured.signer.as_ref(), &ctx(&contract, None))
        .expect("an unset ETH ceiling means the default, not a refusal");
    assert!(
        matches!(
            outcome,
            HeadlessOutcome::PurchasedAndActivated {
                paid: PaymentRail::Eth { .. },
                ..
            }
        ),
        "got {outcome:?}",
    );
    assert_eq!(
        rpc::owner_of(&rpc_url(), contract_addr, 1).unwrap(),
        unconfigured.address(),
        "the unconfigured agent bought too",
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

// ── A node that answers the pre-flight with a revert it invented ─────────────

/// Forwards JSON-RPC to anvil, keeps a copy of every request body, and
/// optionally intervenes on one call: answering a selector's `eth_call` with a
/// revert the chain never gave, or refusing to answer a method at all.
///
/// This is the endpoint the pre-flight actually talks to, modelled honestly:
/// it sees every byte the wrapper sends it, and it decides what to answer.
/// Watching and reverting are one fixture on purpose, because they are one
/// threat - the party that receives a signed authorization is the same party
/// that says whether it executes, so "the revert was transient" and "the revert
/// was a lie" are indistinguishable from the wrapper's side and have the same
/// consequence: a valid authorization is now in someone else's hands and the
/// buyer has been sent down the ETH rail. Refusing a method joins them because
/// it is the same endpoint failing in the other direction, and the launch path
/// has to survive that (see
/// [`headless_launch_survives_a_node_that_will_not_answer_a_code_read_e2e`]).
struct RecordingProxy {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// What [`RecordingProxy`] does to a request instead of forwarding it.
///
/// Recording is unconditional in every mode: what a test asserts about a call
/// the wrapper never made is as load-bearing as what it asserts about one it
/// did.
#[derive(Clone)]
enum Intercept {
    /// Forward everything. The proxy only watches.
    Nothing,
    /// Answer every `eth_call` carrying this 4-byte selector with
    /// `execution reverted`, a revert the chain never gave.
    RevertCall(String),
    /// Close the connection with no reply on any request naming this JSON-RPC
    /// method, which is what a node that will not answer one call looks like
    /// from the client.
    DropMethod(String),
}

impl RecordingProxy {
    /// A faithful relay that only watches.
    fn watching() -> Self {
        Self::new(Intercept::Nothing)
    }

    /// A relay that answers every `eth_call` carrying `selector` with
    /// `execution reverted`, and forwards everything else untouched.
    fn reverting(selector: &str) -> Self {
        Self::new(Intercept::RevertCall(
            selector.trim_start_matches("0x").to_ascii_lowercase(),
        ))
    }

    /// A relay that will not answer `method` at all, and answers everything
    /// else faithfully.
    fn refusing_method(method: &str) -> Self {
        Self::new(Intercept::DropMethod(method.to_ascii_lowercase()))
    }

    fn new(intercept: Intercept) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let url = format!("http://{}", listener.local_addr().expect("proxy addr"));
        // Same reason as `BlockingProxy`: the accept loop polls a shutdown flag
        // and must not park in `accept`.
        listener.set_nonblocking(true).expect("proxy non-blocking");

        let upstream = format!("127.0.0.1:{PORT}");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let recorder = Arc::clone(&seen);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let upstream = upstream.clone();
                        let intercept = intercept.clone();
                        let recorder = Arc::clone(&recorder);
                        std::thread::spawn(move || {
                            record_and_relay(client, &upstream, recorder, &intercept)
                        });
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
            seen,
            shutdown,
            handle: Some(handle),
        }
    }

    /// Every request body the wrapper sent, in order.
    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for RecordingProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Relays one client connection, recording each request and substituting a
/// revert for the intercepted selector.
///
/// Unlike [`relay`], this one keeps going after it acts: the intercepted call
/// is a fallback trigger, not the end of the run, and everything after it -
/// the ETH price read, the estimate, the broadcast - must still reach anvil.
fn record_and_relay(
    mut client: TcpStream,
    upstream_addr: &str,
    seen: Arc<Mutex<Vec<String>>>,
    intercept: &Intercept,
) {
    // See `relay`: the accepted stream inherits the listener's non-blocking
    // flag, which would make the first read fail before the request arrives.
    if client.set_nonblocking(false).is_err() {
        return;
    }
    let _ = client.set_read_timeout(Some(Duration::from_secs(10)));
    let mut upstream: Option<TcpStream> = None;

    loop {
        let Some(request) = read_http_message(&mut client) else {
            return;
        };
        let body = String::from_utf8_lossy(&request).to_string();
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(body.clone());

        let lowered = body.to_ascii_lowercase();
        match intercept {
            Intercept::Nothing => {}
            Intercept::RevertCall(selector) => {
                if lowered.contains("\"eth_call\"") && lowered.contains(selector.as_str()) {
                    if client.write_all(revert_response(&body).as_bytes()).is_err() {
                        return;
                    }
                    continue;
                }
            }
            // No reply, and no more requests on this connection either: a node
            // that goes away mid-call does not come back for the next one.
            Intercept::DropMethod(method) => {
                if lowered.contains(method.as_str()) {
                    return;
                }
            }
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

/// The JSON-RPC body a node returns for a reverted `eth_call`, carrying the
/// request's own id.
///
/// Error code 3 is geth's and reth's, and is the shape the wrapper's own
/// classifier reads as "the chain answered" rather than "the node failed" - so
/// this drives the fallback rather than the hard-error path.
fn revert_response(request_body: &str) -> String {
    let id: u64 = request_body
        .split("\"id\":")
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse().ok()
        })
        .unwrap_or(1);
    let payload = format!(
        r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":3,"message":"execution reverted"}}}}"#
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len(),
    )
}

/// The 4-byte selector of `purchaseWithAuthorization`, taken from the wrapper's
/// own encoder rather than written out, so a signature change cannot leave this
/// fixture watching for a call nobody makes.
fn purchase_with_authorization_selector() -> String {
    encoded_authorization_call()
        .chars()
        .skip(2)
        .take(8)
        .collect()
}

/// Calldata for one `purchaseWithAuthorization`, with every field zeroed. Only
/// its shape is used: the selector, and the offsets the field readers below
/// index by.
fn encoded_authorization_call() -> String {
    rpc::encode_purchase_with_authorization_calldata(
        Address::ZERO,
        rpc::IRub3License::PaymentAuthorization {
            from: Address::ZERO,
            validAfter: alloy::primitives::U256::ZERO,
            validBefore: alloy::primitives::U256::ZERO,
            salt: B256::ZERO,
            signature: vec![0u8; 65].into(),
        },
    )
}

/// The `purchaseWithAuthorization` calldata carried in `blob`, if any: every
/// hex digit from the selector to the first character that is not one.
///
/// The blob is a JSON-RPC body, so this recovers the calldata whether it sat in
/// an `eth_call`'s `data` field or inside the hex of a signed transaction.
fn authorization_calldata(blob: &str) -> Option<String> {
    let lowered = blob.to_ascii_lowercase();
    let at = lowered.find(&purchase_with_authorization_selector())?;
    Some(
        lowered[at..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect(),
    )
}

/// The 32-byte word at `index`, counting from the end of the 4-byte selector.
fn calldata_word(calldata: &str, index: usize) -> u64 {
    let start = 8 + index * 64;
    let word = calldata.get(start..start + 64).unwrap_or_else(|| {
        panic!(
            "calldata is {} chars, too short for word {index}",
            calldata.len()
        )
    });
    // Every field this test reads - an ABI offset and a unix timestamp - lives
    // in the low 8 bytes, so the high 24 must be zero. Asserting that catches a
    // misaligned read rather than silently truncating one.
    assert!(
        word[..48].chars().all(|c| c == '0'),
        "word {index} is not a small integer: 0x{word}",
    );
    u64::from_str_radix(&word[48..], 16).expect("hex word")
}

/// `auth.validBefore` out of `purchaseWithAuthorization` calldata.
///
/// The tuple is dynamic (it ends in `bytes signature`), so its position is read
/// from the ABI head rather than assumed: head word 0 is `recipient`, head word
/// 1 is the offset to the tuple, and `validBefore` is the tuple's third field
/// after `from` and `validAfter`.
fn valid_before(calldata: &str) -> u64 {
    let tuple_at = calldata_word(calldata, 1) as usize;
    assert_eq!(tuple_at % 32, 0, "ABI offsets are word-aligned");
    calldata_word(calldata, tuple_at / 32 + 2)
}

/// Moves the chain's clock forward and mines, so a test can cross an expiry
/// without sleeping through it.
fn warp(secs: u64) {
    let url = rpc_url();
    let output = Command::new("cast")
        .args([
            "rpc",
            "evm_increaseTime",
            &secs.to_string(),
            "--rpc-url",
            &url,
        ])
        .output()
        .expect("failed to run cast rpc evm_increaseTime");
    assert!(
        output.status.success(),
        "evm_increaseTime failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    mine(1);
}

/// Submits raw calldata to `contract` from the deployer key - the third party
/// any endpoint holding an authorization could be - and reports whether the
/// chain took it.
fn submit_raw(contract: &str, calldata: &str) -> Result<(), String> {
    let url = rpc_url();
    let data = format!("0x{}", calldata.trim_start_matches("0x"));
    let output = Command::new("cast")
        .args([
            "send",
            contract,
            &data,
            "--private-key",
            DEPLOYER_KEY,
            "--rpc-url",
            &url,
        ])
        .output()
        .expect("failed to run cast send");
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

/// Simulates raw calldata against `contract` with `eth_call`, moving nothing.
///
/// The positive control for a replay test: it proves the captured blob is a
/// live payment instrument at the moment it is captured, without spending it.
fn call_raw(contract: &str, calldata: &str) -> Result<(), String> {
    let url = rpc_url();
    let data = format!("0x{}", calldata.trim_start_matches("0x"));
    let output = Command::new("cast")
        .args(["call", contract, &data, "--rpc-url", &url])
        .output()
        .expect("failed to run cast call");
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

/// True when a `cast` failure is the token refusing an expired authorization.
///
/// The mock reverts with the custom error `AuthorizationExpired()`; running
/// from raw calldata, `cast` may print the decoded name or only the bare
/// selector, so both spellings count.
fn names_the_expiry(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("authorizationexpired") || lower.contains("0f05f5bf")
}

/// Unix seconds now, the clock `validBefore` is measured against.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the epoch")
        .as_secs()
}

/// The endpoint reverts the pre-flight, keeps the authorization, and tries to
/// spend it after the wrapper has already paid in ETH.
///
/// This is the fallback path's real hazard, and the reason the disclosed copy
/// is short-lived. The token here is the *working* mock: nothing about this
/// purchase is structurally impossible, so the authorization the endpoint is
/// holding is a live payment instrument for the full listed price. The wrapper
/// cannot tell that revert from a genuine one - which is the point - so it does
/// the safe thing and buys in ETH. If the disclosed copy outlived the fallback
/// by any useful margin, the buyer would then pay a second time, in USDC, for a
/// licence they had already bought.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_disclosed_authorization_expires_before_the_endpoint_can_spend_it_e2e() {
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

    let funded_usdc: u128 = USDC_FUNDING.parse().expect("test constant");
    let eth_before = eth_balance(agent.address());

    let proxy = RecordingProxy::reverting(&purchase_with_authorization_selector());
    let mut through_proxy = ctx(&contract, None);
    through_proxy.rpc_url = proxy.url.clone();

    let signed_at = unix_now();
    let (session, outcome) = ensure_headless(agent.signer.as_ref(), &through_proxy)
        .expect("a reverted pre-flight must fall back, not fail");

    // The fallback happened, and it really was the ETH rail that paid.
    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { token_id, paid } => {
            assert_eq!(*token_id, 0);
            assert!(
                matches!(paid, PaymentRail::Eth { .. }),
                "a reverted pre-flight must select ETH, got {paid:?}",
            );
        }
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }
    assert!(
        eth_before - eth_balance(agent.address()) > PRICE_WEI.parse::<u128>().unwrap(),
        "the ETH price plus gas must have left the wallet",
    );
    assert_eq!(
        usdc_balance(&usdc, agent.address()),
        funded_usdc,
        "the wrapper itself moved no USDC",
    );
    session::verify_local(&session).expect("locally signed session must verify");

    // What the endpoint is now holding.
    let disclosed = proxy
        .requests()
        .iter()
        .filter(|body| body.to_ascii_lowercase().contains("\"eth_call\""))
        .find_map(|body| authorization_calldata(body))
        .expect("the pre-flight disclosed an authorization");

    let lifetime = valid_before(&disclosed).saturating_sub(signed_at);
    assert!(
        lifetime <= 60,
        "a disclosed authorization must expire in seconds, not minutes: {lifetime}s",
    );

    // Before the window closes the instrument is genuinely live: simulated,
    // not sent, so it moves nothing and the balance assertions below still
    // mean what they say. This is what makes the failure after the warp
    // attributable to expiry rather than to a blob this test mis-extracted.
    call_raw(&contract, &disclosed).unwrap_or_else(|e| {
        panic!("the disclosed authorization must be spendable before it expires, but: {e}")
    });

    // And it is worthless once it has. Nothing here is faked: the calldata is
    // the bytes that left the machine, replayed verbatim by a third party, on
    // the same chain, against a token that would have honoured it.
    warp(lifetime + 1);
    let replay = submit_raw(&contract, &disclosed);
    let refusal = replay.expect_err("an expired authorization must not be spendable");
    assert!(
        names_the_expiry(&refusal),
        "the replay must fail because the authorization expired, not for some other \
         reason: {refusal}",
    );

    assert_eq!(
        usdc_balance(&usdc, agent.address()),
        funded_usdc,
        "the buyer must not have paid a second time, in a second currency",
    );
    assert_eq!(
        rpc::next_token_id(&rpc_url(), contract_addr).unwrap(),
        1,
        "and must hold one licence, not two",
    );
}

/// The other half of the same trade: the copy that is actually broadcast keeps
/// a window long enough to be mined under congestion.
///
/// The pre-flight window and the submission window solve different problems,
/// and this is what stops the short one from silently becoming the long one.
/// Both copies are read off the wire in a single successful run.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn headless_broadcasts_a_longer_window_than_it_discloses_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let usdc = deploy_mock_usdc();
    let contract = deploy_access_with_rail(PRICE_WEI, &usdc, USDC_PRICE, "0", "15");

    let agent = Agent::new();
    fund(agent.address(), FUNDING_ETH);
    mint_usdc(&usdc, agent.address(), USDC_FUNDING);
    set_spend_ceiling(USDC_PRICE);

    let proxy = RecordingProxy::watching();
    let mut through_proxy = ctx(&contract, None);
    through_proxy.rpc_url = proxy.url.clone();

    let signed_at = unix_now();
    let (_session, outcome) = ensure_headless(agent.signer.as_ref(), &through_proxy)
        .expect("the stablecoin rail must still buy a licence through a plain relay");
    match &outcome {
        HeadlessOutcome::PurchasedAndActivated { paid, .. } => assert!(
            matches!(paid, PaymentRail::Erc3009 { .. }),
            "expected the stablecoin rail, got {paid:?}",
        ),
        other => panic!("expected PurchasedAndActivated, got {other:?}"),
    }

    let requests = proxy.requests();
    let of_method = |method: &str| -> String {
        requests
            .iter()
            .filter(|body| body.to_ascii_lowercase().contains(method))
            .find_map(|body| authorization_calldata(body))
            .unwrap_or_else(|| panic!("no {method} carried an authorization"))
    };

    let disclosed = valid_before(&of_method("\"eth_call\"")).saturating_sub(signed_at);
    let broadcast =
        valid_before(&of_method("\"eth_sendrawtransaction\"")).saturating_sub(signed_at);

    assert!(
        disclosed <= 60,
        "the disclosed copy must expire in seconds: {disclosed}s",
    );
    assert!(
        broadcast >= 600,
        "the broadcast copy needs room to be mined under congestion: {broadcast}s",
    );
}
