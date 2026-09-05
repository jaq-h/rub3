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

/// Scans `~/.rub3/sessions/<app_id>/` for all valid, non-expired sessions
/// **signed against the deploy `(chain_id, contract)`** and returns the most
/// recently issued one.
///
/// Solves the "don't know token_id at startup" problem: the fast path doesn't
/// need to know which token to load - it just asks for the best available
/// session. The deploy narrows the scan rather than filtering its result, for
/// the reason [`load_latest_session_for_deploy`] gives: a record for another
/// chain or another contract under the same `app_id` is not a record this
/// build may launch from, and choosing it and then rejecting it would send the
/// caller back on-chain while its own usable record sits one file over.
pub fn load_latest_session(
    app_id: &str,
    chain_id: u64,
    contract: &str,
) -> Result<Session, StoreError> {
    latest_session_where(app_id, |s| {
        !is_expired(s) && is_for_deploy(s, chain_id, contract)
    })
}

/// Whether `session` was signed against the deploy `(chain_id, contract)`.
///
/// Both fields are in the signed preimage, so once the signature has verified
/// this is the record's own statement of the deploy it belongs to and not a
/// label a tamperer could have rewritten.
pub fn is_for_deploy(session: &Session, chain_id: u64, contract: &str) -> bool {
    session.chain_id == chain_id && session.contract.eq_ignore_ascii_case(contract)
}

/// The most recently issued session **signed by `wallet` against the deploy
/// `(chain_id, contract)`**, expired or not.
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
/// **All three narrow the scan rather than filtering its result**, for the
/// same reason [`load_latest_session_for_wallet`]'s wallet does: rejecting the
/// newest record after choosing it would report "nothing to release" while a
/// seat this machine really holds stays taken for the rest of its TTL. Each
/// names a way the newest record can be somebody else's. A §2.4 successor
/// migration keeps the packed `app_id`, so the directory can hold a newer
/// record for another contract; the factory deploys with `CREATE`, so the same
/// contract address can exist on another chain with its own session ids; and
/// a second agent on the same machine writes records under its own key.
///
/// The signature is still checked: a record this machine did not write is not
/// evidence of a seat it took.
pub fn load_latest_session_for_deploy(
    app_id: &str,
    chain_id: u64,
    contract: &str,
    wallet: &str,
) -> Result<Session, StoreError> {
    latest_session_where(app_id, |s| {
        is_for_deploy(s, chain_id, contract) && s.wallet.eq_ignore_ascii_case(wallet)
    })
}

/// The most recently issued valid session **signed by `wallet` against the
/// deploy `(chain_id, contract)`**.
///
/// One machine can hold sessions for several keys under the same `app_id` (a
/// human activated interactively, a second agent runs with its own key), and
/// for several deploys (a testnet build and the production build share an
/// `app_id`). Each has to narrow the scan rather than filter its result:
/// rejecting the single newest session because it belongs to another key or
/// another deploy would send a caller back on-chain while its own cached
/// session sits unused one file over.
///
/// `wallet` is compared case-insensitively against the session's signed
/// `wallet` field, which the scan's signature check has already tied to the
/// signature.
pub fn load_latest_session_for_wallet(
    app_id: &str,
    chain_id: u64,
    contract: &str,
    wallet: &str,
) -> Result<Session, StoreError> {
    latest_session_where(app_id, |s| {
        !is_expired(s)
            && is_for_deploy(s, chain_id, contract)
            && s.wallet.eq_ignore_ascii_case(wallet)
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

/// How many session records sit under `app_id`, whatever their state.
///
/// The teardown path (§3.4) needs it to tell "this machine kept no record" from
/// "it kept records and none of them is one it may release from": the scan
/// above answers both with [`StoreError::NotFound`], and the second is the
/// state that leaves a seat held for the rest of the contract's TTL with
/// nothing on this machine able to hand it back. Counts files rather than
/// parsing them, because the record that most needs reporting is the one too
/// damaged to parse.
///
/// A directory that cannot be read holds nothing this machine can act on, which
/// is the same answer as an empty one.
pub fn stored_record_count(app_id: &str) -> usize {
    let Ok(root) = sessions_root() else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(root.join(app_id)) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .count()
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

    /// The chain every record here is signed against.
    const TEST_CHAIN_ID: u64 = 8453;

    fn signed_session(app_id: &str, token_id: u64, expires_at: &str) -> Session {
        signed_session_on(app_id, token_id, expires_at, TEST_CONTRACT)
    }

    /// A record signed against a named contract on the test chain.
    fn signed_session_on(app_id: &str, token_id: u64, expires_at: &str, contract: &str) -> Session {
        signed_session_for(app_id, token_id, expires_at, TEST_CHAIN_ID, contract)
    }

    /// A record signed against a named deploy, under a fresh key.
    ///
    /// `chain_id` and `contract` are in the preimage, so a record for another
    /// deploy has to be signed for it rather than rewritten afterwards -
    /// rewriting either is exactly the tamper the signature catches.
    fn signed_session_for(
        app_id: &str,
        token_id: u64,
        expires_at: &str,
        chain_id: u64,
        contract: &str,
    ) -> Session {
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let signing_key = SigningKey::random(&mut OsRng);
        let wallet = crate::license::public_key_to_address(signing_key.verifying_key());
        let nonce = new_nonce();
        let identity = "access";
        let user_id = wallet.clone();
        let msg = session_message(
            app_id,
            chain_id,
            contract,
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
            chain_id,
            contract: contract.into(),
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

        let latest = load_latest_session("com.rub3.test", TEST_CHAIN_ID, TEST_CONTRACT).unwrap();
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

        let err = load_latest_session("com.rub3.test", TEST_CHAIN_ID, TEST_CONTRACT).unwrap_err();
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
            load_latest_session("com.rub3.test", TEST_CHAIN_ID, TEST_CONTRACT),
            Err(StoreError::NotFound)
        ));
        let found = own_latest("com.rub3.test", &expired.wallet)
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
            own_latest("com.rub3.test", &tampered.wallet),
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

        // `issued_at` is not in the signed preimage, so moving it leaves both
        // records verifying as the ones their own keys wrote. `contract` is,
        // which is why the foreign record is signed for its own deploy rather
        // than rewritten to name it.
        let mut ours = signed_session("com.rub3.test", 9, "2000-01-01T00:00:00Z");
        ours.issued_at = "2020-01-01T00:00:00Z".into();
        let mut foreign =
            signed_session_on("com.rub3.test", 5, "2000-01-01T00:00:00Z", OTHER_CONTRACT);
        foreign.issued_at = "2030-01-01T00:00:00Z".into();
        save_session(&ours).unwrap();
        save_session(&foreign).unwrap();

        let found = own_latest("com.rub3.test", &ours.wallet)
            .expect("this contract's own record is still nameable");
        assert_eq!(
            found.token_id, 9,
            "a newer record for another deploy must not be chosen and then rejected",
        );

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// **The same contract on another chain is another deploy.** The factory
    /// deploys with `CREATE`, so one deploy sequence lands the licence at the
    /// same address on Base Sepolia and on Base, and session ids start at 1 on
    /// each. A record signed for the other chain names a stranger's session
    /// here, so the scan must never choose it - and, since `chain_id` is
    /// signed, it cannot be repointed at this chain either.
    #[test]
    fn the_teardown_scan_refuses_a_record_signed_for_another_chain() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let elsewhere = signed_session_for(
            "com.rub3.test",
            4,
            "2000-01-01T00:00:00Z",
            TEST_CHAIN_ID + 1,
            TEST_CONTRACT,
        );
        save_session(&elsewhere).unwrap();
        assert!(matches!(
            own_latest("com.rub3.test", &elsewhere.wallet),
            Err(StoreError::NotFound)
        ));

        let mut repointed = elsewhere.clone();
        repointed.chain_id = TEST_CHAIN_ID;
        save_session(&repointed).unwrap();
        assert!(
            matches!(
                own_latest("com.rub3.test", &repointed.wallet),
                Err(StoreError::NotFound)
            ),
            "a chain id rewritten after signing no longer verifies",
        );

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// **A newer record under another key must not hide this key's seat.** One
    /// machine can run two agents with two keys against one contract, and the
    /// launch path already lets each reuse its own cache. Choosing the newest
    /// record and then refusing it as another key's would leave the older
    /// agent unable to release the seat its own record names.
    #[test]
    fn the_teardown_scan_chooses_the_newest_record_for_this_wallet() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let mut ours = signed_session("com.rub3.test", 1, "2000-01-01T00:00:00Z");
        ours.issued_at = "2020-01-01T00:00:00Z".into();
        let mut theirs = signed_session("com.rub3.test", 2, "2000-01-01T00:00:00Z");
        theirs.issued_at = "2030-01-01T00:00:00Z".into();
        assert_ne!(ours.wallet, theirs.wallet, "two agents, two keys");
        save_session(&ours).unwrap();
        save_session(&theirs).unwrap();

        let found = own_latest("com.rub3.test", &ours.wallet)
            .expect("this key's own record is still nameable");
        assert_eq!(
            found.token_id, 1,
            "the newer record belongs to the other agent"
        );
        let found =
            own_latest("com.rub3.test", &theirs.wallet).expect("and the other agent's is its own");
        assert_eq!(found.token_id, 2);

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// **The launch scan narrows on the deploy too.** A testnet build and the
    /// production build share an `app_id`, so a valid record signed for the
    /// other chain, or for another contract on this one, can be the newest
    /// under it. It must never be served, and it must not hide this deploy's
    /// own valid record behind it.
    #[test]
    fn the_launch_scan_serves_only_this_deploys_records() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let mut ours = signed_session("com.rub3.test", 1, "2099-01-01T00:00:00Z");
        ours.issued_at = "2020-01-01T00:00:00Z".into();
        let mut other_chain = signed_session_for(
            "com.rub3.test",
            2,
            "2099-01-01T00:00:00Z",
            TEST_CHAIN_ID + 1,
            TEST_CONTRACT,
        );
        other_chain.issued_at = "2030-01-01T00:00:00Z".into();
        let mut other_contract =
            signed_session_on("com.rub3.test", 3, "2099-01-01T00:00:00Z", OTHER_CONTRACT);
        other_contract.issued_at = "2031-01-01T00:00:00Z".into();
        save_session(&other_chain).unwrap();
        save_session(&other_contract).unwrap();

        assert!(
            matches!(
                load_latest_session("com.rub3.test", TEST_CHAIN_ID, TEST_CONTRACT),
                Err(StoreError::NotFound)
            ),
            "a valid record for another deploy is not a record for this one",
        );

        save_session(&ours).unwrap();
        let found = load_latest_session("com.rub3.test", TEST_CHAIN_ID, TEST_CONTRACT)
            .expect("this deploy's own record is served");
        assert_eq!(
            found.token_id, 1,
            "newer records for other deploys do not hide it"
        );
        let found = load_latest_session_for_wallet(
            "com.rub3.test",
            TEST_CHAIN_ID,
            TEST_CONTRACT,
            &ours.wallet,
        )
        .expect("and the wallet-scoped scan agrees");
        assert_eq!(found.token_id, 1);

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// The teardown scan as the release path calls it: this key's newest
    /// record on the test deploy.
    fn own_latest(app_id: &str, wallet: &str) -> Result<Session, StoreError> {
        load_latest_session_for_deploy(app_id, TEST_CHAIN_ID, TEST_CONTRACT, wallet)
    }
}
