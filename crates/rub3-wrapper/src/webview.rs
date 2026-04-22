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

/// Tx receipt polling — attempts × interval = total timeout.
#[cfg(feature = "cooldown")]
const TX_POLL_ATTEMPTS:     u32 = 10;
#[cfg(feature = "cooldown")]
const TX_POLL_INTERVAL_SECS: u64 = 3;

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
    LegacySuccess { proof: LicenseProof },
    /// Tier-3 session issued after a confirmed `activate()` tx.
    #[cfg(feature = "cooldown")]
    SessionSuccess { session: Session },
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
    Connect { address: String },
    /// User selected a token from the multi-token selection screen.
    TokenSelected { token_id: u64, owner_address: String },
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
    /// Tier-3 path: user signed the session message. All fields from the
    /// tx-confirmation step are echoed back so the wrapper can assemble the
    /// Session without holding in-process state between IPC calls.
    #[cfg(feature = "cooldown")]
    SessionSigned {
        signature:             String,
        token_id:              u64,
        owner_address:         String,
        identity:              String,
        user_id:               String,
        tba:                   Option<String>,
        nonce:                 String,
        expires_at:            String,
        session_id:            u64,
        activation_tx:         String,
        activation_block:      u64,
        activation_block_hash: String,
    },
    Cancel,
    Error { message: String },
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
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
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
    app_id:           String,
    contract:         String,
    chain_id:         u64,
    rpc_url:          String,
    developer_ens:    Option<String>,
    session_ttl_secs: i64,
    cmd_tx:           mpsc::Sender<Cmd>,
    result_tx:        mpsc::Sender<ActivationResult>,
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
                let contract_addr: alloy::primitives::Address =
                    self.contract.parse().unwrap_or(alloy::primitives::Address::ZERO);

                if contract_addr.is_zero() {
                    // No contract configured — skip on-chain check, use token 1 (legacy).
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
                        self.eval(format!(
                            "window.rub3.onError({})",
                            serde_json::json!("No license tokens found for this wallet")
                        ));
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

            IpcMessage::TokenSelected { token_id, owner_address } => {
                self.proceed_after_token_selected(&owner_address, token_id);
            }

            IpcMessage::Signed { token_id, owner_address, signature, paid_by } => {
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
                let _ = self.result_tx.send(ActivationResult::LegacySuccess { proof });
                let _ = self.cmd_tx.send(Cmd::Close);
            }

            #[cfg(feature = "cooldown")]
            IpcMessage::ActivateTxSent { tx_hash, token_id, owner_address } => {
                self.spawn_tx_poller(tx_hash, token_id, owner_address);
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
        #[cfg(feature = "cooldown")]
        {
            self.show_cooldown(owner_address, token_id);
            return;
        }
        #[cfg(not(feature = "cooldown"))]
        {
            self.show_activate(owner_address, token_id);
        }
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

            let receipt = match poll_receipt(&state.rpc_url, &tx_hash) {
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
                        "activate() tx was sent to {to}, expected {}", state.contract
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

            let session_id = match crate::rpc::active_session_id(
                &state.rpc_url, contract_addr, token_id,
            ) {
                Ok(s) => s,
                Err(e) => {
                    state.eval_err(&format!("failed to read activeSessionId: {e}"));
                    return;
                }
            };

            // ── Identity model + TBA derivation ─────────────────────────────
            // Read identityModel once; for account-model deploys, read the
            // tbaImplementation and derive the TBA locally (pure CREATE2).
            let model_u8 = match crate::rpc::identity_model(&state.rpc_url, contract_addr) {
                Ok(m) => m,
                Err(e) => {
                    state.eval_err(&format!("failed to read identityModel: {e}"));
                    return;
                }
            };
            let model = match crate::identity::IdentityModel::from_u8(model_u8) {
                Some(m) => m,
                None => {
                    state.eval_err(&format!("contract returned unknown identityModel = {model_u8}"));
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

            let tba_addr_opt: Option<alloy::primitives::Address> = match model {
                crate::identity::IdentityModel::Access => None,
                crate::identity::IdentityModel::Account => {
                    let impl_addr = match crate::rpc::tba_implementation(
                        &state.rpc_url, contract_addr,
                    ) {
                        Ok(a) => a,
                        Err(e) => {
                            state.eval_err(&format!("failed to read tbaImplementation: {e}"));
                            return;
                        }
                    };
                    Some(crate::identity::derive_tba(
                        impl_addr, state.chain_id, contract_addr, token_id,
                    ))
                }
            };

            let user_id = crate::identity::resolve_user_id(model, wallet_addr, tba_addr_opt);
            let tba_str = tba_addr_opt.map(crate::identity::format_addr);
            let identity_str = model.as_str();

            let nonce = crate::session::new_nonce();
            let expires_at = (chrono::Utc::now()
                + chrono::Duration::seconds(state.session_ttl_secs))
            .to_rfc3339();

            let session_msg = crate::session::session_message(
                &state.app_id,
                token_id,
                identity_str,
                &user_id,
                &owner_address,
                &nonce,
                Some(&expires_at),
                Some(&receipt.block_hash),
                Some(session_id),
                None,
            );
            let session_msg_hex = format!("0x{}", hex::encode(session_msg));

            let payload = serde_json::json!({
                "tokenId":             token_id,
                "ownerAddress":        owner_address,
                "identity":            identity_str,
                "userId":              user_id,
                "tba":                 tba_str,
                "txHash":              tx_hash,
                "blockNumber":         receipt.block_number,
                "blockHash":           receipt.block_hash,
                "sessionId":           session_id,
                "nonce":               nonce,
                "expiresAt":           expires_at,
                "sessionMessage":      session_msg_hex,
            });
            state.eval(format!("window.rub3.onTxConfirmed({})", payload));
        });
    }

    #[cfg(feature = "cooldown")]
    fn finalize_session(&self, a: FinalizeArgs) {
        let session = Session {
            app_id:                self.app_id.clone(),
            token_id:              a.token_id,
            identity:              a.identity,
            user_id:               a.user_id,
            tba:                   a.tba,
            wallet:                a.owner_address,
            nonce:                 a.nonce,
            issued_at:             chrono::Utc::now().to_rfc3339(),
            expires_at:            Some(a.expires_at),
            signature:             a.signature,
            chain:                 "base".to_string(),
            contract:              self.contract.clone(),
            activation_tx:         Some(a.activation_tx),
            activation_block:      Some(a.activation_block),
            activation_block_hash: Some(a.activation_block_hash),
            session_id:            Some(a.session_id),
            device_pubkey:         None,
        };

        if let Err(e) = crate::session::verify_local(&session) {
            self.eval_err(&format!("signature verification failed: {e}"));
            return;
        }

        let _ = self.result_tx.send(ActivationResult::SessionSuccess { session });
        let _ = self.cmd_tx.send(Cmd::Close);
    }

    // ── Primitives ───────────────────────────────────────────────────────────

    fn eval(&self, script: String) {
        let _ = self.cmd_tx.send(Cmd::Eval(script));
    }

    fn eval_err(&self, msg: &str) {
        self.eval(format!(
            "window.rub3.onError({})",
            serde_json::json!(msg)
        ));
    }
}

// ── Tier-3 helpers ────────────────────────────────────────────────────────────

#[cfg(feature = "cooldown")]
struct FinalizeArgs {
    signature:             String,
    token_id:              u64,
    owner_address:         String,
    identity:              String,
    user_id:               String,
    tba:                   Option<String>,
    nonce:                 String,
    expires_at:            String,
    session_id:            u64,
    activation_tx:         String,
    activation_block:      u64,
    activation_block_hash: String,
}

/// Poll `get_tx_receipt` until mined or timeout. Returns the receipt on
/// success, or an error string on timeout/malformed hash.
#[cfg(feature = "cooldown")]
fn poll_receipt(rpc_url: &str, tx_hash: &str) -> Result<crate::rpc::TxReceipt, String> {
    for attempt in 0..TX_POLL_ATTEMPTS {
        match crate::rpc::get_tx_receipt(rpc_url, tx_hash) {
            Ok(Some(r)) => return Ok(r),
            Ok(None)    => {}
            Err(e)      => return Err(e.to_string()),
        }
        if attempt + 1 < TX_POLL_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_secs(TX_POLL_INTERVAL_SECS));
        }
    }
    Err(format!(
        "tx not confirmed within {}s",
        TX_POLL_ATTEMPTS as u64 * TX_POLL_INTERVAL_SECS
    ))
}
