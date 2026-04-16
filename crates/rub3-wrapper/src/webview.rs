use std::sync::mpsc;

use serde::Deserialize;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

use crate::license::LicenseProof;

const ACTIVATION_HTML: &str = include_str!("../assets/activation.html");

// ── Public types ──────────────────────────────────────────────────────────────

pub struct ActivationContext {
    pub app_id: String,
    pub contract: String,
    pub chain_id: u64,
    pub rpc_url: String,
    pub developer_ens: Option<String>,
}

pub enum ActivationResult {
    Success { proof: LicenseProof },
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
    /// User completed the signature prompt.
    Signed {
        token_id: u64,
        owner_address: String,
        signature: String,
        paid_by: Option<String>,
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
        .with_inner_size(tao::dpi::LogicalSize::new(480u32, 600u32))
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

        // Drain commands sent by the IPC handler.
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

/// Shared state available to the IPC callback.
struct IpcState {
    app_id: String,
    contract: String,
    chain_id: u64,
    rpc_url: String,
    developer_ens: Option<String>,
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
                // TODO: enumerate tokens via tokenOfOwnerByIndex once the
                // contract exposes ERC-721 Enumerable. For now check token 1.
                let contract_addr: alloy::primitives::Address =
                    self.contract.parse().unwrap_or(alloy::primitives::Address::ZERO);

                let token_id: u64 = 1;

                if contract_addr.is_zero() {
                    let payload = serde_json::json!({
                        "tokenId":      token_id,
                        "ownerAddress": address,
                    });
                    self.eval(format!("window.rub3.onShowActivate({})", payload));
                    return;
                }

                match crate::rpc::owner_of(&self.rpc_url, contract_addr, token_id) {
                    Ok(owner) => {
                        let owner_hex = format!("0x{}", hex::encode(owner.as_slice()));
                        if owner_hex.eq_ignore_ascii_case(&address) {
                            let payload = serde_json::json!({
                                "tokenId":      token_id,
                                "ownerAddress": address,
                            });
                            self.eval(format!("window.rub3.onShowActivate({})", payload));
                        } else {
                            self.eval(
                                "window.rub3.onError('Wallet does not own a license token')"
                                    .into(),
                            );
                        }
                    }
                    Err(e) => {
                        self.eval(format!(
                            "window.rub3.onError({})",
                            serde_json::json!(format!("ownership check failed: {e}"))
                        ));
                    }
                }
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
                let _ = self.result_tx.send(ActivationResult::Success { proof });
                let _ = self.cmd_tx.send(Cmd::Close);
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

    fn eval(&self, script: String) {
        let _ = self.cmd_tx.send(Cmd::Eval(script));
    }
}
