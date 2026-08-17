//! The wrapper-to-application protocol: one request per line, one response per
//! line, JSON both ways.
//!
//! This module is public because the wrapper serves what it defines and the two
//! halves must agree byte for byte. Applications use [`crate::heartbeat`] and
//! [`crate::session`] instead; nothing here needs to be read to use the SDK.
//!
//! Line-delimited JSON over a stream, rather than a length-prefixed frame or a
//! request-per-connection, because it costs nothing over either and stays
//! legible: `nc -U "$RUB3_SDK_SOCKET"` plus a typed line is a working client,
//! which is worth a lot when the question is "is the channel up".

use std::io::{self, BufRead, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::SessionInfo;

/// Environment variable through which the wrapper hands the channel's address
/// to the application it launches: a socket path on unix, a named-pipe name on
/// Windows.
///
/// Its absence is how an application launched *without* a wrapper is
/// recognised, so the wrapper must set it on the child and nowhere else.
pub const ADDRESS_ENV: &str = "RUB3_SDK_SOCKET";

/// Protocol version. Both sides send it and both sides check it.
///
/// A wrapper and the application it launches are packaged separately - the
/// wrapper is built at `rub3 pack` time, the application links this crate at
/// its own build time - so a mismatched pair is an ordinary deployment state,
/// not corruption, and it is reported as one rather than as a parse failure.
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest line either side will read before giving up, in bytes.
///
/// Framing is line-delimited, so without a ceiling a peer that never sends a
/// newline can make the reader buffer without bound.
pub const MAX_LINE_BYTES: u64 = 64 * 1024;

/// What the application asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// "Are you there?" Answered by [`Response::Alive`].
    Heartbeat,
    /// "Who is running me?" Answered by [`Response::Session`] or
    /// [`Response::NoSession`].
    Session,
}

/// What the wrapper answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    /// The wrapper is alive and served this request.
    Alive,
    /// The session the wrapper launched this application on.
    Session { session: SessionInfo },
    /// The wrapper is alive but launched without a session it can report -
    /// a tier-0 build, or a launch served from the legacy licence proof, which
    /// carries no identity model and therefore no `user_id`.
    NoSession { reason: String },
    /// The wrapper understood the request and could not serve it.
    Error { message: String },
}

/// A message plus the protocol version it was written at.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub protocol: u32,
    #[serde(flatten)]
    pub body: T,
}

impl<T> Envelope<T> {
    /// Wraps `body` at this build's [`PROTOCOL_VERSION`].
    pub fn new(body: T) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            body,
        }
    }
}

/// Writes one message as a single line and flushes it.
pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &Envelope<T>) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// Reads one message. `Ok(None)` means the peer closed the stream cleanly
/// before sending anything, which is how a well-behaved client disconnects.
///
/// A line longer than [`MAX_LINE_BYTES`] is an error rather than a truncated
/// parse: a reader that silently stopped at a ceiling would hand the parser a
/// half message and blame the JSON.
pub fn read_message<R: BufRead, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<Envelope<T>>> {
    let mut line = String::new();
    // Explicit call syntax: the receiver is already a `&mut R`, and `Read::take`
    // consumes its receiver, so `r.take(..)` would try to move out of the
    // borrow. `Take<&mut R>` is still a `BufRead`, which is what `read_line`
    // needs.
    let mut limited = io::Read::take(r, MAX_LINE_BYTES + 1);
    let read = limited.read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if read as u64 > MAX_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message exceeds {MAX_LINE_BYTES} bytes"),
        ));
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(io::Error::other)
}
