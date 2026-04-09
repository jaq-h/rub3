mod activation;
mod license;
mod machine_id;
mod rpc;
mod store;
mod supervisor;
mod webview;

use clap::Parser;
use std::path::PathBuf;

// ── App configuration ─────────────────────────────────────────────────────────
//
// These constants are placeholders for the POC.
// Phase 2.1 (deotp pack) will inject them at build time from the developer's
// config, embedding the correct values for each distributed binary.

/// Reverse-DNS identifier for this application.
const APP_ID: &str = "com.deotp.example";

/// ERC-721 license contract address on the target chain.
const CONTRACT: &str = "0x0000000000000000000000000000000000000000";

/// EVM chain ID. 8453 = Base mainnet.
const CHAIN_ID: u64 = 8453;

/// JSON-RPC endpoint for the target chain.
const RPC_URL: &str = "https://mainnet.base.org";

/// Optional ENS name the developer registered for this app.
/// Set to None if the developer has not registered an ENS name.
const DEVELOPER_ENS: Option<&str> = None;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "deotp-wrapper", about = "deotp license wrapper")]
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

    if let Err(e) = activation::ensure(
        APP_ID,
        CONTRACT,
        CHAIN_ID,
        RPC_URL,
        DEVELOPER_ENS.map(str::to_string),
    ) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }

    std::process::exit(supervisor::run(&cli.binary, &cli.args));
}
