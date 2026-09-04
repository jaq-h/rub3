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
/// stable identity key the app sees: wallet address for access model, TBA
/// address for account model. `tba` is populated only for the account model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub app_id: String,
    pub token_id: u64,

    // ── Identity ─────────────────────────────────────────────────────────────
    pub identity: String,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tba: Option<String>,

    pub wallet: String,

    pub nonce: String,
    pub issued_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,

    pub signature: String,
    pub chain: String,
    /// EIP-155 chain id of the deploy this session was issued against.
    ///
    /// Signed, unlike `chain`, which is the network family's display name.
    /// A record names the chain and the contract it belongs to, and the
    /// signature covers both, so neither can be repointed at another deploy
    /// after the fact.
    pub chain_id: u64,
    pub contract: String,

    // ── tier 3+ ──────────────────────────────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_tx: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_block: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_block_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u64>,

    // ── tier 4 ───────────────────────────────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_pubkey: Option<String>,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum VerifyError {
    InvalidSignature(String),
    AddressMismatch {
        expected: String,
        recovered: String,
    },
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
    ContractMismatch {
        expected: String,
        got: String,
    },
    #[cfg(feature = "cooldown")]
    BlockHashMismatch {
        expected: String,
        got: String,
    },
    #[cfg(feature = "cooldown")]
    SeatNotHeld {
        token_id: u64,
        session_id: u64,
    },
    #[cfg(feature = "cooldown")]
    UnreadableContract(String),
    #[cfg(feature = "cooldown")]
    SeatUnreadable(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::InvalidSignature(e) => write!(f, "invalid signature: {e}"),
            VerifyError::AddressMismatch {
                expected,
                recovered,
            } => write!(
                f,
                "address mismatch: session claims {expected}, signature recovers {recovered}"
            ),
            VerifyError::Expired => write!(f, "session expired"),
            #[cfg(feature = "cooldown")]
            VerifyError::MissingTxHash => {
                write!(
                    f,
                    "session is missing activation_tx (required for tier-3 re-verify)"
                )
            }
            #[cfg(feature = "cooldown")]
            VerifyError::MissingBlockHash => {
                write!(
                    f,
                    "session is missing activation_block_hash (required for tier-3 re-verify)"
                )
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
            #[cfg(feature = "cooldown")]
            VerifyError::SeatNotHeld {
                token_id,
                session_id,
            } => write!(
                f,
                "session {session_id} no longer holds a seat on token {token_id}: it was released \
                 or its seat lapsed"
            ),
            #[cfg(feature = "cooldown")]
            VerifyError::UnreadableContract(c) => {
                write!(f, "session names an unusable contract address: {c}")
            }
            #[cfg(feature = "cooldown")]
            VerifyError::SeatUnreadable(e) => {
                write!(f, "the contract did not answer sessionSeat: {e}")
            }
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
/// **`chain_id` + `contract` name the deploy the record belongs to, and the
/// signature covers them.** Every guard that asks "is this record one this
/// build may act on" compares those two fields, and the session directory is
/// user-writable: unsigned, they are the one part of a genuine record a
/// tamperer could rewrite to point a `release(tokenId, sessionId)` at another
/// deploy, where the same ids name somebody else's session. Session ids start
/// at 1 on every deploy, so the pair is what makes an id mean anything at all.
/// `contract` is hashed lower-cased so a record survives being written with
/// checksummed casing.
///
/// Tier mapping:
///   1-2: app_id, chain_id, contract, token_id, identity, user_id, wallet,
///        nonce, expires_at
///   3:   + activation_block_hash, session_id
///   4:   + device_pubkey (expires_at is None for tier 4)
//
// One parameter per preimage field is the point: the hash commits to each one
// individually, and the tier mapping above is the signature. Bundling them into
// a struct would only move the same twelve fields behind another name.
#[allow(clippy::too_many_arguments)]
pub fn session_message(
    app_id: &str,
    chain_id: u64,
    contract: &str,
    token_id: u64,
    identity: &str,
    user_id: &str,
    wallet: &str,
    nonce: &str,
    expires_at: Option<&str>,
    activation_block_hash: Option<&str>,
    session_id: Option<u64>,
    device_pubkey: Option<&str>,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(app_id.as_bytes());
    h.update(chain_id.to_be_bytes());
    h.update(contract.to_ascii_lowercase().as_bytes());
    h.update(token_id.to_be_bytes());
    h.update(identity.as_bytes());
    h.update(user_id.as_bytes());
    h.update(wallet.as_bytes());
    h.update(nonce.as_bytes());
    if let Some(exp) = expires_at {
        h.update(exp.as_bytes());
    }
    if let Some(bh) = activation_block_hash {
        h.update(bh.as_bytes());
    }
    if let Some(sid) = session_id {
        h.update(sid.to_be_bytes());
    }
    if let Some(dpk) = device_pubkey {
        h.update(dpk.as_bytes());
    }
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
    verify_signature(session)
}

/// The signature half of [`verify_local`], with no view on expiry.
///
/// For the one caller that has to reason about a session it would never
/// launch from: the seat teardown path (§3.4) needs to know whether a lapsed
/// record is one this machine wrote, because that is what makes it evidence of
/// a seat this machine took. Everything that decides whether to *run* wants
/// [`verify_local`].
pub fn verify_signature(session: &Session) -> Result<(), VerifyError> {
    let msg = session_message(
        &session.app_id,
        session.chain_id,
        &session.contract,
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
            expected: session.wallet.clone(),
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
            Err(_) => true,
        },
    }
}

// ── Tier-3 on-chain re-verification ───────────────────────────────────────────

/// Fetches the activation tx receipt and confirms it corresponds to the session:
///   1. `status == true` (tx didn't revert)
///   2. `to` matches the session's `contract`
///   3. `block_hash` matches the session's `activation_block_hash`
///   4. the session still holds a seat on its token (§3.4)
///
/// Forged sessions that carry made-up `activation_tx` / `activation_block_hash`
/// fields fail (1) or (3). Sessions pointing at a tx that hit a different
/// contract fail (2).
///
/// **(4) is the seat bound, and it is the only place a launch consults it.**
/// `activate()` admits at most `seatsPerToken` live sessions per token, but
/// nothing about that bound survives into the session file: a copy of a record
/// launches on the seat the original took. Asking the chain whether the seat is
/// still this session's is what stops a record outliving the seat that admitted
/// it - a released or lapsed seat, and every id that never held one. It does
/// not detect a copy while the original's seat is live: two instances sharing
/// one record share one seat, and only a device key (tier 4) tells them apart.
/// [`should_reverify`] samples the launches this runs on, so the refusal lands
/// on a later launch rather than the first.
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
                got: to.clone(),
            });
        }
        None => {
            return Err(VerifyError::ContractMismatch {
                expected: session.contract.clone(),
                got: "<none>".into(),
            });
        }
    }

    if !receipt.block_hash.eq_ignore_ascii_case(expected_block_hash) {
        return Err(VerifyError::BlockHashMismatch {
            expected: expected_block_hash.to_string(),
            got: receipt.block_hash.clone(),
        });
    }

    // The seat bound. Reached only for a session that went through `activate()`
    // - a tier-1/2 record has no session id, took no seat, and has nothing to
    // check here.
    if let Some(session_id) = session.session_id {
        let contract: alloy::primitives::Address = session
            .contract
            .parse()
            .map_err(|_| VerifyError::UnreadableContract(session.contract.clone()))?;

        // A node that cannot be reached falls open, exactly as the receipt
        // read above does: offline launches are not a licence failure. A node
        // that answers and contradicts falls closed - `Contract` here is the
        // call reverting or returning something that is not a seat, which is
        // never a session this build should launch from.
        let (live, _) = crate::rpc::session_seat(rpc_url, contract, session.token_id, session_id)
            .map_err(|e| match e {
            crate::rpc::RpcError::Transport(e) => VerifyError::Rpc(e),
            other => VerifyError::SeatUnreadable(other.to_string()),
        })?;
        if !live {
            return Err(VerifyError::SeatNotHeld {
                token_id: session.token_id,
                session_id,
            });
        }
    }

    Ok(())
}

/// Returns `true` with probability ~1/5 - the sampling gate for tier-3
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

// ── Tier-3 session drafting ───────────────────────────────────────────────────

/// Everything a confirmed `activate()` transaction determines about the session
/// that follows it - everything except the wallet signature.
///
/// Produced by [`draft_from_activation`] and consumed by both front doors: the
/// webview serialises it to JS for the signing screen, headless signs `message`
/// on the spot. Keeping one producer means the two doors can never drift into
/// signing different preimages for the same on-chain facts.
#[cfg(feature = "cooldown")]
#[derive(Debug, Clone)]
pub struct SessionDraft {
    /// Wire string for the contract's identity model: "access" | "account".
    pub identity: String,
    /// The holder address, lower-cased. The preimage commits to this exact
    /// string, so `Session.wallet` must be set from here rather than from
    /// whatever casing the caller started with.
    pub wallet: String,
    /// Stable identity key the app sees: wallet (access) or TBA (account).
    pub user_id: String,
    /// Derived token-bound account - account model only.
    pub tba: Option<String>,
    pub nonce: String,
    pub expires_at: String,
    /// Session id the contract assigned to this activation.
    pub session_id: u64,
    /// The 32-byte preimage the wallet signs.
    pub message: [u8; 32],
}

#[cfg(feature = "cooldown")]
impl SessionDraft {
    /// `message` as 0x-hex, for display and for IPC payloads.
    pub fn message_hex(&self) -> String {
        format!("0x{}", hex::encode(self.message))
    }
}

/// The session's `expires_at`: the earlier of the packed TTL and the on-chain
/// seat expiry, as RFC 3339.
///
/// The clamp is the whole of "the chain is authoritative". A session that
/// outlived its seat would keep launching after the contract had freed the seat
/// for somebody else, which is a token running more concurrent sessions than it
/// sold - so where the two clocks disagree, the shorter one wins.
#[cfg(feature = "cooldown")]
fn session_expiry(session_ttl_secs: i64, seat_expires_at: u64) -> Result<String, String> {
    let packed = chrono::Utc::now() + chrono::Duration::seconds(session_ttl_secs);
    let seat = chrono::DateTime::from_timestamp(
        i64::try_from(seat_expires_at)
            .map_err(|_| format!("contract reported an unusable seat expiry: {seat_expires_at}"))?,
        0,
    )
    .ok_or_else(|| format!("contract reported an unusable seat expiry: {seat_expires_at}"))?;

    Ok(packed.min(seat).to_rfc3339())
}

/// Reads the on-chain facts a fresh tier-3 session binds to, and assembles the
/// preimage the wallet must sign.
///
/// Given a landed `activate()` receipt, this decodes the seat that activation
/// took, resolves the contract's identity model (deriving the ERC-6551 TBA
/// locally for account-model deploys), mints a nonce, computes `expires_at`,
/// and builds the session message over all of it.
///
/// `block_hash` is the activation transaction's block hash, which binds the
/// session to a specific point on the chain - `verify_onchain` re-checks it.
///
/// `activation` comes from the transaction's own receipt rather than from a
/// state read, because under seats (§3.4) a fleet coming up puts several
/// activations in one block and a view read cannot say which was yours.
///
/// **The session never outlives the seat that admits it.** `expires_at` is the
/// earlier of the packed TTL and the seat's own on-chain expiry, so packaging
/// can shorten a session and never lengthen one past the concurrency the
/// contract sold. Getting that backwards would let a token run more sessions at
/// once than it has seats.
///
/// Errors are returned as display strings: the callers surface them to a UI or
/// wrap them in their own error type, and none of them branch on the variant.
#[cfg(feature = "cooldown")]
#[allow(clippy::too_many_arguments)]
pub fn draft_from_activation(
    rpc_url: &str,
    contract: alloy::primitives::Address,
    chain_id: u64,
    app_id: &str,
    token_id: u64,
    wallet: alloy::primitives::Address,
    block_hash: &str,
    activation: crate::rpc::ActivationRecord,
    session_ttl_secs: i64,
) -> Result<SessionDraft, String> {
    let session_id = activation.session_id;

    // Identity model + TBA derivation. The TBA is pure CREATE2 - no RPC beyond
    // reading the implementation address the contract was deployed with.
    let model_u8 = crate::rpc::identity_model(rpc_url, contract)
        .map_err(|e| format!("failed to read identityModel: {e}"))?;
    let model = crate::identity::IdentityModel::from_u8(model_u8)
        .ok_or_else(|| format!("contract returned unknown identityModel = {model_u8}"))?;

    let tba = match model {
        crate::identity::IdentityModel::Access => None,
        crate::identity::IdentityModel::Account => {
            let implementation = crate::rpc::tba_implementation(rpc_url, contract)
                .map_err(|e| format!("failed to read tbaImplementation: {e}"))?;
            Some(crate::identity::derive_tba(
                implementation,
                chain_id,
                contract,
                token_id,
            ))
        }
    };

    let user_id = crate::identity::resolve_user_id(model, wallet, tba);
    let wallet_str = crate::identity::format_addr(wallet);
    let nonce = new_nonce();
    let expires_at = session_expiry(session_ttl_secs, activation.seat_expires_at)?;

    let message = session_message(
        app_id,
        chain_id,
        &crate::identity::format_addr(contract),
        token_id,
        model.as_str(),
        &user_id,
        &wallet_str,
        &nonce,
        Some(&expires_at),
        Some(block_hash),
        Some(session_id),
        None,
    );

    Ok(SessionDraft {
        identity: model.as_str().to_string(),
        wallet: wallet_str,
        user_id,
        tba: tba.map(crate::identity::format_addr),
        nonce,
        expires_at,
        session_id,
        message,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACT: &str = "0x0000000000000000000000000000000000000002";
    const CHAIN_ID: u64 = 8453;

    fn make_session(expires_at: Option<&str>) -> Session {
        let wallet = "0x0000000000000000000000000000000000000001";
        Session {
            app_id: "com.rub3.test".into(),
            token_id: 1,
            identity: "access".into(),
            user_id: wallet.into(),
            tba: None,
            wallet: wallet.into(),
            nonce: "aabbcc".into(),
            issued_at: "2026-01-01T00:00:00Z".into(),
            expires_at: expires_at.map(String::from),
            signature: "0x00".into(),
            chain: "base".into(),
            chain_id: CHAIN_ID,
            contract: CONTRACT.into(),
            activation_tx: None,
            activation_block: None,
            activation_block_hash: None,
            session_id: None,
            device_pubkey: None,
        }
    }

    /// A session signed by a fresh key, with `wallet` and `user_id` set to that
    /// key's address.
    ///
    /// Every tamper test below is the same shape: take one of these, rewrite a
    /// single field, and assert the signature no longer recovers. Signing from
    /// the session's own fields rather than from a second hand-written argument
    /// list is what makes that shape honest - a preimage field the helper
    /// forgot would let a tamper test pass for the wrong reason.
    fn signed_session(expires_at: Option<&str>) -> (Session, k256::ecdsa::SigningKey) {
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let key = SigningKey::random(&mut OsRng);
        let wallet = crate::license::public_key_to_address(key.verifying_key());

        let mut session = make_session(expires_at);
        session.wallet = wallet.clone();
        session.user_id = wallet;
        session.nonce = new_nonce();
        sign_in_place(&mut session, &key);
        (session, key)
    }

    /// Writes the signature `session`'s own fields call for.
    fn sign_in_place(session: &mut Session, key: &k256::ecdsa::SigningKey) {
        use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature};

        let msg = session_message(
            &session.app_id,
            session.chain_id,
            &session.contract,
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
        let prefixed = crate::license::personal_sign_hash(&msg);
        let (sig, rec_id): (Signature, RecoveryId) = key.sign_prehash(&prefixed).unwrap();
        let bytes: Vec<u8> = sig
            .to_bytes()
            .iter()
            .copied()
            .chain(std::iter::once(rec_id.to_byte() + 27))
            .collect();
        session.signature = format!("0x{}", hex::encode(&bytes));
    }

    #[test]
    fn session_message_is_deterministic() {
        let a = session_message(
            "app",
            CHAIN_ID,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "nonce",
            Some("2030-01-01T00:00:00Z"),
            None,
            None,
            None,
        );
        let b = session_message(
            "app",
            CHAIN_ID,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "nonce",
            Some("2030-01-01T00:00:00Z"),
            None,
            None,
            None,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn session_message_differs_by_nonce() {
        let a = session_message(
            "app", CHAIN_ID, CONTRACT, 1, "access", "0xabc", "0xabc", "nonce1", None, None, None,
            None,
        );
        let b = session_message(
            "app", CHAIN_ID, CONTRACT, 1, "access", "0xabc", "0xabc", "nonce2", None, None, None,
            None,
        );
        assert_ne!(a, b);
    }

    /// The deploy the record belongs to is signed. Session ids start at 1 on
    /// every contract, so a record repointed at another deploy names a live
    /// session there that belongs to somebody else.
    #[test]
    fn session_message_differs_by_contract() {
        let a = session_message(
            "app",
            CHAIN_ID,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            None,
            None,
            Some(3),
            None,
        );
        let b = session_message(
            "app",
            CHAIN_ID,
            "0x0000000000000000000000000000000000000099",
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            None,
            None,
            Some(3),
            None,
        );
        assert_ne!(a, b);
    }

    /// The same contract address on two chains is two deploys, and the same
    /// factory address really does produce them.
    #[test]
    fn session_message_differs_by_chain_id() {
        let a = session_message(
            "app",
            8453,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            None,
            None,
            Some(3),
            None,
        );
        let b = session_message(
            "app",
            84532,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            None,
            None,
            Some(3),
            None,
        );
        assert_ne!(a, b);
    }

    /// Casing is not part of the identity of an address, so a record written
    /// with a checksummed contract verifies against the same signature as a
    /// lower-cased one.
    #[test]
    fn session_message_ignores_contract_casing() {
        let lower = session_message(
            "app",
            CHAIN_ID,
            "0x00000000000000000000000000000000000000ab",
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            None,
            None,
            None,
            None,
        );
        let upper = session_message(
            "app",
            CHAIN_ID,
            "0x00000000000000000000000000000000000000AB",
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            None,
            None,
            None,
            None,
        );
        assert_eq!(lower, upper);
    }

    #[test]
    fn session_message_differs_by_expires_at_presence() {
        let with_exp = session_message(
            "app",
            CHAIN_ID,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            Some("2030-01-01T00:00:00Z"),
            None,
            None,
            None,
        );
        let without_exp = session_message(
            "app", CHAIN_ID, CONTRACT, 1, "access", "0xabc", "0xabc", "n", None, None, None, None,
        );
        assert_ne!(with_exp, without_exp);
    }

    #[test]
    fn session_message_differs_by_tier3_fields() {
        let tier2 = session_message(
            "app",
            CHAIN_ID,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            Some("2030-01-01T00:00:00Z"),
            None,
            None,
            None,
        );
        let tier3 = session_message(
            "app",
            CHAIN_ID,
            CONTRACT,
            1,
            "access",
            "0xabc",
            "0xabc",
            "n",
            Some("2030-01-01T00:00:00Z"),
            Some("0xdeadbeef"),
            Some(42),
            None,
        );
        assert_ne!(tier2, tier3);
    }

    #[test]
    fn session_message_differs_by_identity() {
        // Flipping access -> account (with a different user_id) MUST change
        // the preimage, so a captured signature cannot be replayed with a
        // different identity model.
        let access = session_message(
            "app", CHAIN_ID, CONTRACT, 1, "access", "0xwallet", "0xwallet", "n", None, None, None,
            None,
        );
        let account = session_message(
            "app", CHAIN_ID, CONTRACT, 1, "account", "0xtba", "0xwallet", "n", None, None, None,
            None,
        );
        assert_ne!(access, account);
    }

    #[test]
    fn session_message_differs_by_user_id_only() {
        // Same identity string, but swapping user_id alone (e.g. pointing at
        // a different TBA) must change the preimage.
        let a = session_message(
            "app", CHAIN_ID, CONTRACT, 1, "account", "0xtba1", "0xwallet", "n", None, None, None,
            None,
        );
        let b = session_message(
            "app", CHAIN_ID, CONTRACT, 1, "account", "0xtba2", "0xwallet", "n", None, None, None,
            None,
        );
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
        assert!(
            !is_expired(&s),
            "tier 4 sessions with no expires_at should not expire"
        );
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
        let (session, _key) = signed_session(Some("2099-01-01T00:00:00Z"));
        assert!(verify_local(&session).is_ok());
    }

    #[test]
    fn verify_local_wrong_wallet_fails() {
        let (mut session, _key) = signed_session(Some("2099-01-01T00:00:00Z"));
        let fake_wallet = "0x0000000000000000000000000000000000000099";
        session.wallet = fake_wallet.into();
        session.user_id = fake_wallet.into();

        assert!(matches!(
            verify_local(&session),
            Err(VerifyError::AddressMismatch { .. })
        ));
    }

    #[test]
    fn verify_local_tampered_identity_fails() {
        // Sign a valid access-model session, then flip `identity` to "account"
        // without re-signing. Verification must fail because the tampered
        // identity string changes the preimage.
        let (mut session, _key) = signed_session(Some("2099-01-01T00:00:00Z"));
        session.identity = "account".into();

        assert!(matches!(
            verify_local(&session),
            Err(VerifyError::AddressMismatch { .. })
        ));
    }

    /// **The teardown path's guard rests on this.** `release(tokenId,
    /// sessionId)` is broadcast against whichever contract the record names,
    /// and the session directory is user-writable: repointing a genuine record
    /// at a second deploy, where the same ids name a stranger's live session,
    /// is one machine ending another's - the revocation shape §2.4 rules out.
    /// The rewritten field has to break the signature, and here it does.
    #[test]
    fn verify_local_tampered_contract_fails() {
        let (mut session, key) = signed_session(Some("2099-01-01T00:00:00Z"));
        session.session_id = Some(3);
        sign_in_place(&mut session, &key);
        assert!(verify_local(&session).is_ok(), "the record starts genuine");

        session.contract = "0x0000000000000000000000000000000000000099".into();

        assert!(matches!(
            verify_local(&session),
            Err(VerifyError::AddressMismatch { .. })
        ));
    }

    /// The same address on another chain is another deploy, so the chain id is
    /// signed for the same reason the contract is.
    #[test]
    fn verify_local_tampered_chain_id_fails() {
        let (mut session, _key) = signed_session(Some("2099-01-01T00:00:00Z"));
        session.chain_id = 84_532;

        assert!(matches!(
            verify_local(&session),
            Err(VerifyError::AddressMismatch { .. })
        ));
    }

    #[test]
    fn verify_local_expired_fails() {
        let s = make_session(Some("2000-01-01T00:00:00Z"));
        assert!(matches!(verify_local(&s), Err(VerifyError::Expired)));
    }

    // ── Seats: the session never outlives the seat that admits it (§3.4) ─────

    /// The clamp that keeps a token from running more sessions at once than it
    /// has seats. A packed TTL longer than the contract's would leave a session
    /// launching after the chain had handed its seat to somebody else.
    #[test]
    #[cfg(feature = "cooldown")]
    fn session_expiry_takes_the_seat_when_the_seat_is_shorter() {
        let seat = chrono::Utc::now().timestamp() as u64 + 60;
        let expiry = session_expiry(86_400, seat).expect("a usable expiry");
        let parsed: chrono::DateTime<chrono::Utc> = expiry.parse().expect("rfc3339");
        assert_eq!(parsed.timestamp(), seat as i64);
    }

    /// And the other direction: packaging may shorten a session. A wrapper that
    /// wants sessions to last an hour on a contract that grants a day gets an
    /// hour.
    #[test]
    #[cfg(feature = "cooldown")]
    fn session_expiry_takes_the_packed_ttl_when_it_is_shorter() {
        let now = chrono::Utc::now().timestamp();
        let seat = now as u64 + 86_400;
        let expiry = session_expiry(3_600, seat).expect("a usable expiry");
        let parsed: chrono::DateTime<chrono::Utc> = expiry.parse().expect("rfc3339");
        assert!(
            (parsed.timestamp() - (now + 3_600)).abs() <= 1,
            "expected the packed hour, got {expiry}"
        );
    }

    /// A seat expiry a `DateTime` cannot hold is a contract answering something
    /// this build cannot reason about. Reported rather than silently clamped:
    /// a clamp would be this wrapper choosing a session lifetime the chain
    /// never agreed to.
    #[test]
    #[cfg(feature = "cooldown")]
    fn session_expiry_refuses_an_unusable_seat_expiry() {
        assert!(session_expiry(3_600, u64::MAX).is_err());
    }

    // ── verify_onchain - pre-flight error paths (no network needed) ──────────

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
        s.activation_tx =
            Some("0x0000000000000000000000000000000000000000000000000000000000000001".into());
        let err = verify_onchain(&s, "https://invalid.example").unwrap_err();
        assert!(matches!(err, VerifyError::MissingBlockHash));
    }

    #[cfg(feature = "cooldown")]
    #[test]
    fn verify_onchain_bad_rpc_url_returns_rpc_error() {
        // Has all required fields but the URL is unreachable → Rpc(..) variant.
        let mut s = make_session(Some("2099-01-01T00:00:00Z"));
        s.activation_tx =
            Some("0x0000000000000000000000000000000000000000000000000000000000000001".into());
        s.activation_block_hash =
            Some("0x0000000000000000000000000000000000000000000000000000000000000002".into());
        let err = verify_onchain(&s, "not-a-url").unwrap_err();
        assert!(matches!(err, VerifyError::Rpc(_)));
    }

    #[cfg(feature = "cooldown")]
    #[test]
    fn should_reverify_is_not_constant() {
        // Probabilistic test - over many samples the result should not always be
        // the same. With p=0.2 the odds of all-true or all-false across 200 tries
        // is ~4e-20, so flakes are effectively impossible.
        let mut saw_true = false;
        let mut saw_false = false;
        for _ in 0..200 {
            if should_reverify() {
                saw_true = true;
            } else {
                saw_false = true;
            }
            if saw_true && saw_false {
                break;
            }
        }
        assert!(
            saw_true && saw_false,
            "should_reverify() appears non-random"
        );
    }
}
