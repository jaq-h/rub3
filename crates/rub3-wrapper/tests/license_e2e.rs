mod helpers;

use std::process::Command;

use k256::ecdsa::SigningKey;
use rub3_wrapper::license::{self, LicenseProof};

const APP_ID: &str = "com.rub3.example";
const TOKEN_ID: u64 = 1;

// Deterministic test keypair (32 bytes, hex-encoded).
// This is a throwaway key used only for testing — it holds no value.
const STATIC_PRIVKEY_HEX: &str =
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

fn static_signing_key() -> SigningKey {
    let bytes = hex::decode(STATIC_PRIVKEY_HEX).unwrap();
    SigningKey::from_slice(&bytes).unwrap()
}

fn static_address() -> String {
    helpers::verifying_key_to_address(static_signing_key().verifying_key())
}

fn static_signature() -> String {
    helpers::sign_activation(&static_signing_key(), APP_ID, TOKEN_ID)
}

// ── Static tests ─────────────────────────────────────────────────────────────

#[test]
fn static_license_verifies() {
    let proof = LicenseProof {
        app_id: APP_ID.to_string(),
        token_id: TOKEN_ID,
        wallet_address: static_address(),
        paid_by: None,
        signature: static_signature(),
        activated_at: "2026-01-01T00:00:00Z".to_string(),
        chain: "base".to_string(),
        contract: "0x0000000000000000000000000000000000000000".to_string(),
    };

    license::verify(&proof).expect("static license should verify");
}

#[test]
fn static_license_loads_and_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let license_dir = tmp.path().join("licenses");

    helpers::create_license_json(
        &license_dir,
        APP_ID,
        TOKEN_ID,
        &static_address(),
        &static_signature(),
    );

    std::env::set_var("RUB3_LICENSE_DIR", &license_dir);
    let proof = rub3_wrapper::store::load_proof(APP_ID).expect("load failed");
    std::env::remove_var("RUB3_LICENSE_DIR");

    license::verify(&proof).expect("loaded proof should verify");
}

#[test]
fn static_wrapper_runs_with_valid_license() {
    let tmp = tempfile::tempdir().unwrap();
    let license_dir = tmp.path().join("licenses");

    helpers::create_license_json(
        &license_dir,
        APP_ID,
        TOKEN_ID,
        &static_address(),
        &static_signature(),
    );

    let output = Command::new(helpers::wrapper_bin())
        .args(["--binary", "/bin/echo", "--", "hello"])
        .env("RUB3_LICENSE_DIR", &license_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "wrapper should exit 0");
    assert_eq!(output.stdout, b"hello\n");
}

// ── Dynamic tests ────────────────────────────────────────────────────────────

#[test]
fn dynamic_wallet_generates_valid_signature() {
    let (signing_key, address) = helpers::generate_wallet();
    let signature = helpers::sign_activation(&signing_key, APP_ID, TOKEN_ID);

    let proof = LicenseProof {
        app_id: APP_ID.to_string(),
        token_id: TOKEN_ID,
        wallet_address: address,
        paid_by: None,
        signature,
        activated_at: "2026-01-01T00:00:00Z".to_string(),
        chain: "base".to_string(),
        contract: "0x0000000000000000000000000000000000000000".to_string(),
    };

    license::verify(&proof).expect("dynamic license should verify");
}

#[test]
fn dynamic_license_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let license_dir = tmp.path().join("licenses");

    let (signing_key, address) = helpers::generate_wallet();
    let signature = helpers::sign_activation(&signing_key, APP_ID, TOKEN_ID);

    helpers::create_license_json(&license_dir, APP_ID, TOKEN_ID, &address, &signature);

    std::env::set_var("RUB3_LICENSE_DIR", &license_dir);
    let proof = rub3_wrapper::store::load_proof(APP_ID).expect("load failed");
    std::env::remove_var("RUB3_LICENSE_DIR");

    license::verify(&proof).expect("round-tripped dynamic proof should verify");
}

#[test]
fn dynamic_wrapper_runs_with_fresh_license() {
    let tmp = tempfile::tempdir().unwrap();
    let license_dir = tmp.path().join("licenses");

    let (signing_key, address) = helpers::generate_wallet();
    let signature = helpers::sign_activation(&signing_key, APP_ID, TOKEN_ID);

    helpers::create_license_json(&license_dir, APP_ID, TOKEN_ID, &address, &signature);

    let output = Command::new(helpers::wrapper_bin())
        .args(["--binary", "/bin/echo", "--", "dynamic-ok"])
        .env("RUB3_LICENSE_DIR", &license_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "wrapper should exit 0");
    assert_eq!(output.stdout, b"dynamic-ok\n");
}

// ── SIGTERM forwarding ───────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn wrapper_forwards_sigterm() {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let license_dir = tmp.path().join("licenses");

    helpers::create_license_json(
        &license_dir,
        APP_ID,
        TOKEN_ID,
        &static_address(),
        &static_signature(),
    );

    let mut child = Command::new(helpers::wrapper_bin())
        .args(["--binary", "/bin/sleep", "--", "300"])
        .env("RUB3_LICENSE_DIR", &license_dir)
        .spawn()
        .unwrap();

    std::thread::sleep(Duration::from_secs(1));

    let pid = Pid::from_raw(child.id() as i32);
    signal::kill(pid, Signal::SIGTERM).expect("failed to send SIGTERM");

    let status = child.wait().unwrap();
    assert!(!status.success(), "wrapper should exit non-zero after SIGTERM");
}
