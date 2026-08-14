use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub fn run(binary: &Path, args: &[String]) -> i32 {
    let mut child = match spawn(binary, args) {
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
fn spawn(binary: &Path, args: &[String]) -> std::io::Result<Child> {
    let mut cmd = Command::new(binary);
    cmd.args(args);
    for name in crate::agent_env::AGENT_ENV_VARS {
        cmd.env_remove(name);
    }
    cmd.spawn()
}

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
        libc::signal(signum, trampoline as libc::sighandler_t);
    }

    extern "C" fn trampoline(_: i32) {
        unsafe {
            if let Some(h) = (*(&raw const HANDLER)).as_ref() {
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
        let names: Vec<&str> = seen.lines().filter_map(|l| l.split_once('=')).map(|(k, _)| k).collect();
        for stripped in AGENT_ENV_VARS {
            assert!(!names.contains(&stripped), "{stripped} reached the wrapped binary");
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
}
