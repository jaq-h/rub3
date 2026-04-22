//! Session persistence at `~/.rub3/sessions/<app_id>/<token_id>.json`.
//!
//! Env override: `RUB3_SESSION_DIR` replaces `~/.rub3/sessions` — used by
//! integration tests to point at a tmpdir.

use std::path::PathBuf;

use crate::session::{is_expired, verify_local, Session};

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    Io(std::io::Error),
    Serde(serde_json::Error),
    NoDataDir,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound   => write!(f, "session not found"),
            StoreError::Io(e)      => write!(f, "io error: {e}"),
            StoreError::Serde(e)   => write!(f, "json error: {e}"),
            StoreError::NoDataDir  => write!(f, "no home directory available"),
        }
    }
}

// ── Path resolution ───────────────────────────────────────────────────────────

fn sessions_root() -> Result<PathBuf, StoreError> {
    if let Ok(dir) = std::env::var("RUB3_SESSION_DIR") {
        return Ok(PathBuf::from(dir));
    }
    dirs::home_dir()
        .ok_or(StoreError::NoDataDir)
        .map(|h| h.join(".rub3").join("sessions"))
}

/// Resolves the session file path for `app_id` + `token_id`.
pub fn session_path(app_id: &str, token_id: u64) -> Result<PathBuf, StoreError> {
    Ok(sessions_root()?.join(app_id).join(format!("{token_id}.json")))
}

// ── Load / save ───────────────────────────────────────────────────────────────

pub fn load_session(app_id: &str, token_id: u64) -> Result<Session, StoreError> {
    let path = session_path(app_id, token_id)?;
    let data = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StoreError::NotFound
        } else {
            StoreError::Io(e)
        }
    })?;
    serde_json::from_str(&data).map_err(StoreError::Serde)
}

pub fn save_session(session: &Session) -> Result<(), StoreError> {
    let path = session_path(&session.app_id, session.token_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(StoreError::Io)?;
    }
    let json = serde_json::to_string_pretty(session).map_err(StoreError::Serde)?;
    std::fs::write(&path, json).map_err(StoreError::Io)
}

// ── Latest-session scan ───────────────────────────────────────────────────────

/// Scans `~/.rub3/sessions/<app_id>/` for all valid, non-expired sessions and
/// returns the most recently issued one.
///
/// Solves the "don't know token_id at startup" problem: the fast path doesn't
/// need to know which token to load — it just asks for the best available session.
pub fn load_latest_session(app_id: &str) -> Result<Session, StoreError> {
    let dir = sessions_root()?.join(app_id);

    let entries = std::fs::read_dir(&dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            StoreError::NotFound
        } else {
            StoreError::Io(e)
        }
    })?;

    let mut sessions: Vec<Session> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str::<Session>(&s).ok())
        .filter(|s| !is_expired(s) && verify_local(s).is_ok())
        .collect();

    if sessions.is_empty() {
        return Err(StoreError::NotFound);
    }

    // Most-recently issued session wins.
    sessions.sort_by(|a, b| b.issued_at.cmp(&a.issued_at));
    Ok(sessions.into_iter().next().unwrap())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{new_nonce, session_message, Session};
    use std::sync::Mutex;

    // Tests that mutate RUB3_SESSION_DIR must not run concurrently.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn signed_session(app_id: &str, token_id: u64, expires_at: &str) -> Session {
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let signing_key   = SigningKey::random(&mut OsRng);
        let wallet        = crate::license::public_key_to_address(signing_key.verifying_key());
        let nonce         = new_nonce();
        let identity      = "access";
        let user_id       = wallet.clone();
        let msg           = session_message(app_id, token_id, identity, &user_id, &wallet, &nonce, Some(expires_at), None, None, None);
        let prefixed      = crate::license::personal_sign_hash(&msg);

        use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature};
        let (sig, rec_id): (Signature, RecoveryId) = signing_key.sign_prehash(&prefixed).unwrap();
        let v = rec_id.to_byte() + 27;
        let sig_bytes: Vec<u8> = sig.to_bytes().iter().copied().chain(std::iter::once(v)).collect();

        Session {
            app_id:                app_id.into(),
            token_id,
            identity:              identity.into(),
            user_id,
            tba:                   None,
            wallet,
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
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let session = signed_session("com.rub3.test", 1, "2099-01-01T00:00:00Z");
        save_session(&session).unwrap();

        let loaded = load_session("com.rub3.test", 1).unwrap();
        assert_eq!(loaded.token_id, session.token_id);
        assert_eq!(loaded.wallet,   session.wallet);
        assert_eq!(loaded.nonce,    session.nonce);

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    #[test]
    fn load_session_not_found() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let err = load_session("com.rub3.test", 999).unwrap_err();
        assert!(matches!(err, StoreError::NotFound));

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    #[test]
    fn load_latest_returns_most_recent_valid() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        // Two tokens, one expired.
        let valid   = signed_session("com.rub3.test", 1, "2099-01-01T00:00:00Z");
        let expired = signed_session("com.rub3.test", 2, "2000-01-01T00:00:00Z");
        save_session(&valid).unwrap();
        save_session(&expired).unwrap();

        let latest = load_latest_session("com.rub3.test").unwrap();
        assert_eq!(latest.token_id, 1, "should return the non-expired session");

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    #[test]
    fn load_latest_not_found_when_all_expired() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let expired = signed_session("com.rub3.test", 3, "2000-01-01T00:00:00Z");
        save_session(&expired).unwrap();

        let err = load_latest_session("com.rub3.test").unwrap_err();
        assert!(matches!(err, StoreError::NotFound));

        std::env::remove_var("RUB3_SESSION_DIR");
    }
}
