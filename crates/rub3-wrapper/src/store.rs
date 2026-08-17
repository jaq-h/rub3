use std::fs;
use std::path::PathBuf;

use crate::license::LicenseProof;

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    DataDirNotFound,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "I/O error: {e}"),
            StoreError::Parse(e) => write!(f, "parse error: {e}"),
            StoreError::DataDirNotFound => write!(f, "platform data directory not found"),
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Parse(e)
    }
}

/// Returns the path for the license proof file.
///
/// Uses `$RUB3_LICENSE_DIR` if set, otherwise falls back to the
/// platform data directory (`~/Library/Application Support` on macOS,
/// `$XDG_DATA_HOME` / `~/.local/share` on Linux, `%APPDATA%` on Windows).
///
/// Full path: `{data_dir}/rub3/licenses/<app_id>.json`
fn proof_path(app_id: &str) -> Result<PathBuf, StoreError> {
    let base = if let Some(override_dir) = std::env::var_os("RUB3_LICENSE_DIR") {
        PathBuf::from(override_dir)
    } else {
        dirs::data_dir()
            .ok_or(StoreError::DataDirNotFound)?
            .join("rub3")
            .join("licenses")
    };
    Ok(base.join(format!("{app_id}.json")))
}

/// Reads and deserialises the stored license proof for `app_id`.
///
/// Returns `Err(StoreError::Io)` with `kind() == NotFound` if no proof exists yet.
pub fn load_proof(app_id: &str) -> Result<LicenseProof, StoreError> {
    let path = proof_path(app_id)?;
    let data = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

/// Serialises and writes the license proof to `~/.rub3/licenses/<app_id>.json`,
/// creating the directory if it does not exist.
pub fn save_proof(app_id: &str, proof: &LicenseProof) -> Result<(), StoreError> {
    let path = proof_path(app_id)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(proof)?;
    fs::write(&path, json)?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Points the proof store at a tmpdir for the duration of one test.
    ///
    /// These tests resolve their path from `RUB3_LICENSE_DIR`, which is
    /// process-global, so they hold [`crate::ENV_LOCK`] for the whole body:
    /// another test setting or clearing that variable between a save and the
    /// load that follows it would send the two at different directories, and
    /// the lock is only exclusive if the readers take it too. Holding the
    /// variable also keeps the suite out of the developer's real data
    /// directory, which is where these used to write, and where they left a
    /// file behind whenever an assertion failed ahead of the cleanup line.
    struct LicenseDir {
        _guard: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
    }

    impl LicenseDir {
        fn set_up() -> Self {
            Self::set_up_in(None)
        }

        /// The same guard, pointed at a path inside the tmpdir that does not
        /// exist yet.
        ///
        /// `tempfile::tempdir` creates its directory, so a store pointed
        /// straight at it never exercises `save_proof`'s `create_dir_all` -
        /// which is the whole subject of one of the tests below, and the first
        /// thing a stock build does on a machine that has never run it.
        fn set_up_missing() -> Self {
            Self::set_up_in(Some("rub3/licenses"))
        }

        fn set_up_in(sub: Option<&str>) -> Self {
            let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().expect("tempdir");
            let base = match sub {
                Some(sub) => dir.path().join(sub),
                None => dir.path().to_path_buf(),
            };
            std::env::set_var("RUB3_LICENSE_DIR", base);
            Self {
                _guard: guard,
                _dir: dir,
            }
        }
    }

    impl Drop for LicenseDir {
        fn drop(&mut self) {
            // Runs before the fields, so the variable is cleared while the
            // lock is still held.
            std::env::remove_var("RUB3_LICENSE_DIR");
        }
    }

    fn test_proof(app_id: &str) -> LicenseProof {
        LicenseProof {
            app_id: app_id.into(),
            token_id: 7,
            wallet_address: "0xabc123".into(),
            paid_by: None,
            signature: "0xsig".into(),
            activated_at: "2026-04-09T00:00:00Z".into(),
            chain: "base".into(),
            contract: "0x1234".into(),
        }
    }

    #[test]
    fn round_trip() {
        let _license_dir = LicenseDir::set_up();
        let app_id = "com.rub3.store_test_round_trip";
        let original = test_proof(app_id);

        save_proof(app_id, &original).expect("save failed");
        let loaded = load_proof(app_id).expect("load failed");

        assert_eq!(original.app_id, loaded.app_id);
        assert_eq!(original.token_id, loaded.token_id);
        assert_eq!(original.wallet_address, loaded.wallet_address);
        assert_eq!(original.signature, loaded.signature);
    }

    #[test]
    fn load_missing_returns_not_found() {
        let _license_dir = LicenseDir::set_up();
        let err = load_proof("com.rub3.store_test_does_not_exist").unwrap_err();
        match err {
            StoreError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected Io(NotFound), got: {other}"),
        }
    }

    #[test]
    fn save_creates_missing_directories() {
        let _license_dir = LicenseDir::set_up_missing();
        let app_id = "com.rub3.store_test_mkdir";
        let proof = test_proof(app_id);

        let path = proof_path(app_id).unwrap();
        assert!(
            !path.parent().unwrap().exists(),
            "the store directory has to be missing, or this proves nothing",
        );

        save_proof(app_id, &proof).expect("save failed");

        assert!(path.exists(), "proof file should exist after save");
    }

    #[test]
    fn save_overwrites_existing_proof() {
        let _license_dir = LicenseDir::set_up();
        let app_id = "com.rub3.store_test_overwrite";

        let mut proof = test_proof(app_id);
        save_proof(app_id, &proof).expect("first save failed");

        proof.token_id = 99;
        save_proof(app_id, &proof).expect("second save failed");

        let loaded = load_proof(app_id).expect("load failed");
        assert_eq!(loaded.token_id, 99);
    }

    #[test]
    fn paid_by_round_trips() {
        let _license_dir = LicenseDir::set_up();
        let app_id = "com.rub3.store_test_paid_by";
        let mut proof = test_proof(app_id);
        proof.paid_by = Some("0xpayer".into());

        save_proof(app_id, &proof).expect("save failed");
        let loaded = load_proof(app_id).expect("load failed");

        assert_eq!(loaded.paid_by, Some("0xpayer".into()));
    }
}
