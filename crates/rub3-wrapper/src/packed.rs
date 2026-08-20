//! What `rub3 pack` baked into this binary: the licence it checks, the chain it
//! checks on, the canonical factory it recognises, and the application it
//! carries (implementation.md §2.5).
//!
//! Every constant below has a development placeholder and a packed value. The
//! placeholder is what an ordinary `cargo build` compiles, which is why a stock
//! build never touches the chain: `CONTRACT` is the zero address, and the
//! wrapper reads that as "no contract configured". The packed value arrives as
//! a `RUB3_PACK_*` environment variable that `rub3 pack` sets on the `cargo
//! build` it runs, and `build.rs` refuses the build if any of them is
//! malformed, a placeholder, or missing from a set that has any of the others.
//!
//! **`FACTORY` is the one with no placeholder.** It is `None` on a development
//! build and `Some` on every packed one, because there is no value that could
//! stand in for "the canonical `Rub3Factory` on this chain" without being a
//! claim the binary cannot back. `rub3 pack` reads it out of
//! `contracts/deployments.json` and fails when that file says `null`, which is
//! what every entry says until launch. Baking it is what lets a wrapper tell a
//! canonical deploy from any other with no network round trip, and the address
//! is in that file rather than in this source for the same reason the code
//! registry's is (see [`crate::attest::REGISTRIES`]): one committed record, and
//! no second copy in Rust to drift away from it.

use std::path::PathBuf;

// ── The packed identity ───────────────────────────────────────────────────────

/// Reverse-DNS identifier for this application.
pub const APP_ID: &str = packed_or("com.rub3.example", option_env!("RUB3_PACK_APP_ID"));

/// ERC-721 licence contract address on the target chain.
///
/// The placeholder is the zero address, which the wrapper reads as "no contract
/// configured" and skips the on-chain ownership check for. `build.rs` rejects
/// it as a packed value.
pub const CONTRACT: &str = packed_or(
    "0x0000000000000000000000000000000000000000",
    option_env!("RUB3_PACK_CONTRACT"),
);

/// EVM chain id. 8453 = Base mainnet.
pub const CHAIN_ID: u64 = parse_u64(packed_or("8453", option_env!("RUB3_PACK_CHAIN_ID")));

/// JSON-RPC endpoint for the target chain.
pub const RPC_URL: &str = packed_or("https://mainnet.base.org", option_env!("RUB3_PACK_RPC_URL"));

/// The canonical `Rub3Factory` on [`CHAIN_ID`], or `None` on a build that
/// `rub3 pack` did not produce.
///
/// A packed binary always names one. See the module docs for why there is no
/// placeholder.
pub const FACTORY: Option<&str> = option_env!("RUB3_PACK_FACTORY");

/// Optional ENS name the developer registered for this app.
pub const DEVELOPER_ENS: Option<&str> = option_env!("RUB3_PACK_DEVELOPER_ENS");

/// Session lifetime in seconds, applied when a new session is minted. The
/// placeholder is the 7 days `architecture.md` gives as the default
/// `session_ttl_days`.
pub const SESSION_TTL_SECS: i64 = parse_i64(packed_or(
    "604800",
    option_env!("RUB3_PACK_SESSION_TTL_SECS"),
));

/// Whether `rub3 pack` produced this binary.
pub const IS_PACKED: bool = option_env!("RUB3_PACK_APP_ID").is_some();

// ── Compile-time helpers ──────────────────────────────────────────────────────

/// The packed value when there is one, the development placeholder otherwise.
const fn packed_or(placeholder: &'static str, packed: Option<&'static str>) -> &'static str {
    match packed {
        Some(value) => value,
        None => placeholder,
    }
}

/// Parses a decimal integer at compile time, so a malformed pack value is a
/// compile error rather than a runtime surprise.
///
/// `build.rs` rejects the same inputs with a better message; this is what makes
/// the constant a `u64` at all, and it is the backstop if the two ever part
/// company.
const fn parse_u64(s: &str) -> u64 {
    let bytes = s.as_bytes();
    assert!(!bytes.is_empty(), "expected a decimal integer, got nothing");
    let mut value: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let digit = bytes[i];
        assert!(digit.is_ascii_digit(), "expected a decimal integer");
        value = value * 10 + (digit - b'0') as u64;
        i += 1;
    }
    value
}

/// [`parse_u64`] for the signed constants. Nothing packed is negative, so this
/// parses the same grammar and widens.
const fn parse_i64(s: &str) -> i64 {
    let value = parse_u64(s);
    assert!(value <= i64::MAX as u64, "value does not fit in an i64");
    value as i64
}

// ── The embedded application ──────────────────────────────────────────────────

/// The application `rub3 pack` embedded, and the name to extract it under.
pub struct EmbeddedApp {
    /// File name the application is launched as, taken from the binary
    /// `rub3 pack` was pointed at. Argv[0] and anything the application prints
    /// about itself follow from it.
    pub name: &'static str,
    /// The application itself, compiled into this binary.
    pub bytes: &'static [u8],
}

#[cfg(rub3_packed_app)]
const EMBEDDED: EmbeddedApp = EmbeddedApp {
    name: env!("RUB3_PACK_APP_NAME"),
    bytes: include_bytes!(env!("RUB3_PACK_APP")),
};

/// The application this binary carries, or `None` when it carries none and
/// `--binary` names what to launch instead.
pub const fn embedded_app() -> Option<&'static EmbeddedApp> {
    #[cfg(rub3_packed_app)]
    {
        Some(&EMBEDDED)
    }
    #[cfg(not(rub3_packed_app))]
    {
        None
    }
}

/// Why an embedded application could not be written to disk.
#[derive(Debug)]
pub enum MaterialiseError {
    /// No platform data directory, and `RUB3_APP_DIR` did not name one.
    NoDataDirectory,
    /// The cache directory or the executable itself could not be written.
    Io(std::io::Error),
}

impl std::fmt::Display for MaterialiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaterialiseError::NoDataDirectory => write!(
                f,
                "cannot locate a data directory to extract the embedded application into. \
                 Set RUB3_APP_DIR to a writable directory."
            ),
            MaterialiseError::Io(e) => write!(f, "cannot extract the embedded application: {e}"),
        }
    }
}

impl std::error::Error for MaterialiseError {}

impl From<std::io::Error> for MaterialiseError {
    fn from(e: std::io::Error) -> Self {
        MaterialiseError::Io(e)
    }
}

impl EmbeddedApp {
    /// Writes the application to a cache directory and returns the path to run.
    ///
    /// The directory is keyed by the sha-256 of the payload, so a packed binary
    /// carrying a new version of the application extracts beside the old one
    /// rather than over a copy another process may be running, and re-running
    /// the same binary reuses what is already there. The write goes to a
    /// temporary file in the same directory and is renamed into place, so a
    /// half-written executable is never reachable under the final name.
    ///
    /// Path: `{data_dir}/rub3/apps/{app_id}/{sha256}/{name}`, with
    /// `$RUB3_APP_DIR` replacing `{data_dir}/rub3/apps` when it is set.
    pub fn materialise(&self, app_id: &str) -> Result<PathBuf, MaterialiseError> {
        use sha2::{Digest, Sha256};

        let base = match std::env::var_os("RUB3_APP_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => dirs::data_dir()
                .ok_or(MaterialiseError::NoDataDirectory)?
                .join("rub3")
                .join("apps"),
        };

        let digest = hex::encode(Sha256::digest(self.bytes));
        let dir = base.join(app_id).join(digest);
        let path = dir.join(self.name);
        if path.is_file() {
            return Ok(path);
        }

        std::fs::create_dir_all(&dir)?;
        let staged = dir.join(format!(".{}.{}", self.name, std::process::id()));
        std::fs::write(&staged, self.bytes)?;
        set_executable(&staged)?;
        // Two processes extracting at once both rename onto the same content.
        std::fs::rename(&staged, &path)?;
        Ok(path)
    }
}

#[cfg(unix)]
fn set_executable(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(()) // Windows takes its answer from the file extension.
}

// ── Provenance ────────────────────────────────────────────────────────────────

/// What this binary was packed to check, as the long `--version` output.
///
/// A distributable is run by somebody who did not build it, and the questions
/// they have are which licence it gates on, on which chain, and which factory
/// it treats as canonical. All three are compiled in, so answering costs no
/// network call and cannot be answered differently by a different network.
pub fn provenance() -> String {
    // clap prints this after the binary's own name, so it opens with the
    // version and nothing else.
    let mut out = format!("{}\n", env!("CARGO_PKG_VERSION"));
    if !IS_PACKED {
        out.push_str(
            "\nDevelopment build: not produced by `rub3 pack`, so the identity below is\n\
             the placeholder set in src/packed.rs.\n",
        );
    }
    out.push_str(&format!("\napp id:      {APP_ID}\n"));
    out.push_str(&format!("contract:    {CONTRACT}\n"));
    out.push_str(&format!("chain id:    {CHAIN_ID}\n"));
    out.push_str(&format!("rpc:         {RPC_URL}\n"));
    out.push_str(&format!(
        "factory:     {}\n",
        FACTORY.unwrap_or("none (development build)")
    ));
    if let Some(ens) = DEVELOPER_ENS {
        out.push_str(&format!("developer:   {ens}\n"));
    }
    out.push_str(&format!("session ttl: {SESSION_TTL_SECS} seconds\n"));
    out.push_str(&format!(
        "application: {}\n",
        match embedded_app() {
            Some(app) => format!("{} embedded, {} bytes", app.name, app.bytes.len()),
            None => "none embedded; --binary names what to launch".to_string(),
        }
    ));
    out.push_str(&format!("tier:        {}\n", tier()));
    out.push_str(&format!("front doors: {}\n", front_doors()));
    out
}

/// The tier bundle this binary was built with, named as `architecture.md` ->
/// "Security Tiers" names it.
///
/// Derived from the capability flags rather than from a recorded bundle name,
/// because the flags are what the code branches on: a build that enabled them
/// by hand gets the honest answer.
fn tier() -> &'static str {
    let session = cfg!(feature = "session");
    let onchain_read = cfg!(feature = "onchain-read");
    let cooldown = cfg!(feature = "cooldown");
    let device_key = cfg!(feature = "device-key");
    match (session, onchain_read, cooldown, device_key) {
        (_, _, _, true) => "4 (hardened)",
        (_, _, true, false) => "3 (cooldown)",
        (true, true, false, false) => "2 (verified)",
        (true, false, false, false) => "1 (cached)",
        (false, false, false, false) => "0 (offline)",
        _ => "custom capability set",
    }
}

fn front_doors() -> String {
    let doors: Vec<&str> = [
        cfg!(feature = "webview").then_some("webview"),
        cfg!(feature = "headless").then_some("headless"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if doors.is_empty() {
        "none (interactive activation always fails)".to_string()
    } else {
        doors.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_development_build_names_no_factory() {
        // The placeholder identity is what an unpacked build compiles, and the
        // factory is the one field with no placeholder at all: there is no
        // address that could stand in for "the canonical factory here".
        const { assert!(!IS_PACKED, "the test build must not be a packed build") };
        assert_eq!(FACTORY, None);
        assert!(embedded_app().is_none());
    }

    #[test]
    fn the_placeholder_contract_is_the_no_contract_marker() {
        // main.rs and testing.md -> "App constants" both rely on this: a stock
        // build performs no on-chain ownership check because the zero address
        // is read as "no contract configured".
        assert_eq!(CONTRACT, "0x0000000000000000000000000000000000000000");
    }

    #[test]
    fn parses_decimal_constants_at_compile_time() {
        assert_eq!(CHAIN_ID, 8453);
        assert_eq!(SESSION_TTL_SECS, 7 * 24 * 60 * 60);
    }

    #[test]
    fn provenance_names_the_licence_this_binary_gates_on() {
        let text = provenance();
        for expected in [APP_ID, CONTRACT, RPC_URL] {
            assert!(
                text.contains(expected),
                "provenance omits {expected}:\n{text}"
            );
        }
        assert!(text.contains("8453"));
        assert!(
            text.contains("none (development build)"),
            "an unpacked build must say so rather than name a factory:\n{text}"
        );
    }
}
