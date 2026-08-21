use clap::Parser;
use std::path::PathBuf;

use rub3_wrapper::activation;
use rub3_wrapper::packed;

// ── App configuration ─────────────────────────────────────────────────────────
//
// `rub3 pack` (implementation.md §2.5) injects the app identity at build time;
// an unpacked build compiles the placeholders. Both live in `packed`, which is
// also where the canonical `Rub3Factory` this binary recognises comes from and
// where the embedded application is kept.

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rub3-wrapper",
    about = "rub3 license wrapper",
    version,
    // `--version` answers which licence this binary gates on, on which chain,
    // and which factory it treats as canonical - the questions somebody who did
    // not build it has, all of them answerable without a network call.
    long_version = provenance(),
    after_help = activation::EXIT_CODE_HELP,
)]
struct Cli {
    /// Path to the binary to launch. A packed build carries its own
    /// application and rejects this flag.
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Activate without a window: read a signer from the environment, purchase
    /// and activate on-chain as needed, sign the session locally, then launch.
    /// Requires a build with the `headless` feature; exits 18 otherwise.
    #[arg(long)]
    headless: bool,

    /// Activate this specific token id. Headless only. Without it the flow
    /// uses the lowest-numbered token the signer holds, or purchases one.
    #[arg(long, requires = "headless")]
    token_id: Option<u64>,

    /// Hand this machine's seat back and exit without launching anything.
    ///
    /// The teardown half of concurrent seats (§3.4): a licence grants a fixed
    /// number of live sessions, and an instance being retired should return its
    /// seat now rather than after the contract's session TTL. Releases only the
    /// session cached on this machine, never anybody else's. Headless only.
    #[arg(long, requires = "headless", conflicts_with = "binary")]
    release_seat: bool,

    /// Arguments to pass through to the wrapped binary
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

/// [`packed::provenance`] as the `'static` string clap needs, built once.
fn provenance() -> &'static str {
    static PROVENANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PROVENANCE.get_or_init(packed::provenance).as_str()
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    // Teardown, not a launch: it resolves no binary, opens no window and
    // extracts nothing, because nothing is going to run.
    if cli.release_seat {
        std::process::exit(run_release_seat(cli.token_id));
    }

    // Resolved before activation so a launch that cannot happen opens no
    // activation window and signs nothing.
    let target = match LaunchTarget::resolve(cli.binary.as_deref()) {
        Ok(target) => target,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(activation::EXIT_GENERIC);
        }
    };

    // Whichever door authorises the launch hands back what the SDK channel will
    // report to the wrapped application (§3.5).
    let launch = if cli.headless {
        match run_headless(cli.token_id) {
            Ok(launch) => launch,
            Err(code) => std::process::exit(code),
        }
    } else {
        match rub3_wrapper::ensure(
            packed::APP_ID,
            packed::CONTRACT,
            packed::CHAIN_ID,
            packed::RPC_URL,
            packed::DEVELOPER_ENS.map(str::to_string),
            packed::SESSION_TTL_SECS,
        ) {
            Ok(launch) => launch,
            Err(e) => {
                eprintln!("error: {e}");
                // A headless-only build has exactly one door, and it is opt-in:
                // signing and broadcasting on the operator's behalf should never
                // happen because a flag was forgotten.
                #[cfg(all(feature = "headless", not(feature = "webview")))]
                eprintln!("hint: this build activates headlessly - re-run with --headless");
                std::process::exit(activation::EXIT_GENERIC);
            }
        }
    };

    // Only now, with the launch authorised: an embedded application is written
    // to disk after the licence check and never before it, so a failed
    // activation leaves nothing extracted for the caller to run directly.
    let binary = match target.path() {
        Ok(path) => path,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(activation::EXIT_GENERIC);
        }
    };

    std::process::exit(rub3_wrapper::supervisor_run(&binary, &cli.args, &launch));
}

/// What this launch runs: the application `rub3 pack` embedded, or the one
/// `--binary` names.
enum LaunchTarget {
    /// A packed build's own application, still compiled into this binary.
    Embedded(&'static packed::EmbeddedApp),
    /// A path on this machine, checked to exist.
    External(PathBuf),
}

impl LaunchTarget {
    /// Decides what to launch, and refuses anything that cannot be launched.
    ///
    /// A packed build refuses `--binary` rather than honouring it: the point of
    /// packing is a distributable that is one file, and a wrapper that would
    /// launch a different application on request is a licence gate wrapped
    /// around whatever the caller felt like running.
    fn resolve(requested: Option<&std::path::Path>) -> Result<LaunchTarget, String> {
        match (packed::embedded_app(), requested) {
            (Some(_), Some(path)) => Err(format!(
                "this build carries its own application, so --binary {} cannot be honoured",
                path.display()
            )),
            (Some(app), None) => Ok(LaunchTarget::Embedded(app)),
            (None, Some(path)) if path.exists() => Ok(LaunchTarget::External(path.to_path_buf())),
            (None, Some(path)) => Err(format!("binary not found: {}", path.display())),
            (None, None) => Err(
                "no application to launch: this build embeds none, so --binary must name one"
                    .to_string(),
            ),
        }
    }

    /// The executable to run, extracting the embedded application if that is
    /// what this build carries.
    fn path(self) -> Result<PathBuf, String> {
        match self {
            LaunchTarget::Embedded(app) => {
                app.materialise(packed::APP_ID).map_err(|e| e.to_string())
            }
            LaunchTarget::External(path) => Ok(path),
        }
    }
}

// ── Headless ──────────────────────────────────────────────────────────────────

/// Runs the agent activation path. `Err(code)` is the process exit code to use;
/// every code is documented in [`activation::EXIT_CODE_HELP`].
#[cfg(feature = "headless")]
fn run_headless(token_id: Option<u64>) -> Result<rub3_wrapper::Launch, i32> {
    use rub3_wrapper::activation::{ensure_headless, HeadlessContext, HeadlessError};
    use rub3_wrapper::signer::resolve_signer;

    let signer = resolve_signer().map_err(|e| {
        let e = HeadlessError::Signer(e);
        report(&e);
        e.exit_code()
    })?;

    let ctx = HeadlessContext {
        app_id: packed::APP_ID.to_string(),
        contract: packed::CONTRACT.to_string(),
        chain_id: packed::CHAIN_ID,
        rpc_url: packed::RPC_URL.to_string(),
        session_ttl_secs: packed::SESSION_TTL_SECS,
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
            Ok(rub3_wrapper::Launch::from_session(session))
        }
        Err(e) => {
            report(&e);
            Err(e.exit_code())
        }
    }
}

/// Runs `--release-seat`. Returns the process exit code.
#[cfg(feature = "headless")]
fn run_release_seat(token_id: Option<u64>) -> i32 {
    use rub3_wrapper::activation::{HeadlessContext, HeadlessError, ReleaseOutcome};
    use rub3_wrapper::signer::resolve_signer;

    let signer = match resolve_signer() {
        Ok(signer) => signer,
        Err(e) => {
            let e = HeadlessError::Signer(e);
            report(&e);
            return e.exit_code();
        }
    };

    let ctx = HeadlessContext {
        app_id: packed::APP_ID.to_string(),
        contract: packed::CONTRACT.to_string(),
        chain_id: packed::CHAIN_ID,
        rpc_url: packed::RPC_URL.to_string(),
        session_ttl_secs: packed::SESSION_TTL_SECS,
        token_id,
    };

    match rub3_wrapper::release_headless(signer.as_ref(), &ctx) {
        Ok(ReleaseOutcome::Released {
            token_id,
            session_id,
            tx_hash,
        }) => {
            eprintln!("rub3: seat released token_id={token_id} session_id={session_id}");
            eprintln!("rub3-detail: token_id={token_id} session_id={session_id} tx_hash={tx_hash}");
            activation::EXIT_OK
        }
        // Nothing to hand back is the outcome an orchestrator asked for, so it
        // is a success. The `released=false` key is how it tells the two apart
        // without parsing the sentence above it.
        Ok(ReleaseOutcome::NoSeatHeld { token_id }) => {
            match token_id {
                Some(token_id) => {
                    eprintln!("rub3: no seat held for token {token_id}; nothing to release");
                    eprintln!("rub3-detail: token_id={token_id} released=false");
                }
                None => {
                    eprintln!(
                        "rub3: this machine holds no seat on this contract; nothing to release"
                    );
                    eprintln!("rub3-detail: released=false");
                }
            }
            activation::EXIT_OK
        }
        Err(e) => {
            report(&e);
            e.exit_code()
        }
    }
}

#[cfg(not(feature = "headless"))]
fn run_release_seat(_token_id: Option<u64>) -> i32 {
    eprintln!(
        "error: --release-seat requires a build with the `headless` feature \
         (rebuild with --features headless)"
    );
    activation::EXIT_HEADLESS_UNSUPPORTED
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
fn run_headless(_token_id: Option<u64>) -> Result<rub3_wrapper::Launch, i32> {
    eprintln!(
        "error: --headless requires a build with the `headless` feature \
         (rebuild with --features headless)"
    );
    Err(activation::EXIT_HEADLESS_UNSUPPORTED)
}
