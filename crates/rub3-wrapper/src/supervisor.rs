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

/// Environment variables the wrapped binary must never see.
///
/// Headless activation reads a funded private key, or the password that
/// decrypts one, out of the environment. The wrapped binary is the licensed
/// product, not the license holder: leaving either variable in its environment
/// would hand it (and its own children, and any crash reporter it ships) the
/// ability to drain the wallet. Stripping them is unconditional and not gated
/// behind the `headless` feature, because what matters is that the child never
/// sees key material regardless of how this wrapper was built.
///
/// Mirrors `signer::ENV_AGENT_KEY` and `signer::ENV_AGENT_KEYSTORE_PASSWORD`,
/// which cannot be named here: `signer` exists only in headless builds.
pub(crate) const STRIPPED_ENV: [&str; 2] = ["RUB3_AGENT_KEY", "RUB3_AGENT_KEYSTORE_PASSWORD"];

fn spawn(binary: &Path, args: &[String]) -> std::io::Result<Child> {
    let mut cmd = Command::new(binary);
    cmd.args(args);
    for name in STRIPPED_ENV {
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

    /// The wrapped binary runs with the operator's key in reach unless the
    /// wrapper takes it away. Proven by letting a real child report its own
    /// environment: a `printenv` dump is the child's view of what it inherited.
    #[test]
    fn the_wrapped_binary_does_not_inherit_agent_key_material() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().expect("tempdir");
        let dump = dir.path().join("child-env.txt");

        // Inherited on purpose: it is how the child tells us what it saw, and
        // it proves the dump ran rather than the file merely being empty.
        std::env::set_var("RUB3_TEST_ENV_DUMP", &dump);
        std::env::set_var("RUB3_AGENT_KEY", "0xdeadbeef");
        std::env::set_var("RUB3_AGENT_KEYSTORE_PASSWORD", "hunter2");

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

        let child_env = std::fs::read_to_string(&dump).expect("child wrote its environment");
        let names: Vec<&str> = child_env
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k)
            .collect();

        assert!(
            names.contains(&"RUB3_TEST_ENV_DUMP"),
            "the dump did not capture the child's environment: {child_env:?}",
        );
        for stripped in STRIPPED_ENV {
            assert!(
                !names.contains(&stripped),
                "{stripped} reached the wrapped binary",
            );
        }
        assert!(
            !child_env.contains("hunter2") && !child_env.contains("0xdeadbeef"),
            "key material reached the wrapped binary under some other name",
        );

        std::env::remove_var("RUB3_TEST_ENV_DUMP");
        std::env::remove_var("RUB3_AGENT_KEY");
        std::env::remove_var("RUB3_AGENT_KEYSTORE_PASSWORD");
    }
}
