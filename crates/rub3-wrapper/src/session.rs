//! Session schema and verification (tiers 1-4).
//!
//! Replaces the legacy `LicenseProof` model from `license.rs` for tiers ≥ 1.
//! See `architecture.md` §"Session Model" for field semantics per tier.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

// ── Session schema ───────────────────────────────────────────────────────────

/// Cached session written to `~/.rub3/sessions/<app_id>/<token_id>.json`.
///
/// Which fields are populated depends on the tier:
///   tier 1: app_id, token_id, wallet, nonce, issued_at, expires_at, signature, chain, contract
///   tier 2: same as tier 1 (verification differs, not the payload)
///   tier 3: adds activation_tx, activation_block, activation_block_hash, session_id
///   tier 4: adds device_pubkey; omits expires_at (device challenge replaces TTL)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub app_id: String,
    pub token_id: u64,
    pub wallet: String,

    pub nonce: String,
    pub issued_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,

    pub signature: String,
    pub chain: String,
    pub contract: String,

    // ── tier 3+ ──
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_tx: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_block: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_block_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u64>,

    // ── tier 4 ──
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_pubkey: Option<String>,
}

#[derive(Debug)]
pub enum VerifyError {
    InvalidSignature(String),
    AddressMismatch { expected: String, recovered: String },
    Expired,
    MissingField(&'static str),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::InvalidSignature(e) => write!(f, "invalid signature: {e}"),
            VerifyError::AddressMismatch { expected, recovered } => write!(
                f,
                "address mismatch: session claims {expected}, signature recovers {recovered}"
            ),
            VerifyError::Expired => write!(f, "session expired"),
            VerifyError::MissingField(name) => write!(f, "missing required field: {name}"),
        }
    }
}

// ── Message construction ──────────────────────────────────────────────────────

/// Builds the preimage the wallet signs at session creation.
///
/// Inputs are tier-dependent — pass `None` for fields not used at the caller's tier.
pub fn session_message(
    _app_id: &str,
    _token_id: u64,
    _wallet: &str,
    _nonce: &str,
    _expires_at: Option<&str>,
    _activation_block_hash: Option<&str>,
    _session_id: Option<u64>,
    _device_pubkey: Option<&str>,
) -> [u8; 32] {
    // TODO: SHA-256 over the tier-appropriate concatenation.
    // See architecture.md §"Session Format" for exact layout per tier.
    unimplemented!("session_message: scaffold only — implement per architecture.md §Session Format")
}

// ── Verification ──────────────────────────────────────────────────────────────

/// Local signature + expiry verification. Does not touch the network.
pub fn verify_local(_session: &Session) -> Result<(), VerifyError> {
    // TODO: reconstruct session_message, recover signer via license::recover_address,
    //       compare to session.wallet, then check expires_at if present.
    unimplemented!("verify_local: scaffold only")
}

/// Returns true when the session has an `expires_at` in the past.
/// Tier 4 sessions have no expires_at and therefore never expire by time alone.
pub fn is_expired(_session: &Session) -> bool {
    // TODO: parse session.expires_at as RFC3339, compare to chrono::Utc::now().
    unimplemented!("is_expired: scaffold only")
}
