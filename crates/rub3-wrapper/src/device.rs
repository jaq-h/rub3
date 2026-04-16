//! Device keypair management for tier 4 (hardened).
//!
//! Generates an ephemeral secp256k1 keypair at activation. The public key is
//! registered on-chain via `activateDevice(tokenId, devicePubKey)`. The private
//! key is stored locally — via file, OS keychain, or hardware enclave per the
//! developer's `device_key_storage` config.
//!
//! At every launch the wrapper signs the current block hash with the device
//! private key and verifies against the on-chain pubkey.

#![allow(dead_code)]

#[derive(Debug, Clone, Copy)]
pub enum StorageBackend {
    /// Plain file at `~/.rub3/devices/<app_id>/<token_id>.key`.
    /// Easiest to reason about but extractable with file access.
    File,
    /// OS keychain (macOS Keychain / Windows DPAPI / Linux Secret Service)
    /// via the `keyring` crate. Extractable with the user's OS password.
    Keychain,
    /// Hardware-backed key storage. macOS Secure Enclave or Windows TPM.
    /// Non-extractable — signing happens inside the secure chip.
    Enclave,
}

#[derive(Debug)]
pub enum DeviceError {
    Generate(String),
    Store(String),
    Load(String),
    Sign(String),
    BackendUnsupported(StorageBackend),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::Generate(e) => write!(f, "keypair generation failed: {e}"),
            DeviceError::Store(e) => write!(f, "key storage failed: {e}"),
            DeviceError::Load(e) => write!(f, "key load failed: {e}"),
            DeviceError::Sign(e) => write!(f, "signing failed: {e}"),
            DeviceError::BackendUnsupported(b) => {
                write!(f, "storage backend not supported on this platform: {b:?}")
            }
        }
    }
}

/// Generates a fresh secp256k1 keypair, stores the private key via `backend`,
/// and returns the compressed 33-byte public key (hex-encoded).
pub fn generate_and_store(
    _app_id: &str,
    _token_id: u64,
    _backend: StorageBackend,
) -> Result<String, DeviceError> {
    unimplemented!("generate_and_store: scaffold only")
}

/// Signs `message` with the device private key for `(app_id, token_id)`.
/// Returns a 65-byte ECDSA signature (r || s || v) hex-encoded.
pub fn sign_challenge(
    _app_id: &str,
    _token_id: u64,
    _backend: StorageBackend,
    _message: &[u8; 32],
) -> Result<String, DeviceError> {
    unimplemented!("sign_challenge: scaffold only")
}

/// Verifies that `signature` over `message` recovers to `expected_pubkey`.
/// Used to check the device signature against the on-chain registered pubkey.
pub fn verify_signature(
    _message: &[u8; 32],
    _signature: &str,
    _expected_pubkey: &str,
) -> Result<bool, DeviceError> {
    unimplemented!("verify_signature: scaffold only")
}
