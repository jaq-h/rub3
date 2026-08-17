//! The §1.8 activation flows, driven at the IPC seam.
//!
//! [`run_activation_window`](super::run_activation_window) is two things
//! welded together: a `tao` event loop owning a `wry` view, and the protocol
//! that view speaks. Only the second half decides anything. Every branch in
//! the flow, every chain read it makes, and every screen it asks for lives in
//! [`IpcState::handle`](super::IpcState), which takes the JSON the page posts
//! and answers with the JS the page would have run.
//!
//! [`Window`] wires that state to a pair of channels instead of a view, so a
//! test can post the same messages the page posts and read back the same
//! scripts. That is the highest seam these tests can reach honestly, and it
//! stops short of two things:
//!
//!   * the `wry`/`tao` layer itself - creating a view, pumping the event loop,
//!     and evaluating a script in it;
//!   * `assets/activation.html` - the JS that renders each screen, tracks
//!     `pendingSessionCtx` across the cooldown → confirm → sign hand-offs, and
//!     posts the messages back.
//!
//! Those two remain manual testing (§1.7). What is covered here is everything
//! between them: the whole Rust side of the flow. The tier-3 flows in
//! [`onchain`] drive it against a live EVM node; the zero-contract legacy path
//! below reads no chain at all, which is itself the thing it asserts.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use k256::ecdsa::SigningKey;

use super::{ActivationResult, Cmd, IpcState};

#[cfg(feature = "cooldown")]
mod onchain;

/// How long to wait on a message produced by a background thread. Generous:
/// this bounds a hang, it is not a latency assertion.
const RECV_TIMEOUT: Duration = Duration::from_secs(60);

/// One outbound call, split into the `window.rub3` method name and its single
/// JSON argument.
#[derive(Debug, Clone)]
struct Call {
    name: String,
    arg: serde_json::Value,
}

/// The activation window's IPC handler, wired to channels instead of a view.
///
/// Constructed with exactly the fields
/// [`run_activation_window`](super::run_activation_window) constructs them
/// with, so nothing here is a stand-in for production state - it is the
/// production state, minus the view it would have talked to.
struct Window {
    state: IpcState,
    cmd_rx: mpsc::Receiver<Cmd>,
    result_rx: mpsc::Receiver<ActivationResult>,
    /// Set once a `Cmd::Close` has been observed. The close and the result
    /// travel on different channels, so the flag has to survive a drain.
    closed: std::cell::Cell<bool>,
}

impl Window {
    fn open(app_id: &str, contract: &str, chain_id: u64, rpc_url: &str, ttl_secs: i64) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (result_tx, result_rx) = mpsc::channel::<ActivationResult>();
        Self {
            state: IpcState {
                app_id: app_id.to_string(),
                contract: contract.to_string(),
                chain_id,
                rpc_url: rpc_url.to_string(),
                developer_ens: None,
                session_ttl_secs: ttl_secs,
                cmd_tx,
                result_tx,
            },
            cmd_rx,
            result_rx,
            closed: std::cell::Cell::new(false),
        }
    }

    /// Posts one IPC message, exactly as `window.ipc.postMessage` does.
    ///
    /// Synchronous for every handler except `activate_tx_sent` and
    /// `purchase_tx_sent`, which spawn a poller thread; use [`Self::wait_for`]
    /// for what those emit.
    fn post(&self, message: serde_json::Value) {
        self.state.handle(message.to_string());
    }

    /// Everything queued for the view right now, in order.
    fn drain(&self) -> Vec<Call> {
        let mut calls = Vec::new();
        for cmd in self.cmd_rx.try_iter() {
            match cmd {
                Cmd::Eval(script) => calls.push(parse_call(&script)),
                Cmd::Close => self.closed.set(true),
            }
        }
        calls
    }

    /// The argument of the one call the last message produced, asserting it was
    /// `name` and that it was the only one.
    ///
    /// Deliberately strict: "it showed the cooldown screen *and* an error" is
    /// not the cooldown screen, and a flow that emits a screen it should have
    /// withheld has to fail here rather than be filtered out.
    fn expect_only(&self, name: &str) -> serde_json::Value {
        let calls = self.drain();
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one window.rub3.{name} call, got {calls:?}",
        );
        assert_eq!(calls[0].name, name, "unexpected call: {:?}", calls[0]);
        calls[0].arg.clone()
    }

    /// Waits for a call named `name`, ignoring anything emitted before it but
    /// failing loudly on `onError` - an error is the flow giving up, and
    /// waiting out the timeout would report it as a hang instead.
    ///
    /// Only the tx pollers answer asynchronously, and they are gated on
    /// `cooldown`, so a bundle without it has nothing to wait for.
    #[cfg_attr(not(feature = "cooldown"), allow(dead_code))]
    fn wait_for(&self, name: &str) -> serde_json::Value {
        let deadline = Instant::now() + RECV_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for window.rub3.{name}");
            match self.cmd_rx.recv_timeout(left) {
                Ok(Cmd::Eval(script)) => {
                    let call = parse_call(&script);
                    if call.name == "onError" {
                        panic!("waiting for {name}, but the flow errored: {}", call.arg);
                    }
                    if call.name == name {
                        return call.arg;
                    }
                }
                Ok(Cmd::Close) => self.closed.set(true),
                Err(e) => panic!("waiting for window.rub3.{name}: {e}"),
            }
        }
    }

    /// The final outcome the window hands back to `activation::ensure`.
    ///
    /// Watches the command channel alongside the result channel for the same
    /// reason [`Self::wait_for`] does: a flow that gives up emits `onError` and
    /// sends no result, so waiting on the result alone would spend the whole
    /// [`RECV_TIMEOUT`] and then report a hang while the reason sat unread.
    fn result(&self) -> ActivationResult {
        const POLL: Duration = Duration::from_millis(50);

        let deadline = Instant::now() + RECV_TIMEOUT;
        loop {
            match self.result_rx.try_recv() {
                Ok(result) => {
                    self.drain();
                    assert!(
                        self.closed.get(),
                        "a terminal result must also close the window",
                    );
                    return result;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("the activation window dropped its result channel")
                }
            }

            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "activation window should have produced a result",
            );
            match self.cmd_rx.recv_timeout(POLL.min(left)) {
                Ok(Cmd::Eval(script)) => {
                    let call = parse_call(&script);
                    if call.name == "onError" {
                        panic!("no result: the flow errored: {}", call.arg);
                    }
                }
                Ok(Cmd::Close) => self.closed.set(true),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the activation window dropped its command channel")
                }
            }
        }
    }
}

/// Splits `window.rub3.onThing({...})` into its name and its argument.
///
/// The first `(` ends the name and the last `)` ends the argument, which is
/// what makes this safe for payloads that contain brackets of their own -
/// `onProcessing("Waiting for activate() tx to land…")` is a real message.
fn parse_call(script: &str) -> Call {
    let body = script
        .strip_prefix("window.rub3.")
        .unwrap_or_else(|| panic!("not a window.rub3 call: {script}"));
    let open = body
        .find('(')
        .unwrap_or_else(|| panic!("call has no argument list: {script}"));
    let arg = body[open + 1..]
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("call is not closed: {script}"));
    Call {
        name: body[..open].to_string(),
        arg: serde_json::from_str(arg)
            .unwrap_or_else(|e| panic!("call argument is not JSON ({e}): {script}")),
    }
}

// ── Wallet stand-in ───────────────────────────────────────────────────────────

/// A throwaway keypair standing in for the user's wallet.
///
/// The wallet is the one participant these tests cannot drive through the
/// wrapper - it is the thing the wrapper deliberately does not hold - so it is
/// modelled the only way it can be: a key that signs what the screen asks it
/// to sign, and broadcasts what the screen asks it to broadcast.
struct Wallet {
    key: SigningKey,
    address: String,
}

impl Wallet {
    fn new() -> Self {
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let address = crate::license::public_key_to_address(key.verifying_key());
        Self { key, address }
    }

    /// `personal_sign` over a 32-byte preimage given as 0x-hex, returning the
    /// `r || s || v` hex a wallet returns. Same shape `session::verify_local`
    /// and `license::verify` both expect.
    fn personal_sign(&self, message_hex: &str) -> String {
        let bytes = hex::decode(message_hex.trim_start_matches("0x")).expect("preimage is hex");
        let message: [u8; 32] = bytes.as_slice().try_into().expect("preimage is 32 bytes");
        let digest = crate::license::personal_sign_hash(&message);
        let (sig, recovery) = self
            .key
            .sign_prehash_recoverable(&digest)
            .expect("signing failed");
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&sig.to_bytes());
        out[64] = recovery.to_byte() + 27;
        format!("0x{}", hex::encode(out))
    }
}

// ── An unreachable node ───────────────────────────────────────────────────────

/// Port 1 on loopback, where nothing listens.
///
/// Used as the RPC URL by the zero-contract test: passing an address that
/// cannot answer is what turns "should not touch the chain" from a claim into
/// an assertion, since any chain read on that path would fail the test rather
/// than quietly succeed against a working node.
const UNREACHABLE_RPC: &str = "http://127.0.0.1:1";

const ZERO_CONTRACT: &str = "0x0000000000000000000000000000000000000000";
const LEGACY_APP_ID: &str = "com.rub3.legacy-flow-test";

/// Points the licence and session stores at tmpdirs for one test, and holds
/// [`crate::ENV_LOCK`] while they are pointed there.
///
/// Same shape as `store::tests::LicenseDir`, and for the same reason: both
/// variables are process-global, so clearing them on the way out of an
/// assertion failure is what stops a red test from leaving the rest of the
/// binary reading a directory the unwind has already deleted.
struct StoreDirs {
    _guard: std::sync::MutexGuard<'static, ()>,
    _license: tempfile::TempDir,
    _session: tempfile::TempDir,
}

impl StoreDirs {
    fn set_up() -> Self {
        let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let license = tempfile::tempdir().expect("tempdir");
        let session = tempfile::tempdir().expect("tempdir");
        std::env::set_var("RUB3_LICENSE_DIR", license.path());
        std::env::set_var("RUB3_SESSION_DIR", session.path());
        Self {
            _guard: guard,
            _license: license,
            _session: session,
        }
    }
}

impl Drop for StoreDirs {
    fn drop(&mut self) {
        // Runs before the fields, so both variables are cleared while the lock
        // is still held and before the directories they name go away.
        std::env::remove_var("RUB3_LICENSE_DIR");
        std::env::remove_var("RUB3_SESSION_DIR");
    }
}

/// The pre-session `LicenseProof` path still works when no contract is
/// configured: the window issues a proof, and a later launch is served from it
/// without a window and without a chain read.
///
/// §1.8 kept this fallback deliberately. It is the path every tier-0 to tier-2
/// build takes, and the path a tier-3 build takes when `CONTRACT` is still the
/// zero placeholder - which is what a stock build is - so a session model
/// compiled in on top of it must not have displaced it. Not gated on
/// `cooldown`, so the same assertion runs on the bundles that have only this
/// path and on the one that has both.
#[test]
fn a_zero_contract_build_still_issues_and_serves_a_legacy_licence_proof() {
    let _dirs = StoreDirs::set_up();

    let wallet = Wallet::new();
    let window = Window::open(LEGACY_APP_ID, ZERO_CONTRACT, 8453, UNREACHABLE_RPC, 3600);

    window.post(serde_json::json!({ "type": "ready" }));
    let info = window.expect_only("onAppInfo");
    assert_eq!(info["appId"], LEGACY_APP_ID);
    assert_eq!(info["contractAddress"], ZERO_CONTRACT);

    // Connect. With no contract there is no ownership to read and no cooldown
    // to check, so the window must go straight to the legacy signing screen -
    // and must not have consulted the node to get there.
    window.post(serde_json::json!({ "type": "connect", "address": wallet.address }));
    let activate = window.expect_only("onShowActivate");
    assert_eq!(
        activate["tokenId"], 1,
        "the zero-contract path stands in token 1",
    );
    assert_eq!(activate["ownerAddress"], wallet.address);

    // The wallet signs what the screen showed it.
    let signature = wallet.personal_sign(
        activate["activationMessage"]
            .as_str()
            .expect("activationMessage should be a hex string"),
    );
    window.post(serde_json::json!({
        "type":          "signed",
        "token_id":      1,
        "owner_address": wallet.address,
        "signature":     signature,
        "paid_by":       serde_json::Value::Null,
    }));

    let result = window.result();
    let proof = match &result {
        ActivationResult::LegacySuccess { proof } => proof.clone(),
        other => panic!("expected LegacySuccess, got {}", describe(other)),
    };
    assert_eq!(proof.app_id, LEGACY_APP_ID);
    assert_eq!(proof.token_id, 1);
    assert_eq!(proof.contract, ZERO_CONTRACT);
    crate::license::verify(&proof).expect("the issued proof must verify");

    // Persisted through the same call the launch path makes.
    crate::activation::persist_activation(LEGACY_APP_ID, result)
        .expect("the proof should be written to the licence store");

    // Restart. No window is opened: `ensure` returns from the legacy fast path,
    // and against an RPC URL nothing answers on, so it cannot have read the
    // chain to do it.
    crate::activation::ensure(
        LEGACY_APP_ID,
        ZERO_CONTRACT,
        8453,
        UNREACHABLE_RPC,
        None,
        3600,
    )
    .expect("a stored legacy proof must launch without re-activating");

    // And the proof on disk is the one the window issued.
    let stored = crate::store::load_proof(LEGACY_APP_ID).expect("proof should be on disk");
    assert_eq!(stored.signature, proof.signature);
    assert_eq!(stored.wallet_address, proof.wallet_address);
}

/// Names an [`ActivationResult`] for a panic message. The type is not `Debug`,
/// and the message an `Error` carries is the whole reason a test failed there,
/// so it is carried through rather than collapsed to the variant name.
fn describe(result: &ActivationResult) -> String {
    match result {
        ActivationResult::LegacySuccess { .. } => "LegacySuccess".to_string(),
        #[cfg(feature = "cooldown")]
        ActivationResult::SessionSuccess { .. } => "SessionSuccess".to_string(),
        ActivationResult::Cancelled => "Cancelled".to_string(),
        ActivationResult::Error(msg) => format!("Error({msg})"),
    }
}
