//! End-to-end test for `session::verify_onchain` against a live EVM node.
//!
//! Spawns `anvil`, deploys `Rub3Access`, executes a purchase + activate,
//! extracts the real tx hash + block hash, and exercises both the happy path
//! and the three tampered-field failure modes of `verify_onchain`.
//!
//! Requires the Foundry toolchain (`anvil`, `forge`, `cast`) on PATH.
//! Ignored by default — run with:
//!
//!     cargo test -p rub3-wrapper --no-default-features --features tier-3 \
//!         -- --ignored session_verify_onchain_e2e
//!
//! The test prints `SKIP: ...` and returns Ok when the toolchain is missing,
//! so it is safe to run in any environment.

#![cfg(feature = "cooldown")]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rub3_wrapper::rpc;
use rub3_wrapper::session::{self, Session, VerifyError};

// Anvil's built-in account #0 (deterministic, documented, no real value).
const DEPLOYER_KEY:  &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEPLOYER_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

const PORT: u16 = 8547;

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

fn forge_create_rub3_access() -> String {
    // 9 constructor args:
    //   name, symbol, identityModel, tbaImplementation, wrapperHash,
    //   price, supplyCap, cooldownBlocks, owner
    let zero_hash = "0x0000000000000000000000000000000000000000000000000000000000000000";
    let zero_addr = "0x0000000000000000000000000000000000000000";
    let output = Command::new("forge")
        .current_dir(contracts_dir())
        .args([
            "create",
            "src/Rub3Access.sol:Rub3Access",
            "--broadcast",
            "--private-key", DEPLOYER_KEY,
            "--rpc-url", &rpc_url(),
            "--constructor-args",
            "Rub3 Test", "RUB3", "0", zero_addr, zero_hash, "0", "0", "15", DEPLOYER_ADDR,
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

    // forge prints "Deployed to: 0x..." — parse that line.
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Deployed to: ") {
            return rest.trim().to_string();
        }
    }
    panic!("could not find 'Deployed to:' in forge output:\n{stdout}");
}

fn cast_send(contract: &str, sig: &str, args: &[&str]) -> String {
    let mut cmd = Command::new("cast");
    cmd.args([
        "send", contract, sig,
        "--private-key", DEPLOYER_KEY,
        "--rpc-url", &rpc_url(),
        "--json",
    ]);
    for a in args {
        cmd.arg(a);
    }
    let output = cmd.output().expect("failed to run cast send");
    if !output.status.success() {
        panic!(
            "cast send {sig} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cast send returned non-json");
    json.get("transactionHash")
        .and_then(|v| v.as_str())
        .expect("cast send json missing transactionHash")
        .to_string()
}

fn cast_tx_block_hash(tx_hash: &str) -> String {
    let output = Command::new("cast")
        .args([
            "receipt", tx_hash,
            "blockHash",
            "--rpc-url", &rpc_url(),
        ])
        .output()
        .expect("failed to run cast receipt");
    if !output.status.success() {
        panic!(
            "cast receipt failed:\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
    String::from_utf8(output.stdout)
        .expect("cast receipt returned non-utf8")
        .trim()
        .to_string()
}

// ── The test ──────────────────────────────────────────────────────────────────

#[test]
#[ignore = "requires anvil + forge + cast on PATH"]
fn session_verify_onchain_e2e() {
    for bin in ["anvil", "forge", "cast"] {
        if !tool_available(bin) {
            eprintln!("SKIP: {bin} not found on PATH");
            return;
        }
    }

    let _anvil = start_anvil();

    // 1) Deploy Rub3Access.
    let contract = forge_create_rub3_access();
    let contract_addr: alloy::primitives::Address =
        contract.parse().expect("forge returned a malformed address");

    // Pre-purchase supply state.
    assert_eq!(rpc::supply_cap(&rpc_url(), contract_addr).unwrap(), 0,
        "supplyCap should be unlimited (0) in this fixture");
    assert_eq!(rpc::next_token_id(&rpc_url(), contract_addr).unwrap(), 0,
        "nextTokenId should be 0 before any mint");

    // 2) purchase(address) — mints token_id 0 to DEPLOYER_ADDR.
    //    `price` is 0, so msg.value = 0.
    let purchase_tx = cast_send(&contract, "purchase(address)", &[DEPLOYER_ADDR]);

    // Parse the Transfer log to recover the minted tokenId, and confirm the
    // `nextTokenId` counter advanced.
    let deployer_addr: alloy::primitives::Address = DEPLOYER_ADDR.parse().unwrap();
    let minted = rpc::mint_token_id(&rpc_url(), &purchase_tx, contract_addr, deployer_addr)
        .expect("mint_token_id should find the Transfer log");
    assert_eq!(minted, 0, "first mint should be token id 0");
    assert_eq!(rpc::next_token_id(&rpc_url(), contract_addr).unwrap(), 1,
        "nextTokenId should be 1 after one mint");

    // 3) activate(uint256) — records cooldown, assigns session id 1.
    let activate_tx = cast_send(&contract, "activate(uint256)", &["0"]);

    // 4) Pull the block hash the receipt recorded.
    let block_hash = cast_tx_block_hash(&activate_tx);
    assert!(block_hash.starts_with("0x") && block_hash.len() == 66,
        "unexpected block hash from cast: {block_hash}");

    // 5) Build a Session referencing the real on-chain values. Signature/nonce
    //    are irrelevant here — `verify_onchain` never touches them.
    let session = Session {
        app_id:                "com.rub3.test".into(),
        token_id:              0,
        identity:              "access".into(),
        user_id:               DEPLOYER_ADDR.into(),
        tba:                   None,
        wallet:                DEPLOYER_ADDR.into(),
        nonce:                 "00".into(),
        issued_at:             chrono::Utc::now().to_rfc3339(),
        expires_at:            Some("2099-01-01T00:00:00Z".into()),
        signature:             "0x00".into(),
        chain:                 "31337".into(),
        contract:              contract.clone(),
        activation_tx:         Some(activate_tx.clone()),
        activation_block:      None,
        activation_block_hash: Some(block_hash.clone()),
        session_id:            Some(1),
        device_pubkey:         None,
    };

    // ── Happy path ───────────────────────────────────────────────────────────
    session::verify_onchain(&session, &rpc_url())
        .expect("verify_onchain should succeed against real chain");

    // ── Tamper: wrong contract ───────────────────────────────────────────────
    let mut bad_contract = session.clone();
    bad_contract.contract = "0x0000000000000000000000000000000000000099".into();
    match session::verify_onchain(&bad_contract, &rpc_url()) {
        Err(VerifyError::ContractMismatch { .. }) => {}
        other => panic!("expected ContractMismatch, got {other:?}"),
    }

    // ── Tamper: wrong block hash ─────────────────────────────────────────────
    let mut bad_block = session.clone();
    bad_block.activation_block_hash = Some(
        "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
    );
    match session::verify_onchain(&bad_block, &rpc_url()) {
        Err(VerifyError::BlockHashMismatch { .. }) => {}
        other => panic!("expected BlockHashMismatch, got {other:?}"),
    }

    // ── Tamper: non-existent tx hash ─────────────────────────────────────────
    let mut missing_tx = session.clone();
    missing_tx.activation_tx = Some(
        "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
    );
    match session::verify_onchain(&missing_tx, &rpc_url()) {
        Err(VerifyError::ReceiptNotFound) => {}
        other => panic!("expected ReceiptNotFound, got {other:?}"),
    }
}
