//! Session schema and verification (tiers 1-4).
//!
//! Replaces the legacy `LicenseProof` model from `license.rs` for tiers ≥ 1.
//! See `architecture.md` §"Session Model" for field semantics per tier.

use serde::{Deserialize, Serialize};

// ── Session schema ────────────────────────────────────────────────────────────

/// Cached session written to `~/.rub3/sessions/<app_id>/<token_id>.json`.
///
/// Populated fields depend on tier:
///   1-2: app_id, token_id, wallet, nonce, issued_at, expires_at, signature, chain, contract
///   3:   adds activation_tx, activation_block, activation_block_hash, session_id
///   4:   adds device_pubkey; omits expires_at (device challenge replaces TTL)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub app_id:    String,
    pub token_id:  u64,
    pub wallet:    String,

    pub nonce:     String,
    pub issued_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,

    pub signature: String,
    pub chain:     String,
    pub contract:  String,

    // ── tier 3+ ──────────────────────────────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_tx:         Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_block:      Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_block_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id:            Option<u64>,

    // ── tier 4 ───────────────────────────────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_pubkey: Option<String>,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum VerifyError {
    InvalidSignature(String),
    AddressMismatch { expected: String, recovered: String },
    Expired,
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
        }
    }
}

// ── Message construction ──────────────────────────────────────────────────────

/// Builds the 32-byte preimage the wallet signs at session creation.
///
/// Fields are SHA-256'd in a fixed order; optional fields are omitted when
/// `None`. Integers use big-endian encoding for fixed width.
///
/// Tier mapping:
///   1-2: app_id, token_id, wallet, nonce, expires_at
///   3:   + activation_block_hash, session_id
///   4:   + device_pubkey (expires_at is None for tier 4)
pub fn session_message(
    app_id:                &str,
    token_id:              u64,
    wallet:                &str,
    nonce:                 &str,
    expires_at:            Option<&str>,
    activation_block_hash: Option<&str>,
    session_id:            Option<u64>,
    device_pubkey:         Option<&str>,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(app_id.as_bytes());
    h.update(token_id.to_be_bytes());
    h.update(wallet.as_bytes());
    h.update(nonce.as_bytes());
    if let Some(exp) = expires_at            { h.update(exp.as_bytes()); }
    if let Some(bh)  = activation_block_hash { h.update(bh.as_bytes());  }
    if let Some(sid) = session_id            { h.update(sid.to_be_bytes()); }
    if let Some(dpk) = device_pubkey         { h.update(dpk.as_bytes()); }
    h.finalize().into()
}

/// Generates a cryptographically random 32-byte hex nonce.
pub fn new_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// ── Verification ──────────────────────────────────────────────────────────────

/// Local signature + expiry check. Does not touch the network.
///
/// Reconstructs the session message from the stored fields, recovers the
/// signer via `personal_sign`, compares to `session.wallet`, and checks expiry.
pub fn verify_local(session: &Session) -> Result<(), VerifyError> {
    if is_expired(session) {
        return Err(VerifyError::Expired);
    }

    let msg = session_message(
        &session.app_id,
        session.token_id,
        &session.wallet,
        &session.nonce,
        session.expires_at.as_deref(),
        session.activation_block_hash.as_deref(),
        session.session_id,
        session.device_pubkey.as_deref(),
    );

    let recovered = crate::license::recover_address(&msg, &session.signature)
        .map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;

    if !recovered.eq_ignore_ascii_case(&session.wallet) {
        return Err(VerifyError::AddressMismatch {
            expected:  session.wallet.clone(),
            recovered,
        });
    }

    Ok(())
}

/// Returns `true` when the session has an `expires_at` in the past.
///
/// Tier 4 sessions have no `expires_at` and are never considered expired by
/// this function (device-key challenge handles their validity instead).
/// An unparseable timestamp is treated as already expired.
pub fn is_expired(session: &Session) -> bool {
    match &session.expires_at {
        None => false,
        Some(ts) => match ts.parse::<chrono::DateTime<chrono::Utc>>() {
            Ok(exp) => chrono::Utc::now() >= exp,
            Err(_)  => true,
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(expires_at: Option<&str>) -> Session {
        Session {
            app_id:                "com.rub3.test".into(),
            token_id:              1,
            wallet:                "0x0000000000000000000000000000000000000001".into(),
            nonce:                 "aabbcc".into(),
            issued_at:             "2026-01-01T00:00:00Z".into(),
            expires_at:            expires_at.map(String::from),
            signature:             "0x00".into(),
            chain:                 "base".into(),
            contract:              "0x0000000000000000000000000000000000000002".into(),
            activation_tx:         None,
            activation_block:      None,
            activation_block_hash: None,
            session_id:            None,
            device_pubkey:         None,
        }
    }

    #[test]
    fn session_message_is_deterministic() {
        let a = session_message("app", 1, "0xabc", "nonce", Some("2030-01-01T00:00:00Z"), None, None, None);
        let b = session_message("app", 1, "0xabc", "nonce", Some("2030-01-01T00:00:00Z"), None, None, None);
        assert_eq!(a, b);
    }

    #[test]
    fn session_message_differs_by_nonce() {
        let a = session_message("app", 1, "0xabc", "nonce1", None, None, None, None);
        let b = session_message("app", 1, "0xabc", "nonce2", None, None, None, None);
        assert_ne!(a, b);
    }

    #[test]
    fn session_message_differs_by_expires_at_presence() {
        let with_exp    = session_message("app", 1, "0xabc", "n", Some("2030-01-01T00:00:00Z"), None, None, None);
        let without_exp = session_message("app", 1, "0xabc", "n", None, None, None, None);
        assert_ne!(with_exp, without_exp);
    }

    #[test]
    fn session_message_differs_by_tier3_fields() {
        let tier2 = session_message("app", 1, "0xabc", "n", Some("2030-01-01T00:00:00Z"), None, None, None);
        let tier3 = session_message("app", 1, "0xabc", "n", Some("2030-01-01T00:00:00Z"), Some("0xdeadbeef"), Some(42), None);
        assert_ne!(tier2, tier3);
    }

    #[test]
    fn is_expired_false_for_future() {
        let s = make_session(Some("2099-01-01T00:00:00Z"));
        assert!(!is_expired(&s));
    }

    #[test]
    fn is_expired_true_for_past() {
        let s = make_session(Some("2000-01-01T00:00:00Z"));
        assert!(is_expired(&s));
    }

    #[test]
    fn is_expired_false_for_none() {
        let s = make_session(None);
        assert!(!is_expired(&s), "tier 4 sessions with no expires_at should not expire");
    }

    #[test]
    fn is_expired_true_for_unparseable_timestamp() {
        let s = make_session(Some("not-a-date"));
        assert!(is_expired(&s));
    }

    #[test]
    fn new_nonce_is_unique() {
        let a = new_nonce();
        let b = new_nonce();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64); // 32 bytes hex-encoded
    }

    #[test]
    fn verify_local_round_trip() {
        // Generate a real wallet + sign a session message, then verify_local.
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let wallet = crate::license::public_key_to_address(verifying_key);

        let app_id   = "com.rub3.test";
        let token_id = 7u64;
        let nonce    = new_nonce();
        let expires_at = "2099-01-01T00:00:00Z";

        let msg = session_message(app_id, token_id, &wallet, &nonce, Some(expires_at), None, None, None);

        // Apply personal_sign prefix before signing (matches wallet behaviour).
        let prefixed = crate::license::personal_sign_hash(&msg);

        use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature};
        let (sig, rec_id): (Signature, RecoveryId) = signing_key.sign_prehash(&prefixed).unwrap();
        let v = rec_id.to_byte() + 27;
        let sig_bytes: Vec<u8> = sig.to_bytes().iter().copied().chain(std::iter::once(v)).collect();
        let sig_hex = format!("0x{}", hex::encode(&sig_bytes));

        let session = Session {
            app_id:                app_id.into(),
            token_id,
            wallet:                wallet.clone(),
            nonce,
            issued_at:             chrono::Utc::now().to_rfc3339(),
            expires_at:            Some(expires_at.into()),
            signature:             sig_hex,
            chain:                 "base".into(),
            contract:              "0x0000000000000000000000000000000000000002".into(),
            activation_tx:         None,
            activation_block:      None,
            activation_block_hash: None,
            session_id:            None,
            device_pubkey:         None,
        };

        assert!(verify_local(&session).is_ok());
    }

    #[test]
    fn verify_local_wrong_wallet_fails() {
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let signing_key  = SigningKey::random(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let real_wallet  = crate::license::public_key_to_address(verifying_key);

        let nonce     = new_nonce();
        let expires_at = "2099-01-01T00:00:00Z";
        let msg = session_message("app", 1, &real_wallet, &nonce, Some(expires_at), None, None, None);
        let prefixed = crate::license::personal_sign_hash(&msg);

        use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature};
        let (sig, rec_id): (Signature, RecoveryId) = signing_key.sign_prehash(&prefixed).unwrap();
        let v = rec_id.to_byte() + 27;
        let sig_bytes: Vec<u8> = sig.to_bytes().iter().copied().chain(std::iter::once(v)).collect();
        let sig_hex = format!("0x{}", hex::encode(&sig_bytes));

        let session = Session {
            app_id:                "app".into(),
            token_id:              1,
            wallet:                "0x0000000000000000000000000000000000000099".into(), // wrong
            nonce,
            issued_at:             chrono::Utc::now().to_rfc3339(),
            expires_at:            Some(expires_at.into()),
            signature:             sig_hex,
            chain:                 "base".into(),
            contract:              "0x0000000000000000000000000000000000000002".into(),
            activation_tx:         None,
            activation_block:      None,
            activation_block_hash: None,
            session_id:            None,
            device_pubkey:         None,
        };

        assert!(matches!(verify_local(&session), Err(VerifyError::AddressMismatch { .. })));
    }

    #[test]
    fn verify_local_expired_fails() {
        let s = make_session(Some("2000-01-01T00:00:00Z"));
        assert!(matches!(verify_local(&s), Err(VerifyError::Expired)));
    }
}
