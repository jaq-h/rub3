//! The application side of the channel: connect, send one request, read one
//! response.
//!
//! Both platforms carry the same line-delimited JSON over a stream, so only the
//! act of connecting differs. On unix that is a `UnixStream`; on Windows a
//! named-pipe client is a file handle, so `OpenOptions` opening
//! `\\.\pipe\<name>` for read and write is the whole of it and needs no
//! dependency either.

use std::ffi::OsStr;
use std::io::{self, BufReader, Read, Write};
#[cfg(unix)]
use std::time::Duration;

use crate::wire::{self, Envelope, Request, Response};
use crate::Error;

/// How long a single request/response exchange may take on unix.
///
/// The wrapper answers from memory, so anything approaching this is a wedged
/// wrapper rather than a slow one, and an application that blocked forever on
/// it would be a worse failure than the one the SDK exists to report. See
/// [`connect`] for why the Windows path has no equivalent.
#[cfg(unix)]
const TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::fs::File;

/// Sends one request over a fresh connection and returns the answer.
///
/// A connection per call, rather than a cached one: the calls are rare (a launch
/// check, a periodic liveness poll), a fresh connect is what makes a dead
/// wrapper detectable at all, and it leaves no state to go stale across a fork.
pub(crate) fn request(request: Request) -> Result<Response, Error> {
    let address = std::env::var_os(wire::ADDRESS_ENV).ok_or(Error::NotWrapped)?;
    if address.is_empty() {
        return Err(Error::NotWrapped);
    }
    if address.as_os_str() == OsStr::new(wire::ADDRESS_NO_CHANNEL) {
        return Err(Error::NoChannel);
    }

    let stream = connect(&address).map_err(Error::Unreachable)?;
    exchange(stream, request)
}

/// Generic over the stream so the version-before-body rule below can be driven
/// without a socket; production passes a [`Stream`].
pub(crate) fn exchange<S: Read + Write>(stream: S, request: Request) -> Result<Response, Error> {
    let mut writer = stream;
    wire::write_message(&mut writer, &Envelope::new(request)).map_err(Error::Transport)?;

    // The version is read before the body, the way the wrapper reads ours: a
    // future protocol may change the body's shape entirely, and a peer speaking
    // one deserves "we speak different versions" rather than a JSON complaint
    // that sends its reader looking for a broken connection.
    let mut reader = BufReader::new(writer);
    let envelope: Envelope<serde_json::Value> = wire::read_message(&mut reader)
        .map_err(Error::Transport)?
        .ok_or_else(|| {
            Error::Protocol("the wrapper closed the connection without answering".to_string())
        })?;

    if envelope.protocol != wire::PROTOCOL_VERSION {
        return Err(Error::ProtocolVersion {
            expected: wire::PROTOCOL_VERSION,
            found: envelope.protocol,
        });
    }
    serde_json::from_value(envelope.body)
        .map_err(|e| Error::Protocol(format!("the wrapper's answer did not parse: {e}")))
}

#[cfg(unix)]
fn connect(address: &OsStr) -> io::Result<Stream> {
    let stream = Stream::connect(address)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    Ok(stream)
}

/// Windows: a named-pipe client is an ordinary file handle on `\\.\pipe\<name>`,
/// so no `windows-sys` dependency is needed on this side of the channel.
///
/// Unlike the unix path this sets no read timeout. A `File` on a byte-mode pipe
/// has no equivalent of `set_read_timeout`, and the alternatives - overlapped
/// I/O, or a watchdog thread per call - would each buy a bound on one failure
/// mode at a real cost in machinery. The wrapper answers from memory and closes
/// the connection when it exits, so the unbounded case is a wrapper whose
/// serving thread is wedged while its process lives.
#[cfg(windows)]
fn connect(address: &OsStr) -> io::Result<Stream> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(address)
}

#[cfg(not(any(unix, windows)))]
compile_error!("rub3 supports Unix domain sockets and Windows named pipes only");
