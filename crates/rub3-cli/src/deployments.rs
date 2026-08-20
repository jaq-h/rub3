//! `contracts/deployments.json`, read the way that file says to read it.
//!
//! It is the one committed record of which `Rub3Factory` is canonical on which
//! chain, and its own `note` states the rule this module implements: **`null`
//! is the only marker for "not deployed", and a consumer must not substitute
//! any other address for it.** Every entry is `null` today and stays that way
//! until launch.
//!
//! So the resolution here has exactly two outcomes and no third: an address the
//! file publishes, or a refusal that names the chain. There is no fallback to
//! the zero address, no placeholder, and no warning-and-continue - a
//! distributable that claimed a canonical factory it cannot name would be worse
//! than one that refuses to build, and a deploy that quietly went direct would
//! silently forfeit the fee stamp and the `isDeployed` record the registry and
//! the marketplace list from.
//!
//! An operator who *means* to work outside the canonical set says so: `pack
//! --factory` and `deploy --factory` name an address explicitly, and `deploy
//! --direct` deploys through no factory at all. Those are choices, not
//! fallbacks, and neither is reachable by forgetting a flag.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The schema version this CLI understands.
///
/// A bump means the file gained a shape this code has never seen, and reading
/// it anyway is how a consumer ends up substituting something for an address it
/// could not find.
const SUPPORTED_SCHEMA: u32 = 1;

/// Path of the manifest inside a checkout, used in messages as well as reads.
pub const MANIFEST_PATH: &str = "contracts/deployments.json";

/// `contracts/deployments.json`, parsed.
#[derive(Debug, Clone)]
pub struct Manifest {
    chains: BTreeMap<u64, ChainRecord>,
}

/// One chain's record. Every address field is `Option` because `null` is the
/// file's only way of saying "not deployed", and it must survive as such all
/// the way to the refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRecord {
    /// Chain id, the key the file is keyed by.
    pub chain_id: u64,
    /// Human-readable name, matching the `[rpc_endpoints]` key in
    /// `contracts/foundry.toml`.
    pub name: String,
    /// The canonical `Rub3Factory`, or `None` when none is deployed here.
    pub factory: Option<String>,
    /// The canonical `Rub3CodeRegistry`, or `None` when none is deployed here.
    pub code_registry: Option<String>,
}

/// Why the manifest could not answer.
#[derive(Debug)]
pub enum ManifestError {
    /// The file could not be read.
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is not the JSON this CLI knows how to read.
    Malformed { path: PathBuf, detail: String },
    /// The file declares a schema this CLI was not written against.
    UnsupportedSchema { found: u32 },
    /// A chain named on the command line is not one the manifest knows.
    UnknownChainName {
        requested: String,
        known: Vec<String>,
    },
    /// The manifest has no record for this chain id at all.
    UnknownChainId { requested: u64, known: Vec<String> },
    /// The chain is recorded and its `factory` is `null`. The load-bearing one.
    NoCanonicalFactory {
        chain_id: u64,
        name: String,
        /// What the caller could do instead, phrased for the subcommand that
        /// asked. The refusal itself is identical either way.
        alternatives: &'static str,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Unreadable { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            ManifestError::Malformed { path, detail } => {
                write!(
                    f,
                    "{} is not readable as a deployments manifest: {detail}",
                    path.display()
                )
            }
            ManifestError::UnsupportedSchema { found } => write!(
                f,
                "{MANIFEST_PATH} declares schema {found}, and this rub3 CLI was written against \
                 schema {SUPPORTED_SCHEMA}. Update the CLI rather than guessing at the new shape."
            ),
            ManifestError::UnknownChainName { requested, known } => write!(
                f,
                "unknown chain `{requested}`. {MANIFEST_PATH} names {}. \
                 Pass one of those names, or a chain id.",
                known.join(", ")
            ),
            ManifestError::UnknownChainId { requested, known } => write!(
                f,
                "{MANIFEST_PATH} has no record for chain {requested}. It answers for {}.",
                known.join(", ")
            ),
            ManifestError::NoCanonicalFactory {
                chain_id,
                name,
                alternatives,
            } => write!(
                f,
                "no canonical Rub3Factory is published for chain {name} ({chain_id}).\n\n\
                 {MANIFEST_PATH} records `factory: null` there, and null is the only marker that \
                 file uses for \"not deployed\": there is deliberately no placeholder address, no \
                 zero address and no TBD string in it, and a consumer must not substitute one. \
                 Nothing is deployed to any public network yet, so there is no canonical address \
                 to use.\n\n{alternatives}"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

/// What `pack` offers an operator who has no canonical factory to point at.
pub const PACK_ALTERNATIVES: &str =
    "A packed binary names the factory it treats as canonical, so this build cannot be produced \
     yet. To build against a factory you deployed yourself - a local anvil or a testnet - name it \
     with --factory <address>. That binary claims nothing about canonicity, which is the honest \
     answer while nothing is deployed.";

/// The same, for `deploy`.
pub const DEPLOY_ALTERNATIVES: &str =
    "To deploy through a factory you deployed yourself, name it with --factory <address>. To \
     deploy directly on purpose, pass --direct: the contract works and carries no protocol fee, \
     but no factory records it, so the registry and the marketplace cannot list it.";

/// How a chain was named on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainSelector {
    /// A name from the manifest, such as `base`.
    Name(String),
    /// A chain id. Unambiguous on its own, so it needs no lookup to be
    /// understood - only to be given a canonical factory.
    Id(u64),
}

impl ChainSelector {
    /// Parses `--chain`. A bare decimal number is an id; anything else is a
    /// name, which has meaning only if the manifest publishes it.
    pub fn parse(value: &str) -> ChainSelector {
        match value.parse::<u64>() {
            Ok(id) => ChainSelector::Id(id),
            Err(_) => ChainSelector::Name(value.to_string()),
        }
    }
}

/// A chain the CLI has settled on: an id, and the name to say it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub id: u64,
    /// The manifest's name for it, or the id rendered as text for a chain the
    /// manifest does not answer for.
    pub name: String,
    /// Whether the manifest carries a record for it at all.
    pub in_manifest: bool,
}

impl Manifest {
    /// Reads the manifest out of a checkout.
    pub fn read(repo_root: &Path) -> Result<Manifest, ManifestError> {
        let path = repo_root.join(MANIFEST_PATH);
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Unreadable {
            path: path.clone(),
            source,
        })?;
        Manifest::parse(&text).map_err(|detail| match detail {
            ParseFailure::Schema(found) => ManifestError::UnsupportedSchema { found },
            ParseFailure::Shape(detail) => ManifestError::Malformed { path, detail },
        })
    }

    /// Parses manifest text. Split from [`Manifest::read`] so a fixture is one
    /// string away.
    pub fn parse(text: &str) -> Result<Manifest, ParseFailure> {
        let file: ManifestFile =
            serde_json::from_str(text).map_err(|e| ParseFailure::Shape(e.to_string()))?;
        if file.schema != SUPPORTED_SCHEMA {
            return Err(ParseFailure::Schema(file.schema));
        }
        let mut chains = BTreeMap::new();
        for (key, record) in file.chains {
            let chain_id: u64 = key
                .parse()
                .map_err(|_| ParseFailure::Shape(format!("chain key `{key}` is not a chain id")))?;
            chains.insert(
                chain_id,
                ChainRecord {
                    chain_id,
                    name: record.name,
                    factory: record.factory,
                    code_registry: record.code_registry,
                },
            );
        }
        Ok(Manifest { chains })
    }

    /// Every chain the manifest answers for, as `name (id)`, for error text.
    pub fn known(&self) -> Vec<String> {
        self.chains
            .values()
            .map(|c| format!("{} ({})", c.name, c.chain_id))
            .collect()
    }

    /// The record for a chain id, if the manifest has one.
    pub fn record(&self, chain_id: u64) -> Option<&ChainRecord> {
        self.chains.get(&chain_id)
    }

    /// Turns `--chain` into a chain.
    ///
    /// A **name** must be one the manifest publishes: names mean nothing
    /// outside it, and inventing an id for one would be exactly the guess this
    /// module exists to refuse. An **id** is taken at face value, because it is
    /// already the unambiguous form - the manifest is consulted for the
    /// canonical factory, not for permission to address a chain.
    pub fn resolve_chain(&self, selector: &ChainSelector) -> Result<Chain, ManifestError> {
        match selector {
            ChainSelector::Name(name) => self
                .chains
                .values()
                .find(|c| &c.name == name)
                .map(|c| Chain {
                    id: c.chain_id,
                    name: c.name.clone(),
                    in_manifest: true,
                })
                .ok_or_else(|| ManifestError::UnknownChainName {
                    requested: name.clone(),
                    known: self.known(),
                }),
            ChainSelector::Id(id) => Ok(match self.chains.get(id) {
                Some(record) => Chain {
                    id: *id,
                    name: record.name.clone(),
                    in_manifest: true,
                },
                None => Chain {
                    id: *id,
                    name: id.to_string(),
                    in_manifest: false,
                },
            }),
        }
    }

    /// The canonical `Rub3Factory` for a chain, or a refusal naming it.
    ///
    /// `alternatives` is the way out this subcommand offers; it never changes
    /// what the answer is, only what the operator is told to do about it.
    pub fn canonical_factory(
        &self,
        chain: &Chain,
        alternatives: &'static str,
    ) -> Result<String, ManifestError> {
        let Some(record) = self.chains.get(&chain.id) else {
            return Err(ManifestError::UnknownChainId {
                requested: chain.id,
                known: self.known(),
            });
        };
        match record.factory.as_deref() {
            // Validated rather than trusted: this address is about to be
            // compiled into somebody else's binary, and the one thing that must
            // never reach it is a placeholder.
            Some(address) => match validate_address(address) {
                Ok(()) => Ok(address.to_string()),
                Err(detail) => Err(ManifestError::Malformed {
                    path: PathBuf::from(MANIFEST_PATH),
                    detail: format!("chain {} publishes a factory that {detail}", record.name),
                }),
            },
            None => Err(ManifestError::NoCanonicalFactory {
                chain_id: record.chain_id,
                name: record.name.clone(),
                alternatives,
            }),
        }
    }
}

/// Why parsing failed, before the path is known.
#[derive(Debug)]
pub enum ParseFailure {
    /// A schema version this CLI does not implement.
    Schema(u32),
    /// Anything else.
    Shape(String),
}

/// Accepts exactly 20 hex-encoded bytes, and refuses the zero address.
///
/// The zero address is refused wherever an address is required because every
/// consumer in this project reads it as "nothing configured": the wrapper skips
/// its ownership check on a zero `CONTRACT`, and `forge` reads a zero `FACTORY`
/// as "deploy directly". A value that silently degrades a gate into no gate is
/// the failure `contracts/deployments.json` was written to prevent, so it is
/// refused at every point one could enter.
pub fn validate_address(value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("0x") else {
        return Err(format!("`{value}` is not 0x-prefixed"));
    };
    if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "`{value}` is not an address: expected 0x followed by 40 hex digits"
        ));
    }
    if hex.bytes().all(|b| b == b'0') {
        return Err(
            "is the zero address, which every consumer here reads as \"nothing configured\""
                .to_string(),
        );
    }
    Ok(())
}

// ── The file's shape ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct ManifestFile {
    schema: u32,
    chains: BTreeMap<String, ChainRecordFile>,
}

#[derive(serde::Deserialize)]
struct ChainRecordFile {
    name: String,
    factory: Option<String>,
    code_registry: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed manifest, compiled in so a test can assert on the real
    /// thing rather than on a fixture that agrees with it by hand.
    const COMMITTED: &str = include_str!("../../../contracts/deployments.json");

    fn committed() -> Manifest {
        Manifest::parse(COMMITTED).expect("the committed manifest parses")
    }

    fn populated() -> Manifest {
        Manifest::parse(include_str!("../tests/fixtures/deployments-populated.json"))
            .expect("the fixture parses")
    }

    #[test]
    fn every_committed_entry_is_still_null() {
        // The premise of every refusal below. When this fails, something is
        // deployed, and the tests that assert a refusal need rereading rather
        // than deleting.
        for record in committed().chains.values() {
            assert_eq!(
                record.factory, None,
                "chain {} publishes a factory; the pack path is no longer inert",
                record.name
            );
        }
    }

    #[test]
    fn a_null_factory_is_refused_and_the_refusal_names_the_chain() {
        let manifest = committed();
        let chain = manifest
            .resolve_chain(&ChainSelector::Name("base".into()))
            .unwrap();
        let err = manifest
            .canonical_factory(&chain, PACK_ALTERNATIVES)
            .unwrap_err();
        assert!(
            matches!(
                err,
                ManifestError::NoCanonicalFactory { chain_id: 8453, .. }
            ),
            "expected a canonical-factory refusal, got {err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("base"), "{text}");
        assert!(text.contains("8453"), "{text}");
        assert!(text.contains(MANIFEST_PATH), "{text}");
        assert!(
            !text.contains("0x0000"),
            "a refusal must not offer an address: {text}"
        );
    }

    #[test]
    fn a_name_resolves_to_the_id_the_manifest_keys_it_by() {
        let manifest = committed();
        for (name, id) in [("base", 8453), ("base_sepolia", 84532)] {
            let chain = manifest
                .resolve_chain(&ChainSelector::Name(name.into()))
                .unwrap();
            assert_eq!(chain.id, id);
            assert_eq!(chain.name, name);
            assert!(chain.in_manifest);
        }
    }

    #[test]
    fn an_unknown_name_is_refused_rather_than_guessed_at() {
        let err = committed()
            .resolve_chain(&ChainSelector::Name("optimism".into()))
            .unwrap_err();
        assert!(matches!(err, ManifestError::UnknownChainName { .. }));
        let text = err.to_string();
        assert!(text.contains("optimism"), "{text}");
        assert!(
            text.contains("base (8453)"),
            "the refusal lists what it does know: {text}"
        );
    }

    #[test]
    fn an_id_needs_no_manifest_record_to_be_addressable() {
        // A local anvil is a real chain to build against; it is simply one no
        // factory is canonical on. The refusal for that comes from
        // `canonical_factory`, not from being unable to name the chain.
        let manifest = committed();
        let chain = manifest.resolve_chain(&ChainSelector::Id(31337)).unwrap();
        assert_eq!(chain.id, 31337);
        assert!(!chain.in_manifest);
        assert!(matches!(
            manifest.canonical_factory(&chain, PACK_ALTERNATIVES),
            Err(ManifestError::UnknownChainId {
                requested: 31337,
                ..
            })
        ));
    }

    #[test]
    fn a_populated_entry_resolves_to_the_address_it_publishes() {
        let manifest = populated();
        let chain = manifest
            .resolve_chain(&ChainSelector::Name("base".into()))
            .unwrap();
        assert_eq!(
            manifest
                .canonical_factory(&chain, PACK_ALTERNATIVES)
                .unwrap(),
            "0xf4c70a7000000000000000000000000000000001"
        );
        // The second chain in the same fixture is still null, because the two
        // records have independent lifecycles and one publishing says nothing
        // about the other.
        let sepolia = manifest
            .resolve_chain(&ChainSelector::Name("base_sepolia".into()))
            .unwrap();
        assert!(matches!(
            manifest.canonical_factory(&sepolia, PACK_ALTERNATIVES),
            Err(ManifestError::NoCanonicalFactory { .. })
        ));
    }

    #[test]
    fn a_factory_that_is_a_placeholder_is_refused_as_malformed() {
        // Belt and braces for the rule the manifest states about itself: even
        // if a zero address were committed, nothing downstream may treat it as
        // an address.
        let text = COMMITTED.replace(
            "\"factory\": null",
            "\"factory\": \"0x0000000000000000000000000000000000000000\"",
        );
        let manifest = Manifest::parse(&text).unwrap();
        let chain = manifest
            .resolve_chain(&ChainSelector::Name("base".into()))
            .unwrap();
        let err = manifest
            .canonical_factory(&chain, PACK_ALTERNATIVES)
            .unwrap_err();
        assert!(matches!(err, ManifestError::Malformed { .. }), "{err:?}");
        assert!(err.to_string().contains("zero address"), "{err}");
    }

    #[test]
    fn a_schema_bump_is_refused_rather_than_read_anyway() {
        let text = COMMITTED.replace("\"schema\": 1", "\"schema\": 2");
        assert!(matches!(
            Manifest::parse(&text),
            Err(ParseFailure::Schema(2))
        ));
    }

    #[test]
    fn addresses_are_validated_the_same_way_everywhere() {
        assert!(validate_address("0xf4c70a7000000000000000000000000000000001").is_ok());
        assert!(validate_address("f4c70a7000000000000000000000000000000001").is_err());
        assert!(validate_address("0xdeadbeef").is_err());
        assert!(validate_address("0x0000000000000000000000000000000000000000").is_err());
        assert!(validate_address("0xf4c70a700000000000000000000000000000000g").is_err());
    }
}
