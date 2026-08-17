//! `rub3-docs-mcp`: serve this repository's derived facts over MCP on stdio.
//!
//! Wire it into a coding agent as a stdio MCP server. For Claude Code:
//!
//! ```bash
//! claude mcp add rub3-docs -- cargo run --quiet -p rub3-docs-mcp
//! ```
//!
//! or, against a built binary from anywhere on the machine:
//!
//! ```bash
//! claude mcp add rub3-docs -- /path/to/rub3-docs-mcp --repo-root /path/to/rub3
//! ```
//!
//! stdout carries the JSON-RPC stream and nothing else; diagnostics go to
//! stderr. Printing anything else on stdout would corrupt the protocol, which is
//! why the resolved root is logged to stderr at startup rather than announced.

use std::path::PathBuf;

use clap::Parser;
use rub3_docs_mcp::{DocsServer, Repo};

#[derive(Parser)]
#[command(
    about = "Serve rub3's documents, contract ABIs and Rust API over MCP (stdio)",
    long_about = "Serves facts derived from a rub3 checkout: the Markdown documents, the contract \
                  ABIs forge built, and the workspace's public Rust signatures sliced out of their \
                  source. Nothing served is transcribed, and a fact that cannot be derived is an \
                  error rather than a guess.\n\nThe checkout is resolved from --repo-root, then \
                  RUB3_REPO_ROOT, then by walking up from the working directory, then from the \
                  tree this binary was compiled in."
)]
struct Cli {
    /// The rub3 checkout to serve. Defaults to RUB3_REPO_ROOT, then a walk up
    /// from the working directory, then the checkout this binary was built in.
    #[arg(long, value_name = "PATH")]
    repo_root: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let repo = match Repo::resolve_from_env(cli.repo_root) {
        Ok(repo) => repo,
        Err(error) => {
            eprintln!("rub3-docs-mcp: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    eprintln!("rub3-docs-mcp: serving {}", repo.root().display());

    let service =
        match rmcp::ServiceExt::serve(DocsServer::new(repo), rmcp::transport::stdio()).await {
            Ok(service) => service,
            Err(error) => {
                eprintln!("rub3-docs-mcp: could not start on stdio: {error}");
                return std::process::ExitCode::FAILURE;
            }
        };
    if let Err(error) = service.waiting().await {
        eprintln!("rub3-docs-mcp: stopped: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
