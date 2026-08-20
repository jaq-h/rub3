//! Validates what `rub3 pack` (implementation.md §2.5) injects, and tells cargo
//! when a rebuild is due.
//!
//! `src/packed.rs` reads every one of these variables with `option_env!`, so
//! this script exists for two reasons and no others:
//!
//! 1. **Cargo has to know the build depends on them.** `option_env!` leaves no
//!    trace in the dependency graph, so without the `rerun-if-env-changed`
//!    lines below a second `pack` with different constants would reuse the
//!    first one's object files and ship the wrong contract address.
//! 2. **A malformed or placeholder value must not reach a distributable.** This
//!    is the last gate before the values are literal text in a binary somebody
//!    else runs, so it is the one that has to hold even when the CLI that
//!    normally sets them was not involved. The zero address is rejected on both
//!    `CONTRACT` and `FACTORY` for the reason `contracts/deployments.json`
//!    states in its own note: no value may be substituted for an address that
//!    is not known, and a packed binary claiming a canonical factory it cannot
//!    name is worse than one that refuses to build.
//!
//! Nothing here has an effect on an ordinary `cargo build`: with no
//! `RUB3_PACK_*` set, the packed identity is the placeholder set in
//! `src/packed.rs` and the binary carries no embedded application.

use std::path::Path;

/// Every variable `rub3 pack` sets, in the order the error messages list them.
const PACK_VARS: &[&str] = &[
    "RUB3_PACK_APP_ID",
    "RUB3_PACK_CONTRACT",
    "RUB3_PACK_CHAIN_ID",
    "RUB3_PACK_RPC_URL",
    "RUB3_PACK_SESSION_TTL_SECS",
    "RUB3_PACK_FACTORY",
    "RUB3_PACK_APP",
    "RUB3_PACK_APP_NAME",
    "RUB3_PACK_DEVELOPER_ENS",
];

/// The set a pack build must supply in full. `RUB3_PACK_DEVELOPER_ENS` is
/// absent on purpose: an app with no ENS name is an ordinary app.
const REQUIRED: &[&str] = &[
    "RUB3_PACK_APP_ID",
    "RUB3_PACK_CONTRACT",
    "RUB3_PACK_CHAIN_ID",
    "RUB3_PACK_RPC_URL",
    "RUB3_PACK_SESSION_TTL_SECS",
    "RUB3_PACK_FACTORY",
    "RUB3_PACK_APP",
    "RUB3_PACK_APP_NAME",
];

fn main() {
    for var in PACK_VARS {
        println!("cargo::rerun-if-env-changed={var}");
    }
    // `packed.rs` gates the `include_bytes!` of the application on this.
    println!("cargo::rustc-check-cfg=cfg(rub3_packed_app)");

    let set: Vec<&str> = PACK_VARS
        .iter()
        .copied()
        .filter(|v| std::env::var_os(v).is_some())
        .collect();
    if set.is_empty() {
        return; // An ordinary development build.
    }

    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|v| std::env::var_os(v).is_none())
        .collect();
    if !missing.is_empty() {
        fail(&format!(
            "a packed build must set every one of {}, and this one is missing {}.\n\
             A half-configured distributable would carry the placeholder identity from \
             src/packed.rs for whatever was left out. Run `rub3 pack`, which sets them together.",
            REQUIRED.join(", "),
            missing.join(", ")
        ));
    }

    check_app_id(&var("RUB3_PACK_APP_ID"));
    check_address("RUB3_PACK_CONTRACT", &var("RUB3_PACK_CONTRACT"));
    check_address("RUB3_PACK_FACTORY", &var("RUB3_PACK_FACTORY"));
    check_chain_id(&var("RUB3_PACK_CHAIN_ID"));
    check_rpc_url(&var("RUB3_PACK_RPC_URL"));
    check_session_ttl(&var("RUB3_PACK_SESSION_TTL_SECS"));
    check_app_name(&var("RUB3_PACK_APP_NAME"));
    check_app(&var("RUB3_PACK_APP"));

    println!("cargo::rustc-cfg=rub3_packed_app");
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fail(&format!("{name} is not valid UTF-8")))
}

/// The application's cache directory is named after it, and its licence proof
/// file is too, so it has to be a plain path component for the same reason
/// [`check_app_name`] does.
fn check_app_id(app_id: &str) {
    if app_id.trim().is_empty() {
        fail("RUB3_PACK_APP_ID is empty. It names the licence this binary checks and the file the proof is stored under.");
    }
    if !is_path_component(app_id) {
        fail(&format!(
            "RUB3_PACK_APP_ID={app_id} is not a plain name. It names the directory this binary \
             extracts its application into and the file its licence proof is stored under, so a \
             separator or `..` would put them outside the rub3 cache directory."
        ));
    }
}

/// Rejects anything that is not exactly 20 hex-encoded bytes, and rejects the
/// zero address outright.
///
/// The zero address is a legitimate *development* value - the wrapper reads a
/// zero `CONTRACT` as "no contract configured" and skips the ownership check -
/// which is precisely why it cannot be allowed through here. A distributable
/// that shipped it would gate on nothing at all and look configured while
/// doing it.
fn check_address(name: &str, value: &str) {
    let hex = match value.strip_prefix("0x") {
        Some(hex) => hex,
        None => fail(&format!("{name}={value} is not 0x-prefixed")),
    };
    if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        fail(&format!(
            "{name}={value} is not an address: expected 0x followed by 40 hex digits"
        ));
    }
    if hex.bytes().all(|b| b == b'0') {
        fail(&format!(
            "{name} is the zero address. Nothing may be substituted for an address that is \
             not known - see the note in contracts/deployments.json - and a distributable \
             carrying a placeholder here would ship a licence gate that checks nothing."
        ));
    }
}

fn check_chain_id(value: &str) {
    match value.parse::<u64>() {
        Ok(0) | Err(_) => fail(&format!(
            "RUB3_PACK_CHAIN_ID={value} is not a chain id: expected a positive decimal integer"
        )),
        Ok(_) => {}
    }
}

fn check_rpc_url(value: &str) {
    if !(value.starts_with("http://") || value.starts_with("https://")) {
        fail(&format!(
            "RUB3_PACK_RPC_URL={value} is not an http(s) endpoint"
        ));
    }
}

fn check_session_ttl(value: &str) {
    match value.parse::<i64>() {
        Ok(secs) if secs > 0 => {}
        _ => fail(&format!(
            "RUB3_PACK_SESSION_TTL_SECS={value} is not a session lifetime: expected a positive \
             number of seconds"
        )),
    }
}

/// The extracted application's file name, so it must be a file name.
///
/// A separator here would let the payload land outside the cache directory the
/// wrapper extracts into, and `..` would let it climb out of it.
fn check_app_name(value: &str) {
    if !is_path_component(value) {
        fail(&format!(
            "RUB3_PACK_APP_NAME={value} is not a plain file name"
        ));
    }
}

/// One path component and nothing else: no separator, no `.` or `..`, no NUL.
///
/// Every pack value that becomes part of a path on the machine the
/// distributable runs on goes through this, so the rule has one owner.
fn is_path_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

fn check_app(value: &str) {
    let path = Path::new(value);
    if !path.is_absolute() {
        fail(&format!(
            "RUB3_PACK_APP={value} is not an absolute path. It is embedded with include_bytes!, \
             which would otherwise resolve it against src/."
        ));
    }
    if !path.is_file() {
        fail(&format!("RUB3_PACK_APP={value} is not a file"));
    }
    // The payload is part of the compiled output, so a changed application is a
    // changed binary.
    println!("cargo::rerun-if-changed={value}");
}

fn fail(message: &str) -> ! {
    println!("cargo::error=rub3 pack: {message}");
    std::process::exit(1);
}
