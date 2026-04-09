// These items are used in Phase 1.3 when the license check is wired into main.
#[allow(dead_code)]
/// Derives a stable, per-machine identifier salted with `app_id`.
///
/// Formula: SHA-256(platform_uuid || app_id)
///
/// The salt prevents cross-app tracking: two apps on the same machine
/// produce different machine IDs even though they share the same hardware.
pub fn machine_id(app_id: &str) -> Result<String, MachineIdError> {
    let uuid = machine_uid::get().map_err(|e| MachineIdError::UuidNotFound(e.to_string()))?;

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(uuid.as_bytes());
    hasher.update(app_id.as_bytes());

    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

#[derive(Debug)]
pub enum MachineIdError {
    UuidNotFound(String),
}

impl std::fmt::Display for MachineIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MachineIdError::UuidNotFound(msg) => write!(f, "platform UUID unavailable: {msg}"),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_APP_ID: &str = "com.deotp.test";

    #[test]
    fn machine_id_is_stable_across_calls() {
        let a = machine_id(TEST_APP_ID).expect("machine_id failed");
        let b = machine_id(TEST_APP_ID).expect("machine_id failed");
        assert_eq!(a, b);
    }

    #[test]
    fn machine_id_has_expected_format() {
        let id = machine_id(TEST_APP_ID).expect("machine_id failed");
        assert!(
            id.starts_with("sha256:"),
            "expected sha256: prefix, got: {id}"
        );
        // sha256: + 64 hex chars
        assert_eq!(id.len(), 7 + 64, "unexpected length: {}", id.len());
    }

    #[test]
    fn different_app_ids_produce_different_machine_ids() {
        let a = machine_id("com.example.app_a").expect("machine_id failed");
        let b = machine_id("com.example.app_b").expect("machine_id failed");
        assert_ne!(a, b, "different app_ids must produce different machine IDs");
    }
}
