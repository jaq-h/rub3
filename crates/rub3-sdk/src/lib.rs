//! Ask the rub3 wrapper who is running this application.
//!
//! A wrapped application is launched by `rub3-wrapper` as a child process: the
//! wrapper verifies the licence on the machine, then execs the application. This
//! crate is the channel back the other way - two calls the application makes to
//! find out that the wrapper is still there, and who it says the user is.
//!
//! At the top of the application's `main`:
//!
//! ```no_run
//! // Fail loudly and immediately if this build was not launched by a wrapper.
//! rub3::heartbeat();
//!
//! let session = rub3::session();
//! println!("licensed to {} on token {}", session.user_id, session.token_id);
//!
//! // Every stored row hangs off `user_id`. Never off `session.wallet`.
//! let state_dir = std::path::Path::new("state").join(session.user_id.as_str());
//! std::fs::create_dir_all(state_dir).unwrap();
//! ```
//!
//! # What [`heartbeat`] proves, and what it does not
//!
//! It is an **honest-integration and liveness check**. That is the whole claim,
//! and the transport is chosen to match it rather than to imply more.
//!
//! What a successful call establishes:
//!
//! - This process was launched by a rub3 wrapper, which had already verified a
//!   licence before it launched anything - the wrapper is the gate, and the gate
//!   ran before this code did.
//! - That wrapper is still alive and still answering. A wrapper that died, was
//!   killed, or wedged its serving thread stops answering, and a long-running
//!   application that polls finds out.
//! - The channel address the wrapper published is reachable, so the application
//!   is wired up the way its developer intended.
//!
//! What it does **not** establish, stated here rather than implied away:
//!
//! - **It is not a licence check.** Licensing is enforced by the wrapper before
//!   launch. This call re-verifies no signature, reads no chain, and consults no
//!   contract. A live heartbeat says a wrapper is there, not that anything it
//!   decided was correct.
//! - **It does not resist a determined local attacker, and cannot.** Anyone who
//!   can run the wrapped binary outside its wrapper controls the machine it runs
//!   on: they can set [`wire::ADDRESS_ENV`] to a socket of their own and answer
//!   every request however they like. There is no secret this crate could check
//!   that such an attacker does not also hold, because the checking code ships
//!   inside the binary they already control. Authentication on this channel
//!   would be theatre, so there is none.
//! - **It is not a tamper-evidence mechanism.** A patched application simply
//!   does not call it. Nothing here observes the binary that runs.
//! - **It says nothing about the session's current validity.** The wrapper
//!   answers from the session it launched on; see [`SessionInfo::is_expired`].
//!
//! So the failures it genuinely catches are the ordinary ones: an application
//! run directly instead of through its wrapper, a wrapper that died mid-run, a
//! stale or absent channel address, a wrapper that serves no channel at all, a
//! version-skewed pair. Those are worth catching, they are common, and a plain
//! local socket catches all of them, each as its own [`Error`] rather than as
//! one undifferentiated "no wrapper" - which of them a developer hit is the
//! whole of what they need to know next.
//! rub3's enforcement lives on-chain and in the wrapper's pre-launch check;
//! `architecture.md` -> "Security Model" owns that side.
//!
//! # Key persistent data on `user_id`, never on `wallet`
//!
//! A licence NFT can be sold, gifted, or moved to a fresh key. The wallet that
//! signed today's session is therefore not a stable name for anybody: key it,
//! and the first transfer or key rotation orphans the user's data behind an
//! address nobody holds any more.
//!
//! [`SessionInfo::user_id`] is the name that survives. Under the access model it
//! is the holder's wallet address; under the account model it is the token's
//! ERC-6551 account address, which does not move when the token does. The
//! wrapper resolves that difference before the application sees it, so
//! application code never needs to branch on the model.
//!
//! The types enforce the rule so a code review does not have to. [`UserId`]
//! implements `Hash`, `Eq`, `Ord`, `Display` and `AsRef<str>`, so using it as a
//! map key or a path segment is the path of least resistance. [`Wallet`]
//! implements none of those - only `Display` - so keying anything on it does not
//! compile.
//!
//! # Failure behaviour
//!
//! [`heartbeat`] and [`session`] panic, which is what
//! `implementation.md` §3.5 specifies and what an assertion about a broken
//! integration should do: an application that requires a wrapper and does not
//! have one is misconfigured, and continuing would produce a worse failure
//! later. [`try_heartbeat`] and [`try_session`] are the same checks with an
//! [`Error`] instead, for an application that would rather degrade than die.

use std::fmt;

mod info;
mod transport;
pub mod wire;

pub use info::{Identity, SessionInfo, UserId, Wallet};

/// Why a call to the wrapper did not succeed.
#[derive(Debug)]
pub enum Error {
    /// No channel address in the environment: this process was almost certainly
    /// not launched by a rub3 wrapper.
    NotWrapped,
    /// A wrapper launched this application and published no channel to talk to:
    /// one built without its `sdk` feature, or one whose channel failed to
    /// start. Distinct from [`Error::NotWrapped`] on purpose - there *is* a
    /// wrapper here, so "launch this through the wrapper" is not the fix.
    NoChannel,
    /// There is an address, but nothing is listening on it. A wrapper that
    /// exited leaves exactly this.
    Unreachable(std::io::Error),
    /// The connection failed part-way through the exchange.
    Transport(std::io::Error),
    /// The wrapper answered something this SDK cannot make sense of.
    Protocol(String),
    /// The wrapper speaks a different version of the protocol. The two halves
    /// are packaged separately, so this is a deployment mismatch: repack the
    /// wrapper, or build the application against the matching SDK.
    ProtocolVersion { expected: u32, found: u32 },
    /// The wrapper is alive and has no session to report. A tier-0 build has no
    /// session model at all; a wrapper below tier-3 has one but no `cooldown`
    /// capability, which is what mints a session, so every launch there comes
    /// from the legacy licence proof; and that proof predates the identity
    /// model, so it carries no `user_id`.
    NoSession(String),
    /// The wrapper understood the request and refused or failed it.
    Wrapper(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotWrapped => write!(
                f,
                "no rub3 wrapper: {} is not set, so this process was not launched by one",
                wire::ADDRESS_ENV
            ),
            Error::NoChannel => write!(
                f,
                "the rub3 wrapper that launched this application serves no channel: it was \
                 built without its `sdk` feature, or its channel failed to start"
            ),
            Error::Unreachable(e) => write!(f, "the rub3 wrapper is not answering: {e}"),
            Error::Transport(e) => write!(f, "the rub3 wrapper connection failed: {e}"),
            Error::Protocol(m) => write!(f, "unexpected answer from the rub3 wrapper: {m}"),
            Error::ProtocolVersion { expected, found } => write!(
                f,
                "rub3 protocol mismatch: this application speaks version {expected}, \
                 the wrapper speaks {found}"
            ),
            Error::NoSession(reason) => {
                write!(f, "the rub3 wrapper reports no session: {reason}")
            }
            Error::Wrapper(m) => write!(f, "the rub3 wrapper refused the request: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Unreachable(e) | Error::Transport(e) => Some(e),
            _ => None,
        }
    }
}

/// Confirms a live rub3 wrapper is on the other end of the channel, and panics
/// if it is not.
///
/// Read "What [`heartbeat`] proves, and what it does not" in the crate
/// documentation before relying on this for anything. In short: it catches a
/// broken integration and a dead wrapper. It is not a licence check and not a
/// defence against someone who controls the machine.
///
/// Call it once at startup. Long-running applications - an MCP server, a daemon
/// - can call it periodically to notice a wrapper that went away.
///
/// # Panics
///
/// On any [`Error`]. Use [`try_heartbeat`] to handle the failure instead.
pub fn heartbeat() {
    if let Err(e) = try_heartbeat() {
        panic!("{}", panic_message(&e));
    }
}

/// [`heartbeat`] without the panic.
pub fn try_heartbeat() -> Result<(), Error> {
    match transport::request(wire::Request::Heartbeat)? {
        wire::Response::Alive => Ok(()),
        wire::Response::Error { message } => Err(Error::Wrapper(message)),
        other => Err(Error::Protocol(format!(
            "expected a heartbeat answer, got {}",
            describe(&other)
        ))),
    }
}

/// Returns the session this application was launched on, and panics if the
/// wrapper cannot report one.
///
/// # Panics
///
/// On any [`Error`], including a live wrapper that has no session to report.
/// Use [`try_session`] to handle those separately.
pub fn session() -> SessionInfo {
    match try_session() {
        Ok(session) => session,
        Err(e) => panic!("{}", panic_message(&e)),
    }
}

/// [`session`] without the panic.
pub fn try_session() -> Result<SessionInfo, Error> {
    match transport::request(wire::Request::Session)? {
        wire::Response::Session { session } => Ok(session),
        wire::Response::NoSession { reason } => Err(Error::NoSession(reason)),
        wire::Response::Error { message } => Err(Error::Wrapper(message)),
        other => Err(Error::Protocol(format!(
            "expected a session answer, got {}",
            describe(&other)
        ))),
    }
}

/// The panic text. One line saying what failed, one saying what to do about it,
/// because the reader is usually a developer who has just run a wrapped binary
/// directly and does not yet know that is what happened.
fn panic_message(e: &Error) -> String {
    let hint = match e {
        Error::NotWrapped | Error::Unreachable(_) => {
            "launch this binary through the rub3 wrapper: \
             rub3-wrapper --binary <this binary>"
        }
        Error::NoChannel => {
            "a wrapper did launch this application, so repack it with the `sdk` feature; \
             if it has one, its stderr says why the channel could not start"
        }
        Error::ProtocolVersion { .. } => {
            "the wrapper and this application were built against different \
             rub3 protocol versions; repack the wrapper"
        }
        Error::NoSession(_) => {
            "this wrapper build launched without a session it can report; a session needs the \
             `cooldown` capability - tier-3 or higher, or the headless front door - since \
             below that every launch is served from the legacy licence proof"
        }
        Error::Transport(_) | Error::Protocol(_) | Error::Wrapper(_) => {
            "the wrapper is reachable but the exchange failed; \
             run with the wrapper's stderr visible"
        }
    };
    format!("rub3: {e}\nrub3: {hint}")
}

fn describe(r: &wire::Response) -> &'static str {
    match r {
        wire::Response::Alive => "a heartbeat answer",
        wire::Response::Session { .. } => "a session",
        wire::Response::NoSession { .. } => "no session",
        wire::Response::Error { .. } => "an error",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// One process-wide guard for the tests that set or clear the channel
    /// address. `set_var` mutates state shared by the whole test binary and
    /// libtest runs tests on parallel threads, so a `setenv` racing a `getenv`
    /// elsewhere is a genuine data race. Mirrors `rub3_wrapper::ENV_LOCK`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn session_json() -> &'static str {
        r#"{
            "app_id": "com.rub3.example",
            "token_id": 7,
            "user_id": "0x00000000000000000000000000000000000000bb",
            "wallet": "0x00000000000000000000000000000000000000aa",
            "identity": "account",
            "expires_at": "2099-01-01T00:00:00Z"
        }"#
    }

    // ── The wire format ──────────────────────────────────────────────────────

    /// `SessionInfo` carries exactly the six fields `implementation.md` §3.5
    /// names. The wrapper holds the signature, the nonce, the activation
    /// transaction and the device key, and an application has no business with
    /// any of them, so widening this struct is the mistake this test exists to
    /// catch.
    #[test]
    fn session_info_carries_exactly_the_six_specified_fields() {
        let info: SessionInfo = serde_json::from_str(session_json()).unwrap();
        let value = serde_json::to_value(&info).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("SessionInfo serializes as an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "app_id",
                "expires_at",
                "identity",
                "token_id",
                "user_id",
                "wallet"
            ],
        );
    }

    #[test]
    fn session_round_trips_through_the_wire_format() {
        let info: SessionInfo = serde_json::from_str(session_json()).unwrap();
        assert_eq!(info.app_id, "com.rub3.example");
        assert_eq!(info.token_id, 7);
        assert_eq!(
            info.user_id.as_str(),
            "0x00000000000000000000000000000000000000bb"
        );
        assert_eq!(
            info.wallet.to_string(),
            "0x00000000000000000000000000000000000000aa"
        );
        assert_eq!(info.identity, Identity::Account);

        let back: SessionInfo =
            serde_json::from_str(&serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(back.user_id, info.user_id);
        assert_eq!(back.expires_at, info.expires_at);
    }

    #[test]
    fn a_session_without_a_ttl_deserializes_and_never_expires() {
        let json = session_json().replace(r#""2099-01-01T00:00:00Z""#, "null");
        let info: SessionInfo = serde_json::from_str(&json).unwrap();
        assert!(info.expires_at.is_none());
        assert!(!info.is_expired(), "no TTL means no expiry (tier 4)");
    }

    #[test]
    fn a_past_ttl_reads_as_expired() {
        let json = session_json().replace("2099-01-01", "2000-01-01");
        let info: SessionInfo = serde_json::from_str(&json).unwrap();
        assert!(info.is_expired());
    }

    /// The wrapper and the application are packaged separately and can be built
    /// from different revisions, so an identity model this SDK has never heard
    /// of has to survive deserialization rather than sink the whole session.
    #[test]
    fn an_unknown_identity_model_is_carried_through_rather_than_rejected() {
        let json = session_json().replace("account", "delegated");
        let info: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info.identity, Identity::Other("delegated".to_string()));
        assert_eq!(info.identity.as_str(), "delegated");
        // And it survives a round trip unchanged, so a proxy of this crate does
        // not silently rewrite what the wrapper said.
        let back: SessionInfo =
            serde_json::from_str(&serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(back.identity, info.identity);
    }

    #[test]
    fn the_request_and_response_envelopes_carry_the_protocol_version() {
        let line = serde_json::to_string(&wire::Envelope::new(wire::Request::Heartbeat)).unwrap();
        assert_eq!(line, r#"{"protocol":1,"op":"heartbeat"}"#);

        let line = serde_json::to_string(&wire::Envelope::new(wire::Response::Alive)).unwrap();
        assert_eq!(line, r#"{"protocol":1,"result":"alive"}"#);
    }

    #[test]
    fn a_line_past_the_ceiling_is_an_error_rather_than_a_truncated_parse() {
        let mut line = vec![b'x'; (wire::MAX_LINE_BYTES + 10) as usize];
        line.push(b'\n');
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(line));
        let err = wire::read_message::<_, wire::Request>(&mut reader).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeds"),
            "the error should name the ceiling, not the JSON: {err}"
        );
    }

    #[test]
    fn a_closed_stream_reads_as_no_message_rather_than_an_error() {
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(Vec::new()));
        let got = wire::read_message::<_, wire::Request>(&mut reader).unwrap();
        assert!(got.is_none());
    }

    // ── The documented failure with no wrapper ────────────────────────────────

    #[test]
    fn without_a_channel_address_the_failure_is_not_wrapped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(wire::ADDRESS_ENV);

        assert!(matches!(try_heartbeat(), Err(Error::NotWrapped)));
        assert!(matches!(try_session(), Err(Error::NotWrapped)));
    }

    /// An empty value is the shape a shell leaves behind (`RUB3_SDK_SOCKET=`),
    /// and it means the same thing as absent. Treating it as an address would
    /// report a confusing connect error instead.
    #[test]
    fn an_empty_channel_address_reads_as_not_wrapped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(wire::ADDRESS_ENV, "");
        let got = try_heartbeat();
        std::env::remove_var(wire::ADDRESS_ENV);

        assert!(matches!(got, Err(Error::NotWrapped)), "got {got:?}");
    }

    #[test]
    fn an_address_nothing_listens_on_is_unreachable_rather_than_not_wrapped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rub3-sdk-absent-{}", std::process::id()));
        std::env::set_var(wire::ADDRESS_ENV, dir.join("nothing-here.sock"));
        let got = try_heartbeat();
        std::env::remove_var(wire::ADDRESS_ENV);

        assert!(matches!(got, Err(Error::Unreachable(_))), "got {got:?}");
    }

    /// A wrapper that serves no channel publishes the sentinel rather than
    /// leaving the variable unset, and it must not read as "no wrapper": the
    /// advice that failure carries is to launch through a wrapper, which is
    /// exactly what the developer already did.
    #[test]
    fn the_no_channel_sentinel_reads_as_a_wrapper_without_a_channel() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(wire::ADDRESS_ENV, wire::ADDRESS_NO_CHANNEL);
        let alive = try_heartbeat();
        let session = try_session();
        std::env::remove_var(wire::ADDRESS_ENV);

        assert!(matches!(alive, Err(Error::NoChannel)), "got {alive:?}");
        assert!(matches!(session, Err(Error::NoChannel)), "got {session:?}");
        assert!(
            panic_message(&Error::NoChannel).contains("`sdk` feature"),
            "the way out is to repack the wrapper: {}",
            panic_message(&Error::NoChannel)
        );
    }

    /// The version is checked before the body is parsed, which is what the
    /// wrapper does with our requests and what [`wire::PROTOCOL_VERSION`]
    /// promises. A protocol 2 wrapper may answer in a shape this build has never
    /// seen; reported as a parse failure it would send its reader after a broken
    /// connection rather than a mismatched pair.
    #[test]
    fn an_answer_at_another_protocol_version_reports_the_mismatch_rather_than_the_parse() {
        let peer = tests_support::Duplex::answering(r#"{"protocol":2,"shape":"unheard of"}"#);
        let got = transport::exchange(peer, wire::Request::Heartbeat);

        assert!(
            matches!(
                got,
                Err(Error::ProtocolVersion {
                    expected: 1,
                    found: 2
                })
            ),
            "got {got:?}"
        );
    }

    /// The same shape at *this* version really is a broken answer, so the two
    /// failures stay distinguishable.
    #[test]
    fn an_unparseable_answer_at_this_version_is_a_protocol_error() {
        let peer = tests_support::Duplex::answering(r#"{"protocol":1,"shape":"unheard of"}"#);
        let got = transport::exchange(peer, wire::Request::Heartbeat);

        assert!(matches!(got, Err(Error::Protocol(_))), "got {got:?}");
    }

    mod tests_support {
        use std::io::{self, Read, Write};

        /// A peer in memory: it answers with one scripted line and swallows
        /// whatever is written to it, which is all the exchange needs of a
        /// socket.
        pub struct Duplex {
            answer: io::Cursor<Vec<u8>>,
        }

        impl Duplex {
            pub fn answering(line: &str) -> Self {
                let mut bytes = line.as_bytes().to_vec();
                bytes.push(b'\n');
                Self {
                    answer: io::Cursor::new(bytes),
                }
            }
        }

        impl Read for Duplex {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.answer.read(buf)
            }
        }

        impl Write for Duplex {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
    }

    /// The panic text is the whole user interface for the commonest failure -
    /// a developer running a wrapped binary directly - so it says what happened
    /// and what to do next.
    #[test]
    fn the_panic_message_names_the_variable_and_the_way_out() {
        let text = panic_message(&Error::NotWrapped);
        assert!(text.contains(wire::ADDRESS_ENV), "{text}");
        assert!(text.contains("rub3-wrapper --binary"), "{text}");
    }

    #[test]
    fn every_error_reads_as_finished_prose() {
        let errors = [
            Error::NotWrapped,
            Error::NoChannel,
            Error::Unreachable(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
            Error::Transport(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            Error::Protocol("nonsense".to_string()),
            Error::ProtocolVersion {
                expected: 1,
                found: 2,
            },
            Error::NoSession("tier-0 build".to_string()),
            Error::Wrapper("no".to_string()),
        ];
        for e in errors {
            let text = e.to_string();
            assert!(
                text.starts_with("no ")
                    || text.starts_with("the ")
                    || text.starts_with("rub3 ")
                    || text.starts_with("unexpected "),
                "{text}"
            );
            assert!(!text.ends_with(' '), "{text}");
            let message = panic_message(&e);
            assert_eq!(message.lines().count(), 2, "{message}");
        }
    }
}
