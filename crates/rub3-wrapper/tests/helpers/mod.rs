// Shared by several integration test binaries via `mod helpers;`. Each one
// compiles the whole module but uses only part of it, so anything not needed by
// the binary currently being built is legitimately dead within that unit.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use k256::ecdsa::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use sha3::Digest;

use rub3_wrapper::license::{self, LicenseProof};

pub fn wrapper_bin() -> PathBuf {
    env!("CARGO_BIN_EXE_rub3-wrapper").into()
}

pub fn generate_wallet() -> (SigningKey, String) {
    let signing_key = SigningKey::random(&mut OsRng);
    let address = verifying_key_to_address(signing_key.verifying_key());
    (signing_key, address)
}

pub fn sign_activation(signing_key: &SigningKey, app_id: &str, token_id: u64) -> String {
    let message = license::activation_message(app_id, token_id);
    let prefixed = personal_sign_hash(&message);

    let (sig, recovery_id) = signing_key
        .sign_prehash_recoverable(&prefixed)
        .expect("signing failed");

    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    // Encode v as legacy (27/28) to match Ethereum convention
    sig_bytes[64] = recovery_id.to_byte() + 27;

    format!("0x{}", hex::encode(sig_bytes))
}

pub fn create_license_json(
    dir: &Path,
    app_id: &str,
    token_id: u64,
    wallet_address: &str,
    signature: &str,
) -> PathBuf {
    std::fs::create_dir_all(dir).expect("failed to create license dir");

    let proof = LicenseProof {
        app_id: app_id.to_string(),
        token_id,
        wallet_address: wallet_address.to_string(),
        paid_by: None,
        signature: signature.to_string(),
        activated_at: "2026-01-01T00:00:00Z".to_string(),
        chain: "base".to_string(),
        contract: "0x0000000000000000000000000000000000000000".to_string(),
    };

    let path = dir.join(format!("{app_id}.json"));
    let json = serde_json::to_string_pretty(&proof).expect("failed to serialize proof");
    std::fs::write(&path, json).expect("failed to write license json");
    path
}

fn personal_sign_hash(message: &[u8; 32]) -> [u8; 32] {
    let mut hasher = sha3::Keccak256::new();
    hasher.update(b"\x19Ethereum Signed Message:\n32");
    hasher.update(message);
    hasher.finalize().into()
}

pub fn verifying_key_to_address(key: &VerifyingKey) -> String {
    let uncompressed = key.to_encoded_point(false);
    let bytes = uncompressed.as_bytes();
    let hash = sha3::Keccak256::digest(&bytes[1..]);
    format!("0x{}", hex::encode(&hash[12..]))
}

/// Writes a signed session where the launch fast path will look for it, and
/// returns it.
///
/// The signature is produced over `session::session_message`, the same preimage
/// the wrapper verifies, so the seeded session passes `verify_local` rather than
/// merely deserializing. `session_dir` is what the wrapper is given as
/// `RUB3_SESSION_DIR`.
#[cfg(feature = "session")]
pub fn create_session_json(
    session_dir: &Path,
    app_id: &str,
    token_id: u64,
    signing_key: &SigningKey,
    identity: &str,
    user_id: &str,
    expires_at: &str,
) -> rub3_wrapper::session::Session {
    use rub3_wrapper::session::{new_nonce, session_message, Session};

    let wallet = verifying_key_to_address(signing_key.verifying_key());
    let nonce = new_nonce();
    let message = session_message(
        app_id,
        token_id,
        identity,
        user_id,
        &wallet,
        &nonce,
        Some(expires_at),
        None,
        None,
        None,
    );

    let prefixed = personal_sign_hash(&message);
    let (sig, recovery_id) = signing_key
        .sign_prehash_recoverable(&prefixed)
        .expect("signing failed");
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recovery_id.to_byte() + 27;

    let session = Session {
        app_id: app_id.to_string(),
        token_id,
        identity: identity.to_string(),
        user_id: user_id.to_string(),
        tba: None,
        wallet,
        nonce,
        issued_at: "2026-01-01T00:00:00Z".to_string(),
        expires_at: Some(expires_at.to_string()),
        signature: format!("0x{}", hex::encode(sig_bytes)),
        chain: "base".to_string(),
        contract: "0x0000000000000000000000000000000000000000".to_string(),
        activation_tx: None,
        activation_block: None,
        activation_block_hash: None,
        session_id: None,
        device_pubkey: None,
    };

    let dir = session_dir.join(app_id);
    std::fs::create_dir_all(&dir).expect("failed to create session dir");
    let json = serde_json::to_string_pretty(&session).expect("failed to serialize session");
    std::fs::write(dir.join(format!("{token_id}.json")), json).expect("failed to write session");
    session
}
