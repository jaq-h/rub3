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

fn spawn(binary: &Path, args: &[String]) -> std::io::Result<Child> {
    Command::new(binary).args(args).spawn()
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
