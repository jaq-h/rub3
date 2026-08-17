//! rub3 docs MCP server: the repository's own facts, served to a coding agent.
//!
//! An agent integrating rub3 needs three kinds of fact that it must not invent:
//! what the documents say, what the contracts' ABIs actually are, and what the
//! wrapper's Rust API actually looks like. This crate serves those over the
//! Model Context Protocol, and it serves nothing else.
//!
//! # Everything served here is derived
//!
//! No signature, selector, ABI entry, heading or exit code is written down in
//! this crate. Contract facts are read out of the artifacts `forge build`
//! wrote; Rust facts are read out of the wrapper's source with [`syn`], and the
//! signature text handed back is a *byte range of the file itself* rather than
//! a re-rendering; document facts are parsed out of the Markdown. The repo
//! already holds this line for the canonical fingerprints
//! (`scripts/canonical-bytecode-hashes.sh` derives the deployable set from
//! artifacts rather than by parsing Solidity, and `attest`'s manifest mirror
//! test exists so a second hand-maintained copy cannot rot). A docs server that
//! transcribed a signature it read once would rot faster than either, because
//! nothing would notice.
//!
//! The consequence is deliberate: when a fact cannot be derived, this crate
//! serves an error naming the command that would produce it, never a guess.
//! `contracts/out/` is not checked in, so the contract tools ask for
//! `forge build` rather than answering from memory.
//!
//! # Not on the wrapper's dependency path
//!
//! This is a developer-facing crate. It is a separate workspace member and the
//! wrapper does not depend on it, so nothing here adds weight to a shipped
//! `rub3-wrapper` binary. `cargo tree -p rub3-wrapper` is the check.
//!
//! # Layout
//!
//! - [`repo`] resolves the repository root and is the only thing that reads files
//! - [`docs`] parses the Markdown documents (inventory, sections, search)
//! - [`solidity`] derives the deployable contract set and ABIs from forge artifacts
//! - [`rustapi`] derives the workspace's public Rust API from its source
//! - [`server`] is the MCP surface over the four above

pub mod docs;
pub mod repo;
pub mod rustapi;
pub mod server;
pub mod solidity;

pub use repo::{Repo, RepoError};
pub use server::DocsServer;
