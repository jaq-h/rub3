use clap::Parser;
use std::path::PathBuf;

use rub3_wrapper::activation;

// ── App configuration ─────────────────────────────────────────────────────────
//
// These constants are placeholders for the POC.
// `rub3 pack` (§2.5) will inject them at build time from the developer's
// config, embedding the correct values for each distributed binary.

/// Reverse-DNS identifier for this application.
const APP_ID: &str = "com.rub3.example";

/// ERC-721 license contract address on the target chain.
const CONTRACT: &str = "0x0000000000000000000000000000000000000000";

/// EVM chain ID. 8453 = Base mainnet.
const CHAIN_ID: u64 = 8453;

/// JSON-RPC endpoint for the target chain.
const RPC_URL: &str = "https://mainnet.base.org";

/// Optional ENS name the developer registered for this app.
/// Set to None if the developer has not registered an ENS name.
const DEVELOPER_ENS: Option<&str> = None;

/// Session lifetime (seconds) applied when a new tier-3 session is minted.
/// 7 days matches the default `session_ttl_days` from `architecture.md`.
const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rub3-wrapper",
    about = "rub3 license wrapper",
    after_help = activation::EXIT_CODE_HELP,
)]
struct Cli {
    /// Path to the binary to launch
    #[arg(long)]
    binary: PathBuf,

    /// Activate without a window: read a signer from the environment, purchase
    /// and activate on-chain as needed, sign the session locally, then launch.
    /// Requires a build with the `headless` feature; exits 18 otherwise.
    #[arg(long)]
    headless: bool,

    /// Activate this specific token id. Headless only. Without it the flow
    /// uses the lowest-numbered token the signer holds, or purchases one.
    #[arg(long, requires = "headless")]
    token_id: Option<u64>,

    /// Arguments to pass through to the wrapped binary
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    if !cli.binary.exists() {
        eprintln!("error: binary not found: {}", cli.binary.display());
        std::process::exit(activation::EXIT_GENERIC);
    }

    if cli.headless {
        if let Err(code) = run_headless(cli.token_id) {
            std::process::exit(code);
        }
    } else if let Err(e) = rub3_wrapper::ensure(
        APP_ID,
        CONTRACT,
        CHAIN_ID,
        RPC_URL,
        DEVELOPER_ENS.map(str::to_string),
        SESSION_TTL_SECS,
    ) {
        eprintln!("error: {e}");
        // A headless-only build has exactly one door, and it is opt-in: signing
        // and broadcasting on the operator's behalf should never happen because
        // a flag was forgotten.
        #[cfg(all(feature = "headless", not(feature = "webview")))]
        eprintln!("hint: this build activates headlessly - re-run with --headless");
        std::process::exit(activation::EXIT_GENERIC);
    }

    std::process::exit(rub3_wrapper::supervisor_run(&cli.binary, &cli.args));
}

// ── Headless ──────────────────────────────────────────────────────────────────

/// Runs the agent activation path. `Err(code)` is the process exit code to use;
/// every code is documented in [`activation::EXIT_CODE_HELP`].
#[cfg(feature = "headless")]
fn run_headless(token_id: Option<u64>) -> Result<(), i32> {
    use rub3_wrapper::activation::{ensure_headless, HeadlessContext, HeadlessError};
    use rub3_wrapper::signer::resolve_signer;

    let signer = resolve_signer().map_err(|e| {
        let e = HeadlessError::Signer(e);
        report(&e);
        e.exit_code()
    })?;

    let ctx = HeadlessContext {
        app_id: APP_ID.to_string(),
        contract: CONTRACT.to_string(),
        chain_id: CHAIN_ID,
        rpc_url: RPC_URL.to_string(),
        session_ttl_secs: SESSION_TTL_SECS,
        token_id,
    };

    match ensure_headless(signer.as_ref(), &ctx) {
        Ok((session, outcome)) => {
            // One line, parseable, no key material - the signer's address is
            // public by construction.
            eprintln!(
                "rub3: {outcome:?} token_id={} wallet={} signer={}",
                session.token_id,
                session.wallet,
                signer.source(),
            );
            Ok(())
        }
        Err(e) => {
            report(&e);
            Err(e.exit_code())
        }
    }
}

/// Prints a failure as one human line plus, when the failure has structured
/// parameters, one `key=value` line an orchestrator can parse.
#[cfg(feature = "headless")]
fn report(e: &rub3_wrapper::activation::HeadlessError) {
    eprintln!("error: {e}");
    if let Some(detail) = e.machine_detail() {
        eprintln!("rub3-detail: {detail}");
    }
}

#[cfg(not(feature = "headless"))]
fn run_headless(_token_id: Option<u64>) -> Result<(), i32> {
    eprintln!(
        "error: --headless requires a build with the `headless` feature \
         (rebuild with --features headless)"
    );
    Err(activation::EXIT_HEADLESS_UNSUPPORTED)
}
