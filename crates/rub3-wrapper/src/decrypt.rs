//! Binary decryption (tiers 3-4, when `encrypt_binary = true`).
//!
//! The app binary is packed as an AES-256-GCM ciphertext. The AEK (app
//! encryption key) is wrapped with a KEK derived from on-chain state:
//!
//!   tier 3: KEK = SHA-256(contract_address || chain_id || salt)
//!   tier 4: KEK = SHA-256(contract_address || chain_id || salt || device_fingerprint)
//!
//! The wrapped AEK and its nonce/salt are embedded alongside the ciphertext.
//! The AEK's SHA-256 hash is recorded on-chain as `encryptedBinaryKeyHash` so
//! the wrapper can verify the unwrap produced the correct key.
//!
//! After decryption, the plaintext is executed directly from memory (never
//! written to persistent storage) and zeroed once the child has mapped it.

#![allow(dead_code)]

#[derive(Debug)]
pub enum DecryptError {
    KekDerivation(String),
    Unwrap(String),
    KeyHashMismatch,
    Decrypt(String),
    ExecFailed(String),
}

impl std::fmt::Display for DecryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecryptError::KekDerivation(e) => write!(f, "KEK derivation failed: {e}"),
            DecryptError::Unwrap(e) => write!(f, "AEK unwrap failed: {e}"),
            DecryptError::KeyHashMismatch => {
                write!(f, "unwrapped AEK hash does not match on-chain value")
            }
            DecryptError::Decrypt(e) => write!(f, "ciphertext decryption failed: {e}"),
            DecryptError::ExecFailed(e) => write!(f, "in-memory exec failed: {e}"),
        }
    }
}

/// Derives the KEK from on-chain state. Tier 3 passes `device_fingerprint = None`,
/// tier 4 passes the device key fingerprint to bind decryption to this device.
pub fn derive_kek(
    _contract_address: &str,
    _chain_id: u64,
    _salt: &[u8],
    _device_fingerprint: Option<&[u8]>,
) -> Result<[u8; 32], DecryptError> {
    unimplemented!("derive_kek: scaffold only")
}

/// Unwraps the AEK using the KEK, then verifies `SHA-256(aek)` matches the
/// `expected_hash` pulled from the contract's `encryptedBinaryKeyHash`.
pub fn unwrap_aek(
    _wrapped_aek: &[u8],
    _kek: &[u8; 32],
    _expected_hash: &[u8; 32],
) -> Result<[u8; 32], DecryptError> {
    unimplemented!("unwrap_aek: scaffold only")
}

/// Decrypts the embedded ciphertext with AES-256-GCM into an owned Vec.
/// Caller is responsible for zeroing the returned buffer after exec.
pub fn decrypt_binary(
    _ciphertext: &[u8],
    _aek: &[u8; 32],
    _nonce: &[u8; 12],
) -> Result<Vec<u8>, DecryptError> {
    unimplemented!("decrypt_binary: scaffold only")
}

/// Executes `plaintext` in-memory, never writing to persistent storage.
///
/// Platform behaviour:
///   Linux:   memfd_create + write + fexecve
///   macOS:   write to $TMPDIR with 0700, exec, unlink before child starts
///   Windows: CreateFileMapping(INVALID_HANDLE_VALUE) + section mapping
pub fn exec_in_memory(_plaintext: &[u8], _args: &[String]) -> Result<i32, DecryptError> {
    unimplemented!("exec_in_memory: scaffold only")
}
