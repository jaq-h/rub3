//! Test-only scaffolding shared by more than one module.
//!
//! Nothing here is compiled into a shipped binary: `lib.rs` declares the module
//! `#[cfg(test)]`. It exists because the alternative to sharing [`StubNode`] is
//! a second copy of it, and two copies of a socket server drift in exactly the
//! details that made the first one work.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A local HTTP endpoint standing in for a JSON-RPC node.
///
/// Answers either the same canned body every time ([`StubNode::serving`]) or a
/// result chosen from the method each request names ([`StubNode::routed`]).
///
/// Each instance binds its own ephemeral port, so nothing is shared between
/// tests and they may run in parallel. The port is bound before the URL is
/// handed out, which is what makes the endpoint reachable the instant a
/// test can name it: a bound listening socket queues incoming connections
/// in the kernel whether or not the accept loop has reached `accept` yet.
pub struct StubNode {
    pub url: String,
    shutdown: Arc<AtomicBool>,
    #[allow(dead_code)]
    requests: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Turns a JSON-RPC method name and its params into the `result` to send back.
type Responder = dyn Fn(&str, &serde_json::Value) -> serde_json::Value + Send + Sync;

/// How a [`StubNode`] decides what to send back.
enum Reply {
    /// The same body for every request, whatever it asked.
    Fixed(&'static str),
    /// A JSON-RPC `result` chosen from the method name and params. Wrapped in
    /// the envelope by [`answer`], so a route says only what it answers.
    ///
    /// Unused below tier 3, where nothing makes more than one kind of call.
    #[cfg_attr(not(feature = "onchain-write"), allow(dead_code))]
    Routed(Box<Responder>),
}

impl StubNode {
    pub fn serving(body: &'static str) -> Self {
        Self::with_reply(Reply::Fixed(body))
    }

    /// A node that answers each JSON-RPC call from its method name.
    ///
    /// What [`StubNode::serving`] cannot do: a flow that makes several
    /// different calls in one poll - `lastActivationBlock`, then the logs of
    /// the block it named - needs a different answer to each, and one canned
    /// body would have the first call's shape stand in for both. The responder
    /// returns the JSON-RPC `result` only.
    #[cfg_attr(not(feature = "onchain-write"), allow(dead_code))]
    pub fn routed<F>(responder: F) -> Self
    where
        F: Fn(&str, &serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    {
        Self::with_reply(Reply::Routed(Box::new(responder)))
    }

    /// How many complete requests this node has answered.
    ///
    /// The only way to observe a polling loop from outside it: a loop that was
    /// asked to stop and did stops adding to this, and one that leaked goes on
    /// adding to it whether or not anything is still listening.
    // Read only by the §5.1a watch-cancellation tests, which need both a
    // watch to start (`onchain-write`) and the window that starts one
    // (`webview`). Spelling that pair out at two more call sites costs more
    // than it explains.
    #[allow(dead_code)]
    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    fn with_reply(reply: Reply) -> Self {
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
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if answer(stream, &reply) {
                            counter.fetch_add(1, Ordering::Relaxed);
                        }
                    }
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
            requests,
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

/// Answers one accepted connection.
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
///
/// Reports whether a whole request was read and answered, which is what the
/// request counter counts: a half-open connection is not a question asked.
fn answer(mut stream: TcpStream, reply: &Reply) -> bool {
    if stream.set_nonblocking(false).is_err() {
        return false;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let request = match drain_request(&mut stream) {
        Some(r) => r,
        None => return false,
    };

    let body = match reply {
        Reply::Fixed(body) => (*body).to_string(),
        Reply::Routed(responder) => {
            let call: serde_json::Value = serde_json::from_slice(&request).unwrap_or_default();
            let method = call["method"].as_str().unwrap_or_default();
            let result = responder(method, &call["params"]);
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": call.get("id").cloned().unwrap_or(serde_json::json!(0)),
                "result": result,
            })
            .to_string()
        }
    };

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
    true
}

/// Reads one complete HTTP request: the headers, then exactly the body
/// length they declare. Returns the body, or `None` when the request never
/// arrived in full.
fn drain_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
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
                return Some(buf[head_end + 4..head_end + 4 + length].to_vec());
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}
