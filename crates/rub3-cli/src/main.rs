//! The `rub3` command.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

use rub3_cli::deploy::{self, DeployArgs, DeployPlan};
use rub3_cli::deployments::{Manifest, ManifestError};
use rub3_cli::pack::{self, PackArgs, PackError, PackPlan};
use rub3_cli::repo::Repo;

/// Nothing could be done because the command line asked for something
/// impossible or contradictory.
const EXIT_USAGE: i32 = 1;
/// No canonical `Rub3Factory` is published for the chain asked about. Its own
/// code because it is the one failure that is nobody's mistake: it is what
/// every chain says until launch, and an orchestrator should be able to tell it
/// from a typo without reading the message.
const EXIT_NO_CANONICAL_FACTORY: i32 = 2;
/// The tool this command drives - cargo, or forge - failed.
const EXIT_TOOL_FAILED: i32 = 3;

const EXIT_CODE_HELP: &str = "\
Exit codes:
  0   done
  1   the command line asked for something impossible or contradictory
  2   no canonical Rub3Factory is published for that chain
  3   the build (cargo) or the deploy (forge) failed";

#[derive(Parser)]
#[command(
    name = "rub3",
    version,
    about = "Pack a rub3-wrapped distributable, and deploy the licence contract it checks",
    after_help = EXIT_CODE_HELP,
)]
struct Cli {
    /// The rub3 checkout to work in. Defaults to RUB3_REPO_ROOT, or the first
    /// checkout at or above the working directory.
    #[arg(long, global = true, value_name = "PATH")]
    repo_root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a single distributable binary: the wrapper, the application it
    /// gates, and the configuration, compiled into one file.
    Pack(Box<PackArgs>),
    /// Deploy a licence contract, through the canonical Rub3Factory by default.
    Deploy(Box<DeployArgs>),
}

fn main() {
    let cli = Cli::parse();

    let repo = match Repo::resolve(cli.repo_root) {
        Ok(repo) => repo,
        Err(e) => fail(&e.to_string(), EXIT_USAGE),
    };
    let manifest = match Manifest::read(repo.root()) {
        Ok(manifest) => manifest,
        Err(e) => fail(&e.to_string(), exit_code_for(&e)),
    };

    match cli.command {
        Command::Pack(args) => run_pack(&args, &repo, &manifest),
        Command::Deploy(args) => run_deploy(&args, &repo, &manifest),
    }
}

fn run_pack(args: &PackArgs, repo: &Repo, manifest: &Manifest) -> ! {
    let plan = match PackPlan::resolve(args, manifest) {
        Ok(plan) => plan,
        Err(e) => {
            let code = match &e {
                PackError::Manifest(e) => exit_code_for(e),
                PackError::Config(_) | PackError::Io { .. } => EXIT_USAGE,
                PackError::Build(_) => EXIT_TOOL_FAILED,
            };
            fail(&e.to_string(), code)
        }
    };

    print!("{}", plan.render());
    if args.dry_run {
        println!("\nDry run: nothing was built.");
        std::process::exit(0);
    }

    println!();
    let outcome = match pack::execute(&plan, repo) {
        Ok(outcome) => outcome,
        Err(e) => fail(&e.to_string(), EXIT_TOOL_FAILED),
    };

    println!(
        "\nPacked {} ({} bytes)",
        outcome.output.display(),
        outcome.bytes
    );
    println!("sha-256: 0x{}", outcome.sha256);
    println!(
        "\nThat hash is this platform's wrapper hash. Seed it into the licence contract at \
         deploy with `rub3 deploy --wrapper-hash 0x{}`, or add it to a deployed one with \
         `addWrapperHash`; the set is append-only and takes one hash per platform.",
        outcome.sha256
    );
    std::process::exit(0);
}

fn run_deploy(args: &DeployArgs, repo: &Repo, manifest: &Manifest) -> ! {
    let plan = match DeployPlan::resolve(args, manifest) {
        Ok(plan) => plan,
        Err(e) => {
            let code = match &e {
                deploy::DeployError::Manifest(e) => exit_code_for(e),
                deploy::DeployError::Config(_) => EXIT_USAGE,
                deploy::DeployError::Forge(_) => EXIT_TOOL_FAILED,
            };
            fail(&e.to_string(), code)
        }
    };

    print!("{}", plan.render());
    if args.dry_run {
        println!("\nDry run: forge was not run.");
        std::process::exit(0);
    }

    println!();
    match deploy::execute(&plan, repo) {
        Ok(()) => std::process::exit(0),
        Err(e) => fail(&e.to_string(), EXIT_TOOL_FAILED),
    }
}

/// The one failure with its own exit code, so an orchestrator can branch on
/// "nothing is deployed yet" without parsing English.
fn exit_code_for(e: &ManifestError) -> i32 {
    match e {
        ManifestError::NoCanonicalFactory { .. } => EXIT_NO_CANONICAL_FACTORY,
        _ => EXIT_USAGE,
    }
}

fn fail(message: &str, code: i32) -> ! {
    eprintln!("error: {message}");
    std::process::exit(code);
}
