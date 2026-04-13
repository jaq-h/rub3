use crate::webview::{ActivationContext, ActivationResult};
use crate::{license, store, webview};

#[derive(Debug)]
pub enum ActivationError {
    Cancelled,
    Error(String),
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationError::Cancelled => write!(f, "activation cancelled"),
            ActivationError::Error(e) => write!(f, "{e}"),
        }
    }
}

/// Ensures a valid license proof exists for `app_id` on this machine.
///
/// Returns immediately if a valid proof is already stored. Otherwise opens
/// the activation window and blocks until the user completes or cancels.
/// On success the proof is written to disk.
pub fn ensure(
    app_id: &str,
    contract: &str,
    chain_id: u64,
    rpc_url: &str,
    developer_ens: Option<String>,
) -> Result<(), ActivationError> {
    // Fast path: a valid proof is already stored.
    if let Ok(proof) = store::load_proof(app_id) {
        if license::verify(&proof).is_ok() {
            return Ok(());
        }
        // Proof exists but is invalid (bad sig, etc.) — fall through.
    }

    // Slow path: open the activation window.
    let ctx = ActivationContext {
        app_id: app_id.to_string(),
        contract: contract.to_string(),
        chain_id,
        rpc_url: rpc_url.to_string(),
        developer_ens,
    };

    match webview::run_activation_window(ctx) {
        ActivationResult::Success { proof } => {
            store::save_proof(app_id, &proof).map_err(|e| ActivationError::Error(e.to_string()))?;
            Ok(())
        }
        ActivationResult::Cancelled => Err(ActivationError::Cancelled),
        ActivationResult::Error(msg) => Err(ActivationError::Error(msg)),
    }
}
