//! Finding the repository, and the only file reads in this crate.
//!
//! An MCP server is started by a client whose working directory is whatever the
//! client felt like, so the repository root cannot be assumed to be `.`. It is
//! resolved once at startup, from an explicit path if one was given and by
//! walking up from the working directory otherwise, and every read afterwards
//! goes through [`Repo::read`] relative to it.
//!
//! `read` also refuses to leave the root. The document tools take a path from
//! the caller, and a docs server that would hand back `../../.ssh/id_ed25519`
//! on request is a file-exfiltration tool with a Markdown parser attached. The
//! document surface resolves caller paths against the derived inventory as
//! well, so the guard here is the second of two locks rather than the only one.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// A resolved rub3 repository root.
#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
}

/// Why a repository root could not be resolved, or a read refused.
#[derive(Debug)]
pub enum RepoError {
    /// The path given is not a rub3 checkout.
    NotARub3Repo {
        path: PathBuf,
        missing: &'static str,
    },
    /// No rub3 checkout was found by walking up from the working directory.
    NotFound { from: PathBuf },
    /// The working directory could not be read.
    NoWorkingDirectory(std::io::Error),
    /// A relative path tried to leave the repository root.
    OutsideRepo { requested: String },
    /// A file under the root could not be read.
    Unreadable {
        path: String,
        source: std::io::Error,
    },
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
            RepoError::OutsideRepo { requested } => write!(
                f,
                "{requested} resolves outside the repository, and this server reads nothing \
                 outside it"
            ),
            RepoError::Unreadable { path, source } => write!(f, "cannot read {path}: {source}"),
        }
    }
}

impl std::error::Error for RepoError {}

/// The markers that identify a rub3 checkout.
///
/// Both are load-bearing rather than decorative: the contract tools resolve
/// `contracts/foundry.toml` for the source and artifact directories, and the
/// Rust tools read the workspace's crates. A directory holding one but not the
/// other would answer half the tools and fail the rest at call time, which is a
/// worse failure than refusing to start.
const MARKERS: [&str; 2] = ["Cargo.toml", "contracts/foundry.toml"];

impl Repo {
    /// Resolves a root from an explicit path, or by walking up from `from`.
    ///
    /// An explicit path that is not a rub3 checkout is an error rather than a
    /// starting point for the walk: somebody who names a directory means that
    /// directory, and silently serving a parent's documents instead would be a
    /// wrong answer dressed as a working one.
    pub fn resolve(explicit: Option<PathBuf>, from: PathBuf) -> Result<Repo, RepoError> {
        match explicit {
            Some(path) => {
                if let Some(missing) = Repo::missing_marker(&path) {
                    return Err(RepoError::NotARub3Repo { path, missing });
                }
                Ok(Repo { root: path })
            }
            None => {
                let mut candidate = from.as_path();
                loop {
                    if Repo::missing_marker(candidate).is_none() {
                        return Ok(Repo {
                            root: candidate.to_path_buf(),
                        });
                    }
                    candidate = match candidate.parent() {
                        Some(parent) => parent,
                        None => return Err(RepoError::NotFound { from }),
                    };
                }
            }
        }
    }

    /// Resolves a root the way the binary does: `--repo-root`, then
    /// `RUB3_REPO_ROOT`, then a walk up from the working directory, then the
    /// checkout this binary was compiled in.
    ///
    /// The last step is what makes the server usable from an MCP client, which
    /// starts it with whatever working directory it happens to have - often
    /// `/`. It is a fallback rather than the first choice because a binary can
    /// outlive or be copied away from the tree it was built in, and it is
    /// checked against the same markers as any other candidate, so a stale
    /// compile-time path is a refusal to start and not a wrong answer.
    pub fn resolve_from_env(explicit: Option<PathBuf>) -> Result<Repo, RepoError> {
        let explicit = explicit.or_else(|| std::env::var_os("RUB3_REPO_ROOT").map(PathBuf::from));
        let cwd = std::env::current_dir().map_err(RepoError::NoWorkingDirectory)?;
        match Repo::resolve(explicit, cwd) {
            Ok(repo) => Ok(repo),
            Err(walk_failed) => Repo::compiled_in().map_err(|_| walk_failed),
        }
    }

    /// The checkout this crate was compiled in.
    ///
    /// `CARGO_MANIFEST_DIR` is `crates/rub3-docs-mcp`, so the root is two
    /// directories up. Every test that wants real documents, real artifacts or
    /// real source starts here, and so does the binary's last-resort fallback.
    pub fn compiled_in() -> Result<Repo, RepoError> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| RepoError::NotFound {
                from: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            })?;
        Repo::resolve(Some(root), PathBuf::from("/"))
    }

    fn missing_marker(path: &Path) -> Option<&'static str> {
        MARKERS
            .into_iter()
            .find(|marker| !path.join(marker).is_file())
    }

    /// The resolved root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Joins a repository-relative path, refusing anything that escapes.
    ///
    /// Rejecting `..` textually rather than canonicalising and comparing is
    /// deliberate: canonicalisation resolves symlinks, and `CLAUDE.md` in this
    /// repository is a symlink to `AGENTS.md`, so a prefix check on the
    /// canonical path would reject a file the inventory legitimately lists.
    pub fn path(&self, relative: &str) -> Result<PathBuf, RepoError> {
        let candidate = Path::new(relative);
        let escapes = candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
        if escapes {
            return Err(RepoError::OutsideRepo {
                requested: relative.to_string(),
            });
        }
        Ok(self.root.join(candidate))
    }

    /// Reads a repository-relative UTF-8 file.
    pub fn read(&self, relative: &str) -> Result<String, RepoError> {
        let path = self.path(relative)?;
        std::fs::read_to_string(&path).map_err(|source| RepoError::Unreadable {
            path: relative.to_string(),
            source,
        })
    }

    /// True when a repository-relative path exists as a file.
    pub fn is_file(&self, relative: &str) -> bool {
        self.path(relative).map(|p| p.is_file()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn this_repo() -> Repo {
        Repo::compiled_in().expect("this crate lives in a rub3 checkout")
    }

    #[test]
    fn the_walk_finds_the_root_from_a_subdirectory() {
        let repo = this_repo();
        let deep = repo.root().join("crates/rub3-docs-mcp/src");
        let found = Repo::resolve(None, deep).expect("the walk reaches the root");
        assert_eq!(found.root(), repo.root());
    }

    #[test]
    fn a_named_directory_that_is_not_a_checkout_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = Repo::resolve(Some(dir.path().to_path_buf()), PathBuf::from("/"))
            .expect_err("an empty directory is not a rub3 checkout");
        assert!(
            matches!(err, RepoError::NotARub3Repo { .. }),
            "expected NotARub3Repo, got {err}"
        );
    }

    #[test]
    fn a_relative_path_cannot_leave_the_repository() {
        let repo = this_repo();
        for escape in ["../secrets", "docs/../../secrets", "/etc/passwd"] {
            let err = repo
                .path(escape)
                .expect_err("{escape} must not resolve inside the repo");
            assert!(
                matches!(err, RepoError::OutsideRepo { .. }),
                "expected OutsideRepo for {escape}, got {err}"
            );
        }
    }

    #[test]
    fn a_symlinked_document_is_readable() {
        // CLAUDE.md is a symlink to AGENTS.md in this repository. The guard
        // rejects `..` textually rather than canonicalising precisely so that
        // this read works.
        let repo = this_repo();
        assert!(repo.read("CLAUDE.md").is_ok(), "CLAUDE.md must be readable");
    }
}
