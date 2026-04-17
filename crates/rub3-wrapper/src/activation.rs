use alloy::primitives::Address;

use crate::webview::{ActivationContext, ActivationResult};
use crate::{license, rpc, store, webview};

#[derive(Debug)]
pub enum ActivationError {
    Cancelled,
    OwnershipMismatch,
    Error(String),
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationError::Cancelled => write!(f, "activation cancelled"),
            ActivationError::OwnershipMismatch => {
                write!(f, "wallet does not own the license token on-chain")
            }
            ActivationError::Error(e) => write!(f, "{e}"),
        }
    }
}

/// Ensures a valid license exists for `app_id` on this machine.
///
/// Tries three paths in order:
///   1. Tier-3 session fast path (cooldown feature): load the most-recent
///      valid session, verify its signature + expiry, and return if good.
///   2. Legacy `LicenseProof` fast path: load the stored proof, verify
///      signature + (when a contract is configured) on-chain ownership.
///   3. Slow path: open the activation webview and wait for user completion.
///
/// On webview success the appropriate record is persisted to disk before
/// returning `Ok(())`.
pub fn ensure(
    app_id: &str,
    contract: &str,
    chain_id: u64,
    rpc_url: &str,
    developer_ens: Option<String>,
    session_ttl_secs: i64,
) -> Result<(), ActivationError> {
    // ── Fast path 1: existing session (tier 3) ───────────────────────────────
    #[cfg(feature = "cooldown")]
    if try_session_fast_path(app_id, rpc_url) {
        return Ok(());
    }

    // ── Fast path 2: existing legacy proof ───────────────────────────────────
    if try_legacy_fast_path(app_id, contract, rpc_url) {
        return Ok(());
    }

    // ── Slow path: activation window ─────────────────────────────────────────
    let ctx = ActivationContext {
        app_id: app_id.to_string(),
        contract: contract.to_string(),
        chain_id,
        rpc_url: rpc_url.to_string(),
        developer_ens,
        session_ttl_secs,
    };

    match webview::run_activation_window(ctx) {
        ActivationResult::LegacySuccess { proof } => {
            store::save_proof(app_id, &proof)
                .map_err(|e| ActivationError::Error(e.to_string()))?;
            Ok(())
        }
        #[cfg(feature = "cooldown")]
        ActivationResult::SessionSuccess { session } => {
            crate::session_store::save_session(&session)
                .map_err(|e| ActivationError::Error(e.to_string()))?;
            Ok(())
        }
        ActivationResult::Cancelled => Err(ActivationError::Cancelled),
        ActivationResult::Error(msg) => Err(ActivationError::Error(msg)),
    }
}

// ── Fast paths ────────────────────────────────────────────────────────────────

/// Returns `true` if a valid, non-expired session is cached for `app_id`.
///
/// Always performs local verification (signature + expiry). On roughly 1 in 5
/// cold starts it additionally performs on-chain re-verification: fetching the
/// activation tx receipt and confirming it lines up with the session's
/// `contract` + `activation_block_hash`. This catches forged sessions that
/// carry fabricated tx hashes without paying network cost on every launch.
///
/// An on-chain check that fails with a transport error (no network, bad URL)
/// falls open — i.e. we still return `true` — so offline launches aren't
/// broken. A check that succeeds-and-contradicts (wrong contract, wrong block
/// hash, reverted tx) falls closed and forces re-activation.
#[cfg(feature = "cooldown")]
fn try_session_fast_path(app_id: &str, rpc_url: &str) -> bool {
    let session = match crate::session_store::load_latest_session(app_id) {
        Ok(s)  => s,
        Err(_) => return false,
    };

    if crate::session::verify_local(&session).is_err() {
        return false;
    }

    // Re-verify probabilistically — only when the session carries the fields
    // (session_id present ⇒ tier-3 session that went through activate()).
    if session.session_id.is_some() && crate::session::should_reverify() {
        match crate::session::verify_onchain(&session, rpc_url) {
            Ok(())                              => {}
            Err(crate::session::VerifyError::Rpc(_)) => {
                // Offline / transport failure: fall open, keep the session.
            }
            Err(_) => return false,
        }
    }

    true
}

/// Returns `true` if the legacy `LicenseProof` is present and still valid.
///
/// When `contract` is non-zero, also confirms the wallet still owns the token
/// on-chain. Network errors fall closed (return false) so the user re-activates.
fn try_legacy_fast_path(app_id: &str, contract: &str, rpc_url: &str) -> bool {
    let proof = match store::load_proof(app_id) {
        Ok(p) => p,
        Err(_) => return false,
    };

    if license::verify(&proof).is_err() {
        return false;
    }

    let contract_addr: Address = contract.parse().unwrap_or(Address::ZERO);
    if contract_addr.is_zero() {
        return true;
    }

    match rpc::owner_of(rpc_url, contract_addr, proof.token_id) {
        Ok(owner) => {
            let owner_hex = format!("0x{}", hex::encode(owner.as_slice()));
            owner_hex.eq_ignore_ascii_case(&proof.wallet_address)
        }
        Err(_) => false,
    }
}
