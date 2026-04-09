mod license;
mod machine_id;
mod supervisor;

use clap::Parser;
use std::path::PathBuf;

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

fn main() {
    let cli = Cli::parse();

    if !cli.binary.exists() {
        eprintln!("error: binary not found: {}", cli.binary.display());
        std::process::exit(1);
    }

    // TODO: license check will go here (Phase 1.3 / 1.4)

    std::process::exit(supervisor::run(&cli.binary, &cli.args));
}
