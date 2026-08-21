//! Session persistence at `~/.rub3/sessions/<app_id>/<token_id>.json`.
//!
//! Env override: `RUB3_SESSION_DIR` replaces `~/.rub3/sessions` - used by
//! integration tests to point at a tmpdir.

use std::path::PathBuf;

use crate::session::{is_expired, verify_signature, Session};

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
            StoreError::NotFound => write!(f, "session not found"),
            StoreError::Io(e) => write!(f, "io error: {e}"),
            StoreError::Serde(e) => write!(f, "json error: {e}"),
            StoreError::NoDataDir => write!(f, "no home directory available"),
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
    Ok(sessions_root()?
        .join(app_id)
        .join(format!("{token_id}.json")))
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

/// Removes the cached session for `token_id`, if there is one.
///
/// Succeeds when there was nothing to remove: the caller wants the session
/// gone, and it already is. Used by the seat teardown path (§3.4), where a
/// record left behind would be launched from on the next run after the seat it
/// names has been handed back.
pub fn delete_session(app_id: &str, token_id: u64) -> Result<(), StoreError> {
    let path = session_path(app_id, token_id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StoreError::Io(e)),
    }
}

// ── Latest-session scan ───────────────────────────────────────────────────────

/// Scans `~/.rub3/sessions/<app_id>/` for all valid, non-expired sessions and
/// returns the most recently issued one.
///
/// Solves the "don't know token_id at startup" problem: the fast path doesn't
/// need to know which token to load - it just asks for the best available session.
pub fn load_latest_session(app_id: &str) -> Result<Session, StoreError> {
    latest_session_where(app_id, |s| !is_expired(s))
}

/// The most recently issued session **written against `contract`**, expired or
/// not.
///
/// For the seat teardown path (§3.4), which is asking a different question
/// from every other caller here: not "may this session launch" but "which seat
/// did this machine take". A session whose local expiry has passed still holds
/// its seat until the contract's own `sessionTtlSeconds` runs out, and on any
/// build whose packed TTL is the shorter one that is the ordinary state of an
/// instance being retired - the case `--release-seat` exists for. Filtering it
/// out here would leave the seat held for the rest of the contract's TTL with
/// nothing left on disk naming it.
///
/// **The contract narrows the scan rather than filtering its result**, for the
/// same reason [`load_latest_session_for_wallet`]'s wallet does: a §2.4
/// successor migration keeps the packed `app_id`, so this directory can hold a
/// newer record written against another deploy, and rejecting the newest record
/// after choosing it would report "nothing to release" while a seat this
/// machine really holds stays taken for the rest of its TTL.
///
/// The signature is still checked: a record this machine did not write is not
/// evidence of a seat it took.
pub fn load_latest_session_for_contract(
    app_id: &str,
    contract: &str,
) -> Result<Session, StoreError> {
    latest_session_where(app_id, |s| s.contract.eq_ignore_ascii_case(contract))
}

/// The most recently issued valid session **signed by `wallet`**.
///
/// One machine can hold sessions for several keys under the same `app_id` (a
/// human activated interactively, a second agent runs with its own key). The
/// wallet has to narrow the scan rather than filter its result: rejecting the
/// single newest session because it belongs to another key would send a caller
/// back on-chain while its own cached session sits unused one file over.
///
/// `wallet` is compared case-insensitively against the session's signed
/// `wallet` field, which the scan's signature check has already tied to the
/// signature.
pub fn load_latest_session_for_wallet(app_id: &str, wallet: &str) -> Result<Session, StoreError> {
    latest_session_where(app_id, |s| {
        !is_expired(s) && s.wallet.eq_ignore_ascii_case(wallet)
    })
}

/// Shared scan: every locally valid session for `app_id` that `keep` accepts,
/// newest first. Expiry is `keep`'s to decide, since the teardown path wants a
/// session the launch paths would refuse.
fn latest_session_where(
    app_id: &str,
    keep: impl Fn(&Session) -> bool,
) -> Result<Session, StoreError> {
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
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str::<Session>(&s).ok())
        .filter(|s| verify_signature(s).is_ok())
        .filter(|s| keep(s))
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

    /// The contract `signed_session` writes into every record it builds.
    const TEST_CONTRACT: &str = "0x0000000000000000000000000000000000000002";
    /// A second deploy under the same `app_id`, as a §2.4 successor migration
    /// leaves behind.
    const OTHER_CONTRACT: &str = "0x0000000000000000000000000000000000000003";

    fn signed_session(app_id: &str, token_id: u64, expires_at: &str) -> Session {
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let signing_key = SigningKey::random(&mut OsRng);
        let wallet = crate::license::public_key_to_address(signing_key.verifying_key());
        let nonce = new_nonce();
        let identity = "access";
        let user_id = wallet.clone();
        let msg = session_message(
            app_id,
            token_id,
            identity,
            &user_id,
            &wallet,
            &nonce,
            Some(expires_at),
            None,
            None,
            None,
        );
        let prefixed = crate::license::personal_sign_hash(&msg);

        use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature};
        let (sig, rec_id): (Signature, RecoveryId) = signing_key.sign_prehash(&prefixed).unwrap();
        let v = rec_id.to_byte() + 27;
        let sig_bytes: Vec<u8> = sig
            .to_bytes()
            .iter()
            .copied()
            .chain(std::iter::once(v))
            .collect();

        Session {
            app_id: app_id.into(),
            token_id,
            identity: identity.into(),
            user_id,
            tba: None,
            wallet,
            nonce,
            issued_at: chrono::Utc::now().to_rfc3339(),
            expires_at: Some(expires_at.into()),
            signature: format!("0x{}", hex::encode(&sig_bytes)),
            chain: "base".into(),
            contract: "0x0000000000000000000000000000000000000002".into(),
            activation_tx: None,
            activation_block: None,
            activation_block_hash: None,
            session_id: None,
            device_pubkey: None,
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let session = signed_session("com.rub3.test", 1, "2099-01-01T00:00:00Z");
        save_session(&session).unwrap();

        let loaded = load_session("com.rub3.test", 1).unwrap();
        assert_eq!(loaded.token_id, session.token_id);
        assert_eq!(loaded.wallet, session.wallet);
        assert_eq!(loaded.nonce, session.nonce);

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    #[test]
    fn load_session_not_found() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let err = load_session("com.rub3.test", 999).unwrap_err();
        assert!(matches!(err, StoreError::NotFound));

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    #[test]
    fn load_latest_returns_most_recent_valid() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        // Two tokens, one expired.
        let valid = signed_session("com.rub3.test", 1, "2099-01-01T00:00:00Z");
        let expired = signed_session("com.rub3.test", 2, "2000-01-01T00:00:00Z");
        save_session(&valid).unwrap();
        save_session(&expired).unwrap();

        let latest = load_latest_session("com.rub3.test").unwrap();
        assert_eq!(latest.token_id, 1, "should return the non-expired session");

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    #[test]
    fn load_latest_not_found_when_all_expired() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let expired = signed_session("com.rub3.test", 3, "2000-01-01T00:00:00Z");
        save_session(&expired).unwrap();

        let err = load_latest_session("com.rub3.test").unwrap_err();
        assert!(matches!(err, StoreError::NotFound));

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// **The seat teardown path has to see the session the launch paths hide.**
    /// A locally expired session still names the seat this machine took, and on
    /// a build whose packed TTL is shorter than the contract's that is the
    /// state every retiring instance is in. A scan that skips it leaves the
    /// seat held with nothing on disk naming it.
    #[test]
    fn the_expiry_agnostic_scan_finds_a_session_the_launch_scan_refuses() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let expired = signed_session("com.rub3.test", 7, "2000-01-01T00:00:00Z");
        save_session(&expired).unwrap();

        assert!(matches!(
            load_latest_session("com.rub3.test"),
            Err(StoreError::NotFound)
        ));
        let found = load_latest_session_for_contract("com.rub3.test", TEST_CONTRACT)
            .expect("the seat this machine took is still nameable");
        assert_eq!(found.token_id, 7);

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// A record this machine did not write is not evidence of a seat it took,
    /// expiry or no expiry.
    #[test]
    fn the_expiry_agnostic_scan_still_refuses_an_unverifiable_record() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let mut tampered = signed_session("com.rub3.test", 8, "2000-01-01T00:00:00Z");
        tampered.token_id = 9;
        save_session(&tampered).unwrap();

        assert!(matches!(
            load_latest_session_for_contract("com.rub3.test", TEST_CONTRACT),
            Err(StoreError::NotFound)
        ));

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// **A newer record for another deploy must not hide this one's seat.** A
    /// §2.4 successor migration keeps the `app_id`, so the newest record under
    /// it can belong to a contract this build is not pointed at. Choosing it
    /// and then rejecting it would report "nothing to release" while a seat
    /// this machine really holds stays taken for the rest of its TTL.
    #[test]
    fn the_teardown_scan_chooses_the_newest_record_for_this_contract() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        // Neither `contract` nor `issued_at` is in the signed preimage, so both
        // records still verify as the ones their own keys wrote.
        let mut ours = signed_session("com.rub3.test", 9, "2000-01-01T00:00:00Z");
        ours.issued_at = "2020-01-01T00:00:00Z".into();
        let mut foreign = signed_session("com.rub3.test", 5, "2000-01-01T00:00:00Z");
        foreign.contract = OTHER_CONTRACT.into();
        foreign.issued_at = "2030-01-01T00:00:00Z".into();
        save_session(&ours).unwrap();
        save_session(&foreign).unwrap();

        let found = load_latest_session_for_contract("com.rub3.test", TEST_CONTRACT)
            .expect("this contract's own record is still nameable");
        assert_eq!(
            found.token_id, 9,
            "a newer record for another deploy must not be chosen and then rejected",
        );

        std::env::remove_var("RUB3_SESSION_DIR");
    }
}
