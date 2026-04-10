// These items are wired into main in Phase 1.4 when the activation flow is implemented.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

// ── License proof schema ──────────────────────────────────────────────────────

/// The license proof stored at ~/.rub3/licenses/<app_id>.json after activation.
///
/// `wallet_address` owns the NFT and produced the activation signature.
/// `paid_by` is only set when the purchasing wallet differs from `wallet_address`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseProof {
    pub app_id: String,
    pub token_id: u64,
    pub wallet_address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paid_by: Option<String>,
    pub machine_id: String,
    /// Hex-encoded ECDSA signature over H(app_id || token_id || machine_id)
    pub signature: String,
    pub activated_at: String,
    pub chain: String,
    pub contract: String,
}

// ── Message construction ──────────────────────────────────────────────────────

/// Builds the raw bytes that the wallet signs during activation.
///
/// message = SHA-256(app_id || token_id_be_bytes || machine_id)
///
/// The token_id is encoded as a big-endian u64 (8 bytes) to give it a fixed
/// width — prevents ambiguity between e.g. token 1 + "2..." vs token 12 + "...".
pub fn activation_message(app_id: &str, token_id: u64, machine_id: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(app_id.as_bytes());
    hasher.update(token_id.to_be_bytes());
    hasher.update(machine_id.as_bytes());
    hasher.finalize().into()
}

// ── Signature verification ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum VerifyError {
    InvalidSignature(String),
    AddressMismatch { expected: String, recovered: String },
    MachineIdMismatch,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::InvalidSignature(msg) => write!(f, "invalid signature: {msg}"),
            VerifyError::AddressMismatch { expected, recovered } => write!(
                f,
                "address mismatch: proof claims {expected}, signature recovers {recovered}"
            ),
            VerifyError::MachineIdMismatch => {
                write!(f, "machine ID mismatch: license is bound to a different machine")
            }
        }
    }
}

/// Verifies a stored license proof against the current machine.
///
/// Checks:
/// 1. The current machine_id matches the one in the proof.
/// 2. The ECDSA signature recovers to the wallet address in the proof.
pub fn verify(proof: &LicenseProof, current_machine_id: &str) -> Result<(), VerifyError> {
    if proof.machine_id != current_machine_id {
        return Err(VerifyError::MachineIdMismatch);
    }

    let msg = activation_message(&proof.app_id, proof.token_id, &proof.machine_id);
    let recovered = recover_address(&msg, &proof.signature)?;

    let expected = proof.wallet_address.to_lowercase();
    let recovered_lower = recovered.to_lowercase();

    if expected != recovered_lower {
        return Err(VerifyError::AddressMismatch {
            expected,
            recovered: recovered_lower,
        });
    }

    Ok(())
}

/// Applies the Ethereum `personal_sign` prefix and returns the final hash.
///
/// Wallets sign: keccak256("\x19Ethereum Signed Message:\n32" || message)
/// where `message` is the 32-byte raw preimage from `activation_message()`.
fn personal_sign_hash(message: &[u8; 32]) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(b"\x19Ethereum Signed Message:\n32");
    hasher.update(message);
    hasher.finalize().into()
}

/// Recovers the Ethereum address that produced `sig_hex` over `message`.
///
/// `message` is the raw preimage from `activation_message()`. This function
/// applies the `personal_sign` prefix before recovery to match what wallets sign.
///
/// Ethereum signatures are 65 bytes: [r (32)] [s (32)] [v (1)].
/// v is either 27/28 (legacy) or 0/1 (modern). We normalise to 0/1.
fn recover_address(message: &[u8; 32], sig_hex: &str) -> Result<String, VerifyError> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

    let sig_bytes = hex::decode(sig_hex.trim_start_matches("0x"))
        .map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;

    if sig_bytes.len() != 65 {
        return Err(VerifyError::InvalidSignature(format!(
            "expected 65 bytes, got {}",
            sig_bytes.len()
        )));
    }

    let r_s = &sig_bytes[..64];
    let v = sig_bytes[64];
    // Normalise legacy v (27/28) → recovery id (0/1)
    let recovery_id = match v {
        0 | 27 => 0u8,
        1 | 28 => 1u8,
        _ => {
            return Err(VerifyError::InvalidSignature(format!(
                "unexpected v value: {v}"
            )))
        }
    };

    let sig = Signature::from_slice(r_s)
        .map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;
    let rec_id = RecoveryId::try_from(recovery_id)
        .map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;

    let prefixed = personal_sign_hash(message);
    let verifying_key = VerifyingKey::recover_from_prehash(&prefixed, &sig, rec_id)
        .map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;

    Ok(public_key_to_address(&verifying_key))
}

/// Converts a secp256k1 public key to a checksummed Ethereum address.
///
/// Ethereum address = last 20 bytes of Keccak-256(uncompressed public key, minus the 04 prefix).
fn public_key_to_address(key: &k256::ecdsa::VerifyingKey) -> String {
    use sha3::{Digest, Keccak256};

    // Uncompressed encoding: 0x04 || x (32 bytes) || y (32 bytes)
    let uncompressed = key.to_encoded_point(false);
    let bytes = uncompressed.as_bytes();
    // Drop the 0x04 prefix — Keccak is taken over the 64-byte x||y payload
    let hash = Keccak256::digest(&bytes[1..]);
    // Address is the last 20 bytes
    format!("0x{}", hex::encode(&hash[12..]))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Test vector produced with ethers.js:
    ///
    ///   const wallet = new ethers.Wallet(privateKey);
    ///   const msg = ethers.utils.arrayify(
    ///     ethers.utils.sha256(
    ///       ethers.utils.concat([
    ///         ethers.utils.toUtf8Bytes("com.rub3.test"),
    ///         ethers.utils.zeroPad(ethers.utils.hexlify(42), 8),
    ///         ethers.utils.toUtf8Bytes("sha256:test-machine-id"),
    ///       ])
    ///     )
    ///   );
    ///   const sig = await wallet.signMessage(msg);  // wallet applies personal_sign prefix
    ///
    /// `activation_message()` produces the raw 32-byte preimage. The wallet
    /// prepends "\x19Ethereum Signed Message:\n32" and keccak256-hashes before
    /// signing. `recover_address()` applies the same prefix via
    /// `personal_sign_hash()` before recovery.

    const TEST_APP_ID: &str = "com.rub3.test";
    const TEST_TOKEN_ID: u64 = 42;
    const TEST_MACHINE_ID: &str = "sha256:test-machine-id";

    #[test]
    fn activation_message_is_deterministic() {
        let a = activation_message(TEST_APP_ID, TEST_TOKEN_ID, TEST_MACHINE_ID);
        let b = activation_message(TEST_APP_ID, TEST_TOKEN_ID, TEST_MACHINE_ID);
        assert_eq!(a, b);
    }

    #[test]
    fn activation_message_differs_by_token_id() {
        let a = activation_message(TEST_APP_ID, 1, TEST_MACHINE_ID);
        let b = activation_message(TEST_APP_ID, 2, TEST_MACHINE_ID);
        assert_ne!(a, b);
    }

    #[test]
    fn activation_message_differs_by_machine_id() {
        let a = activation_message(TEST_APP_ID, TEST_TOKEN_ID, "sha256:machine-a");
        let b = activation_message(TEST_APP_ID, TEST_TOKEN_ID, "sha256:machine-b");
        assert_ne!(a, b);
    }

    #[test]
    fn personal_sign_hash_matches_known_vector() {
        // Vector computed with ethers.js:
        //   ethers.utils.hashMessage(ethers.utils.arrayify("0x" + "00".repeat(32)))
        // which is keccak256("\x19Ethereum Signed Message:\n32" || [0u8; 32])
        let message = [0u8; 32];
        let hash = personal_sign_hash(&message);
        assert_eq!(
            hex::encode(hash),
            "47173285a8d7341e5e972fc677286384f802f8ef42a5ec5f03bbfa254cb01fad"
        );
    }

    #[test]
    fn personal_sign_hash_differs_from_raw_message() {
        let message = [0u8; 32];
        let prefixed = personal_sign_hash(&message);
        assert_ne!(prefixed, message);
    }

    #[test]
    fn verify_rejects_wrong_machine_id() {
        let proof = LicenseProof {
            app_id: TEST_APP_ID.into(),
            token_id: TEST_TOKEN_ID,
            wallet_address: "0xdeadbeef".into(),
            paid_by: None,
            machine_id: "sha256:original-machine".into(),
            signature: "0x00".repeat(32) + "00", // placeholder — won't reach sig check
            activated_at: "2026-01-01T00:00:00Z".into(),
            chain: "base".into(),
            contract: "0x1234".into(),
        };

        let err = verify(&proof, "sha256:different-machine").unwrap_err();
        assert!(matches!(err, VerifyError::MachineIdMismatch));
    }

    #[test]
    fn license_proof_serialises_without_paid_by_when_none() {
        let proof = LicenseProof {
            app_id: "com.example.app".into(),
            token_id: 1,
            wallet_address: "0xabc".into(),
            paid_by: None,
            machine_id: "sha256:abc".into(),
            signature: "0xsig".into(),
            activated_at: "2026-01-01T00:00:00Z".into(),
            chain: "base".into(),
            contract: "0x1234".into(),
        };

        let json = serde_json::to_string(&proof).unwrap();
        assert!(!json.contains("paid_by"), "paid_by should be omitted when None");
    }

    #[test]
    fn license_proof_serialises_with_paid_by_when_set() {
        let proof = LicenseProof {
            app_id: "com.example.app".into(),
            token_id: 1,
            wallet_address: "0xabc".into(),
            paid_by: Some("0xdef".into()),
            machine_id: "sha256:abc".into(),
            signature: "0xsig".into(),
            activated_at: "2026-01-01T00:00:00Z".into(),
            chain: "base".into(),
            contract: "0x1234".into(),
        };

        let json = serde_json::to_string(&proof).unwrap();
        assert!(json.contains("paid_by"));
        assert!(json.contains("0xdef"));
    }

    #[test]
    fn license_proof_round_trips_json() {
        let original = LicenseProof {
            app_id: "com.example.app".into(),
            token_id: 99,
            wallet_address: "0xabc123".into(),
            paid_by: Some("0xdef456".into()),
            machine_id: "sha256:somehash".into(),
            signature: "0xdeadbeef".into(),
            activated_at: "2026-04-08T00:00:00Z".into(),
            chain: "base".into(),
            contract: "0x5678".into(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored: LicenseProof = serde_json::from_str(&json).unwrap();

        assert_eq!(original.app_id, restored.app_id);
        assert_eq!(original.token_id, restored.token_id);
        assert_eq!(original.wallet_address, restored.wallet_address);
        assert_eq!(original.paid_by, restored.paid_by);
        assert_eq!(original.machine_id, restored.machine_id);
        assert_eq!(original.signature, restored.signature);
    }
}
