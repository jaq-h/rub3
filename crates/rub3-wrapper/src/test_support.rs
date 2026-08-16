//! Test-only scaffolding shared by more than one module.
//!
//! Nothing here is compiled into a shipped binary: `lib.rs` declares the module
//! `#[cfg(test)]`. It exists because the alternative to sharing [`StubNode`] is
//! a second copy of it, and two copies of a socket server drift in exactly the
//! details that made the first one work.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A local HTTP endpoint that answers every request with the same body.
///
/// Each instance binds its own ephemeral port, so nothing is shared between
/// tests and they may run in parallel. The port is bound before the URL is
/// handed out, which is what makes the endpoint reachable the instant a
/// test can name it: a bound listening socket queues incoming connections
/// in the kernel whether or not the accept loop has reached `accept` yet.
pub struct StubNode {
    pub url: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubNode {
    pub fn serving(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub node");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        // The accept loop polls a shutdown flag, so it must not park in
        // `accept` forever. This flag is for the listener alone; see
        // `answer` for why it must not reach the accepted stream.
        listener
            .set_nonblocking(true)
            .expect("stub node non-blocking");

        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => answer(stream, body),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            url,
            shutdown,
            handle: Some(handle),
        }
    }
}

impl Drop for StubNode {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Answers one accepted connection with `body`.
///
/// Two details here are load-bearing, and getting either wrong makes the
/// stub answer a request it never read, which the client reports as a send
/// failure rather than as the response it was handed:
///
/// 1. `accept` on the BSD socket layer returns a stream that inherits the
///    listener's non-blocking flag, so the accepted stream is put back into
///    blocking mode. Otherwise the first `read` returns `WouldBlock` the
///    moment the connection lands, before the request has arrived, and
///    `set_read_timeout` has no blocking read to apply to.
/// 2. The request is drained in full before the socket closes. Closing a
///    socket with bytes still queued unread sends a reset instead of a
///    clean shutdown, and the client sees the reset in place of the reply.
fn answer(mut stream: TcpStream, body: &str) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    if !drain_request(&mut stream) {
        return;
    }

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

/// Reads one complete HTTP request: the headers, then exactly the body
/// length they declare. Reports whether a whole request arrived.
fn drain_request(stream: &mut TcpStream) -> bool {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            // A request with no declared length carries no body.
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= head_end + 4 + length {
                return true;
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}
