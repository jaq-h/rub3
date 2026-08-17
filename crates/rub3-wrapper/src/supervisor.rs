use std::ffi::OsStr;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Name of the variable carrying the SDK channel's address to the wrapped
/// application (`implementation.md` §3.5).
///
/// A second copy of `rub3::wire::ADDRESS_ENV`, and deliberately so: every build
/// scrubs the variable from the child's environment, including the ones that do
/// not compile the SDK channel and therefore cannot name that constant. The two
/// are asserted equal by a unit test in any build that has both.
pub const SDK_ADDRESS_ENV: &str = "RUB3_SDK_SOCKET";

/// What a launch can tell the wrapped application about itself.
///
/// Produced by whichever door authorised the launch - [`crate::activation::ensure`]
/// or `ensure_headless` - and handed to [`run`], which serves it over the SDK
/// channel. A build without the `session` capability has nothing to carry, so the
/// type is empty there rather than absent: the launch path then reads the same in
/// every tier bundle.
///
/// The session is carried only when there is somewhere for it to go. The SDK
/// channel is the sole reader - [`Launch::offer`] is the only code that ever
/// looks at it - so without `sdk` the field would be write-only, which is a
/// `dead_code` error under the workspace's `-D warnings` lint on the default
/// bundle. Both constructors stay available whenever `session` is on, because
/// the activation doors call them regardless of whether the channel is compiled.
#[derive(Default)]
pub struct Launch {
    #[cfg(all(feature = "session", feature = "sdk"))]
    session: Option<crate::session::Session>,
}

impl Launch {
    /// A launch with no session to report: a tier-0 build, or one served from
    /// the legacy `LicenseProof`.
    pub fn bare() -> Self {
        Self::default()
    }

    /// A launch authorised by `session`, which is what the application will be
    /// told when it asks - in a build that compiled the channel to ask over.
    #[cfg(feature = "session")]
    pub fn from_session(session: crate::session::Session) -> Self {
        #[cfg(not(feature = "sdk"))]
        drop(session);
        Self {
            #[cfg(feature = "sdk")]
            session: Some(session),
        }
    }
}

pub fn run(binary: &Path, args: &[String], launch: &Launch) -> i32 {
    // Started before the child, so the address handed to it is already
    // listening, and held for the child's whole lifetime: dropping the channel
    // stops it and removes the endpoint.
    let channel = start_channel(launch);

    let mut child = match spawn(binary, args, channel_address(&channel)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to launch {}: {}", binary.display(), e);
            return 1;
        }
    };

    let terminating = Arc::new(AtomicBool::new(false));

    #[cfg(unix)]
    setup_signal_handler(child.id(), Arc::clone(&terminating));

    loop {
        if terminating.load(Ordering::SeqCst) {
            let _ = child.kill();
            return 1;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                return status.code().unwrap_or(1);
            }
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("error: wait failed: {e}");
                return 1;
            }
        }
    }
}

/// Launches the wrapped binary without the agent credential in its
/// environment.
///
/// Headless activation reads a funded private key, or an encrypted keystore
/// and the password that opens it, out of the environment. The wrapped binary
/// is the licensed product, not the license holder: leaving any of those
/// variables in its environment would hand it (and its own children, and any
/// crash reporter it ships) what it needs to spend from the wallet. So every
/// name in [`crate::agent_env::AGENT_ENV_VARS`] is removed first.
///
/// Unconditional, and not gated behind the `headless` feature: what matters is
/// that the child is never handed the credential, however this wrapper was
/// built. It is containment, not a sandbox - the child runs as the same UID
/// and can still read whatever that user can read.
fn spawn(binary: &Path, args: &[String], sdk_address: Option<&OsStr>) -> std::io::Result<Child> {
    let mut cmd = Command::new(binary);
    cmd.args(args);
    for name in crate::agent_env::AGENT_ENV_VARS {
        cmd.env_remove(name);
    }

    // Cleared unconditionally, then set only when this launch really serves a
    // channel. An address inherited from this wrapper's own environment - a
    // wrapper launched by a wrapper, a variable left exported in a shell - would
    // otherwise point the application at somebody else's channel, and it would
    // answer.
    cmd.env_remove(SDK_ADDRESS_ENV);
    if let Some(address) = sdk_address {
        cmd.env(SDK_ADDRESS_ENV, address);
    }

    cmd.spawn()
}

// ── SDK channel (feature `sdk`) ───────────────────────────────────────────────
//
// Two small pairs so that `run` above reads the same in every bundle. The
// channel is `sdk`-gated because the wire types live in the `rub3` crate, which
// only that feature pulls in.

#[cfg(feature = "sdk")]
type ChannelGuard = Option<crate::sdk::Channel>;

/// Nothing to hold open in a build that serves no channel. A distinct type
/// rather than `()` so that [`run`] reads identically in every bundle without
/// binding a unit value.
#[cfg(not(feature = "sdk"))]
struct NoChannel;
#[cfg(not(feature = "sdk"))]
type ChannelGuard = NoChannel;

/// Starts the channel, or reports why it could not start and launches anyway.
///
/// Never fatal. Refusing to start a program the user has already paid for
/// because a socket could not be created would be a de-facto revocation
/// surface, which `architecture.md` -> "Ownership invariants" rules out. The
/// application's own `rub3::heartbeat()` then fails, which is the right place
/// for that decision: the developer chose whether their application requires
/// the channel.
#[cfg(feature = "sdk")]
fn start_channel(launch: &Launch) -> ChannelGuard {
    match crate::sdk::serve(launch.offer()) {
        Ok(channel) => Some(channel),
        Err(e) => {
            eprintln!("rub3: warning: the SDK channel could not start: {e}");
            eprintln!(
                "rub3: warning: launching anyway - the wrapped application's \
                 rub3::heartbeat() will fail"
            );
            None
        }
    }
}

#[cfg(not(feature = "sdk"))]
fn start_channel(_launch: &Launch) -> ChannelGuard {
    NoChannel
}

#[cfg(feature = "sdk")]
fn channel_address(channel: &ChannelGuard) -> Option<&OsStr> {
    channel.as_ref().map(|c| c.address())
}

#[cfg(not(feature = "sdk"))]
fn channel_address(_channel: &ChannelGuard) -> Option<&OsStr> {
    None
}

#[cfg(feature = "sdk")]
impl Launch {
    /// What the channel answers a `session` request with.
    fn offer(&self) -> crate::sdk::Offer {
        #[cfg(feature = "session")]
        if let Some(session) = &self.session {
            return crate::sdk::offer(session);
        }
        crate::sdk::Offer::None(NO_SESSION_REASON.to_string())
    }
}

/// Why a launch has no session to report. Three different facts, and an
/// application developer reading the panic needs to know which one they hit:
/// a build with no session model, a build that has one but can never mint a
/// session because sessions are gated on `cooldown`, and a launch that could
/// have carried one and did not.
#[cfg(all(feature = "sdk", not(feature = "session")))]
const NO_SESSION_REASON: &str =
    "this wrapper was built without the session capability (tier-0), so it has no session";
#[cfg(all(feature = "sdk", feature = "session", not(feature = "cooldown")))]
const NO_SESSION_REASON: &str =
    "this wrapper was built without the cooldown capability (below tier-3), so every launch is \
     served from the legacy licence proof, which carries no identity model and therefore no \
     user_id";
#[cfg(all(feature = "sdk", feature = "session", feature = "cooldown"))]
const NO_SESSION_REASON: &str =
    "this launch was served from the legacy licence proof, which carries no identity model \
     and therefore no user_id";

/// On Unix: forward SIGTERM to the child, then exit.
/// SIGCHLD is handled implicitly by try_wait().
#[cfg(unix)]
fn setup_signal_handler(child_pid: u32, terminating: Arc<AtomicBool>) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    // SAFETY: signal handler only sets an atomic flag and sends a signal.
    unsafe {
        libc_signal::register(libc_signal::SIGTERM, move || {
            terminating.store(true, Ordering::SeqCst);
            let _ = kill(Pid::from_raw(child_pid as i32), Signal::SIGTERM);
        });
    }
}

/// Thin wrapper around libc signal() for SIGTERM.
#[cfg(unix)]
mod libc_signal {
    pub const SIGTERM: i32 = libc::SIGTERM;

    static mut HANDLER: Option<Box<dyn Fn() + Send>> = None;

    pub unsafe fn register<F: Fn() + Send + 'static>(signum: i32, f: F) {
        HANDLER = Some(Box::new(f));
        libc::signal(
            signum,
            trampoline as extern "C" fn(i32) as libc::sighandler_t,
        );
    }

    extern "C" fn trampoline(_: i32) {
        unsafe {
            if let Some(h) = (&raw const HANDLER).as_ref().and_then(|h| h.as_ref()) {
                h();
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agent_env::{
        AGENT_ENV_VARS, ENV_AGENT_KEY, ENV_AGENT_KEYSTORE, ENV_AGENT_KEYSTORE_PASSWORD,
        ENV_AGENT_KEYSTORE_PASSWORD_FILE,
    };

    /// Runs `spawn` against a child that dumps its own environment, and
    /// returns what that child saw. The dump is the child's report of what it
    /// inherited, which is the thing under test.
    fn child_environment(dir: &std::path::Path) -> String {
        let dump = dir.join("child-env.txt");
        // Inherited on purpose: it is how the child tells us what it saw, and
        // its presence proves the dump ran rather than the file being empty.
        std::env::set_var("RUB3_TEST_ENV_DUMP", &dump);

        let status = spawn(
            Path::new("/bin/sh"),
            &[
                "-c".to_string(),
                "printenv > \"$RUB3_TEST_ENV_DUMP\"".to_string(),
            ],
            None,
        )
        .expect("spawn /bin/sh")
        .wait()
        .expect("wait for child");
        assert!(status.success(), "child failed: {status:?}");

        let seen = std::fs::read_to_string(&dump).expect("child wrote its environment");
        assert!(
            seen.lines().any(|l| l.starts_with("RUB3_TEST_ENV_DUMP=")),
            "the dump did not capture the child's environment: {seen:?}",
        );
        std::env::remove_var("RUB3_TEST_ENV_DUMP");
        seen
    }

    fn assert_no_agent_vars(seen: &str, secrets: &[&str]) {
        let names: Vec<&str> = seen
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k)
            .collect();
        for stripped in AGENT_ENV_VARS {
            assert!(
                !names.contains(&stripped),
                "{stripped} reached the wrapped binary"
            );
        }
        for secret in secrets {
            assert!(
                !seen.contains(secret),
                "credential material reached the wrapped binary under another name",
            );
        }
    }

    /// The raw-key configuration: `RUB3_AGENT_KEY` alone.
    #[test]
    fn the_wrapped_binary_does_not_inherit_a_raw_agent_key() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");

        std::env::set_var(ENV_AGENT_KEY, "0xdeadbeef");
        let seen = child_environment(dir.path());
        std::env::remove_var(ENV_AGENT_KEY);

        assert_no_agent_vars(&seen, &["0xdeadbeef"]);
    }

    /// The documented preferred configuration: an encrypted keystore plus a
    /// mode-0600 password file, no inline password. Neither variable is the
    /// key, but a child holding both paths can decrypt one, so neither may
    /// survive into its environment.
    #[test]
    fn the_wrapped_binary_does_not_inherit_the_keystore_or_its_password_file() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let keystore = dir.path().join("agent-key.json");
        let password_file = dir.path().join("pw.txt");

        std::env::set_var(ENV_AGENT_KEYSTORE, &keystore);
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD_FILE, &password_file);
        let seen = child_environment(dir.path());
        std::env::remove_var(ENV_AGENT_KEYSTORE);
        std::env::remove_var(ENV_AGENT_KEYSTORE_PASSWORD_FILE);

        assert_no_agent_vars(
            &seen,
            &[
                keystore.to_str().expect("utf-8 path"),
                password_file.to_str().expect("utf-8 path"),
            ],
        );
    }

    /// Every source at once, including the inline password.
    #[test]
    fn the_wrapped_binary_inherits_no_agent_variable_from_any_source() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");

        std::env::set_var(ENV_AGENT_KEY, "0xdeadbeef");
        std::env::set_var(ENV_AGENT_KEYSTORE, dir.path().join("agent-key.json"));
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD, "hunter2");
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD_FILE, dir.path().join("pw.txt"));
        let seen = child_environment(dir.path());
        for name in AGENT_ENV_VARS {
            std::env::remove_var(name);
        }

        assert_no_agent_vars(&seen, &["0xdeadbeef", "hunter2"]);
    }

    /// The channel address is scrubbed from every child's environment in every
    /// bundle, including the ones that serve no channel at all - a child that
    /// inherited one would talk to somebody else's channel and be answered.
    ///
    /// `tests/sdk_e2e.rs` proves the same thing for a build that does serve one,
    /// but that suite does not compile without the `sdk` feature, and the scrub
    /// is deliberately unconditional. This is the half no `sdk`-gated test can
    /// reach.
    #[test]
    fn the_wrapped_binary_does_not_inherit_a_stale_sdk_channel_address() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let stale = dir.path().join("somebody-elses.sock");

        std::env::set_var(SDK_ADDRESS_ENV, &stale);
        let seen = child_environment(dir.path());
        std::env::remove_var(SDK_ADDRESS_ENV);

        let assigned = format!("{SDK_ADDRESS_ENV}=");
        assert!(
            !seen.lines().any(|l| l.starts_with(&assigned)),
            "{SDK_ADDRESS_ENV} reached the wrapped binary: {seen}"
        );
    }
}
