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
