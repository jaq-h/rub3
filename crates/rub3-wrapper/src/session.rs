//! Session schema and verification (tiers 1-4).
//!
//! Replaces the legacy `LicenseProof` model from `license.rs` for tiers ≥ 1.
//! See `architecture.md` §"Session Model" for field semantics per tier.

use serde::{Deserialize, Serialize};

// ── Session schema ────────────────────────────────────────────────────────────

/// Cached session written to `~/.rub3/sessions/<app_id>/<token_id>.json`.
///
/// Populated fields depend on tier:
///   1-2: app_id, token_id, identity, user_id, (tba?), wallet, nonce, issued_at,
///        expires_at, signature, chain, contract
///   3:   adds activation_tx, activation_block, activation_block_hash, session_id
///   4:   adds device_pubkey; omits expires_at (device challenge replaces TTL)
///
/// `identity` is the wire string ("access" | "account"). `user_id` is the
/// stable identity key the app sees — wallet address for access model, TBA
/// address for account model. `tba` is populated only for the account model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub app_id:    String,
    pub token_id:  u64,

    // ── Identity ─────────────────────────────────────────────────────────────
    pub identity:  String,
    pub user_id:   String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tba:       Option<String>,

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

    // ── tier-3 on-chain re-verification errors ───────────────────────────────
    #[cfg(feature = "cooldown")]
    MissingTxHash,
    #[cfg(feature = "cooldown")]
    MissingBlockHash,
    #[cfg(feature = "cooldown")]
    Rpc(String),
    #[cfg(feature = "cooldown")]
    ReceiptNotFound,
    #[cfg(feature = "cooldown")]
    TxReverted,
    #[cfg(feature = "cooldown")]
    ContractMismatch { expected: String, got: String },
    #[cfg(feature = "cooldown")]
    BlockHashMismatch { expected: String, got: String },
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
            #[cfg(feature = "cooldown")]
            VerifyError::MissingTxHash => {
                write!(f, "session is missing activation_tx (required for tier-3 re-verify)")
            }
            #[cfg(feature = "cooldown")]
            VerifyError::MissingBlockHash => {
                write!(f, "session is missing activation_block_hash (required for tier-3 re-verify)")
            }
            #[cfg(feature = "cooldown")]
            VerifyError::Rpc(e) => write!(f, "rpc error during on-chain re-verify: {e}"),
            #[cfg(feature = "cooldown")]
            VerifyError::ReceiptNotFound => write!(f, "activation tx receipt not found on-chain"),
            #[cfg(feature = "cooldown")]
            VerifyError::TxReverted => write!(f, "activation tx reverted on-chain"),
            #[cfg(feature = "cooldown")]
            VerifyError::ContractMismatch { expected, got } => write!(
                f,
                "activation tx did not target the license contract: expected {expected}, got {got}"
            ),
            #[cfg(feature = "cooldown")]
            VerifyError::BlockHashMismatch { expected, got } => write!(
                f,
                "activation block hash mismatch: session bound to {expected}, receipt reports {got}"
            ),
        }
    }
}

// ── Message construction ──────────────────────────────────────────────────────

/// Builds the 32-byte preimage the wallet signs at session creation.
///
/// Fields are SHA-256'd in a fixed order; optional fields are omitted when
/// `None`. Integers use big-endian encoding for fixed width.
///
/// `identity` + `user_id` are part of the preimage so a forger cannot flip the
/// identity model of a captured session (e.g. turn an access session into an
/// account session, changing the `user_id` the app keys its data on).
///
/// Tier mapping:
///   1-2: app_id, token_id, identity, user_id, wallet, nonce, expires_at
///   3:   + activation_block_hash, session_id
///   4:   + device_pubkey (expires_at is None for tier 4)
pub fn session_message(
    app_id:                &str,
    token_id:              u64,
    identity:              &str,
    user_id:               &str,
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
    h.update(identity.as_bytes());
    h.update(user_id.as_bytes());
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
        &session.identity,
        &session.user_id,
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

// ── Tier-3 on-chain re-verification ───────────────────────────────────────────

/// Fetches the activation tx receipt and confirms it corresponds to the session:
///   1. `status == true` (tx didn't revert)
///   2. `to` matches the session's `contract`
///   3. `block_hash` matches the session's `activation_block_hash`
///
/// Forged sessions that carry made-up `activation_tx` / `activation_block_hash`
/// fields fail (1) or (3). Sessions pointing at a tx that hit a different
/// contract fail (2).
#[cfg(feature = "cooldown")]
pub fn verify_onchain(session: &Session, rpc_url: &str) -> Result<(), VerifyError> {
    let tx_hash = session
        .activation_tx
        .as_deref()
        .ok_or(VerifyError::MissingTxHash)?;
    let expected_block_hash = session
        .activation_block_hash
        .as_deref()
        .ok_or(VerifyError::MissingBlockHash)?;

    let receipt = crate::rpc::get_tx_receipt(rpc_url, tx_hash)
        .map_err(|e| VerifyError::Rpc(e.to_string()))?
        .ok_or(VerifyError::ReceiptNotFound)?;

    if !receipt.status {
        return Err(VerifyError::TxReverted);
    }

    match &receipt.to {
        Some(to) if to.eq_ignore_ascii_case(&session.contract) => {}
        Some(to) => {
            return Err(VerifyError::ContractMismatch {
                expected: session.contract.clone(),
                got:      to.clone(),
            });
        }
        None => {
            return Err(VerifyError::ContractMismatch {
                expected: session.contract.clone(),
                got:      "<none>".into(),
            });
        }
    }

    if !receipt.block_hash.eq_ignore_ascii_case(expected_block_hash) {
        return Err(VerifyError::BlockHashMismatch {
            expected: expected_block_hash.to_string(),
            got:      receipt.block_hash.clone(),
        });
    }

    Ok(())
}

/// Returns `true` with probability ~1/5 — the sampling gate for tier-3
/// probabilistic on-chain re-verification.
///
/// Amortises the network cost across cold starts: legitimate sessions see a
/// re-verify roughly every five launches, while a forged session is caught
/// after a small, bounded number of attempts.
#[cfg(feature = "cooldown")]
pub fn should_reverify() -> bool {
    use rand::Rng;
    rand::thread_rng().gen_range(0..5) == 0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(expires_at: Option<&str>) -> Session {
        let wallet = "0x0000000000000000000000000000000000000001";
        Session {
            app_id:                "com.rub3.test".into(),
            token_id:              1,
            identity:              "access".into(),
            user_id:               wallet.into(),
            tba:                   None,
            wallet:                wallet.into(),
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
        let a = session_message("app", 1, "access", "0xabc", "0xabc", "nonce", Some("2030-01-01T00:00:00Z"), None, None, None);
        let b = session_message("app", 1, "access", "0xabc", "0xabc", "nonce", Some("2030-01-01T00:00:00Z"), None, None, None);
        assert_eq!(a, b);
    }

    #[test]
    fn session_message_differs_by_nonce() {
        let a = session_message("app", 1, "access", "0xabc", "0xabc", "nonce1", None, None, None, None);
        let b = session_message("app", 1, "access", "0xabc", "0xabc", "nonce2", None, None, None, None);
        assert_ne!(a, b);
    }

    #[test]
    fn session_message_differs_by_expires_at_presence() {
        let with_exp    = session_message("app", 1, "access", "0xabc", "0xabc", "n", Some("2030-01-01T00:00:00Z"), None, None, None);
        let without_exp = session_message("app", 1, "access", "0xabc", "0xabc", "n", None, None, None, None);
        assert_ne!(with_exp, without_exp);
    }

    #[test]
    fn session_message_differs_by_tier3_fields() {
        let tier2 = session_message("app", 1, "access", "0xabc", "0xabc", "n", Some("2030-01-01T00:00:00Z"), None, None, None);
        let tier3 = session_message("app", 1, "access", "0xabc", "0xabc", "n", Some("2030-01-01T00:00:00Z"), Some("0xdeadbeef"), Some(42), None);
        assert_ne!(tier2, tier3);
    }

    #[test]
    fn session_message_differs_by_identity() {
        // Flipping access -> account (with a different user_id) MUST change
        // the preimage, so a captured signature cannot be replayed with a
        // different identity model.
        let access  = session_message("app", 1, "access",  "0xwallet", "0xwallet", "n", None, None, None, None);
        let account = session_message("app", 1, "account", "0xtba",    "0xwallet", "n", None, None, None, None);
        assert_ne!(access, account);
    }

    #[test]
    fn session_message_differs_by_user_id_only() {
        // Same identity string, but swapping user_id alone (e.g. pointing at
        // a different TBA) must change the preimage.
        let a = session_message("app", 1, "account", "0xtba1", "0xwallet", "n", None, None, None, None);
        let b = session_message("app", 1, "account", "0xtba2", "0xwallet", "n", None, None, None, None);
        assert_ne!(a, b);
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
        let identity  = "access";
        let user_id   = wallet.clone();

        let msg = session_message(app_id, token_id, identity, &user_id, &wallet, &nonce, Some(expires_at), None, None, None);

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
            identity:              identity.into(),
            user_id,
            tba:                   None,
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
        let msg = session_message("app", 1, "access", &real_wallet, &real_wallet, &nonce, Some(expires_at), None, None, None);
        let prefixed = crate::license::personal_sign_hash(&msg);

        use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature};
        let (sig, rec_id): (Signature, RecoveryId) = signing_key.sign_prehash(&prefixed).unwrap();
        let v = rec_id.to_byte() + 27;
        let sig_bytes: Vec<u8> = sig.to_bytes().iter().copied().chain(std::iter::once(v)).collect();
        let sig_hex = format!("0x{}", hex::encode(&sig_bytes));

        let fake_wallet = "0x0000000000000000000000000000000000000099";
        let session = Session {
            app_id:                "app".into(),
            token_id:              1,
            identity:              "access".into(),
            user_id:               fake_wallet.into(),
            tba:                   None,
            wallet:                fake_wallet.into(), // wrong
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
    fn verify_local_tampered_identity_fails() {
        // Sign a valid access-model session, then flip `identity` to "account"
        // without re-signing. Verification must fail because the tampered
        // identity string changes the preimage.
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let signing_key  = SigningKey::random(&mut OsRng);
        let wallet       = crate::license::public_key_to_address(signing_key.verifying_key());

        let nonce      = new_nonce();
        let expires_at = "2099-01-01T00:00:00Z";
        let msg = session_message("app", 1, "access", &wallet, &wallet, &nonce, Some(expires_at), None, None, None);
        let prefixed = crate::license::personal_sign_hash(&msg);

        use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature};
        let (sig, rec_id): (Signature, RecoveryId) = signing_key.sign_prehash(&prefixed).unwrap();
        let v = rec_id.to_byte() + 27;
        let sig_bytes: Vec<u8> = sig.to_bytes().iter().copied().chain(std::iter::once(v)).collect();

        let session = Session {
            app_id:                "app".into(),
            token_id:              1,
            identity:              "account".into(),          // tampered
            user_id:               wallet.clone(),            // keep matching
            tba:                   None,
            wallet:                wallet.clone(),
            nonce,
            issued_at:             chrono::Utc::now().to_rfc3339(),
            expires_at:            Some(expires_at.into()),
            signature:             format!("0x{}", hex::encode(&sig_bytes)),
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

    // ── verify_onchain — pre-flight error paths (no network needed) ──────────

    #[cfg(feature = "cooldown")]
    #[test]
    fn verify_onchain_missing_tx_hash() {
        let s = make_session(Some("2099-01-01T00:00:00Z"));
        let err = verify_onchain(&s, "https://invalid.example").unwrap_err();
        assert!(matches!(err, VerifyError::MissingTxHash));
    }

    #[cfg(feature = "cooldown")]
    #[test]
    fn verify_onchain_missing_block_hash() {
        let mut s = make_session(Some("2099-01-01T00:00:00Z"));
        s.activation_tx = Some(
            "0x0000000000000000000000000000000000000000000000000000000000000001".into(),
        );
        let err = verify_onchain(&s, "https://invalid.example").unwrap_err();
        assert!(matches!(err, VerifyError::MissingBlockHash));
    }

    #[cfg(feature = "cooldown")]
    #[test]
    fn verify_onchain_bad_rpc_url_returns_rpc_error() {
        // Has all required fields but the URL is unreachable → Rpc(..) variant.
        let mut s = make_session(Some("2099-01-01T00:00:00Z"));
        s.activation_tx = Some(
            "0x0000000000000000000000000000000000000000000000000000000000000001".into(),
        );
        s.activation_block_hash = Some(
            "0x0000000000000000000000000000000000000000000000000000000000000002".into(),
        );
        let err = verify_onchain(&s, "not-a-url").unwrap_err();
        assert!(matches!(err, VerifyError::Rpc(_)));
    }

    #[cfg(feature = "cooldown")]
    #[test]
    fn should_reverify_is_not_constant() {
        // Probabilistic test — over many samples the result should not always be
        // the same. With p=0.2 the odds of all-true or all-false across 200 tries
        // is ~4e-20, so flakes are effectively impossible.
        let mut saw_true  = false;
        let mut saw_false = false;
        for _ in 0..200 {
            if should_reverify() { saw_true  = true; }
            else                 { saw_false = true; }
            if saw_true && saw_false { break; }
        }
        assert!(saw_true  && saw_false, "should_reverify() appears non-random");
    }
}
