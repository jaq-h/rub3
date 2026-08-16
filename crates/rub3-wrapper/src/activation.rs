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
    interactive_slow_path(
        app_id,
        contract,
        chain_id,
        rpc_url,
        developer_ens,
        session_ttl_secs,
    )
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
/// agent never launches on a session belonging to a different key. Like
/// `require_token` it **selects** rather than filters: the newest session
/// signed by that wallet wins, even when a session for another key is newer,
/// so a second agent on the same machine still reuses its own cache.
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
    let session = match (require_token, require_wallet) {
        (Some(token_id), _) => crate::session_store::load_session(app_id, token_id).ok()?,
        (None, Some(wallet)) => crate::session_store::load_latest_session_for_wallet(
            app_id,
            &crate::identity::format_addr(wallet),
        )
        .ok()?,
        (None, None) => crate::session_store::load_latest_session(app_id).ok()?,
    };

    if crate::session::verify_local(&session).is_err() {
        return None;
    }

    // The session directory is user-writable, so the filename a session was
    // loaded from proves nothing about the token it was issued for. Only the
    // signed field counts.
    if let Some(token_id) = require_token {
        if session.token_id != token_id {
            return None;
        }
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
/// An `activate()` transaction reverted, or did not confirm inside the poll
/// budget. Retryable, but not because nothing was spent: the same run may
/// already have completed a `purchase()`. A re-run re-reads ownership first,
/// so it activates the token it now holds instead of buying a second one. A
/// `purchase()` that fails to confirm is `EXIT_PURCHASE_UNCONFIRMED` instead.
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
/// A `purchase()` transaction was broadcast but did not confirm inside the
/// receipt budget, whether because no receipt arrived or because polling for
/// it kept failing. Deliberately distinct from `EXIT_ACTIVATION_FAILED`: the
/// money may already be spent, so this one must not be retried blindly.
pub const EXIT_PURCHASE_UNCONFIRMED: i32 = 21;
/// A listed price is above the ceiling the operator configured for that rail.
/// Never retried and never quietly worked around: the price exceeded a policy,
/// which is a different thing from the network having failed.
pub const EXIT_PRICE_ABOVE_POLICY: i32 = 22;
/// The contract at the configured address is not one this build will buy a
/// licence from, so nothing was bought from it. A refusal of the address, not a
/// network failure: retrying reaches the same code.
///
/// Two causes, needing two different responses, told apart by which key the
/// detail line carries:
///
/// - `contract=0x... canonical=<name> sells_licences=false` - the code *is*
///   canonical rub3 code, but at an address that sells no licences (the factory
///   or one of its deployer helpers). The build is pointed at the wrong
///   address; check what it was packed with.
/// - `contract=0x... code_bytes=N exposed=<a>|<b>` - the code matched no
///   fingerprint this build pins. That is a modified copy, or a template
///   release newer than this binary.
///
/// Neither is an accusation: `exposed` is a diagnostic naming what a blacklist
/// of names happened to see, and a miss says the code is unrecognised here, not
/// that it is malicious.
pub const EXIT_NOT_CANONICAL_CONTRACT: i32 = 23;

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
  14  activate() reverted, or did not confirm in time - retryable: a
      re-run re-reads ownership, so a purchase this run may already have
      completed is activated rather than paid for twice
  15  session verification failed
  16  chain RPC / transport failure
  17  session could not be persisted
  18  headless mode not compiled into this build
  19  chain id mismatch between the RPC endpoint and this build
  20  --token-id names a token this signer does not hold
  21  purchase broadcast but not confirmed - the receipt never arrived in
      time, or the receipt query itself kept failing. Either way the price
      may already be spent, so do NOT retry blindly: stderr carries
      `tx_hash=0x...`; resolve that transaction first, then re-run once it
      has mined or been dropped
  22  the listed price is above the configured spend ceiling - stderr
      carries `rail=... listed=... maximum=... token=0x...`. It reports
      only that the price was refused: the ceiling is weighed before
      anything is signed, so the rail was not exercised and this is no
      evidence it is otherwise usable. Not retryable: raise
      RUB3_AGENT_MAX_TOKEN_AMOUNT or do not buy
  23  this build will not buy a licence from the contract at that
      address. Checked before anything is signed, so no transaction was
      sent and nothing was spent. Not retryable: the same address holds
      the same code. Two causes, told apart by which key stderr
      carries:
        `contract=0x... canonical=NAME sells_licences=false` - the code
          IS canonical rub3 code, at an address that sells no licences
          (the factory, or a deployer helper). Wrong address: check
          what this build is pointed at
        `contract=0x... code_bytes=N exposed=A|B` - the code matched no
          fingerprint this build pins: a modified copy, or a template
          release newer than this binary. `exposed` is a
          pipe-separated list, since a signature may contain commas,
          and is `none` when the scan named nothing
      Neither is an accusation: the scan is a diagnostic, and a miss
      says the code is unrecognised here, not that it is malicious

Signer sources, highest precedence first:
  RUB3_AGENT_KEY                        raw hex private key (dev / CI only)
  RUB3_AGENT_KEYSTORE                   encrypted V3 keystore file
  RUB3_AGENT_KEYSTORE_PASSWORD_FILE     password file for the keystore (preferred)
  RUB3_AGENT_KEYSTORE_PASSWORD          password, inline

Spend policy:
  RUB3_AGENT_MAX_TOKEN_AMOUNT           the most this agent may authorize on a
                                        contract's stablecoin rail, an integer
                                        in that payment token's own smallest
                                        unit (USDC has 6 decimals, so 5 USDC is
                                        5000000). No default: token decimals
                                        differ, so no single number means the
                                        same thing twice. Unset leaves the
                                        stablecoin rail unavailable and buys in
                                        ETH; a malformed value is a hard error.
                                        Weighed after the rail is known to be
                                        advertised, affordable and signable, and
                                        before anything is signed: an
                                        authorization is spendable by anyone who
                                        sees it, so one must never exist for an
                                        amount policy refuses";

// ── Headless activation ───────────────────────────────────────────────────────

#[cfg(feature = "headless")]
pub use self::headless::{
    ensure_headless, HeadlessContext, HeadlessError, HeadlessOutcome, PaymentRail, SpendPolicy,
    SpendVerdict, ENV_MAX_TOKEN_AMOUNT,
};

#[cfg(feature = "headless")]
mod headless {
    use super::*;
    use alloy::primitives::U256;

    use crate::attest;
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

    /// The environment variable holding the stablecoin spend ceiling.
    ///
    /// Part of the `RUB3_AGENT_*` family, alongside the signer sources: an
    /// operator already has to configure this family before a headless build
    /// can spend anything at all, so the ceiling joins a surface they have
    /// already met rather than introducing a new class of setup.
    pub const ENV_MAX_TOKEN_AMOUNT: &str = "RUB3_AGENT_MAX_TOKEN_AMOUNT";

    /// The operator's ceiling on what one headless run may pay, per rail.
    ///
    /// One type holds every "never spend more than this" rule and one function
    /// checks against it, so the queued ETH ceiling (`rub3-max-price-policy`,
    /// for the gap between reading `price()` and sending `value: price`)
    /// becomes another field here and another [`SpendPolicy::check_token_amount`]
    /// sibling, rather than a second mechanism grown beside this one.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct SpendPolicy {
        /// The most a single EIP-3009 authorization may carry, in the payment
        /// token's own smallest unit.
        ///
        /// `None` means the operator configured none, and that makes the
        /// stablecoin rail *unavailable* rather than unlimited. There is no
        /// number this crate could pick instead: the unit belongs to whichever
        /// token the contract lists, and decimals differ between them (USDC 6,
        /// DAI 18), so any fixed default is wrongly scaled for some token. An
        /// unset ceiling therefore falls back to ETH, which never spends a
        /// currency the operator did not size.
        pub max_token_amount: Option<U256>,
    }

    /// What the policy says about one proposed spend.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SpendVerdict {
        /// Within a ceiling the operator configured.
        Allowed,
        /// No ceiling is configured for this rail, so the rail cannot be used.
        /// Carries the variable an operator sets to enable it, so the printed
        /// fallback reason can name it.
        NoCeiling { var: &'static str },
    }

    impl SpendPolicy {
        /// Reads the policy from the process environment.
        pub fn from_env() -> Result<Self, HeadlessError> {
            let raw = std::env::var(ENV_MAX_TOKEN_AMOUNT).ok();
            Self::from_raw(raw.as_deref())
        }

        /// The parsing rules, separated from the environment so they can be
        /// exercised without mutating process-global state.
        ///
        /// A value that is present but unreadable is a hard error rather than
        /// a fallback: silently treating a typo as zero would refuse every
        /// purchase, and silently treating it as unlimited would authorize an
        /// amount nobody chose. Both are worse than stopping.
        pub fn from_raw(max_token_amount: Option<&str>) -> Result<Self, HeadlessError> {
            let Some(raw) = max_token_amount else {
                return Ok(Self {
                    max_token_amount: None,
                });
            };

            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(HeadlessError::Config {
                    var: ENV_MAX_TOKEN_AMOUNT,
                    detail: "is set but empty; unset it to buy in ETH, or set an amount"
                        .to_string(),
                });
            }

            let parsed = trimmed.parse::<U256>().map_err(|e| HeadlessError::Config {
                var: ENV_MAX_TOKEN_AMOUNT,
                detail: format!(
                    "is not a whole non-negative amount in the payment token's smallest \
                     unit: {trimmed:?} ({e})"
                ),
            })?;

            Ok(Self {
                max_token_amount: Some(parsed),
            })
        }

        /// The single place a stablecoin price is weighed against the policy.
        ///
        /// Three outcomes, deliberately distinct: within a configured ceiling,
        /// no ceiling configured (the rail is unusable, and the caller falls
        /// back to ETH), or above the ceiling, which is a refusal rather than a
        /// fallback - see [`HeadlessError::PriceAbovePolicy`].
        pub fn check_token_amount(
            &self,
            token: Address,
            listed: U256,
        ) -> Result<SpendVerdict, HeadlessError> {
            let Some(maximum) = self.max_token_amount else {
                return Ok(SpendVerdict::NoCeiling {
                    var: ENV_MAX_TOKEN_AMOUNT,
                });
            };

            if listed > maximum {
                return Err(HeadlessError::PriceAbovePolicy {
                    rail: "erc3009",
                    listed: listed.to_string(),
                    maximum: maximum.to_string(),
                    token: Some(crate::identity::format_addr(token)),
                });
            }

            Ok(SpendVerdict::Allowed)
        }
    }

    /// Which currency a purchase was settled in, and how much of it.
    ///
    /// Reported inside [`HeadlessOutcome::PurchasedAndActivated`] so the rail
    /// the wrapper chose is visible in the one line the CLI prints, rather than
    /// something an operator has to reconstruct from the chain.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PaymentRail {
        /// Native ETH: the transaction carried the price as its value.
        Eth { price_wei: String },
        /// EIP-3009 stablecoin (§2.2): the buyer signed an authorization over
        /// `amount` of `token`, and the transaction itself carried no value.
        Erc3009 { token: String, amount: String },
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
        PurchasedAndActivated { token_id: u64, paid: PaymentRail },
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
        InsufficientFunds {
            shortfall: Option<crate::tx::Shortfall>,
        },
        /// The signer holds no token and the contract has minted its cap.
        SoldOut { supply_cap: u64, minted: u64 },
        /// `activate()` is rate-limited for this token for another N blocks.
        CooldownActive {
            token_id: u64,
            blocks_remaining: u64,
        },
        /// An `activate()` transaction reverted, or did not confirm inside
        /// the poll budget. Retryable, but not because nothing was spent: the
        /// same run may already have completed a `purchase()`. A re-run
        /// re-reads ownership first, so it activates the token it now holds
        /// instead of buying a second one. A `purchase()` that fails to
        /// confirm is `PurchaseUnconfirmed` instead.
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
        /// A `purchase()` transaction was broadcast but was not confirmed
        /// inside the poll budget, either because no receipt arrived or
        /// because the node stopped answering. It may still be mined, so the
        /// price may already have been paid: retrying before resolving
        /// `tx_hash` can buy a second license. `reason` carries the transport
        /// failure when polling itself was what broke.
        PurchaseUnconfirmed {
            tx_hash: String,
            after_secs: u64,
            reason: Option<String>,
        },
        /// The price a contract lists on a rail is above the ceiling the
        /// operator configured for that rail. Deliberately its own outcome and
        /// its own exit code: an orchestrator has to tell "this costs more than
        /// my policy allows" apart from "the network failed", and nothing on
        /// the network will change this answer. Never falls back to another
        /// rail, because a refusal is not a routing problem.
        ///
        /// It says the price was refused and nothing more. The rail is *not*
        /// validated end to end first: doing that would mean signing an
        /// authorization for the refused amount and handing it to an endpoint
        /// that could broadcast it. So this outcome is not evidence that the
        /// rail is otherwise healthy, and the message says so.
        PriceAbovePolicy {
            /// Which rail was refused, so an ETH ceiling reports through the
            /// same variant when it lands.
            rail: &'static str,
            listed: String,
            maximum: String,
            /// The payment token, when the rail has one.
            token: Option<String>,
        },
        /// The contract at the configured address is not canonical rub3 code,
        /// so this run refused to buy from it. Nothing was signed and nothing
        /// was sent: the check runs before the first signature, because a
        /// refusal that has already signed something is not a refusal.
        ///
        /// A refusal of *this address*, never retryable, and deliberately not
        /// an accusation: it says the deployed code matched no entry this
        /// build pins, which is equally what a legitimate contract released
        /// after this wrapper was packed looks like.
        NotCanonicalContract {
            contract: String,
            refusal: attest::Refusal,
        },
        /// An operator-supplied configuration value could not be read. A hard
        /// stop rather than a guess: every reading of a malformed spend
        /// ceiling is wrong, and the wrong ones spend money.
        Config { var: &'static str, detail: String },
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
                HeadlessError::PurchaseUnconfirmed { tx_hash, after_secs, reason } => {
                    write!(
                        f,
                        "purchase() tx {tx_hash} was broadcast but did not confirm within \
                         {after_secs}s"
                    )?;
                    if let Some(reason) = reason {
                        write!(f, " ({reason})")?;
                    }
                    write!(
                        f,
                        ": it may still be mined, so check it before re-running rather than \
                         retrying blindly"
                    )
                }
                HeadlessError::PriceAbovePolicy { rail, listed, maximum, token } => {
                    write!(f, "price above policy: the {rail} rail lists {listed}")?;
                    if let Some(token) = token {
                        write!(f, " of {token}")?;
                    }
                    write!(
                        f,
                        ", above the configured maximum of {maximum}. Raise \
                         {ENV_MAX_TOKEN_AMOUNT} if this price is acceptable. This says only \
                         that the price was refused: the rail was not exercised, because the \
                         purchase pre-flight is deliberately not run for an amount policy \
                         refuses, so it is no evidence the rail is otherwise usable"
                    )
                }
                HeadlessError::NotCanonicalContract { contract, refusal } => write!(
                    f,
                    "refusing to buy from {contract}: {refusal}. Nothing was signed and no \
                     transaction was sent. This build compares a contract's deployed code \
                     against the canonical fingerprints it was packed with, and buys only on a \
                     match; a contract released after this build was packed will also land here"
                ),
                HeadlessError::Config { var, detail } => {
                    write!(f, "invalid configuration: {var} {detail}")
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
                HeadlessError::PurchaseUnconfirmed { .. } => EXIT_PURCHASE_UNCONFIRMED,
                HeadlessError::PriceAbovePolicy { .. } => EXIT_PRICE_ABOVE_POLICY,
                HeadlessError::NotCanonicalContract { .. } => EXIT_NOT_CANONICAL_CONTRACT,
                // No dedicated code: a malformed variable is an operator
                // mistake to read on stderr, not a state an orchestrator
                // branches on. It is still a hard stop.
                HeadlessError::Config { .. } => EXIT_GENERIC,
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
                HeadlessError::CooldownActive {
                    token_id,
                    blocks_remaining,
                } => Some(format!(
                    "token_id={token_id} blocks_remaining={blocks_remaining}"
                )),
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
                HeadlessError::ChainIdMismatch { expected, actual } => Some(format!(
                    "expected_chain_id={expected} actual_chain_id={actual}"
                )),
                HeadlessError::TokenNotOwned { token_id, .. } => {
                    Some(format!("token_id={token_id}"))
                }
                HeadlessError::PurchaseUnconfirmed {
                    tx_hash,
                    after_secs,
                    ..
                } => Some(format!("tx_hash={tx_hash} waited_secs={after_secs}")),
                // Every number a policy decision turned on, so an orchestrator
                // can tell how far over the ceiling the listing was without
                // reading the chain again.
                HeadlessError::PriceAbovePolicy {
                    rail,
                    listed,
                    maximum,
                    token,
                } => Some(match token {
                    Some(token) => {
                        format!("rail={rail} listed={listed} maximum={maximum} token={token}")
                    }
                    None => format!("rail={rail} listed={listed} maximum={maximum}"),
                }),
                // What the check actually saw, so an operator can tell "an
                // address holding no contract" from "a contract that answers
                // to seize(uint256)" without reading the chain again. The
                // exposed list is a diagnostic and nothing more: an empty one
                // is not a clean bill of health.
                HeadlessError::NotCanonicalContract { contract, refusal } => Some(match refusal {
                    attest::Refusal::Unrecognised(finding) => format!(
                        "contract={contract} code_bytes={} exposed={}",
                        finding.code_len,
                        if finding.exposed.is_empty() {
                            "none".to_string()
                        } else {
                            finding.exposed.join("|")
                        }
                    ),
                    attest::Refusal::NotALicence { contract: name, .. } => {
                        format!("contract={contract} canonical={name} sells_licences=false")
                    }
                }),
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
        let contract: Address = ctx
            .contract
            .parse()
            .map_err(|_| HeadlessError::NoContract)?;
        if contract.is_zero() {
            return Err(HeadlessError::NoContract);
        }

        // Signing for the wrong network is silent and expensive; catch it before
        // the first transaction rather than after.
        let node_chain_id =
            rpc::chain_id(&ctx.rpc_url).map_err(|e| HeadlessError::Rpc(e.to_string()))?;
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
                let (id, paid) = purchase(signer, ctx, contract, wallet)?;
                (
                    id,
                    HeadlessOutcome::PurchasedAndActivated { token_id: id, paid },
                )
            }
        };

        // ── Cooldown gate ────────────────────────────────────────────────────
        let (ready, blocks_remaining) = rpc::cooldown_ready(&ctx.rpc_url, contract, token_id)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        if !ready {
            return Err(HeadlessError::CooldownActive {
                token_id,
                blocks_remaining,
            });
        }

        // ── activate() ───────────────────────────────────────────────────────
        let calldata = decode_calldata(&rpc::encode_activate_calldata(token_id))?;
        let tx_hash = tx::send(
            &ctx.rpc_url,
            signer,
            &TxPlan {
                to: contract,
                value: U256::ZERO,
                input: calldata,
            },
        )?;

        let receipt = rpc::wait_for_receipt(&ctx.rpc_url, &tx_hash).map_err(|e| {
            HeadlessError::ActivationFailed(format!("activate() tx {tx_hash}: {e}"))
        })?;
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

        let signature =
            crate::signer::personal_sign(signer, &draft.message).map_err(HeadlessError::Signer)?;

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

    /// How long a purchase authorization stays spendable, in seconds.
    ///
    /// The wrapper signs and broadcasts in the same breath, so this only has to
    /// survive congestion, not a human. Keeping it short bounds how long a
    /// signature that never landed is worth anything to anyone who saw it.
    const AUTHORIZATION_TTL_SECS: u64 = 900;

    /// Buys a token for `wallet` and returns `(token_id, rail)`.
    ///
    /// Mirrors the interactive purchase flow (§1.7) - same supply check, same
    /// `Transfer` log parse to recover the minted id - with the broadcast done
    /// by the signer instead of a human's wallet, and with one addition: it
    /// pays in the contract's stablecoin when the contract advertises one, the
    /// operator's spend ceiling covers the listed amount, and the wallet holds
    /// enough of it - and in ETH otherwise (§2.2). See [`choose_rail`].
    fn purchase(
        signer: &dyn Signer,
        ctx: &HeadlessContext,
        contract: Address,
        wallet: Address,
    ) -> Result<(u64, PaymentRail), HeadlessError> {
        // Is this contract the code we think it is? One `eth_getCode`, compared
        // against the fingerprints this build was packed with.
        //
        // **First, before every other step in this function.** "Before
        // `tx::send`" is not enough: `choose_rail` signs an EIP-3009
        // authorization and hands it to the RPC endpoint as pre-flight
        // calldata, and anyone may submit a `purchaseWithAuthorization`, so
        // disclosure is the spend. A refusal that arrives after that has
        // already paid. The ordering rule is the same one the spend ceiling
        // follows, for the same reason.
        //
        // The gate fails closed, including on a chain read that did not
        // complete: refusing to spend money on code that could not be verified
        // is the correct default here. It is emphatically *not* the default on
        // the launch path, which never calls this - see `attest`'s module docs.
        let canonical =
            attest::verify_before_purchase(&ctx.rpc_url, contract).map_err(|e| match e {
                attest::GateError::Fetch(e) => HeadlessError::Rpc(e.to_string()),
                attest::GateError::Refused(refusal) => HeadlessError::NotCanonicalContract {
                    contract: crate::identity::format_addr(contract),
                    refusal,
                },
            })?;
        // One line, on the one path that spends money, naming what the money
        // is about to go to. Mirrors the rail-fallback note below.
        eprintln!(
            "rub3: {contract} verified as canonical {} ({})",
            canonical.contract, canonical.release
        );

        let supply_cap = rpc::supply_cap(&ctx.rpc_url, contract)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        let minted = rpc::next_token_id(&ctx.rpc_url, contract)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?;
        if supply_cap != 0 && minted >= supply_cap {
            return Err(HeadlessError::SoldOut { supply_cap, minted });
        }

        let (plan, paid) = match choose_rail(signer, ctx, contract, wallet)? {
            Some(rail) => {
                let calldata = decode_calldata(&rpc::encode_purchase_with_authorization_calldata(
                    wallet, rail.auth,
                ))?;
                (
                    TxPlan {
                        to: contract,
                        // The stablecoin rail carries no ETH at all: the price
                        // moves inside the token, and this transaction is pure
                        // calldata plus gas.
                        value: U256::ZERO,
                        input: calldata,
                    },
                    PaymentRail::Erc3009 {
                        token: crate::identity::format_addr(rail.price.token),
                        amount: rail.price.amount.to_string(),
                    },
                )
            }
            None => {
                let price = rpc::eth_price(&ctx.rpc_url, contract)
                    .map_err(|e| HeadlessError::Rpc(e.to_string()))?;
                (
                    TxPlan {
                        to: contract,
                        value: price,
                        input: decode_calldata(&rpc::encode_purchase_calldata(wallet))?,
                    },
                    PaymentRail::Eth {
                        price_wei: price.to_string(),
                    },
                )
            }
        };

        let tx_hash = tx::send(&ctx.rpc_url, signer, &plan)?;

        let receipt =
            rpc::wait_for_receipt(&ctx.rpc_url, &tx_hash).map_err(|e| unconfirmed(&tx_hash, e))?;
        if !receipt.status {
            return Err(HeadlessError::ActivationFailed(format!(
                "purchase() reverted on-chain (tx {tx_hash})"
            )));
        }

        let token_id = rpc::mint_token_id(&ctx.rpc_url, &tx_hash, contract, wallet)
            .map_err(|e| HeadlessError::ActivationFailed(e.to_string()))?;

        Ok((token_id, paid))
    }

    /// The stablecoin rail, with the authorization that pays for it already
    /// signed and already proven to execute.
    ///
    /// Everything the rail needs is resolved before one of these exists: the
    /// payment token's EIP-712 domain, the operator's spend ceiling, the
    /// buyer's signature, and an `eth_call` of the exact transaction that will
    /// be broadcast. Once [`choose_rail`] hands one back the rail is committed,
    /// and nothing downstream can discover a reason it was unusable after the
    /// ETH path has already been passed over.
    pub(super) struct TokenRail {
        price: rpc::StablecoinPrice,
        auth: rpc::IRub3License::PaymentAuthorization,
    }

    /// Picks the rail: the stablecoin one when the contract advertises it, the
    /// wallet can afford it, the payment token answers the reads an
    /// authorization needs, the operator's ceiling covers the listed amount,
    /// *and* the payment token accepts the authorization that pays for it. ETH
    /// otherwise.
    ///
    /// **The order is load-bearing. The ceiling is weighed before anything is
    /// signed, and nothing may be moved in front of it.** A refusal that has
    /// already signed a valid authorization for the full amount and shipped it
    /// to an endpoint that can broadcast it is not a refusal, it is the payment
    /// with extra steps: `purchaseWithAuthorization` is submittable by anyone
    /// by design, so disclosure *is* spending. The ceiling bounds what a single
    /// authorization may carry, which means bounding whether one is created at
    /// all. Hence: advertised -> affordable -> domain readable -> ceiling ->
    /// sign -> pre-flight.
    ///
    /// That splits the outcomes cleanly in two, and they must not be blurred:
    ///
    ///   * **The ceiling refused the listed price.** A refusal:
    ///     [`HeadlessError::PriceAbovePolicy`] and its own non-retryable exit
    ///     code, never a quiet ETH purchase instead. An orchestrator has to be
    ///     able to tell a policy breach from a network failure.
    ///   * **The rail was not usable at all** - not advertised, not affordable,
    ///     no readable EIP-712 domain, no ceiling configured to size it by, or
    ///     an authorization the payment token will not accept. ETH is then not
    ///     a fallback from anything: it is the path this agent was always on.
    ///     The printed reason names the fact that put it there and says nothing
    ///     about a spend limit.
    ///
    /// The accepted cost of signing last is that "otherwise usable" cannot
    /// include the pre-flight: a token lacking the `bytes signature` overload
    /// but priced *within* the ceiling still falls back to ETH, while one that
    /// is both unusable and priced above the ceiling reports the refusal
    /// instead. The refusal therefore does not claim the rail was healthy - see
    /// [`HeadlessError::PriceAbovePolicy`] - and paying to find out would mean
    /// disclosing the very authorization the ceiling exists to prevent.
    ///
    /// A **transport** failure on any of these reads is neither of those: it
    /// stops the run. That is why the reads branch on
    /// [`rpc::RpcError::is_transport`] rather than treating every failure as an
    /// answer - a blinking node must never silently change the currency.
    ///
    /// So ETH stays exactly what it was before §2.2 rather than a deprecated
    /// path: nothing that bought a licence before may start failing because a
    /// contract now also lists a token, because that token is imperfect, or
    /// because the agent cannot afford a listing that happens to exceed a
    /// ceiling.
    fn choose_rail(
        signer: &dyn Signer,
        ctx: &HeadlessContext,
        contract: Address,
        wallet: Address,
    ) -> Result<Option<TokenRail>, HeadlessError> {
        let Some(price) = rpc::stablecoin_rail(&ctx.rpc_url, contract)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?
        else {
            return Ok(None);
        };
        let token = crate::identity::format_addr(price.token);

        let balance = match rpc::erc20_balance_of(&ctx.rpc_url, price.token, wallet) {
            Ok(balance) => balance,
            Err(e) if e.is_transport() => return Err(HeadlessError::Rpc(e.to_string())),
            Err(e) => {
                return Ok(eth_instead(format!(
                    "{token} did not answer balanceOf ({e})"
                )));
            }
        };
        if balance < price.amount {
            return Ok(eth_instead(format!(
                "{} holds {} of {}, price is {}",
                crate::identity::format_addr(wallet),
                balance,
                token,
                price.amount,
            )));
        }

        // EIP-3009 mandates the authorization functions and `authorizationState`,
        // which is all the licence contract's constructor probe can check. The
        // `DOMAIN_SEPARATOR()` getter is a convention on top, and a token
        // without it cannot have an authorization built for it off-chain. That
        // is a fact about the token, so it selects ETH rather than ending a
        // purchase that the ETH rail would have completed.
        let domain_separator = match rpc::token_domain_separator(&ctx.rpc_url, price.token) {
            Ok(domain_separator) => domain_separator,
            Err(e) if e.is_transport() => return Err(HeadlessError::Rpc(e.to_string())),
            Err(e) => {
                return Ok(eth_instead(format!(
                    "{token} did not answer DOMAIN_SEPARATOR() ({e}), so no EIP-3009 \
                     authorization can be signed for it"
                )));
            }
        };

        // Before anything is signed, and nothing may be moved in front of this.
        // An authorization the policy refuses must never exist: it is
        // submittable by anyone, so handing one to an RPC endpoint spends the
        // money whatever this function returns afterwards.
        match SpendPolicy::from_env()?.check_token_amount(price.token, price.amount)? {
            SpendVerdict::Allowed => {}
            SpendVerdict::NoCeiling { var } => {
                return Ok(eth_instead(format!(
                    "no stablecoin spend ceiling is configured - set {var} to the most this \
                     agent may authorize, in {token}'s own smallest unit",
                )));
            }
        }

        let auth = authorize_purchase(
            signer,
            ctx,
            contract,
            wallet,
            price.amount,
            domain_separator,
        )?;

        // The licence contracts call the `bytes signature` overload of
        // `receiveWithAuthorization`, which EIP-3009 does not mandate: a token
        // implementing only the split `(v, r, s)` form is conforming, passes
        // the licence contract's constructor probe, and still reverts here. The
        // contract cannot detect that at deploy time, so the wrapper executes
        // the exact transaction it is about to send and reads the answer before
        // any gas is spent. It executes the whole purchase, so the revert it
        // reports may be about the licence contract or the buyer rather than
        // the overload: lead with what the chain said.
        match rpc::preflight_purchase_with_authorization(
            &ctx.rpc_url,
            contract,
            wallet,
            wallet,
            auth.clone(),
        ) {
            Ok(()) => {}
            Err(e) if e.is_transport() => return Err(HeadlessError::Rpc(e.to_string())),
            Err(e) => {
                return Ok(eth_instead(format!(
                    "a purchase paid in {token} does not execute: {e}. One possible cause is \
                     that {token} does not implement the `bytes signature` overload of \
                     receiveWithAuthorization that this licence contract calls; the revert \
                     above is the authoritative detail"
                )));
            }
        }

        Ok(Some(TokenRail { price, auth }))
    }

    /// Records why the stablecoin rail was passed over and selects ETH.
    ///
    /// One place, so every fallback reaches the operator in the same shape and
    /// none of them is silent.
    fn eth_instead(reason: String) -> Option<TokenRail> {
        eprintln!("rub3: falling back to the ETH rail - {reason}");
        None
    }

    /// Signs the EIP-3009 authorization that pays for one purchase.
    ///
    /// Everything that binds the authorization is read from the chain rather
    /// than assumed here: the nonce from the licence contract (it is what ties
    /// the signature to the mint recipient) and the EIP-712 domain from the
    /// payment token itself, resolved in [`choose_rail`]. The wrapper
    /// contributes only the salt and the validity window.
    ///
    /// A failure reading the nonce stays a hard error, including a
    /// contract-level one: `purchaseAuthorizationNonce` lives on the licence
    /// contract, so an address that cannot answer it is not a rub3 licence
    /// contract at all. That is not a payment-rail question, and quietly
    /// buying in ETH would hide it.
    fn authorize_purchase(
        signer: &dyn Signer,
        ctx: &HeadlessContext,
        contract: Address,
        wallet: Address,
        amount: U256,
        domain_separator: alloy::primitives::B256,
    ) -> Result<rpc::IRub3License::PaymentAuthorization, HeadlessError> {
        use alloy::primitives::B256;
        use rand::RngCore;

        let mut salt_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut salt_bytes);
        let salt = B256::from(salt_bytes);

        let nonce = rpc::purchase_authorization_nonce(&ctx.rpc_url, contract, wallet, salt)
            .map_err(|e| HeadlessError::Rpc(e.to_string()))?;

        let valid_after = U256::ZERO;
        let valid_before =
            U256::from(chrono::Utc::now().timestamp().max(0) as u64 + AUTHORIZATION_TTL_SECS);

        let digest = rpc::receive_authorization_digest(
            domain_separator,
            wallet,
            contract,
            amount,
            valid_after,
            valid_before,
            nonce,
        );

        let signature = signer.sign_prehash(digest).map_err(HeadlessError::Signer)?;

        Ok(rpc::IRub3License::PaymentAuthorization {
            from: wallet,
            validAfter: valid_after,
            validBefore: valid_before,
            salt,
            signature: pack_signature(&signature).into(),
        })
    }

    /// The 65-byte `r || s || v` packing an EOA signature is, with `v` in
    /// {27, 28}, which is what [`alloy::primitives::Signature::as_bytes`]
    /// produces.
    ///
    /// The licence contract hands these bytes straight to the payment token,
    /// whose signature checker recovers a signer from exactly this layout, so
    /// the recovery byte is deliberately not hand-rolled here: an off-by-27
    /// would yield an authorization the token attributes to some other address
    /// entirely. A smart-contract wallet's EIP-1271 signature is not built here
    /// and need not look like this at all - it goes through the same field
    /// untouched - but the wrapper's own signers are keys, so this is what they
    /// produce.
    fn pack_signature(signature: &alloy::primitives::Signature) -> [u8; 65] {
        signature.as_bytes()
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

    /// Every way of failing to confirm a broadcast `purchase()`.
    ///
    /// Once the transaction is on the wire the price is committed, so no such
    /// failure is retryable - not a timeout, and not the node going away while
    /// we poll. Both map to the terminal code that hands the orchestrator the
    /// hash to resolve, because a retry buys a second license.
    pub(super) fn unconfirmed(tx_hash: &str, e: rpc::ReceiptWaitError) -> HeadlessError {
        HeadlessError::PurchaseUnconfirmed {
            tx_hash: tx_hash.to_string(),
            after_secs: e.after_secs(),
            reason: e.transport_message().map(str::to_string),
        }
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
    use crate::attest;
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
            (
                HeadlessError::SoldOut {
                    supply_cap: 10,
                    minted: 10,
                },
                12,
            ),
            (
                HeadlessError::CooldownActive {
                    token_id: 3,
                    blocks_remaining: 42,
                },
                13,
            ),
            (HeadlessError::ActivationFailed("reverted".into()), 14),
            (HeadlessError::VerificationFailed("bad sig".into()), 15),
            (HeadlessError::Rpc("offline".into()), 16),
            (HeadlessError::Persist("read-only fs".into()), 17),
            (
                HeadlessError::ChainIdMismatch {
                    expected: 8453,
                    actual: 31337,
                },
                19,
            ),
            (
                HeadlessError::TokenNotOwned {
                    token_id: 7,
                    wallet: "0x01".into(),
                },
                20,
            ),
            (HeadlessError::NoContract, 1),
            (
                HeadlessError::PurchaseUnconfirmed {
                    tx_hash: "0xfeed".into(),
                    after_secs: 30,
                    reason: None,
                },
                21,
            ),
            (
                HeadlessError::PriceAbovePolicy {
                    rail: "erc3009",
                    listed: "10000000000".into(),
                    maximum: "5000000".into(),
                    token: Some("0x0000000000000000000000000000000000000abc".into()),
                },
                22,
            ),
            (
                HeadlessError::NotCanonicalContract {
                    contract: "0x0000000000000000000000000000000000000abc".into(),
                    refusal: attest::Refusal::Unrecognised(attest::Unrecognised {
                        code_len: 4096,
                        exposed: vec!["seize(uint256)"],
                    }),
                },
                23,
            ),
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
            EXIT_PURCHASE_UNCONFIRMED,
            EXIT_PRICE_ABOVE_POLICY,
            EXIT_NOT_CANONICAL_CONTRACT,
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            codes.len(),
            "duplicate exit code in the table"
        );
        assert!(!codes.contains(&EXIT_OK));
        assert!(!codes.contains(&EXIT_GENERIC));
        // clap exits 2 on usage errors - nothing of ours may collide with it.
        assert!(!codes.contains(&2));
    }

    // ── Spend policy (§2.2) ──────────────────────────────────────────────────

    /// An unconfigured ceiling makes the stablecoin rail unavailable, not
    /// unlimited. The verdict names the variable so the fallback printed to the
    /// operator can tell them what to set.
    #[test]
    fn an_unset_ceiling_leaves_the_stablecoin_rail_unavailable() {
        let policy = SpendPolicy::from_raw(None).expect("an unset ceiling is not an error");
        assert_eq!(policy.max_token_amount, None);
        assert_eq!(
            policy
                .check_token_amount(Address::ZERO, alloy::primitives::U256::from(1u64))
                .expect("an unset ceiling refuses nothing outright"),
            SpendVerdict::NoCeiling {
                var: ENV_MAX_TOKEN_AMOUNT
            },
        );
    }

    /// The ceiling is inclusive: a listing at exactly the configured maximum is
    /// within policy, and one wei of the token above it is not.
    #[test]
    fn a_listing_at_the_ceiling_is_allowed_and_one_above_it_is_refused() {
        let policy = SpendPolicy::from_raw(Some("5000000")).expect("a plain integer parses");
        assert_eq!(
            policy
                .check_token_amount(Address::ZERO, alloy::primitives::U256::from(5_000_000u64))
                .expect("equal to the ceiling is within policy"),
            SpendVerdict::Allowed,
        );

        let err = policy
            .check_token_amount(Address::ZERO, alloy::primitives::U256::from(5_000_001u64))
            .expect_err("above the ceiling must refuse");
        assert_eq!(err.exit_code(), EXIT_PRICE_ABOVE_POLICY);
    }

    /// The refusal has to be machine-readable: an orchestrator reads how far
    /// over the listing was, and against which token, without asking the chain
    /// again.
    #[test]
    fn a_refused_price_reports_the_amounts_and_the_token() {
        let token: Address = "0x0000000000000000000000000000000000000abc"
            .parse()
            .expect("test address");
        let err = SpendPolicy::from_raw(Some("1000000"))
            .expect("a plain integer parses")
            .check_token_amount(token, alloy::primitives::U256::from(9_000_000u64))
            .expect_err("above the ceiling must refuse");

        let detail = err
            .machine_detail()
            .expect("a policy refusal must carry a detail line");
        assert!(detail.contains("listed=9000000"), "{detail}");
        assert!(detail.contains("maximum=1000000"), "{detail}");
        assert!(detail.contains("rail=erc3009"), "{detail}");
        assert!(
            detail.to_ascii_lowercase().contains("0abc"),
            "the refused token must be named: {detail}"
        );

        let rendered = err.to_string();
        assert!(rendered.contains("9000000"), "{rendered}");
        assert!(rendered.contains("1000000"), "{rendered}");
        assert!(rendered.contains(ENV_MAX_TOKEN_AMOUNT), "{rendered}");
    }

    /// A malformed ceiling is a hard stop. Reading it as zero would refuse
    /// every purchase and reading it as unlimited would authorize an amount
    /// nobody chose, so neither silent reading is available.
    #[test]
    fn a_malformed_ceiling_is_a_hard_configuration_error() {
        for raw in ["abc", "-1", "5.0", "1e6", " ", "5000000usdc"] {
            let err = SpendPolicy::from_raw(Some(raw))
                .err()
                .unwrap_or_else(|| panic!("{raw:?} must not parse"));
            assert!(
                matches!(err, HeadlessError::Config { .. }),
                "{raw:?} produced {err:?}"
            );
            assert!(
                err.to_string().contains(ENV_MAX_TOKEN_AMOUNT),
                "the message must name the variable: {err}"
            );
        }
    }

    /// Zero is a legitimate ceiling - it means "never pay on this rail" - and
    /// must not be confused with the variable being unset.
    #[test]
    fn a_zero_ceiling_refuses_every_non_zero_price() {
        let policy = SpendPolicy::from_raw(Some("0")).expect("zero is a valid ceiling");
        assert_eq!(
            policy.max_token_amount,
            Some(alloy::primitives::U256::ZERO),
            "zero must be a set ceiling, not an unset one",
        );
        assert_eq!(
            policy
                .check_token_amount(Address::ZERO, alloy::primitives::U256::ZERO)
                .expect("a free listing is within a zero ceiling"),
            SpendVerdict::Allowed,
        );
        assert_eq!(
            policy
                .check_token_amount(Address::ZERO, alloy::primitives::U256::from(1u64))
                .expect_err("any price is above a zero ceiling")
                .exit_code(),
            EXIT_PRICE_ABOVE_POLICY,
        );
    }

    /// And the environment is really the source: `from_env` reads the same
    /// variable the help text names.
    #[test]
    fn the_ceiling_is_read_from_the_documented_variable() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let previous = std::env::var(ENV_MAX_TOKEN_AMOUNT).ok();
        std::env::set_var(ENV_MAX_TOKEN_AMOUNT, "42");
        let read = SpendPolicy::from_env();
        match previous {
            Some(value) => std::env::set_var(ENV_MAX_TOKEN_AMOUNT, value),
            None => std::env::remove_var(ENV_MAX_TOKEN_AMOUNT),
        }

        assert_eq!(
            read.expect("a plain integer parses").max_token_amount,
            Some(alloy::primitives::U256::from(42u64)),
        );
    }

    // ── Pre-purchase attestation ─────────────────────────────────────────────

    /// The refusal is legible to a human and parseable by an orchestrator, and
    /// it names the function the pre-filter saw rather than saying only
    /// "unrecognised code".
    #[test]
    fn a_non_canonical_contract_refusal_names_what_it_saw() {
        let err = HeadlessError::NotCanonicalContract {
            contract: "0x00000000000000000000000000000000000000ab".into(),
            refusal: attest::Refusal::Unrecognised(attest::Unrecognised {
                code_len: 4096,
                // `burn(address,uint256)` carries a comma of its own, which is
                // the whole point: 10 of the 30 forbidden signatures do, so a
                // comma-separated list is not recoverable by the orchestrator
                // the detail line exists for.
                exposed: vec!["seize(uint256)", "burn(address,uint256)", "pause()"],
            }),
        };

        let message = err.to_string();
        assert!(message.contains("seize(uint256)"), "{message}");
        assert!(
            message.contains("Nothing was signed"),
            "the message must make it clear no money moved: {message}"
        );

        let detail = err
            .machine_detail()
            .expect("a refusal must carry a detail line");
        assert!(detail.contains("code_bytes=4096"), "{detail}");

        let exposed = detail
            .split("exposed=")
            .nth(1)
            .expect("the detail line names the exposed list");
        assert_eq!(
            exposed.split('|').collect::<Vec<_>>(),
            vec!["seize(uint256)", "burn(address,uint256)", "pause()"],
            "splitting the field on its separator must recover the signatures \
             whole, argument lists included: {detail}"
        );
    }

    /// An empty pre-filter result is reported as `none` rather than as an empty
    /// value, because `exposed=` reads like a truncated line and, worse, like a
    /// clean bill of health. The scan proves nothing either way.
    #[test]
    fn a_refusal_with_nothing_to_name_still_reports_a_parseable_line() {
        let err = HeadlessError::NotCanonicalContract {
            contract: "0x00000000000000000000000000000000000000ab".into(),
            refusal: attest::Refusal::Unrecognised(attest::Unrecognised {
                code_len: 0,
                exposed: vec![],
            }),
        };
        let detail = err.machine_detail().expect("still a detail line");
        assert!(detail.contains("exposed=none"), "{detail}");
        assert!(err.to_string().contains("no contract code"), "{err}");
    }

    /// Canonical code that sells nothing gets its own detail line: the operator
    /// pointed the build at the factory, which is a different mistake from
    /// pointing it at a modified copy, and the fix is different too.
    #[test]
    fn buying_from_the_factory_is_refused_as_the_wrong_address() {
        let err = HeadlessError::NotCanonicalContract {
            contract: "0x00000000000000000000000000000000000000ab".into(),
            refusal: attest::Refusal::NotALicence {
                contract: "Rub3Factory",
                role: attest::Role::Factory,
            },
        };
        assert_eq!(err.exit_code(), EXIT_NOT_CANONICAL_CONTRACT);
        let detail = err.machine_detail().expect("a refusal carries a line");
        assert!(detail.contains("canonical=Rub3Factory"), "{detail}");
        assert!(detail.contains("sells_licences=false"), "{detail}");
    }

    /// Every classified exit code appears in the table the binary prints, so
    /// `--help` and the code cannot drift apart.
    #[test]
    fn the_help_table_documents_every_classified_exit_code() {
        for code in [
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
            EXIT_PURCHASE_UNCONFIRMED,
            EXIT_PRICE_ABOVE_POLICY,
            EXIT_NOT_CANONICAL_CONTRACT,
        ] {
            assert!(
                EXIT_CODE_HELP.contains(&format!("  {code}  ")),
                "exit code {code} is not documented in EXIT_CODE_HELP"
            );
        }
    }

    #[test]
    fn cooldown_detail_reports_blocks_remaining() {
        let err = HeadlessError::CooldownActive {
            token_id: 3,
            blocks_remaining: 42,
        };
        let detail = err
            .machine_detail()
            .expect("cooldown must carry a detail line");
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
        assert!(
            detail.contains("required_covers=price_plus_gas"),
            "{detail}"
        );
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
        assert!(
            !detail.contains("required_covers=price_plus_gas"),
            "{detail}"
        );
        let rendered = err.to_string();
        assert!(rendered.contains("before gas"), "{rendered}");
        assert!(!rendered.contains("price + gas"), "{rendered}");
    }

    /// A node that rejects the transaction for lack of balance reports no
    /// parseable amounts. Emitting zeroes would tell an orchestrator the wallet
    /// needs nothing, so the detail line is omitted instead.
    #[test]
    fn insufficient_funds_with_unknown_amounts_emits_no_detail_line() {
        let err: HeadlessError = crate::tx::TxError::InsufficientFunds(None).into();
        assert_eq!(err.exit_code(), EXIT_INSUFFICIENT_FUNDS);
        assert!(err.machine_detail().is_none(), "{:?}", err.machine_detail());
        let rendered = err.to_string();
        assert!(rendered.contains("insufficient funds"), "{rendered}");
        assert!(
            !rendered.contains('0'),
            "must not imply an amount: {rendered}"
        );
    }

    #[test]
    fn sold_out_detail_reports_supply() {
        let detail = HeadlessError::SoldOut {
            supply_cap: 100,
            minted: 100,
        }
        .machine_detail()
        .unwrap();
        assert!(detail.contains("supply_cap=100"), "{detail}");
        assert!(detail.contains("minted=100"), "{detail}");
    }

    /// A malformed `CONTRACT` constant is a build-time mistake: no retry can
    /// make it parse, so it must not land on the retryable RPC code.
    #[test]
    fn malformed_contract_address_is_terminal() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        assert_ne!(
            err.exit_code(),
            EXIT_RPC,
            "an orchestrator would retry forever"
        );
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
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    /// Two keys share one machine and one `app_id`: a human activated
    /// interactively, or a second agent runs alongside the first. The newest
    /// session on disk belongs to the other key, but each signer still has a
    /// valid session of its own, and going back on-chain for one it already
    /// holds costs gas or a spurious cooldown back-off.
    #[test]
    fn each_wallet_reuses_its_own_session_when_another_key_activated_later() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let app_id = "com.rub3.test.two-wallets";
        let agent = crate::signer::LocalSigner::from_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();
        let other = crate::signer::LocalSigner::from_hex(
            "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
        )
        .unwrap();

        let agent_session = signed_session(app_id, 3, &agent, "2026-01-01T00:00:00+00:00");
        let other_session = signed_session(app_id, 7, &other, "2026-06-01T00:00:00+00:00");
        crate::session_store::save_session(&agent_session).unwrap();
        crate::session_store::save_session(&other_session).unwrap();

        let picked =
            try_session_fast_path(app_id, "http://127.0.0.1:1", Some(agent.address()), None)
                .expect("the agent holds a valid session of its own");
        assert_eq!(picked.token_id, 3);
        assert_eq!(picked.nonce, agent_session.nonce);

        let picked =
            try_session_fast_path(app_id, "http://127.0.0.1:1", Some(other.address()), None)
                .expect("the other key holds one too");
        assert_eq!(picked.token_id, 7);

        // A wallet with nothing cached is still a miss, not someone else's session.
        let stranger = crate::signer::LocalSigner::from_hex(
            "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
        )
        .unwrap();
        assert!(
            try_session_fast_path(app_id, "http://127.0.0.1:1", Some(stranger.address()), None)
                .is_none(),
            "a key with no session must not launch on another key's",
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

    /// A purchase that broadcast but did not confirm may already have spent the
    /// price. It gets its own terminal code, and the detail line must carry the
    /// hash, or an orchestrator cannot resolve the pending transaction before
    /// deciding whether re-running is safe.
    #[test]
    fn unconfirmed_purchase_is_terminal_and_names_its_transaction() {
        let err = HeadlessError::PurchaseUnconfirmed {
            tx_hash: "0xabc123".into(),
            after_secs: 30,
            reason: None,
        };
        assert_eq!(err.exit_code(), EXIT_PURCHASE_UNCONFIRMED);
        assert_ne!(
            err.exit_code(),
            EXIT_ACTIVATION_FAILED,
            "the retryable code would send an orchestrator into a second purchase",
        );
        let detail = err.machine_detail().expect("must carry a detail line");
        assert!(detail.contains("tx_hash=0xabc123"), "{detail}");
        assert!(detail.contains("waited_secs=30"), "{detail}");
        assert!(err.to_string().contains("0xabc123"), "{err}");
    }

    /// The purchase receipt wait has two ways to fail and both spend money:
    /// once the transaction is broadcast, a node that stops answering is no
    /// more retryable than one that answers "not mined yet". Classifying the
    /// transport case as the retryable code sends an orchestrator into a
    /// second purchase of the same license.
    #[test]
    fn every_unconfirmed_purchase_outcome_is_terminal_and_names_its_transaction() {
        let cases = [
            crate::rpc::ReceiptWaitError::Timeout { after_secs: 30 },
            crate::rpc::ReceiptWaitError::Transport {
                after_secs: 30,
                message: "transport error: 502 Bad Gateway".into(),
            },
        ];

        for case in cases {
            let rendered = case.to_string();
            let err = headless::unconfirmed("0xdeadbeef", case);
            assert_eq!(
                err.exit_code(),
                EXIT_PURCHASE_UNCONFIRMED,
                "wrong code for {rendered}",
            );
            assert_ne!(err.exit_code(), EXIT_ACTIVATION_FAILED, "{rendered}");
            let detail = err.machine_detail().expect("must carry a detail line");
            assert!(detail.contains("tx_hash=0xdeadbeef"), "{detail}");
            assert!(detail.contains("waited_secs=30"), "{detail}");
        }
    }

    /// The transport failure has to survive into the message, or the operator
    /// resolving the hash cannot tell a congested chain from a dead endpoint.
    #[test]
    fn an_unconfirmed_purchase_reports_why_polling_ended() {
        let err = headless::unconfirmed(
            "0xdeadbeef",
            crate::rpc::ReceiptWaitError::Transport {
                after_secs: 30,
                message: "transport error: 502 Bad Gateway".into(),
            },
        );
        let rendered = err.to_string();
        assert!(rendered.contains("502 Bad Gateway"), "{rendered}");
        assert!(rendered.contains("retrying blindly"), "{rendered}");
    }

    /// A session file whose signed `token_id` disagrees with the filename it
    /// was loaded from must not satisfy a request for that filename's token.
    #[test]
    fn token_scoped_fast_path_rejects_a_session_for_another_token() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("RUB3_SESSION_DIR", dir.path());

        let app_id = "com.rub3.test.mislabelled-session";
        let signer = crate::signer::LocalSigner::from_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();
        let wallet = signer.address();

        // Signed for token 3, but written where token 7's session belongs.
        let session = signed_session(app_id, 3, &signer, "2026-01-01T00:00:00+00:00");
        let path = crate::session_store::session_path(app_id, 7).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&session).unwrap()).unwrap();

        assert!(
            try_session_fast_path(app_id, "http://127.0.0.1:1", Some(wallet), Some(7)).is_none(),
            "a session issued for token 3 must not stand in for token 7",
        );

        std::env::remove_var("RUB3_SESSION_DIR");
    }

    #[test]
    fn unclassified_failures_have_no_detail_line() {
        assert!(HeadlessError::Rpc("offline".into())
            .machine_detail()
            .is_none());
        assert!(HeadlessError::ActivationFailed("x".into())
            .machine_detail()
            .is_none());
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
