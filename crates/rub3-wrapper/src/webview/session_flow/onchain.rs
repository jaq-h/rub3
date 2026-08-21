//! The tier-3 session flow, end to end against a live EVM node.
//!
//! What §1.8 Phase C deferred: the connect → broadcast → sign →
//! persistence-across-restarts flow driven against a real chain, the cooldown
//! the contract enforces met by the wrapper that has to report it, and a
//! deliberately short session TTL expiring into a fresh activation.
//!
//! The seam is [`super::Window`] - the window's IPC handler without the view.
//! Read that module's header for what is and is not covered.
//!
//! Requires the Foundry toolchain (`anvil`, `forge`, `cast`) on PATH, and is
//! `#[ignore]`d by default. Run with:
//!
//!     cargo test -p rub3-wrapper --no-default-features --features tier-3,webview \
//!         --lib -- --ignored webview::session_flow
//!
//! Each test prints `SKIP: ...` and passes when the toolchain is missing, so it
//! is safe to run anywhere.
//!
//! Modelled on `tests/session_onchain_e2e.rs` and `tests/headless_e2e.rs`, on
//! its own anvil port so all three can run at once. Nothing here goes near the
//! purchase screen, so nothing here depends on the locally compiled contracts
//! reproducing `contracts/canonical-bytecode.json`: a licence is put in the
//! wallet's hands with `cast` and the flow starts from a holder, which is where
//! §1.8's flow starts.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use alloy::primitives::Address;

use super::{describe, Wallet, Window};
use crate::webview::ActivationResult;

// Anvil's built-in account #0 - deterministic, documented, holds nothing real.
// Deployer and faucet only; the wallet under test is a fresh key every run.
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEPLOYER_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// Distinct from `session_onchain_e2e.rs` (8547) and `headless_e2e.rs` (8549)
/// so all three suites can run side by side.
const PORT: u16 = 8551;

const APP_ID: &str = "com.rub3.session-flow-test";
const CHAIN_ID: u64 = 31337; // anvil's default
const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Seconds a seat stays taken on the fixture contract (§3.4). Long enough that
/// no flow here lapses mid-test, so the seat state these tests see is only ever
/// what they put there.
const SEAT_TTL_SECS: u64 = 24 * 60 * 60;

/// The contract's enforced floor, `MIN_COOLDOWN_BLOCKS`. Every fixture here
/// deploys at it: the shortest legal window is the fastest to mine past, and it
/// is the value a real deploy is most likely to be checked against.
const COOLDOWN_BLOCKS: u64 = 15;

/// The fixture mints for free. These tests are about activation, not payment -
/// the price paths have their own arms in `headless_e2e.rs` - so the wallet
/// needs funding for gas only.
const PRICE_WEI: &str = "0";

const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";

/// The selector of `CooldownActive(uint256)`, the error
/// `contracts/src/Rub3License.sol` reverts with inside the window. Verified
/// with `cast sig "CooldownActive(uint256)"`.
const COOLDOWN_ACTIVE_SELECTOR: &str = "c1ab61a1";

/// True when a `cast` refusal is the contract enforcing the cooldown.
///
/// [`wallet_sends`] broadcasts raw calldata, so `cast` has no ABI for the
/// target and can only name a custom error when its selector resolves through
/// the machine-global signature cache or an online lookup. Where neither is
/// available it prints the bare selector, so both spellings count - the same
/// hazard `names_the_expiry` in `tests/headless_e2e.rs` documents.
///
/// Deliberately still specific to this one error: a bare "the send failed"
/// check would let an out-of-gas or a `NotTokenOwner` pass as a cooldown
/// refusal, which is the whole point of asserting on it.
fn names_the_cooldown(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("cooldownactive") || lower.contains(COOLDOWN_ACTIVE_SELECTOR)
}

/// Every test here binds [`PORT`] and sets `RUB3_SESSION_DIR`, which is
/// process-global. Held for the whole body so the anvil instance and the
/// session directory belong to one test at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// Takes the file lock and [`crate::ENV_LOCK`], always in that order.
///
/// Both are needed: the file lock keeps these tests off each other's anvil and
/// session directory, and the crate-wide one keeps them off the environment the
/// rest of the crate's unit tests mutate, which knows nothing about the first.
/// A panicking test poisons them; that test has already failed and the next one
/// starts from a fresh anvil, so the poison is cleared rather than cascading.
fn serial_guard() -> (MutexGuard<'static, ()>, MutexGuard<'static, ()>) {
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    (serial, env)
}

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
    // crates/rub3-wrapper → crates → workspace root → contracts
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

/// Deploys `Rub3Access` minting for free on the ETH rail, with no supply cap.
///
/// Constructor args (10): name, symbol, identity, wrapperHashes, sale, fee,
/// supplyCap, session, predecessor, owner - the shape `headless_e2e.rs`
/// documents at length. `identity` is `(model, tbaImpl)`; `sale` is
/// `(price, priceToken, priceAmount)`, where a zero `priceToken` advertises no
/// stablecoin rail; `fee` is `(feeBps, treasury)`, zero for a direct deploy.
/// `wrapperHashes` is seeded with one stand-in release hash because the zero
/// hash is the `Unknown` sentinel and is rejected on-chain. `session` is the
/// `SessionTerms` of §3.4 - `(cooldownBlocks, seatsPerToken,
/// sessionTtlSeconds)` - with the single seat these flows are about; the seat
/// count itself is proven in `contracts/test/Rub3Seats.t.sol`.
fn deploy_access() -> String {
    deploy_access_with_seats(1)
}

/// The same fixture granting `seats` concurrent sessions per token.
fn deploy_access_with_seats(seats: u64) -> String {
    let sale = format!("({PRICE_WEI},{ZERO_ADDR},0)");
    let session = format!("({COOLDOWN_BLOCKS},{seats},{SEAT_TTL_SECS})");
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
            "Rub3 Session Flow Test",
            "RUB3S",
            "(0,0x0000000000000000000000000000000000000000)",
            "[0x1111111111111111111111111111111111111111111111111111111111111111]",
            &sale,
            "(0,0x0000000000000000000000000000000000000000)",
            "0",
            &session,
            ZERO_ADDR,
            DEPLOYER_ADDR,
        ])
        .output()
        .expect("failed to run forge create");

    assert!(
        output.status.success(),
        "forge create failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Deployed to: ") {
            return rest.trim().to_string();
        }
    }
    panic!("could not find 'Deployed to:' in forge output:\n{stdout}");
}

/// Sends `amount` of ETH from the faucet to `to`, and confirms it landed.
///
/// A `cast send` exit code says the request was accepted, not that the balance
/// moved. Reading the balance back is what makes a failed fund fail here rather
/// than as a confusing out-of-gas three steps later.
fn fund(to: &str, amount: &str) {
    let output = Command::new("cast")
        .args([
            "send",
            to,
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
        "funding {to} failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    let balance = cast_stdout(&["balance", to, "--rpc-url", &rpc_url()]);
    assert!(
        balance
            .trim()
            .parse::<u128>()
            .expect("cast balance should print a decimal")
            > 0,
        "funding {to} reported success but the balance is still zero",
    );
}

/// Mints a licence to `owner` without going through the wrapper, and confirms
/// the token really is theirs before returning its id.
///
/// `purchase(address)` is callable by anyone and mints to whoever is named, so
/// the faucet can put a licence in the wallet's hands. §1.8's flow starts from
/// a holder; how they became one is §1.7's.
fn seed_licence(contract: &str, owner: &str) -> u64 {
    let token_id = cast_stdout(&[
        "call",
        contract,
        "nextTokenId()(uint256)",
        "--rpc-url",
        &rpc_url(),
    ])
    .trim()
    .parse::<u64>()
    .expect("nextTokenId should be a decimal");

    let output = Command::new("cast")
        .args([
            "send",
            contract,
            "purchase(address)",
            owner,
            "--value",
            PRICE_WEI,
            "--private-key",
            DEPLOYER_KEY,
            "--rpc-url",
            &rpc_url(),
        ])
        .output()
        .expect("failed to run cast send purchase");
    assert!(
        output.status.success(),
        "seeding a licence for {owner} failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    let owner_addr: Address = owner.parse().expect("owner address should parse");
    let contract_addr: Address = contract.parse().expect("contract address should parse");
    assert_eq!(
        crate::rpc::owner_of(&rpc_url(), contract_addr, token_id).unwrap(),
        owner_addr,
        "seeding reported success but token {token_id} is not owned by {owner}",
    );
    token_id
}

/// Broadcasts raw calldata to `contract` from `wallet`, the way the screen asks
/// the user to. Returns the transaction hash, or the node's refusal.
///
/// The refusal is both output streams, because where `cast` writes it moved
/// between releases: up to 1.5 a failed `cast send --json` printed the error
/// on stderr, and from 1.8 it prints a JSON envelope on stdout and nothing on
/// stderr. CI installs the current stable, so reading one stream alone hands
/// [`names_the_cooldown`] an empty string on one side of that line.
fn wallet_sends(wallet: &Wallet, contract: &str, calldata: &str) -> Result<String, String> {
    let key = format!("0x{}", hex::encode(wallet.key.to_bytes()));
    let output = Command::new("cast")
        .args([
            "send",
            contract,
            calldata,
            "--private-key",
            &key,
            "--rpc-url",
            &rpc_url(),
            "--json",
        ])
        .output()
        .expect("failed to run cast send");

    if !output.status.success() {
        let refusal = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return Err(refusal);
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cast send returned non-json");
    Ok(json
        .get("transactionHash")
        .and_then(|v| v.as_str())
        .expect("cast send json missing transactionHash")
        .to_string())
}

/// Mines `n` blocks so a cooldown window can elapse without waiting.
fn mine(n: u64) {
    let before = current_block();
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
    let after = current_block();
    assert_eq!(
        after,
        before + n,
        "test setup failed: mining {n} blocks left the chain at height {after}, up from \
         {before} - any cooldown this was meant to clear has not elapsed",
    );
}

fn current_block() -> u64 {
    cast_stdout(&["block-number", "--rpc-url", &rpc_url()])
        .trim()
        .parse()
        .expect("cast block-number should print a decimal")
}

fn cast_stdout(args: &[&str]) -> String {
    let output = Command::new("cast")
        .args(args)
        .output()
        .expect("failed to run cast");
    assert!(
        output.status.success(),
        "cast {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("cast returned non-utf8")
}

// ── Fixture ───────────────────────────────────────────────────────────────────

/// A holder ready to activate: a funded fresh key owning one licence on a fresh
/// contract, with a session directory of its own.
struct Holder {
    wallet: Wallet,
    contract: String,
    token_id: u64,
    _session_dir: tempfile::TempDir,
}

impl Holder {
    fn set_up() -> Self {
        Self::set_up_with_seats(1)
    }

    /// The same holder on a contract granting `seats` concurrent sessions.
    fn set_up_with_seats(seats: u64) -> Self {
        let contract = deploy_access_with_seats(seats);
        let wallet = Wallet::new();
        // Gas only - the fixture mints for free.
        fund(&wallet.address, "1ether");
        let token_id = seed_licence(&contract, &wallet.address);

        let session_dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("RUB3_SESSION_DIR", session_dir.path());

        Self {
            wallet,
            contract,
            token_id,
            _session_dir: session_dir,
        }
    }

    fn window(&self, ttl_secs: i64) -> Window {
        Window::open(APP_ID, &self.contract, CHAIN_ID, &rpc_url(), ttl_secs)
    }

    /// Drives one whole activation: connect, broadcast what the cooldown screen
    /// hands over, wait for the confirmation, sign the preimage, take the
    /// result.
    ///
    /// Only the sequencing the three tests share lives here; the assertions
    /// that belong to each step stay in the test that is about that step.
    fn activate(&self, window: &Window) -> crate::session::Session {
        let cooldown = self.connect(window);
        assert_eq!(cooldown["ready"], true, "the token should be activatable");

        let calldata = cooldown["calldata"].as_str().expect("calldata is a string");
        let tx_hash = wallet_sends(&self.wallet, &self.contract, calldata)
            .expect("the wallet should be able to broadcast the screen's calldata");

        let confirmed = self.confirm(window, &tx_hash);
        self.sign(window, &confirmed);

        match window.result() {
            ActivationResult::SessionSuccess { session } => session,
            other => panic!("expected SessionSuccess, got {}", describe(&other)),
        }
    }

    /// Posts `connect` and returns the cooldown screen's payload.
    ///
    /// Connects with the checksummed spelling, which is what a wallet hands
    /// the page, and what every screen downstream of here has to cope with.
    fn connect(&self, window: &Window) -> serde_json::Value {
        window.post(serde_json::json!({
            "type": "connect",
            "address": self.wallet.checksummed(),
        }));
        let payload = window.expect_only("onShowCooldown");
        assert_eq!(payload["tokenId"], self.token_id);
        payload
    }

    /// Posts `activate_tx_sent` and returns the payload the background poller
    /// produces once the transaction has landed.
    ///
    /// The page carries the address the cooldown screen showed it, so this is
    /// still the checksummed spelling; normalising it is the wrapper's job.
    fn confirm(&self, window: &Window, tx_hash: &str) -> serde_json::Value {
        window.post(serde_json::json!({
            "type":          "activate_tx_sent",
            "tx_hash":       tx_hash,
            "token_id":      self.token_id,
            "owner_address": self.wallet.checksummed(),
        }));
        window.wait_for("onProcessing");
        window.wait_for("onTxConfirmed")
    }

    /// Signs the preimage the confirmation screen showed and posts it back,
    /// echoing every field the page echoes.
    fn sign(&self, window: &Window, confirmed: &serde_json::Value) {
        let signature = self.wallet.personal_sign(
            confirmed["sessionMessage"]
                .as_str()
                .expect("sessionMessage should be a hex string"),
        );
        window.post(serde_json::json!({
            "type":                  "session_signed",
            "signature":             signature,
            "token_id":              confirmed["tokenId"],
            "owner_address":         confirmed["ownerAddress"],
            "identity":              confirmed["identity"],
            "user_id":               confirmed["userId"],
            "tba":                   confirmed["tba"],
            "nonce":                 confirmed["nonce"],
            "expires_at":            confirmed["expiresAt"],
            "session_id":            confirmed["sessionId"],
            "activation_tx":         confirmed["txHash"],
            "activation_block":      confirmed["blockNumber"],
            "activation_block_hash": confirmed["blockHash"],
        }));
    }

    fn contract_addr(&self) -> Address {
        self.contract
            .parse()
            .expect("contract address should parse")
    }

    /// The block the contract recorded for this token's last activation.
    fn last_activation_block(&self) -> u64 {
        crate::rpc::last_activation_block(&rpc_url(), self.contract_addr(), self.token_id)
            .expect("lastActivationBlock read")
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        std::env::remove_var("RUB3_SESSION_DIR");
    }
}

/// Writes a session where the launch fast path will look for it, through the
/// same call `activation::ensure` makes when the window succeeds.
fn persist(session: crate::session::Session) {
    crate::activation::persist_activation(APP_ID, ActivationResult::SessionSuccess { session })
        .expect("the session should be written to the session store");
}

// ── The full flow ─────────────────────────────────────────────────────────────

/// Connect → broadcast → confirm → sign → persist, then a relaunch served from
/// the persisted session with no window and no second transaction.
///
/// The §1.8 thesis for the human door, the way `headless_purchase_activate_
/// persist_e2e` is for the agent door.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn a_connected_wallet_activates_signs_and_the_session_survives_a_restart_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let holder = Holder::set_up();
    let window = holder.window(SESSION_TTL_SECS);

    // ── The page loads ───────────────────────────────────────────────────────
    window.post(serde_json::json!({ "type": "ready" }));
    let info = window.expect_only("onAppInfo");
    assert_eq!(info["appId"], APP_ID);
    assert_eq!(info["contractAddress"], holder.contract);
    assert_eq!(info["chainId"], CHAIN_ID);

    // ── Connect: one token held, so straight to the cooldown screen ──────────
    assert_eq!(
        holder.last_activation_block(),
        0,
        "nothing should have activated yet",
    );
    let posted_address = holder.wallet.checksummed();
    assert_ne!(
        posted_address, holder.wallet.address,
        "the flow has to be driven with a spelling the wrapper must normalise",
    );
    let cooldown = holder.connect(&window);
    assert_eq!(
        cooldown["ownerAddress"], posted_address,
        "the cooldown screen hands back the address it was given",
    );
    assert_eq!(cooldown["contractAddress"], holder.contract);
    assert_eq!(
        cooldown["ready"], true,
        "a token that has never activated is ready",
    );
    assert_eq!(cooldown["blocksRemaining"], 0);

    // ── The wallet broadcasts the calldata the screen handed it ──────────────
    let calldata = cooldown["calldata"]
        .as_str()
        .expect("calldata is a string")
        .to_string();
    assert_eq!(
        calldata.len(),
        2 + 8 + 64,
        "activate(uint256) is a selector plus one word: {calldata}",
    );

    let tx_hash = wallet_sends(&holder.wallet, &holder.contract, &calldata)
        .expect("the chain should accept the screen's calldata");
    assert_ne!(
        holder.last_activation_block(),
        0,
        "the broadcast calldata should have recorded an activation",
    );

    // ── The wrapper polls, reads the chain, and builds the preimage ──────────
    let confirmed = holder.confirm(&window, &tx_hash);
    assert_eq!(confirmed["tokenId"], holder.token_id);
    assert_eq!(confirmed["txHash"], tx_hash);
    assert_eq!(
        confirmed["sessionId"], 1,
        "the first activation is session 1"
    );
    assert_eq!(confirmed["identity"], "access");
    assert_eq!(
        confirmed["ownerAddress"], holder.wallet.address,
        "the preimage commits to the normalised address, so that is the value \
         JS has to be handed back",
    );
    assert_ne!(
        confirmed["ownerAddress"],
        serde_json::Value::from(posted_address.as_str()),
        "echoing the page's own spelling back would sign one string and hand \
         JS another, and the session would fail to verify",
    );
    assert_eq!(
        confirmed["blockHash"],
        cast_stdout(&["receipt", &tx_hash, "blockHash", "--rpc-url", &rpc_url()]).trim(),
        "the session must bind to the block the activation really landed in",
    );

    // ── The wallet signs, and the window hands back a session ────────────────
    holder.sign(&window, &confirmed);
    let session = match window.result() {
        ActivationResult::SessionSuccess { session } => session,
        other => panic!("expected SessionSuccess, got {}", describe(&other)),
    };

    assert_eq!(session.app_id, APP_ID);
    assert_eq!(session.token_id, holder.token_id);
    assert_eq!(session.session_id, Some(1));
    assert_eq!(session.contract, holder.contract);
    assert_eq!(session.activation_tx.as_deref(), Some(tx_hash.as_str()));
    assert!(session.tba.is_none(), "the access model has no TBA");
    crate::session::verify_local(&session).expect("the issued session must verify locally");
    crate::session::verify_onchain(&session, &rpc_url())
        .expect("the issued session must match the chain");

    // ── Persisted through the same call the launch path makes ────────────────
    let nonce = session.nonce.clone();
    persist(session);

    let stored = crate::session_store::load_session(APP_ID, holder.token_id)
        .expect("the session should be on disk under its token id");
    assert_eq!(stored.nonce, nonce);

    // ── Restart: served from disk, no window, no second activation ───────────
    let activated_at = holder.last_activation_block();
    crate::activation::ensure(
        APP_ID,
        &holder.contract,
        CHAIN_ID,
        &rpc_url(),
        None,
        SESSION_TTL_SECS,
    )
    .expect("a stored session must launch without re-activating");
    assert_eq!(
        holder.last_activation_block(),
        activated_at,
        "the fast path must not send a second activate()",
    );
}

// ── Cooldown enforcement ──────────────────────────────────────────────────────

/// A second `activate()` inside `cooldownBlocks` is refused by the contract,
/// and the window says how many blocks are left rather than presenting a screen
/// whose calldata would revert.
///
/// Both halves matter. The contract-side rule has been live since §1.5; what
/// §1.8 left unproven is the wrapper meeting it - reading `cooldownReady` and
/// carrying its answer into the screen a person acts on.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn a_second_activation_inside_the_cooldown_is_refused_and_the_window_says_how_long_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let holder = Holder::set_up();

    // First activation, through the whole flow, starts the window.
    let first = holder.activate(&holder.window(SESSION_TTL_SECS));
    assert_eq!(first.session_id, Some(1));
    let first_nonce = first.nonce.clone();
    persist(first);
    let activated_at = holder.last_activation_block();

    // ── The contract refuses ─────────────────────────────────────────────────
    let calldata = crate::rpc::encode_activate_calldata(holder.token_id);
    let refusal = wallet_sends(&holder.wallet, &holder.contract, &calldata)
        .expect_err("the contract must refuse a second activate() inside the window");
    assert!(
        names_the_cooldown(&refusal),
        "the refusal should be CooldownActive, got: {refusal}",
    );
    assert_eq!(
        holder.last_activation_block(),
        activated_at,
        "a refused activate() must not move lastActivationBlock",
    );

    // ── The window reports it ────────────────────────────────────────────────
    // A person only ever meets this screen with the cached session gone:
    // while it is on disk the launch path is served from it and opens no
    // window at all, inside the cooldown or out of it.
    assert!(
        crate::activation::try_session_fast_path(APP_ID, &rpc_url(), None, None).is_some(),
        "the session persisted above must still be served from cache",
    );
    std::fs::remove_file(crate::session_store::session_path(APP_ID, holder.token_id).unwrap())
        .expect("remove the cached session");
    assert!(
        crate::activation::try_session_fast_path(APP_ID, &rpc_url(), None, None).is_none(),
        "with the session gone the launch path has to open the window instead",
    );

    let payload = holder.connect(&holder.window(SESSION_TTL_SECS));
    assert_eq!(
        payload["ready"], false,
        "the window must not offer a ready screen inside the cooldown",
    );
    let remaining = payload["blocksRemaining"]
        .as_u64()
        .expect("blocksRemaining should be a number");
    assert_eq!(
        remaining,
        COOLDOWN_BLOCKS - (current_block() - activated_at),
        "the window should report the blocks the contract is still counting",
    );

    // ── Short of the window is still short ───────────────────────────────────
    //
    // Two blocks, not one. `cooldownReady` is a view evaluated at the current
    // head, while a transaction broadcast against that answer executes in the
    // next block, so a send at `blocksRemaining == 1` lands exactly on the
    // boundary and is accepted. Two is the smallest margin at which the view
    // and the transaction agree, and it is the margin that matters: the screen
    // must keep saying "not yet" while the chain would still refuse.
    assert!(
        remaining >= 3,
        "the fixture should leave room to step down to two blocks, got {remaining}",
    );
    mine(remaining - 2);
    let payload = holder.connect(&holder.window(SESSION_TTL_SECS));
    assert_eq!(
        payload["ready"], false,
        "short of the window is still inside it",
    );
    assert_eq!(payload["blocksRemaining"], 2);
    let refusal = wallet_sends(&holder.wallet, &holder.contract, &calldata)
        .expect_err("the chain should still refuse two blocks short");
    assert!(
        names_the_cooldown(&refusal),
        "two blocks short the refusal should still be CooldownActive, got: {refusal}",
    );

    // ── And the last blocks clear it, for both of them ───────────────────────
    mine(2);
    let payload = holder.connect(&holder.window(SESSION_TTL_SECS));
    assert_eq!(payload["ready"], true, "the window has elapsed");
    assert_eq!(payload["blocksRemaining"], 0);

    let second = holder.activate(&holder.window(SESSION_TTL_SECS));
    assert_eq!(
        second.session_id,
        Some(2),
        "a second activate() bumps the session id",
    );
    assert_ne!(
        second.nonce, first_nonce,
        "a re-activation mints a fresh session",
    );
}

// ── Expiry ────────────────────────────────────────────────────────────────────

/// A short TTL, so expiry is observable inside a test rather than in seven days.
const SHORT_TTL_SECS: i64 = 2;

/// An expired session is not silently honoured: the launch fast path declines
/// it, and a fresh activation replaces it.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn an_expired_session_is_refused_and_a_fresh_activation_replaces_it_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let holder = Holder::set_up();

    // ── A session that lapses in two seconds ─────────────────────────────────
    let first = holder.activate(&holder.window(SHORT_TTL_SECS));
    let first_nonce = first.nonce.clone();
    assert!(
        !crate::session::is_expired(&first),
        "a session issued a moment ago is not expired",
    );
    persist(first);

    // While it is live, the launch fast path serves it.
    assert!(
        crate::activation::try_session_fast_path(APP_ID, &rpc_url(), None, None).is_some(),
        "a live session must be served from cache",
    );

    // ── Let it lapse ─────────────────────────────────────────────────────────
    std::thread::sleep(Duration::from_secs(SHORT_TTL_SECS as u64 + 1));

    let stored = crate::session_store::load_session(APP_ID, holder.token_id)
        .expect("the file is still on disk - expiry is a property of its contents");
    assert!(
        crate::session::is_expired(&stored),
        "the stored session should have lapsed",
    );
    assert!(
        crate::activation::try_session_fast_path(APP_ID, &rpc_url(), None, None).is_none(),
        "an expired session must not be honoured",
    );

    // ── Which drives a fresh activation ──────────────────────────────────────
    // Past the cooldown first: an expired session inside a live cooldown is a
    // real state, but it is the previous test's subject, not this one's.
    mine(COOLDOWN_BLOCKS);

    let second = holder.activate(&holder.window(SHORT_TTL_SECS));
    assert_eq!(
        second.session_id,
        Some(2),
        "re-activation goes back on-chain rather than re-dating the old session",
    );
    assert_ne!(
        second.nonce, first_nonce,
        "a re-activation mints a fresh session",
    );
    crate::session::verify_local(&second).expect("the replacement session must verify");
    persist(second);

    assert!(
        crate::activation::try_session_fast_path(APP_ID, &rpc_url(), None, None).is_some(),
        "the replacement session must be the one served",
    );
}

// ── Auto-detect against a live chain (§5.1a) ──────────────────────────────────
//
// `auto_detect.rs` proves the wiring against a stub: that a found hash drives
// the same flow a pasted one does, and that a watch stops when its screen does.
// What a stub cannot prove is that the two watchers ask a real node the right
// question - a filter that pins the wrong topic, or an `Activated` signature
// that has drifted from the deployed contract, answers a stub exactly as well
// as it answers nothing. That is what these two are for.
//
// Still nothing here goes near `show_purchase` or its attestation gate: the
// mint arm posts `auto_watch_start` the way the page does once that screen is
// already up, which is where §1.7 hands over.

/// How long to give the watch thread to read the head block before the wallet
/// broadcasts.
///
/// The watch starts from whatever `eth_blockNumber` says when its thread runs,
/// and only counts what lands after that. A local read takes microseconds, so
/// this is a wide margin around a race that would otherwise be silent: the
/// watch would sit out its whole budget looking past a transaction already
/// mined, and the test would fail as a timeout with no hint why.
const WATCH_STARTUP_GRACE: Duration = Duration::from_millis(1000);

/// Auto-detect finds the `activate()` the wallet broadcast, and the flow
/// continues exactly as a pasted hash would have continued.
///
/// The chain read under test is `watch_for_activate`: `lastActivationBlock`
/// moving past the head the screen opened at, then that one block queried for
/// the `Activated` log naming this token. The assertion that it found the
/// *right* transaction is the hash comparison - a resolution that picked
/// anything else in the block would pass a weaker check and produce a session
/// bound to the wrong activation.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn auto_detect_finds_the_activation_and_the_session_completes_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let holder = Holder::set_up();
    let window = holder.window(SESSION_TTL_SECS);

    let cooldown = holder.connect(&window);
    assert_eq!(cooldown["ready"], true);
    assert_eq!(
        cooldown["autoWatchSecs"],
        crate::rpc::WATCH_BUDGET.as_secs(),
        "a tier-3 build must offer the Auto-detect tab, and say for how long",
    );

    // The page's Auto-detect tab, which is the default, starts the watch as
    // soon as the screen renders - before the wallet has done anything.
    window.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "activate",
        "owner_address": holder.wallet.checksummed(),
        "token_id":      holder.token_id,
    }));
    std::thread::sleep(WATCH_STARTUP_GRACE);

    let calldata = cooldown["calldata"].as_str().expect("calldata is a string");
    let tx_hash = wallet_sends(&holder.wallet, &holder.contract, calldata)
        .expect("the chain should accept the screen's calldata");

    // No paste. The same two calls the manual path produces, in the same order.
    window.wait_for("onProcessing");
    let confirmed = window.wait_for("onTxConfirmed");
    assert_eq!(
        confirmed["txHash"], tx_hash,
        "the watch must resolve to the wallet's own activation, not merely to \
         something in the same block",
    );
    assert_eq!(confirmed["tokenId"], holder.token_id);
    assert_eq!(confirmed["sessionId"], 1);
    assert_eq!(
        confirmed["ownerAddress"], holder.wallet.address,
        "the preimage still commits to the normalised address",
    );

    // And the finalize path is untouched: the same signature over the same
    // preimage produces the same result.
    holder.sign(&window, &confirmed);
    let session = match window.result() {
        ActivationResult::SessionSuccess { session } => session,
        other => panic!("expected SessionSuccess, got {}", describe(&other)),
    };
    assert_eq!(session.activation_tx.as_deref(), Some(tx_hash.as_str()));
    assert_eq!(session.token_id, holder.token_id);
    crate::session::verify_local(&session).expect("the issued session must verify");
}

/// **Above one seat the cooldown screen offers no Auto-detect tab at all.**
/// The watch resolves an `activate()` from the chain alone - the block
/// `lastActivationBlock` names, filtered to this contract and this token - and
/// a fleet instance activating the same token in the same block puts a second
/// log in that filter which nothing separates from the person's own, since
/// every instance signs with the holder's key. The session would then be signed
/// over another instance's `sessionId` and `seatIndex`: two live sessions on
/// one seat, in silence. The screen asks for a pasted hash instead, which is
/// unambiguously the user's.
///
/// The absence of `autoWatchSecs` is what the page reads to decide which tabs
/// exist, so this is the mechanism and not a proxy for it.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn a_multi_seat_licence_offers_no_auto_detect_tab_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let holder = Holder::set_up_with_seats(4);
    let window = holder.window(SESSION_TTL_SECS);

    let cooldown = holder.connect(&window);
    assert_eq!(cooldown["seats"], 4);
    assert_eq!(cooldown["ready"], true);
    assert!(
        cooldown.get("autoWatchSecs").is_none(),
        "a licence with a fleet must offer the Manual tab alone: {cooldown}",
    );

    // And the manual path is untouched: the screen's own calldata still opens a
    // session for the person who broadcast it.
    let calldata = cooldown["calldata"].as_str().expect("calldata is a string");
    let tx_hash = wallet_sends(&holder.wallet, &holder.contract, calldata)
        .expect("the chain should accept the screen's calldata");
    let confirmed = holder.confirm(&window, &tx_hash);
    assert_eq!(confirmed["txHash"], tx_hash);
    holder.sign(&window, &confirmed);
    match window.result() {
        ActivationResult::SessionSuccess { session } => {
            crate::session::verify_local(&session).expect("the issued session must verify");
        }
        other => panic!("expected SessionSuccess, got {}", describe(&other)),
    }
}

/// Auto-detect finds the mint a `purchase()` produced, and the purchase poller
/// carries it into the activation flow.
///
/// The chain read under test is `watch_for_mint`: the `Transfer(0x0, wallet, *)`
/// filter against a real node's `eth_getLogs`. A wallet with no licence buys one
/// the way §1.7's screen asks it to, and the token id the flow arrives at has to
/// be the one the chain actually minted.
#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn auto_detect_finds_the_mint_and_the_flow_continues_e2e() {
    let _serial = serial_guard();
    if !toolchain_ready() {
        return;
    }
    let _anvil = start_anvil();

    let contract = deploy_access();
    let buyer = Wallet::new();
    fund(&buyer.address, "1ether");

    let session_dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var("RUB3_SESSION_DIR", session_dir.path());

    let window = Window::open(APP_ID, &contract, CHAIN_ID, &rpc_url(), SESSION_TTL_SECS);
    window.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "mint",
        "owner_address": buyer.checksummed(),
        "token_id":      serde_json::Value::Null,
    }));
    std::thread::sleep(WATCH_STARTUP_GRACE);

    let recipient: Address = buyer.address.parse().expect("buyer address should parse");
    let calldata = crate::rpc::encode_purchase_calldata(recipient);
    let tx_hash = wallet_sends(&buyer, &contract, &calldata)
        .expect("the chain should accept a purchase from a funded wallet");

    window.wait_for("onProcessing");
    let cooldown = window.wait_for("onShowCooldown");

    let minted = crate::rpc::mint_token_id(
        &rpc_url(),
        &tx_hash,
        contract.parse().expect("contract address should parse"),
        recipient,
    )
    .expect("the purchase should have minted a token");
    assert_eq!(
        cooldown["tokenId"], minted,
        "the flow must arrive at the token the chain actually minted",
    );
    assert_eq!(cooldown["ownerAddress"], buyer.checksummed());

    std::env::remove_var("RUB3_SESSION_DIR");
}
