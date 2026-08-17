//! End-to-end proof of the SDK channel (`implementation.md` §3.5): a real
//! wrapper process launches a real application that links the `rub3` crate, and
//! the application's own stdout is what is asserted.
//!
//! The application is `rub3-sdk-probe`, which prints `key=value` lines for
//! exactly what the SDK handed it. Nothing here reaches into the wrapper's
//! internals: every assertion is on what a wrapped application observes, which
//! is the only claim §3.5 makes.
//!
//! The negative case - no wrapper at all - is here too, and it is the same
//! binary run without one.
#![cfg(feature = "sdk")]

mod helpers;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const APP_ID: &str = "com.rub3.example";
const TOKEN_ID: u64 = 1;

/// Deterministic throwaway key, as used by the sibling suites. Holds no value.
const STATIC_PRIVKEY_HEX: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

fn probe_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rub3-sdk-probe")
}

fn static_key() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&hex::decode(STATIC_PRIVKEY_HEX).unwrap()).unwrap()
}

/// A temp directory holding a valid legacy licence proof, which is what makes a
/// wrapper launch succeed in every tier bundle without a network or a window.
fn licensed() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let key = static_key();
    let address = helpers::verifying_key_to_address(key.verifying_key());
    let signature = helpers::sign_activation(&key, APP_ID, TOKEN_ID);
    helpers::create_license_json(
        &tmp.path().join("licenses"),
        APP_ID,
        TOKEN_ID,
        &address,
        &signature,
    );
    tmp
}

/// An empty session directory inside `root`.
///
/// The session fast path runs before the legacy proof path in any bundle that
/// compiles it, and it resolves `~/.rub3/sessions` when `RUB3_SESSION_DIR` is
/// unset. A session left there by a developer's own run would then serve a
/// launch these tests seeded a legacy proof for, so the tests that name the
/// legacy proof have to own an empty directory rather than inherit a machine's.
fn empty_sessions(root: &Path) -> PathBuf {
    let dir = root.join("sessions");
    std::fs::create_dir_all(&dir).expect("the session directory should be creatable");
    dir
}

/// Launches `args` under a real wrapper process holding a valid legacy proof.
fn wrapped(license_dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(helpers::wrapper_bin());
    cmd.arg("--binary").arg(args[0]);
    if args.len() > 1 {
        cmd.arg("--").args(&args[1..]);
    }
    cmd.env("RUB3_LICENSE_DIR", license_dir.join("licenses"))
        .env("RUB3_SESSION_DIR", empty_sessions(license_dir))
        .output()
        .expect("the wrapper should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// One `key=value` line from the probe's output.
fn field(out: &str, key: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .map(str::to_string)
}

// ── The heartbeat, both ways ──────────────────────────────────────────────────

/// The positive case of `rub3::heartbeat()`: an application launched by a live
/// wrapper sees a live wrapper.
#[test]
fn an_application_launched_by_the_wrapper_sees_a_live_heartbeat() {
    let tmp = licensed();
    let out = wrapped(tmp.path(), &[probe_bin(), "heartbeat"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "the probe should exit 0.\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out),
    );
    assert_eq!(field(&stdout(&out), "heartbeat").as_deref(), Some("ok"));
}

/// The documented failure with no wrapper: `rub3::heartbeat()` panics, so the
/// process exits 101, and the panic text says both what happened and how to fix
/// it. This is the assertion behind the crate documentation's claim.
#[test]
fn an_application_launched_without_a_wrapper_panics_with_the_documented_failure() {
    let out = Command::new(probe_bin())
        .arg("heartbeat")
        // Cleared explicitly: the test binary's own environment must not decide
        // the outcome of the no-wrapper case.
        .env_remove("RUB3_SDK_SOCKET")
        .output()
        .expect("the probe should run");

    assert_eq!(
        out.status.code(),
        Some(101),
        "a Rust panic exits 101.\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out),
    );
    let text = stderr(&out);
    assert!(
        text.contains("RUB3_SDK_SOCKET"),
        "the panic should name the variable it looked for: {text}"
    );
    assert!(
        text.contains("rub3-wrapper --binary"),
        "the panic should say how to launch correctly: {text}"
    );
    assert!(
        !text.contains("heartbeat=ok"),
        "nothing should have reported success: {text}"
    );
}

/// The same absence, reported rather than panicked, which is what
/// `try_heartbeat` exists for.
#[test]
fn without_a_wrapper_the_reported_failure_is_not_wrapped() {
    let out = Command::new(probe_bin())
        .arg("try")
        .env_remove("RUB3_SDK_SOCKET")
        .output()
        .expect("the probe should run");

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        field(&stdout(&out), "error_kind").as_deref(),
        Some("not_wrapped"),
        "stdout: {}",
        stdout(&out)
    );
}

/// An address that points at nothing is a different failure from no address at
/// all, and an application developer needs to be able to tell them apart: the
/// first is a wrapper that died, the second is a binary run directly.
#[test]
fn an_address_pointing_at_nothing_reports_a_dead_wrapper_rather_than_no_wrapper() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(probe_bin())
        .arg("try")
        .env("RUB3_SDK_SOCKET", tmp.path().join("gone.sock"))
        .output()
        .expect("the probe should run");

    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        field(&stdout(&out), "error_kind").as_deref(),
        Some("unreachable"),
        "stdout: {}",
        stdout(&out)
    );
}

// ── What a legacy-proof launch reports ────────────────────────────────────────

/// A launch served from the legacy `LicenseProof` has a heartbeat and no
/// session. The proof predates the identity model, so there is no `user_id` to
/// report, and synthesising one from the wallet would invent an identity claim
/// the wrapper never verified.
#[test]
fn a_launch_served_from_a_legacy_proof_reports_no_session() {
    let tmp = licensed();
    let out = wrapped(tmp.path(), &[probe_bin(), "try"]);
    let text = stdout(&out);

    assert_eq!(field(&text, "heartbeat").as_deref(), Some("ok"), "{text}");
    assert_eq!(
        field(&text, "error_kind").as_deref(),
        Some("no_session"),
        "{text}"
    );
    assert!(
        field(&text, "user_id").is_none(),
        "no user_id may be reported when there is no session: {text}"
    );
}

// ── The endpoint itself ───────────────────────────────────────────────────────

/// The address the wrapper publishes exists while the child runs and is gone
/// once the wrapper exits, so a later launch cannot be answered by a stale
/// endpoint.
#[cfg(unix)]
#[test]
fn the_endpoint_is_removed_when_the_wrapper_exits() {
    let tmp = licensed();
    let out = wrapped(
        tmp.path(),
        &[
            "/bin/sh",
            "-c",
            r#"test -S "$RUB3_SDK_SOCKET" && echo "socket=$RUB3_SDK_SOCKET""#,
        ],
    );

    let text = stdout(&out);
    let path = field(&text, "socket")
        .unwrap_or_else(|| panic!("the child should see a live socket at RUB3_SDK_SOCKET: {text}"));
    assert!(!path.is_empty());
    assert!(
        !Path::new(&path).exists(),
        "the endpoint outlived the wrapper: {path}"
    );
    assert!(
        !Path::new(&path).parent().unwrap().exists(),
        "the endpoint's directory outlived the wrapper: {path}"
    );
}

/// The socket sits in a directory only its owner can enter. That mode is the
/// access control on the channel - the name is unguessable only by accident -
/// so it is asserted rather than assumed.
#[cfg(unix)]
#[test]
fn the_endpoint_directory_is_private_to_this_user() {
    let tmp = licensed();
    let out = wrapped(
        tmp.path(),
        &[
            "/bin/sh",
            "-c",
            r#"echo "mode=$(ls -ld "$(dirname "$RUB3_SDK_SOCKET")" | cut -c1-10)""#,
        ],
    );

    let text = stdout(&out);
    assert_eq!(
        field(&text, "mode").as_deref(),
        Some("drwx------"),
        "the socket's directory should be 0700: {text}"
    );
}

/// An address left in the wrapper's own environment - a wrapper launched by a
/// wrapper, a variable still exported in a shell - must never reach the child,
/// which would otherwise talk to somebody else's channel and be answered.
#[test]
fn a_stale_address_in_the_wrappers_environment_never_reaches_the_child() {
    let tmp = licensed();
    let stale = tmp.path().join("stale.sock");

    let out = Command::new(helpers::wrapper_bin())
        .arg("--binary")
        .arg(probe_bin())
        .args(["--", "try"])
        .env("RUB3_LICENSE_DIR", tmp.path().join("licenses"))
        .env("RUB3_SESSION_DIR", empty_sessions(tmp.path()))
        .env("RUB3_SDK_SOCKET", &stale)
        .output()
        .expect("the wrapper should run");

    let text = stdout(&out);
    assert_eq!(
        field(&text, "heartbeat").as_deref(),
        Some("ok"),
        "the child should have reached this launch's channel, not the stale one: {text}\n{}",
        stderr(&out)
    );
}

// ── What a session launch reports ─────────────────────────────────────────────
//
// Sessions are minted and consumed under `cooldown` (tier 3+): `ensure`'s
// session fast path and the window's `SessionSuccess` are both gated on it, so
// below that a launch is served from the legacy proof and has no session to
// report. These two tests therefore only exist in a bundle that has one.

/// Both entry points on one launch: a live heartbeat, and the session the
/// wrapper actually launched on, field for field.
#[cfg(feature = "cooldown")]
#[test]
fn a_wrapped_application_sees_the_session_the_wrapper_launched_on() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");

    // An account-model session, so `user_id` is not the wallet: the field an
    // application keys its data on has to be observably the licence identity
    // rather than the signer.
    let user_id = "0x00000000000000000000000000000000000000bb";
    let session = helpers::create_session_json(
        &sessions,
        APP_ID,
        7,
        &static_key(),
        "account",
        user_id,
        "2099-01-01T00:00:00Z",
    );

    let out = Command::new(helpers::wrapper_bin())
        .arg("--binary")
        .arg(probe_bin())
        .args(["--", "all"])
        .env("RUB3_SESSION_DIR", &sessions)
        .output()
        .expect("the wrapper should run");

    let text = stdout(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {text}\nstderr: {}",
        stderr(&out)
    );

    assert_eq!(field(&text, "heartbeat").as_deref(), Some("ok"), "{text}");
    assert_eq!(field(&text, "app_id").as_deref(), Some(APP_ID), "{text}");
    assert_eq!(field(&text, "token_id").as_deref(), Some("7"), "{text}");
    assert_eq!(field(&text, "user_id").as_deref(), Some(user_id), "{text}");
    assert_eq!(
        field(&text, "wallet").as_deref(),
        Some(session.wallet.as_str()),
        "{text}"
    );
    assert_eq!(
        field(&text, "identity").as_deref(),
        Some("account"),
        "{text}"
    );
    assert_eq!(
        field(&text, "expires_at").as_deref(),
        Some("2099-01-01T00:00:00+00:00"),
        "{text}"
    );
    assert_eq!(field(&text, "expired").as_deref(), Some("false"), "{text}");

    assert_ne!(
        field(&text, "user_id"),
        field(&text, "wallet"),
        "an account-model session must report an identity distinct from its signer: {text}"
    );
}

/// The channel carries the six fields §3.5 names and nothing else. The session
/// signature is the one that matters: an application holding it could replay the
/// session somewhere this wrapper never launched it.
#[cfg(feature = "cooldown")]
#[test]
fn the_application_never_receives_the_session_signature_or_nonce() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let session = helpers::create_session_json(
        &sessions,
        APP_ID,
        3,
        &static_key(),
        "access",
        &helpers::verifying_key_to_address(static_key().verifying_key()),
        "2099-01-01T00:00:00Z",
    );

    let out = Command::new(helpers::wrapper_bin())
        .arg("--binary")
        .arg(probe_bin())
        .args(["--", "session"])
        .env("RUB3_SESSION_DIR", &sessions)
        .output()
        .expect("the wrapper should run");

    let text = stdout(&out);
    assert_eq!(field(&text, "token_id").as_deref(), Some("3"), "{text}");

    for (name, secret) in [
        ("signature", session.signature.as_str()),
        ("nonce", session.nonce.as_str()),
    ] {
        assert!(
            !text.contains(secret),
            "the session {name} reached the wrapped application: {text}"
        );
    }
    for absent in ["contract", "chain", "issued_at", "activation", "device"] {
        assert!(
            field(&text, absent).is_none(),
            "the application should not be told {absent}: {text}"
        );
    }
}
