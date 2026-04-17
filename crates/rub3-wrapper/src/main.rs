use clap::Parser;
use std::path::PathBuf;

// ── App configuration ─────────────────────────────────────────────────────────
//
// These constants are placeholders for the POC.
// Phase 2.1 (rub3 pack) will inject them at build time from the developer's
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
#[command(name = "rub3-wrapper", about = "rub3 license wrapper")]
struct Cli {
    /// Path to the binary to launch
    #[arg(long)]
    binary: PathBuf,

    /// Arguments to pass through to the wrapped binary
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    if !cli.binary.exists() {
        eprintln!("error: binary not found: {}", cli.binary.display());
        std::process::exit(1);
    }

    if let Err(e) = rub3_wrapper::ensure(
        APP_ID,
        CONTRACT,
        CHAIN_ID,
        RPC_URL,
        DEVELOPER_ENS.map(str::to_string),
        SESSION_TTL_SECS,
    ) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }

    std::process::exit(rub3_wrapper::supervisor_run(&cli.binary, &cli.args));
}
