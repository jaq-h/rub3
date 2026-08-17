//! Contract facts, derived from the artifacts `forge build` wrote.
//!
//! The deployable set is discovered the way `scripts/canonical-bytecode-hashes.sh`
//! discovers it: every artifact records the file and contract it was compiled
//! from in `.metadata.settings.compilationTarget`, and the ones declared under
//! the resolved source directory are the rub3 contracts. Nothing here parses
//! Solidity, and nothing here holds a copy of a signature or a selector.
//!
//! `contracts/out/` is not checked in. When it is absent the tools say so and
//! name the command that produces it, because the alternative - answering from
//! a remembered ABI - is the failure this whole crate exists to avoid.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::repo::{Repo, RepoError};

/// Why a contract fact could not be derived.
#[derive(Debug)]
pub enum SolidityError {
    /// `forge build` has not run, so there are no artifacts to read.
    NoArtifacts { artifact_dir: String },
    /// A named contract is not in the derived set.
    UnknownContract { name: String, known: Vec<String> },
    /// An artifact exists but does not carry what it must.
    Malformed { artifact: String, detail: String },
    /// The repository could not be read.
    Repo(RepoError),
}

impl fmt::Display for SolidityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolidityError::NoArtifacts { artifact_dir } => write!(
                f,
                "no forge artifacts under contracts/{artifact_dir}, so no ABI can be derived. \
                 This crate never answers a contract question from a stored copy. Build them \
                 first:\n\n    (cd contracts && forge build)"
            ),
            SolidityError::UnknownContract { name, known } => write!(
                f,
                "no contract named {name} is declared under the contracts source directory. \
                 Derived from this build: {}",
                known.join(", ")
            ),
            SolidityError::Malformed { artifact, detail } => {
                write!(f, "contracts/{artifact} is unusable: {detail}")
            }
            SolidityError::Repo(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SolidityError {}

impl From<RepoError> for SolidityError {
    fn from(e: RepoError) -> Self {
        SolidityError::Repo(e)
    }
}

/// The contracts this build produced, and where they came from.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ContractSet {
    /// Source directory resolved from `contracts/foundry.toml`.
    pub source_dir: String,
    /// Artifact directory resolved from `contracts/foundry.toml`.
    pub artifact_dir: String,
    /// Every contract declared under the source directory, sorted by name.
    pub contracts: Vec<Contract>,
}

/// One contract declared under the contracts source directory.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Contract {
    /// Contract name, and the identifier the ABI tool takes.
    pub name: String,
    /// Repository-relative Solidity source path.
    pub source: String,
    /// Repository-relative artifact path the facts were read from.
    pub artifact: String,
    /// `contract`, `abstract contract`, `interface` or `library`, from the AST
    /// rather than from a keyword scan.
    pub kind: String,
    /// False for an abstract base or an interface, which compile to no runtime
    /// code and cannot be deployed or bought from.
    pub deployable: bool,
    /// The published canonical fingerprint, when this contract has one.
    pub canonical: Option<Canonical>,
}

/// A contract's entry in `contracts/canonical-bytecode.json`.
///
/// The wrapper compares a live deploy against exactly this number before it
/// buys, so an agent auditing a contract wants it from the same published
/// record rather than from a screenshot of one.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Canonical {
    /// sha256 of `deployedBytecode.object`, which has every immutable zeroed.
    pub deployed_bytecode_sha256: String,
    /// The byte ranges a comparator must zero in `eth_getCode(addr)` before
    /// hashing, as `(start, length)` pairs.
    pub immutable_ranges: Vec<(usize, usize)>,
}

/// A contract's ABI, rendered from the artifact.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Abi {
    /// The contract these entries belong to.
    pub contract: String,
    /// Repository-relative Solidity source path.
    pub source: String,
    /// Repository-relative artifact path the ABI was read from.
    pub artifact: String,
    /// `contract`, `abstract contract`, `interface` or `library`, from the AST
    /// rather than from a keyword scan.
    pub kind: String,
    /// False for an abstract base or an interface, which compile to no runtime
    /// code and cannot be deployed or bought from.
    ///
    /// An ABI reads the same either way, so a caller asking for `Rub3License`
    /// would otherwise get a full, correct, entirely unusable answer: the base
    /// declares `purchase` and `activate` but exists at no address.
    pub deployable: bool,
    /// The constructor, when the contract declares one.
    pub constructor: Option<Entry>,
    /// External and public functions.
    pub functions: Vec<Entry>,
    /// Events.
    pub events: Vec<Entry>,
    /// Custom errors.
    pub errors: Vec<Entry>,
    /// `fallback` and `receive`, when present.
    pub special: Vec<String>,
    /// The artifact's `abi` array, verbatim, when the caller asked for it.
    pub raw: Option<Value>,
}

/// One ABI entry.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Entry {
    /// The canonical signature a selector is computed over, for example
    /// `purchase()` or `setTokenPrice(address,uint256)`.
    pub signature: String,
    /// The same entry with parameter names and return types, for reading.
    pub declaration: String,
    /// The 4-byte selector, for functions only, read from the artifact's own
    /// `methodIdentifiers` map rather than hashed here.
    pub selector: Option<String>,
}

/// Resolves the contracts source and artifact directories.
///
/// Read from `contracts/foundry.toml` rather than assumed, because the
/// fingerprint gate resolves the same two directories from the active config and
/// a docs server reading a different pair would describe a different build.
/// Forge's own defaults apply when the file names neither.
pub fn directories(repo: &Repo) -> Result<(String, String), SolidityError> {
    let text = repo.read("contracts/foundry.toml")?;
    let config: toml::Value = toml::from_str(&text).map_err(|e| SolidityError::Malformed {
        artifact: "foundry.toml".to_string(),
        detail: format!("not valid TOML: {e}"),
    })?;
    let profile = config.get("profile").and_then(|p| p.get("default"));
    let read = |key: &str, default: &str| {
        profile
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .trim_end_matches('/')
            .to_string()
    };
    Ok((read("src", "src"), read("out", "out")))
}

/// Derives the contract set from the artifacts on disk.
pub fn contracts(repo: &Repo) -> Result<ContractSet, SolidityError> {
    let (source_dir, artifact_dir) = directories(repo)?;
    let artifact_root = repo.path(&format!("contracts/{artifact_dir}"))?;
    let mut artifacts = Vec::new();
    collect_artifacts(
        &artifact_root,
        &format!("contracts/{artifact_dir}"),
        &mut artifacts,
    );
    if artifacts.is_empty() {
        return Err(SolidityError::NoArtifacts { artifact_dir });
    }

    let published = canonical_manifest(repo)?;
    let source_prefix = format!("{source_dir}/");
    let mut contracts: Vec<Contract> = Vec::new();
    for artifact in artifacts {
        let text = match repo.read(&artifact) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let json: Value = match serde_json::from_str(&text) {
            Ok(json) => json,
            Err(_) => continue,
        };
        let targets = json
            .get("metadata")
            .and_then(|m| m.get("settings"))
            .and_then(|s| s.get("compilationTarget"))
            .and_then(Value::as_object);
        let Some(targets) = targets else { continue };
        for (file, name) in targets {
            if !file.starts_with(&source_prefix) {
                continue;
            }
            let Some(name) = name.as_str() else { continue };
            let object = json
                .pointer("/deployedBytecode/object")
                .and_then(Value::as_str)
                .unwrap_or("");
            contracts.push(Contract {
                name: name.to_string(),
                source: format!("contracts/{file}"),
                artifact: artifact.clone(),
                kind: kind_of(&json, name),
                deployable: !object.is_empty() && object != "0x",
                canonical: published.get(name).cloned(),
            });
        }
    }
    contracts.sort_by(|a, b| a.name.cmp(&b.name));
    contracts.dedup_by(|a, b| a.name == b.name && a.artifact == b.artifact);
    Ok(ContractSet {
        source_dir,
        artifact_dir,
        contracts,
    })
}

/// Derives one contract's ABI.
///
/// forge writes an artifact to `out/<declaring file>/<contract>.json`, so the
/// candidate for a named contract is found by basename and then *checked*
/// against its own `compilationTarget` before it is believed. That check is what
/// keeps this a derivation rather than a filename convention: an artifact whose
/// name matches but whose target is a test contract is rejected, and the full
/// walk answers instead. It matters because the full walk parses every artifact
/// in the tree - the AST output the fingerprint gate needs makes them large -
/// and this is the tool an agent calls repeatedly.
pub fn abi(repo: &Repo, name: &str, include_raw: bool) -> Result<Abi, SolidityError> {
    let (source_dir, artifact_dir) = directories(repo)?;
    let source_prefix = format!("{source_dir}/");
    let artifact_root = repo.path(&format!("contracts/{artifact_dir}"))?;

    let mut candidates = Vec::new();
    collect_named(
        &artifact_root,
        &format!("contracts/{artifact_dir}"),
        &format!("{name}.json"),
        &mut candidates,
    );

    let mut found = None;
    for candidate in candidates {
        let Ok(text) = repo.read(&candidate) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let declares = json
            .pointer("/metadata/settings/compilationTarget")
            .and_then(Value::as_object)
            .map(|targets| {
                targets.iter().find_map(|(file, declared)| {
                    (file.starts_with(&source_prefix) && declared.as_str() == Some(name))
                        .then(|| file.clone())
                })
            })
            .unwrap_or(None);
        if let Some(file) = declares {
            found = Some((candidate, format!("contracts/{file}"), json));
            break;
        }
    }

    let (artifact_path, source, json) = match found {
        Some(found) => found,
        None => {
            // No artifact answers to that name, so the error has to list the
            // ones that do, which needs the full set.
            let set = contracts(repo)?;
            return Err(SolidityError::UnknownContract {
                name: name.to_string(),
                known: set
                    .contracts
                    .iter()
                    .map(|contract| contract.name.clone())
                    .collect(),
            });
        }
    };
    let contract = Contract {
        name: name.to_string(),
        source,
        artifact: artifact_path,
        kind: kind_of(&json, name),
        deployable: json
            .pointer("/deployedBytecode/object")
            .and_then(Value::as_str)
            .is_some_and(|object| !object.is_empty() && object != "0x"),
        canonical: None,
    };
    let entries =
        json.get("abi")
            .and_then(Value::as_array)
            .ok_or_else(|| SolidityError::Malformed {
                artifact: contract.artifact.clone(),
                detail: "carries no `abi` array".to_string(),
            })?;

    // The artifact's own selector map. Reading it rather than hashing the
    // signature here keeps this crate free of a keccak implementation whose
    // agreement with solc would be one more thing to prove.
    let selectors: BTreeMap<String, String> = json
        .get("methodIdentifiers")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(signature, selector)| {
                    Some((signature.clone(), format!("0x{}", selector.as_str()?)))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut abi = Abi {
        contract: contract.name.clone(),
        source: contract.source.clone(),
        artifact: contract.artifact.clone(),
        kind: contract.kind.clone(),
        deployable: contract.deployable,
        constructor: None,
        functions: Vec::new(),
        events: Vec::new(),
        errors: Vec::new(),
        special: Vec::new(),
        raw: include_raw.then(|| Value::Array(entries.clone())),
    };

    for entry in entries {
        let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "constructor" => abi.constructor = Some(render(entry, &selectors, "constructor")),
            "function" => abi.functions.push(render(entry, &selectors, "function")),
            "event" => abi.events.push(render(entry, &selectors, "event")),
            "error" => abi.errors.push(render(entry, &selectors, "error")),
            "fallback" | "receive" => abi.special.push(kind.to_string()),
            _ => {}
        }
    }
    abi.functions.sort_by(|a, b| a.signature.cmp(&b.signature));
    abi.events.sort_by(|a, b| a.signature.cmp(&b.signature));
    abi.errors.sort_by(|a, b| a.signature.cmp(&b.signature));
    Ok(abi)
}

/// The published fingerprints, keyed by contract name.
fn canonical_manifest(repo: &Repo) -> Result<BTreeMap<String, Canonical>, SolidityError> {
    let text = repo.read("contracts/canonical-bytecode.json")?;
    let manifest: Value = serde_json::from_str(&text).map_err(|e| SolidityError::Malformed {
        artifact: "canonical-bytecode.json".to_string(),
        detail: format!("not JSON: {e}"),
    })?;
    let mut published = BTreeMap::new();
    let Some(contracts) = manifest.get("contracts").and_then(Value::as_object) else {
        return Ok(published);
    };
    for (name, record) in contracts {
        let Some(hash) = record
            .get("deployed_bytecode_sha256")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let ranges = record
            .get("immutable_ranges")
            .and_then(Value::as_array)
            .map(|ranges| {
                ranges
                    .iter()
                    .filter_map(|range| {
                        Some((
                            range.get("start")?.as_u64()? as usize,
                            range.get("length")?.as_u64()? as usize,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        published.insert(
            name.clone(),
            Canonical {
                deployed_bytecode_sha256: hash.to_string(),
                immutable_ranges: ranges,
            },
        );
    }
    Ok(published)
}

/// `contract`, `abstract contract`, `interface` or `library`, from the AST.
///
/// `ast = true` in `contracts/foundry.toml` is what emits it, for the
/// fingerprint gate's benefit; the same field tells a caller here whether the
/// thing they are looking at can be deployed at all. Without it the kind reads
/// `unknown` rather than being guessed from the bytecode.
fn kind_of(artifact: &Value, name: &str) -> String {
    let Some(nodes) = artifact.pointer("/ast/nodes").and_then(Value::as_array) else {
        return "unknown".to_string();
    };
    for node in nodes {
        if node.get("nodeType").and_then(Value::as_str) != Some("ContractDefinition") {
            continue;
        }
        if node.get("name").and_then(Value::as_str) != Some(name) {
            continue;
        }
        let kind = node
            .get("contractKind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let abstract_ = node
            .get("abstract")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return if abstract_ {
            format!("abstract {kind}")
        } else {
            kind.to_string()
        };
    }
    "unknown".to_string()
}

/// Renders one ABI entry into a canonical signature and a readable declaration.
fn render(entry: &Value, selectors: &BTreeMap<String, String>, kind: &str) -> Entry {
    let name = entry.get("name").and_then(Value::as_str).unwrap_or("");
    let inputs = entry.get("inputs").and_then(Value::as_array);
    let canonical_types: Vec<String> = inputs
        .map(|inputs| inputs.iter().map(canonical_type).collect())
        .unwrap_or_default();
    let named: Vec<String> = inputs
        .map(|inputs| {
            inputs
                .iter()
                .map(|input| {
                    let type_ = canonical_type(input);
                    let indexed = input
                        .get("indexed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let parameter = input.get("name").and_then(Value::as_str).unwrap_or("");
                    let mut rendered = type_;
                    if indexed {
                        rendered.push_str(" indexed");
                    }
                    if !parameter.is_empty() {
                        rendered.push(' ');
                        rendered.push_str(parameter);
                    }
                    rendered
                })
                .collect()
        })
        .unwrap_or_default();

    let signature = format!("{name}({})", canonical_types.join(","));
    let mutability = entry
        .get("stateMutability")
        .and_then(Value::as_str)
        .unwrap_or("");
    let returns: Vec<String> = entry
        .get("outputs")
        .and_then(Value::as_array)
        .map(|outputs| outputs.iter().map(canonical_type).collect())
        .unwrap_or_default();

    let mut declaration = match kind {
        "constructor" => format!("constructor({})", named.join(", ")),
        "event" => format!("event {name}({})", named.join(", ")),
        "error" => format!("error {name}({})", named.join(", ")),
        _ => format!("function {name}({})", named.join(", ")),
    };
    if kind == "function" || kind == "constructor" {
        // `nonpayable` is the ABI's way of saying nothing, and Solidity has no
        // such keyword, so printing it would produce a declaration that does
        // not compile.
        if !mutability.is_empty() && mutability != "nonpayable" {
            declaration.push(' ');
            declaration.push_str(mutability);
        }
    }
    if !returns.is_empty() {
        declaration.push_str(&format!(" returns ({})", returns.join(", ")));
    }

    Entry {
        selector: if kind == "function" {
            selectors.get(&signature).cloned()
        } else {
            None
        },
        signature,
        declaration,
    }
}

/// The canonical type of one ABI parameter.
///
/// A tuple's ABI type is the word `tuple` plus any array suffix, with the member
/// types in `components`, so the canonical form substitutes the members back in
/// and keeps the suffix: `tuple[]` with two `uint256` members is
/// `(uint256,uint256)[]`. Getting this wrong changes the selector, which is why
/// a test cross-checks every rendered signature against the artifact's own
/// `methodIdentifiers` map.
fn canonical_type(parameter: &Value) -> String {
    let declared = parameter.get("type").and_then(Value::as_str).unwrap_or("");
    let Some(suffix) = declared.strip_prefix("tuple") else {
        return declared.to_string();
    };
    let members: Vec<String> = parameter
        .get("components")
        .and_then(Value::as_array)
        .map(|components| components.iter().map(canonical_type).collect())
        .unwrap_or_default();
    format!("({}){suffix}", members.join(","))
}

/// Artifacts whose file name is exactly `wanted`, `build-info` excluded.
fn collect_named(directory: &std::path::Path, prefix: &str, wanted: &str, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = format!("{prefix}/{name}");
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if name == "build-info" {
                continue;
            }
            collect_named(&entry.path(), &relative, wanted, found);
        } else if file_type.is_file() && name == wanted {
            found.push(relative);
        }
    }
}

/// Every `*.json` under the artifact directory, `build-info` excluded.
///
/// `build-info` holds whole-project standard-json inputs and outputs, megabytes
/// each, and no contract's `compilationTarget` sits in one, so entering it would
/// cost seconds per call and find nothing.
fn collect_artifacts(directory: &std::path::Path, prefix: &str, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = format!("{prefix}/{name}");
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if name == "build-info" {
                continue;
            }
            collect_artifacts(&entry.path(), &relative, found);
        } else if file_type.is_file() && name.ends_with(".json") {
            found.push(relative);
        }
    }
}
