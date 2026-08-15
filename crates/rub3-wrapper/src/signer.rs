//! Signing abstraction for headless (agent) activation.
//!
//! Every interactive flow in the wrapper is built on a hard rule: *the wrapper
//! never holds keys.* It encodes calldata, shows it to a human, and the human's
//! wallet broadcasts. Headless mode cannot honour that literally - an agent
//! holding only a funded key needs the wrapper to sign and broadcast on its
//! behalf. This module is where that capability is contained, and the
//! containment is the design:
//!
//!   * The capability exists only behind the `headless` Cargo feature. A build
//!     without it cannot sign a transaction at all.
//!   * Callers see [`Signer`] - an object-safe trait whose only primitive is
//!     "sign this 32-byte digest". A KMS, HSM, or enclave-backed operator
//!     implements it without any raw key ever entering this process.
//!   * The *only* type in the crate that holds raw key material is
//!     [`LocalSigner`], below. It is the single auditable place.
//!
//! Key material never reaches an observable surface:
//!   * `LocalSigner` has a hand-written [`Debug`] that prints the address and
//!     the source label, never the key. It has no `Display`, no `Serialize`.
//!   * [`SignerError`] carries only fixed strings and a source label; the
//!     underlying hex/keystore errors are deliberately **not** forwarded,
//!     because they can echo fragments of the input.
//!   * Nothing on the load path panics on attacker- or operator-controlled
//!     input, so no key byte can land in a panic payload.
//!   * The decoded key bytes and the keystore password are zeroized as soon as
//!     they have been consumed.
//!   * The wrapped binary is not handed them either: `supervisor::spawn`
//!     strips all four `RUB3_AGENT_*` variables ([`crate::agent_env`]) from the
//!     child's environment before launching it, unconditionally. So the child
//!     is not given the agent credential or the location of one.
//!
//! That last point is containment, not a sandbox. The child runs as the same
//! UID as the wrapper and can read any file that user can read, including the
//! default keystore path `~/.rub3/agent-key.json`. Stripping the variables
//! means the wrapper does not hand the credential over; it does not mean a
//! determined child cannot go looking.
//!
//! # Sources, in order of precedence
//!
//! | Order | Source | Selected by |
//! |---|---|---|
//! | 1 | `RUB3_AGENT_KEY` - raw hex private key | env var set (dev / CI only) |
//! | 2 | Encrypted keystore file (Web3 Secret Storage V3) | `RUB3_AGENT_KEYSTORE` set, or the default path exists |
//! | 3 | Anything else | caller supplies its own `impl Signer` |
//!
//! [`resolve_signer`] implements 1 and 2. Source 3 is the extension point: it
//! is not discoverable, so an operator wiring a KMS constructs their signer and
//! hands it to [`crate::activation::ensure_headless`] directly.

use std::path::{Path, PathBuf};

use alloy::primitives::{Address, B256};
use alloy::signers::local::PrivateKeySigner;
use k256::ecdsa::SigningKey;
use zeroize::Zeroize;

// ── Env / path constants ──────────────────────────────────────────────────────

// Defined in `agent_env`, which is compiled into every build, so the launcher
// can strip exactly what this module reads. Re-exported here because this is
// where a reader looks for them.
pub use crate::agent_env::{
    ENV_AGENT_KEY, ENV_AGENT_KEYSTORE, ENV_AGENT_KEYSTORE_PASSWORD,
    ENV_AGENT_KEYSTORE_PASSWORD_FILE,
};

/// Default keystore location when `RUB3_AGENT_KEYSTORE` is unset:
/// `~/.rub3/agent-key.json`.
pub fn default_keystore_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rub3").join("agent-key.json"))
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Why a signer could not be produced or could not sign.
///
/// Every variant carries fixed text plus, at most, a source label or a
/// filesystem path. No variant can carry key or password bytes - see the
/// module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignerError {
    /// No source was configured: no `RUB3_AGENT_KEY`, no keystore file.
    NoSource,
    /// `RUB3_AGENT_KEY` was set but is not a well-formed 32-byte hex key.
    /// Intentionally does not say *how* it was malformed.
    MalformedKey,
    /// The hex decoded to 32 bytes but is not a valid secp256k1 scalar
    /// (zero, or >= the curve order).
    InvalidKey,
    /// A keystore path was configured but no file exists there.
    KeystoreNotFound(PathBuf),
    /// The keystore file could not be decrypted: wrong password, or the file
    /// is not a valid V3 keystore. The two are not distinguished on purpose.
    KeystoreDecryptFailed,
    /// A keystore was found but no password was supplied.
    KeystorePasswordMissing,
    /// The password file could not be read.
    KeystorePasswordUnreadable(PathBuf),
    /// The signing backend refused or failed. `String` is the backend's own
    /// message - implementors must keep key material out of it.
    Backend(String),
}

impl std::fmt::Display for SignerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignerError::NoSource => write!(
                f,
                "no agent signer configured: set {ENV_AGENT_KEY}, or {ENV_AGENT_KEYSTORE} \
                 pointing at an encrypted keystore"
            ),
            SignerError::MalformedKey => write!(
                f,
                "{ENV_AGENT_KEY} is not a 32-byte hex private key (64 hex chars, `0x` prefix optional)"
            ),
            SignerError::InvalidKey => {
                write!(f, "{ENV_AGENT_KEY} is not a valid secp256k1 private key")
            }
            SignerError::KeystoreNotFound(p) => {
                write!(f, "keystore file not found: {}", p.display())
            }
            SignerError::KeystoreDecryptFailed => write!(
                f,
                "keystore could not be decrypted (wrong password, or not a V3 keystore)"
            ),
            SignerError::KeystorePasswordMissing => write!(
                f,
                "keystore found but no password: set {ENV_AGENT_KEYSTORE_PASSWORD_FILE} \
                 (preferred) or {ENV_AGENT_KEYSTORE_PASSWORD}"
            ),
            SignerError::KeystorePasswordUnreadable(p) => {
                write!(f, "keystore password file unreadable: {}", p.display())
            }
            SignerError::Backend(e) => write!(f, "signer backend error: {e}"),
        }
    }
}

impl std::error::Error for SignerError {}

// ── The trait ─────────────────────────────────────────────────────────────────

/// A source of secp256k1 signatures for one Ethereum address.
///
/// One primitive: sign a 32-byte digest. That is the smallest operation every
/// backend supports - a local key, an AWS KMS asymmetric key, a YubiHSM, a
/// Nitro Enclave - so implementors never have to expose or move key material.
///
/// The wrapper builds every digest it needs from this one call:
///   * session messages, via [`personal_sign`] (applies the EIP-191 prefix);
///   * transaction envelopes, via [`crate::tx`] (signs the EIP-1559 sighash).
///
/// Object-safe on purpose: the flow takes `&dyn Signer`.
///
/// ```no_run
/// # use rub3_wrapper::signer::{Signer, SignerError};
/// # use alloy::primitives::{Address, B256, Signature};
/// struct KmsSigner { address: Address, key_id: String }
///
/// impl Signer for KmsSigner {
///     fn address(&self) -> Address { self.address }
///
///     fn sign_prehash(&self, _hash: B256) -> Result<Signature, SignerError> {
///         // Call out to the KMS. No key material ever enters this process.
///         Err(SignerError::Backend("not wired up".into()))
///     }
///
///     fn source(&self) -> &'static str { "kms" }
/// }
/// ```
pub trait Signer: Send + Sync {
    /// The Ethereum address these signatures recover to.
    fn address(&self) -> Address;

    /// Signs a 32-byte digest that has already had any domain prefix applied.
    ///
    /// Implementors must return a low-`s` normalised signature with a correct
    /// y-parity, i.e. one from which [`Self::address`] is recoverable.
    fn sign_prehash(&self, hash: B256) -> Result<alloy::primitives::Signature, SignerError>;

    /// Short, non-sensitive label naming the backend. Appears in logs.
    fn source(&self) -> &'static str;
}

/// Signs `preimage` the way a wallet's `personal_sign` would, and returns the
/// 65-byte `r || s || v` signature as 0x-hex with `v ∈ {27, 28}`.
///
/// This is the exact shape [`crate::session::verify_local`] expects, so a
/// headless session verifies through the same code path as a webview one.
pub fn personal_sign(signer: &dyn Signer, preimage: &[u8; 32]) -> Result<String, SignerError> {
    let digest = crate::license::personal_sign_hash(preimage);
    let sig = signer.sign_prehash(B256::from(digest))?;

    Ok(format!("0x{}", hex::encode(sig.as_bytes())))
}

// ── Local signer - the only holder of raw key material ────────────────────────

/// A signer backed by a secp256k1 private key held in this process.
///
/// **The one place in the crate that touches raw key material.** Everything
/// else in the headless path works through [`Signer`], so auditing key handling
/// means auditing this type and its three constructors.
///
/// `SigningKey` zeroizes its scalar on drop (`elliptic-curve`'s `SecretKey` is
/// `ZeroizeOnDrop`); the constructors below zeroize the intermediate hex and
/// password buffers they create.
pub struct LocalSigner {
    key: SigningKey,
    address: Address,
    source: &'static str,
}

// Hand-written: the derived impl would print the key. Also blocks the accidental
// `{:?}` in a log line or an `unwrap()` panic payload from leaking it.
impl std::fmt::Debug for LocalSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSigner")
            .field("address", &self.address)
            .field("source", &self.source)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl LocalSigner {
    /// Builds a signer from a raw hex private key.
    ///
    /// Accepts 64 hex chars with or without a `0x` prefix. `hex` is copied into
    /// a local buffer that is zeroized before returning, on every path.
    ///
    /// `pub(crate)` on purpose: raw-key entry points stay inside the crate, so
    /// the only ways in from outside are an env var, a keystore file, or a
    /// caller-supplied [`Signer`] implementation.
    pub(crate) fn from_hex(hex_key: &str) -> Result<Self, SignerError> {
        let trimmed = hex_key.trim();
        let trimmed = trimmed.strip_prefix("0x").unwrap_or(trimmed);

        if trimmed.len() != 64 {
            return Err(SignerError::MalformedKey);
        }

        // `hex::decode`'s error names the offending character - never surface it.
        let mut bytes = match hex::decode(trimmed) {
            Ok(b) => b,
            Err(_) => return Err(SignerError::MalformedKey),
        };

        let parsed = SigningKey::from_slice(&bytes);
        bytes.zeroize();

        let key = parsed.map_err(|_| SignerError::InvalidKey)?;
        Ok(Self::from_signing_key(key, "env"))
    }

    /// Builds a signer from `RUB3_AGENT_KEY`.
    ///
    /// Returns [`SignerError::NoSource`] when the variable is unset or empty.
    /// The value read from the environment is zeroized after use - though note
    /// the OS copy in `environ` is outside our reach, which is why this source
    /// is documented as dev/CI only.
    pub fn from_env() -> Result<Self, SignerError> {
        let mut raw = match std::env::var(ENV_AGENT_KEY) {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return Err(SignerError::NoSource),
        };
        let result = Self::from_hex(&raw);
        raw.zeroize();
        result
    }

    /// Builds a signer by decrypting a Web3 Secret Storage (V3) keystore.
    ///
    /// Decryption failure is reported as a single opaque
    /// [`SignerError::KeystoreDecryptFailed`]: a wrong password and a corrupt
    /// file are indistinguishable to the caller, and the underlying error is
    /// dropped rather than forwarded.
    pub fn from_keystore(path: &Path, password: &str) -> Result<Self, SignerError> {
        if !path.exists() {
            return Err(SignerError::KeystoreNotFound(path.to_path_buf()));
        }
        let decrypted = PrivateKeySigner::decrypt_keystore(path, password)
            .map_err(|_| SignerError::KeystoreDecryptFailed)?;
        Ok(Self::from_signing_key(
            decrypted.into_credential(),
            "keystore",
        ))
    }

    fn from_signing_key(key: SigningKey, source: &'static str) -> Self {
        let address = address_of(&key);
        Self {
            key,
            address,
            source,
        }
    }
}

impl Signer for LocalSigner {
    fn address(&self) -> Address {
        self.address
    }

    fn sign_prehash(&self, hash: B256) -> Result<alloy::primitives::Signature, SignerError> {
        use k256::ecdsa::signature::hazmat::PrehashSigner;
        use k256::ecdsa::{RecoveryId, Signature as K256Signature};

        let (sig, recid): (K256Signature, RecoveryId) = self
            .key
            .sign_prehash(hash.as_slice())
            .map_err(|_| SignerError::Backend("secp256k1 signing failed".into()))?;

        let r = alloy::primitives::U256::from_be_slice(&sig.r().to_bytes());
        let s = alloy::primitives::U256::from_be_slice(&sig.s().to_bytes());
        Ok(alloy::primitives::Signature::new(r, s, recid.is_y_odd()))
    }

    fn source(&self) -> &'static str {
        self.source
    }
}

/// Derives the Ethereum address for a signing key.
fn address_of(key: &SigningKey) -> Address {
    use sha3::{Digest, Keccak256};
    let encoded = key.verifying_key().to_encoded_point(false);
    // Uncompressed encoding is 0x04 || x || y; Keccak is taken over x || y.
    let hash = Keccak256::digest(&encoded.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

// ── Resolution ────────────────────────────────────────────────────────────────

/// Resolves a signer from the environment, in the documented precedence order:
/// `RUB3_AGENT_KEY`, then an encrypted keystore file.
///
/// Precedence is strict: when `RUB3_AGENT_KEY` is set, a keystore is never
/// read - even if the key turns out to be malformed. An operator who sets both
/// should get a hard error about the one they set most recently, not a silent
/// fall-through to the other identity.
///
/// Callers with a KMS/enclave backend skip this entirely and pass their own
/// `impl Signer`.
pub fn resolve_signer() -> Result<Box<dyn Signer>, SignerError> {
    match LocalSigner::from_env() {
        Ok(s) => return Ok(Box::new(s)),
        // Fall through to the keystore only when the env var was absent.
        Err(SignerError::NoSource) => {}
        Err(e) => return Err(e),
    }

    let explicit = std::env::var(ENV_AGENT_KEYSTORE)
        .ok()
        .filter(|p| !p.trim().is_empty());
    let path = match explicit {
        Some(p) => PathBuf::from(p),
        None => match default_keystore_path() {
            // The default path is a convenience, not a configuration: if
            // nothing is there, the operator simply has no signer configured.
            Some(p) if p.exists() => p,
            _ => return Err(SignerError::NoSource),
        },
    };

    let mut password = read_keystore_password()?;
    let result = LocalSigner::from_keystore(&path, &password);
    password.zeroize();
    Ok(Box::new(result?))
}

/// Reads the keystore password from the password file, else the inline env var.
fn read_keystore_password() -> Result<String, SignerError> {
    if let Some(file) = std::env::var(ENV_AGENT_KEYSTORE_PASSWORD_FILE)
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        let path = PathBuf::from(file);
        let mut contents = std::fs::read_to_string(&path)
            .map_err(|_| SignerError::KeystorePasswordUnreadable(path))?;
        // Trailing newline from `echo`/heredoc is not part of the password.
        let password = contents.trim_end_matches(['\n', '\r']).to_string();
        // The file buffer is a second plaintext copy; it dies here, not on a
        // later allocator reuse.
        contents.zeroize();
        return Ok(password);
    }

    match std::env::var(ENV_AGENT_KEYSTORE_PASSWORD) {
        Ok(p) if !p.is_empty() => Ok(p),
        _ => Err(SignerError::KeystorePasswordMissing),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    // Anvil account #0 - deterministic, documented, holds nothing real.
    const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const ANVIL_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    /// Clears every signer env var so each test starts from a known state.
    ///
    /// Takes the crate-wide lock, not a local one: these tests share a binary
    /// with every other test that reads the environment.
    fn env_guard() -> MutexGuard<'static, ()> {
        let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for k in [
            ENV_AGENT_KEY,
            ENV_AGENT_KEYSTORE,
            ENV_AGENT_KEYSTORE_PASSWORD,
            ENV_AGENT_KEYSTORE_PASSWORD_FILE,
        ] {
            std::env::remove_var(k);
        }
        guard
    }

    /// `Box<dyn Signer>` is deliberately not `Debug` - the trait must stay
    /// implementable by backends that would rather not describe themselves -
    /// so `unwrap_err()` is unavailable here.
    fn resolve_err() -> SignerError {
        match resolve_signer() {
            Ok(s) => panic!("expected no signer, got one for {}", s.address()),
            Err(e) => e,
        }
    }

    fn write_keystore(dir: &Path, password: &str) -> (PathBuf, Address) {
        use rand::rngs::OsRng;
        let mut rng = OsRng;
        let key = SigningKey::random(&mut rng);
        let address = address_of(&key);
        // `encrypt_keystore` returns the keystore's UUID, not its filename -
        // the file lands under the name we asked for.
        const NAME: &str = "agent-key.json";
        PrivateKeySigner::encrypt_keystore(dir, &mut rng, key.to_bytes(), password, Some(NAME))
            .expect("encrypt_keystore");
        (dir.join(NAME), address)
    }

    // ── from_hex ─────────────────────────────────────────────────────────────

    #[test]
    fn from_hex_accepts_prefixed_and_bare() {
        let expected: Address = ANVIL_ADDR.parse().unwrap();
        assert_eq!(
            LocalSigner::from_hex(ANVIL_KEY).unwrap().address(),
            expected
        );
        assert_eq!(
            LocalSigner::from_hex(ANVIL_KEY.trim_start_matches("0x"))
                .unwrap()
                .address(),
            expected
        );
    }

    #[test]
    fn from_hex_tolerates_surrounding_whitespace() {
        let padded = format!("  {ANVIL_KEY}\n");
        assert_eq!(
            LocalSigner::from_hex(&padded).unwrap().address(),
            ANVIL_ADDR.parse::<Address>().unwrap()
        );
    }

    #[test]
    fn from_hex_rejects_wrong_length() {
        assert_eq!(
            LocalSigner::from_hex("0xdeadbeef").unwrap_err(),
            SignerError::MalformedKey
        );
        assert_eq!(
            LocalSigner::from_hex(&format!("{ANVIL_KEY}ff")).unwrap_err(),
            SignerError::MalformedKey
        );
    }

    #[test]
    fn from_hex_rejects_non_hex() {
        let bad = "z".repeat(64);
        assert_eq!(
            LocalSigner::from_hex(&bad).unwrap_err(),
            SignerError::MalformedKey
        );
    }

    #[test]
    fn from_hex_rejects_out_of_range_scalar() {
        // All-zero is not a valid secp256k1 scalar.
        assert_eq!(
            LocalSigner::from_hex(&"0".repeat(64)).unwrap_err(),
            SignerError::InvalidKey
        );
        // Above the curve order.
        assert_eq!(
            LocalSigner::from_hex(&"f".repeat(64)).unwrap_err(),
            SignerError::InvalidKey
        );
    }

    /// The whole point of the redacted `Debug`: `{:?}` must never widen into a
    /// key leak, including through a panic payload.
    #[test]
    fn debug_never_prints_key_material() {
        let signer = LocalSigner::from_hex(ANVIL_KEY).unwrap();
        let rendered = format!("{signer:?}");
        assert!(
            !rendered.contains("ac0974"),
            "debug leaked key bytes: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        assert!(
            rendered.to_lowercase().contains("f39fd6"),
            "debug should name the address"
        );
    }

    /// Error messages are the other surface that could echo a key. None of the
    /// load-path errors may contain any fragment of the input.
    #[test]
    fn errors_never_echo_key_material() {
        let secret = format!("{}zz", &"ab".repeat(31)); // 64 chars, invalid hex
        let rendered = LocalSigner::from_hex(&secret).unwrap_err().to_string();
        assert!(!rendered.contains("ab"), "error echoed input: {rendered}");
        assert!(!rendered.contains("zz"), "error echoed input: {rendered}");
    }

    // ── Signing ──────────────────────────────────────────────────────────────

    #[test]
    fn personal_sign_round_trips_through_verify() {
        let signer = LocalSigner::from_hex(ANVIL_KEY).unwrap();
        let preimage = [7u8; 32];
        let sig = personal_sign(&signer, &preimage).unwrap();

        assert!(sig.starts_with("0x"));
        assert_eq!(sig.len(), 2 + 130, "65-byte signature expected");

        let recovered = crate::license::recover_address(&preimage, &sig).unwrap();
        assert!(recovered.eq_ignore_ascii_case(ANVIL_ADDR));
    }

    #[test]
    fn sign_prehash_recovers_signer_address() {
        let signer = LocalSigner::from_hex(ANVIL_KEY).unwrap();
        let digest = B256::from([3u8; 32]);
        let sig = signer.sign_prehash(digest).unwrap();
        assert_eq!(
            sig.recover_address_from_prehash(&digest).unwrap(),
            signer.address()
        );
    }

    #[test]
    fn signing_is_deterministic() {
        // RFC-6979 - same key, same digest, same signature. Guards against a
        // backend swap silently introducing randomness into the tx path.
        let signer = LocalSigner::from_hex(ANVIL_KEY).unwrap();
        let d = B256::from([9u8; 32]);
        assert_eq!(
            signer.sign_prehash(d).unwrap(),
            signer.sign_prehash(d).unwrap()
        );
    }

    // ── Source selection ─────────────────────────────────────────────────────

    #[test]
    fn from_env_missing_is_no_source() {
        let _g = env_guard();
        assert_eq!(LocalSigner::from_env().unwrap_err(), SignerError::NoSource);
    }

    #[test]
    fn from_env_empty_is_no_source() {
        let _g = env_guard();
        std::env::set_var(ENV_AGENT_KEY, "   ");
        assert_eq!(LocalSigner::from_env().unwrap_err(), SignerError::NoSource);
        std::env::remove_var(ENV_AGENT_KEY);
    }

    #[test]
    fn resolve_prefers_env_over_keystore() {
        let _g = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let (path, keystore_addr) = write_keystore(dir.path(), "hunter2");

        std::env::set_var(ENV_AGENT_KEY, ANVIL_KEY);
        std::env::set_var(ENV_AGENT_KEYSTORE, &path);
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD, "hunter2");

        let signer = resolve_signer().unwrap();
        assert_eq!(signer.address(), ANVIL_ADDR.parse::<Address>().unwrap());
        assert_ne!(signer.address(), keystore_addr);
        assert_eq!(signer.source(), "env");

        for k in [
            ENV_AGENT_KEY,
            ENV_AGENT_KEYSTORE,
            ENV_AGENT_KEYSTORE_PASSWORD,
        ] {
            std::env::remove_var(k);
        }
    }

    /// A malformed `RUB3_AGENT_KEY` must be a hard error, not a silent
    /// fall-through to whatever keystore happens to be lying around - that
    /// would activate under the wrong identity.
    #[test]
    fn resolve_does_not_fall_through_on_malformed_env_key() {
        let _g = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = write_keystore(dir.path(), "hunter2");

        std::env::set_var(ENV_AGENT_KEY, "0xnope");
        std::env::set_var(ENV_AGENT_KEYSTORE, &path);
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD, "hunter2");

        assert_eq!(resolve_err(), SignerError::MalformedKey);

        for k in [
            ENV_AGENT_KEY,
            ENV_AGENT_KEYSTORE,
            ENV_AGENT_KEYSTORE_PASSWORD,
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn resolve_uses_keystore_when_env_key_absent() {
        let _g = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let (path, addr) = write_keystore(dir.path(), "hunter2");

        std::env::set_var(ENV_AGENT_KEYSTORE, &path);
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD, "hunter2");

        let signer = resolve_signer().unwrap();
        assert_eq!(signer.address(), addr);
        assert_eq!(signer.source(), "keystore");

        for k in [ENV_AGENT_KEYSTORE, ENV_AGENT_KEYSTORE_PASSWORD] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn resolve_reads_password_from_file_in_preference_to_env() {
        let _g = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let (path, addr) = write_keystore(dir.path(), "correct-horse");

        let pw_file = dir.path().join("pw.txt");
        // Trailing newline is the common case (`echo pw > file`) and must not
        // become part of the password.
        std::fs::write(&pw_file, "correct-horse\n").unwrap();

        std::env::set_var(ENV_AGENT_KEYSTORE, &path);
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD, "wrong");
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD_FILE, &pw_file);

        assert_eq!(resolve_signer().unwrap().address(), addr);

        for k in [
            ENV_AGENT_KEYSTORE,
            ENV_AGENT_KEYSTORE_PASSWORD,
            ENV_AGENT_KEYSTORE_PASSWORD_FILE,
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn keystore_wrong_password_is_opaque() {
        let _g = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = write_keystore(dir.path(), "hunter2");

        let err = LocalSigner::from_keystore(&path, "not-the-password").unwrap_err();
        assert_eq!(err, SignerError::KeystoreDecryptFailed);
        let rendered = err.to_string();
        assert!(
            !rendered.contains("not-the-password"),
            "error echoed password: {rendered}"
        );
    }

    #[test]
    fn keystore_missing_file_reports_path() {
        let _g = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.json");
        assert_eq!(
            LocalSigner::from_keystore(&missing, "x").unwrap_err(),
            SignerError::KeystoreNotFound(missing)
        );
    }

    #[test]
    fn resolve_keystore_without_password_reports_missing_password() {
        let _g = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = write_keystore(dir.path(), "hunter2");

        std::env::set_var(ENV_AGENT_KEYSTORE, &path);
        assert_eq!(resolve_err(), SignerError::KeystorePasswordMissing);
        std::env::remove_var(ENV_AGENT_KEYSTORE);
    }

    #[test]
    fn resolve_with_nothing_configured_is_no_source() {
        let _g = env_guard();
        // Point the keystore at a path that certainly does not exist so the
        // developer's real `~/.rub3/agent-key.json`, if any, cannot be picked up.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(ENV_AGENT_KEYSTORE, dir.path().join("absent.json"));
        std::env::set_var(ENV_AGENT_KEYSTORE_PASSWORD, "x");
        assert_eq!(
            resolve_err(),
            SignerError::KeystoreNotFound(dir.path().join("absent.json"))
        );
        for k in [ENV_AGENT_KEYSTORE, ENV_AGENT_KEYSTORE_PASSWORD] {
            std::env::remove_var(k);
        }
    }
}
