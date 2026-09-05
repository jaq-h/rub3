//! The wrapper's half of the SDK channel (`implementation.md` §3.5).
//!
//! A per-launch local endpoint - a Unix domain socket, or a Windows named pipe -
//! that answers two questions from the application this wrapper launched: "are
//! you there" and "who is running me". Its address is handed to the child in
//! [`rub3::wire::ADDRESS_ENV`] and nowhere else, so an application started by
//! anything other than a wrapper finds nothing to talk to.
//!
//! **What the channel is for, and what it is not.** It is an honest-integration
//! and liveness aid. Licensing is enforced before the wrapper launches anything;
//! by the time this endpoint exists the gate has already run. So the channel
//! carries no authentication and no shared secret, deliberately: an attacker who
//! can run the wrapped binary outside its wrapper can also publish a socket of
//! their own and answer every request however they like, and any credential this
//! side could demand would have to ship inside the binary they already control.
//! The `rub3` crate's own documentation states the same conclusion to the
//! developer who reads it. What the channel does catch is the ordinary set: an
//! application launched directly, a wrapper that died mid-run, a stale address,
//! a version-skewed pair.
//!
//! **What it leaves behind.** Nothing, on any exit it can see: the endpoint goes
//! with the [`Channel`], and one that outlived its wrapper - killed, Ctrl-C'd,
//! crashed - is collected by the next `serve` on the machine, which only ever
//! removes a directory it can name and whose pid is gone.
//!
//! **What crosses it.** Exactly the six fields of [`rub3::SessionInfo`]. The
//! signature, the nonce, the activation transaction and the device public key
//! stay on this side: an application that could read the session signature could
//! replay the session somewhere this wrapper never launched it.
//!
//! **It never gates a launch.** A channel that fails to start is reported on
//! stderr and the wrapped binary runs anyway. Refusing to start a program the
//! user has already paid for because a socket could not be created would be a
//! revocation surface, which `architecture.md` -> "Ownership invariants" rules
//! out; the same fail-open-on-launch posture as §2.6's attestation. The child is
//! still told a wrapper launched it - `supervisor::SDK_ADDRESS_NO_CHANNEL` goes
//! into the variable in place of an address - so what it reports is a wrapper
//! serving no channel rather than no wrapper, which is the one thing it is not.

use std::ffi::{OsStr, OsString};
use std::io::{self, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rub3::wire::{self, Envelope, Request, Response};
use rub3::SessionInfo;

// ── What a launch can report ──────────────────────────────────────────────────

/// What this launch can tell the application about itself.
pub enum Offer {
    /// The session the wrapper launched on.
    Session(SessionInfo),
    /// Nothing to report, and why. A tier-0 build has no session model at all,
    /// and a launch served from the legacy `LicenseProof` carries no identity
    /// model, so it has no `user_id` to answer with - inventing one from the
    /// wallet would be a second identity notion, which `session.rs` already
    /// owns properly.
    None(String),
}

impl Offer {
    fn respond(&self) -> Response {
        match self {
            Offer::Session(session) => Response::Session {
                session: session.clone(),
            },
            Offer::None(reason) => Response::NoSession {
                reason: reason.clone(),
            },
        }
    }
}

/// Projects a verified session onto the six fields an application may see.
///
/// An unparseable `expires_at` becomes [`Offer::None`] rather than a session
/// with the TTL quietly dropped. It should be unreachable - `session::is_expired`
/// reads an unparseable timestamp as expired, so such a session never reaches a
/// launch - and understating an expiry is the one way this conversion could
/// mislead an application, so it fails visibly instead.
#[cfg(feature = "session")]
pub fn offer(session: &crate::session::Session) -> Offer {
    let expires_at = match &session.expires_at {
        None => None,
        Some(raw) => match raw.parse::<chrono::DateTime<chrono::Utc>>() {
            Ok(ts) => Some(ts),
            Err(e) => {
                return Offer::None(format!(
                    "the session's expires_at is not an RFC 3339 timestamp: {e}"
                ))
            }
        },
    };

    Offer::Session(SessionInfo {
        app_id: session.app_id.clone(),
        token_id: session.token_id,
        user_id: rub3::UserId::new(session.user_id.clone()),
        wallet: rub3::Wallet::new(session.wallet.clone()),
        identity: rub3::Identity::from(session.identity.clone()),
        expires_at,
    })
}

// ── The channel ───────────────────────────────────────────────────────────────

/// A listening channel. Dropping it stops accepting and removes the endpoint.
pub struct Channel {
    address: OsString,
    shutdown: Arc<AtomicBool>,
    /// The 0700 directory holding the socket, removed on drop. Unix only:
    /// a named pipe is a kernel object with no filesystem residue to clean up.
    #[cfg(unix)]
    dir: std::path::PathBuf,
}

impl Channel {
    /// The address to publish to the child in [`rub3::wire::ADDRESS_ENV`].
    pub fn address(&self) -> &OsStr {
        &self.address
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Starts the channel and returns it, or the reason it could not start.
///
/// The caller publishes [`Channel::address`] to the child and keeps the value
/// alive for as long as the child runs.
pub fn serve(offer: Offer) -> io::Result<Channel> {
    let shutdown = Arc::new(AtomicBool::new(false));
    platform::serve(offer, shutdown)
}

/// Answers one connection until the peer closes it.
///
/// A connection may carry any number of requests, so a long-running application
/// can hold one open and poll over it rather than reconnecting.
fn serve_connection<S>(stream: S, offer: &Offer) -> io::Result<()>
where
    S: Read + Write + TryCloneStream,
{
    let mut writer = stream.try_clone_stream()?;
    let mut reader = BufReader::new(stream);

    loop {
        // Read the envelope with an opaque body first. A future protocol version
        // may change the body's shape entirely, and a peer that speaks one
        // deserves "we speak different versions" rather than a JSON complaint.
        let envelope: Envelope<serde_json::Value> = match wire::read_message(&mut reader) {
            Ok(Some(envelope)) => envelope,
            Ok(None) => return Ok(()),
            Err(e) => {
                // Answer once, then stop: the stream is desynchronised and
                // parsing on from here would report noise.
                let _ = answer(
                    &mut writer,
                    Response::Error {
                        message: format!("malformed request: {e}"),
                    },
                );
                return Ok(());
            }
        };

        let response = if envelope.protocol != wire::PROTOCOL_VERSION {
            Response::Error {
                message: format!(
                    "this wrapper speaks rub3 protocol version {}, the request is version {}",
                    wire::PROTOCOL_VERSION,
                    envelope.protocol
                ),
            }
        } else {
            match serde_json::from_value::<Request>(envelope.body) {
                Ok(Request::Heartbeat) => Response::Alive,
                Ok(Request::Session) => offer.respond(),
                Err(e) => Response::Error {
                    message: format!("unrecognised request: {e}"),
                },
            }
        };

        answer(&mut writer, response)?;
    }
}

fn answer<W: Write>(writer: &mut W, response: Response) -> io::Result<()> {
    wire::write_message(writer, &Envelope::new(response))
}

/// Duplicating the handle is what lets one connection be read and written at
/// once; both platforms' stream types have it under a different name.
trait TryCloneStream: Sized {
    fn try_clone_stream(&self) -> io::Result<Self>;
}

// ── Unix: a socket in a 0700 directory ────────────────────────────────────────

#[cfg(unix)]
mod platform {
    use super::*;

    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    impl TryCloneStream for UnixStream {
        fn try_clone_stream(&self) -> io::Result<Self> {
            self.try_clone()
        }
    }

    /// How long the accept loop sleeps between polls when nothing is connecting.
    /// The listener is non-blocking so that dropping the [`Channel`] ends the
    /// loop, which a blocking `accept` would sit in until a client arrived.
    const POLL: std::time::Duration = std::time::Duration::from_millis(20);

    /// Longest socket path this will build before falling back to `/tmp`.
    ///
    /// `sockaddr_un.sun_path` is 104 bytes on macOS and 108 on Linux, and the
    /// per-user temporary directory on macOS is already about 50 of them. A bind
    /// that fails on a long `TMPDIR` would be a channel that works on a laptop
    /// and not on a build agent, which is exactly the kind of failure that gets
    /// diagnosed as flakiness.
    const MAX_SOCKET_PATH: usize = 100;

    pub(super) fn serve(offer: Offer, shutdown: Arc<AtomicBool>) -> io::Result<Channel> {
        let dir = EndpointDir::create()?;
        let path = dir.path().join("s");
        sweep_stale_endpoints(dir.parent());

        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;

        let offer = Arc::new(offer);
        let loop_shutdown = Arc::clone(&shutdown);
        std::thread::Builder::new()
            .name("rub3-sdk-channel".to_string())
            .spawn(move || accept_loop(listener, offer, loop_shutdown))?;

        Ok(Channel {
            address: path.into_os_string(),
            shutdown,
            dir: dir.keep(),
        })
    }

    /// Owns the endpoint directory until the [`Channel`] that will remove it
    /// exists.
    ///
    /// Everything in [`serve`] after the directory is created can fail, and
    /// `Channel::drop` is the only other thing that removes it, so a `?` on any
    /// of those steps would leave a 0700 directory holding a socket with no
    /// owner and no reaper. One guard covers all of them, and covers a fallible
    /// step added here later without that step having to remember to.
    pub(super) struct EndpointDir {
        path: Option<PathBuf>,
    }

    impl EndpointDir {
        pub(super) fn create() -> io::Result<Self> {
            Ok(Self {
                path: Some(endpoint_dir()?),
            })
        }

        pub(super) fn path(&self) -> &std::path::Path {
            self.path.as_deref().expect("armed until `keep` takes it")
        }

        /// The directory the endpoints live in, which is what the sweep reads.
        fn parent(&self) -> &std::path::Path {
            self.path()
                .parent()
                .expect("an endpoint directory is created inside a base directory")
        }

        /// Hands the directory to the caller, so dropping the guard no longer
        /// removes it. Called only where a [`Channel`] takes over that job.
        pub(super) fn keep(mut self) -> PathBuf {
            self.path.take().expect("armed until `keep` takes it")
        }
    }

    impl Drop for EndpointDir {
        fn drop(&mut self) {
            if let Some(path) = &self.path {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }

    /// Removes endpoint directories whose wrapper is gone.
    ///
    /// `Channel::drop` is the ordinary reaper, and it does not run when the
    /// wrapper is killed: Ctrl-C on a wrapped CLI is a SIGINT to the whole
    /// foreground group, which takes its default action and skips every
    /// destructor, as do SIGHUP and SIGKILL. Sweeping at [`serve`] time covers
    /// those exits and a hard crash alike, which is why it is here rather than
    /// in another signal handler - a handler cannot run for the exits that
    /// matter most.
    ///
    /// It deletes directories under a temporary directory shared with the rest
    /// of the machine, so it is deliberately narrow. A name that does not parse
    /// as one [`endpoint_dir`] wrote is left alone, and so is one whose pid is
    /// still running: several wrappers running at once is ordinary here, and one
    /// of those directories is a live channel serving a live application.
    ///
    /// Non-fatal throughout. A temp directory that cannot be read or an entry
    /// that will not delete must never stop a paid-for launch, which is the same
    /// fail-open-on-launch posture as the rest of this module.
    pub(super) fn sweep_stale_endpoints(base: &std::path::Path) {
        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };

        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(pid) = endpoint_owner(&name) else {
                continue;
            };
            if pid == std::process::id() || pid_is_alive(pid) {
                continue;
            }
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }

    /// The pid inside an endpoint directory's own name, or `None` when the name
    /// is not one [`endpoint_dir`] produced.
    ///
    /// Every field has to parse, and there has to be nothing after the last one:
    /// the sweep removes what this accepts, so anything it cannot account for is
    /// somebody else's and stays.
    fn endpoint_owner(name: &str) -> Option<u32> {
        let mut parts = name.split('-');
        if parts.next()? != "rub3" {
            return None;
        }
        let pid = parts.next()?.parse::<u32>().ok()?;
        parts.next()?.parse::<u32>().ok()?;
        parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        // `getpid` never returns 0, so a name carrying it is not ours - and 0
        // means "this process group" to `kill`, which is not a question worth
        // asking on the way to a delete.
        (pid != 0).then_some(pid)
    }

    /// Whether a pid still names a running process. Unknowable answers count as
    /// alive: `EPERM` is another user's process, and the sweep only ever deletes
    /// on a definite "gone".
    fn pid_is_alive(pid: u32) -> bool {
        let Ok(raw) = i32::try_from(pid) else {
            return true;
        };
        !matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None),
            Err(nix::errno::Errno::ESRCH)
        )
    }

    fn accept_loop(listener: UnixListener, offer: Arc<Offer>, shutdown: Arc<AtomicBool>) {
        while !shutdown.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    // macOS and the BSDs hand the accepted socket the
                    // listener's O_NONBLOCK; Linux does not. So this call is
                    // load-bearing on one platform and a no-op on the other,
                    // and without it a read that arrives before the client's
                    // first line would return EAGAIN, which `serve_connection`
                    // reads as a malformed request and answers before closing.
                    // A stream that cannot be put back into blocking mode is
                    // dropped rather than served, so the client sees a dead
                    // connection rather than an invented protocol error.
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }

                    // A thread per connection: an application holding a
                    // keep-alive connection open must not stop a second one
                    // being served, and a blocking read on an idle connection
                    // would do exactly that if connections were serialised.
                    let offer = Arc::clone(&offer);
                    let _ = std::thread::Builder::new()
                        .name("rub3-sdk-conn".to_string())
                        .spawn(move || {
                            let _ = serve_connection(stream, &offer);
                        });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
                // Any other accept error is per-connection, not fatal to the
                // channel: keep listening rather than silently going deaf.
                Err(_) => std::thread::sleep(POLL),
            }
        }
    }

    /// Distinguishes two endpoints served by one process. `getpid` separates
    /// processes and the clock reading separates a reused pid from the endpoint
    /// a previous holder of it leaked, but neither separates two [`serve`] calls
    /// in the same process and the same clock tick, and `DirBuilder::create`
    /// fails on a collision rather than retrying.
    static ENDPOINT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Creates the 0700 directory the socket lives in.
    ///
    /// The directory's mode is the access control, not the name: it is what
    /// stops another user on the machine connecting to the channel or replacing
    /// the socket with one of their own. The name only has to be unique per
    /// call, which `getpid` plus a nanosecond clock reading plus
    /// [`ENDPOINT_SEQ`] covers: two wrappers can run at once, the same pid can
    /// be reused after one exits, and one process can serve more than one
    /// channel.
    fn endpoint_dir() -> io::Result<PathBuf> {
        let name = format!(
            "rub3-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
            ENDPOINT_SEQ.fetch_add(1, Ordering::Relaxed)
        );

        let mut base = std::env::temp_dir();
        if base.join(&name).join("s").as_os_str().len() > MAX_SOCKET_PATH {
            base = PathBuf::from("/tmp");
        }

        let dir = base.join(name);
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(dir)
    }
}

// ── Windows: a named pipe ─────────────────────────────────────────────────────
//
// NEITHER COMPILED NOR RUN ON WINDOWS. Written from the documented Win32
// contract, but nothing in this project has ever compiled it: cross-checking
// `rub3-wrapper` for `x86_64-pc-windows-msvc` dies in `aws-lc-sys` (`rustls` <-
// `reqwest` <- `alloy`, an unconditional dependency), which stops on a missing
// `windows.h` before this crate is reached, and no rub3 CI runner is a Windows
// box (`.github/workflows/ci.yml` is macOS-only). So the `windows-sys` feature
// selection, the import paths and the call signatures below are unverified, and
// no test here has executed a single one of these calls. Treat the unix path as
// the tested one, and this as an honest implementation awaiting a host that can
// build it. The SDK crate's client half of the same channel does type-check for
// that target (`cargo check -p rub3 --target x86_64-pc-windows-msvc`).
#[cfg(windows)]
mod platform {
    use super::*;

    use std::fs::File;
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    /// Both directions get their own buffer; the messages are a few hundred
    /// bytes and the kernel grows the buffer on demand anyway.
    const BUFFER_BYTES: u32 = 8 * 1024;

    impl TryCloneStream for File {
        fn try_clone_stream(&self) -> io::Result<Self> {
            self.try_clone()
        }
    }

    pub(super) fn serve(offer: Offer, shutdown: Arc<AtomicBool>) -> io::Result<Channel> {
        // The name is only required to be unique per launch. Unlike the unix
        // socket there is no directory mode to lean on: a named pipe's default
        // DACL admits other processes in the same session, and per this module's
        // header that is not a boundary the channel claims to hold anyway.
        let name = format!(
            r"\\.\pipe\rub3-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        );

        // Create the first instance before returning, so that a child launched
        // immediately afterwards cannot beat the server to the pipe and see
        // "file not found" instead of the channel.
        let first = create_instance(&name)?;

        let offer = Arc::new(offer);
        let loop_name = name.clone();
        let loop_shutdown = Arc::clone(&shutdown);
        std::thread::Builder::new()
            .name("rub3-sdk-channel".to_string())
            .spawn(move || accept_loop(first, loop_name, offer, loop_shutdown))?;

        Ok(Channel {
            address: OsString::from(name),
            shutdown,
        })
    }

    fn accept_loop(first: File, name: String, offer: Arc<Offer>, shutdown: Arc<AtomicBool>) {
        let mut instance = first;
        loop {
            // `ConnectNamedPipe` blocks until a client arrives, so the shutdown
            // flag is observed on the next connection rather than promptly. That
            // is sufficient: the wrapper drops the channel as it exits, and
            // process exit takes this thread with it.
            let connected = wait_for_client(&instance);
            if shutdown.load(Ordering::SeqCst) {
                return;
            }

            if connected {
                // One pipe instance serves one client, so the instance itself
                // moves into the connection thread, which owns it, disconnects
                // it and closes it. The next client needs a new instance.
                let offer = Arc::clone(&offer);
                match std::thread::Builder::new()
                    .name("rub3-sdk-conn".to_string())
                    .spawn(move || {
                        if let Ok(stream) = instance.try_clone() {
                            let _ = serve_connection(stream, &offer);
                        }
                        disconnect(&instance);
                    }) {
                    Ok(_) => {}
                    // The instance was moved into the closure either way, so a
                    // failed spawn has already released it.
                    Err(_) => return,
                }
            }

            // A failure to create the next instance ends the loop rather than
            // spinning on it.
            match create_instance(&name) {
                Ok(next) => instance = next,
                Err(_) => return,
            }
        }
    }

    fn create_instance(name: &str) -> io::Result<File> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
        // call, and the remaining arguments are plain scalars. The returned
        // handle is checked against INVALID_HANDLE_VALUE before it is adopted.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER_BYTES,
                BUFFER_BYTES,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` is a fresh, valid, owned pipe handle, so `File` takes
        // sole ownership of it and closes it on drop.
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }

    /// Blocks until a client connects. A client that connected between the
    /// create and this call reports `ERROR_PIPE_CONNECTED`, which is success.
    fn wait_for_client(instance: &File) -> bool {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the handle is owned by `instance` and outlives the call.
        let ok = unsafe { ConnectNamedPipe(instance.as_raw_handle() as _, std::ptr::null_mut()) };
        if ok != 0 {
            return true;
        }
        // SAFETY: no arguments, reads this thread's last error.
        unsafe { GetLastError() == ERROR_PIPE_CONNECTED }
    }

    fn disconnect(instance: &File) {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the handle is owned by `instance` and outlives the call.
        unsafe {
            DisconnectNamedPipe(instance.as_raw_handle() as _);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    /// The wrapper sets the channel variable on every child's environment, to a
    /// real address or to the no-channel sentinel, including in builds that
    /// cannot name `rub3::wire`'s constants because they do not compile the SDK
    /// channel at all. Those second copies are only safe while they agree, and
    /// this is what makes disagreeing fail loudly.
    #[test]
    fn the_published_variable_and_sentinel_match_the_sdk_crates_constants() {
        assert_eq!(crate::supervisor::SDK_ADDRESS_ENV, wire::ADDRESS_ENV);
        assert_eq!(
            crate::supervisor::SDK_ADDRESS_NO_CHANNEL,
            wire::ADDRESS_NO_CHANNEL
        );
    }

    // ── Request handling ─────────────────────────────────────────────────────

    /// A connection standing in for a socket: everything written is captured,
    /// everything read comes from a scripted buffer, and `try_clone_stream`
    /// shares both the way a duplicated socket handle does.
    #[derive(Clone)]
    struct Loopback {
        input: Arc<Mutex<io::Cursor<Vec<u8>>>>,
        output: Arc<Mutex<Vec<u8>>>,
    }

    impl Loopback {
        fn scripted(lines: &[&str]) -> Self {
            let mut bytes = Vec::new();
            for line in lines {
                bytes.extend_from_slice(line.as_bytes());
                bytes.push(b'\n');
            }
            Self {
                input: Arc::new(Mutex::new(io::Cursor::new(bytes))),
                output: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn written(&self) -> String {
            String::from_utf8(self.output.lock().unwrap().clone()).expect("utf-8")
        }
    }

    impl Read for Loopback {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.lock().unwrap().read(buf)
        }
    }

    impl Write for Loopback {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TryCloneStream for Loopback {
        fn try_clone_stream(&self) -> io::Result<Self> {
            Ok(self.clone())
        }
    }

    /// Drives `serve_connection` over the scripted requests and returns the
    /// answers, parsed.
    fn answers(requests: &[&str], offer: Offer) -> Vec<serde_json::Value> {
        let stream = Loopback::scripted(requests);
        serve_connection(stream.clone(), &offer).expect("the connection should be served");
        stream
            .written()
            .lines()
            .map(|l| serde_json::from_str(l).expect("each answer is one JSON line"))
            .collect()
    }

    fn no_session() -> Offer {
        Offer::None("nothing to report".to_string())
    }

    #[test]
    fn a_heartbeat_is_answered_alive_at_this_protocol_version() {
        let got = answers(&[r#"{"protocol":1,"op":"heartbeat"}"#], no_session());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["result"], "alive");
        assert_eq!(got[0]["protocol"], wire::PROTOCOL_VERSION);
    }

    /// One connection, several requests: a long-running application holds the
    /// channel open and polls over it rather than reconnecting every time.
    #[test]
    fn several_requests_on_one_connection_are_each_answered() {
        let got = answers(
            &[
                r#"{"protocol":1,"op":"heartbeat"}"#,
                r#"{"protocol":1,"op":"session"}"#,
                r#"{"protocol":1,"op":"heartbeat"}"#,
            ],
            no_session(),
        );
        assert_eq!(got.len(), 3);
        assert_eq!(got[0]["result"], "alive");
        assert_eq!(got[1]["result"], "no_session");
        assert_eq!(got[2]["result"], "alive");
    }

    /// A version the wrapper does not speak is reported as that, not as a
    /// parse failure: the wrapper and the application it launches are packaged
    /// separately, so a mismatched pair is an ordinary deployment state and the
    /// developer needs to be told which side to move.
    #[test]
    fn a_request_at_another_protocol_version_is_told_which_versions_are_in_play() {
        let got = answers(&[r#"{"protocol":99,"op":"heartbeat"}"#], no_session());
        assert_eq!(got[0]["result"], "error");
        let message = got[0]["message"].as_str().expect("a message");
        assert!(message.contains("99"), "{message}");
        assert!(
            message.contains(&wire::PROTOCOL_VERSION.to_string()),
            "{message}"
        );
    }

    #[test]
    fn an_unrecognised_operation_is_an_error_rather_than_a_dropped_connection() {
        let got = answers(
            &[r#"{"protocol":1,"op":"transfer_the_licence"}"#],
            no_session(),
        );
        assert_eq!(got[0]["result"], "error");
        assert!(got[0]["message"]
            .as_str()
            .unwrap()
            .contains("unrecognised request"));
    }

    /// A desynchronised stream is answered once and then closed. Parsing on
    /// from the middle of a broken line would report noise as further failures.
    #[test]
    fn a_malformed_line_is_answered_once_and_the_connection_ends() {
        let got = answers(
            &["}not json{", r#"{"protocol":1,"op":"heartbeat"}"#],
            no_session(),
        );
        assert_eq!(got.len(), 1, "the second request must not be read: {got:?}");
        assert_eq!(got[0]["result"], "error");
        assert!(got[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("malformed request"));
    }

    // ── The session projection ───────────────────────────────────────────────

    #[cfg(feature = "session")]
    mod projection {
        use super::*;

        fn session(identity: &str, expires_at: Option<&str>) -> crate::session::Session {
            crate::session::Session {
                app_id: "com.rub3.example".to_string(),
                token_id: 42,
                identity: identity.to_string(),
                user_id: "0x00000000000000000000000000000000000000bb".to_string(),
                tba: Some("0x00000000000000000000000000000000000000bb".to_string()),
                wallet: "0x00000000000000000000000000000000000000aa".to_string(),
                nonce: "a".repeat(64),
                issued_at: "2026-01-01T00:00:00Z".to_string(),
                expires_at: expires_at.map(str::to_string),
                signature: format!("0x{}", "b".repeat(130)),
                chain: "base".to_string(),
                chain_id: 8453,
                contract: "0x0000000000000000000000000000000000000002".to_string(),
                activation_tx: Some(format!("0x{}", "c".repeat(64))),
                activation_block: Some(9),
                activation_block_hash: Some(format!("0x{}", "d".repeat(64))),
                session_id: Some(1),
                device_pubkey: None,
            }
        }

        fn projected(session: &crate::session::Session) -> SessionInfo {
            match offer(session) {
                Offer::Session(info) => info,
                Offer::None(reason) => panic!("expected a session, got: {reason}"),
            }
        }

        /// `user_id` is the licence identity and `wallet` is the signer, and the
        /// account model is where they differ. Crossing them would be the one
        /// mistake §3.5 is written to prevent.
        #[test]
        fn the_account_model_reports_the_identity_and_the_signer_separately() {
            let session = session("account", Some("2099-01-01T00:00:00Z"));
            let info = projected(&session);

            assert_eq!(info.app_id, session.app_id);
            assert_eq!(info.token_id, 42);
            assert_eq!(info.user_id.as_str(), session.user_id);
            assert_eq!(info.wallet.to_string(), session.wallet);
            assert_eq!(info.identity, rub3::Identity::Account);
            assert_ne!(info.user_id.as_str(), info.wallet.to_string());
            assert_eq!(
                info.expires_at.map(|t| t.to_rfc3339()),
                Some("2099-01-01T00:00:00+00:00".to_string())
            );
        }

        #[test]
        fn the_access_model_reports_the_wallet_as_the_identity() {
            let mut session = session("access", Some("2099-01-01T00:00:00Z"));
            session.user_id = session.wallet.clone();
            session.tba = None;

            let info = projected(&session);
            assert_eq!(info.identity, rub3::Identity::Access);
            assert_eq!(info.user_id.as_str(), info.wallet.to_string());
        }

        /// A tier-4 session has no TTL, and the SDK's `is_expired` has to read
        /// that as "does not expire" rather than as "expired now".
        #[test]
        fn a_session_with_no_ttl_projects_to_no_expiry() {
            let info = projected(&session("access", None));
            assert!(info.expires_at.is_none());
            assert!(!info.is_expired());
        }

        /// Understating an expiry is the one way this projection could mislead an
        /// application, so a timestamp it cannot read becomes a refusal to report
        /// the session rather than a session with the TTL quietly dropped.
        #[test]
        fn an_unreadable_expiry_refuses_the_session_rather_than_dropping_the_ttl() {
            match offer(&session("access", Some("not-a-date"))) {
                Offer::None(reason) => assert!(reason.contains("expires_at"), "{reason}"),
                Offer::Session(_) => panic!("a session with an unreadable TTL must not be served"),
            }
        }

        /// The wrapper may know an identity model this SDK build does not. It is
        /// carried through rather than mapped onto one of the two known ones,
        /// which would misreport what the contract declared.
        #[test]
        fn an_unknown_identity_model_is_carried_through() {
            let info = projected(&session("delegated", Some("2099-01-01T00:00:00Z")));
            assert_eq!(
                info.identity,
                rub3::Identity::Other("delegated".to_string())
            );
        }
    }

    // ── The real endpoint ────────────────────────────────────────────────────

    /// The whole channel over a real socket, driven by the SDK's own client:
    /// serve, publish the address, ask both questions.
    ///
    /// The end-to-end proof through two real processes is `tests/sdk_e2e.rs`.
    /// This one exists because it compiles in every bundle that has the feature,
    /// including tier-0, where there is no session model for the process-level
    /// suite to seed.
    #[test]
    fn a_served_channel_answers_the_sdk_client_over_a_real_endpoint() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let channel = serve(Offer::None("tier-0 build".to_string())).expect("the channel starts");
        std::env::set_var(wire::ADDRESS_ENV, channel.address());

        let alive = rub3::try_heartbeat();
        let session = rub3::try_session();

        // Dropping the channel stops it, and the address must then stop
        // answering: this is what makes a dead wrapper detectable.
        let address = channel.address().to_os_string();
        drop(channel);
        let after = rub3::try_heartbeat();
        std::env::remove_var(wire::ADDRESS_ENV);

        alive.expect("a live channel answers a heartbeat");
        assert!(
            matches!(session, Err(rub3::Error::NoSession(_))),
            "got {session:?}"
        );
        assert!(
            matches!(after, Err(rub3::Error::Unreachable(_))),
            "a dropped channel must stop answering, got {after:?}"
        );
        assert!(
            !std::path::Path::new(&address).exists(),
            "the endpoint should be removed with the channel"
        );
    }

    /// One process serving several channels at once, each on its own endpoint.
    /// An endpoint name unique only per *process* collides here rather than on
    /// a second launch, and the create fails rather than retrying.
    #[cfg(unix)]
    #[test]
    fn several_channels_served_from_one_process_get_distinct_live_endpoints() {
        let channels: Vec<Channel> = (0..8)
            .map(|i| serve(Offer::None(format!("channel {i}"))).expect("every channel starts"))
            .collect();

        let mut addresses: Vec<_> = channels
            .iter()
            .map(|c| c.address().to_os_string())
            .collect();
        addresses.sort();
        addresses.dedup();
        assert_eq!(
            addresses.len(),
            channels.len(),
            "two channels were published at the same address"
        );

        for channel in &channels {
            std::os::unix::net::UnixStream::connect(channel.address())
                .unwrap_or_else(|e| panic!("{:?} should be listening: {e}", channel.address()));
        }
    }

    /// The keep-alive contract `serve_connection` documents, over a real socket:
    /// one connection, two requests, and the reader idle between them.
    ///
    /// The idle gaps are the whole point, and they are why the `Loopback` tests
    /// above cannot stand in for this one. A scripted in-memory stream always
    /// has the next request already waiting, so it never exercises the read that
    /// finds nothing there yet - the read a socket still carrying the listener's
    /// O_NONBLOCK answers with EAGAIN, and this side then reports as a malformed
    /// request before closing the connection.
    #[cfg(unix)]
    #[test]
    fn two_requests_on_one_idle_real_connection_are_both_answered() {
        use std::io::BufRead;
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        /// Long enough that the accept loop has picked the connection up and
        /// reached its first read before anything is written to it.
        const IDLE: Duration = Duration::from_millis(150);

        let channel = serve(Offer::None("tier-0 build".to_string())).expect("the channel starts");
        let mut stream = UnixStream::connect(channel.address()).expect("the channel accepts");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read deadline, so a silent server fails rather than hangs");
        let mut reader = BufReader::new(stream.try_clone().expect("a duplicated handle"));

        let mut got = Vec::new();
        for op in ["heartbeat", "session"] {
            // Idle first, so the server is already blocked in a read when the
            // request arrives rather than finding it buffered.
            std::thread::sleep(IDLE);

            let request = format!("{{\"protocol\":1,\"op\":\"{op}\"}}\n");
            stream
                .write_all(request.as_bytes())
                .expect("the connection should still accept a request");
            stream.flush().expect("flush");

            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("an answer");
            assert!(
                read > 0,
                "the connection closed instead of answering {op}: got {got:?}"
            );
            got.push(serde_json::from_str::<serde_json::Value>(&line).expect("one JSON line"));
        }

        assert_eq!(got[0]["result"], "alive", "{got:?}");
        assert_eq!(got[1]["result"], "no_session", "{got:?}");
    }

    // ── The endpoint's directory ─────────────────────────────────────────────

    /// `serve` can fail after the directory exists - the bind, the listener's
    /// mode, the accept thread - and the `Channel` that would remove it is never
    /// built on those paths. The guard is what covers all of them, so it is
    /// tested as what it is: armed it removes the directory, handed on it does
    /// not.
    #[cfg(unix)]
    #[test]
    fn an_endpoint_directory_outlives_its_guard_only_once_a_channel_owns_it() {
        let guard = platform::EndpointDir::create().expect("the directory is created");
        let abandoned = guard.path().to_path_buf();
        assert!(abandoned.is_dir(), "the guard should have created it");
        drop(guard);
        assert!(
            !abandoned.exists(),
            "a `serve` that gave up left {abandoned:?} behind"
        );

        let guard = platform::EndpointDir::create().expect("the directory is created");
        let kept = guard.keep();
        assert!(
            kept.is_dir(),
            "the directory a Channel now owns must survive: {kept:?}"
        );
        std::fs::remove_dir_all(&kept).expect("cleanup");
    }

    /// The sweep collects what a killed wrapper left behind, and - the direction
    /// that matters, because this code deletes directories - leaves everything
    /// else exactly where it is: a live wrapper's endpoint, and any name it
    /// cannot account for.
    ///
    /// Both pids are real ones. The dead one is a child that has been waited
    /// for, so the kernel is certain it is gone rather than the test guessing at
    /// an unused number; the live one is *another* running process rather than
    /// this one, so the decoy is answered by the liveness check itself and not
    /// by the separate "skip our own" rule. A third directory covers that rule.
    #[cfg(unix)]
    #[test]
    fn the_sweep_collects_dead_endpoints_and_never_a_live_or_foreign_one() {
        let base = tempfile::tempdir().expect("tempdir");

        let dead = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("a child to outlive");
        let dead_pid = dead.id();
        dead.wait_with_output().expect("the child exits");

        let mut other = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("a child to outlive the sweep");
        let other_pid = other.id();

        let concurrent = base.path().join(format!("rub3-{other_pid}-123-0"));
        let ours = base
            .path()
            .join(format!("rub3-{}-123-0", std::process::id()));
        let stale = base.path().join(format!("rub3-{dead_pid}-456-0"));
        let foreign = base.path().join("rub3-notes");
        for dir in [&concurrent, &ours, &stale, &foreign] {
            std::fs::create_dir(dir).expect("the fixture directory is created");
            std::fs::write(dir.join("s"), b"").expect("something inside it");
        }

        platform::sweep_stale_endpoints(base.path());
        let _ = other.kill();
        let _ = other.wait();

        assert!(
            !stale.exists(),
            "a dead wrapper's endpoint should have been collected: {stale:?}"
        );
        assert!(
            concurrent.is_dir(),
            "another live wrapper's endpoint must survive the sweep: {concurrent:?}"
        );
        assert!(
            ours.is_dir(),
            "this process's own endpoint must survive the sweep: {ours:?}"
        );
        assert!(
            foreign.is_dir(),
            "a name this module did not write must survive the sweep: {foreign:?}"
        );
    }

    /// A sweep is not allowed to be a reason a launch fails, so a base directory
    /// that is not there is a no-op rather than a panic or an error.
    #[cfg(unix)]
    #[test]
    fn a_sweep_of_a_directory_that_is_not_there_is_harmless() {
        let base = tempfile::tempdir().expect("tempdir");
        let missing = base.path().join("gone");
        platform::sweep_stale_endpoints(&missing);
    }
}
