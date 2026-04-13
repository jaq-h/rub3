mod helpers;

use std::process::Command;

const APP_ID: &str = "com.rub3.example";
const TOKEN_ID: u64 = 1;
const STATIC_PRIVKEY_HEX: &str =
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

fn license_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("licenses");
    let key = k256::ecdsa::SigningKey::from_slice(&hex::decode(STATIC_PRIVKEY_HEX).unwrap()).unwrap();
    let address = helpers::verifying_key_to_address(key.verifying_key());
    let sig = helpers::sign_activation(&key, APP_ID, TOKEN_ID);
    helpers::create_license_json(&dir, APP_ID, TOKEN_ID, &address, &sig);
    tmp
}

#[cfg(target_os = "macos")]
const TRUE_BIN: &str = "/usr/bin/true";
#[cfg(not(target_os = "macos"))]
const TRUE_BIN: &str = "/bin/true";

#[cfg(target_os = "macos")]
const FALSE_BIN: &str = "/usr/bin/false";
#[cfg(not(target_os = "macos"))]
const FALSE_BIN: &str = "/bin/false";

#[test]
fn runs_child_and_exits_zero() {
    let tmp = license_dir();
    let status = Command::new(helpers::wrapper_bin())
        .args(["--binary", TRUE_BIN])
        .env("RUB3_LICENSE_DIR", tmp.path().join("licenses"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn propagates_nonzero_exit_code() {
    let tmp = license_dir();
    let status = Command::new(helpers::wrapper_bin())
        .args(["--binary", FALSE_BIN])
        .env("RUB3_LICENSE_DIR", tmp.path().join("licenses"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn passes_args_to_child() {
    let tmp = license_dir();
    let output = Command::new(helpers::wrapper_bin())
        .args(["--binary", "/bin/echo", "--", "hello", "rub3"])
        .env("RUB3_LICENSE_DIR", tmp.path().join("licenses"))
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"hello rub3\n");
}

#[test]
fn errors_on_missing_binary() {
    let status = Command::new(helpers::wrapper_bin())
        .args(["--binary", "/nonexistent/binary"])
        .status()
        .unwrap();
    assert_ne!(status.code(), Some(0));
}
