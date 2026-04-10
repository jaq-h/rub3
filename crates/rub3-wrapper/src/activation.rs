use crate::webview::{ActivationContext, ActivationResult};
use crate::{license, machine_id, store, webview};

#[derive(Debug)]
pub enum ActivationError {
    MachineId(machine_id::MachineIdError),
    Cancelled,
    Error(String),
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationError::MachineId(e) => write!(f, "machine ID unavailable: {e}"),
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
    let mid = machine_id::machine_id(app_id).map_err(ActivationError::MachineId)?;

    // Fast path: a valid proof is already stored.
    if let Ok(proof) = store::load_proof(app_id) {
        if license::verify(&proof, &mid).is_ok() {
            return Ok(());
        }
        // Proof exists but is invalid (wrong machine, bad sig, etc.) — fall through.
    }

    // Slow path: open the activation window.
    let ctx = ActivationContext {
        app_id: app_id.to_string(),
        contract: contract.to_string(),
        chain_id,
        rpc_url: rpc_url.to_string(),
        developer_ens,
        machine_id: mid,
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
