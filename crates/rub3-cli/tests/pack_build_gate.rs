//! The wrapper's own gate on what `rub3 pack` injects.
//!
//! `crates/rub3-wrapper/build.rs` is the last thing between a pack value and a
//! binary somebody else runs, and it is deliberately not reachable from the CLI
//! that normally sets those values - so the only honest way to test it is to
//! run a real `cargo check` against a poisoned environment and read what cargo
//! says. That costs a compile, so these are `#[ignore]`d and run as their own
//! CI step, in the same shape as the anvil-gated suites.
//!
//! ```bash
//! cargo test -p rub3-cli --test pack_build_gate -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A `cargo check` of the leanest bundle, carrying `env` as the pack input.
///
/// tier-0 with no front door is chosen for speed: `build.rs` runs before any of
/// the crate is compiled, so the bundle decides only how much work a *passing*
/// run does.
fn check(root: &Path, env: &[(&str, &str)]) -> Output {
    // One cargo at a time. They share a target directory, and two runs with
    // different pack values would otherwise rebuild over each other's work for
    // no benefit - cargo would serialise them on its own lock anyway.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut command = Command::new(cargo);
    command
        .args([
            "check",
            "-p",
            "rub3-wrapper",
            "--no-default-features",
            "--features",
            "tier-0",
        ])
        .current_dir(root);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("cargo runs")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// A complete, valid pack environment, pointing at a file that exists.
fn valid(app: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("RUB3_PACK_APP_ID", "com.example.myapp".to_string()),
        (
            "RUB3_PACK_CONTRACT",
            "0x1234567890abcdef1234567890abcdef12345678".to_string(),
        ),
        ("RUB3_PACK_CHAIN_ID", "8453".to_string()),
        ("RUB3_PACK_RPC_URL", "https://mainnet.base.org".to_string()),
        ("RUB3_PACK_SESSION_TTL_SECS", "604800".to_string()),
        (
            "RUB3_PACK_FACTORY",
            "0xf4c70a7000000000000000000000000000000001".to_string(),
        ),
        ("RUB3_PACK_APP", app.display().to_string()),
        ("RUB3_PACK_APP_NAME", "myapp".to_string()),
    ]
}

fn with(app: &Path, key: &str, value: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = valid(app)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    match env.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value.to_string(),
        None => env.push((key.to_string(), value.to_string())),
    }
    env
}

fn run(root: &Path, env: Vec<(String, String)>) -> Output {
    let borrowed: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    check(root, &borrowed)
}

fn app_file() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"not really a binary, but it is a file").unwrap();
    file
}

#[test]
#[ignore = "runs a real cargo check"]
fn a_zero_factory_cannot_be_compiled_in() {
    let root = repo_root();
    let app = app_file();
    let output = run(
        &root,
        with(
            app.path(),
            "RUB3_PACK_FACTORY",
            "0x0000000000000000000000000000000000000000",
        ),
    );
    assert!(
        !output.status.success(),
        "the build accepted a zero factory"
    );
    assert!(
        stderr(&output).contains("zero address"),
        "{}",
        stderr(&output)
    );
}

#[test]
#[ignore = "runs a real cargo check"]
fn a_zero_contract_cannot_be_compiled_in() {
    // The zero address is the wrapper's own "no contract configured" marker, so
    // a distributable carrying it would gate on nothing while looking
    // configured.
    let root = repo_root();
    let app = app_file();
    let output = run(
        &root,
        with(
            app.path(),
            "RUB3_PACK_CONTRACT",
            "0x0000000000000000000000000000000000000000",
        ),
    );
    assert!(
        !output.status.success(),
        "the build accepted a zero contract"
    );
    assert!(
        stderr(&output).contains("zero address"),
        "{}",
        stderr(&output)
    );
}

#[test]
#[ignore = "runs a real cargo check"]
fn a_half_configured_pack_cannot_be_compiled_in() {
    let root = repo_root();
    let output = check(&root, &[("RUB3_PACK_APP_ID", "com.example.myapp")]);
    assert!(
        !output.status.success(),
        "the build accepted a partial pack"
    );
    let message = stderr(&output);
    assert!(message.contains("RUB3_PACK_FACTORY"), "{message}");
    assert!(message.contains("missing"), "{message}");
}

#[test]
#[ignore = "runs a real cargo check"]
fn a_placeholder_that_is_not_an_address_cannot_be_compiled_in() {
    let root = repo_root();
    let app = app_file();
    for placeholder in ["null", "TBD", "0xYourFactory"] {
        let output = run(&root, with(app.path(), "RUB3_PACK_FACTORY", placeholder));
        assert!(
            !output.status.success(),
            "the build accepted the placeholder {placeholder}"
        );
        assert!(
            stderr(&output).contains("is not"),
            "{placeholder}: {}",
            stderr(&output)
        );
    }
}

#[test]
#[ignore = "runs a real cargo check"]
fn a_complete_pack_environment_builds() {
    // The positive control: without it, every assertion above would still pass
    // if the gate refused everything.
    let root = repo_root();
    let app = app_file();
    let env: Vec<(String, String)> = valid(app.path())
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let output = run(&root, env);
    assert!(
        output.status.success(),
        "a valid pack environment was refused: {}",
        stderr(&output)
    );
}
