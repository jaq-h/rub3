//! The session an application is allowed to see, and the two newtypes that keep
//! its identity fields from being confused for each other.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Who is running this application, according to the wrapper that launched it.
///
/// Exactly the six fields `implementation.md` §3.5 names, and deliberately no
/// more. The wrapper holds a good deal else - the session signature, its nonce,
/// the activation transaction, the device public key - and none of it is here:
/// an application that could read the signature could replay the session
/// somewhere the wrapper never launched it, and an application that never
/// receives it cannot leak it either.
///
/// [`SessionInfo::user_id`] is the key. See the crate documentation for why, and
/// [`Wallet`] for what stops the other field being used as one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Reverse-DNS identifier of the application the licence is for.
    pub app_id: String,

    /// The licence NFT's token id.
    pub token_id: u64,

    /// **The identity key.** Stable across a wallet change under the account
    /// model, and the only field a persistent row should hang off.
    pub user_id: UserId,

    /// The wallet that signed this session. Incidental: it can change hands
    /// while the licence stays the same. Display and on-chain use only.
    pub wallet: Wallet,

    /// Which identity model the licence contract declares.
    pub identity: Identity,

    /// When the session stops being valid, or `None` when it carries no TTL
    /// (tier 4 replaces the TTL with a device challenge).
    pub expires_at: Option<DateTime<Utc>>,
}

impl SessionInfo {
    /// Whether the session's TTL has passed.
    ///
    /// A session with no `expires_at` never expires, so this is `false` for it.
    ///
    /// The wrapper checked this before it launched the application, so a fresh
    /// call is about a long-running process outliving its own session, not
    /// about deciding whether to start.
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            None => false,
            Some(exp) => Utc::now() >= exp,
        }
    }
}

/// The licence identity: the wallet address under the access model, the ERC-6551
/// token-bound account address under the account model.
///
/// **Key every persistent row on this.** It is what the licence grants, so it
/// survives the licence being transferred to a different wallet, and it is the
/// one identifier an application can store without its user's data being
/// orphaned by an ordinary key rotation or resale.
///
/// It carries the traits that make that the easy thing to do - `Hash`, `Eq`,
/// `Ord`, `Display`, `AsRef<str>` - so it drops straight into a `HashMap` key,
/// a `BTreeMap` key, a sort, or a formatted path segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(String);

impl UserId {
    /// Wraps an identity string. The wrapper builds these; an application
    /// normally reads [`SessionInfo::user_id`] rather than constructing one.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for UserId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The wallet address that signed the current session.
///
/// **Not an identity.** A licence NFT can be sold, gifted, or moved to a fresh
/// key at any time, and the wallet on the next session will be a different
/// address for the same licence and the same user.
///
/// The traits are the guard rail rather than the doc comment: `Wallet`
/// implements neither `Hash` nor `Eq` nor `Ord`, so it cannot be a `HashMap` or
/// `BTreeMap` key, cannot be compared with `==`, and cannot be sorted. Every
/// use that would key data off it stops at the type checker. What is left is
/// what a wallet is legitimately for - showing it to a human, and naming an
/// address to the chain - both of which go through `Display`:
///
/// ```
/// # use rub3::Wallet;
/// # let wallet = Wallet::new("0x00000000000000000000000000000000000000aa");
/// println!("signed by {wallet}");
/// let for_rpc: String = wallet.to_string();
/// ```
///
/// Keying on it does not compile:
///
/// ```compile_fail
/// # use rub3::Wallet;
/// fn store(wallet: Wallet) {
///     let mut rows = std::collections::HashMap::new();
///     rows.insert(wallet, "some row");   // Wallet: !Hash
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Wallet(String);

impl Wallet {
    /// Wraps a wallet address. The wrapper builds these; an application
    /// normally reads [`SessionInfo::wallet`] rather than constructing one.
    pub fn new(address: impl Into<String>) -> Self {
        Self(address.into())
    }
}

impl fmt::Display for Wallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which identity model the licence contract declares.
///
/// An application does not usually need to branch on this: `user_id` already
/// resolves to the right address for the model in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Identity {
    /// `user_id` is the holder's wallet address. The NFT gates access.
    Access,
    /// `user_id` is the token's ERC-6551 account address. The NFT *is* the
    /// account, and the identity outlives any one wallet holding it.
    Account,
    /// A model this build of the SDK does not know, carried through verbatim.
    ///
    /// The wrapper and the application are packaged separately and can be
    /// built from different revisions, so an unknown model is a version skew
    /// rather than corruption: reporting it beats failing to deserialize the
    /// whole session over a field most applications never read.
    Other(String),
}

impl Identity {
    /// The wire string, as the wrapper's session schema stores it.
    pub fn as_str(&self) -> &str {
        match self {
            Identity::Access => "access",
            Identity::Account => "account",
            Identity::Other(s) => s,
        }
    }
}

impl From<String> for Identity {
    fn from(s: String) -> Self {
        match s.as_str() {
            "access" => Identity::Access,
            "account" => Identity::Account,
            _ => Identity::Other(s),
        }
    }
}

impl From<Identity> for String {
    fn from(i: Identity) -> String {
        match i {
            Identity::Other(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
