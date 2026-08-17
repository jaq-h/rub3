//! The binary really speaks MCP.
//!
//! The rest of the suite exercises the derivation through the library, which
//! proves the answers but not the wire. This drives the shipped binary the way
//! an editor does: spawn it, talk newline-delimited JSON-RPC over its stdin and
//! stdout, and read what comes back.
//!
//! Two properties are worth a process for. Handshake and tool discovery have to
//! work against the protocol as it is, not as this crate imagines it, and stdout
//! has to carry the JSON-RPC stream and nothing else - one stray `println!`
//! anywhere in the crate would corrupt every message after it, and no in-process
//! test can see that.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

/// The revision this test speaks.
///
/// `2025-11-25` rather than the newer `2026-07-28`, deliberately: it still has
/// the `initialize` handshake, so this exercises the compatibility path a
/// deployed editor is most likely to take today. The SDK negotiates both.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// A running server, its pipes, and a watchdog.
///
/// Reading a pipe has no timeout, so a server that never answers would hang the
/// suite rather than fail it. The watchdog kills the child after a deadline,
/// which closes the pipe and turns the hang into a readable failure.
struct Server {
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    finished: Arc<AtomicBool>,
    next_id: i64,
}

impl Server {
    fn start() -> Server {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/rub3-docs-mcp has two ancestors")
            .to_path_buf();

        let mut child = Command::new(env!("CARGO_BIN_EXE_rub3-docs-mcp"))
            .arg("--repo-root")
            .arg(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the docs server binary starts");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));

        let child = Arc::new(Mutex::new(child));
        let finished = Arc::new(AtomicBool::new(false));
        let watchdog_child = Arc::clone(&child);
        let watchdog_finished = Arc::clone(&finished);
        std::thread::spawn(move || {
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(500));
                if watchdog_finished.load(Ordering::SeqCst) {
                    return;
                }
            }
            let _ = watchdog_child.lock().expect("the child lock").kill();
        });

        Server {
            child,
            stdin,
            stdout,
            finished,
            next_id: 0,
        }
    }

    fn send(&mut self, message: Value) {
        let line = serde_json::to_string(&message).expect("serialisable");
        writeln!(self.stdin, "{line}").expect("the server is still listening");
        self.stdin.flush().expect("flush");
    }

    /// Sends a request and returns the whole reply, error or result.
    fn request_raw(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));

        loop {
            let mut line = String::new();
            let read = self
                .stdout
                .read_line(&mut line)
                .expect("the server's stdout is readable");
            assert!(
                read > 0,
                "the server closed stdout without answering {method}"
            );
            let message: Value = serde_json::from_str(line.trim()).unwrap_or_else(|error| {
                panic!(
                    "the server wrote a line on stdout that is not JSON-RPC ({error}): {line:?}. \
                     stdout carries the protocol, so nothing else may be printed to it."
                )
            });
            // Notifications and unrelated responses are skipped rather than
            // asserted about, so this stays a protocol test and not a
            // transcript test.
            if message.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            return message;
        }
    }

    /// Sends a request and returns its result, failing on a JSON-RPC error.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let message = self.request_raw(method, params);
        assert!(
            message.get("error").is_none(),
            "{method} failed: {}",
            message["error"]
        );
        message["result"].clone()
    }

    /// Calls a tool and returns its structured content.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        assert_ne!(
            result.get("isError"),
            Some(&Value::Bool(true)),
            "{tool} returned an error result: {result}"
        );
        result
            .get("structuredContent")
            .cloned()
            .unwrap_or_else(|| panic!("{tool} returned no structuredContent: {result}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::SeqCst);
        let mut child = self.child.lock().expect("the child lock");
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// One conversation, in order, because the handshake gates everything after it.
#[test]
fn the_binary_serves_its_tools_over_stdio() {
    let mut server = Server::start();

    let initialised = server.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "rub3-docs-mcp-test", "version": "0"}
        }),
    );
    assert_eq!(
        initialised["serverInfo"]["name"], "rub3-docs-mcp",
        "the server identifies itself: {initialised}"
    );
    assert!(
        initialised["capabilities"]["tools"].is_object(),
        "the server advertises tools: {initialised}"
    );
    let instructions = initialised["instructions"]
        .as_str()
        .expect("the server sends instructions");
    assert!(
        instructions.contains("derived") && instructions.contains("forge build"),
        "the instructions must tell a client that answers are derived and how to produce the \
         artifacts: {instructions:?}"
    );
    server.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Tool discovery. The names are asserted because they are the contract an
    // editor's configuration and an agent's prompt both depend on.
    let listed = server.request("tools/list", json!({}));
    let tools = listed["tools"].as_array().expect("a tools array");
    let mut names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("a tool name"))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "contract_abi",
            "list_contracts",
            "list_documents",
            "read_document",
            "rust_api",
            "search_documents",
        ]
    );
    for tool in tools {
        assert!(
            tool["description"].as_str().is_some_and(|d| d.len() > 40),
            "{} needs a description an agent can choose on: {tool}",
            tool["name"]
        );
        assert!(
            tool["inputSchema"].is_object(),
            "{} needs an input schema: {tool}",
            tool["name"]
        );
    }

    // A document answer, through the wire.
    let documents = server.call("list_documents", json!({}));
    let paths: Vec<&str> = documents["documents"]
        .as_array()
        .expect("a documents array")
        .iter()
        .map(|document| document["path"].as_str().expect("a path"))
        .collect();
    for expected in ["README.md", "architecture.md", "implementation.md"] {
        assert!(
            paths.contains(&expected),
            "{expected} is missing: {paths:?}"
        );
    }

    // A derived Rust answer, through the wire, checked against the source file
    // this test reads for itself. This is the end-to-end form of the crate's
    // central claim: what arrives over the protocol is what is in the file.
    let api = server.call(
        "rust_api",
        json!({"crate_name": "rub3-wrapper", "module": "activation", "name": "EXIT_COOLDOWN_ACTIVE"}),
    );
    let served = api["crates"][0]["modules"][0]["items"][0]["signature"]
        .as_str()
        .unwrap_or_else(|| panic!("no signature came back: {api}"));
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("two ancestors")
            .join("crates/rub3-wrapper/src/activation.rs"),
    )
    .expect("activation.rs reads");
    assert!(
        source.contains(served),
        "the server served {served:?} for EXIT_COOLDOWN_ACTIVE, which is not in activation.rs"
    );
    assert!(
        served.contains("EXIT_COOLDOWN_ACTIVE"),
        "the wrong item came back: {served:?}"
    );

    // A refusal, through the wire: an unknown document is an error result with
    // the inventory named, not an empty success.
    // A refusal, through the wire: an unreachable path comes back as an
    // `invalid params` error naming the inventory, not as an empty success that
    // an agent would read as "that document is empty".
    let refusal = server.request_raw(
        "tools/call",
        json!({"name": "read_document", "arguments": {"path": "../../etc/passwd"}}),
    );
    let message = refusal
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("reading outside the inventory must be refused: {refusal}"));
    assert!(
        message.contains("no document at") && message.contains("list_documents"),
        "the refusal should name the inventory: {message:?}"
    );
}
