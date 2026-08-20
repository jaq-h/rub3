//! `rub3 pack`: the wrapper, the application, and the configuration, as one
//! distributable binary (implementation.md §2.5).
//!
//! A pack is one `cargo build` of `rub3-wrapper` with the app identity in the
//! environment and the application's path alongside it. The wrapper's
//! `build.rs` validates what arrives and its `src/packed.rs` compiles it in, so
//! the values are literal text in the produced binary rather than a file beside
//! it that could be edited or lost.
//!
//! **The canonical `Rub3Factory` is one of those values, and it comes out of
//! `contracts/deployments.json`.** Baking it is what lets a wrapper tell a
//! canonical deploy from any other with no network round trip, and reading it
//! from the manifest is what keeps the address out of Rust source, where a
//! second copy would rot. A `null` entry - which is every entry until launch -
//! stops the pack; see [`crate::deployments`] for why that is a refusal rather
//! than a fallback.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::deployments::{validate_address, Chain, Manifest, ManifestError, PACK_ALTERNATIVES};
use crate::repo::Repo;
use crate::tier::Tier;

/// Produce a single distributable binary: the wrapper, the application it
/// gates, and the configuration, compiled into one file.
#[derive(Debug, clap::Args)]
pub struct PackArgs {
    /// The application to wrap. Embedded in the packed binary, extracted to a
    /// cache directory on first launch, and run as a supervised child process.
    #[arg(long, value_name = "PATH")]
    pub binary: PathBuf,

    /// Reverse-DNS identifier for the application, such as com.example.myapp.
    /// Names the licence proof and the session on disk.
    #[arg(long, value_name = "ID")]
    pub app_id: String,

    /// The ERC-721 licence contract the packed binary checks ownership against.
    #[arg(long, value_name = "ADDRESS")]
    pub contract: String,

    /// Chain to check on: a name from contracts/deployments.json, such as
    /// `base`, or a chain id. A name that file does not publish is refused
    /// rather than guessed at.
    #[arg(long, value_name = "NAME|ID")]
    pub chain: String,

    /// Security tier: offline, cached, verified, cooldown or hardened (or the
    /// tier-0 .. tier-4 spelling). architecture.md -> "Security Tiers" says
    /// what each one enforces.
    #[arg(long, value_name = "TIER")]
    pub tier: String,

    /// Build the agent front door: a signer read from the environment, a
    /// session written to stdout, and no GUI dependency at all. Composes with
    /// --webview; without either, the packed binary gets the webview.
    #[arg(long)]
    pub headless: bool,

    /// Build the native activation window explicitly. This is the default when
    /// --headless is not given, so it is only needed to ask for both doors.
    #[arg(long)]
    pub webview: bool,

    /// Serve the rub3 SDK channel, so the wrapped application can ask who is
    /// running it. Needed by an application that links the `rub3` crate.
    #[arg(long)]
    pub sdk: bool,

    /// Session lifetime in days. Ignored by the offline and hardened tiers.
    #[arg(long, value_name = "DAYS", default_value_t = 7)]
    pub session_ttl: u32,

    /// Where to write the packed binary.
    #[arg(long, value_name = "PATH")]
    pub output: PathBuf,

    /// JSON-RPC endpoint the packed binary reads the chain through. Defaults to
    /// the public endpoint for the chain, which is fine for a trial and rate
    /// limited for anything else.
    #[arg(long, value_name = "URL")]
    pub rpc_url: Option<String>,

    /// ENS name the developer registered for this app, shown during activation.
    #[arg(long, value_name = "NAME")]
    pub developer_ens: Option<String>,

    /// Bake a factory address of your own instead of the canonical one.
    ///
    /// For building against a factory you deployed yourself - a local anvil or
    /// a testnet. It is not a way past a missing canonical entry: a binary
    /// packed this way makes no claim to canonicity, and saying so explicitly
    /// is the point.
    #[arg(long, value_name = "ADDRESS")]
    pub factory: Option<String>,

    /// Cross-compile for a target triple, such as
    /// aarch64-apple-darwin or x86_64-unknown-linux-gnu. The toolchain target
    /// must be installed, and the application given to --binary must already be
    /// built for it: it is embedded byte for byte, not recompiled.
    #[arg(long, value_name = "TRIPLE")]
    pub target: Option<String>,

    /// Resolve everything and print what would be built, without building it.
    #[arg(long)]
    pub dry_run: bool,
}

/// `--app-id` has to be one plain path component.
///
/// It names a directory the packed binary extracts its application into
/// (`{data_dir}/rub3/apps/{app_id}/{sha256}/{name}`) and the file its licence
/// proof is stored under, so a separator would put both somewhere else and `..`
/// would climb out of the cache directory entirely. The wrapper's `build.rs`
/// applies the same rule to the same value, since it is reachable without this
/// command.
fn check_app_id(app_id: &str) -> Result<(), String> {
    if app_id.trim().is_empty() {
        return Err(
            "--app-id is empty. It names the licence proof and the session on disk.".into(),
        );
    }
    let plain = app_id != "."
        && app_id != ".."
        && !app_id.contains('/')
        && !app_id.contains('\\')
        && !app_id.contains('\0');
    if !plain {
        return Err(format!(
            "--app-id {app_id} is not a plain name. It becomes a directory the packed binary \
             extracts its application into and the file name its licence proof is stored under, \
             so a path separator or `..` would put them outside the rub3 cache directory. Use a \
             reverse-DNS name such as com.example.myapp."
        ));
    }
    Ok(())
}

/// A resolved pack: everything the build needs, with nothing left to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackPlan {
    /// Cargo features, in the order they are passed.
    pub features: Vec<String>,
    /// Cross-compilation target, if any.
    pub target: Option<String>,
    /// The `RUB3_PACK_*` environment the build is given.
    pub env: BTreeMap<String, String>,
    /// Where the packed binary is written.
    pub output: PathBuf,
    /// The tier, for the summary.
    pub tier: Tier,
    /// The chain, for the summary.
    pub chain: Chain,
    /// Whether the factory was named on the command line rather than read from
    /// the manifest.
    pub factory_is_explicit: bool,
}

/// Why a pack could not be produced.
#[derive(Debug)]
pub enum PackError {
    /// The manifest refused, most often because no canonical factory exists.
    Manifest(ManifestError),
    /// An input is wrong or two inputs contradict each other.
    Config(String),
    /// The build failed, or produced nothing to copy.
    Build(String),
    /// A file could not be read, written or copied.
    Io {
        doing: String,
        source: std::io::Error,
    },
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackError::Manifest(e) => write!(f, "{e}"),
            PackError::Config(message) => write!(f, "{message}"),
            PackError::Build(message) => write!(f, "{message}"),
            PackError::Io { doing, source } => write!(f, "{doing}: {source}"),
        }
    }
}

impl std::error::Error for PackError {}

impl From<ManifestError> for PackError {
    fn from(e: ManifestError) -> Self {
        PackError::Manifest(e)
    }
}

/// Every `RUB3_PACK_*` variable the wrapper's `build.rs` reads.
///
/// Listed so the ones a plan does not set can be *removed* from the build's
/// environment rather than inherited. `RUB3_PACK_DEVELOPER_ENS` is the one that
/// makes this load-bearing: it is optional, so the wrapper's gate lets a build
/// through without it, and a stale one exported from an earlier pack would be
/// compiled into this distributable and shown to every licence holder during
/// activation as this app's developer.
///
/// `crates/rub3-wrapper/build.rs` holds the same list, where it decides which
/// variables are read and validated. The duplication is forced: this crate
/// stays off the wrapper's dependency path, so there is no crate the two could
/// share it from. A `RUB3_PACK_*` added there and not here would be inherited
/// from the operator's shell rather than cleared.
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

/// Public JSON-RPC endpoints, per chain id.
///
/// A convenience default, not a record of anything: an operator packing for
/// distribution should name an endpoint they control. Unknown chains have no
/// default and must be given one.
fn default_rpc_url(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        8453 => Some("https://mainnet.base.org"),
        84532 => Some("https://sepolia.base.org"),
        _ => None,
    }
}

impl PackPlan {
    /// Turns the command line into a build, refusing anything that would
    /// produce a distributable nobody should ship.
    pub fn resolve(args: &PackArgs, manifest: &Manifest) -> Result<PackPlan, PackError> {
        let tier = Tier::parse(&args.tier).map_err(PackError::Config)?;
        let chain =
            manifest.resolve_chain(&crate::deployments::ChainSelector::parse(&args.chain))?;

        if let Some(address) = &args.factory {
            validate_address(address)
                .map_err(|detail| PackError::Config(format!("--factory {detail}")))?;
        }

        validate_address(&args.contract)
            .map_err(|detail| PackError::Config(format!("--contract {detail}")))?;

        check_app_id(&args.app_id).map_err(PackError::Config)?;

        if args.session_ttl == 0 {
            return Err(PackError::Config(
                "--session-ttl 0 would mint sessions that have already expired.".into(),
            ));
        }

        // The headless front door enables session, onchain-read, onchain-write
        // and cooldown on top of whatever tier bundle is selected, and cargo
        // features are additive, so a lower tier would not survive the build.
        // Refusing beats shipping a binary whose tier is not the one asked for.
        if args.headless && tier.level() < Tier::Cooldown.level() {
            return Err(PackError::Config(format!(
                "--headless cannot be combined with the {tier} tier. The agent front door needs \
                 the on-chain writes and the cooldown that tier 3 brings, so the build would \
                 silently come out at tier 3 rather than the tier asked for. Pack it as cooldown \
                 or hardened, or drop --headless."
            )));
        }

        let app = args.binary.canonicalize().map_err(|source| PackError::Io {
            doing: format!("cannot read the application at {}", args.binary.display()),
            source,
        })?;
        if !app.is_file() {
            return Err(PackError::Config(format!(
                "--binary {} is not a file",
                args.binary.display()
            )));
        }
        let app_name = app
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                PackError::Config(format!(
                    "--binary {} has no usable file name",
                    args.binary.display()
                ))
            })?
            .to_string();

        let rpc_url = match &args.rpc_url {
            Some(url) => url.clone(),
            None => default_rpc_url(chain.id)
                .ok_or_else(|| {
                    PackError::Config(format!(
                        "no default JSON-RPC endpoint is known for chain {} ({}). Pass --rpc-url.",
                        chain.name, chain.id
                    ))
                })?
                .to_string(),
        };
        if !(rpc_url.starts_with("http://") || rpc_url.starts_with("https://")) {
            return Err(PackError::Config(format!(
                "--rpc-url {rpc_url} is not an http(s) endpoint"
            )));
        }

        let mut features = vec![tier.feature().to_string()];
        if args.headless {
            features.push("headless".into());
        }
        if args.webview || !args.headless {
            features.push("webview".into());
        }
        if args.sdk {
            features.push("sdk".into());
        }

        // The load-bearing resolution: the manifest's address, or a refusal
        // that names the chain. `--factory` is the only other way to get one,
        // and it is an explicit claim rather than a fallback.
        //
        // Last, so that every refusal above is reported as itself. Until a
        // factory is published anywhere, resolving it first would report a
        // malformed command line as the null-factory refusal, which is the one
        // signal an orchestrator reads as "nothing is deployed yet".
        let (factory, factory_is_explicit) = match &args.factory {
            Some(address) => (address.clone(), true),
            None => (
                manifest.canonical_factory(&chain, PACK_ALTERNATIVES)?,
                false,
            ),
        };

        let mut env = BTreeMap::new();
        env.insert("RUB3_PACK_APP_ID".into(), args.app_id.clone());
        env.insert("RUB3_PACK_CONTRACT".into(), args.contract.clone());
        env.insert("RUB3_PACK_CHAIN_ID".into(), chain.id.to_string());
        env.insert("RUB3_PACK_RPC_URL".into(), rpc_url);
        env.insert(
            "RUB3_PACK_SESSION_TTL_SECS".into(),
            (u64::from(args.session_ttl) * 24 * 60 * 60).to_string(),
        );
        env.insert("RUB3_PACK_FACTORY".into(), factory);
        env.insert("RUB3_PACK_APP".into(), app.display().to_string());
        env.insert("RUB3_PACK_APP_NAME".into(), app_name);
        if let Some(ens) = &args.developer_ens {
            env.insert("RUB3_PACK_DEVELOPER_ENS".into(), ens.clone());
        }

        Ok(PackPlan {
            features,
            target: args.target.clone(),
            env,
            output: args.output.clone(),
            tier,
            chain,
            factory_is_explicit,
        })
    }

    /// The `cargo build` this plan runs.
    ///
    /// `--no-default-features` is not optional: cargo features are additive, so
    /// without it the crate's own default (tier-2 plus the webview) would be
    /// enabled on top of whatever tier was asked for. `--locked` is here for
    /// the same class of reason: a packed binary's sha-256 goes on-chain as a
    /// wrapper hash, and a dependency that resolved differently between two
    /// packs would move it.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "build",
            "--locked",
            "--release",
            "-p",
            "rub3-wrapper",
            "--bin",
            "rub3-wrapper",
            "--no-default-features",
            "--features",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.push(self.features.join(","));
        if let Some(target) = &self.target {
            args.push("--target".into());
            args.push(target.clone());
        }
        args.push("--message-format".into());
        args.push("json-render-diagnostics".into());
        args
    }

    /// What this plan will do, for `--dry-run` and for the summary a real pack
    /// prints before it starts.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("app id:      {}\n", self.env["RUB3_PACK_APP_ID"]));
        out.push_str(&format!(
            "contract:    {}\n",
            self.env["RUB3_PACK_CONTRACT"]
        ));
        out.push_str(&format!(
            "chain:       {} ({})\n",
            self.chain.name, self.chain.id
        ));
        out.push_str(&format!(
            "factory:     {}{}\n",
            self.env["RUB3_PACK_FACTORY"],
            if self.factory_is_explicit {
                "  (named with --factory: not a canonical deploy)"
            } else {
                "  (canonical, from contracts/deployments.json)"
            }
        ));
        out.push_str(&format!("rpc:         {}\n", self.env["RUB3_PACK_RPC_URL"]));
        out.push_str(&format!("tier:        {}\n", self.tier));
        out.push_str(&format!("features:    {}\n", self.features.join(",")));
        out.push_str(&format!(
            "session ttl: {} seconds\n",
            self.env["RUB3_PACK_SESSION_TTL_SECS"]
        ));
        out.push_str(&format!("application: {}\n", self.env["RUB3_PACK_APP"]));
        if let Some(ens) = self.env.get("RUB3_PACK_DEVELOPER_ENS") {
            out.push_str(&format!("developer:   {ens}\n"));
        }
        if let Some(target) = &self.target {
            out.push_str(&format!("target:      {target}\n"));
        }
        out.push_str(&format!("output:      {}\n", self.output.display()));
        out.push_str(&format!(
            "build:       cargo {}\n",
            self.cargo_args().join(" ")
        ));
        let cleared: Vec<&str> = PACK_VARS
            .iter()
            .copied()
            .filter(|var| !self.env.contains_key(*var))
            .collect();
        if !cleared.is_empty() {
            out.push_str(&format!(
                "             (removed from the build's environment: {})\n",
                cleared.join(", ")
            ));
        }
        out
    }
}

/// What a completed pack produced.
pub struct PackOutcome {
    /// The packed binary.
    pub output: PathBuf,
    /// Its sha-256, which is what a licence contract's wrapper hash set holds.
    pub sha256: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// Runs a resolved plan: builds the wrapper, copies it to `--output`, and
/// fingerprints what it wrote.
pub fn execute(plan: &PackPlan, repo: &Repo) -> Result<PackOutcome, PackError> {
    let built = build(plan, repo)?;

    if let Some(parent) = plan.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| PackError::Io {
                doing: format!("cannot create {}", parent.display()),
                source,
            })?;
        }
    }
    std::fs::copy(&built, &plan.output).map_err(|source| PackError::Io {
        doing: format!(
            "cannot write {} from {}",
            plan.output.display(),
            built.display()
        ),
        source,
    })?;
    set_executable(&plan.output).map_err(|source| PackError::Io {
        doing: format!("cannot make {} executable", plan.output.display()),
        source,
    })?;

    let bytes = std::fs::read(&plan.output).map_err(|source| PackError::Io {
        doing: format!("cannot read back {}", plan.output.display()),
        source,
    })?;
    Ok(PackOutcome {
        output: plan.output.clone(),
        sha256: {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(&bytes))
        },
        bytes: bytes.len() as u64,
    })
}

/// Runs the build and returns the binary cargo wrote.
///
/// The artifact path comes from cargo's own JSON output rather than from
/// guessing at `target/release/`: a `CARGO_TARGET_DIR`, a `--target` triple or
/// a workspace layout change all move it, and a pack that copied the wrong file
/// would be a distributable with somebody else's configuration in it.
fn build(plan: &PackPlan, repo: &Repo) -> Result<PathBuf, PackError> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut command = Command::new(&cargo);
    command
        .args(plan.cargo_args())
        .current_dir(repo.root())
        .stdout(Stdio::piped());
    // Set what the plan decided, and clear the rest. A pack value the operator
    // happens to have exported is an identity nobody typed, and the optional
    // ones are the ones that would survive the wrapper's gate.
    for var in PACK_VARS {
        match plan.env.get(*var) {
            Some(value) => command.env(var, value),
            None => command.env_remove(var),
        };
    }

    let mut child = command.spawn().map_err(|source| PackError::Io {
        doing: format!("cannot run {cargo}"),
        source,
    })?;
    let stdout = child.stdout.take().expect("stdout was piped");

    let mut executable = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|source| PackError::Io {
            doing: "cannot read cargo's output".into(),
            source,
        })?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if message["reason"] == "compiler-artifact"
            && message["target"]["name"] == "rub3-wrapper"
            && message["executable"].is_string()
        {
            executable = message["executable"].as_str().map(PathBuf::from);
        }
    }

    let status = child.wait().map_err(|source| PackError::Io {
        doing: "cannot wait for cargo".into(),
        source,
    })?;
    if !status.success() {
        return Err(PackError::Build(format!(
            "the wrapper build failed ({status}). The output above is cargo's."
        )));
    }
    executable.ok_or_else(|| {
        PackError::Build(
            "the build reported success but produced no rub3-wrapper executable".into(),
        )
    })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
