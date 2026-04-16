//! Session persistence at `~/.rub3/sessions/<app_id>/<token_id>.json`.
//!
//! Env override: `RUB3_SESSION_DIR` — used by integration tests to point at a tmpdir.

#![allow(dead_code)]

use std::path::PathBuf;

use crate::session::Session;

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    Io(std::io::Error),
    Serde(serde_json::Error),
    NoDataDir,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "session not found"),
            StoreError::Io(e) => write!(f, "io error: {e}"),
            StoreError::Serde(e) => write!(f, "json error: {e}"),
            StoreError::NoDataDir => write!(f, "no data directory available"),
        }
    }
}

/// Resolves the session file path for a given `app_id` + `token_id`.
pub fn session_path(_app_id: &str, _token_id: u64) -> Result<PathBuf, StoreError> {
    // TODO: use RUB3_SESSION_DIR env var if set, otherwise dirs::data_dir()/rub3/sessions/<app_id>/<token_id>.json
    unimplemented!("session_path: scaffold only")
}

pub fn load_session(_app_id: &str, _token_id: u64) -> Result<Session, StoreError> {
    unimplemented!("load_session: scaffold only")
}

pub fn save_session(_session: &Session) -> Result<(), StoreError> {
    unimplemented!("save_session: scaffold only")
}

/// Scans `~/.rub3/sessions/<app_id>/` for any valid (signature-valid, non-expired)
/// session and returns the most recently issued one. Solves the "don't know
/// token_id at startup" problem for the fast path.
pub fn load_latest_session(_app_id: &str) -> Result<Session, StoreError> {
    unimplemented!("load_latest_session: scaffold only")
}
