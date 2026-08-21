//! `rub3 deploy`: a licence contract, through the canonical factory by default
//! (implementation.md §2.5).
//!
//! It drives `contracts/script/Deploy.s.sol` rather than building the
//! transaction itself. That script is the deploy path the contract tests, the
//! walkthrough in `contracts/contracts.md` and CI all exercise, and a second
//! implementation in Rust would be a second thing to keep correct about
//! constructor argument order, the identity model's conditions, and which
//! payment rail a price belongs to. What this command adds is the part a
//! `forge script` invocation cannot do for itself: resolving the canonical
//! factory out of `contracts/deployments.json`, refusing when there is none,
//! and making sure nothing the operator's environment happens to hold reaches
//! the script by accident.
//!
//! **`FACTORY` is why that last part matters.** `vm.envOr` reads a value it
//! cannot parse exactly as it reads an unset one, so a stray `null` or a
//! half-substituted placeholder does not fail the deploy - it produces a
//! direct, unrecorded contract that carries no protocol fee and that the
//! registry and the marketplace will never list. Every variable the script
//! reads is therefore either set to a validated value here or removed from the
//! child's environment outright.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;

use crate::deployments::{
    validate_address, Chain, ChainSelector, Manifest, ManifestError, DEPLOY_ALTERNATIVES,
};
use crate::repo::Repo;

/// Every environment variable `contracts/script/Deploy.s.sol` reads.
///
/// Listed so the ones this command does not set can be *removed* rather than
/// inherited. A `PRICE` left over from an earlier `source .env` is a deploy
/// nobody asked for.
const SCRIPT_VARS: &[&str] = &[
    "TOKEN_NAME",
    "TOKEN_SYMBOL",
    "IDENTITY_MODEL",
    "TBA_IMPLEMENTATION",
    "PRICE",
    "PRICE_TOKEN",
    "PRICE_AMOUNT",
    "SUPPLY_CAP",
    "COOLDOWN_BLOCKS",
    "OWNER",
    "PREDECESSOR",
    "FACTORY",
    "WRAPPER_HASH",
    "WRAPPER_HASHES",
];

/// Deploy a licence contract through the canonical Rub3Factory.
#[derive(Debug, clap::Args)]
pub struct DeployArgs {
    /// ERC-721 collection name, such as "My App License".
    #[arg(long, value_name = "NAME")]
    pub name: String,

    /// ERC-721 collection symbol, such as MAL.
    #[arg(long, value_name = "SYMBOL")]
    pub symbol: String,

    /// Identity model: `access` keys users by wallet, `account` keys them by
    /// the token's ERC-6551 token-bound account. Frozen at deploy.
    #[arg(long, value_name = "MODEL", default_value = "access")]
    pub identity: String,

    /// ERC-6551 account implementation. Required by --identity account, and
    /// refused by --identity access.
    #[arg(long, value_name = "ADDRESS")]
    pub tba_implementation: Option<String>,

    /// Chain to deploy to: a name from contracts/deployments.json, such as
    /// `base`, or a chain id.
    #[arg(long, value_name = "NAME|ID")]
    pub chain: String,

    /// Price in ETH, as a decimal number.
    #[arg(long, value_name = "ETH", conflicts_with = "price_wei")]
    pub price_eth: Option<String>,

    /// Price in wei, exactly.
    #[arg(long, value_name = "WEI")]
    pub price_wei: Option<String>,

    /// Price in USDC, as a decimal number. Configures the EIP-3009 rail, so it
    /// needs --price-token naming the token that rail settles in.
    #[arg(long, value_name = "USDC", conflicts_with = "price_amount")]
    pub price_usdc: Option<String>,

    /// Price in the payment token's smallest unit, exactly. For a token whose
    /// decimals are not USDC's six.
    #[arg(long, value_name = "UNITS")]
    pub price_amount: Option<String>,

    /// The EIP-3009 token the stablecoin rail settles in, such as USDC on the
    /// target chain. rub3 publishes no token address, so this is never guessed
    /// from --price-usdc.
    #[arg(long, value_name = "ADDRESS")]
    pub price_token: Option<String>,

    /// Maximum number of licences mintable. Omitted means uncapped.
    #[arg(long, value_name = "N")]
    pub supply_cap: Option<u64>,

    /// Blocks between activations of one token. The contract's floor is 15.
    #[arg(long, value_name = "N")]
    pub cooldown_blocks: Option<u64>,

    /// Contract owner. Defaults to the deploying key.
    #[arg(long, value_name = "ADDRESS")]
    pub owner: Option<String>,

    /// A licence contract whose holders may migrate onto this one. Frozen at
    /// deploy, and through a factory it must be a contract that factory
    /// recorded.
    #[arg(long, value_name = "ADDRESS")]
    pub predecessor: Option<String>,

    /// sha-256 of a packed wrapper binary, as `rub3 pack` prints it. Repeatable,
    /// once per platform in the launch release. More can be added later with
    /// addWrapperHash; the set is append-only.
    #[arg(long = "wrapper-hash", value_name = "0xHASH")]
    pub wrapper_hashes: Vec<String>,

    /// Deploy through a factory of your own instead of the canonical one, such
    /// as one you deployed on a local anvil.
    #[arg(long, value_name = "ADDRESS", conflicts_with = "direct")]
    pub factory: Option<String>,

    /// Deploy through no factory at all. The contract works and carries no
    /// protocol fee, and no factory records it, so the registry and the
    /// marketplace cannot list it. A deliberate choice, never a fallback.
    #[arg(long)]
    pub direct: bool,

    /// JSON-RPC endpoint. Defaults to the chain's alias in
    /// contracts/foundry.toml, which resolves it from the environment.
    #[arg(long, value_name = "URL")]
    pub rpc_url: Option<String>,

    /// Actually send the transaction. Without it forge simulates the deploy and
    /// broadcasts nothing.
    #[arg(long)]
    pub broadcast: bool,

    /// Resolve everything and print what would run, without running it.
    #[arg(long)]
    pub dry_run: bool,

    /// Arguments passed straight to `forge script`. Everything after an
    /// explicit `--` and nothing before it, so a rub3 flag is never mistaken
    /// for one of these: this is where the signer goes, and a `--dry-run`
    /// swallowed into it would broadcast instead of simulating.
    #[arg(
        last = true,
        num_args = 0..,
        allow_hyphen_values = true,
        value_name = "FORGE ARGS"
    )]
    pub forge_args: Vec<String>,
}

/// A resolved deploy: the script's whole environment, and the forge invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlan {
    /// What the script reads. Anything absent here is removed from the child's
    /// environment rather than inherited.
    pub env: BTreeMap<String, String>,
    /// The arguments after `forge` that this command chose: the script, the
    /// endpoint, and `--broadcast` when it was asked for.
    pub forge_args: Vec<String>,
    /// What the operator put after `--`, handed to forge untouched.
    ///
    /// Kept apart from [`DeployPlan::forge_args`] so that [`DeployPlan::render`]
    /// can report the invocation without reporting these: this is where the
    /// signer goes, and the summary is printed on every deploy, not only a
    /// `--dry-run`, so echoing it would put an expanded `--private-key` in
    /// terminal scrollback and in any CI log that captures the step.
    pub passthrough: Vec<String>,
    /// The chain, for the summary.
    pub chain: Chain,
    /// How the factory was decided.
    pub factory: FactoryChoice,
    /// Whether this plan broadcasts.
    pub broadcast: bool,
}

/// Which factory a deploy goes through, and on whose say-so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactoryChoice {
    /// Read from `contracts/deployments.json`.
    Canonical(String),
    /// Named with `--factory`.
    Explicit(String),
    /// `--direct`: no factory, no fee, recorded nowhere.
    None,
}

/// Why a deploy could not be prepared or did not finish.
#[derive(Debug)]
pub enum DeployError {
    /// The manifest refused, most often because no canonical factory exists.
    Manifest(ManifestError),
    /// An input is wrong or two inputs contradict each other.
    Config(String),
    /// `forge` could not be run, or reported failure.
    Forge(String),
}

impl fmt::Display for DeployError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeployError::Manifest(e) => write!(f, "{e}"),
            DeployError::Config(message) => write!(f, "{message}"),
            DeployError::Forge(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for DeployError {}

impl From<ManifestError> for DeployError {
    fn from(e: ManifestError) -> Self {
        DeployError::Manifest(e)
    }
}

impl DeployPlan {
    /// Turns the command line into a `forge script` invocation and the
    /// environment it reads.
    pub fn resolve(args: &DeployArgs, manifest: &Manifest) -> Result<DeployPlan, DeployError> {
        let chain = manifest.resolve_chain(&ChainSelector::parse(&args.chain))?;

        if let Some(address) = &args.factory {
            validate_address(address)
                .map_err(|detail| DeployError::Config(format!("--factory {detail}")))?;
        }

        let identity_model = match args.identity.as_str() {
            "access" => 0u8,
            "account" => 1u8,
            other => {
                return Err(DeployError::Config(format!(
                    "unknown --identity `{other}`: expected access (user_id is the wallet) or \
                     account (user_id is the token's ERC-6551 account)"
                )))
            }
        };

        let mut env = BTreeMap::new();
        env.insert("TOKEN_NAME".to_string(), args.name.clone());
        env.insert("TOKEN_SYMBOL".to_string(), args.symbol.clone());
        env.insert("IDENTITY_MODEL".to_string(), identity_model.to_string());

        match (identity_model, &args.tba_implementation) {
            (1, Some(address)) => {
                validate_address(address).map_err(|detail| {
                    DeployError::Config(format!("--tba-implementation {detail}"))
                })?;
                env.insert("TBA_IMPLEMENTATION".to_string(), address.clone());
            }
            (1, None) => {
                return Err(DeployError::Config(
                    "--identity account needs --tba-implementation <address>: the account model \
                     derives every user's identity from that ERC-6551 implementation, and it is \
                     immutable once deployed."
                        .into(),
                ))
            }
            (0, Some(_)) => {
                return Err(DeployError::Config(
                    "--tba-implementation belongs to --identity account. The access model keys \
                     users by wallet and the constructor rejects an implementation address."
                        .into(),
                ))
            }
            _ => {}
        }

        // The ETH rail is always present, so the script always reads PRICE. A
        // listing with no ETH price is zero, not absent.
        let price_wei = match (&args.price_eth, &args.price_wei) {
            (Some(eth), None) => decimal_to_units(eth, 18)
                .map_err(|e| DeployError::Config(format!("--price-eth {e}")))?,
            (None, Some(wei)) => {
                integer(wei).map_err(|e| DeployError::Config(format!("--price-wei {e}")))?
            }
            (None, None) => "0".to_string(),
            (Some(_), Some(_)) => unreachable!("clap rejects both"),
        };
        env.insert("PRICE".to_string(), price_wei);

        // The stablecoin rail is opt-in, and its two halves travel together:
        // the amount is in the token's own units, so it means nothing without
        // the token.
        let price_amount = match (&args.price_usdc, &args.price_amount) {
            (Some(usdc), None) => Some(
                decimal_to_units(usdc, 6)
                    .map_err(|e| DeployError::Config(format!("--price-usdc {e}")))?,
            ),
            (None, Some(units)) => Some(
                integer(units).map_err(|e| DeployError::Config(format!("--price-amount {e}")))?,
            ),
            (None, None) => None,
            (Some(_), Some(_)) => unreachable!("clap rejects both"),
        };
        match (&args.price_token, price_amount) {
            (Some(token), Some(amount)) => {
                validate_address(token)
                    .map_err(|detail| DeployError::Config(format!("--price-token {detail}")))?;
                env.insert("PRICE_TOKEN".to_string(), token.clone());
                env.insert("PRICE_AMOUNT".to_string(), amount);
            }
            (Some(token), None) => {
                validate_address(token)
                    .map_err(|detail| DeployError::Config(format!("--price-token {detail}")))?;
                // A token with no amount is a free stablecoin tier, which the
                // contract allows; say so rather than guessing at a price.
                env.insert("PRICE_TOKEN".to_string(), token.clone());
                env.insert("PRICE_AMOUNT".to_string(), "0".to_string());
            }
            (None, Some(_)) => {
                return Err(DeployError::Config(
                    "a stablecoin price needs --price-token <address>. rub3 publishes no token \
                     address for any chain, and the payment token is exactly the kind of address \
                     that must never be guessed - pass the EIP-3009 token this listing settles \
                     in."
                    .into(),
                ))
            }
            (None, None) => {}
        }

        if let Some(cap) = args.supply_cap {
            env.insert("SUPPLY_CAP".to_string(), cap.to_string());
        }
        if let Some(blocks) = args.cooldown_blocks {
            env.insert("COOLDOWN_BLOCKS".to_string(), blocks.to_string());
        }
        if let Some(owner) = &args.owner {
            validate_address(owner)
                .map_err(|detail| DeployError::Config(format!("--owner {detail}")))?;
            env.insert("OWNER".to_string(), owner.clone());
        }
        if let Some(predecessor) = &args.predecessor {
            validate_address(predecessor)
                .map_err(|detail| DeployError::Config(format!("--predecessor {detail}")))?;
            env.insert("PREDECESSOR".to_string(), predecessor.clone());
        }

        if !args.wrapper_hashes.is_empty() {
            for hash in &args.wrapper_hashes {
                validate_hash(hash)
                    .map_err(|detail| DeployError::Config(format!("--wrapper-hash {detail}")))?;
            }
            env.insert("WRAPPER_HASHES".to_string(), args.wrapper_hashes.join(","));
        }

        let rpc_url = match (&args.rpc_url, chain.in_manifest) {
            (Some(url), _) => url.clone(),
            // foundry.toml keys its [rpc_endpoints] by the same names
            // contracts/deployments.json uses, so the chain name is a usable
            // --rpc-url on its own and resolves from the environment there.
            (None, true) => chain.name.clone(),
            (None, false) => {
                return Err(DeployError::Config(format!(
                    "chain {} has no alias in contracts/foundry.toml, so --rpc-url is required.",
                    chain.id
                )))
            }
        };

        // Last, so that every refusal above is reported as itself. Until a
        // factory is published anywhere, resolving it first would report a
        // malformed command line as the null-factory refusal, which is the one
        // signal an orchestrator reads as "nothing is deployed yet".
        let factory = match (&args.factory, args.direct) {
            (Some(address), _) => FactoryChoice::Explicit(address.clone()),
            (None, true) => FactoryChoice::None,
            (None, false) => {
                FactoryChoice::Canonical(manifest.canonical_factory(&chain, DEPLOY_ALTERNATIVES)?)
            }
        };
        match &factory {
            FactoryChoice::Canonical(address) | FactoryChoice::Explicit(address) => {
                env.insert("FACTORY".to_string(), address.clone());
            }
            FactoryChoice::None => {}
        }

        let mut forge_args = vec![
            "script".to_string(),
            "script/Deploy.s.sol".to_string(),
            "--rpc-url".to_string(),
            rpc_url,
        ];
        if args.broadcast {
            forge_args.push("--broadcast".to_string());
        }
        Ok(DeployPlan {
            env,
            forge_args,
            passthrough: args.forge_args.clone(),
            chain,
            factory,
            broadcast: args.broadcast,
        })
    }

    /// What this plan will do, for `--dry-run` and for the summary a real
    /// deploy prints before it starts.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "name:      {} ({})\n",
            self.env["TOKEN_NAME"], self.env["TOKEN_SYMBOL"]
        ));
        out.push_str(&format!(
            "chain:     {} ({})\n",
            self.chain.name, self.chain.id
        ));
        out.push_str(&format!(
            "factory:   {}\n",
            match &self.factory {
                FactoryChoice::Canonical(address) =>
                    format!("{address}  (canonical, from contracts/deployments.json)"),
                FactoryChoice::Explicit(address) =>
                    format!("{address}  (named with --factory: not the canonical factory)"),
                FactoryChoice::None =>
                    "none  (--direct: no protocol fee, and no factory records it)".to_string(),
            }
        ));
        out.push_str(&format!(
            "identity:  {}\n",
            if self.env["IDENTITY_MODEL"] == "1" {
                "account (user_id is the token's ERC-6551 account)"
            } else {
                "access (user_id is the wallet)"
            }
        ));
        out.push_str(&format!("price:     {} wei\n", self.env["PRICE"]));
        if let Some(token) = self.env.get("PRICE_TOKEN") {
            out.push_str(&format!(
                "           {} of {token}\n",
                self.env["PRICE_AMOUNT"]
            ));
        }
        out.push_str(&format!(
            "broadcast: {}\n",
            if self.broadcast {
                "yes - this sends a transaction"
            } else {
                "no - forge simulates only (pass --broadcast to send)"
            }
        ));
        out.push_str(&format!("run:       forge {}\n", self.forge_args.join(" ")));
        if !self.passthrough.is_empty() {
            let n = self.passthrough.len();
            out.push_str(&format!(
                "           plus {n} argument{} passed straight to forge, not shown here: that \
                 is where the signer goes\n",
                if n == 1 { "" } else { "s" }
            ));
        }
        out.push_str("environment:\n");
        for (key, value) in &self.env {
            out.push_str(&format!("  {key}={value}\n"));
        }
        let cleared: Vec<&str> = SCRIPT_VARS
            .iter()
            .copied()
            .filter(|var| !self.env.contains_key(*var))
            .collect();
        if !cleared.is_empty() {
            out.push_str(&format!(
                "  (removed from the child's environment: {})\n",
                cleared.join(", ")
            ));
        }
        out
    }
}

/// Runs a resolved plan: `forge script`, from `contracts/`.
pub fn execute(plan: &DeployPlan, repo: &Repo) -> Result<(), DeployError> {
    let contracts: PathBuf = repo.path("contracts");
    let mut command = Command::new("forge");
    command
        .args(&plan.forge_args)
        .args(&plan.passthrough)
        .current_dir(&contracts);
    // Set what the plan decided, and clear the rest. An inherited value is a
    // deploy input nobody typed.
    for var in SCRIPT_VARS {
        match plan.env.get(*var) {
            Some(value) => command.env(var, value),
            None => command.env_remove(var),
        };
    }

    let status = command.status().map_err(|e| {
        DeployError::Forge(format!(
            "cannot run forge in {}: {e}. Foundry is what deploys; \
             contracts/contracts.md -> \"Setup\" installs it.",
            contracts.display()
        ))
    })?;
    if !status.success() {
        return Err(DeployError::Forge(format!(
            "forge script failed ({status}). The output above is forge's."
        )));
    }
    Ok(())
}

/// Parses a decimal amount into an integer number of the smallest unit.
///
/// `20` at 6 decimals is `20000000`. More fractional digits than the unit has
/// is an error rather than a rounding: nobody means to round a price.
fn decimal_to_units(value: &str, decimals: u32) -> Result<String, String> {
    let value = value.trim();
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (value, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return Err("is empty".to_string());
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("`{value}` is not a decimal number"));
    }
    if fraction.len() as u32 > decimals {
        return Err(format!(
            "`{value}` has more than {decimals} decimal places, and there is no smaller unit to \
             round into"
        ));
    }
    let padded = format!("{fraction:0<width$}", width = decimals as usize);
    let digits = format!("{whole}{padded}");
    let trimmed = digits.trim_start_matches('0');
    Ok(if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    })
}

/// Accepts an exact integer amount, in whatever unit the caller named.
fn integer(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("`{value}` is not a whole number"));
    }
    let trimmed = value.trim_start_matches('0');
    Ok(if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    })
}

/// Accepts exactly 32 hex-encoded bytes, and refuses the zero hash.
///
/// `bytes32(0)` is the contract's `Unknown` sentinel and is rejected as a
/// member of the wrapper hash set, so catching it here says why rather than
/// leaving a revert to explain it.
fn validate_hash(value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("0x") else {
        return Err(format!("`{value}` is not 0x-prefixed"));
    };
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "`{value}` is not a sha-256: expected 0x followed by 64 hex digits"
        ));
    }
    if hex.bytes().all(|b| b == b'0') {
        return Err("is the zero hash, which the contract reads as \"unknown\"".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_price_becomes_the_smallest_unit_of_its_own_rail() {
        assert_eq!(decimal_to_units("20", 6).unwrap(), "20000000");
        assert_eq!(decimal_to_units("19.99", 6).unwrap(), "19990000");
        assert_eq!(decimal_to_units("0.05", 18).unwrap(), "50000000000000000");
        assert_eq!(decimal_to_units("0", 6).unwrap(), "0");
        assert_eq!(decimal_to_units("0.000000", 6).unwrap(), "0");
    }

    #[test]
    fn a_price_finer_than_the_unit_is_refused_rather_than_rounded() {
        let err = decimal_to_units("1.9999999", 6).unwrap_err();
        assert!(err.contains("decimal places"), "{err}");
        assert!(decimal_to_units("1.2.3", 6).is_err());
        assert!(decimal_to_units("twenty", 6).is_err());
    }

    #[test]
    fn a_wrapper_hash_must_be_a_sha256_and_not_the_sentinel() {
        assert!(validate_hash(&format!("0x{}", "ab".repeat(32))).is_ok());
        assert!(validate_hash(&format!("0x{}", "00".repeat(32))).is_err());
        assert!(validate_hash("0xabc").is_err());
        assert!(validate_hash(&"ab".repeat(32)).is_err());
    }
}
