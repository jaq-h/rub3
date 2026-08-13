//! Activation orchestration - the two front doors and the fast paths they share.
//!
//! There are exactly two ways a launch acquires a session:
//!
//!   * [`ensure`] - the interactive door. Opens the native activation window
//!     and waits for a human to connect a wallet, broadcast, and sign.
//!     Compiled in with the `webview` feature.
//!   * [`ensure_headless`] - the agent door. Takes a [`Signer`] and runs the
//!     same pipeline end to end with nothing to click. Compiled in with the
//!     `headless` feature.
//!
//! Both sit on top of identical machinery: `rpc` for chain reads and calldata,
//! `session` for the preimage and verification, `session_store` for
//! persistence. The webview was only ever the human-shaped piece at the top.

use alloy::primitives::Address;

use crate::{license, rpc, store};

#[cfg(feature = "webview")]
use crate::webview::{self, ActivationContext, ActivationResult};

#[cfg(feature = "headless")]
use crate::session::Session;
#[cfg(feature = "headless")]
use crate::signer::Signer;

#[derive(Debug)]
pub enum ActivationError {
    Cancelled,
    OwnershipMismatch,
    /// The build has no interactive front door: `webview` was not compiled in.
    /// Such a build can still launch from a cached session, and - when
    /// `headless` is compiled in - activate through [`ensure_headless`].
    NoInteractiveFrontDoor,
    Error(String),
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationError::Cancelled => write!(f, "activation cancelled"),
            ActivationError::OwnershipMismatch => {
                write!(f, "wallet does not own the license token on-chain")
            }
            ActivationError::NoInteractiveFrontDoor => write!(
                f,
                "no cached session and this build has no activation window \
                 (built without the `webview` feature)"
            ),
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
    if try_session_fast_path(app_id, rpc_url, None, None).is_some() {
        return Ok(());
    }

    // ── Fast path 2: existing legacy proof ───────────────────────────────────
    if try_legacy_fast_path(app_id, contract, rpc_url) {
        return Ok(());
    }

    // ── Slow path: activation window ─────────────────────────────────────────
    interactive_slow_path(app_id, contract, chain_id, rpc_url, developer_ens, session_ttl_secs)
}

#[cfg(feature = "webview")]
fn interactive_slow_path(
    app_id: &str,
    contract: &str,
    chain_id: u64,
    rpc_url: &str,
    developer_ens: Option<String>,
    session_ttl_secs: i64,
) -> Result<(), ActivationError> {
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
            store::save_proof(app_id, &proof).map_err(|e| ActivationError::Error(e.to_string()))?;
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

#[cfg(not(feature = "webview"))]
fn interactive_slow_path(
    _app_id: &str,
    _contract: &str,
    _chain_id: u64,
    _rpc_url: &str,
    _developer_ens: Option<String>,
    _session_ttl_secs: i64,
) -> Result<(), ActivationError> {
    Err(ActivationError::NoInteractiveFrontDoor)
}

// ── Fast paths ────────────────────────────────────────────────────────────────

/// Returns the cached session for `app_id` when one is valid, else `None`.
///
/// Always performs local verification (signature + expiry). On roughly 1 in 5
/// cold starts it additionally performs on-chain re-verification: fetching the
/// activation tx receipt and confirming it lines up with the session's
/// `contract` + `activation_block_hash`. This catches forged sessions that
/// carry fabricated tx hashes without paying network cost on every launch.
///
/// An on-chain check that fails with a transport error (no network, bad URL)
/// falls open - i.e. we still accept the session - so offline launches aren't
/// broken. A check that succeeds-and-contradicts (wrong contract, wrong block
/// hash, reverted tx) falls closed and forces re-activation.
///
/// `require_wallet` narrows the match to sessions signed by one specific
/// address. The interactive door passes `None` (whoever last activated on this
/// machine is the user). The headless door passes its signer's address, so an
/// agent never launches on a session belonging to a different key.
///
/// `require_token` selects one specific token's session instead of the newest
/// across all of them. The interactive door passes `None`; the headless door
/// passes `--token-id` when it was given, so an explicit token constrains cache
/// reuse exactly as it constrains purchasing. Selecting rather than filtering
/// matters once a signer holds several licenses: the requested token's session
/// is reused even when another token was activated more recently.
#[cfg(feature = "cooldown")]
fn try_session_fast_path(
    app_id: &str,
    rpc_url: &str,
    require_wallet: Option<Address>,
    require_token: Option<u64>,
) -> Option<crate::session::Session> {
    let session = match require_token {
        Some(token_id) => crate::session_store::load_session(app_id, token_id).ok()?,
        None => crate::session_store::load_latest_session(app_id).ok()?,
    };

    if crate::session::verify_local(&session).is_err() {
        return None;
    }

    if let Some(wallet) = require_wallet {
        let expected = crate::identity::format_addr(wallet);
        if !session.wallet.eq_ignore_ascii_case(&expected) {
            return None;
        }
    }

    // Re-verify probabilistically — only when the session carries the fields
    // (session_id present ⇒ tier-3 session that went through activate()).
    if session.session_id.is_some() && crate::session::should_reverify() {
        match crate::session::verify_onchain(&session, rpc_url) {
            Ok(()) => {}
            Err(crate::session::VerifyError::Rpc(_)) => {
                // Offline / transport failure: fall open, keep the session.
            }
            Err(_) => return None,
        }
    }

    Some(session)
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

// ── Exit codes ────────────────────────────────────────────────────────────────
//
// The machine-readable contract of `--headless`. Defined unconditionally so a
// build without the `headless` feature can still report
// `EXIT_HEADLESS_UNSUPPORTED` rather than a generic failure - an orchestrator
// that gets code 18 knows it picked the wrong build, not that activation broke.
//
// Reproduced in `rub3-wrapper --help` and in the README. Renumbering any of
// these is a breaking change.

/// Success - a valid session exists and the wrapped binary was launched.
pub const EXIT_OK: i32 = 0;
/// Unclassified failure.
pub const EXIT_GENERIC: i32 = 1;
// 2 is reserved: clap uses it for command-line usage errors.
/// No usable signer configured, or the configured one is malformed.
pub const EXIT_SIGNER: i32 = 10;
/// Wallet cannot cover the purchase price plus gas.
pub const EXIT_INSUFFICIENT_FUNDS: i32 = 11;
/// No token held and the contract has minted its supply cap.
pub const EXIT_SOLD_OUT: i32 = 12;
/// `activate()` is rate-limited; `blocks_remaining` says for how long.
pub const EXIT_COOLDOWN_ACTIVE: i32 = 13;
/// A transaction reverted, or did not confirm inside the poll budget.
pub const EXIT_ACTIVATION_FAILED: i32 = 14;
/// The assembled session failed local signature/expiry verification.
pub const EXIT_VERIFICATION_FAILED: i32 = 15;
/// Chain read/transport failure.
pub const EXIT_RPC: i32 = 16;
/// The session could not be written to disk.
pub const EXIT_PERSIST: i32 = 17;
/// `--headless` was passed to a build compiled without the `headless` feature.
pub const EXIT_HEADLESS_UNSUPPORTED: i32 = 18;
/// The node's chain id disagrees with the one this binary was built for.
pub const EXIT_CHAIN_MISMATCH: i32 = 19;
/// `--token-id N` names a token the signer does not hold.
pub const EXIT_TOKEN_NOT_OWNED: i32 = 20;

/// The exit-code table rendered for `--help`, so the contract is discoverable
/// from the binary itself and not only from the docs.
pub const EXIT_CODE_HELP: &str = "\
Headless exit codes (--headless). These are emitted only when headless
activation itself fails. Once the wrapped binary launches, its own exit status
is passed through unchanged, so a code in this range coming from a launched
child is the child's status and not an activation failure.

   0  success - session valid, wrapped binary launched
   1  unclassified failure
   2  command-line usage error
  10  no usable signer (set RUB3_AGENT_KEY or RUB3_AGENT_KEYSTORE)
  11  insufficient funds for purchase + gas
  12  no token held and supply is sold out
  13  cooldown active - stderr carries `blocks_remaining=N`
  14  activation failed (tx reverted, or not confirmed in time)
  15  session verification failed
  16  chain RPC / transport failure
  17  session could not be persisted
  18  headless mode not compiled into this build
  19  chain id mismatch between the RPC endpoint and this build
  20  --token-id names a token this signer does not hold

Signer sources, highest precedence first:
  RUB3_AGENT_KEY                        raw hex private key (dev / CI only)
  RUB3_AGENT_KEYSTORE                   encrypted V3 keystore file
  RUB3_AGENT_KEYSTORE_PASSWORD_FILE     password file for the keystore (preferred)
  RUB3_AGENT_KEYSTORE_PASSWORD          password, inline";

// ── Headless activation ───────────────────────────────────────────────────────

#[cfg(feature = "headless")]
pub use self::headless::{ensure_headless, HeadlessContext, HeadlessError, HeadlessOutcome};

#[cfg(feature = "headless")]
mod headless {
    use super::*;
    use alloy::primitives::U256;

    use crate::tx::{self, TxError, TxPlan};

    /// Build-time facts the headless flow needs. Mirrors the webview's
    /// `ActivationContext`; `developer_ens` is absent because ENS resolution is
    /// a human-trust affordance and is still a stub (§1.6).
    #[derive(Debug, Clone)]
    pub struct HeadlessContext {
        pub app_id: String,
        pub contract: String,
        pub chain_id: u64,
        pub rpc_url: String,
        pub session_ttl_secs: i64,
        /// Activate this specific token. `None` lets the flow choose: the
        /// lowest-numbered token the signer holds, or a freshly purchased one.
        pub token_id: Option<u64>,
    }

    /// What a successful [`ensure_headless`] actually did - an orchestrator
    /// reads this to tell "launched from cache" apart from "spent money".
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HeadlessOutcome {
        /// A cached session was still valid; nothing was sent on-chain.
        Reused,
        /// A token was already held; `activate()` minted a new session.
        Activated,
        /// A token was purchased first, then activated.
        PurchasedAndActivated { token_id: u64, price_wei: String },
    }

    /// Everything that can stop a headless activation, in a shape an
    /// orchestrator can branch on. Each variant maps to a distinct process exit
    /// code - see [`HeadlessError::exit_code`].
    #[derive(Debug)]
    pub enum HeadlessError {
        /// No usable signer: nothing configured, or what was configured is
        /// malformed. Never carries key material.
        Signer(crate::signer::SignerError),
        /// The wallet cannot cover the purchase price plus gas. `shortfall`
        /// carries the amounts only when the wrapper measured them itself; a
        /// node that rejected the transaction reports no parseable numbers.
        InsufficientFunds { shortfall: Option<crate::tx::Shortfall> },
        /// The signer holds no token and the contract has minted its cap.
        SoldOut { supply_cap: u64, minted: u64 },
        /// `activate()` is rate-limited for this token for another N blocks.
        CooldownActive { token_id: u64, blocks_remaining: u64 },
        /// A transaction reverted, or did not confirm inside the poll budget.
        ActivationFailed(String),
        /// The session was assembled but failed local verification. Signals a
        /// signer whose signatures do not recover to the address it claims.
        VerificationFailed(String),
        /// Chain read/transport failure.
        Rpc(String),
        /// The session could not be written to disk.
        Persist(String),
        /// The node's chain id disagrees with the one this binary was built
        /// for. Signing anyway would produce a transaction for another network.
        ChainIdMismatch { expected: u64, actual: u64 },
        /// `--token-id N` names a token this signer does not hold.
        TokenNotOwned { token_id: u64, wallet: String },
        /// Headless activation needs a real license contract; this build points
        /// at the zero address, or at something that is not an address at all.
        NoContract,
    }

    impl std::fmt::Display for HeadlessError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                HeadlessError::Signer(e) => write!(f, "{e}"),
                HeadlessError::InsufficientFunds { shortfall: Some(s) } => match s.covers {
                    crate::tx::Covers::PriceAndGas => write!(
                        f,
                        "insufficient funds: need {} wei (price + gas), wallet holds {} wei",
                        s.required, s.available
                    ),
                    crate::tx::Covers::PriceOnly => write!(
                        f,
                        "insufficient funds: need {} wei for the price alone, before gas, \
                         wallet holds {} wei",
                        s.required, s.available
                    ),
                },
                HeadlessError::InsufficientFunds { shortfall: None } => write!(
                    f,
                    "insufficient funds: the node rejected the transaction for lack of \
                     balance, without reporting the amounts"
                ),
                HeadlessError::SoldOut { supply_cap, minted } => write!(
                    f,
                    "sold out: {minted} of {supply_cap} tokens minted and this wallet holds none"
                ),
                HeadlessError::CooldownActive { token_id, blocks_remaining } => write!(
                    f,
                    "cooldown active on token {token_id}: retry in {blocks_remaining} blocks"
                ),
                HeadlessError::ActivationFailed(e) => write!(f, "activation failed: {e}"),
                HeadlessError::VerificationFailed(e) => {
                    write!(f, "session verification failed: {e}")
                }
                HeadlessError::Rpc(e) => write!(f, "chain error: {e}"),
                HeadlessError::Persist(e) => write!(f, "could not persist session: {e}"),
                HeadlessError::ChainIdMismatch { expected, actual } => write!(
                    f,
                    "chain id mismatch: this build targets {expected}, the RPC endpoint reports {actual}"
                ),
                HeadlessError::TokenNotOwned { token_id, wallet } => {
                    write!(f, "token {token_id} is not owned by {wallet}")
                }
                HeadlessError::NoContract => write!(
                    f,
                    "headless activation requires a usable license contract, but this build has none configured"
                ),
            }
        }
    }

    impl std::error::Error for HeadlessError {}

    impl HeadlessError {
        /// The process exit code for this failure.
        ///
        /// Stable, documented, and machine-readable - an orchestrator branches
        /// on it instead of parsing stderr. The table is reproduced in
        /// `rub3-wrapper --help` and in the README.
        pub fn exit_code(&self) -> i32 {
            match self {
                HeadlessError::Signer(_) => EXIT_SIGNER,
                HeadlessError::InsufficientFunds { .. } => EXIT_INSUFFICIENT_FUNDS,
                HeadlessError::SoldOut { .. } => EXIT_SOLD_OUT,
                HeadlessError::CooldownActive { .. } => EXIT_COOLDOWN_ACTIVE,
                HeadlessError::ActivationFailed(_) => EXIT_ACTIVATION_FAILED,
                HeadlessError::VerificationFailed(_) => EXIT_VERIFICATION_FAILED,
                HeadlessError::Rpc(_) => EXIT_RPC,
                HeadlessError::Persist(_) => EXIT_PERSIST,
                HeadlessError::ChainIdMismatch { .. } => EXIT_CHAIN_MISMATCH,
                HeadlessError::TokenNotOwned { .. } => EXIT_TOKEN_NOT_OWNED,
                HeadlessError::NoContract => EXIT_GENERIC,
            }
        }

        /// A single `key=value` line for orchestrators that want the failure's
        /// parameters without parsing prose. Emitted on stderr by the CLI.
        ///
        /// The cooldown case is the one that matters most: `blocks_remaining`
        /// tells a scheduler exactly how long to back off. A failure whose
        /// parameters are unknown emits no line at all rather than a line of
        /// placeholder zeroes.
        pub fn machine_detail(&self) -> Option<String> {
            match self {
                HeadlessError::CooldownActive { token_id, blocks_remaining } => {
                    Some(format!("token_id={token_id} blocks_remaining={blocks_remaining}"))
                }
                // Only when the amounts are real: an orchestrator that read
                // `required_wei=0` would top the wallet up by nothing.
                HeadlessError::InsufficientFunds { shortfall: Some(s) } => Some(format!(
                    "required_wei={} available_wei={} required_covers={}",
                    s.required,
                    s.available,
                    s.covers.as_str()
                )),
                HeadlessError::SoldOut { supply_cap, minted } => {
                    Some(format!("supply_cap={supply_cap} minted={minted}"))
                }
                HeadlessError::ChainIdMismatch { expected, actual } => {
                    Some(format!("expected_chain_id={expected} actual_chain_id={actual}"))
                }
                HeadlessError::TokenNotOwned { token_id, .. } => {
                    Some(format!("token_id={token_id}"))
                }
                _ => None,
            }
        }
    }

    impl From<TxError> for HeadlessError {
        fn from(e: TxError) -> Self {
            match e {
                TxError::InsufficientFunds(shortfall) => {
                    HeadlessError::InsufficientFunds { shortfall }
                }
                TxError::Signer(s) => HeadlessError::Signer(s),
                TxError::Rpc(m) => HeadlessError::Rpc(m),
                TxError::Rejected(m) => HeadlessError::ActivationFailed(m),
            }
        }
    }

    // ── The flow ─────────────────────────────────────────────────────────────

    /// The agent path: signer in, session out.
    ///
    /// Runs the whole activation pipeline with no window and no human:
    ///
    /// ```text
    /// cached session for this signer? ─yes─▶ done (Reused)
    ///            │no
    /// tokensOfOwner(signer) ─empty─▶ purchase()  ─▶ minted token id
    ///            │holds ≥1
    ///     pick the token
    ///            │
    ///     cooldownReady? ─no─▶ CooldownActive { blocks_remaining }
    ///            │yes
    ///        activate() ─▶ wait for receipt ─▶ activeSessionId + identity model
    ///            │
    ///   sign the session message locally ─▶ verify_local ─▶ persist
    /// ```
    ///
    /// Every step below the front door is the same code the webview drives -
    /// `rpc` for reads and calldata, [`crate::session::draft_from_activation`]
    /// for the preimage, [`crate::session_store`] for persistence.
    ///
    /// Returns the live session plus a [`HeadlessOutcome`] describing what it
    /// took to get there.
    pub fn ensure_headless(
        signer: &dyn Signer,
        ctx: &HeadlessContext,
    ) -> Result<(Session, HeadlessOutcome), HeadlessError> {
        let wallet = signer.address();

        // ── Fast path: a session this signer already owns ────────────────────
        if let Some(session) =
            try_session_fast_path(&ctx.app_id, &ctx.rpc_url, Some(wallet), ctx.token_id)
        {
            return Ok((session, HeadlessOutcome::Reused));
        }

        // An unparseable address and the zero address mean the same thing: this
        // build carries no usable contract. Both are build-time constants, so
        // neither is worth a retry.
        let contract: Address = ctx.contract.parse().map_err(|_| HeadlessError::NoContract)?;
        if contract.is_zero() {
            return Err(HeadlessError::NoContract);
        }

        // Signing for the wrong network is silent and expensive; catch it before
        // the first transaction rather than after.
        let node_chain_id = rpc::chain_id(&ctx.rpc_url).map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        if node_chain_id != ctx.chain_id {
            return Err(HeadlessError::ChainIdMismatch {
                expected: ctx.chain_id,
                actual: node_chain_id,
            });
        }

        // ── Token selection, purchasing if the signer holds none ─────────────
        let owned = rpc::tokens_of_owner(&ctx.rpc_url, contract, wallet)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?;

        let (token_id, outcome) = match (ctx.token_id, lowest_token(&owned)) {
            // An explicitly requested token must actually be held: buying a
            // second token because the requested one is missing would spend
            // money the caller did not ask to spend.
            (Some(requested), _) => {
                if !owned.contains(&requested) {
                    return Err(HeadlessError::TokenNotOwned {
                        token_id: requested,
                        wallet: crate::identity::format_addr(wallet),
                    });
                }
                (requested, HeadlessOutcome::Activated)
            }
            (None, Some(held)) => (held, HeadlessOutcome::Activated),
            (None, None) => {
                let (id, price) = purchase(signer, ctx, contract, wallet)?;
                (
                    id,
                    HeadlessOutcome::PurchasedAndActivated {
                        token_id: id,
                        price_wei: price.to_string(),
                    },
                )
            }
        };

        // ── Cooldown gate ────────────────────────────────────────────────────
        let (ready, blocks_remaining) = rpc::cooldown_ready(&ctx.rpc_url, contract, token_id)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        if !ready {
            return Err(HeadlessError::CooldownActive { token_id, blocks_remaining });
        }

        // ── activate() ───────────────────────────────────────────────────────
        let calldata = decode_calldata(&rpc::encode_activate_calldata(token_id))?;
        let tx_hash = tx::send(
            &ctx.rpc_url,
            signer,
            &TxPlan { to: contract, value: U256::ZERO, input: calldata },
        )?;

        let receipt = rpc::wait_for_receipt(&ctx.rpc_url, &tx_hash)
            .map_err(HeadlessError::ActivationFailed)?;
        if !receipt.status {
            return Err(HeadlessError::ActivationFailed(format!(
                "activate() reverted on-chain (tx {tx_hash})"
            )));
        }
        if let Some(to) = receipt.to.as_deref() {
            let to_addr: Address = to.parse().map_err(|_| {
                HeadlessError::ActivationFailed(format!(
                    "activate() receipt carries a malformed `to` address ({to})"
                ))
            })?;
            if to_addr != contract {
                return Err(HeadlessError::ActivationFailed(format!(
                    "activate() tx was sent to {to_addr}, expected {contract}"
                )));
            }
        }

        // ── Draft, sign, verify, persist ─────────────────────────────────────
        let draft = crate::session::draft_from_activation(
            &ctx.rpc_url,
            contract,
            ctx.chain_id,
            &ctx.app_id,
            token_id,
            wallet,
            &receipt.block_hash,
            ctx.session_ttl_secs,
        )
        .map_err(HeadlessError::Rpc)?;

        let signature = crate::signer::personal_sign(signer, &draft.message)
            .map_err(HeadlessError::Signer)?;

        let session = Session {
            app_id: ctx.app_id.clone(),
            token_id,
            identity: draft.identity,
            user_id: draft.user_id,
            tba: draft.tba,
            wallet: draft.wallet,
            nonce: draft.nonce,
            issued_at: chrono::Utc::now().to_rfc3339(),
            expires_at: Some(draft.expires_at),
            signature,
            chain: "base".to_string(),
            contract: ctx.contract.clone(),
            activation_tx: Some(tx_hash),
            activation_block: Some(receipt.block_number),
            activation_block_hash: Some(receipt.block_hash),
            session_id: Some(draft.session_id),
            device_pubkey: None,
        };

        crate::session::verify_local(&session)
            .map_err(|e| HeadlessError::VerificationFailed(e.to_string()))?;

        crate::session_store::save_session(&session)
            .map_err(|e| HeadlessError::Persist(e.to_string()))?;

        Ok((session, outcome))
    }

    /// Buys a token for `wallet` and returns `(token_id, price_wei)`.
    ///
    /// Mirrors the interactive purchase flow (§1.7) exactly - same supply
    /// check, same calldata, same `Transfer` log parse to recover the minted id
    /// - with the broadcast done by the signer instead of a human's wallet.
    fn purchase(
        signer: &dyn Signer,
        ctx: &HeadlessContext,
        contract: Address,
        wallet: Address,
    ) -> Result<(u64, U256), HeadlessError> {
        let supply_cap =
            rpc::supply_cap(&ctx.rpc_url, contract).map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        let minted = rpc::next_token_id(&ctx.rpc_url, contract)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        if supply_cap != 0 && minted >= supply_cap {
            return Err(HeadlessError::SoldOut { supply_cap, minted });
        }

        let price =
            rpc::token_price(&ctx.rpc_url, contract).map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        let calldata = decode_calldata(&rpc::encode_purchase_calldata(wallet))?;

        let tx_hash =
            tx::send(&ctx.rpc_url, signer, &TxPlan { to: contract, value: price, input: calldata })?;

        let receipt =
            rpc::wait_for_receipt(&ctx.rpc_url, &tx_hash).map_err(HeadlessError::ActivationFailed)?;
        if !receipt.status {
            return Err(HeadlessError::ActivationFailed(format!(
                "purchase() reverted on-chain (tx {tx_hash})"
            )));
        }

        let token_id = rpc::mint_token_id(&ctx.rpc_url, &tx_hash, contract, wallet)
            .map_err(|e| HeadlessError::ActivationFailed(e.to_string()))?;

        Ok((token_id, price))
    }

    /// The token an unqualified run activates: the lowest id the signer holds.
    ///
    /// `rpc::tokens_of_owner` returns OpenZeppelin's ERC721Enumerable owner
    /// array, which swap-and-pop leaves in an arbitrary order after any
    /// transfer out. Taking the minimum makes the choice depend on what the
    /// signer holds, not on its transfer history.
    pub(super) fn lowest_token(owned: &[u64]) -> Option<u64> {
        owned.iter().copied().min()
    }

    /// `rpc::encode_*_calldata` returns display hex; the transaction envelope
    /// wants bytes. The encoders are pure and always emit valid hex, so a
    /// failure here means the ABI encoder itself is broken.
    fn decode_calldata(hex_str: &str) -> Result<Vec<u8>, HeadlessError> {
        hex::decode(hex_str.trim_start_matches("0x")).map_err(|e| {
            HeadlessError::ActivationFailed(format!("internal: malformed calldata ({e})"))
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "headless"))]
mod tests {
    use super::*;
    use crate::signer::SignerError;

    /// The exit-code contract is a public API: orchestrators branch on these
    /// numbers, so a silent renumbering is a breaking change.
    #[test]
    fn exit_codes_match_the_documented_table() {
        let cases: Vec<(HeadlessError, i32)> = vec![
            (HeadlessError::Signer(SignerError::NoSource), 10),
            (
                HeadlessError::InsufficientFunds {
                    shortfall: Some(crate::tx::Shortfall {
                        required: alloy::primitives::U256::from(1u64),
                        available: alloy::primitives::U256::ZERO,
                        covers: crate::tx::Covers::PriceAndGas,
                    }),
                },
                11,
            ),
            (HeadlessError::InsufficientFunds { shortfall: None }, 11),
            (HeadlessError::SoldOut { supply_cap: 10, minted: 10 }, 12),
            (HeadlessError::CooldownActive { token_id: 3, blocks_remaining: 42 }, 13),
            (HeadlessError::ActivationFailed("reverted".into()), 14),
            (HeadlessError::VerificationFailed("bad sig".into()), 15),
            (HeadlessError::Rpc("offline".into()), 16),
            (HeadlessError::Persist("read-only fs".into()), 17),
            (HeadlessError::ChainIdMismatch { expected: 8453, actual: 31337 }, 19),
            (
                HeadlessError::TokenNotOwned { token_id: 7, wallet: "0x01".into() },
                20,
            ),
            (HeadlessError::NoContract, 1),
        ];

        for (err, expected) in cases {
            assert_eq!(err.exit_code(), expected, "wrong exit code for {err:?}");
        }
    }

    /// Every classified failure must be distinguishable from every other one,
    /// or an orchestrator cannot tell "top up the wallet" from "back off".
    #[test]
    fn classified_exit_codes_are_distinct() {
        let codes = [
            EXIT_SIGNER,
            EXIT_INSUFFICIENT_FUNDS,
            EXIT_SOLD_OUT,
            EXIT_COOLDOWN_ACTIVE,
            EXIT_ACTIVATION_FAILED,
            EXIT_VERIFICATION_FAILED,
            EXIT_RPC,
            EXIT_PERSIST,
            EXIT_HEADLESS_UNSUPPORTED,
            EXIT_CHAIN_MISMATCH,
            EXIT_TOKEN_NOT_OWNED,
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "duplicate exit code in the table");
        assert!(!codes.contains(&EXIT_OK));
        assert!(!codes.contains(&EXIT_GENERIC));
        // clap exits 2 on usage errors - nothing of ours may collide with it.
        assert!(!codes.contains(&2));
    }

    #[test]
    fn cooldown_detail_reports_blocks_remaining() {
        let err = HeadlessError::CooldownActive { token_id: 3, blocks_remaining: 42 };
        let detail = err.machine_detail().expect("cooldown must carry a detail line");
        assert!(detail.contains("blocks_remaining=42"), "{detail}");
        assert!(detail.contains("token_id=3"), "{detail}");
        // The prose message must carry it too, for a human reading the logs.
        assert!(err.to_string().contains("42"));
    }

    #[test]
    fn insufficient_funds_detail_reports_both_amounts() {
        let err = HeadlessError::InsufficientFunds {
            shortfall: Some(crate::tx::Shortfall {
                required: alloy::primitives::U256::from(1_000_000u64),
                available: alloy::primitives::U256::from(12u64),
                covers: crate::tx::Covers::PriceAndGas,
            }),
        };
        let detail = err.machine_detail().unwrap();
        assert!(detail.contains("required_wei=1000000"), "{detail}");
        assert!(detail.contains("available_wei=12"), "{detail}");
        assert!(detail.contains("required_covers=price_plus_gas"), "{detail}");
    }

    /// The pre-flight shortfall is measured before gas can be estimated, so
    /// both the detail line and the message must say the figure is price-only.
    /// An orchestrator that topped up exactly this would fail again on gas.
    #[test]
    fn price_only_shortfall_is_labelled_as_excluding_gas() {
        let err = HeadlessError::InsufficientFunds {
            shortfall: Some(crate::tx::Shortfall {
                required: alloy::primitives::U256::from(10_000u64),
                available: alloy::primitives::U256::ZERO,
                covers: crate::tx::Covers::PriceOnly,
            }),
        };
        let detail = err.machine_detail().unwrap();
        assert!(detail.contains("required_covers=price"), "{detail}");
        assert!(!detail.contains("required_covers=price_plus_gas"), "{detail}");
        let rendered = err.to_string();
        assert!(rendered.contains("before gas"), "{rendered}");
        assert!(!rendered.contains("price + gas"), "{rendered}");
    }

    /// A node that rejects the transaction for lack of balance reports no
    /// parseable amounts. Emitting zeroes would tell an orchestrator the wallet
    /// needs nothing, so the detail line is omitted instead.
    #[test]
    fn insufficient_funds_with_unknown_amounts_emits_no_detail_line() {
        let err: HeadlessError =
            crate::tx::TxError::InsufficientFunds(None).into();
        assert_eq!(err.exit_code(), EXIT_INSUFFICIENT_FUNDS);
        assert!(err.machine_detail().is_none(), "{:?}", err.machine_detail());
        let rendered = err.to_string();
        assert!(rendered.contains("insufficient funds"), "{rendered}");
        assert!(!rendered.contains('0'), "must not imply an amount: {rendered}");
    }

    #[test]
    fn sold_out_detail_reports_supply() {
        let detail = HeadlessError::SoldOut { supply_cap: 100, minted: 100 }
            .machine_detail()
            .unwrap();
        assert!(detail.contains("supply_cap=100"), "{detail}");
        assert!(detail.contains("minted=100"), "{detail}");
    }

    /// A malformed `CONTRACT` constant is a build-time mistake: no retry can
    /// make it parse, so it must not land on the retryable RPC code.
    #[test]
    fn malformed_contract_address_is_terminal() {
        let ctx = HeadlessContext {
            // Unique per test run so no cached session can satisfy the fast
            // path before the address is parsed.
            app_id: "com.rub3.test.malformed-contract".to_string(),
            contract: "not-an-address".to_string(),
            chain_id: 8453,
            rpc_url: "http://127.0.0.1:1".to_string(),
            session_ttl_secs: 60,
            token_id: None,
        };
        let signer = crate::signer::LocalSigner::from_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();

        let err = ensure_headless(&signer, &ctx).expect_err("a malformed contract cannot activate");
        assert!(matches!(err, HeadlessError::NoContract), "got {err:?}");
        assert_eq!(err.exit_code(), EXIT_GENERIC);
        assert_ne!(err.exit_code(), EXIT_RPC, "an orchestrator would retry forever");
    }

    /// The enumeration order of `tokensOfOwner` is arbitrary after any
    /// transfer out, so an unqualified run must pick by id, not by position.
    #[test]
    fn unqualified_run_selects_the_lowest_token_id() {
        use super::headless::lowest_token;
        assert_eq!(lowest_token(&[5, 3, 9]), Some(3));
        assert_eq!(lowest_token(&[7]), Some(7));
        assert_eq!(lowest_token(&[]), None);
    }

    /// A signer holding several licenses activates them at different times. A
    /// run naming one token must reuse that token's own cached session, even
    /// when another token was activated more recently.
    #[test]
    fn explicit_token_reuses_its_own_session_when_another_is_newer() {
        let _guard = crate::session_store::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let app_id = "com.rub3.test.multi-token";
        let signer = crate::signer::LocalSigner::from_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();
        let wallet = signer.address();

        let older = signed_session(app_id, 7, &signer, "2026-01-01T00:00:00+00:00");
        let newer = signed_session(app_id, 3, &signer, "2026-06-01T00:00:00+00:00");
        crate::session_store::save_session(&older).unwrap();
        crate::session_store::save_session(&newer).unwrap();

        let picked = try_session_fast_path(app_id, "http://127.0.0.1:1", Some(wallet), Some(7))
            .expect("token 7 has a valid cached session of its own");
        assert_eq!(picked.token_id, 7);
        assert_eq!(picked.nonce, older.nonce);

        let latest = try_session_fast_path(app_id, "http://127.0.0.1:1", Some(wallet), None)
            .expect("an unqualified run still takes the newest session");
        assert_eq!(latest.token_id, 3);

        assert!(
            try_session_fast_path(app_id, "http://127.0.0.1:1", Some(wallet), Some(9)).is_none(),
            "a token with no cached session must be a miss",
        );

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    /// Builds a locally signed, unexpired session for `token_id`, persisted the
    /// way the headless door persists one.
    fn signed_session(
        app_id: &str,
        token_id: u64,
        signer: &crate::signer::LocalSigner,
        issued_at: &str,
    ) -> crate::session::Session {
        let wallet = crate::identity::format_addr(signer.address());
        let nonce = crate::session::new_nonce();
        let expires_at = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let message = crate::session::session_message(
            app_id,
            token_id,
            "access",
            &wallet,
            &wallet,
            &nonce,
            Some(&expires_at),
            None,
            None,
            None,
        );
        let signature = crate::signer::personal_sign(signer, &message).expect("sign session");

        crate::session::Session {
            app_id: app_id.to_string(),
            token_id,
            identity: "access".to_string(),
            user_id: wallet.clone(),
            tba: None,
            wallet,
            nonce,
            issued_at: issued_at.to_string(),
            expires_at: Some(expires_at),
            signature,
            chain: "base".to_string(),
            contract: "0x5FbDB2315678afecb367f032d93F642f64180aa3".to_string(),
            activation_tx: None,
            activation_block: None,
            activation_block_hash: None,
            session_id: None,
            device_pubkey: None,
        }
    }

    #[test]
    fn unclassified_failures_have_no_detail_line() {
        assert!(HeadlessError::Rpc("offline".into()).machine_detail().is_none());
        assert!(HeadlessError::ActivationFailed("x".into()).machine_detail().is_none());
    }

    /// Errors surface to stderr and to orchestrator logs; none of them may
    /// carry key material forwarded up from the signer.
    #[test]
    fn signer_errors_pass_through_without_key_material() {
        let err = HeadlessError::Signer(SignerError::MalformedKey);
        assert_eq!(err.exit_code(), EXIT_SIGNER);
        assert!(err.to_string().contains("RUB3_AGENT_KEY"));
    }

    #[test]
    fn tx_errors_map_to_their_classified_codes() {
        use crate::tx::TxError;
        use alloy::primitives::U256;

        let mapped: HeadlessError = TxError::InsufficientFunds(Some(crate::tx::Shortfall {
            required: U256::from(5u64),
            available: U256::ZERO,
            covers: crate::tx::Covers::PriceAndGas,
        }))
        .into();
        assert_eq!(mapped.exit_code(), EXIT_INSUFFICIENT_FUNDS);

        let mapped: HeadlessError = TxError::Rpc("no route to host".into()).into();
        assert_eq!(mapped.exit_code(), EXIT_RPC);

        let mapped: HeadlessError = TxError::Rejected("execution reverted".into()).into();
        assert_eq!(mapped.exit_code(), EXIT_ACTIVATION_FAILED);

        let mapped: HeadlessError = TxError::Signer(SignerError::NoSource).into();
        assert_eq!(mapped.exit_code(), EXIT_SIGNER);
    }
}
