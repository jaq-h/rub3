use std::sync::mpsc;

use serde::Deserialize;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

use crate::license::LicenseProof;

#[cfg(feature = "cooldown")]
use crate::session::Session;

const ACTIVATION_HTML: &str = include_str!("../assets/activation.html");

// ── Public types ──────────────────────────────────────────────────────────────

pub struct ActivationContext {
    pub app_id: String,
    pub contract: String,
    pub chain_id: u64,
    pub rpc_url: String,
    pub developer_ens: Option<String>,
    /// Session TTL in seconds. Used to compute `expires_at` when issuing a
    /// new tier-3 session. Ignored by the legacy `LicenseProof` path.
    pub session_ttl_secs: i64,
}

pub enum ActivationResult {
    /// Legacy `LicenseProof` (zero-contract / tier 0-2 fallback).
    LegacySuccess {
        proof: LicenseProof,
    },
    /// Tier-3 session issued after a confirmed `activate()` tx.
    #[cfg(feature = "cooldown")]
    SessionSuccess {
        session: Session,
    },
    Cancelled,
    Error(String),
}

// ── Inbound IPC messages (JS → Rust) ─────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum IpcMessage {
    /// Page finished loading; Rust should respond with onAppInfo().
    Ready,
    /// User submitted a wallet address; check ownership on-chain.
    Connect {
        address: String,
    },
    /// User selected a token from the multi-token selection screen.
    TokenSelected {
        token_id: u64,
        owner_address: String,
    },
    /// Legacy path: user signed the activation_message locally and pasted the
    /// signature. Used when no contract is configured (zero address).
    Signed {
        token_id: u64,
        owner_address: String,
        signature: String,
        paid_by: Option<String>,
    },
    /// Tier-3 path: user sent `activate(tokenId)` from their wallet and is
    /// now providing the tx hash so the wrapper can poll for confirmation.
    #[cfg(feature = "cooldown")]
    ActivateTxSent {
        tx_hash: String,
        token_id: u64,
        owner_address: String,
    },
    /// Purchase path: user sent `purchase(recipient)` from their wallet and is
    /// providing the tx hash so the wrapper can poll, extract the minted token
    /// id from the Transfer log, and continue into the activation flow.
    #[cfg(feature = "onchain-write")]
    PurchaseTxSent {
        tx_hash: String,
        owner_address: String,
    },
    /// Auto-detect (§5.1a): start watching the chain for the transaction the
    /// screen just asked the user to send, so its hash arrives without a
    /// paste. Answers into the same handler `activate_tx_sent` and
    /// `purchase_tx_sent` answer into.
    #[cfg(feature = "onchain-write")]
    AutoWatchStart {
        kind: AutoWatchKind,
        owner_address: String,
        /// The token being activated. Absent for a mint, which has no token id
        /// until the purchase lands.
        token_id: Option<u64>,
    },
    /// Auto-detect: stop the running watch. Sent when the user switches to
    /// another confirmation tab or leaves the screen.
    #[cfg(feature = "onchain-write")]
    AutoWatchCancel,
    /// Tier-3 path: user signed the session message. All fields from the
    /// tx-confirmation step are echoed back so the wrapper can assemble the
    /// Session without holding in-process state between IPC calls.
    #[cfg(feature = "cooldown")]
    SessionSigned {
        signature: String,
        token_id: u64,
        owner_address: String,
        identity: String,
        user_id: String,
        tba: Option<String>,
        nonce: String,
        expires_at: String,
        session_id: u64,
        activation_tx: String,
        activation_block: u64,
        activation_block_hash: String,
    },
    Cancel,
    Error {
        message: String,
    },
}

/// Which transaction an auto-detect watch is waiting for.
///
/// The two screens that can ask for one, named by what they are waiting to
/// see rather than by the screen they belong to: §5.1b's WalletConnect tab
/// will send the same two kinds from the same two screens.
#[cfg(feature = "onchain-write")]
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AutoWatchKind {
    /// The purchase screen: the ERC-721 mint a `purchase()` produces.
    Mint,
    /// The cooldown screen: the `activate(tokenId)` that opens a session.
    Activate,
}

#[cfg(feature = "onchain-write")]
impl AutoWatchKind {
    /// The spelling the page uses, and the one §5.1a names.
    fn as_str(self) -> &'static str {
        match self {
            AutoWatchKind::Mint => "mint",
            AutoWatchKind::Activate => "activate",
        }
    }
}

/// The one auto-detect watch a window may have running, the handle that stops
/// it, and the block the screen it belongs to was opened at.
///
/// Shared rather than owned because every party that can end a watch holds a
/// different clone of [`IpcState`]: the thread running the watch, the IPC
/// handler that will cancel it when the page moves, and the event loop that
/// closes the window. A watch that only its own thread could end would go on
/// polling an endpoint for the rest of its budget after the screen that wanted
/// the answer is gone.
///
/// The starting block lives here rather than in the watch because it belongs to
/// the *screen*, and a screen outlives the watches armed on it: the page arms
/// one every time the user returns to the Auto-detect tab. Reading the head on
/// each arm instead would walk the window forward past a transaction that landed
/// while the user was on the Manual tab, and the watch would then report,
/// truthfully from its own point of view and falsely from the user's, that it
/// watched for two minutes and saw nothing.
#[cfg(feature = "onchain-write")]
#[derive(Clone, Default)]
struct AutoWatch(std::sync::Arc<std::sync::Mutex<AutoWatchSlot>>);

#[cfg(feature = "onchain-write")]
#[derive(Default)]
struct AutoWatchSlot {
    running: Option<crate::rpc::Cancel>,
    /// The screen currently offering auto-detect, and the head when it opened.
    opened_at: Option<(AutoWatchKind, u64)>,
    /// When the cooldown this screen is showing is expected to end, from the
    /// same `cooldownReady` read the screen and the watch's hold are drawn
    /// from. `None` on a screen with no cooldown to wait out.
    cooling_until: Option<std::time::Instant>,
}

#[cfg(feature = "onchain-write")]
impl AutoWatch {
    /// Stops whatever is running and hands back the handle for its replacement.
    fn restart(&self) -> crate::rpc::Cancel {
        let next = crate::rpc::Cancel::new();
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = slot.running.replace(next.clone()) {
            previous.cancel();
        }
        next
    }

    /// Stops whatever is running. Idempotent, and a no-op when nothing is.
    ///
    /// Leaves the starting block alone: cancelling is what the page does when
    /// the user picks the Manual tab, and coming back to Auto-detect must
    /// resume the same window rather than open a later one.
    fn stop(&self) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = slot.running.take() {
            running.cancel();
        }
    }

    /// A tabbed screen is being displayed, so the next watch armed on it starts
    /// a fresh window.
    fn screen_opened(&self) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.opened_at = None;
        slot.cooling_until = None;
    }

    /// Records that the screen now on display is waiting out `remaining` of
    /// cooldown, so a watch that gives up can say the right thing about what to
    /// do next. `Duration::ZERO` means there is nothing left to wait for.
    ///
    /// Set from a `cooldownReady` read the flow already makes rather than from a
    /// read of its own: this is the same number the screen's copy and the
    /// watch's hold are drawn from, and a second source for it would let the
    /// words and the waiting disagree.
    #[cfg(feature = "cooldown")]
    fn cooling_for(&self, remaining: std::time::Duration) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.cooling_until = (!remaining.is_zero())
            .then(|| std::time::Instant::now().checked_add(remaining))
            .flatten();
    }

    /// Whether the screen this watch belongs to is still inside its cooldown.
    fn cooling(&self) -> bool {
        let slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.cooling_until
            .is_some_and(|end| std::time::Instant::now() < end)
    }

    /// The block a watch of `kind` starts from, or `None` before this screen has
    /// captured one.
    fn opened_at(&self, kind: AutoWatchKind) -> Option<u64> {
        let slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.opened_at
            .and_then(|(at, block)| (at == kind).then_some(block))
    }

    /// Records `head` as this screen's starting block, and answers with the
    /// block now in effect.
    ///
    /// The recorded one wins if there already is one, so two arms racing to
    /// capture settle on the earlier of the two reads rather than the later.
    fn opened_at_or(&self, kind: AutoWatchKind, head: u64) -> u64 {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match slot.opened_at {
            Some((at, block)) if at == kind => block,
            _ => {
                slot.opened_at = Some((kind, head));
                head
            }
        }
    }
}

/// What a finished watch found.
///
/// The two kinds hand off to different pollers, and naming the outcome lets
/// them do it from one place - so there is one cancellation check between a
/// watch returning and the flow moving on, rather than one per kind with the
/// second waiting to be forgotten.
#[cfg(feature = "onchain-write")]
enum Found {
    Mint(String),
    #[cfg(feature = "cooldown")]
    Activation(String, u64),
}

// ── Internal channel ──────────────────────────────────────────────────────────

enum Cmd {
    /// Evaluate a JS expression inside the webview.
    Eval(String),
    /// Close the window and exit the event loop.
    Close,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Opens the activation window and blocks until the user completes or cancels.
///
/// Must be called on the main thread (macOS WKWebView requirement).
pub fn run_activation_window(ctx: ActivationContext) -> ActivationResult {
    let mut event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_title("Activate License")
        .with_inner_size(tao::dpi::LogicalSize::new(480u32, 640u32))
        .with_resizable(false)
        .build(&event_loop)
        .expect("failed to create activation window");

    // cmd_tx: IPC handler → event loop (scripts to evaluate, close signal)
    // result_tx: IPC handler → caller (final outcome)
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (result_tx, result_rx) = mpsc::channel::<ActivationResult>();

    // Held here as well as inside the IPC state so the watch ends when the
    // window does: `run_return` gives control back with the view already gone,
    // and a watch still polling at that point is polling for nobody.
    #[cfg(feature = "onchain-write")]
    let auto_watch = AutoWatch::default();

    let ipc_state = IpcState {
        app_id: ctx.app_id.clone(),
        contract: ctx.contract.clone(),
        chain_id: ctx.chain_id,
        rpc_url: ctx.rpc_url.clone(),
        developer_ens: ctx.developer_ens.clone(),
        session_ttl_secs: ctx.session_ttl_secs,
        cmd_tx: cmd_tx.clone(),
        result_tx: result_tx.clone(),
        #[cfg(feature = "onchain-write")]
        auto_watch: auto_watch.clone(),
    };

    let webview = WebViewBuilder::new(&window)
        .with_html(ACTIVATION_HTML)
        .with_ipc_handler(move |request| {
            let body = request.body().clone();
            ipc_state.handle(body);
        })
        .build()
        .expect("failed to create webview");

    // run_return exits when ControlFlow::Exit is set, giving control back to caller.
    use tao::platform::run_return::EventLoopExtRunReturn;

    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::Poll;

        // Drain commands sent by the IPC handler (and background threads).
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Eval(script) => {
                    let _ = webview.evaluate_script(&script);
                }
                Cmd::Close => {
                    *control_flow = ControlFlow::Exit;
                }
            }
        }

        // User clicked the OS window close button.
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            let _ = result_tx.send(ActivationResult::Cancelled);
            *control_flow = ControlFlow::Exit;
        }
    });

    #[cfg(feature = "onchain-write")]
    auto_watch.stop();

    result_rx.recv().unwrap_or(ActivationResult::Cancelled)
}

// ── IPC handler ───────────────────────────────────────────────────────────────

/// Shared state available to the IPC callback. Cloneable because background
/// threads spawned from the handler (tx polling) need their own copy.
#[derive(Clone)]
struct IpcState {
    app_id: String,
    contract: String,
    chain_id: u64,
    rpc_url: String,
    developer_ens: Option<String>,
    // Only the cooldown-gated activate() poller builds a session draft, so
    // without that feature nothing reads the TTL. Kept unconditionally so the
    // field does not have to be cfg'd back out through every caller of
    // `interactive_slow_path`.
    #[cfg_attr(not(feature = "cooldown"), allow(dead_code))]
    session_ttl_secs: i64,
    cmd_tx: mpsc::Sender<Cmd>,
    result_tx: mpsc::Sender<ActivationResult>,
    /// The auto-detect watch this window has running, if any (§5.1a).
    #[cfg(feature = "onchain-write")]
    auto_watch: AutoWatch,
}

impl IpcState {
    fn handle(&self, body: String) {
        let msg: IpcMessage = match serde_json::from_str(&body) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("webview: malformed IPC message ({e}): {body}");
                return;
            }
        };

        // Every inbound message means the page moved on: a tab changed, a
        // screen changed, or the flow ended. An auto-detect watch belongs to
        // the screen that started it, so it stops here and `AutoWatchStart`
        // below is the only thing that ever starts another. Doing it once, in
        // front of the match, makes "a watch never outlives its screen" a
        // property of this seam rather than a rule every arm has to remember.
        #[cfg(feature = "onchain-write")]
        self.auto_watch.stop();

        match msg {
            IpcMessage::Ready => {
                let payload = serde_json::json!({
                    "appId":           self.app_id,
                    "contractAddress": self.contract,
                    "chainId":         self.chain_id,
                    "developerEns":    self.developer_ens,
                });
                self.eval(format!("window.rub3.onAppInfo({})", payload));
            }

            IpcMessage::Connect { address } => {
                let contract_addr: alloy::primitives::Address = self
                    .contract
                    .parse()
                    .unwrap_or(alloy::primitives::Address::ZERO);

                if contract_addr.is_zero() {
                    // No contract configured - skip on-chain check, use token 1 (legacy).
                    self.show_activate(&address, 1);
                    return;
                }

                let owner_addr: alloy::primitives::Address = match address.parse() {
                    Ok(a) => a,
                    Err(_) => {
                        self.eval(format!(
                            "window.rub3.onError({})",
                            serde_json::json!("Invalid wallet address")
                        ));
                        return;
                    }
                };

                match crate::rpc::tokens_of_owner(&self.rpc_url, contract_addr, owner_addr) {
                    Ok(tokens) if tokens.is_empty() => {
                        #[cfg(feature = "onchain-write")]
                        {
                            self.show_purchase(&address, owner_addr, contract_addr);
                        }
                        #[cfg(not(feature = "onchain-write"))]
                        {
                            self.eval_err("No license tokens found for this wallet");
                        }
                    }
                    Ok(tokens) if tokens.len() == 1 => {
                        self.proceed_after_token_selected(&address, tokens[0]);
                    }
                    Ok(tokens) => {
                        let payload = serde_json::json!({
                            "ownerAddress": address,
                            "tokens": tokens,
                        });
                        self.eval(format!("window.rub3.onShowTokenSelect({})", payload));
                    }
                    Err(e) => {
                        self.eval(format!(
                            "window.rub3.onError({})",
                            serde_json::json!(format!("ownership check failed: {e}"))
                        ));
                    }
                }
            }

            IpcMessage::TokenSelected {
                token_id,
                owner_address,
            } => {
                self.proceed_after_token_selected(&owner_address, token_id);
            }

            IpcMessage::Signed {
                token_id,
                owner_address,
                signature,
                paid_by,
            } => {
                let proof = LicenseProof {
                    app_id: self.app_id.clone(),
                    token_id,
                    wallet_address: owner_address,
                    paid_by,
                    signature,
                    activated_at: chrono::Utc::now().to_rfc3339(),
                    chain: "base".to_string(),
                    contract: self.contract.clone(),
                };
                let _ = self
                    .result_tx
                    .send(ActivationResult::LegacySuccess { proof });
                let _ = self.cmd_tx.send(Cmd::Close);
            }

            #[cfg(feature = "cooldown")]
            IpcMessage::ActivateTxSent {
                tx_hash,
                token_id,
                owner_address,
            } => {
                self.spawn_tx_poller(tx_hash, token_id, owner_address);
            }

            #[cfg(feature = "onchain-write")]
            IpcMessage::PurchaseTxSent {
                tx_hash,
                owner_address,
            } => {
                self.spawn_purchase_poller(tx_hash, owner_address);
            }

            #[cfg(feature = "onchain-write")]
            IpcMessage::AutoWatchStart {
                kind,
                owner_address,
                token_id,
            } => {
                self.spawn_auto_watch(kind, owner_address, token_id);
            }

            // The stop above did the work; this arm exists so that saying so
            // is a message the page can send rather than a side effect it has
            // to trigger by accident.
            #[cfg(feature = "onchain-write")]
            IpcMessage::AutoWatchCancel => {}

            #[cfg(feature = "cooldown")]
            IpcMessage::SessionSigned {
                signature,
                token_id,
                owner_address,
                identity,
                user_id,
                tba,
                nonce,
                expires_at,
                session_id,
                activation_tx,
                activation_block,
                activation_block_hash,
            } => {
                self.finalize_session(FinalizeArgs {
                    signature,
                    token_id,
                    owner_address,
                    identity,
                    user_id,
                    tba,
                    nonce,
                    expires_at,
                    session_id,
                    activation_tx,
                    activation_block,
                    activation_block_hash,
                });
            }

            IpcMessage::Cancel => {
                let _ = self.result_tx.send(ActivationResult::Cancelled);
                let _ = self.cmd_tx.send(Cmd::Close);
            }

            IpcMessage::Error { message } => {
                let _ = self.result_tx.send(ActivationResult::Error(message));
                let _ = self.cmd_tx.send(Cmd::Close);
            }
        }
    }

    // ── Flow helpers ─────────────────────────────────────────────────────────

    /// Branching point after a token is settled (either via single-token
    /// auto-select in Connect, or explicit TokenSelected). Under cooldown
    /// feature, goes to the tier-3 cooldown screen. Otherwise, falls back
    /// to the legacy activation-message screen.
    fn proceed_after_token_selected(&self, owner_address: &str, token_id: u64) {
        // One statement per bundle, so the two cannot both run: the earlier
        // shape needed a `return` to stop the tier-3 arm falling into the
        // legacy one, and that `return` is dead code under every bundle that
        // compiles it.
        #[cfg(feature = "cooldown")]
        self.show_cooldown(owner_address, token_id);
        #[cfg(not(feature = "cooldown"))]
        self.show_activate(owner_address, token_id);
    }

    fn show_activate(&self, address: &str, token_id: u64) {
        let msg = crate::license::activation_message(&self.app_id, token_id);
        let msg_hex = format!("0x{}", hex::encode(msg));
        let payload = serde_json::json!({
            "tokenId":           token_id,
            "ownerAddress":      address,
            "activationMessage": msg_hex,
        });
        self.eval(format!("window.rub3.onShowActivate({})", payload));
    }

    #[cfg(feature = "cooldown")]
    fn show_cooldown(&self, address: &str, token_id: u64) {
        #[cfg(feature = "onchain-write")]
        self.auto_watch.screen_opened();

        let contract_addr: alloy::primitives::Address = match self.contract.parse() {
            Ok(a) => a,
            Err(_) => {
                self.eval_err("contract address is malformed");
                return;
            }
        };

        let (ready, blocks_remaining) =
            match crate::rpc::cooldown_ready(&self.rpc_url, contract_addr, token_id) {
                Ok(r) => r,
                Err(e) => {
                    self.eval_err(&format!("cooldown check failed: {e}"));
                    return;
                }
            };

        let calldata = crate::rpc::encode_activate_calldata(token_id);

        // The cooldown in seconds as well as in blocks: the screen speaks in
        // both, and the number is estimated here rather than in the page so the
        // copy, the drain bar and the watch's own hold share one estimate.
        let cooldown_remaining = if ready {
            std::time::Duration::ZERO
        } else {
            cooldown_wait(blocks_remaining)
        };
        let cooldown_secs = cooldown_remaining.as_secs();

        // The same estimate the words a failed watch falls back to are chosen
        // by: inside a cooldown, "send it now" asks for a transaction the
        // contract still reverts.
        #[cfg(feature = "onchain-write")]
        self.auto_watch.cooling_for(cooldown_remaining);

        #[cfg_attr(not(feature = "onchain-write"), allow(unused_mut))]
        let mut payload = serde_json::json!({
            "tokenId":               token_id,
            "ownerAddress":          address,
            "contractAddress":       self.contract,
            "chainId":               self.chain_id,
            "ready":                 ready,
            "blocksRemaining":       blocks_remaining,
            "cooldownSecsRemaining": cooldown_secs,
            "calldata":              calldata,
        });
        // Present only where a watch can actually run, because its presence is
        // what puts the Auto-detect tab on the screen (§5.1). Same mechanism
        // §5.1b's `wc_project_id` will use, so the page keeps one rule for
        // which tabs exist rather than one per mode.
        //
        // The budget is the watching and not the waiting: inside a cooldown the
        // watch holds until the contract will accept an `activate()` and only
        // then spends it. The page draws its bar the same way, from this budget
        // and the cooldown above, so the bar running out and the watch giving up
        // stay one moment.
        #[cfg(feature = "onchain-write")]
        {
            payload["autoWatchSecs"] = crate::rpc::WATCH_BUDGET.as_secs().into();
        }
        self.eval(format!("window.rub3.onShowCooldown({})", payload));
    }

    /// Display the purchase screen: reads current supply state + price from
    /// the contract, encodes `purchase(recipient)` calldata, and emits
    /// `onShowPurchase` with the data the UI needs. Emits an error if supply
    /// is exhausted.
    ///
    /// **The code attestation runs first, before every other step here.** This
    /// is the one function in this file that asks a person to pay, and the
    /// screen it builds is the whole apparatus of the ask: the contract
    /// address to send to, the value, the calldata. A person cannot read
    /// bytecode, so presenting that screen at all is the wrapper vouching for
    /// the address. Refusing after it is on screen is not refusing - the
    /// address and calldata are already in front of a wallet - which is why the
    /// gate returns before anything is displayed rather than warning alongside
    /// it.
    ///
    /// Fails closed, including on a chain read that did not complete, for the
    /// same reason the agent door does. That posture stops here: `show_activate`,
    /// `show_cooldown` and `finalize_session` serve a licence already paid for
    /// and must never consult this gate - see `attest`'s module docs, and the
    /// test that holds them to it.
    #[cfg(feature = "onchain-write")]
    fn show_purchase(
        &self,
        address: &str,
        recipient: alloy::primitives::Address,
        contract_addr: alloy::primitives::Address,
    ) {
        self.auto_watch.screen_opened();

        let advisory = match crate::attest::verify_before_purchase(
            &self.rpc_url,
            self.chain_id,
            contract_addr,
        ) {
            // One line, on the one path in this file that spends money, naming
            // what the money is about to go to. Mirrors the agent door, warning
            // included: a release the registry stopped recommending is still
            // genuine code and still buyable, so it is said and not refused - and it is said to the
            // person too, on the screen below, because a buyer does not read
            // stderr.
            Ok(canonical) => {
                eprintln!("rub3: {contract_addr} verified as canonical {canonical}");
                if let Some(warning) = canonical.advisory() {
                    eprintln!("rub3: warning: {warning}");
                }
                purchase_advisory(&canonical)
            }
            Err(e) => {
                let notice = refusal_notice(&self.contract, &e);
                eprintln!("rub3: refusing to present a purchase: {e}");
                self.eval(format!(
                    "window.rub3.onPurchaseBlocked({})",
                    serde_json::json!({
                        "title":     notice.title,
                        "body":      notice.body,
                        "nextStep":  notice.next_step,
                        "detail":    notice.detail,
                        "retryable": notice.retryable,
                    })
                ));
                return;
            }
        };

        let cap = match crate::rpc::supply_cap(&self.rpc_url, contract_addr) {
            Ok(c) => c,
            Err(e) => {
                self.eval_err(&format!("supply cap read failed: {e}"));
                return;
            }
        };
        let next_id = match crate::rpc::next_token_id(&self.rpc_url, contract_addr) {
            Ok(n) => n,
            Err(e) => {
                self.eval_err(&format!("nextTokenId read failed: {e}"));
                return;
            }
        };
        if cap != 0 && next_id >= cap {
            self.eval_err("Sold out: no more tokens available for purchase");
            return;
        }

        let price = match crate::rpc::eth_price(&self.rpc_url, contract_addr) {
            Ok(p) => p,
            Err(e) => {
                self.eval_err(&format!("price read failed: {e}"));
                return;
            }
        };
        let calldata = crate::rpc::encode_purchase_calldata(recipient);

        // JSON numbers can't safely represent values above 2^53, and price
        // may be full uint256. Emit as decimal + hex strings and let the UI
        // format them.
        let payload = serde_json::json!({
            "ownerAddress":    address,
            "contractAddress": self.contract,
            "chainId":         self.chain_id,
            "priceWei":        price.to_string(),
            "valueHex":        format!("0x{:x}", price),
            "supplyCap":       cap,
            "nextTokenId":     next_id,
            "calldata":        calldata,
            "advisory":        advisory,
            "autoWatchSecs":   crate::rpc::WATCH_BUDGET.as_secs(),
        });
        self.eval(format!("window.rub3.onShowPurchase({})", payload));
    }

    /// Start an auto-detect watch for the transaction the current screen just
    /// asked the user to send (§5.1a).
    ///
    /// The thread it spawns does one thing the manual path does not: it finds
    /// the transaction hash. Everything after the hash is the manual path,
    /// reached by calling the very same method the pasted hash reaches, which
    /// is what keeps this a front door and not a second branch of the session
    /// pipeline. If a parallel finalize path ever appears below this line,
    /// something has gone wrong.
    #[cfg(feature = "onchain-write")]
    fn spawn_auto_watch(&self, kind: AutoWatchKind, owner_address: String, token_id: Option<u64>) {
        let cancel = self.auto_watch.restart();
        let state = self.clone();

        std::thread::spawn(move || {
            let contract_addr: alloy::primitives::Address = match state.contract.parse() {
                Ok(a) => a,
                Err(_) => {
                    state.eval_err("contract address is malformed");
                    return;
                }
            };
            let owner_addr: alloy::primitives::Address = match owner_address.parse() {
                Ok(a) => a,
                Err(_) => {
                    state.eval_err("owner address is malformed");
                    return;
                }
            };

            // The budget covers the whole watch, the two reads that set it up
            // included, and the cancel flag reaches them too. Those reads go
            // through `retry_read` so a 429 on the first request of the flow is
            // absorbed exactly as one mid-poll would be, rather than ending
            // auto-detect a second after the screen rendered.
            let deadline =
                crate::rpc::Deadline::after(crate::rpc::WATCH_BUDGET).cancelled_by(cancel.clone());

            // The block the screen was opened at. Read here rather than sent by
            // the page, which cannot see the chain: a watch starting from a
            // number the page guessed would either miss the transaction or
            // match one sent before the screen existed. Read once per screen
            // rather than once per watch, so returning to the Auto-detect tab
            // resumes the window the screen opened instead of starting a later
            // one past a transaction that has already landed.
            let from_block = match state.auto_watch.opened_at(kind) {
                Some(block) => block,
                None => {
                    let head = crate::rpc::retry_read(
                        || {
                            crate::rpc::get_block_number_within(
                                &state.rpc_url,
                                crate::rpc::WATCH_REQUEST_TIMEOUT,
                            )
                        },
                        &deadline,
                    );
                    match head {
                        Ok(block) => state.auto_watch.opened_at_or(kind, block),
                        Err(e) => {
                            state.auto_watch_ended(kind, &cancel, &e);
                            return;
                        }
                    }
                }
            };

            // How long before there is anything for this watch to find. A mint
            // can land in the next block, so a purchase watch starts now. An
            // activation cannot: the contract reverts one until the cooldown the
            // screen is showing runs out, and on the default 1800 blocks a watch
            // armed now would give up an hour before the user could legally
            // send. Read fresh rather than echoed by the page, which learned it
            // when the screen opened and may have been sitting on the manual tab
            // since.
            let hold = match kind {
                AutoWatchKind::Mint => std::time::Duration::ZERO,
                #[cfg(feature = "cooldown")]
                AutoWatchKind::Activate => match token_id {
                    Some(id) => {
                        let read = crate::rpc::retry_read(
                            || {
                                crate::rpc::cooldown_ready_within(
                                    &state.rpc_url,
                                    contract_addr,
                                    id,
                                    crate::rpc::WATCH_REQUEST_TIMEOUT,
                                )
                            },
                            &deadline,
                        );
                        match read {
                            Ok((_, blocks_remaining)) => {
                                let remaining = cooldown_wait(blocks_remaining);
                                // Fresher than the screen's own reading of the
                                // same view, and the words a give-up falls back
                                // to should follow the hold they describe.
                                state.auto_watch.cooling_for(remaining);
                                remaining
                            }
                            Err(e) => {
                                state.auto_watch_ended(kind, &cancel, &e);
                                return;
                            }
                        }
                    }
                    None => std::time::Duration::ZERO,
                },
                #[cfg(not(feature = "cooldown"))]
                AutoWatchKind::Activate => std::time::Duration::ZERO,
            };

            let deadline = deadline.starting_in(hold);

            let found = match kind {
                AutoWatchKind::Mint => crate::rpc::watch_for_mint(
                    &state.rpc_url,
                    contract_addr,
                    owner_addr,
                    from_block,
                    deadline,
                )
                .map(Found::Mint),

                #[cfg(feature = "cooldown")]
                AutoWatchKind::Activate => {
                    let Some(id) = token_id else {
                        state.eval_err("auto-detect asked to watch an activation with no token");
                        return;
                    };
                    crate::rpc::watch_for_activate(
                        &state.rpc_url,
                        contract_addr,
                        id,
                        from_block,
                        deadline,
                    )
                    .map(|tx_hash| Found::Activation(tx_hash, id))
                }

                // No activation step is compiled in, so there is nothing to
                // watch for. Unreachable from the shipped bundles - every one
                // that has `onchain-write` has `cooldown` too - and kept
                // because the two flags are independently selectable.
                #[cfg(not(feature = "cooldown"))]
                AutoWatchKind::Activate => {
                    let _ = token_id;
                    state.eval_err("this build has no activation step to watch for");
                    return;
                }
            };

            // Both outcomes are checked against the flag in one place, because
            // both are answers to a question the screen may have stopped asking.
            // A cancel raised while the last request was in flight still lets
            // that request come back with a hash, and dispatching it would drive
            // the purchase or activation flow on from a screen the user has
            // already left. Silence is what cancellation means, on either side.
            if cancel.is_cancelled() {
                return;
            }

            match found {
                Ok(Found::Mint(tx_hash)) => state.spawn_purchase_poller(tx_hash, owner_address),
                #[cfg(feature = "cooldown")]
                Ok(Found::Activation(tx_hash, id)) => {
                    state.spawn_tx_poller(tx_hash, id, owner_address)
                }
                Err(e) => state.auto_watch_ended(kind, &cancel, &e),
            }
        });
    }

    /// Tell the page that auto-detect gave up, so it can fall back to the
    /// manual paste with something useful already said.
    ///
    /// A cancelled watch is silent: cancellation means the page asked for this
    /// to stop, so it is already showing something else and a message now would
    /// land on the wrong screen.
    ///
    /// Whether it was cancelled is the flag's answer and not the error's. A
    /// cancel raised while a request is in flight - the head read this watch
    /// starts with, or the poll it is sleeping between - surfaces as whatever
    /// that request failed with, so reading only the error would write "we
    /// could not reach the network" into a screen the user has already left.
    #[cfg(feature = "onchain-write")]
    fn auto_watch_ended(
        &self,
        kind: AutoWatchKind,
        cancel: &crate::rpc::Cancel,
        e: &crate::rpc::RpcError,
    ) {
        use crate::rpc::{RpcError, WatchEnd};

        if cancel.is_cancelled() || matches!(e, RpcError::WatchEnded(WatchEnd::Cancelled)) {
            return;
        }
        eprintln!(
            "rub3: auto-detect stopped watching for a {}: {e}",
            kind.as_str()
        );

        let timed_out = matches!(e, RpcError::WatchEnded(WatchEnd::Timeout));
        self.eval(format!(
            "window.rub3.onAutoWatchEnded({})",
            serde_json::json!({
                "kind":   kind.as_str(),
                "reason": if timed_out { "timeout" } else { "rpc" },
                "detail": auto_watch_detail(kind, timed_out, self.auto_watch.cooling()),
            })
        ));
    }

    /// Spawn a background thread that polls for the purchase() tx receipt.
    ///
    /// On confirmation: asserts the tx hit this contract + succeeded, parses
    /// the ERC-721 Transfer log to recover the minted token id, and hands off
    /// to `proceed_after_token_selected` (same path as a wallet that already
    /// owned a token). On timeout/failure: emits an error to JS.
    #[cfg(feature = "onchain-write")]
    fn spawn_purchase_poller(&self, tx_hash: String, owner_address: String) {
        let state = self.clone();

        std::thread::spawn(move || {
            state.eval(format!(
                "window.rub3.onProcessing({})",
                serde_json::json!("Waiting for purchase() tx to land…")
            ));

            let receipt = match crate::rpc::wait_for_receipt(&state.rpc_url, &tx_hash) {
                Ok(r) => r,
                Err(e) => {
                    state.eval_err(&format!("tx polling failed: {e}"));
                    return;
                }
            };

            if !receipt.status {
                state.eval_err("purchase() tx reverted on-chain");
                return;
            }

            if let Some(to) = receipt.to.as_deref() {
                if !to.eq_ignore_ascii_case(&state.contract) {
                    state.eval_err(&format!(
                        "purchase() tx was sent to {to}, expected {}",
                        state.contract
                    ));
                    return;
                }
            }

            let contract_addr: alloy::primitives::Address = match state.contract.parse() {
                Ok(a) => a,
                Err(_) => {
                    state.eval_err("contract address is malformed");
                    return;
                }
            };
            let recipient: alloy::primitives::Address = match owner_address.parse() {
                Ok(a) => a,
                Err(_) => {
                    state.eval_err("owner address is malformed");
                    return;
                }
            };

            let token_id =
                match crate::rpc::mint_token_id(&state.rpc_url, &tx_hash, contract_addr, recipient)
                {
                    Ok(id) => id,
                    Err(e) => {
                        state.eval_err(&format!("failed to extract minted tokenId: {e}"));
                        return;
                    }
                };

            // Re-enter the normal flow exactly as if tokens_of_owner had
            // returned this single token.
            state.proceed_after_token_selected(&owner_address, token_id);
        });
    }

    /// Spawn a background thread that polls for the activate() tx receipt.
    ///
    /// On confirmation: reads `activeSessionId` from the contract, generates a
    /// nonce, computes the session `expires_at`, builds the session message,
    /// and tells JS to display the signing screen. On timeout/failure: emits
    /// an error to JS.
    #[cfg(feature = "cooldown")]
    fn spawn_tx_poller(&self, tx_hash: String, token_id: u64, owner_address: String) {
        let state = self.clone();

        std::thread::spawn(move || {
            state.eval(format!(
                "window.rub3.onProcessing({})",
                serde_json::json!("Waiting for activate() tx to land…")
            ));

            let receipt = match crate::rpc::wait_for_receipt(&state.rpc_url, &tx_hash) {
                Ok(r) => r,
                Err(e) => {
                    state.eval_err(&format!("tx polling failed: {e}"));
                    return;
                }
            };

            if !receipt.status {
                state.eval_err("activate() tx reverted on-chain");
                return;
            }

            // Confirm the tx actually went to the configured license contract.
            if let Some(to) = receipt.to.as_deref() {
                if !to.eq_ignore_ascii_case(&state.contract) {
                    state.eval_err(&format!(
                        "activate() tx was sent to {to}, expected {}",
                        state.contract
                    ));
                    return;
                }
            }

            let contract_addr: alloy::primitives::Address = match state.contract.parse() {
                Ok(a) => a,
                Err(_) => {
                    state.eval_err("contract address is malformed");
                    return;
                }
            };

            let wallet_addr: alloy::primitives::Address = match owner_address.parse() {
                Ok(a) => a,
                Err(_) => {
                    state.eval_err("owner address is malformed");
                    return;
                }
            };

            // Reads activeSessionId + the identity model, derives the TBA for
            // account-model deploys, and builds the preimage. Shared with the
            // headless door so both sign identical bytes for identical facts.
            let draft = match crate::session::draft_from_activation(
                &state.rpc_url,
                contract_addr,
                state.chain_id,
                &state.app_id,
                token_id,
                wallet_addr,
                &receipt.block_hash,
                state.session_ttl_secs,
            ) {
                Ok(d) => d,
                Err(e) => {
                    state.eval_err(&e);
                    return;
                }
            };

            // `ownerAddress` echoes back the draft's normalised casing: the
            // preimage commits to that exact string, so the value JS returns in
            // `session_signed` has to be the one that was hashed.
            let payload = serde_json::json!({
                "tokenId":             token_id,
                "ownerAddress":        draft.wallet,
                "identity":            draft.identity,
                "userId":              draft.user_id,
                "tba":                 draft.tba,
                "txHash":              tx_hash,
                "blockNumber":         receipt.block_number,
                "blockHash":           receipt.block_hash,
                "sessionId":           draft.session_id,
                "nonce":               draft.nonce,
                "expiresAt":           draft.expires_at,
                "sessionMessage":      draft.message_hex(),
            });
            state.eval(format!("window.rub3.onTxConfirmed({})", payload));
        });
    }

    #[cfg(feature = "cooldown")]
    fn finalize_session(&self, a: FinalizeArgs) {
        let session = Session {
            app_id: self.app_id.clone(),
            token_id: a.token_id,
            identity: a.identity,
            user_id: a.user_id,
            tba: a.tba,
            wallet: a.owner_address,
            nonce: a.nonce,
            issued_at: chrono::Utc::now().to_rfc3339(),
            expires_at: Some(a.expires_at),
            signature: a.signature,
            chain: "base".to_string(),
            contract: self.contract.clone(),
            activation_tx: Some(a.activation_tx),
            activation_block: Some(a.activation_block),
            activation_block_hash: Some(a.activation_block_hash),
            session_id: Some(a.session_id),
            device_pubkey: None,
        };

        if let Err(e) = crate::session::verify_local(&session) {
            self.eval_err(&format!("signature verification failed: {e}"));
            return;
        }

        let _ = self
            .result_tx
            .send(ActivationResult::SessionSuccess { session });
        let _ = self.cmd_tx.send(Cmd::Close);
    }

    // ── Primitives ───────────────────────────────────────────────────────────

    fn eval(&self, script: String) {
        let _ = self.cmd_tx.send(Cmd::Eval(script));
    }

    fn eval_err(&self, msg: &str) {
        self.eval(format!("window.rub3.onError({})", serde_json::json!(msg)));
    }
}

// ── How long a cooldown has left, in seconds ─────────────────────────────────

/// Seconds per block, as an estimate.
///
/// A cooldown is denominated in blocks, but everything that consumes one is
/// denominated in time: the sentence the screen shows, the bar that drains
/// beside it, and the hold an auto-detect watch takes before there is anything
/// to watch for. Base's target is 2 s, and this is the project's only copy of
/// that number so those three cannot disagree with each other - the screen is
/// explicit that it is an estimate, and the watch treats it as one, holding
/// only to avoid a poll that cannot succeed rather than to time anything.
#[cfg(feature = "cooldown")]
const ESTIMATED_BLOCK_SECS: u64 = 2;

/// How long `blocks_remaining` of cooldown is expected to take.
#[cfg(feature = "cooldown")]
fn cooldown_wait(blocks_remaining: u64) -> std::time::Duration {
    std::time::Duration::from_secs(blocks_remaining.saturating_mul(ESTIMATED_BLOCK_SECS))
}

// ── When auto-detect gives up, in a person's words ───────────────────────────

/// The sentence the manual tab carries when an auto-detect watch ends without
/// a transaction.
///
/// Not an error message, because in the likeliest case nothing failed: the
/// person has sent the transaction, the chain is slow, and the wrapper stopped
/// looking first. So it says what was watched for and what to do with the box
/// that is now in front of them, and never suggests the transaction is lost or
/// that it should be sent twice. Sending twice would buy a second licence.
///
/// `cooling` is the other half of "what to do now". Inside a cooldown the
/// contract still reverts an `activate()`, so telling someone to send it now
/// costs them gas and gets them nothing; the sentence has to point at the
/// cooldown above instead. The whole sentence is composed here, in every case,
/// so the page has nothing to say about wording and one edit here is the only
/// edit there is - a copy in JS that only applied on one screen would be
/// invisible to anyone reading this function.
#[cfg(feature = "onchain-write")]
fn auto_watch_detail(kind: AutoWatchKind, timed_out: bool, cooling: bool) -> String {
    let subject = match kind {
        AutoWatchKind::Mint => "purchase",
        AutoWatchKind::Activate => "activation",
    };
    match (timed_out, cooling) {
        (true, false) => format!(
            "We watched the chain for {} and did not see your {subject} transaction. \
             If you have already sent it, paste its hash below. If you have not, send it \
             now and paste the hash here - do not send it twice.",
            spoken_duration(crate::rpc::WATCH_BUDGET)
        ),
        (false, false) => format!(
            "We could not reach the network to watch for your {subject} transaction. \
             Send it from your wallet and paste its hash below."
        ),
        (true, true) => format!(
            "We stopped watching the chain, and the cooldown above has not ended yet. \
             If you have already sent the {subject}, paste its hash below. Otherwise wait \
             for the cooldown to end, send it, then paste the hash here."
        ),
        (false, true) => format!(
            "We could not reach the network to watch for your {subject} transaction. \
             Wait for the cooldown above to end, send it from your wallet, then paste its \
             hash below."
        ),
    }
}

/// A duration as someone would say it out loud: "two minutes", not "120s".
///
/// Whole minutes only, because that is the only shape the budget takes and a
/// half-spelled "2 minutes 30" would read worse than the seconds it replaced.
#[cfg(feature = "onchain-write")]
fn spoken_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 || !secs.is_multiple_of(60) {
        return format!("{secs} seconds");
    }
    match secs / 60 {
        1 => "a minute".to_string(),
        2 => "two minutes".to_string(),
        n => format!("{n} minutes"),
    }
}

// ── The deprecation advisory, in a person's words ────────────────────────────

/// What the window says beside a purchase the code registry no longer
/// recommends, or `None` when it recommends it.
///
/// A person cannot read bytecode, which is the premise the whole purchase
/// screen is built on, and a person does not read stderr either - so an
/// advisory that only ever reached a terminal would reach the automated buyer
/// and not the human one, which is the inversion this screen exists to close.
/// [`crate::attest::Attestation::advisory`] returns the sentence rather than
/// printing it precisely so each door can say it in its own voice; this is the
/// window's.
///
/// **It is advice and never a refusal.** A release the registry stopped
/// recommending is genuine rub3 code, the purchase is not blocked, and a licence
/// bought from it stays valid, so the words carry that reassurance and the
/// screen renders them beside the price rather than in place of it.
///
/// **It claims only what the record carries.** `Deprecated` is a status with no
/// reason field and no successor pointer, so the sentence must not promise a
/// newer version or send the buyer off to fetch one: a deprecation issued for a
/// defect with no fix yet would send them after something that does not exist. A version authority able to stop a
/// purchase would be a revocation surface with an extra step, and neither the
/// contract nor this screen gives it one. Emitted as `"advisory"`, written into
/// `#p-advisory-body`, which stays hidden when this is `None`.
#[cfg(feature = "onchain-write")]
fn purchase_advisory(canonical: &crate::attest::Attestation) -> Option<String> {
    canonical.advisory().map(|_| {
        format!(
            "The on-chain code registry no longer recommends {} ({}) for a new purchase. Its \
             code is genuine rub3 code, this purchase works normally, and the licence you buy \
             from it stays valid. The registry does not say why, and it does not say that a \
             replacement exists. Go ahead, or cancel and check with whoever published this \
             software first.",
            canonical.contract, canonical.release
        )
    })
}

// ── The purchase refusal, in a person's words ────────────────────────────────

/// What the window says when the contract does not verify.
///
/// The agent door answers the same refusal with an exit code and a machine
/// detail line, which is the right answer for an orchestrator and no answer at
/// all for a person: "NotCanonicalContract, code_bytes=0" tells a buyer neither
/// what happened nor what to do about it. The verification is shared - one gate,
/// in `attest`, called from both doors - and only the wording differs, which is
/// where it should differ.
#[cfg(feature = "onchain-write")]
struct RefusalNotice {
    /// The headline: what this is, in one line. Emitted as `"title"`, written
    /// into `#b-title`.
    title: &'static str,
    /// What the check found, said without jargon and without accusing anyone.
    /// A refusal is equally what a legitimate contract released after this app
    /// was packed looks like, and the wording must not pretend otherwise.
    /// Emitted as `"body"`, written into `#b-body`.
    body: String,
    /// The one thing the person can usefully do next. Emitted as `"nextStep"`,
    /// written into `#b-next`.
    next_step: &'static str,
    /// The technical finding, for a message to whoever published the software.
    /// Emitted as `"detail"`, written into `#b-detail`.
    ///
    /// A refusal carries its finding verbatim: it is a verdict on the contract,
    /// and every word of it is about the contract. A failed read carries only
    /// the kind of failure, because the transport error it came from embeds the
    /// request URL, and a packed `RPC_URL` can hold a provider API key. That
    /// key would then be on a buyer's screen under a line inviting them to
    /// share it, and it tells them nothing their next step does not already
    /// say. The full error still goes to stderr for whoever ran the wrapper
    /// from a terminal.
    detail: String,
    /// Whether trying again could plausibly change the answer. True only for a
    /// chain read that did not complete; a refused address is a settled answer
    /// and offering a retry for it would invite the buyer to keep clicking
    /// until the check passes. Derived from [`crate::rpc::RpcError::is_retryable`]
    /// rather than decided here, so the two cannot drift apart. Emitted as
    /// `"retryable"`, and toggles `#btn-b-retry`.
    retryable: bool,
}

/// Turns a gate failure into what the window shows.
///
/// Pure, so the words a buyer is about to read are exercised by tests directly
/// rather than inferred from a screenshot.
#[cfg(feature = "onchain-write")]
fn refusal_notice(contract: &str, error: &crate::attest::GateError) -> RefusalNotice {
    use crate::attest::{GateError, Refusal, RegistryOutcome, Role};
    use crate::rpc::RpcError;

    match error {
        // Not a verdict on the contract: the wrapper has no opinion yet,
        // because it never got the bytes. Said plainly, because "could not
        // verify" reads as "found something wrong" to most people.
        GateError::Fetch(e) => RefusalNotice {
            title: "Could not check this contract",
            body: format!(
                "Before showing you a payment, this app reads the code deployed at {contract} \
                 and compares it with the licence contracts it was built to trust. That read \
                 did not complete, so there is nothing to compare. Nothing has been signed and \
                 nothing has been sent."
            ),
            // Tied to `retryable` below, so the words and the button always
            // agree: telling someone to try again beside a screen with no Try
            // Again button on it is an instruction they cannot follow.
            next_step: if e.is_retryable() {
                "This is a connection problem, not a verdict on the contract. \
                 Check your network and try again."
            } else {
                "This is not a verdict on the contract, but repeating it will not help. \
                 Check the network and address settings with whoever published this software."
            },
            // The kind of failure only, which is a narrower thing than the
            // redaction `RpcError::transport` already applies. That one strips
            // the URL out of the error value so no surface can leak the packed
            // key; this one decides that a buyer being told to forward what
            // they see should be forwarding a sentence about the failure and
            // not a network error at all. The redaction is the floor; this is
            // the choice made on top of it, and neither replaces the other.
            // Stderr still gets the sanitized error in full.
            detail: match e {
                RpcError::Transport(_) => {
                    "The node this app reads the chain through did not answer the request for \
                     the contract's code."
                        .to_string()
                }
                RpcError::Contract(_) => {
                    "The request for the contract's code came back as an error rather than as \
                     code."
                        .to_string()
                }
                RpcError::InvalidInput(_) => {
                    "The request for the contract's code was rejected as malformed before it \
                     reached the network."
                        .to_string()
                }
                RpcError::EnsNotSupported => {
                    "The contract is named by an ENS name, which this app cannot resolve, so \
                     its code was never requested."
                        .to_string()
                }
                // Attestation fetches the code with a single `eth_getCode` and
                // never watches, so this cannot arrive here. Spelled out all
                // the same rather than folded into a wildcard: this match is
                // what forces a sentence for every way the fetch can fail, and
                // a wildcard would absorb the next variant without one.
                RpcError::WatchEnded(_) => {
                    "The request for the contract's code did not complete.".to_string()
                }
            },
            retryable: e.is_retryable(),
        },

        GateError::Refused(refusal) => match refusal {
            // An empty address is the one refusal a person can often fix
            // themselves - wrong network, or a copied address that lost a
            // character - so it says that instead of talking about code.
            Refusal::Unrecognised(finding) if finding.code_len == 0 => RefusalNotice {
                title: "Nothing is deployed at this address",
                body: format!(
                    "There is no contract at {contract} on this network, so there is no licence \
                     to buy here. A payment sent to it would leave your wallet and buy nothing."
                ),
                next_step: "Either the address or the network is wrong. Check both with \
                            whoever published this software before sending anything.",
                detail: finding.shareable_detail(),
                retryable: false,
            },

            // Code is there and it is not ours. Both innocent explanations are
            // stated first, because the honest reading of a miss is "this app
            // does not know that contract", not "that contract is a fake".
            //
            // What the on-chain code registry said, if anything, changes which
            // of the two is likelier, so it changes the words and the next step
            // rather than being left in the technical detail. It never changes
            // `retryable`: a refused address is a settled answer for this
            // attempt, and a Try Again button beside one invites clicking until
            // the check passes.
            Refusal::Unrecognised(finding) => RefusalNotice {
                title: "This app does not recognise the contract's code",
                body: format!(
                    "There is a contract at {contract}, but its code does not match any rub3 \
                     licence contract this app was built to trust. It may be a newer release \
                     than this copy of the app knows about, or it may be a modified copy that \
                     does not behave the way a rub3 licence does. From here the two look the \
                     same, so you are not being asked to pay either of them. Nothing has been \
                     signed and nothing has been sent.{}",
                    match &finding.registry {
                        // The published record of genuine rub3 releases was
                        // asked and had no record of this code, so "newer than
                        // this app" is a much thinner explanation than it looks
                        // above. Said plainly, and still without accusing
                        // anyone: a release published somewhere else would look
                        // the same way.
                        RegistryOutcome::Unknown =>
                            " This app also checked the on-chain record of genuine rub3 \
                             releases, which has no entry for this code either.",
                        // The second opinion was not available, so the check
                        // that would have told a newer release from a modified
                        // copy did not happen. Saying so is the difference
                        // between "we looked and found nothing" and "we could
                        // not look".
                        RegistryOutcome::Unavailable(_) =>
                            " This app could not reach the on-chain record of genuine rub3 \
                             releases, so it could not tell those two apart.",
                        RegistryOutcome::NotConsulted => "",
                    }
                ),
                next_step: "Confirm the address with whoever published this software. If they \
                            say it is current, this copy of the app is older than the contract \
                            and needs updating.",
                detail: finding.shareable_detail(),
                retryable: false,
            },

            // The code is ours; the address is a category error. Saying so
            // keeps the buyer from hunting for a compromise that is not there.
            Refusal::NotALicence {
                contract: name,
                role,
            } => RefusalNotice {
                title: "This address is rub3 code, but not a licence contract",
                body: format!(
                    "The code at {contract} is genuine rub3 code - {name}, {}. A payment sent \
                     to it would buy nothing and would not come back. The address is wrong; \
                     the code is not.",
                    match role {
                        Some(Role::Factory) =>
                            "the factory that deploys licence contracts rather than one that \
                             sells them",
                        Some(Role::Deployer) =>
                            "an internal helper the factory uses to deploy licence contracts",
                        Some(Role::CodeRegistry) =>
                            "the registry that records which rub3 code is genuine, which is how \
                             this app checked the address in the first place",
                        Some(Role::DiscoveryRegistry) =>
                            "the registry that lists which rub3 apps exist, which is where an \
                             address like this one is found rather than bought from",
                        // Not a state the gate produces - it accepts every
                        // licence-role match - but kept total rather than
                        // unreachable so a role added to the table later
                        // cannot panic a buyer's window.
                        Some(Role::Licence) => "which this app did not accept as a purchase target",
                        // A role published by a code registry newer than this
                        // app. The code is vouched for and the app still cannot
                        // say what it is for, which is the honest reading and
                        // the reason it is refused rather than guessed at.
                        None => "a kind of rub3 contract this app is too old to know about",
                    }
                ),
                next_step: "Ask whoever published this software for the address of its \
                            licence contract.",
                detail: refusal.to_string(),
                retryable: false,
            },
        },
    }
}

// ── Tier-3 helpers ────────────────────────────────────────────────────────────

#[cfg(feature = "cooldown")]
struct FinalizeArgs {
    signature: String,
    token_id: u64,
    owner_address: String,
    identity: String,
    user_id: String,
    tba: Option<String>,
    nonce: String,
    expires_at: String,
    session_id: u64,
    activation_tx: String,
    activation_block: u64,
    activation_block_hash: String,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The §1.8 activation flows, driven through this module's IPC handler with
/// channels standing in for the view. See the module's own header for the seam
/// and its limits.
#[cfg(test)]
mod session_flow;

/// The purchase gate, and the words it puts in front of a person.
///
/// Compiled only where a purchase can be made from this window, which is the
/// same condition the gate itself is compiled under: below tier-3 there is no
/// `show_purchase` to guard and nothing here to test.
#[cfg(all(test, feature = "onchain-write"))]
mod tests {
    use super::*;
    use crate::attest::{Refusal, RegistryOutcome, Role, Unrecognised};
    use crate::test_support::StubNode;

    /// An address that is not the zero address, so nothing short-circuits on
    /// "no contract configured" before the gate is reached.
    const CONTRACT: &str = "0x000000000000000000000000000000000000dEaD";
    const BUYER: &str = "0x00000000000000000000000000000000000B0B0b";

    /// An `IpcState` wired to channels instead of a window, so the scripts it
    /// would have evaluated can be read back.
    fn state_for(rpc_url: &str) -> (IpcState, mpsc::Receiver<Cmd>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (result_tx, _result_rx) = mpsc::channel::<ActivationResult>();
        // The result receiver is dropped on purpose: a refusal must not send a
        // result at all, and `send` on a dropped channel is an error the code
        // already ignores, so this cannot mask one.
        (
            IpcState {
                app_id: "test.app".to_string(),
                contract: CONTRACT.to_string(),
                chain_id: 8453,
                rpc_url: rpc_url.to_string(),
                developer_ens: None,
                session_ttl_secs: 3600,
                cmd_tx,
                result_tx,
                auto_watch: AutoWatch::default(),
            },
            cmd_rx,
        )
    }

    /// Every script the state emitted, in order.
    fn scripts(rx: &mpsc::Receiver<Cmd>) -> Vec<String> {
        rx.try_iter()
            .filter_map(|cmd| match cmd {
                Cmd::Eval(script) => Some(script),
                Cmd::Close => None,
            })
            .collect()
    }

    /// The argument of the one `onPurchaseBlocked` call, parsed. Fails if the
    /// window was told anything else, so "it also showed the purchase screen"
    /// cannot pass as a refusal.
    fn blocked_payload(rx: &mpsc::Receiver<Cmd>) -> serde_json::Value {
        let scripts = scripts(rx);
        assert_eq!(
            scripts.len(),
            1,
            "a refusal is one message and one screen, got {scripts:?}"
        );
        let script = &scripts[0];
        let arg = script
            .strip_prefix("window.rub3.onPurchaseBlocked(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or_else(|| panic!("expected a purchase refusal, got: {script}"));
        serde_json::from_str(arg).expect("the payload handed to the window is valid JSON")
    }

    fn buyer() -> alloy::primitives::Address {
        BUYER.parse().expect("test buyer address")
    }

    fn contract() -> alloy::primitives::Address {
        CONTRACT.parse().expect("test contract address")
    }

    // ── The auto-detect protocol (§5.1a) ─────────────────────────────────────

    /// The messages the page posts are messages the handler accepts.
    ///
    /// Spelled out as the JSON that goes over the seam rather than as the enum,
    /// because the enum is the half that cannot drift on its own: serde answers
    /// a tag it does not know by logging "malformed IPC message" and returning,
    /// so a rename here fails a test instead of failing in front of a person.
    ///
    /// The other half of the seam - that `assets/activation.html` posts exactly
    /// these, and defines the `window.rub3.onAutoWatchEnded` this handler calls
    /// back into - is not assertable from Rust: the page is JS, and matching
    /// text in its source would prove only that the text is there, not that the
    /// path is live. It is covered by driving the rendered page in a browser,
    /// the way §5.1a was built.
    #[test]
    fn the_handler_accepts_the_auto_detect_messages_the_page_posts() {
        for message in [
            // A mint carries no token id: there is no token until the purchase
            // lands, and `null` is what the page sends.
            serde_json::json!({
                "type": "auto_watch_start",
                "kind": "mint",
                "owner_address": BUYER,
                "token_id": serde_json::Value::Null,
            }),
            serde_json::json!({
                "type": "auto_watch_start",
                "kind": "activate",
                "owner_address": BUYER,
                "token_id": 7,
            }),
            serde_json::json!({ "type": "auto_watch_cancel" }),
        ] {
            serde_json::from_str::<IpcMessage>(&message.to_string())
                .unwrap_or_else(|e| panic!("the handler must accept {message} ({e})"));
        }
    }

    // ── The gate ─────────────────────────────────────────────────────────────

    /// The refusal a person is most likely to meet: a contract is there, and it
    /// is not one this build knows.
    ///
    /// Driven through `show_purchase` against a node that answers with real
    /// non-canonical code, so what is asserted is the behaviour of the entry
    /// point the connect screen calls, not of a helper beneath it.
    #[test]
    fn unrecognised_code_never_reaches_the_purchase_screen() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"result":"0x6080604052348015600f57600080fd5b50"}"#,
        );
        let (state, rx) = state_for(&node.url);

        state.show_purchase(BUYER, buyer(), contract());

        let payload = blocked_payload(&rx);
        assert_eq!(
            payload["title"], "This app does not recognise the contract's code",
            "payload: {payload}"
        );
        assert_eq!(
            payload["retryable"], false,
            "a refused address is a settled answer, not something to click again"
        );
        assert!(
            payload["detail"]
                .as_str()
                .expect("detail is a string")
                .contains("17 bytes"),
            "the finding must survive to the screen verbatim: {payload}"
        );
    }

    /// The ordering claim, which is the whole point of the gate: the code is
    /// checked before price and supply are even read.
    ///
    /// A node that answers nothing fails both the code read and the supply
    /// read, so the two orderings are told apart by *which* failure the window
    /// is given. Getting "supply cap read failed" here would mean the contract
    /// was being priced before it was verified.
    #[test]
    fn the_code_is_checked_before_supply_and_price_are_read() {
        // Port 1 is reserved and never listening: the connection is refused
        // immediately rather than hanging the test out to a timeout.
        let (state, rx) = state_for("http://127.0.0.1:1");

        state.show_purchase(BUYER, buyer(), contract());

        let payload = blocked_payload(&rx);
        assert_eq!(
            payload["title"], "Could not check this contract",
            "the failure reported must be the code check, not a later read: {payload}"
        );
        assert_eq!(
            payload["retryable"], true,
            "an unreachable node is the one refusal worth retrying"
        );
    }

    // ── The words ────────────────────────────────────────────────────────────

    /// A deprecated release reaches the person, and reaches them as advice.
    ///
    /// The premise of this whole screen is that a person cannot read bytecode,
    /// and a person does not read stderr either: an advisory that only ever
    /// went to a terminal would be seen by the automated buyer and not by the
    /// human one. The second half is that it must not read as a refusal. The
    /// purchase completes, the code is genuine, and the licence stays valid, so
    /// the reassurance the sentence carries is asserted as tightly as its
    /// presence is. The third is that it may claim no more than the record
    /// carries: a `Deprecated` status has no reason and no successor pointer.
    #[test]
    fn a_deprecated_release_advises_the_buyer_rather_than_alarming_them() {
        use crate::attest::{Attestation, Authority, RecordStatus};

        let attested = |status| Attestation {
            contract: "Rub3Access".to_string(),
            release: "2026-04".to_string(),
            authority: Authority::Registry {
                status,
                registered_at_block: 31_415_926,
            },
        };

        assert_eq!(
            purchase_advisory(&attested(RecordStatus::Active)),
            None,
            "a current release has nothing to say, and a note that is always there is noise"
        );

        let advisory = purchase_advisory(&attested(RecordStatus::Deprecated))
            .expect("a superseded release has to reach the person doing the buying");
        assert!(
            advisory.contains("Rub3Access") && advisory.contains("2026-04"),
            "the advisory must name what it is about: {advisory}"
        );
        assert!(
            advisory.contains("genuine rub3 code"),
            "a person told only that their contract is superseded hears 'this is a fake': \
             {advisory}"
        );
        assert!(
            advisory.contains("stays valid"),
            "the licence being unaffected is the half that keeps this advice rather than a \
             warning: {advisory}"
        );
        for promise in [
            "later release",
            "newer release",
            "newer version",
            "updated copy",
        ] {
            assert!(
                !advisory.to_lowercase().contains(promise),
                "the record carries no successor pointer, so the advisory must not send the \
                 buyer after one: {advisory}"
            );
        }
        assert!(
            purchase_advisory(&Attestation {
                contract: "Rub3Access".to_string(),
                release: "2026-04".to_string(),
                authority: Authority::Pinned,
            })
            .is_none(),
            "the pinned table publishes no status, so it can never advise"
        );
    }

    /// Two refusal causes, two different explanations. A buyer told "the code
    /// did not verify" for both has been told nothing they can act on: one of
    /// them means the address may be dangerous, the other means the address is
    /// simply the wrong one.
    #[test]
    fn the_two_refusal_causes_read_differently() {
        let unrecognised = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                code_len: 4_096,
                exposed: vec!["seize(uint256)"],
                registry: RegistryOutcome::NotConsulted,
            })),
        );
        let not_a_licence = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::NotALicence {
                contract: "Rub3Factory".to_string(),
                role: Some(Role::Factory),
            }),
        );

        assert_ne!(unrecognised.title, not_a_licence.title);
        assert_ne!(unrecognised.body, not_a_licence.body);
        assert_ne!(unrecognised.next_step, not_a_licence.next_step);

        // The unrecognised case cannot claim to know what the code is.
        assert!(
            unrecognised.body.contains("may be a newer release")
                && unrecognised.body.contains("modified copy"),
            "a miss is equally a newer contract and a hostile one; both must be said: {}",
            unrecognised.body
        );
        // The not-a-licence case knows exactly what the code is, and says the
        // address is the mistake rather than leaving a buyer hunting for one.
        assert!(
            not_a_licence.body.contains("genuine rub3 code")
                && not_a_licence.body.contains("Rub3Factory")
                && not_a_licence.body.contains("The address is wrong"),
            "the address, not the code, is the fault here: {}",
            not_a_licence.body
        );
        assert!(!unrecognised.retryable && !not_a_licence.retryable);
    }

    /// Every non-licence role gets its own sentence, and the two registries get
    /// two different ones.
    ///
    /// `Rub3Registry` (discovery) and `Rub3CodeRegistry` (the code authority)
    /// are the pair a buyer is most likely to have confused in the first place -
    /// both are "a rub3 registry", and only one of them is the reason this app
    /// trusted the address it just refused. A window that described them with
    /// the same words would send someone back to the same wrong address, so the
    /// copy is checked to differ rather than merely to exist.
    #[test]
    fn the_two_registries_are_described_differently_on_the_blocked_screen() {
        let notice_for = |contract: &str, role: Role| {
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Refused(Refusal::NotALicence {
                    contract: contract.to_string(),
                    role: Some(role),
                }),
            )
        };

        let discovery = notice_for("Rub3Registry", Role::DiscoveryRegistry);
        let code = notice_for("Rub3CodeRegistry", Role::CodeRegistry);

        assert!(
            discovery.body.contains("Rub3Registry")
                && discovery.body.contains("lists which rub3 apps exist"),
            "the discovery registry has to be described as what it is: {}",
            discovery.body
        );
        assert_ne!(
            discovery.body, code.body,
            "the two registries must not be described with the same sentence"
        );

        // And no two roles share a description, which is what keeps a role added
        // later from silently inheriting another one's words.
        let bodies: Vec<String> = [
            Role::Factory,
            Role::Deployer,
            Role::CodeRegistry,
            Role::DiscoveryRegistry,
            Role::Licence,
        ]
        .into_iter()
        .map(|role| notice_for("Rub3Whatever", role).body)
        .collect();
        for (i, a) in bodies.iter().enumerate() {
            for b in bodies.iter().skip(i + 1) {
                assert_ne!(a, b, "two roles read identically on the blocked screen");
            }
        }
    }

    /// An address holding nothing is a third thing a person can act on - almost
    /// always the wrong network or a mistyped address - and saying "the code
    /// does not match" about an empty address would send them looking for a
    /// contract that is not there.
    #[test]
    fn an_empty_address_says_the_address_is_empty() {
        let notice = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                code_len: 0,
                exposed: vec![],
                registry: RegistryOutcome::NotConsulted,
            })),
        );
        assert_eq!(notice.title, "Nothing is deployed at this address");
        assert!(
            notice.next_step.contains("network"),
            "the likeliest cause has to be named: {}",
            notice.next_step
        );
    }

    /// Every notice is prose a person reads, so it has to survive the source
    /// formatting that produced it: no doubled spaces from a line continuation,
    /// no stray indentation, and the address always present so the words are
    /// about something.
    #[test]
    fn every_notice_reads_as_finished_prose() {
        let notices = [
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Fetch(crate::rpc::RpcError::Transport(
                    "connection refused".into(),
                )),
            ),
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                    code_len: 0,
                    exposed: vec![],
                    registry: RegistryOutcome::NotConsulted,
                })),
            ),
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                    code_len: 4_096,
                    exposed: vec![],
                    registry: RegistryOutcome::NotConsulted,
                })),
            ),
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Refused(Refusal::NotALicence {
                    contract: "Rub3Factory".to_string(),
                    role: Some(Role::Factory),
                }),
            ),
        ];

        for notice in &notices {
            for (field, text) in [
                ("title", notice.title.to_string()),
                ("body", notice.body.clone()),
                ("next_step", notice.next_step.to_string()),
            ] {
                assert!(
                    !text.contains("  ") && !text.contains('\n'),
                    "{field} carries source formatting into the window: {text:?}"
                );
                assert_eq!(text.trim(), text, "{field} has stray whitespace: {text:?}");
                assert!(!text.is_empty(), "{field} is empty");
            }
            assert!(
                notice.body.ends_with('.') && notice.next_step.ends_with('.'),
                "a sentence shown to a person ends: {:?} / {:?}",
                notice.body,
                notice.next_step
            );
            assert!(
                notice.body.contains(CONTRACT),
                "the notice must name the address it is about: {}",
                notice.body
            );
            assert!(!notice.detail.is_empty(), "the finding must be shown");
        }
    }

    /// A failed read must not put the RPC endpoint on a buyer's screen.
    ///
    /// The transport error is built from the request, so it carries the URL the
    /// wrapper was packed with, and that URL can hold a provider API key. The
    /// blocked screen shows `detail` under a line telling the person to send it
    /// to whoever published the software, so anything in it is as good as
    /// published. Every field the screen renders is checked, not just `detail`,
    /// because a leak moved into the body is the same leak.
    #[test]
    fn a_failed_read_never_puts_the_rpc_endpoint_on_screen() {
        const HOST: &str = "base-mainnet.example-provider.io";
        const SECRET: &str = "9f3c1d7ab24e4a1e8c05f6d2b7e19a44";
        let transport = format!(
            "error sending request for url (https://{HOST}/v2/{SECRET}?apiKey={SECRET}): \
             connection closed before message completed"
        );

        let notice = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Fetch(crate::rpc::RpcError::Transport(transport)),
        );

        let shown = format!(
            "{} {} {} {}",
            notice.title, notice.body, notice.next_step, notice.detail
        );
        for leaked in [HOST, SECRET, "https://", "apiKey", "example-provider"] {
            assert!(
                !shown.contains(leaked),
                "the refusal screen shows {leaked:?}, which came from the packed RPC URL: {shown}"
            );
        }
        assert!(
            !notice.detail.is_empty(),
            "redacting the endpoint must not leave the buyer with a blank finding"
        );
    }

    /// The refusal screen is not the only place an RPC failure is shown, and it
    /// is not even the first. `Connect` runs the ownership check before a
    /// purchase screen exists in the flow at all, and its failure arm puts the
    /// error straight into the window's error box - which is also where the
    /// blocked screen's `Try Again` button sends the buyer back to. Driven
    /// through `handle` so the message asserted on is the one the window is
    /// actually given.
    ///
    /// The other `eval_err` call sites in this file - the cooldown check, the
    /// supply cap, `nextTokenId`, the price read and the tx pollers - render
    /// the same `RpcError` the same way, and inherit the same protection from
    /// its constructors rather than from anything here.
    #[test]
    fn the_ownership_check_error_box_never_shows_the_packed_endpoint() {
        const KEY: &str = "c7e1f4a30b9d42e8a6135f0c8b27d954";
        let (state, rx) = state_for(&format!("http://127.0.0.1:1/v2/{KEY}?apiKey={KEY}"));

        state.handle(format!(r#"{{"type":"connect","address":"{BUYER}"}}"#));

        let scripts = scripts(&rx);
        assert_eq!(
            scripts.len(),
            1,
            "expected one error message, got {scripts:?}"
        );
        let shown = &scripts[0];
        assert!(
            shown.contains("ownership check failed"),
            "this test is meant to drive the ownership check: {shown}"
        );
        assert!(
            !shown.contains(KEY),
            "the packed endpoint's key reached the window: {shown}"
        );
    }

    /// What the registry said changes the words, because it changes which of
    /// the two innocent explanations is still standing.
    ///
    /// Three wordings, not one: "we did not ask" (no registry published on this
    /// chain, which is every chain today), "we asked and it had no record", and
    /// "we could not ask". Only the last leaves the buyer's own check
    /// incomplete, and only the middle one weakens "this app is older than the
    /// contract" - so a screen that said the same thing in all three would be
    /// telling a buyer something false in two of them.
    #[test]
    fn the_registrys_answer_changes_what_the_screen_says() {
        let notice_for = |registry: RegistryOutcome| {
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                    code_len: 4_096,
                    exposed: vec![],
                    registry,
                })),
            )
        };

        let not_consulted = notice_for(RegistryOutcome::NotConsulted);
        let unknown = notice_for(RegistryOutcome::Unknown);
        let unavailable = notice_for(RegistryOutcome::Unavailable(
            "the node did not answer".into(),
        ));

        assert!(
            !not_consulted.body.contains("on-chain record"),
            "with no registry published the screen must read exactly as it did before this \
             step existed: {}",
            not_consulted.body
        );
        assert!(
            unknown.body.contains("has no entry for this code either"),
            "{}",
            unknown.body
        );
        assert!(
            unavailable.body.contains("could not reach"),
            "{}",
            unavailable.body
        );

        // All three are the same settled answer about the address. A Try Again
        // button on any of them invites clicking until the check passes.
        for notice in [&not_consulted, &unknown, &unavailable] {
            assert!(!notice.retryable);
            assert_eq!(notice.title, not_consulted.title);
        }
    }

    /// The registry is canonical rub3 code and is still not a licence, so
    /// pointing a purchase at it is the wrong-address mistake rather than the
    /// wrong-code one - and the buyer is told what that address actually is.
    #[test]
    fn the_code_registry_is_named_as_the_wrong_address_not_as_bad_code() {
        let notice = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::NotALicence {
                contract: "Rub3CodeRegistry".to_string(),
                role: Some(Role::CodeRegistry),
            }),
        );
        assert!(notice.body.contains("genuine rub3 code"), "{}", notice.body);
        assert!(
            notice.body.contains("records which rub3 code is genuine"),
            "{}",
            notice.body
        );
        assert!(
            notice.body.contains("The address is wrong"),
            "{}",
            notice.body
        );
    }

    /// A role a code registry newer than this app published. The code is
    /// vouched for and the app still cannot say what it is for, so it refuses
    /// and says that rather than guessing at the first role it knows - which is
    /// a licence, and would be a purchase.
    #[test]
    fn a_role_this_build_has_no_name_for_is_refused_in_words_a_person_can_read() {
        let notice = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::NotALicence {
                contract: "Rub3SomethingNew".to_string(),
                role: None,
            }),
        );
        assert!(
            notice.body.contains("too old to know about"),
            "{}",
            notice.body
        );
        assert!(notice.body.contains("Rub3SomethingNew"), "{}", notice.body);
    }

    /// A failed registry read never puts the packed endpoint on a buyer's
    /// screen.
    ///
    /// The blocked screen tells the buyer to send `detail` to whoever published
    /// the software, so anything in it is as good as published. §2.8 settled
    /// that for the contract's own code read; §2.9 added a second chain read
    /// whose failure reason travels inside the refusal rather than beside it,
    /// which is a different route to the same leak. `rpc` has already reduced
    /// the URL to `scheme://host[:port]`, so what is at stake here is the host
    /// of the endpoint this build was packed with - not the buyer's to publish,
    /// and no use to them either.
    ///
    /// The registry's *answer* is a different thing and is kept: `Unknown`
    /// names no endpoint and is half of what the refusal means.
    #[test]
    fn a_failed_registry_read_never_puts_the_packed_endpoint_on_screen() {
        const HOST: &str = "rpc.example.invalid";
        const KEY: &str = "sk-live-do-not-publish";

        let notice = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                code_len: 4_096,
                exposed: vec![],
                registry: RegistryOutcome::Unavailable(format!(
                    "its record could not be read: transport error: error sending request for url \
                     (https://{HOST}/v2/{KEY})"
                )),
            })),
        );

        for field in [&notice.body, &notice.detail, &notice.title.to_string()] {
            assert!(!field.contains(KEY), "the packed endpoint's key: {field}");
            assert!(!field.contains(HOST), "the packed endpoint's host: {field}");
        }
        assert!(
            notice.body.contains("could not reach"),
            "the buyer still has to be told the check was incomplete: {}",
            notice.body
        );

        // The registry's answer, as opposed to its failure, names no endpoint
        // and stays in the detail the buyer is asked to forward.
        let answered = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                code_len: 4_096,
                exposed: vec![],
                registry: RegistryOutcome::Unknown,
            })),
        );
        assert!(
            answered.detail.contains("has no record of it either"),
            "{}",
            answered.detail
        );
    }
}
