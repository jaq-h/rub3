//! The MCP surface: six tools over the four derivation modules.
//!
//! The protocol itself is [`rmcp`], the official Rust SDK, rather than a
//! hand-rolled JSON-RPC loop. That is a deliberate call. The specification moved
//! twice in the year to August 2026, and the `2026-07-28` revision removed the
//! `initialize` handshake outright in favour of a stateless `server/discover`,
//! while clients in the field still speak `2025-11-25` and older. Version
//! negotiation is therefore real work with a real failure mode, and none of it
//! is rub3's work. What this crate owns is the derivation; the SDK owns the wire.
//!
//! Every tool answers from the repository on disk at call time. There is no
//! cache: a docs server that answered from a snapshot taken at startup would
//! start lying the moment an agent edited a file, which is the one thing an
//! agent does constantly.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde::{Deserialize, Serialize};

use crate::docs;
use crate::repo::Repo;
use crate::rustapi;
use crate::solidity;

/// The rub3 docs server.
#[derive(Clone)]
pub struct DocsServer {
    repo: Arc<Repo>,
    tool_router: ToolRouter<Self>,
}

/// Which document to read, and optionally which section of it.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadDocument {
    /// Repository-relative path, exactly as `list_documents` reports it.
    pub path: String,
    /// A heading of that document, by text or by anchor. Omit for the whole
    /// document. A section carries its subsections.
    pub section: Option<String>,
}

/// What to search for.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchDocuments {
    /// Case-insensitive substring.
    pub query: String,
    /// Most hits to return. Defaults to 50.
    pub limit: Option<usize>,
}

/// Which contract's ABI to derive.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ContractAbi {
    /// Contract name, exactly as `list_contracts` reports it.
    pub contract: String,
    /// Include the artifact's `abi` array verbatim as well as the rendered
    /// signatures. Large; off by default.
    pub include_raw: Option<bool>,
}

/// Which part of the Rust API to derive.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RustApi {
    /// Package name, for example `rub3-wrapper`. Omit for every member.
    pub crate_name: Option<String>,
    /// Module name, for example `activation`. Omit for every module.
    pub module: Option<String>,
    /// Case-insensitive substring an item's name must contain. Omit for every
    /// item.
    pub name: Option<String>,
}

/// The document inventory.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DocumentIndex {
    /// Absolute path of the checkout these documents were read from.
    pub repository: String,
    /// Every Markdown document under the root, sorted by path.
    pub documents: Vec<docs::Document>,
}

/// One document, or one section of one.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DocumentText {
    /// Repository-relative path.
    pub path: String,
    /// The section asked for, when one was.
    pub section: Option<String>,
    /// 1-based first line of the returned text.
    pub start_line: usize,
    /// 1-based last line of the returned text.
    pub end_line: usize,
    /// The Markdown, verbatim.
    pub text: String,
}

/// Search hits.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchHits {
    /// The query, echoed back.
    pub query: String,
    /// True when the limit cut the results short.
    pub truncated: bool,
    /// The hits, in document then line order.
    pub matches: Vec<docs::Match>,
}

/// The workspace's derived Rust API.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct WorkspaceApi {
    /// Every workspace member with a library, after filtering.
    pub crates: Vec<rustapi::CrateApi>,
}

#[tool_router(router = tool_router)]
impl DocsServer {
    /// Builds a server over an already-resolved checkout.
    pub fn new(repo: Repo) -> Self {
        Self {
            repo: Arc::new(repo),
            tool_router: Self::tool_router(),
        }
    }

    /// The checkout this server answers from.
    pub fn repo(&self) -> &Repo {
        &self.repo
    }

    #[tool(
        name = "list_documents",
        description = "List rub3's Markdown documents with what each one owns and its full heading \
                       outline. Start here: the outline is how you pick the section to read next, \
                       instead of pulling a whole specification into context. Discovered by walking \
                       the checkout, so a document added since this server was built is listed too."
    )]
    pub fn list_documents(&self) -> Result<Json<DocumentIndex>, ErrorData> {
        let documents = docs::inventory(&self.repo).map_err(internal)?;
        Ok(Json(DocumentIndex {
            repository: self.repo.root().display().to_string(),
            documents,
        }))
    }

    #[tool(
        name = "read_document",
        description = "Read one rub3 document, or one section of it by heading text or anchor. A \
                       section includes its subsections. Returns the Markdown verbatim with the \
                       line range it came from."
    )]
    pub fn read_document(
        &self,
        Parameters(request): Parameters<ReadDocument>,
    ) -> Result<Json<DocumentText>, ErrorData> {
        let inventory = docs::inventory(&self.repo).map_err(internal)?;
        let document = inventory
            .iter()
            .find(|document| document.path == request.path)
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    format!(
                        "no document at {}. Call list_documents for the paths this checkout has.",
                        request.path
                    ),
                    None,
                )
            })?;
        let text = self.repo.read(&document.path).map_err(internal)?;

        match request.section {
            None => Ok(Json(DocumentText {
                path: document.path.clone(),
                section: None,
                start_line: 1,
                end_line: text.lines().count(),
                text,
            })),
            Some(selector) => {
                let section = docs::section(&text, &selector).ok_or_else(|| {
                    ErrorData::invalid_params(
                        format!(
                            "{} has no heading {selector:?}. Its headings are: {}",
                            document.path,
                            document
                                .headings
                                .iter()
                                .map(|heading| heading.text.as_str())
                                .collect::<Vec<_>>()
                                .join(" | ")
                        ),
                        None,
                    )
                })?;
                Ok(Json(DocumentText {
                    path: document.path.clone(),
                    section: Some(section.heading.text.clone()),
                    start_line: section.start_line,
                    end_line: section.end_line,
                    text: section.text,
                }))
            }
        }
    }

    #[tool(
        name = "search_documents",
        description = "Case-insensitive substring search across every rub3 document. Each hit \
                       carries the document, the line number and the heading it sits under, so the \
                       next call can be a targeted read_document."
    )]
    pub fn search_documents(
        &self,
        Parameters(request): Parameters<SearchDocuments>,
    ) -> Result<Json<SearchHits>, ErrorData> {
        let limit = request.limit.unwrap_or(50).clamp(1, 500);
        let inventory = docs::inventory(&self.repo).map_err(internal)?;
        let hits = docs::search(&self.repo, &inventory, &request.query, limit);
        Ok(Json(SearchHits {
            query: request.query,
            truncated: hits.truncated,
            matches: hits.matches,
        }))
    }

    #[tool(
        name = "list_contracts",
        description = "List the Solidity contracts declared under the contracts source directory, \
                       derived from the artifacts forge built: kind, whether they are deployable, \
                       and the published canonical bytecode fingerprint where there is one. Needs \
                       `cd contracts && forge build` first, because contracts/out is not checked \
                       in and this server will not answer a contract question from memory."
    )]
    pub fn list_contracts(&self) -> Result<Json<solidity::ContractSet>, ErrorData> {
        solidity::contracts(&self.repo).map(Json).map_err(internal)
    }

    #[tool(
        name = "contract_abi",
        description = "Derive one contract's ABI from its forge artifact: canonical signatures, \
                       readable declarations, and the 4-byte selectors read out of the artifact's \
                       own methodIdentifiers map. Use this instead of guessing a signature or a \
                       selector. Needs `cd contracts && forge build` first."
    )]
    pub fn contract_abi(
        &self,
        Parameters(request): Parameters<ContractAbi>,
    ) -> Result<Json<solidity::Abi>, ErrorData> {
        solidity::abi(
            &self.repo,
            &request.contract,
            request.include_raw.unwrap_or(false),
        )
        .map(Json)
        .map_err(internal)
    }

    #[tool(
        name = "rust_api",
        description = "Derive the public Rust API of the workspace's crates from their source: \
                       every signature is sliced verbatim out of the file it is declared in, with \
                       its doc comment, its line, and the #[cfg(...)] feature gate that governs \
                       it. Filter by crate, module or name. The feature gate matters: whole \
                       modules of rub3-wrapper do not exist under lower tier bundles."
    )]
    pub fn rust_api(
        &self,
        Parameters(request): Parameters<RustApi>,
    ) -> Result<Json<WorkspaceApi>, ErrorData> {
        let mut crates = rustapi::workspace(&self.repo).map_err(internal)?;

        if let Some(wanted) = &request.crate_name {
            let known: Vec<String> = crates.iter().map(|api| api.name.clone()).collect();
            crates.retain(|api| &api.name == wanted);
            if crates.is_empty() {
                return Err(ErrorData::invalid_params(
                    format!(
                        "no workspace crate named {wanted} with a library. Members with one: {}",
                        known.join(", ")
                    ),
                    None,
                ));
            }
        }
        if let Some(wanted) = &request.module {
            for api in &mut crates {
                api.modules.retain(|module| &module.name == wanted);
            }
        }
        if let Some(wanted) = &request.name {
            let wanted = wanted.to_lowercase();
            for api in &mut crates {
                for module in &mut api.modules {
                    module.items.retain(|item| {
                        item.name.to_lowercase().contains(&wanted)
                            || item.signature.to_lowercase().contains(&wanted)
                            || item
                                .members
                                .iter()
                                .any(|member| member.name.to_lowercase().contains(&wanted))
                    });
                }
            }
        }
        crates.retain(|api| api.modules.iter().any(|module| !module.items.is_empty()));
        Ok(Json(WorkspaceApi { crates }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DocsServer {
    fn get_info(&self) -> ServerInfo {
        // `Implementation::from_build_env()` would name the SDK rather than this
        // crate, because it reads the environment of whichever crate expanded
        // it. A client listing "rmcp 3.1.2" among its servers is a real
        // confusion once a second rmcp-based server is configured.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS.to_string())
    }
}

/// What a client is told about this server before it calls anything.
///
/// The one property worth stating up front is what these tools are *for*: they
/// exist so an integrating agent does not invent a signature. Saying that the
/// answers are derived, and that a missing artifact is an error rather than a
/// guess, is what makes an agent trust the answer it gets and go and build the
/// artifacts when it does not get one.
const INSTRUCTIONS: &str = "\
rub3 is wallet-native licensing for locally executed software: an ERC-721 token on Base is the \
access credential, and the rub3-wrapper runtime verifies ownership on the machine at launch. There \
is no backend.

Every answer from this server is derived from the checkout at call time. Contract ABIs and \
selectors come from the artifacts `forge build` wrote; Rust signatures are sliced verbatim out of \
the source files; document structure is parsed from the Markdown. Nothing is transcribed, so \
nothing here can be stale in the way a hand-maintained copy would be.

The corollary: when a fact cannot be derived, this server returns an error naming the command that \
would produce it. contracts/out is not checked in, so the two contract tools need \
`cd contracts && forge build` first. Run it rather than filling the gap from memory.

Start with list_documents to see what each document owns and its heading outline, then \
read_document for the section you need. Use contract_abi before writing any calldata, and rust_api \
before calling into the wrapper - it reports the #[cfg(...)] feature gate beside each item, and \
whole modules of rub3-wrapper are compiled away under the lower tier bundles.";

/// Every derivation failure reaches a client with the repository's own wording,
/// including the command that would fix it.
fn internal(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}
