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

    let ipc_state = IpcState {
        app_id: ctx.app_id.clone(),
        contract: ctx.contract.clone(),
        chain_id: ctx.chain_id,
        rpc_url: ctx.rpc_url.clone(),
        developer_ens: ctx.developer_ens.clone(),
        session_ttl_secs: ctx.session_ttl_secs,
        cmd_tx: cmd_tx.clone(),
        result_tx: result_tx.clone(),
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

        let payload = serde_json::json!({
            "tokenId":         token_id,
            "ownerAddress":    address,
            "contractAddress": self.contract,
            "chainId":         self.chain_id,
            "ready":           ready,
            "blocksRemaining": blocks_remaining,
            "calldata":        calldata,
        });
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
        match crate::attest::verify_before_purchase(&self.rpc_url, contract_addr) {
            // One line, on the one path in this file that spends money, naming
            // what the money is about to go to. Mirrors the agent door.
            Ok(canonical) => eprintln!(
                "rub3: {contract_addr} verified as canonical {} ({})",
                canonical.contract, canonical.release
            ),
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
        }

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
        });
        self.eval(format!("window.rub3.onShowPurchase({})", payload));
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
    use crate::attest::{GateError, Refusal, Role};
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
                detail: finding.to_string(),
                retryable: false,
            },

            // Code is there and it is not ours. Both innocent explanations are
            // stated first, because the honest reading of a miss is "this app
            // does not know that contract", not "that contract is a fake".
            Refusal::Unrecognised(finding) => RefusalNotice {
                title: "This app does not recognise the contract's code",
                body: format!(
                    "There is a contract at {contract}, but its code does not match any rub3 \
                     licence contract this app was built to trust. It may be a newer release \
                     than this copy of the app knows about, or it may be a modified copy that \
                     does not behave the way a rub3 licence does. From here the two look the \
                     same, so you are not being asked to pay either of them. Nothing has been \
                     signed and nothing has been sent."
                ),
                next_step: "Confirm the address with whoever published this software. If they \
                            say it is current, this copy of the app is older than the contract \
                            and needs updating.",
                detail: finding.to_string(),
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
                        Role::Factory =>
                            "the factory that deploys licence contracts rather than one that \
                             sells them",
                        Role::Deployer =>
                            "an internal helper the factory uses to deploy licence contracts",
                        // Not a state the gate produces - it accepts every
                        // licence-role match - but kept total rather than
                        // unreachable so a role added to the table later
                        // cannot panic a buyer's window.
                        Role::Licence => "which this app did not accept as a purchase target",
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

/// The purchase gate, and the words it puts in front of a person.
///
/// Compiled only where a purchase can be made from this window, which is the
/// same condition the gate itself is compiled under: below tier-3 there is no
/// `show_purchase` to guard and nothing here to test.
#[cfg(all(test, feature = "onchain-write"))]
mod tests {
    use super::*;
    use crate::attest::{Refusal, Role, Unrecognised};
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
            })),
        );
        let not_a_licence = refusal_notice(
            CONTRACT,
            &crate::attest::GateError::Refused(Refusal::NotALicence {
                contract: "Rub3Factory",
                role: Role::Factory,
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
                })),
            ),
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Refused(Refusal::Unrecognised(Unrecognised {
                    code_len: 4_096,
                    exposed: vec![],
                })),
            ),
            refusal_notice(
                CONTRACT,
                &crate::attest::GateError::Refused(Refusal::NotALicence {
                    contract: "Rub3Factory",
                    role: Role::Factory,
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
}
