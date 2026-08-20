//! Finding the rub3 checkout the CLI works in.
//!
//! Both subcommands need one: `pack` builds the wrapper from source, and both
//! read `contracts/deployments.json` out of it. The working directory is
//! whatever the operator's shell was in, so the root is resolved once - from
//! `--repo-root`, from `RUB3_REPO_ROOT`, or by walking up - and everything
//! afterwards is relative to it.

use std::fmt;
use std::path::{Path, PathBuf};

/// Files whose presence together identify a rub3 checkout.
///
/// All three are load-bearing rather than decorative: `pack` builds the crate,
/// `deploy` runs a forge script from `contracts/`, and both refuse without the
/// manifest. A directory holding one but not the others would fail later and
/// less clearly.
const MARKERS: [&str; 3] = [
    "Cargo.toml",
    "crates/rub3-wrapper/Cargo.toml",
    "contracts/deployments.json",
];

/// A resolved rub3 checkout.
#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
}

/// Why no checkout could be resolved.
#[derive(Debug)]
pub enum RepoError {
    /// A path was named explicitly and is not a rub3 checkout.
    NotARub3Repo {
        path: PathBuf,
        missing: &'static str,
    },
    /// Walking up from the working directory found none.
    NotFound { from: PathBuf },
    /// The working directory could not be read.
    NoWorkingDirectory(std::io::Error),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoError::NotARub3Repo { path, missing } => write!(
                f,
                "{} is not a rub3 checkout: no {missing} in it",
                path.display()
            ),
            RepoError::NotFound { from } => write!(
                f,
                "no rub3 checkout found at {} or in any parent directory. Pass --repo-root \
                 <path>, or set RUB3_REPO_ROOT",
                from.display()
            ),
            RepoError::NoWorkingDirectory(e) => write!(f, "cannot read the working directory: {e}"),
        }
    }
}

impl std::error::Error for RepoError {}

impl Repo {
    /// Resolves a checkout from an explicit path, the environment, or the
    /// working directory's ancestry, in that order.
    ///
    /// An explicit path that is not a checkout is an error rather than a
    /// starting point for the walk: somebody who names a directory means that
    /// directory.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Repo, RepoError> {
        let explicit = explicit.or_else(|| std::env::var_os("RUB3_REPO_ROOT").map(PathBuf::from));
        if let Some(path) = explicit {
            return match missing_marker(&path) {
                Some(missing) => Err(RepoError::NotARub3Repo { path, missing }),
                None => Ok(Repo { root: path }),
            };
        }

        let from = std::env::current_dir().map_err(RepoError::NoWorkingDirectory)?;
        let mut candidate: Option<&Path> = Some(&from);
        while let Some(dir) = candidate {
            if missing_marker(dir).is_none() {
                return Ok(Repo {
                    root: dir.to_path_buf(),
                });
            }
            candidate = dir.parent();
        }
        Err(RepoError::NotFound { from })
    }

    /// The checkout root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path inside the checkout.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

fn missing_marker(root: &Path) -> Option<&'static str> {
    MARKERS
        .into_iter()
        .find(|marker| !root.join(marker).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_missing_a_marker_is_not_a_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let err = Repo::resolve(Some(tmp.path().to_path_buf())).unwrap_err();
        assert!(matches!(err, RepoError::NotARub3Repo { .. }), "{err:?}");
        assert!(
            err.to_string().contains("crates/rub3-wrapper/Cargo.toml"),
            "{err}"
        );
    }

    #[test]
    fn the_workspace_this_test_runs_in_is_a_checkout() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let repo = Repo::resolve(Some(root.clone())).expect("the workspace resolves");
        assert_eq!(repo.root(), root);
    }
}
