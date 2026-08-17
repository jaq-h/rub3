//! Is this contract the code we think it is?
//!
//! An agent about to spend money on a contract it discovered has one question
//! it cannot answer from the contract's name, its ABI, or its own good
//! intentions: is the code at that address a genuine rub3 template, or a
//! modified copy of one? This module answers exactly that, from bytes it
//! fetches once.
//!
//! ## Two authorities, in that order
//!
//! **A table compiled into this binary** answers first, and answers the common
//! case for the price of the code read the gate already made. It says whether
//! the code is one of the releases this build was packed against.
//!
//! **An on-chain `Rub3CodeRegistry`** answers the rest. A table miss alone
//! cannot tell a contract deployed from a template release *newer* than this
//! binary from a modified copy - both are absent from the table - and by design
//! there will be several legitimate releases live at once. The registry is the
//! append-only record of which masked hashes are genuine rub3 releases, and it
//! is consulted only on a miss. Its own code is verified against the pinned
//! [`Role::CodeRegistry`] row before anything it says is believed, which is what
//! keeps the trust rooted in the binary the user chose to run rather than in
//! whoever deployed the registry. See [`consult_registry`].
//!
//! No registry is published on any chain yet ([`REGISTRIES`]), so today a miss
//! costs no extra chain read and is refused exactly as it was before this half
//! existed.
//!
//! ## The comparable quantity
//!
//! Plain runtime-bytecode equality does not work. Solidity `immutable`s are
//! written into the runtime code at deploy time, so two deploys of identical
//! source that chose a different `supplyCap` return different code from
//! `eth_getCode`, and a naive comparison rejects a perfectly legitimate deploy.
//!
//! The quantity that *is* stable is the **masked code hash**: zero every byte
//! range solc reserved for an immutable, then `sha256`. The compiler already
//! emits that form as `deployedBytecode.object`, which is what
//! `contracts/canonical-bytecode.json` fingerprints, so the agent-side
//! computation is "fetch code, zero the published ranges, hash" and nothing
//! more. See `contracts/contracts.md` -> "Reproducible builds and canonical
//! fingerprints" for the publishing side of the same contract.
//!
//! ## What a match proves, and what it does not
//!
//! Every range the pinned table masks is the immediate operand of a `PUSH32`
//! solc emitted, and EVM jump-destination analysis excludes bytes inside push
//! immediates, so no control flow can reach one of those bytes as an
//! instruction. Against the pinned table a masked-hash match is therefore a
//! complete statement about the contract's **executable code**: those ranges
//! arrive with the binary, fixed at build time, and there is nowhere in the
//! blind spot for hidden code to live.
//!
//! Ranges that arrive from the registry are checked rather than trusted, and
//! that check establishes less - [`ranges_are_immutable_slots`] says exactly
//! how much less.
//!
//! It is not a statement about anything else, and the limits are load-bearing:
//!
//! - **The masked values still flow into execution as data.** Canonical code
//!   configured hostilely - `identityModel == 1` pointing `tbaImplementation`
//!   at an attacker's ERC-6551 implementation, say - matches this check and is
//!   not what the buyer wanted. Reading those getters and checking them against
//!   a buyer policy is separate work and is not done here.
//! - **A canonical contract can still be operated adversarially within the
//!   rules.** `setPrice`, `setSuccessor`, `revokeWrapperHash` and `withdraw`
//!   are deliberate owner powers. A match confirms they are the *canonical*
//!   powers, not that they will be used kindly.
//! - **Everything rests on an honest view of chain state.** The whole check
//!   reduces to `eth_getCode` and `eth_call` being answered truthfully by a
//!   single endpoint baked in at pack time. An endpoint that lies returns
//!   canonical code for a hostile contract, and lies about the registry's own
//!   code in the same breath, so the second authority does not dilute this risk
//!   and does not compound it either: one dishonest endpoint defeats both at
//!   once. The honest claim is "an honest view of chain state implies canonical
//!   code", and no stronger one should be made anywhere.
//! - **The registry's owner key can add.** Append-only bounds a compromise of
//!   it to *additions*, each a permanent public event, and leaves it unable to
//!   remove, rewrite, or invalidate a record. That bounds the damage and makes
//!   it detectable; it does not prevent it.
//! - **A registry-supplied range table is checked, not proved.** The masked
//!   bytes of a *registry-vouched* release are not covered by the paragraph
//!   above: [`ranges_are_immutable_slots`] confirms each range is preceded by
//!   the `PUSH32` opcode, which identifies an immutable slot in code a compiler
//!   emitted but does not by itself prove that byte is an instruction rather
//!   than data inside an earlier push's immediate. Somebody able to shape both
//!   the code and the table could therefore mask bytes that do execute. That
//!   needs the registry's owner key, which has the shorter route of publishing
//!   an empty table and letting the hostile code be hashed whole, so this
//!   reaches nothing the bullet above does not; it is stated because the
//!   guarantee is genuinely narrower here than it is for the pinned table.
//!
//! The check is sound against replacement because `evm_version = "cancun"` and
//! Base has been on Cancun since Ecotone: under EIP-6780 `SELFDESTRUCT` only
//! deletes an account created in the same transaction, so code at an address
//! cannot be destroyed and replaced between the agent's `eth_getCode` and its
//! `purchase()`.
//!
//! ## Failure posture: closed on purchase, open on launch
//!
//! [`verify_before_purchase`] is a gate: anything other than a licence
//! vouched for by one of the two authorities stops the run before a transaction
//! is signed, including a chain read that failed and a registry that could not
//! be consulted. Refusing to spend money on code that could not be verified is
//! the correct default.
//!
//! The registry never widens that posture in the other direction either.
//! `Deprecated` is the strongest thing it can say against a release, and it
//! means "not recommended for a new purchase": the purchase proceeds with a
//! warning ([`Attestation::advisory`]). A version authority able to refuse would
//! be a revocation surface with an extra step, and the registry contract has no
//! status to publish that would make one.
//!
//! **Nothing on the launch path calls into this module, deliberately.** A
//! launch is a program the user has already paid for. Refusing to start it
//! because an integrity check could not complete would be a de-facto
//! revocation surface, which this project has ruled out (see `CLAUDE.md` ->
//! "Ownership invariants"), and it would turn an integrity check into a kill
//! switch. There is no shared helper with a flag between the two postures,
//! because a flag is one wrong default away from that outcome: the gate is
//! called from the purchase path and from nowhere else.

use alloy::primitives::Address;

use crate::rpc::{self, RpcError};

// ── The pinned table ──────────────────────────────────────────────────────────

/// What a canonical contract is *for*, which decides what may be done with it.
///
/// A masked-hash match says the code is ours; it does not say the address sells
/// licences. The factory, its two deployer helpers and the code registry are
/// canonical rub3 code and are pinned here so the table stays a total mirror of
/// the published manifest, but buying from one is a category error, and
/// [`verify_before_purchase`] refuses it as such rather than letting a
/// transaction find out on-chain.
///
/// The discriminants mirror `Rub3CodeRegistry.Role`, because a registry record
/// carries one and it arrives as the raw `uint8`. New roles are appended at both
/// ends and existing ones are never renumbered; `tests/code_registry_e2e.rs`
/// publishes every value through a deployed registry and reads it back here,
/// which is what holds the two in step - the numbering is a wire encoding, and
/// only the wire can settle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Sells and validates licences. The only role a purchase may target.
    Licence,
    /// `Rub3Factory` - deploys licence contracts, sells nothing.
    Factory,
    /// A factory-internal deployer helper, holding one licence template's
    /// creation code. Pinned so a factory's declared `accessDeployer()` /
    /// `subscriptionDeployer()` can be checked against it.
    Deployer,
    /// `Rub3CodeRegistry` - the version authority this module consults when its
    /// own table misses. Pinned because its answer is believed only after its
    /// code hashes to this entry, which is what stops the trust from resting on
    /// whoever deployed it.
    CodeRegistry,
}

impl Role {
    /// The role a registry record's raw `uint8` names, or `None` for a value
    /// this build has no name for.
    ///
    /// A registry newer than the binary can publish a role this build has never
    /// heard of, and the honest answer to that is "I do not know what this is",
    /// never "it is the first variant". The caller refuses on `None`, because
    /// the only role a purchase may target is one this build can recognise.
    fn from_u8(value: u8) -> Option<Role> {
        match value {
            0 => Some(Role::Licence),
            1 => Some(Role::Factory),
            2 => Some(Role::Deployer),
            3 => Some(Role::CodeRegistry),
            _ => None,
        }
    }

    /// What this role is, in a refusal a person or an orchestrator reads.
    fn describe(self) -> &'static str {
        match self {
            Role::Licence => "licence contract",
            Role::Factory => "deploy factory",
            Role::Deployer => "factory-internal deployer helper",
            Role::CodeRegistry => "code registry",
        }
    }
}

/// One byte range of the runtime code that solc reserved for an immutable, and
/// that a comparator must therefore zero before hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImmutableRange {
    /// Byte offset into the runtime code.
    pub start: usize,
    /// Width in bytes. Always 32 - one EVM word - for a Solidity immutable.
    pub length: usize,
}

/// One contract this binary recognises as canonical.
///
/// The fields other than `role` and `release` mirror
/// `contracts/canonical-bytecode.json` exactly, and
/// [`tests::pinned_table_mirrors_the_canonical_manifest`] fails if they drift
/// apart. That test is the drift protection the published record needs: without
/// it a contract change would move the manifest while this table went on
/// asserting a hash nothing produces any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalContract {
    /// Solidity contract name, as the manifest keys it.
    pub contract: &'static str,
    /// Repo-relative path of the file declaring it.
    pub source: &'static str,
    /// What the contract is for.
    pub role: Role,
    /// Which contract release produced this fingerprint. One row per contract
    /// for now; from the first deploy onwards rows accumulate, because a
    /// contract deployed at a fingerprint goes on validating its own tokens
    /// forever. [`CANONICAL`] states when that turns on.
    pub release: &'static str,
    /// `sha256` of the runtime code with `immutable_ranges` zeroed, lowercase
    /// hex, no `0x` prefix.
    pub masked_sha256: &'static str,
    /// The ranges to zero, sorted by offset.
    pub immutable_ranges: &'static [ImmutableRange],
}

/// The contract release the current `contracts/` produces, and so the label a
/// freshly generated row carries.
///
/// A label, not a version the code branches on. It exists so a refusal or an
/// audit log can say *which* canonical contract matched, once this table holds
/// more than one release of the same contract.
const RELEASE: &str =
    "2026-08, contracts at implementation.md §2.2 (exact payment on the ETH rail)";

/// Every contract this binary accepts as canonical rub3 code.
///
/// Generated from `contracts/canonical-bytecode.json`, which the blocking
/// `bytecode-fingerprints` CI job regenerates and diffs on every pull request.
/// To refresh after a legitimate contract change: run
/// `scripts/canonical-bytecode-hashes.sh update`, then add the new rows here.
///
/// **Accumulate, do not overwrite: once a contract is deployed at a
/// fingerprint, its row here is permanent.** A deployed contract goes on
/// selling and validating its own tokens after its source has moved on, so
/// dropping its row would refuse code this project itself deployed and sold
/// from.
///
/// That rule is conditional on a deploy having happened, and nothing is
/// deployed to any public network yet - the contracts do not reach mainnet or
/// get declared ready for use until the registry ships, and the factory and the
/// registry launch together (`implementation.md` §1.5, §2.3). That condition
/// has a machine-checked form: `contracts/deployments.json`, whose every
/// `factory` **and** every `code_registry` is still `null`, asserted by
/// [`tests::nothing_is_deployed_so_the_accumulate_only_rule_is_not_live_yet`].
/// Those two are separate records with separate lifecycles, checked
/// independently by `scripts/check-deployments.sh`, so either can be published
/// without the other: the rule goes live for a chain as soon as **either** of
/// them stops being `null`, and it goes live for whichever contracts that
/// deploy actually put on chain. A `Rub3CodeRegistry` published while `factory`
/// is still `null` makes the registry's own row here permanent, and overwriting
/// it after that would leave every build carrying the new row refusing the
/// deployed registry as not the code registry it pins - which costs every
/// pinned-table miss on that chain its second authority. Before the first
/// deploy a superseded fingerprint protects no holder, while it widens the set
/// of code the wrapper will spend money on, which is the opposite of what this
/// table is for; so the pre-exact-payment rows of the §2.3 contracts were
/// dropped rather than carried. That was a one-off taken while the condition above was
/// still false, not licence to prune the table again.
///
/// Rows are grouped by contract, newest release first, so a contract's history
/// reads top to bottom.
// One range per line, kept that way on purpose: this table is reviewed against
// contracts/canonical-bytecode.json by eye as well as by test, and rustfmt's
// four-lines-per-range expansion buries 52 offsets in 200 lines of braces.
#[rustfmt::skip]
pub static CANONICAL: &[CanonicalContract] = &[
    CanonicalContract {
        contract: "Rub3Access",
        source: "contracts/src/Rub3Access.sol",
        role: Role::Licence,
        release: RELEASE,
        masked_sha256: "f0da5f8622b8f5f293876d3acbddda1d56a0abdbc8c4fbf50ebb26f0b245e4ae",
        immutable_ranges: &[
            ImmutableRange { start: 1185, length: 32 },
            ImmutableRange { start: 1443, length: 32 },
            ImmutableRange { start: 1627, length: 32 },
            ImmutableRange { start: 1922, length: 32 },
            ImmutableRange { start: 2210, length: 32 },
            ImmutableRange { start: 2379, length: 32 },
            ImmutableRange { start: 2660, length: 32 },
            ImmutableRange { start: 4508, length: 32 },
            ImmutableRange { start: 5283, length: 32 },
            ImmutableRange { start: 5924, length: 32 },
            ImmutableRange { start: 5981, length: 32 },
            ImmutableRange { start: 6154, length: 32 },
            ImmutableRange { start: 6198, length: 32 },
            ImmutableRange { start: 6673, length: 32 },
            ImmutableRange { start: 6819, length: 32 },
            ImmutableRange { start: 8659, length: 32 },
            ImmutableRange { start: 8701, length: 32 },
            ImmutableRange { start: 10009, length: 32 },
        ],
    },
    CanonicalContract {
        contract: "Rub3AccessDeployer",
        source: "contracts/src/Rub3Factory.sol",
        role: Role::Deployer,
        release: RELEASE,
        masked_sha256: "6a1e8ae8ec90d5c2204184b92ea15111f6cf4889a05cd8316e8758a190b7647a",
        immutable_ranges: &[],
    },
    CanonicalContract {
        contract: "Rub3CodeRegistry",
        source: "contracts/src/Rub3CodeRegistry.sol",
        role: Role::CodeRegistry,
        release: RELEASE,
        masked_sha256: "71c031334bbe5918f71922e0e6b7577ae0ee8c4d2626fe714aa536aa18b06e5c",
        immutable_ranges: &[],
    },
    CanonicalContract {
        contract: "Rub3Factory",
        source: "contracts/src/Rub3Factory.sol",
        role: Role::Factory,
        release: RELEASE,
        masked_sha256: "0b03cc18efb8dd487b52d57bc28b7a3e18bb7575f6575c20810c8517d77f4076",
        immutable_ranges: &[
            ImmutableRange { start: 249, length: 32 },
            ImmutableRange { start: 338, length: 32 },
            ImmutableRange { start: 433, length: 32 },
            ImmutableRange { start: 472, length: 32 },
            ImmutableRange { start: 643, length: 32 },
            ImmutableRange { start: 802, length: 32 },
            ImmutableRange { start: 998, length: 32 },
            ImmutableRange { start: 1239, length: 32 },
            ImmutableRange { start: 1720, length: 32 },
            ImmutableRange { start: 1759, length: 32 },
            ImmutableRange { start: 2010, length: 32 },
            ImmutableRange { start: 2049, length: 32 },
        ],
    },
    CanonicalContract {
        contract: "Rub3Subscription",
        source: "contracts/src/Rub3Subscription.sol",
        role: Role::Licence,
        release: RELEASE,
        masked_sha256: "2eab4428ee29c805b4e151a0068673910e7ea4a8e45a760b61ec745f1aba65a1",
        immutable_ranges: &[
            ImmutableRange { start: 1284, length: 32 },
            ImmutableRange { start: 1628, length: 32 },
            ImmutableRange { start: 1812, length: 32 },
            ImmutableRange { start: 2169, length: 32 },
            ImmutableRange { start: 2488, length: 32 },
            ImmutableRange { start: 2657, length: 32 },
            ImmutableRange { start: 2990, length: 32 },
            ImmutableRange { start: 3346, length: 32 },
            ImmutableRange { start: 4973, length: 32 },
            ImmutableRange { start: 5906, length: 32 },
            ImmutableRange { start: 6557, length: 32 },
            ImmutableRange { start: 6614, length: 32 },
            ImmutableRange { start: 6784, length: 32 },
            ImmutableRange { start: 6828, length: 32 },
            ImmutableRange { start: 7303, length: 32 },
            ImmutableRange { start: 7449, length: 32 },
            ImmutableRange { start: 7984, length: 32 },
            ImmutableRange { start: 8726, length: 32 },
            ImmutableRange { start: 9736, length: 32 },
            ImmutableRange { start: 9778, length: 32 },
            ImmutableRange { start: 9882, length: 32 },
            ImmutableRange { start: 11420, length: 32 },
        ],
    },
    CanonicalContract {
        contract: "Rub3SubscriptionDeployer",
        source: "contracts/src/Rub3Factory.sol",
        role: Role::Deployer,
        release: RELEASE,
        masked_sha256: "218f4165d090e7b7109507b178a50a03b1ad63fb65213ec10ffecc19a4de4429",
        immutable_ranges: &[],
    },
];

// ── The registry this binary will ask, per chain ──────────────────────────────

/// Where the `Rub3CodeRegistry` lives on one chain, or that it does not live
/// there yet.
///
/// The second half of the trust root. The first is the registry's own masked
/// hash, which is an ordinary [`CANONICAL`] row with [`Role::CodeRegistry`]; a
/// pinned address without a pinned hash would be an address trusted because
/// somebody wrote it down, which is the thing this module exists not to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryDeployment {
    /// The chain this answers for.
    pub chain_id: u64,
    /// The chain's name, as `contracts/deployments.json` records it. Carried so
    /// a log line can say `base` rather than `8453`.
    pub name: &'static str,
    /// The registry's address, or `None` when nothing is deployed on this chain
    /// - the state every entry is in today.
    pub address: Option<Address>,
}

/// The code registry this binary consults, per chain.
///
/// Mirrors the `code_registry` field of `contracts/deployments.json`, which is
/// the one committed place that answers "which registry is canonical here", and
/// [`tests::registry_table_mirrors_the_deployment_manifest`] fails if the two
/// drift apart. **`None` is the only way to say "not deployed"**, exactly as
/// `null` is in that file: no placeholder, no zero address, and nothing
/// substituted for it downstream. The manifest's own note is explicit that a
/// consumer must stop rather than guess, and stopping here means the purchase
/// gate behaves precisely as it did before this table existed - a pinned-table
/// hit proceeds, a pinned-table miss is refused, and no chain read happens that
/// would not have happened anyway.
///
/// Nothing is deployed to any public network yet, so every entry is `None`. The
/// registry lookup is therefore built, tested, and inert;
/// [`tests::an_unpublished_registry_changes_nothing_about_the_gate`] is what
/// keeps it that way until an address lands here.
///
/// This is a build-time constant like `CONTRACT` and `RPC_URL` in `main.rs`, and
/// it is trusted for the same reason they are: the user chose to run this
/// binary. That is where the recursion stops.
pub static REGISTRIES: &[RegistryDeployment] = &[
    RegistryDeployment {
        chain_id: 8453,
        name: "base",
        address: None,
    },
    RegistryDeployment {
        chain_id: 84532,
        name: "base_sepolia",
        address: None,
    },
];

/// The registry published for `chain_id`, or `None` when this build knows of
/// none - either because none is deployed there, or because the chain is not
/// one this binary carries an entry for at all. The two are the same answer to
/// the only question the gate asks: is there an authority to consult.
fn registry_for(chain_id: u64) -> Option<Address> {
    REGISTRIES
        .iter()
        .find(|deployment| deployment.chain_id == chain_id)
        .and_then(|deployment| deployment.address)
}

// ── The selector pre-filter (a diagnostic, never evidence) ────────────────────

/// Function signatures a rub3 licence contract must not expose.
///
/// This is the same list `contracts/test/Rub3Invariants.t.sol` asserts absent
/// from the runtime bytecode of every audited target, mirrored here so a
/// refusal can name what it saw.
///
/// **It proves nothing, in either direction.** It is a blacklist of *names*: a
/// modified copy can expose the same power under a name nobody guessed -
/// `seizeToken(uint256)`, say - and pass this scan in silence, and the scan
/// gets weaker with every legitimate function the contracts gain. The whole of
/// its job here is to turn `unrecognised code` into `contract exposes
/// seize(uint256)` in a refusal message a human has to act on. The masked-hash
/// comparison is what actually decides anything.
///
/// [`tests::forbidden_signatures_mirror_the_solidity_audit`] fails if this list
/// and the Solidity one drift apart.
pub const FORBIDDEN_SIGNATURES: &[&str] = &[
    // Burn - nothing may destroy an issued token.
    "burn(uint256)",
    "burn(address,uint256)",
    "burnFrom(address,uint256)",
    // Admin transfer / seizure - nothing may move a token its holder did not
    // consent to move.
    "adminTransfer(address,address,uint256)",
    "forceTransfer(address,address,uint256)",
    "seize(uint256)",
    "clawback(uint256)",
    // Pause - validation reads must never be switchable off.
    "pause()",
    "unpause()",
    "paused()",
    "setPaused(bool)",
    // Direct invalidation of a token or its terms.
    "revoke(uint256)",
    "revokeToken(uint256)",
    "invalidate(uint256)",
    "setExpiresAt(uint256,uint256)",
    "setRenewPrice(uint256,uint256)",
    "setRenewPriceToken(uint256,address)",
    "setRenewPriceAmount(uint256,uint256)",
    "setPeriod(uint256)",
    // Proxies / upgrade hooks - code is frozen at deploy.
    "upgradeTo(address)",
    "upgradeToAndCall(address,bytes)",
    "initialize()",
    // The rotatable hash slot and any way to rewrite the set.
    "setWrapperHash(bytes32)",
    "removeWrapperHash(bytes32)",
    "unrevokeWrapperHash(bytes32)",
    // Forced migration, and repointing an immutable predecessor.
    "forceMigrate(uint256,address)",
    "setPredecessor(address)",
    "setPreviousFactory(address)",
    // The protocol fee, on both sides of it.
    "setFeeBps(uint16)",
    "setTreasury(address)",
];

/// The 4-byte selector of an ABI function signature.
fn selector(signature: &str) -> [u8; 4] {
    use sha3::{Digest, Keccak256};
    let hash = Keccak256::digest(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Which forbidden signatures have a selector sitting in `code`.
///
/// Searched over the raw bytes rather than over the hex text: a selector that
/// really is dispatched appears as a byte-aligned `PUSH4` immediate, while a
/// text search also matches at odd nibble offsets and invents findings.
pub fn exposed_signatures(code: &[u8]) -> Vec<&'static str> {
    FORBIDDEN_SIGNATURES
        .iter()
        .filter(|signature| {
            let needle = selector(signature);
            code.windows(4).any(|window| window == needle)
        })
        .copied()
        .collect()
}

// ── Masking and hashing ───────────────────────────────────────────────────────

/// `code` with `ranges` overwritten by zero bytes.
///
/// `None` when a range falls outside `code`, which means the fetched code
/// cannot be the contract that published those ranges - a shorter deploy, or a
/// different contract entirely.
fn mask(code: &[u8], ranges: &[ImmutableRange]) -> Option<Vec<u8>> {
    let mut masked = code.to_vec();
    for range in ranges {
        let end = range.start.checked_add(range.length)?;
        masked.get_mut(range.start..end)?.fill(0);
    }
    Some(masked)
}

/// The masked code hash: `sha256` of `code` with `ranges` zeroed, lowercase
/// hex. `None` when a range does not fit, exactly as for [`mask`].
///
/// This is the quantity `contracts/canonical-bytecode.json` publishes as
/// `deployed_bytecode_sha256`, computed from the chain side instead of from the
/// compiler's artifact.
pub fn masked_code_hash(code: &[u8], ranges: &[ImmutableRange]) -> Option<String> {
    masked_code_digest(code, ranges).map(hex::encode)
}

/// [`masked_code_hash`] as raw bytes, for the chain reads that take a `bytes32`.
fn masked_code_digest(code: &[u8], ranges: &[ImmutableRange]) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let masked = mask(code, ranges)?;
    Some(Sha256::digest(&masked).into())
}

/// The `PUSH32` opcode. Every byte solc reserves for a Solidity immutable is
/// part of one's immediate operand.
const PUSH32: u8 = 0x7f;

/// Whether `ranges` describe immutable slots of `code`, or why they do not.
///
/// This bounds the blind spot a masked hash accepts, and it matters most for
/// ranges that did **not** come out of this binary. A masked byte is a byte the
/// comparison never looks at, so a range table wide enough or placed freely
/// enough is somewhere an attacker could put a payload - and the offsets come
/// from the registry on the path this guards.
///
/// Four properties, and each rules out a differently shaped lie:
///
/// 1. every range is exactly one 32-byte word, the width of a Solidity
///    immutable;
/// 2. ranges are sorted and disjoint, so nothing is masked twice and the table
///    is the ordered thing the manifest publishes;
/// 3. every range fits inside the fetched code, so a table cannot describe a
///    contract this is not;
/// 4. **every range is preceded by the `PUSH32` opcode**, which in code a
///    compiler emitted means the range is that instruction's immediate operand.
///    EVM jump-destination analysis excludes bytes inside push immediates, so
///    for such code no control flow can reach a masked byte as an instruction,
///    and that is what turns "we did not look at these bytes" into "these bytes
///    cannot execute". It holds by construction on compiler output: solc
///    compiles every read of an immutable to a `PUSH32` whose operand the
///    deployer fills in. Measured on all three of this repository's contracts
///    that have immutables - `Rub3Access`, `Rub3Subscription` and `Rub3Factory`;
///    the two deployer helpers and the registry have none - not assumed. Should
///    a future compiler emit them another way, this rejects the registry's table
///    and the gate falls back to refusing - fail-closed, on the path that spends
///    money, which is the right direction to be wrong in.
///
/// **What the one-byte lookback does not establish**, said plainly because the
/// claim above is the one this module publishes: it does not prove the `PUSH32`
/// byte is itself an instruction. In code shaped freely it could be a byte
/// inside an earlier push's immediate, in which case the bytes after it are
/// reachable as instructions and masking them hides executable code. Settling
/// that needs a linear disassembly from offset 0, which is not built here:
/// producing such a table requires the registry's owner key, and that key has
/// the shorter route of publishing an empty table so the hostile code is hashed
/// whole. So this check bounds a *careless or drifted* table rather than a
/// hostile authority; the authority is bounded by append-only publication, by
/// every addition being a permanent public event, and by the registry's own
/// code having to hash to the row pinned in this binary.
///
/// The pinned table is not put through this. Its ranges arrive with the binary
/// from `contracts/canonical-bytecode.json` and are already covered by the drift
/// tests; running a bytecode-shape check against them would make a build refuse
/// the very contracts it was packed to buy from, on a chain read, for a
/// property the manifest already fixed.
fn ranges_are_immutable_slots(code: &[u8], ranges: &[ImmutableRange]) -> Result<(), &'static str> {
    let mut previous_end = 0usize;
    for range in ranges {
        if range.length != 32 {
            return Err("a range is not one 32-byte word");
        }
        if range.start < previous_end {
            return Err("ranges are not sorted and disjoint");
        }
        let end = range
            .start
            .checked_add(range.length)
            .ok_or("a range overflows")?;
        if end > code.len() {
            return Err("a range falls outside the fetched code");
        }
        if range.start == 0 || code[range.start - 1] != PUSH32 {
            return Err("a range is not the immediate operand of a PUSH32");
        }
        previous_end = end;
    }
    Ok(())
}
// ── The verdict ───────────────────────────────────────────────────────────────

/// What the version authority said, when there was one to ask.
///
/// A refusal that cannot distinguish "the registry has never heard of this code"
/// from "there is no registry to ask" is telling an operator two very different
/// things in the same words, so the refusal carries this and says which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryOutcome {
    /// Nothing was asked. Either the pinned table already answered, or this
    /// build knows of no registry on this chain - which is every chain today.
    NotConsulted,
    /// Asked, and it has no record of this code.
    Unknown,
    /// Asked, and the answer could not be used: unreachable, or answering from
    /// an address whose own code is not the registry this build pins, or
    /// carrying something this build cannot act on. The refusal stands either
    /// way - the gate fails closed - but it is a weaker one, and it says so.
    Unavailable(String),
}

impl std::fmt::Display for RegistryOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryOutcome::NotConsulted => Ok(()),
            RegistryOutcome::Unknown => {
                write!(f, "; the on-chain code registry has no record of it either")
            }
            RegistryOutcome::Unavailable(why) => {
                write!(
                    f,
                    "; the on-chain code registry could not be consulted ({why})"
                )
            }
        }
    }
}

impl RegistryOutcome {
    /// A one-word form for a machine-readable detail line.
    pub fn as_label(&self) -> &'static str {
        match self {
            RegistryOutcome::NotConsulted => "not_consulted",
            RegistryOutcome::Unknown => "unknown",
            RegistryOutcome::Unavailable(_) => "unavailable",
        }
    }
}

/// What the fetched code was, when nothing vouched for it.
///
/// Carries only what a human or an orchestrator needs to act: how much code was
/// there, whether any known-forbidden name showed up in it, and what the version
/// authority said if one was reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrecognised {
    /// Length of the fetched runtime code. Zero means the address holds no
    /// contract at all.
    pub code_len: usize,
    /// Forbidden signatures found by the pre-filter. A diagnostic: an empty
    /// list is not a clean bill of health, it just means the refusal has
    /// nothing specific to name.
    pub exposed: Vec<&'static str>,
    /// What the code registry said. [`RegistryOutcome::NotConsulted`] on a
    /// refusal means this build knows of no registry on this chain, which is
    /// the state every chain is in today.
    pub registry: RegistryOutcome,
}

impl Unrecognised {
    /// What was found in the code itself, without a word about the registry.
    fn write_finding<W: std::fmt::Write>(&self, f: &mut W) -> std::fmt::Result {
        if self.code_len == 0 {
            return write!(f, "the address holds no contract code");
        }
        write!(
            f,
            "{} bytes of code matching no canonical rub3 contract this build knows",
            self.code_len
        )?;
        if !self.exposed.is_empty() {
            write!(f, "; it exposes {}", self.exposed.join(", "))?;
        }
        Ok(())
    }

    /// The finding, with a failed registry read's *reason* left out.
    ///
    /// For a surface that hands its text to a stranger. The reason a registry
    /// read failed is a network error, and although `rpc` has already reduced
    /// every URL in it to `scheme://host[:port]`, that host is still the
    /// endpoint this build was packed with and it is not the reader's to
    /// publish. The rule is §2.8's, applied to the second chain read: a refusal
    /// shows its finding verbatim, a failed read shows only the kind of
    /// failure. A registry that *answered* is a verdict rather than a failure,
    /// so [`RegistryOutcome::Unknown`] is kept - it names no endpoint and it is
    /// half of what the refusal means.
    ///
    /// [`std::fmt::Display`] is the unabridged form, and it is what the agent
    /// door prints: an operator who ran the wrapper already knows the endpoint
    /// and needs to know which read failed.
    pub fn shareable_detail(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = self.write_finding(&mut out);
        if matches!(self.registry, RegistryOutcome::Unknown) {
            let _ = write!(out, "{}", self.registry);
        }
        out
    }
}

impl std::fmt::Display for Unrecognised {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write_finding(f)?;
        write!(f, "{}", self.registry)
    }
}

/// The result of comparing fetched runtime code against the pinned table.
///
/// The table's answer alone, before any chain is asked: `Unrecognised` here
/// means "not in this binary", which is what a release newer than the binary
/// looks like as well as what a modified copy looks like. Telling those apart is
/// [`consult_registry`]'s job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The masked code hash matched a pinned entry. The strongest available
    /// statement about the contract's executable code; see the module docs for
    /// what it deliberately does not cover.
    Canonical(&'static CanonicalContract),
    /// No pinned entry matched. The [`RegistryOutcome`] is
    /// [`RegistryOutcome::NotConsulted`], because this comparison asks nobody.
    Unrecognised(Unrecognised),
}

/// Compares fetched runtime code against the binary's pinned table.
///
/// Pure: every byte it needs is already in hand, so this costs no RPC and works
/// on a degraded network.
pub fn classify(code: &[u8]) -> Verdict {
    classify_against(code, CANONICAL)
}

/// [`classify`] against an explicit table, so the comparison itself can be
/// exercised without depending on which contracts happen to be pinned today.
fn classify_against(code: &[u8], table: &'static [CanonicalContract]) -> Verdict {
    for entry in table {
        // One masking pass per distinct range set. Entries sharing a range set
        // are rare enough that deduplicating them would cost more clarity than
        // it saves work: the table holds a handful of releases, not thousands.
        if let Some(hash) = masked_code_hash(code, entry.immutable_ranges) {
            if hash == entry.masked_sha256 {
                return Verdict::Canonical(entry);
            }
        }
    }

    Verdict::Unrecognised(Unrecognised {
        code_len: code.len(),
        exposed: exposed_signatures(code),
        registry: RegistryOutcome::NotConsulted,
    })
}

// ── The registry: what a release newer than this binary looks like ────────────

/// Whether a release is recommended for new purchases.
///
/// **Neither value can invalidate anything.** `Deprecated` means "prefer a newer
/// release", never "stop honouring": a purchase against deprecated code proceeds
/// with a warning, a held token is untouched, and nothing on the launch path
/// reads any of this. A status that could strand a paid licence would be a
/// revocation surface by the back door, which this project rules out, and the
/// registry contract has no such status to publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStatus {
    /// Current. Buy from it without comment.
    Active,
    /// Superseded. Still genuine rub3 code, still honoured, no longer
    /// recommended for a new purchase.
    Deprecated,
}

/// One release, as the code registry published it, reduced to what this module
/// acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRecord {
    /// Solidity contract name.
    pub contract: String,
    /// The release label. Explanation only: nothing here branches on it.
    pub version: String,
    /// Whether it is recommended for new purchases.
    pub status: RecordStatus,
    /// What a contract carrying this code is for, or `None` for a role this
    /// build has no name for - which a registry newer than the binary can
    /// legitimately publish, and which is refused rather than guessed at.
    pub role: Option<Role>,
    /// The block the record was published in.
    pub registered_at_block: u64,
    /// The immutable ranges the release declares. Cross-checked against the
    /// table the lookup masked under, so a registry cannot answer about one
    /// layout while describing another.
    pub offsets: Vec<ImmutableRange>,
}

/// The chain reads attestation makes.
///
/// A trait rather than three direct calls so the decision logic can be driven
/// over a scripted chain in tests: the interesting cases here are a registry
/// that is unreachable, one answering from code that is not the registry, and
/// one publishing a table that does not describe the code - none of which a
/// live endpoint can be asked to produce on demand. [`RpcChain`] is the real
/// implementation and the only one a shipped binary builds.
pub trait ChainReader {
    /// The runtime code deployed at `address`, empty when there is none.
    fn code(&self, address: Address) -> Result<Vec<u8>, RpcError>;
    /// At most `limit` of the distinct immutable-offset tables the registry
    /// publishes, **newest first**.
    ///
    /// The limit is part of the request rather than something the caller trims
    /// off the answer: a purchase only ever tries a fixed number of candidates,
    /// so it asks for that many and never pays to transfer or decode a set the
    /// registry's owner key made arbitrarily long. An implementation may return
    /// fewer, and a caller must still hold its own bound: nothing here can make
    /// a remote node honest about a limit or about an order.
    ///
    /// The end matters as much as the size. A registry is consulted only when
    /// the pinned table missed, which is by definition a question about code
    /// newer than this binary, so the budget is spent on the newest layouts.
    fn offset_tables(
        &self,
        registry: Address,
        limit: usize,
    ) -> Result<Vec<Vec<ImmutableRange>>, RpcError>;
    /// The release published for a masked code hash, or `None` for no record.
    fn record(
        &self,
        registry: Address,
        masked_code_hash: [u8; 32],
    ) -> Result<Option<RegistryRecord>, RpcError>;
}

/// [`ChainReader`] over the JSON-RPC endpoint this binary was packed with.
pub struct RpcChain<'a> {
    /// The endpoint. Every error built from it is already redacted by `rpc`, so
    /// nothing here has to remember to strip a key out of a message.
    pub rpc_url: &'a str,
}

impl ChainReader for RpcChain<'_> {
    fn code(&self, address: Address) -> Result<Vec<u8>, RpcError> {
        rpc::get_code(self.rpc_url, address)
    }

    fn offset_tables(
        &self,
        registry: Address,
        limit: usize,
    ) -> Result<Vec<Vec<ImmutableRange>>, RpcError> {
        Ok(
            rpc::code_registry_offset_tables(self.rpc_url, registry, limit)?
                .into_iter()
                .map(|table| table.into_iter().map(range_from_chain).collect())
                .collect(),
        )
    }

    fn record(
        &self,
        registry: Address,
        masked_code_hash: [u8; 32],
    ) -> Result<Option<RegistryRecord>, RpcError> {
        let Some(record) = rpc::code_registry_record(self.rpc_url, registry, masked_code_hash)?
        else {
            return Ok(None);
        };
        // The status and the role arrive as the raw `uint8`s the registry's
        // enums encode as, and a registry newer than this binary can publish a
        // value neither enum here has a name for. An unknown status is not a
        // record this build can act on and is dropped as no record at all; an
        // unknown role is carried through as `None` so the refusal can say the
        // code is genuine and the address is still not a licence.
        let status = match record.status {
            1 => RecordStatus::Active,
            2 => RecordStatus::Deprecated,
            _ => return Ok(None),
        };
        Ok(Some(RegistryRecord {
            contract: record.contract_name,
            version: record.version,
            status,
            role: Role::from_u8(record.role),
            registered_at_block: record.registered_at_block,
            offsets: record.offsets.into_iter().map(range_from_chain).collect(),
        }))
    }
}

/// One published range, as this module represents it.
fn range_from_chain(range: rpc::CodeRange) -> ImmutableRange {
    ImmutableRange {
        start: range.start as usize,
        length: range.length as usize,
    }
}

/// The most registry-supplied text this module will ever repeat, in characters.
///
/// `Rub3CodeRegistry.publish` requires a record's `contractName` and `version`
/// to be non-empty and requires nothing else of them, so their length is the
/// publisher's choice and every surface that quotes them - a buyer's screen, a
/// log line, a machine-readable detail line - is one this module hands to
/// somebody else.
const MAX_REGISTRY_TEXT: usize = 96;

/// Registry-supplied text, reduced to something safe to repeat verbatim.
///
/// Printable ASCII survives; everything else becomes `?`, and the result is
/// bounded by [`MAX_REGISTRY_TEXT`]. The characters this drops are the ones
/// that let published text act rather than read: a newline in a `version` label
/// would let a record forge a second `rub3: ...` line in the wrapper's own
/// stderr, which is the surface an operator reads the attestation off.
fn safe_text(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(MAX_REGISTRY_TEXT)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect();
    if text.chars().count() > MAX_REGISTRY_TEXT {
        out.push_str("...");
    }
    if out.trim().is_empty() {
        return "(unnamed)".to_string();
    }
    out
}

/// Registry-supplied text reduced further, to one token safe in a `key=value`
/// line.
///
/// A contract name reaches the agent door's machine-readable detail line as
/// `canonical=<name>`, where a space would start a field an orchestrator then
/// mis-parses and an `=` would invent one. A real Solidity contract name passes
/// through unchanged; anything outside `[A-Za-z0-9._-]` becomes `_`, which
/// keeps the value a single readable token whatever was published.
fn safe_token(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(MAX_REGISTRY_TEXT)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if text.chars().count() > MAX_REGISTRY_TEXT {
        out.push_str("...");
    }
    if out.is_empty() {
        return "unnamed".to_string();
    }
    out
}

/// A fetched record with its two text fields reduced to what this module is
/// willing to repeat.
///
/// One place, at the point the registry's answer enters the decision, rather
/// than at each of the surfaces that quote it: the record is the only untrusted
/// text in this module and every one of those surfaces would otherwise have to
/// remember the same rule.
fn sanitised(record: RegistryRecord) -> RegistryRecord {
    RegistryRecord {
        contract: safe_token(&record.contract),
        version: safe_text(&record.version),
        ..record
    }
}

/// How many candidate offset tables one consultation will read and try.
///
/// Every candidate that survives the shape check costs its own sequential
/// `record` call, so an unbounded list lets whoever holds the registry's owner
/// key turn each purchase into arbitrarily many round trips. The bound this
/// project publishes on that key is that the damage is limited to additions,
/// each one a permanent public event a watcher can alarm on; a purchase that
/// takes minutes is not an addition anyone would alarm on, so the published
/// bound did not cover it. This closes that gap. It bounds latency only: an
/// uncapped loop was never able to produce a wrong verdict, only a slow one.
///
/// It bounds the read as well as the loop, which is one number doing two jobs
/// because both costs come from the same published set. The registry is asked
/// for this many tables through `latestOffsetTables`, so a set the owner key
/// made arbitrarily long is never transferred, decoded or shape-checked in full;
/// the same number then bounds the lookups, so a node that answers with more
/// entries than it was asked for still cannot buy extra round trips.
///
/// **Which end the budget is spent on is not a detail.** The registry is asked
/// for the *newest* tables, because it is consulted only when the pinned table
/// missed and a pinned-table miss is by definition about code newer than this
/// build. Spending the budget oldest-first would mean that once a seventeenth
/// layout was interned, every release published under it became unreadable to
/// every fielded binary carrying this cap while the first releases stayed
/// readable forever - blinding fielded binaries to exactly the releases this
/// whole path exists to recognise. Nothing here can be wrong, in either
/// direction: a table never read is a release refused as unknown, which is what
/// a build with no registry at all does.
///
/// Sixteen is deliberately far above anything legitimate rather than tight. A
/// table is per *code layout*, not per contract and not per release: today
/// exactly one exists, and `Rub3Access` and `Rub3Subscription` share it. A
/// second appears only when a future contract's immutables land at different
/// offsets, so sixteen leaves room for many such releases live at once. Raise
/// it when a real deployment needs more tables than this, never to accommodate
/// a registry publishing tables no contract was deployed under.
const MAX_CANDIDATE_OFFSET_TABLES: usize = 16;

/// What the registry said about code the pinned table did not recognise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryVerdict {
    /// A published release. Canonical, from an authority newer than this build.
    Known(RegistryRecord),
    /// No record. The strongest thing anyone can say about this code is that
    /// neither this binary nor the registry has ever seen it.
    Unknown,
    /// The question could not be put. Carries the sentence a refusal quotes.
    Unavailable(String),
}

/// Asks the code registry about `code`, having first checked that the registry
/// is what it claims to be.
///
/// The order is the whole design and it does not have a cheaper arrangement:
///
/// 1. **Fetch the registry's own runtime code and hash it.** It has to match the
///    [`Role::CodeRegistry`] entry pinned in this binary. Believing an answer
///    before this would move the trust from "one hash the user chose to run"
///    onto "whoever put an address at that slot", which is the recursion this
///    whole module exists to terminate.
/// 2. **Fetch the distinct offset tables**, the newest
///    [`MAX_CANDIDATE_OFFSET_TABLES`] of them. A masked hash cannot be computed
///    without a table, and the table cannot be looked up without the hash, so
///    the candidates come first in one call - a bounded one, because the number
///    of tables published is the owner key's to choose and this call is on the
///    path that spends money, and the newest ones, because this runs only after
///    the pinned table missed and a miss is a question about newer code. Each is
///    checked against the fetched
///    code by [`ranges_are_immutable_slots`] before anything is masked with it;
///    a table this build cannot confirm describes `PUSH32` immediates of *this*
///    code is dropped rather than trusted, because a masked byte is a byte the
///    comparison never looks at. Dropping every candidate is
///    [`RegistryVerdict::Unknown`], not a failure: it says no published release
///    is shaped like this code, which is what a lookup that missed says too.
/// 3. **Look up the hash under each surviving candidate, newest first**, holding
///    the same cap locally so the bound does not rest on the node having
///    honoured it. Today there is one table, so this is one `eth_call`; the loop
///    exists because a future contract that adds an immutable adds a second
///    table. Candidates past the cap are never read or never tried and, like a
///    dropped one, contribute nothing - so a registry that publishes more tables
///    than any deployment uses costs a bounded number of round trips and yields
///    [`RegistryVerdict::Unknown`], never a new outcome.
/// 4. **Cross-check the record against the table that found it.** A record whose
///    own declared offsets differ from the table its hash was computed under is
///    a registry describing one layout while answering about another, which is
///    not an answer to act on.
///
/// Fails closed on every step: this runs on the path that spends money, and
/// every failure here leaves the caller refusing exactly as it would have
/// refused without a registry at all.
pub fn consult_registry(
    chain: &dyn ChainReader,
    registry: Address,
    code: &[u8],
) -> RegistryVerdict {
    consult_registry_against(chain, registry, code, CANONICAL)
}

/// [`consult_registry`] against an explicit pinned table, so the consultation
/// can be exercised without a real `Rub3CodeRegistry` deployment to hash.
fn consult_registry_against(
    chain: &dyn ChainReader,
    registry: Address,
    code: &[u8],
    table: &'static [CanonicalContract],
) -> RegistryVerdict {
    let registry_code = match chain.code(registry) {
        Ok(code) => code,
        Err(e) => return RegistryVerdict::Unavailable(format!("its code could not be read: {e}")),
    };
    match classify_against(&registry_code, table) {
        Verdict::Canonical(entry) if entry.role == Role::CodeRegistry => {}
        Verdict::Canonical(entry) => {
            return RegistryVerdict::Unavailable(format!(
                "the code at {registry} is canonical {}, which is not a code registry",
                entry.contract
            ))
        }
        Verdict::Unrecognised(_) => {
            return RegistryVerdict::Unavailable(format!(
                "the code at {registry} is not the code registry this build pins"
            ))
        }
    }

    let tables = match chain.offset_tables(registry, MAX_CANDIDATE_OFFSET_TABLES) {
        Ok(tables) => tables,
        Err(e) => {
            return RegistryVerdict::Unavailable(format!(
                "its offset tables could not be read: {e}"
            ))
        }
    };
    // A table that does not describe this code's immutables is dropped rather
    // than masked with, and a dropped table is not an error: no published
    // release has that shape, which is a statement about the code and not about
    // the registry. If every candidate goes, the answer is that the registry
    // knows of no release shaped like this - the same `Unknown` a lookup that
    // simply missed would give, because it means the same thing.
    //
    // The cap was already sent to the registry, which is what keeps an
    // arbitrarily long published set off the wire and is what picks the newest
    // layouts rather than the first ones ever interned. It is applied again here
    // because a node is not obliged to honour it and a lookup per extra entry is
    // exactly the cost being bounded; the ones past it are untried rather than
    // dropped, and both mean the same thing here - no answer came from them.
    let usable = tables
        .iter()
        .filter(|table| ranges_are_immutable_slots(code, table).is_ok())
        .take(MAX_CANDIDATE_OFFSET_TABLES);

    for table in usable {
        let Some(digest) = masked_code_digest(code, table) else {
            continue;
        };
        match chain.record(registry, digest) {
            Ok(Some(record)) => {
                let record = sanitised(record);
                if record.offsets != *table {
                    return RegistryVerdict::Unavailable(format!(
                        "its record for {} declares immutable ranges other than the table that \
                         found it",
                        record.contract
                    ));
                }
                return RegistryVerdict::Known(record);
            }
            Ok(None) => continue,
            Err(e) => {
                return RegistryVerdict::Unavailable(format!("its record could not be read: {e}"))
            }
        }
    }

    RegistryVerdict::Unknown
}

// ── The purchase gate ─────────────────────────────────────────────────────────

/// Which authority vouched for a contract's code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// The table compiled into this binary. The common case, and the one that
    /// costs no chain read beyond the code itself.
    Pinned,
    /// The on-chain code registry, whose own code this build verified first. A
    /// release newer than this binary's table.
    Registry {
        /// Whether it is still recommended for new purchases.
        status: RecordStatus,
        /// The block the record was published in, so an audit log can point at
        /// the transaction that made this answer possible.
        registered_at_block: u64,
    },
}

/// A contract's code, vouched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attestation {
    /// Solidity contract name.
    pub contract: String,
    /// The release label of whatever vouched for it.
    pub release: String,
    /// Who vouched.
    pub authority: Authority,
}

impl Attestation {
    /// The warning a caller must print beside a purchase, or `None`.
    ///
    /// Deprecation is advice and never a refusal: an agent that meets it says
    /// so and buys. Returning it rather than printing it keeps the wording of
    /// the two front doors theirs to choose.
    pub fn advisory(&self) -> Option<String> {
        match self.authority {
            Authority::Registry {
                status: RecordStatus::Deprecated,
                ..
            } => Some(format!(
                "the code registry marks {} ({}) as superseded and no longer recommended for new \
                 purchases; it is still genuine rub3 code and licences bought from it are \
                 unaffected",
                self.contract, self.release
            )),
            _ => None,
        }
    }
}

impl std::fmt::Display for Attestation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.contract, self.release)?;
        match self.authority {
            Authority::Pinned => Ok(()),
            Authority::Registry {
                registered_at_block,
                ..
            } => write!(
                f,
                ", published by the on-chain code registry at block {registered_at_block}"
            ),
        }
    }
}

/// Why a candidate contract may not be bought from.
///
/// Both variants are refusals of the address, not of the network: nothing about
/// them is retryable, and neither leaves a transaction behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The code at the address was vouched for by nothing - not the pinned
    /// table, and not the registry if there was one to ask.
    Unrecognised(Unrecognised),
    /// Canonical rub3 code, but not a licence contract - the factory, one of its
    /// deployer helpers, or the code registry itself. It sells nothing, so
    /// buying from it is a mistake about the address rather than about the code.
    NotALicence {
        contract: String,
        /// `None` for a role a registry published that this build has no name
        /// for. Still a refusal: a purchase may only target a role this binary
        /// recognises as a licence.
        role: Option<Role>,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unrecognised(u) => write!(f, "{u}"),
            Refusal::NotALicence { contract, role } => write!(
                f,
                "the code is canonical {contract}, which is a {} and sells no licences",
                match role {
                    Some(role) => role.describe(),
                    None => "kind of rub3 contract this build has no name for",
                }
            ),
        }
    }
}

/// Everything that can stop the purchase gate.
#[derive(Debug)]
pub enum GateError {
    /// The runtime code could not be read. The gate fails closed on this: a
    /// chain read that did not complete is not permission to spend.
    Fetch(RpcError),
    /// The code was read and refused.
    Refused(Refusal),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::Fetch(e) => write!(f, "could not read the contract's code: {e}"),
            GateError::Refused(r) => write!(f, "{r}"),
        }
    }
}

impl std::error::Error for GateError {}

/// The pre-purchase gate: one `eth_getCode`, then a decision, before anything
/// is signed.
///
/// A pinned-table hit is the common case and ends here, having asked the chain
/// exactly once. Only a miss consults the registry, and only when this build
/// carries one for `chain_id` - which is no chain today, so a miss costs no
/// extra read either.
///
/// Returns what vouched for the contract. Every other outcome is an error,
/// including a chain read that failed - see the module docs on failure posture,
/// and do not add a caller on the launch path.
pub fn verify_before_purchase(
    rpc_url: &str,
    chain_id: u64,
    contract: Address,
) -> Result<Attestation, GateError> {
    let chain = RpcChain { rpc_url };
    let code = chain.code(contract).map_err(GateError::Fetch)?;
    decide(&chain, chain_id, &code).map_err(GateError::Refused)
}

/// The decision the gate makes once the bytes are in hand, separated from the
/// contract's own chain read so it can be exercised over a scripted chain.
fn decide(chain: &dyn ChainReader, chain_id: u64, code: &[u8]) -> Result<Attestation, Refusal> {
    decide_verdict(
        classify(code),
        chain,
        registry_for(chain_id),
        code,
        CANONICAL,
    )
}

/// [`decide`] over an already-formed verdict, an explicitly resolved registry
/// and an explicit pinned table.
///
/// The three seams the tests need, and each is a real parameter of the decision
/// rather than an ambient fact: which contracts this build pins, whether there
/// is an authority to ask, and what it says. `registry` is `None` for every
/// chain today, and that is what keeps the miss path a refusal.
fn decide_verdict(
    verdict: Verdict,
    chain: &dyn ChainReader,
    registry: Option<Address>,
    code: &[u8],
    table: &'static [CanonicalContract],
) -> Result<Attestation, Refusal> {
    match verdict {
        Verdict::Canonical(entry) => accept(
            entry.contract.to_string(),
            entry.release.to_string(),
            Some(entry.role),
            Authority::Pinned,
        ),

        // The miss path. A contract from a template release newer than this
        // binary was packed with is indistinguishable from a modified copy at
        // this point - both are a table miss - so the registry is asked which
        // one it is. Until an address is published for this chain there is
        // nobody to ask, and a miss stays a refusal.
        Verdict::Unrecognised(finding) => {
            // An address holding no code is not a version question. There is no
            // release for an authority to recognise, the answer could only ever
            // be `Unknown`, and appending "the on-chain code registry has no
            // record of it either" to "the address holds no contract code"
            // would make the one refusal a person can usually fix themselves
            // read as a verdict on something that is not there.
            if code.is_empty() {
                return Err(Refusal::Unrecognised(finding));
            }
            let Some(registry) = registry else {
                return Err(Refusal::Unrecognised(finding));
            };
            match consult_registry_against(chain, registry, code, table) {
                RegistryVerdict::Known(record) => accept(
                    record.contract,
                    record.version,
                    record.role,
                    Authority::Registry {
                        status: record.status,
                        registered_at_block: record.registered_at_block,
                    },
                ),
                RegistryVerdict::Unknown => Err(Refusal::Unrecognised(Unrecognised {
                    registry: RegistryOutcome::Unknown,
                    ..finding
                })),
                RegistryVerdict::Unavailable(why) => Err(Refusal::Unrecognised(Unrecognised {
                    registry: RegistryOutcome::Unavailable(why),
                    ..finding
                })),
            }
        }
    }
}

/// Canonical code, checked for being a licence before it becomes an acceptance.
///
/// One place rather than two, because the role check is the same question
/// whichever authority vouched: canonical rub3 code that sells nothing is the
/// wrong address, and a registry answering about the factory has to be refused
/// exactly as the pinned table's factory row is.
fn accept(
    contract: String,
    release: String,
    role: Option<Role>,
    authority: Authority,
) -> Result<Attestation, Refusal> {
    if role != Some(Role::Licence) {
        return Err(Refusal::NotALicence { contract, role });
    }
    Ok(Attestation {
        contract,
        release,
        authority,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The published record this table mirrors. Read at compile time, so the
    /// drift check below cannot be skipped by a missing file or a stale copy.
    const MANIFEST: &str = include_str!("../../../contracts/canonical-bytecode.json");

    /// The Solidity audit the selector pre-filter mirrors.
    const INVARIANTS_SOL: &str = include_str!("../../../contracts/test/Rub3Invariants.t.sol");

    // ── Drift protection ─────────────────────────────────────────────────────

    /// Every fingerprint the repository publishes must be pinned here, byte for
    /// byte, with the same immutable ranges.
    ///
    /// This is what stops the published record rotting silently. The contracts
    /// change, `scripts/canonical-bytecode-hashes.sh update` moves the manifest,
    /// and without this test the wrapper would go on asserting a hash nothing
    /// produces any more - and refuse the very contract this project deployed.
    /// The blocking `bytecode-fingerprints` CI job guarantees the manifest
    /// matches the contracts; this test extends that guarantee to the table.
    #[test]
    fn pinned_table_mirrors_the_canonical_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(MANIFEST).expect("contracts/canonical-bytecode.json is not JSON");
        let published = manifest["contracts"]
            .as_object()
            .expect("canonical-bytecode.json has no `contracts` object");

        for (name, record) in published {
            let hash = record["deployed_bytecode_sha256"]
                .as_str()
                .unwrap_or_else(|| panic!("{name} publishes no deployed_bytecode_sha256"));
            let ranges: Vec<ImmutableRange> = record["immutable_ranges"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} publishes no immutable_ranges"))
                .iter()
                .map(|r| ImmutableRange {
                    start: r["start"].as_u64().expect("range start") as usize,
                    length: r["length"].as_u64().expect("range length") as usize,
                })
                .collect();

            // A contract the table already knows has exactly one role, and a
            // new fingerprint for it does not change what the contract is for.
            // Suggesting that role is what keeps a pasted row from silently
            // demoting the factory or a deployer helper to a purchase target; a
            // contract name the table has never seen gets a placeholder that
            // says out loud that it is one.
            let role_line = match CANONICAL.iter().find(|entry| entry.contract == name) {
                Some(known) => format!("role: Role::{:?},", known.role),
                None => {
                    "role: Role::Licence,   // NEW CONTRACT - choose this deliberately".to_string()
                }
            };

            let pinned = CANONICAL
                .iter()
                .find(|entry| entry.contract == name && entry.masked_sha256 == hash)
                .unwrap_or_else(|| {
                    panic!(
                        "contracts/canonical-bytecode.json publishes {name} with masked hash \
                         {hash}, and no entry in attest::CANONICAL matches it.\n\n\
                         The contracts changed, so the table has to gain a row. Add - never \
                         overwrite - an entry, because a contract already deployed at an older \
                         fingerprint stays canonical forever:\n\n\
                         \x20   CanonicalContract {{\n\
                         \x20       contract: {name:?},\n\
                         \x20       source: \"contracts/{}\",\n\
                         \x20       {role_line}\n\
                         \x20       release: RELEASE,      // check this\n\
                         \x20       masked_sha256: {hash:?},\n\
                         \x20       immutable_ranges: &{ranges:?},\n\
                         \x20   }},\n",
                        record["source"].as_str().unwrap_or("src/?.sol"),
                    )
                });

            assert_eq!(
                pinned.immutable_ranges, ranges,
                "{name} is pinned at the published masked hash but with different immutable \
                 ranges. A comparator zeroing these ranges would hash something the manifest \
                 never described; regenerate the row from the manifest."
            );
        }

        // The manifest carries no role, so the row above inherits its
        // contract's role from the table rather than from the published record.
        // This is what makes that inheritance binding: two rows for one
        // contract disagreeing about what it is for would mean one release of
        // `Rub3Factory` or of a deployer helper had quietly become a valid
        // purchase target, with every other test in the crate still green.
        for entry in CANONICAL {
            let first = CANONICAL
                .iter()
                .find(|other| other.contract == entry.contract)
                .expect("an entry finds at least itself");
            assert_eq!(
                entry.role, first.role,
                "{} is pinned as both {:?} and {:?}. What a contract is *for* does not change \
                 between releases, and a row that downgrades a factory or a deployer helper to \
                 Role::Licence removes the NotALicence guard from the purchase path.",
                entry.contract, first.role, entry.role
            );
        }
    }

    /// The masked hashes are what they claim to be: lowercase hex of 32 bytes.
    ///
    /// A stray uppercase digit or `0x` prefix would compare unequal against
    /// every real contract while looking perfectly correct in review, so the
    /// whole table would silently refuse everything.
    #[test]
    fn pinned_hashes_are_lowercase_hex_of_32_bytes() {
        for entry in CANONICAL {
            let hash = entry.masked_sha256;
            assert_eq!(
                hash.len(),
                64,
                "{}: masked_sha256 is {} chars, expected 64",
                entry.contract,
                hash.len()
            );
            assert!(
                hash.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}: masked_sha256 must be lowercase hex with no 0x prefix",
                entry.contract
            );
        }
    }

    /// Immutable ranges arrive sorted and never overlap.
    ///
    /// Overlapping ranges would still mask correctly, but they would mean the
    /// row was assembled by hand rather than copied from the manifest, which is
    /// the mistake this table cannot afford.
    #[test]
    fn pinned_ranges_are_sorted_and_disjoint() {
        for entry in CANONICAL {
            let mut previous_end = 0usize;
            for range in entry.immutable_ranges {
                assert!(
                    range.start >= previous_end,
                    "{}: immutable ranges must be sorted and disjoint, {range:?} overlaps or \
                     precedes the range ending at {previous_end}",
                    entry.contract
                );
                assert_eq!(
                    range.length, 32,
                    "{}: a Solidity immutable occupies one 32-byte word, {range:?} does not",
                    entry.contract
                );
                previous_end = range.start + range.length;
            }
        }
    }

    /// The pre-filter list and the Solidity audit are the same list.
    ///
    /// `CLAUDE.md` records that this list has churned repeatedly and that a
    /// stale copy is the usual casualty. This is a fifth copy of it, so it is
    /// checked against the one that runs against real bytecode rather than
    /// trusted to stay in step.
    ///
    /// The parse leans on a textual contract in `Rub3Invariants.t.sol`, and
    /// asserts each part of it rather than assuming it: `string[` appears
    /// exactly once in the file, its `= [ ... ];` literal holds one quoted
    /// signature per line and nothing else quoted, and the count it parses
    /// matches the declared `N`. Comments come out first, because the prose
    /// around the array quotes text and a naive quoted-string scan would take a
    /// sentence fragment for a signature and misalign every entry after it.
    #[test]
    fn forbidden_signatures_mirror_the_solidity_audit() {
        assert_eq!(
            INVARIANTS_SOL.matches("string[").count(),
            1,
            "Rub3Invariants.t.sol now declares more than one `string[N]` array, so this parse \
             can no longer tell which one is the forbidden-signature list. Point it at that \
             declaration deliberately rather than letting it take whichever comes first."
        );

        let declaration = INVARIANTS_SOL
            .find("string[")
            .expect("Rub3Invariants.t.sol declares no `string[N] memory forbidden` array");
        let tail = &INVARIANTS_SOL[declaration + "string[".len()..];
        let declared_len: usize = tail[..tail.find(']').expect("unterminated string[")]
            .parse()
            .expect("string[N] does not carry a number");

        let body = &tail[tail.find("= [").expect("no array literal") + "= [".len()..];
        let body = &body[..body.find("];").expect("unterminated array literal")];

        // Comments come out first: they are prose, and the prose quotes things
        // ("a developer's economics can never change after deploy"), so a
        // straight scan for quoted strings would pick a sentence fragment out
        // of a comment and misalign every signature after it.
        let code_only: String = body
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");

        // What is left is one quoted signature per line and nothing else
        // quoted.
        let solidity: Vec<String> = code_only
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect();

        assert_eq!(
            solidity.len(),
            declared_len,
            "parsed {} signatures out of a `string[{declared_len}]` declaration; the parse, not \
             the list, is what is wrong",
            solidity.len()
        );
        assert_eq!(
            FORBIDDEN_SIGNATURES,
            &solidity.iter().map(String::as_str).collect::<Vec<_>>()[..],
            "attest::FORBIDDEN_SIGNATURES and the list in contracts/test/Rub3Invariants.t.sol \
             have drifted apart. The Solidity list is the source of truth; mirror it here, and \
             sweep the other copies named in CLAUDE.md while you are at it."
        );
    }

    // ── The comparison itself ────────────────────────────────────────────────

    #[test]
    fn selector_matches_a_published_vector() {
        // ERC-20 `transfer(address,uint256)`, the most widely published 4-byte
        // selector there is: if the hash function or the encoding were wrong,
        // this would not be 0xa9059cbb.
        assert_eq!(
            selector("transfer(address,uint256)"),
            [0xa9, 0x05, 0x9c, 0xbb]
        );
    }

    /// Synthetic runtime code: `len` bytes of something that is not all zeroes,
    /// so a masking bug that zeroed too much would change the hash.
    fn fake_code(len: usize, fill: u8) -> Vec<u8> {
        (0..len).map(|i| fill.wrapping_add(i as u8)).collect()
    }

    /// A table entry describing `code`, built at runtime so the tests exercise
    /// the comparison rather than whichever contracts happen to be pinned.
    fn entry_for(
        code: &[u8],
        ranges: Vec<ImmutableRange>,
        role: Role,
    ) -> &'static [CanonicalContract] {
        let ranges: &'static [ImmutableRange] = Box::leak(ranges.into_boxed_slice());
        let hash: &'static str = Box::leak(
            masked_code_hash(code, ranges)
                .expect("ranges fit the code")
                .into_boxed_str(),
        );
        Box::leak(
            vec![CanonicalContract {
                contract: "FakeLicence",
                source: "contracts/src/FakeLicence.sol",
                role,
                release: "test",
                masked_sha256: hash,
                immutable_ranges: ranges,
            }]
            .into_boxed_slice(),
        )
    }

    #[test]
    fn immutable_values_do_not_move_the_masked_hash() {
        let ranges = vec![
            ImmutableRange {
                start: 64,
                length: 32,
            },
            ImmutableRange {
                start: 160,
                length: 32,
            },
        ];

        // Two deploys of the same code that chose different constructor
        // arguments: identical everywhere except inside the immutable slots.
        let mut deploy_a = fake_code(256, 0x10);
        let mut deploy_b = deploy_a.clone();
        for range in &ranges {
            deploy_a[range.start..range.start + range.length].fill(0xaa);
            deploy_b[range.start..range.start + range.length].fill(0xbb);
        }
        assert_ne!(
            deploy_a, deploy_b,
            "the two deploys must differ on the wire"
        );

        let table = entry_for(&deploy_a, ranges, Role::Licence);
        assert!(matches!(
            classify_against(&deploy_a, table),
            Verdict::Canonical(_)
        ));
        assert!(
            matches!(classify_against(&deploy_b, table), Verdict::Canonical(_)),
            "a legitimate deploy that chose different immutables must still be canonical"
        );
    }

    /// The threat the name scan cannot see, and the hash can.
    ///
    /// An owner-only seizure function under an innocuous name -
    /// `reconcileLedger(uint256,address)` - is exactly the modified copy the
    /// selector blacklist passes in silence. The masked hash rejects it, which
    /// is the entire reason this module exists.
    #[test]
    fn a_renamed_seizure_function_passes_the_name_scan_and_fails_the_hash() {
        let ranges = vec![ImmutableRange {
            start: 64,
            length: 32,
        }];
        let canonical = fake_code(256, 0x10);
        let table = entry_for(&canonical, ranges.clone(), Role::Licence);

        // The attacker's copy: same length, same immutable layout, one
        // dispatcher entry added outside every masked range.
        let mut modified = canonical.clone();
        let hidden = selector("reconcileLedger(uint256,address)");
        modified[200..204].copy_from_slice(&hidden);

        assert!(
            !FORBIDDEN_SIGNATURES.contains(&"reconcileLedger(uint256,address)"),
            "the point of this test is that the blacklist does not know this name"
        );
        assert!(
            exposed_signatures(&modified).is_empty(),
            "the name scan is supposed to miss this - it is a blacklist of names"
        );

        match classify_against(&modified, table) {
            Verdict::Unrecognised(finding) => {
                assert_eq!(finding.code_len, modified.len());
                assert!(finding.exposed.is_empty());
            }
            Verdict::Canonical(entry) => panic!(
                "a modified copy was accepted as canonical {}, which is the failure this \
                 module exists to prevent",
                entry.contract
            ),
        }
    }

    #[test]
    fn a_shorter_deploy_cannot_match_a_longer_entry() {
        let ranges = vec![ImmutableRange {
            start: 200,
            length: 32,
        }];
        let canonical = fake_code(256, 0x10);
        let table = entry_for(&canonical, ranges, Role::Licence);

        // Truncated so the published range no longer fits: masking must refuse
        // rather than mask what it can and hash a shorter blob.
        let truncated = &canonical[..128];
        assert!(matches!(
            classify_against(truncated, table),
            Verdict::Unrecognised(_)
        ));
    }

    #[test]
    fn an_empty_address_is_unrecognised_and_says_so() {
        match classify(&[]) {
            Verdict::Unrecognised(finding) => {
                assert_eq!(finding.code_len, 0);
                assert_eq!(finding.to_string(), "the address holds no contract code");
            }
            Verdict::Canonical(_) => panic!("an address with no code cannot be canonical"),
        }
    }

    #[test]
    fn the_pre_filter_names_the_forbidden_function_it_found() {
        let mut code = fake_code(256, 0x10);
        code[100..104].copy_from_slice(&selector("seize(uint256)"));
        code[120..124].copy_from_slice(&selector("pause()"));

        let finding = match classify(&code) {
            Verdict::Unrecognised(finding) => finding,
            Verdict::Canonical(_) => panic!("synthetic code cannot be canonical"),
        };

        assert_eq!(finding.exposed, vec!["seize(uint256)", "pause()"]);
        let message = finding.to_string();
        assert!(
            message.contains("it exposes seize(uint256), pause()"),
            "the refusal must name what it saw, got: {message}"
        );
    }

    // ── The gate ─────────────────────────────────────────────────────────────

    /// The gate over a synthetic pinned table and a chain that must not be
    /// touched. Every gate test below that is not about the registry runs with
    /// no registry published, which is the shipped configuration.
    fn gate(code: &[u8], table: &'static [CanonicalContract]) -> Result<Attestation, Refusal> {
        let chain = ScriptedChain::new();
        let outcome = decide_verdict(classify_against(code, table), &chain, None, code, table);
        assert!(
            chain.calls().is_empty(),
            "with no registry published the gate must ask the chain nothing beyond the code it \
             was handed, got {:?}",
            chain.calls()
        );
        outcome
    }

    #[test]
    fn the_gate_accepts_a_licence_contract() {
        let code = fake_code(256, 0x10);
        let table = entry_for(&code, vec![], Role::Licence);
        let entry = gate(&code, table).expect("a licence entry is buyable");
        assert_eq!(entry.contract, "FakeLicence");
        assert_eq!(entry.authority, Authority::Pinned);
        assert_eq!(entry.advisory(), None);
    }

    #[test]
    fn the_gate_refuses_canonical_code_that_sells_nothing() {
        let code = fake_code(256, 0x10);
        for role in [Role::Factory, Role::Deployer, Role::CodeRegistry] {
            let table = entry_for(&code, vec![], role);
            match gate(&code, table) {
                Err(Refusal::NotALicence { role: refused, .. }) => {
                    assert_eq!(refused, Some(role))
                }
                other => panic!("{role:?} must not be a purchase target, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_gate_refuses_code_it_does_not_recognise() {
        let code = fake_code(256, 0x10);
        let refusal = gate(
            &code,
            entry_for(&fake_code(128, 0x99), vec![], Role::Licence),
        )
        .expect_err("synthetic code is not a canonical rub3 contract");
        assert!(matches!(refusal, Refusal::Unrecognised(_)));
    }

    /// Every `.rs` file under `root`, recursively, paired with its path
    /// relative to `root` so a module moved into a subdirectory is still named
    /// rather than escaping the walk.
    fn rust_sources(root: &std::path::Path) -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
            let entries =
                std::fs::read_dir(dir).expect("the crate's own src directory is readable");
            for entry in entries {
                let path = entry.expect("readable directory entry").path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let name = path
                        .strip_prefix(root)
                        .expect("walked from root")
                        .to_string_lossy()
                        .into_owned();
                    let body =
                        std::fs::read_to_string(&path).expect("a crate source file is readable");
                    out.push((name, body));
                }
            }
        }

        let mut out = Vec::new();
        walk(root, root, &mut out);
        out
    }

    /// `source` with `//` line comments removed, string literals left alone.
    ///
    /// A doc comment that *names* the module is not a way into it - `rpc.rs`
    /// points at `crate::attest` in prose - so counting one would leave this
    /// check unable to tell a reference from a mention.
    fn strip_line_comments(source: &str) -> String {
        source
            .lines()
            .map(|line| {
                let bytes = line.as_bytes();
                let mut in_string = false;
                let mut escaped = false;
                for i in 0..bytes.len() {
                    match bytes[i] {
                        b'\\' if in_string => escaped = !escaped,
                        b'"' if !escaped => {
                            in_string = !in_string;
                            escaped = false;
                        }
                        b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
                        _ => escaped = false,
                    }
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether `needle` occurs in `haystack` as a whole identifier, so
    /// `attestation` does not read as a reference to `attest`.
    fn mentions_ident(haystack: &str, needle: &str) -> bool {
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        haystack.match_indices(needle).any(|(at, _)| {
            boundary(haystack[..at].chars().next_back())
                && boundary(haystack[at + needle.len()..].chars().next())
        })
    }

    /// The text of `fn name` in `source`, from its signature line to the
    /// closing brace sitting at the signature's own indentation.
    ///
    /// `None` when no such function is declared, which a caller must treat as a
    /// failure rather than as nothing to check.
    fn function_body(source: &str, name: &str) -> Option<String> {
        let signature = format!("fn {name}(");
        let (start, indent) = source.lines().enumerate().find_map(|(i, line)| {
            let trimmed = line.trim_start();
            let declaration = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub "))
                .unwrap_or(trimmed);
            declaration
                .starts_with(&signature)
                .then(|| (i, line.len() - trimmed.len()))
        })?;

        let closing = format!("{}}}", " ".repeat(indent));
        let end = source
            .lines()
            .enumerate()
            .skip(start + 1)
            .find(|(_, line)| *line == closing)
            .map(|(i, _)| i)?;

        Some(
            source
                .lines()
                .take(end + 1)
                .skip(start)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Nothing in the crate reaches this module except a purchase path.
    ///
    /// This is the subtlest property in the module and the one a later change
    /// is most likely to break by accident, because adding "verify the contract
    /// before launching too" looks like an improvement. It is not: a launch is
    /// a program the user has already paid for, and a check that can refuse to
    /// start it is a revocation surface wearing an integrity check's clothes.
    /// The posture is structural - there is no shared helper and no flag - so
    /// what has to be guarded is the absence of a caller outside the paths that
    /// spend money.
    ///
    /// Three assertions, because no one of them is enough on its own:
    ///
    /// 1. the set of referencing modules is a **subset** of the purchase-path
    ///    allowlist;
    /// 2. each allowlisted purchase path holds **exactly one** gate call site,
    ///    since a subset is also satisfied by calling the gate nowhere at all;
    /// 3. the named human launch entry points inside `webview.rs` reference the
    ///    module **not at all**, which is the half the allowlist cannot speak
    ///    for: it is file-granular, and `webview.rs` holds both the path that
    ///    spends money and the paths that serve a token already held.
    ///
    /// **What it does not cover.** The module set is guarded per file and the
    /// launch entry points per name, so a *new* launch function added to
    /// `webview.rs` is unguarded until somebody names it in
    /// `WEBVIEW_LAUNCH_ENTRY_POINTS`, and the same file granularity means an
    /// attest reference elsewhere in `activation.rs` - `ensure_headless` is the
    /// launch path and lives there - is not caught either. This is a guard
    /// against the change made in good faith, not a proof.
    ///
    /// **It guards source structure, not runtime wiring.** It reads the crate's
    /// own `src/` at test time, so it says exactly the same thing under
    /// `tier-2` and `tier-2,webview`, where the call site is `cfg`-compiled out
    /// and the gate never runs at all. It does not prove the gate runs, and the
    /// behaviour it is paired with is
    /// `tests/headless_e2e.rs::headless_launch_of_a_held_licence_never_enters_the_purchase_path_e2e`,
    /// which drives a real launch of an already-held licence and asserts it
    /// activates without minting.
    ///
    /// It guards every way in rather than one name: [`classify`], [`decide`]
    /// and [`Verdict`] are as reachable as [`verify_before_purchase`], and a
    /// launch-path caller written against any of them reintroduces the same
    /// surface.
    #[test]
    fn the_attest_module_is_reachable_only_from_the_purchase_path() {
        // The paths that spend money, and the only modules that may reach this
        // one. Both hold exactly one gate call site: `activation.rs` in
        // `headless::purchase`, `webview.rs` in `show_purchase`.
        const PURCHASE_PATHS: &[&str] = &["activation.rs", "webview.rs"];

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let modules = rust_sources(&src);
        assert!(
            modules.iter().any(|(name, _)| name == "activation.rs"),
            "the walk found no activation.rs, so it is not reading what it thinks it is"
        );

        let mut referencing: Vec<&str> = modules
            .iter()
            // `attest.rs` is this module, and `lib.rs` only declares it.
            .filter(|(name, _)| name != "attest.rs" && name != "lib.rs")
            .filter(|(_, body)| mentions_ident(&strip_line_comments(body), "attest"))
            .map(|(name, _)| name.as_str())
            .collect();
        referencing.sort();

        let outside: Vec<&str> = referencing
            .iter()
            .copied()
            .filter(|name| !PURCHASE_PATHS.contains(name))
            .collect();
        assert!(
            outside.is_empty(),
            "{outside:?} reference the attest module and are not purchase paths.\n\n\
             If this failed because a launch path now names it - through verify_before_purchase, \
             classify, decide, Verdict or any other item - that is the one thing this module \
             must not do. Refusing to start an already-paid-for licence because an integrity \
             check could not complete is a de-facto revocation surface, which this project has \
             ruled out. Fail closed on purchase, fail open on launch.\n\n\
             If instead you are adding attestation to a NEW path that spends money, that is \
             legitimate and welcome: add that module to PURCHASE_PATHS above, deliberately, as \
             its own decision. Do not relax this test to let it through."
        );

        // Where each purchase path calls the gate, and how many times. A subset
        // of the allowlist is also satisfied by calling the gate nowhere at
        // all, so this is the half that keeps it from being dropped - and it
        // has to name every purchase path, or dropping the call from one of
        // them stays invisible.
        const GATE_CALL_SITES: &[(&str, &str)] = &[
            ("activation.rs", "headless::purchase"),
            ("webview.rs", "show_purchase"),
        ];

        for (module, call_site) in GATE_CALL_SITES {
            let gate_calls: usize = modules
                .iter()
                .filter(|(name, _)| name == module)
                .map(|(_, body)| {
                    strip_line_comments(body)
                        .matches("verify_before_purchase")
                        .count()
                })
                .sum();
            assert_eq!(
                gate_calls, 1,
                "{module}::{call_site} must call the gate exactly once, found {gate_calls}. \
                 Every path that asks for money verifies the code first, and a path that stopped \
                 asking the question would otherwise still satisfy the allowlist above."
            );
        }

        // The human launch entry points inside `webview.rs`. `PURCHASE_PATHS`
        // is file-granular and cannot tell `show_purchase`, which spends money,
        // from these, which serve a token the user already holds. Naming them
        // is what stops the allowlist from covering a launch path by accident.
        const WEBVIEW_LAUNCH_ENTRY_POINTS: &[&str] =
            &["show_activate", "show_cooldown", "finalize_session"];

        let webview = modules
            .iter()
            .find(|(name, _)| name == "webview.rs")
            .map(|(_, body)| strip_line_comments(body))
            .expect(
                "webview.rs is not in the crate's src/. If the front door moved, point this \
                 guard at its new home deliberately rather than letting it lapse.",
            );

        for entry_point in WEBVIEW_LAUNCH_ENTRY_POINTS {
            let body = function_body(&webview, entry_point).unwrap_or_else(|| {
                panic!(
                    "webview.rs declares no `fn {entry_point}`, so this guard has stopped \
                     guarding it without saying so. A guard that quietly covers nothing is worse \
                     than no guard, because it still reads as coverage. Update \
                     WEBVIEW_LAUNCH_ENTRY_POINTS against the current webview.rs deliberately - \
                     do not drop the name."
                )
            });
            assert!(
                !mentions_ident(&body, "attest"),
                "webview.rs::{entry_point} references the attest module. It serves a token the \
                 user already holds, so a check there can refuse to start an already-paid-for \
                 licence, which is the de-facto revocation surface this project has ruled out. \
                 Fail closed on purchase, fail open on launch: attestation belongs in \
                 show_purchase, the one path in this file that spends money."
            );
        }
    }

    /// Every pinned licence contract really is buyable, and every other role
    /// really is not - asserted by running the gate's decision over the shipped
    /// table, not a synthetic one.
    #[test]
    fn only_licence_roles_are_purchase_targets() {
        let chain = ScriptedChain::new();
        let mut licences = 0;
        for entry in CANONICAL {
            let outcome = decide_verdict(Verdict::Canonical(entry), &chain, None, &[], CANONICAL);
            match (entry.role, outcome) {
                (Role::Licence, Ok(accepted)) => {
                    assert_eq!(accepted.contract, entry.contract);
                    licences += 1;
                }
                (
                    role,
                    Err(Refusal::NotALicence {
                        contract,
                        role: refused,
                    }),
                ) if role != Role::Licence => {
                    assert_eq!(contract, entry.contract);
                    assert_eq!(refused, Some(role));
                }
                (role, outcome) => panic!(
                    "{} is pinned as {role:?}; the gate must {}, got {outcome:?}",
                    entry.contract,
                    if role == Role::Licence {
                        "accept it"
                    } else {
                        "refuse it as NotALicence"
                    }
                ),
            }
        }
        assert!(
            licences >= 2,
            "the table should pin both licence templates, found {licences}"
        );
    }

    // ── The registry ─────────────────────────────────────────────────────────

    use std::cell::RefCell;
    use std::collections::HashMap;

    /// The deployment manifest the per-chain registry table mirrors.
    const DEPLOYMENTS: &str = include_str!("../../../contracts/deployments.json");

    /// A chain whose every answer is written down in advance, and which records
    /// what it was asked.
    ///
    /// The call log is half the point. Several properties here are about reads
    /// that must *not* happen - a pinned-table hit asking the registry nothing,
    /// an unpublished registry costing no round trip - and those are invisible
    /// to a double that only answers questions.
    struct ScriptedChain {
        code: HashMap<Address, Vec<u8>>,
        code_error: Option<String>,
        tables: Vec<Vec<ImmutableRange>>,
        tables_error: Option<String>,
        /// Whether the scripted registry answers the way the deployed
        /// `latestOffsetTables` does - the newest `limit` tables, newest first.
        /// Turned off to script a node that answers with everything it has in
        /// the order it likes, which is the case the caller's own bound has to
        /// survive.
        honour_limit: bool,
        records: HashMap<[u8; 32], RegistryRecord>,
        record_error: Option<String>,
        calls: RefCell<Vec<String>>,
    }

    impl ScriptedChain {
        fn new() -> Self {
            Self {
                code: HashMap::new(),
                code_error: None,
                tables: Vec::new(),
                tables_error: None,
                honour_limit: true,
                records: HashMap::new(),
                record_error: None,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_code(mut self, at: Address, code: Vec<u8>) -> Self {
            self.code.insert(at, code);
            self
        }

        /// The registry's published tables, oldest first, as `publish` interned
        /// them. What a read returns out of this is the read's business.
        fn with_tables(mut self, tables: Vec<Vec<ImmutableRange>>) -> Self {
            self.tables = tables;
            self
        }

        fn with_record(mut self, hash: [u8; 32], record: RegistryRecord) -> Self {
            self.records.insert(hash, record);
            self
        }

        fn failing_code_reads(mut self) -> Self {
            self.code_error = Some("the node did not answer".to_string());
            self
        }

        /// Answers every published table, oldest first, regardless of the
        /// requested limit - the way a node that ignores `latestOffsetTables`
        /// and answers from the whole set would.
        fn ignoring_the_requested_limit(mut self) -> Self {
            self.honour_limit = false;
            self
        }

        fn failing_table_reads(mut self) -> Self {
            self.tables_error = Some("the node did not answer".to_string());
            self
        }

        fn failing_record_reads(mut self) -> Self {
            self.record_error = Some("the node did not answer".to_string());
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl ChainReader for ScriptedChain {
        fn code(&self, address: Address) -> Result<Vec<u8>, RpcError> {
            self.calls.borrow_mut().push(format!("code({address})"));
            if let Some(e) = &self.code_error {
                return Err(RpcError::Transport(e.clone()));
            }
            Ok(self.code.get(&address).cloned().unwrap_or_default())
        }

        fn offset_tables(
            &self,
            registry: Address,
            limit: usize,
        ) -> Result<Vec<Vec<ImmutableRange>>, RpcError> {
            self.calls
                .borrow_mut()
                .push(format!("latestOffsetTables({registry}, {limit})"));
            match &self.tables_error {
                Some(e) => Err(RpcError::Contract(e.clone())),
                None if self.honour_limit => {
                    Ok(self.tables.iter().rev().take(limit).cloned().collect())
                }
                None => Ok(self.tables.clone()),
            }
        }

        fn record(
            &self,
            registry: Address,
            masked_code_hash: [u8; 32],
        ) -> Result<Option<RegistryRecord>, RpcError> {
            self.calls.borrow_mut().push(format!(
                "record({registry}, {})",
                hex::encode(masked_code_hash)
            ));
            match &self.record_error {
                Some(e) => Err(RpcError::Contract(e.clone())),
                None => Ok(self.records.get(&masked_code_hash).cloned()),
            }
        }
    }

    /// The address a scripted registry lives at. Any address will do: what makes
    /// it believable in these tests is the code at it, never the number.
    fn registry_address() -> Address {
        Address::repeat_byte(0x2c)
    }

    /// Synthetic runtime code shaped like a real deploy: `ranges` are each
    /// preceded by a `PUSH32` opcode and filled with a deploy's values, exactly
    /// as solc emits an immutable and the deployer patches it.
    fn code_with_immutables(len: usize, ranges: &[ImmutableRange], fill: u8) -> Vec<u8> {
        let mut code = fake_code(len, fill);
        for range in ranges {
            code[range.start - 1] = PUSH32;
            code[range.start..range.start + range.length].fill(0xd0);
        }
        code
    }

    /// The immutable layout every registry test below uses. Three words, each
    /// with room for the `PUSH32` in front of it.
    fn layout() -> Vec<ImmutableRange> {
        vec![
            ImmutableRange {
                start: 64,
                length: 32,
            },
            ImmutableRange {
                start: 128,
                length: 32,
            },
            ImmutableRange {
                start: 192,
                length: 32,
            },
        ]
    }

    fn record_for(contract: &str, role: Option<Role>, status: RecordStatus) -> RegistryRecord {
        RegistryRecord {
            contract: contract.to_string(),
            version: "2026-09, a release newer than this binary".to_string(),
            status,
            role,
            registered_at_block: 31_415_926,
            offsets: layout(),
        }
    }

    /// A table pinning only the registry's own code, so a scripted registry can
    /// be believable without a real deployment to hash.
    fn registry_only_table(registry_code: &[u8]) -> &'static [CanonicalContract] {
        entry_for(registry_code, vec![], Role::CodeRegistry)
    }

    /// The three-way verdict, first arm: code the binary already knows.
    ///
    /// The property that matters beyond the acceptance is the call log. A
    /// pinned-table hit is the common case and it must cost nothing beyond the
    /// code read the gate already made, or the registry stops being a fallback
    /// and becomes a dependency on every purchase.
    #[test]
    fn a_pinned_table_hit_is_canonical_and_asks_the_registry_nothing() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let table = entry_for(&code, layout(), Role::Licence);
        let chain = ScriptedChain::new();

        let attested = decide_verdict(
            classify_against(&code, table),
            &chain,
            Some(registry_address()),
            &code,
            table,
        )
        .expect("pinned code is buyable");

        assert_eq!(attested.authority, Authority::Pinned);
        assert!(
            chain.calls().is_empty(),
            "a table hit must not consult the registry, got {:?}",
            chain.calls()
        );
    }

    /// The three-way verdict, second arm, and the whole reason this work exists:
    /// a contract from a template release newer than the binary is canonical,
    /// where before it was refused indistinguishably from a modified copy.
    #[test]
    fn a_release_newer_than_this_binary_is_canonical_from_the_registry() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let hash = masked_code_digest(&code, &layout()).expect("ranges fit");

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![layout()])
            .with_record(
                hash,
                record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active),
            );

        let attested = decide_verdict(
            classify_against(&code, table),
            &chain,
            Some(registry_address()),
            &code,
            table,
        )
        .expect("a published release is buyable");

        assert_eq!(attested.contract, "Rub3Access");
        assert_eq!(
            attested.authority,
            Authority::Registry {
                status: RecordStatus::Active,
                registered_at_block: 31_415_926,
            }
        );
        assert_eq!(attested.advisory(), None);
        assert!(
            attested
                .to_string()
                .contains("code registry at block 31415926"),
            "the acceptance line must say which authority vouched, got: {attested}"
        );
    }

    /// The three-way verdict, third arm: known to nobody, and refused - with the
    /// refusal now able to say that the authority was asked.
    #[test]
    fn a_modified_copy_is_refused_by_both_authorities() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![layout()]);

        let refusal = decide_verdict(
            classify_against(&code, table),
            &chain,
            Some(registry_address()),
            &code,
            table,
        )
        .expect_err("code neither authority knows is refused");

        match refusal {
            Refusal::Unrecognised(finding) => {
                assert_eq!(finding.registry, RegistryOutcome::Unknown);
                assert!(
                    finding
                        .to_string()
                        .contains("the on-chain code registry has no record of it either"),
                    "a refusal must say the authority was asked, got: {finding}"
                );
            }
            other => panic!("expected an unrecognised refusal, got {other:?}"),
        }
    }

    /// Deprecation is advice, and the distinction is a security property: a
    /// version authority that could refuse a purchase would be a revocation
    /// surface, which is the one thing an append-only registry must not become.
    #[test]
    fn a_deprecated_release_is_bought_with_a_warning() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let hash = masked_code_digest(&code, &layout()).expect("ranges fit");

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![layout()])
            .with_record(
                hash,
                record_for("Rub3Access", Some(Role::Licence), RecordStatus::Deprecated),
            );

        let attested = decide_verdict(
            classify_against(&code, table),
            &chain,
            Some(registry_address()),
            &code,
            table,
        )
        .expect("a deprecated release is still genuine code and still buyable");

        let advisory = attested
            .advisory()
            .expect("a deprecated release warns rather than passing in silence");
        assert!(advisory.contains("no longer recommended"), "{advisory}");
        assert!(
            advisory.contains("licences bought from it are unaffected"),
            "the warning must not read as a threat to a held licence: {advisory}"
        );
    }

    /// Canonical code that sells nothing is refused whichever authority vouched
    /// for it. A registry answering about the factory has to be refused exactly
    /// as the pinned table's factory row is, or the role check would hold on one
    /// path and not the other.
    #[test]
    fn a_registry_record_that_is_not_a_licence_is_refused_as_the_wrong_address() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let hash = masked_code_digest(&code, &layout()).expect("ranges fit");

        for role in [
            Some(Role::Factory),
            Some(Role::Deployer),
            Some(Role::CodeRegistry),
            // A role published by a registry newer than this build. Unknown is
            // refused too: a purchase may only target a role this binary can
            // name, and guessing at the first variant would make an unknown
            // role a licence.
            None,
        ] {
            let chain = ScriptedChain::new()
                .with_code(registry_address(), registry_code.clone())
                .with_tables(vec![layout()])
                .with_record(hash, record_for("Rub3Factory", role, RecordStatus::Active));

            match decide_verdict(
                classify_against(&code, table),
                &chain,
                Some(registry_address()),
                &code,
                table,
            ) {
                Err(Refusal::NotALicence {
                    contract,
                    role: refused,
                }) => {
                    assert_eq!(contract, "Rub3Factory");
                    assert_eq!(refused, role);
                }
                other => panic!("{role:?} must not be a purchase target, got {other:?}"),
            }
        }
    }

    // ── Believing the registry only after verifying it ───────────────────────

    /// The load-bearing check. A registry whose own code does not hash to the
    /// entry this binary pins is not an authority, however willing it is to
    /// answer: believing it would move the trust from one hash the user chose to
    /// run onto whoever put an address at that slot.
    ///
    /// The record is scripted and would be accepted if the code check were
    /// skipped, so this fails loudly rather than vacuously.
    #[test]
    fn a_registry_whose_own_code_is_not_canonical_is_never_believed() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let hash = masked_code_digest(&code, &layout()).expect("ranges fit");
        let table = registry_only_table(&fake_code(300, 0x77));

        let chain = ScriptedChain::new()
            // Not the code the table pins: an impostor at the registry address.
            .with_code(registry_address(), fake_code(300, 0x01))
            .with_tables(vec![layout()])
            .with_record(
                hash,
                record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active),
            );

        match consult_registry_against(&chain, registry_address(), &code, table) {
            RegistryVerdict::Unavailable(why) => assert!(
                why.contains("is not the code registry this build pins"),
                "the reason must name what was wrong, got: {why}"
            ),
            other => panic!(
                "an unverified registry must never be believed, got {other:?}. This is the check \
                 that terminates the trust recursion."
            ),
        }
    }

    /// The same refusal when the address holds *canonical* rub3 code that is
    /// simply not a registry - the factory, say. Genuine code at the wrong
    /// address is still not an authority.
    #[test]
    fn canonical_code_that_is_not_a_registry_is_not_believed_either() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let factory_code = fake_code(300, 0x33);
        let table = entry_for(&factory_code, vec![], Role::Factory);

        let chain = ScriptedChain::new()
            .with_code(registry_address(), factory_code)
            .with_tables(vec![layout()]);

        match consult_registry_against(&chain, registry_address(), &code, table) {
            RegistryVerdict::Unavailable(why) => {
                assert!(why.contains("which is not a code registry"), "got: {why}")
            }
            other => panic!("the factory is not a version authority, got {other:?}"),
        }
    }

    /// The code read has to come before anything else, so a registry that
    /// cannot be verified is never asked a question. Asserted through the call
    /// log, because an answer that was never sought cannot be believed by
    /// accident later.
    #[test]
    fn an_unverified_registry_is_never_asked_anything() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let table = registry_only_table(&fake_code(300, 0x77));

        let chain = ScriptedChain::new()
            .with_code(registry_address(), fake_code(300, 0x01))
            .with_tables(vec![layout()]);

        let _ = consult_registry_against(&chain, registry_address(), &code, table);

        assert_eq!(
            chain.calls(),
            vec![format!("code({})", registry_address())],
            "verification comes first, and nothing follows a failed one"
        );
    }

    // ── Reads that fail, on a path that fails closed ─────────────────────────

    #[test]
    fn a_registry_that_cannot_be_reached_leaves_the_refusal_standing() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);

        // Every read fails: the registry's code is the first one, so this is
        // the shape of an unreachable node.
        let unreachable = ScriptedChain::new().failing_code_reads();
        match consult_registry_against(&unreachable, registry_address(), &code, table) {
            RegistryVerdict::Unavailable(why) => {
                assert!(why.contains("its code could not be read"), "got: {why}")
            }
            other => panic!("an unreachable registry cannot vouch for anything, got {other:?}"),
        }

        // Reachable, verified, and then failing on each later read in turn.
        let no_tables = ScriptedChain::new()
            .with_code(registry_address(), registry_code.clone())
            .failing_table_reads();
        match consult_registry_against(&no_tables, registry_address(), &code, table) {
            RegistryVerdict::Unavailable(why) => assert!(
                why.contains("its offset tables could not be read"),
                "got: {why}"
            ),
            other => panic!("expected unavailable, got {other:?}"),
        }

        let no_record = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![layout()])
            .failing_record_reads();
        match consult_registry_against(&no_record, registry_address(), &code, table) {
            RegistryVerdict::Unavailable(why) => {
                assert!(why.contains("its record could not be read"), "got: {why}")
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    /// A registry read that failed is still a refusal - the gate fails closed on
    /// the path that spends money - but the refusal says the answer was missing
    /// rather than negative, because those are different things to act on.
    #[test]
    fn a_failed_registry_read_refuses_and_says_it_could_not_ask() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let table = registry_only_table(&fake_code(300, 0x77));
        let chain = ScriptedChain::new().failing_code_reads();

        let refusal = decide_verdict(
            classify_against(&code, table),
            &chain,
            Some(registry_address()),
            &code,
            table,
        )
        .expect_err("a registry that could not be asked is not permission to spend");

        match refusal {
            Refusal::Unrecognised(finding) => {
                assert!(matches!(finding.registry, RegistryOutcome::Unavailable(_)));
                assert_eq!(finding.registry.as_label(), "unavailable");
                assert!(
                    finding.to_string().contains("could not be consulted"),
                    "got: {finding}"
                );
            }
            other => panic!("expected an unrecognised refusal, got {other:?}"),
        }
    }

    // ── The offsets bootstrap, and the bound on the blind spot ───────────────

    /// Every published table is a candidate, because the hash cannot be computed
    /// without one and the binary does not know which describes this contract.
    /// Candidates are tried newest first, and the record here sits under the
    /// *older* of the two, so a lookup that stopped at the newest would refuse a
    /// perfectly good release.
    #[test]
    fn every_published_offset_table_is_tried() {
        let newer = vec![
            ImmutableRange {
                start: 64,
                length: 32,
            },
            ImmutableRange {
                start: 128,
                length: 32,
            },
        ];
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let hash = masked_code_digest(&code, &layout()).expect("ranges fit");

        let mut record = record_for(
            "Rub3Subscription",
            Some(Role::Licence),
            RecordStatus::Active,
        );
        record.offsets = layout();

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![layout(), newer])
            .with_record(hash, record);

        match consult_registry_against(&chain, registry_address(), &code, table) {
            RegistryVerdict::Known(found) => assert_eq!(found.contract, "Rub3Subscription"),
            other => panic!("the older candidate table holds the answer, got {other:?}"),
        }
        assert_eq!(
            chain.calls().len(),
            4,
            "one code read, one table read, one lookup per candidate: {:?}",
            chain.calls()
        );
    }

    /// The budget is spent on the newest layouts, because a registry is asked
    /// only about code the pinned table did not recognise and that is by
    /// definition code newer than this build.
    ///
    /// The scripted registry publishes four times the cap, and the record sits
    /// under the newest layout of all - the position a release published after
    /// this binary was packed occupies. Reading from the old end would spend
    /// every candidate on layouts interned long before this build existed and
    /// return `Unknown`, refusing a genuine release; that is the failure this
    /// asserts against. One lookup, because the likeliest candidate is tried
    /// first.
    #[test]
    fn the_budget_is_spent_on_the_newest_layouts() {
        let published = MAX_CANDIDATE_OFFSET_TABLES * 4;
        let slots: Vec<ImmutableRange> = (0..published)
            .map(|i| ImmutableRange {
                start: 64 + i * 64,
                length: 32,
            })
            .collect();
        let code = code_with_immutables(128 + published * 64, &slots, 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let candidates: Vec<Vec<ImmutableRange>> = slots.iter().map(|r| vec![*r]).collect();

        let newest = candidates[published - 1].clone();
        let hash = masked_code_digest(&code, &newest).expect("ranges fit");
        let mut record = record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active);
        record.offsets = newest;

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(candidates.clone())
            .with_record(hash, record);

        match consult_registry_against(&chain, registry_address(), &code, table) {
            RegistryVerdict::Known(found) => assert_eq!(found.contract, "Rub3Access"),
            other => panic!(
                "a release published under the newest layout must be reachable, got {other:?}"
            ),
        }
        assert_eq!(
            chain
                .calls()
                .iter()
                .filter(|call| call.starts_with("record("))
                .count(),
            1,
            "newest first also means the likeliest candidate costs one lookup: {:?}",
            chain.calls()
        );

        // The other end of the same set, for contrast: a layout older than the
        // budget is past the cap and is not read. Refusing a release nobody
        // could look up is what a build with no registry at all does, and it is
        // the trade the cap makes - which is why the budget goes to the end the
        // question is about.
        let oldest = candidates[0].clone();
        let hash = masked_code_digest(&code, &oldest).expect("ranges fit");
        let mut record = record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active);
        record.offsets = oldest;

        let ancient = ScriptedChain::new()
            .with_code(registry_address(), fake_code(300, 0x77))
            .with_tables(candidates)
            .with_record(hash, record);

        assert_eq!(
            consult_registry_against(&ancient, registry_address(), &code, table),
            RegistryVerdict::Unknown,
            "a layout older than the budget is untried, and untried says what dropped says"
        );
    }

    /// The purchase path spends money, and every surviving candidate costs its
    /// own sequential round trip, so the list of them is capped - at the read as
    /// well as at the loop.
    ///
    /// The scripted registry publishes one more usable table than the cap
    /// allows and hides the record under the one table the budget cannot reach,
    /// the oldest - the arrangement an owner key would choose to make a purchase
    /// as slow as it can while still, eventually, answering. It answers the way
    /// the deployed `latestOffsetTables` does, newest first and bounded, so the
    /// table past the cap never reaches the wrapper at all. The verdict is
    /// `Unknown`, which is what a dropped table already says, and the call log
    /// is the assertion that matters: the cap is what was asked of the registry,
    /// and exactly the cap's worth of lookups followed, not one more.
    #[test]
    fn candidate_offset_tables_are_capped_on_the_purchase_path() {
        let overflow = MAX_CANDIDATE_OFFSET_TABLES + 1;
        // One immutable slot per candidate, each clear of its neighbours, so
        // every published table describes real `PUSH32` immediates of this code
        // and not one of them is dropped. The cap is then the only thing that
        // can stop the loop.
        let slots: Vec<ImmutableRange> = (0..overflow)
            .map(|i| ImmutableRange {
                start: 64 + i * 64,
                length: 32,
            })
            .collect();
        let code = code_with_immutables(128 + overflow * 64, &slots, 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);

        // Each candidate masks a different slot, so no two digests collide and
        // a lookup under one table cannot stumble onto another's record. The
        // one published record sits under the oldest candidate, one past the
        // cap now that the budget is spent from the newest end.
        let candidates: Vec<Vec<ImmutableRange>> = slots.iter().map(|r| vec![*r]).collect();
        let beyond = candidates[0].clone();
        let hash = masked_code_digest(&code, &beyond).expect("ranges fit");
        let mut record = record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active);
        record.offsets = beyond;

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(candidates)
            .with_record(hash, record);

        assert_eq!(
            consult_registry_against(&chain, registry_address(), &code, table),
            RegistryVerdict::Unknown,
            "past the cap a candidate is untried, and untried says what dropped says"
        );
        let lookups = chain
            .calls()
            .iter()
            .filter(|call| call.starts_with("record("))
            .count();
        assert_eq!(
            lookups,
            MAX_CANDIDATE_OFFSET_TABLES,
            "the cap bounds the round trips a purchase pays for: {:?}",
            chain.calls()
        );
        assert!(
            chain.calls().iter().any(|call| call
                == &format!(
                    "latestOffsetTables({}, {MAX_CANDIDATE_OFFSET_TABLES})",
                    registry_address()
                )),
            "the cap must also be what the registry was asked for, so a long \
             published set is never transferred or decoded: {:?}",
            chain.calls()
        );
    }

    /// The bound is asked of the registry, not trimmed off its answer.
    ///
    /// The published set is the owner key's to grow, and a purchase that pays to
    /// transfer, decode and shape-check all of it is slow before the loop's cap
    /// can engage. So the window is requested bounded: a registry holding many
    /// more tables than the wrapper will try delivers only the ones asked for,
    /// and the shape check never sees the rest. Latency only - reading the whole
    /// set could never have produced a wrong verdict, only a slow one.
    #[test]
    fn the_offset_table_read_is_bounded_before_the_answer_is_decoded() {
        let published = MAX_CANDIDATE_OFFSET_TABLES * 4;
        let slots: Vec<ImmutableRange> = (0..published)
            .map(|i| ImmutableRange {
                start: 64 + i * 64,
                length: 32,
            })
            .collect();
        let code = code_with_immutables(128 + published * 64, &slots, 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let candidates: Vec<Vec<ImmutableRange>> = slots.iter().map(|r| vec![*r]).collect();

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(candidates);

        assert_eq!(
            consult_registry_against(&chain, registry_address(), &code, table),
            RegistryVerdict::Unknown,
            "no record is published, so no candidate answers"
        );
        assert_eq!(
            chain
                .calls()
                .iter()
                .filter(|call| call.starts_with("record("))
                .count(),
            MAX_CANDIDATE_OFFSET_TABLES,
            "only the tables the registry was asked for exist to be tried: {:?}",
            chain.calls()
        );
        assert_eq!(
            chain.calls().first().map(String::as_str),
            Some(format!("code({})", registry_address()).as_str()),
            "verification still comes first"
        );
        assert_eq!(
            chain.calls().get(1),
            Some(&format!(
                "latestOffsetTables({}, {MAX_CANDIDATE_OFFSET_TABLES})",
                registry_address()
            )),
            "one bounded read of the newest layouts, and the bound is the wrapper's cap: {:?}",
            chain.calls()
        );
    }

    /// A node is not obliged to honour the window it was asked for, or the end
    /// of the set it was asked for, so the cap is held locally too.
    ///
    /// Same arrangement as the bounded read above, against a scripted registry
    /// that answers with every table it has, oldest first, regardless of the
    /// limit. The extra entries cost nothing beyond the one response they
    /// arrived in: the loop still stops at the cap, so no additional round trip
    /// is paid for. What such a node can do is hide a record behind the entries
    /// it chose to send first, and that costs coverage rather than correctness -
    /// the verdict is `Unknown` and the purchase is refused, exactly as it would
    /// be with no registry at all.
    #[test]
    fn a_registry_that_ignores_the_requested_window_still_buys_no_extra_lookups() {
        let published = MAX_CANDIDATE_OFFSET_TABLES * 4;
        let slots: Vec<ImmutableRange> = (0..published)
            .map(|i| ImmutableRange {
                start: 64 + i * 64,
                length: 32,
            })
            .collect();
        let code = code_with_immutables(128 + published * 64, &slots, 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let candidates: Vec<Vec<ImmutableRange>> = slots.iter().map(|r| vec![*r]).collect();

        // The record sits under the newest table of all, which this node buries
        // behind everything older: an untried candidate contributes nothing,
        // however the answer arrived.
        let beyond = candidates[published - 1].clone();
        let hash = masked_code_digest(&code, &beyond).expect("ranges fit");
        let mut record = record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active);
        record.offsets = beyond;

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(candidates)
            .ignoring_the_requested_limit()
            .with_record(hash, record);

        assert_eq!(
            consult_registry_against(&chain, registry_address(), &code, table),
            RegistryVerdict::Unknown,
            "past the cap a candidate is untried, and untried says what dropped says"
        );
        assert_eq!(
            chain
                .calls()
                .iter()
                .filter(|call| call.starts_with("record("))
                .count(),
            MAX_CANDIDATE_OFFSET_TABLES,
            "an over-long answer must not buy round trips the cap refused: {:?}",
            chain.calls()
        );
    }

    /// A masked byte is a byte the comparison never looks at, so a table that
    /// does not describe `PUSH32` immediates of *this* code is a blind spot
    /// somebody else chose. It is dropped rather than masked with.
    ///
    /// The assertion that matters is the call log: the hostile table is never
    /// used to compute a hash, so the record scripted under it is never even
    /// asked for. The verdict is `Unknown` because that is what it means - no
    /// published release is shaped like this code - and the refusal is the same
    /// either way.
    #[test]
    fn an_offset_table_that_does_not_describe_immutables_is_dropped() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);

        // Shifted by one byte: the same widths in the same order, and not one
        // of them sitting behind a PUSH32.
        let hostile: Vec<ImmutableRange> = layout()
            .into_iter()
            .map(|r| ImmutableRange {
                start: r.start + 1,
                length: r.length,
            })
            .collect();
        let hash = masked_code_digest(&code, &hostile).expect("ranges fit");

        let mut record = record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active);
        record.offsets = hostile.clone();

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![hostile])
            .with_record(hash, record);

        assert_eq!(
            consult_registry_against(&chain, registry_address(), &code, table),
            RegistryVerdict::Unknown,
            "a table that masks bytes which can execute must never be used"
        );
        assert!(
            !chain.calls().iter().any(|call| call.starts_with("record(")),
            "the hostile table must not even be hashed under, got {:?}",
            chain.calls()
        );
    }

    /// A record whose own declared ranges differ from the table its hash was
    /// computed under is a registry describing one layout while answering about
    /// another. Not an answer to act on.
    #[test]
    fn a_record_declaring_other_ranges_than_the_table_that_found_it_is_not_believed() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let hash = masked_code_digest(&code, &layout()).expect("ranges fit");

        let mut record = record_for("Rub3Access", Some(Role::Licence), RecordStatus::Active);
        record.offsets = vec![ImmutableRange {
            start: 64,
            length: 32,
        }];

        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![layout()])
            .with_record(hash, record);

        match consult_registry_against(&chain, registry_address(), &code, table) {
            RegistryVerdict::Unavailable(why) => assert!(
                why.contains("declares immutable ranges other than the table that found it"),
                "got: {why}"
            ),
            other => panic!("an inconsistent record is not an answer, got {other:?}"),
        }
    }

    #[test]
    fn immutable_slot_validation_rejects_every_shape_that_is_not_one() {
        let code = code_with_immutables(256, &layout(), 0x10);
        assert_eq!(ranges_are_immutable_slots(&code, &layout()), Ok(()));
        assert_eq!(ranges_are_immutable_slots(&code, &[]), Ok(()));

        let cases: &[(ImmutableRange, &str)] = &[
            (
                ImmutableRange {
                    start: 64,
                    length: 64,
                },
                "a range is not one 32-byte word",
            ),
            (
                ImmutableRange {
                    start: 240,
                    length: 32,
                },
                "a range falls outside the fetched code",
            ),
            (
                ImmutableRange {
                    start: 96,
                    length: 32,
                },
                "a range is not the immediate operand of a PUSH32",
            ),
        ];
        for (range, expected) in cases {
            assert_eq!(
                ranges_are_immutable_slots(&code, std::slice::from_ref(range)),
                Err(*expected),
                "{range:?}"
            );
        }

        // Sorted and disjoint, checked in the order the manifest publishes.
        assert_eq!(
            ranges_are_immutable_slots(
                &code,
                &[
                    ImmutableRange {
                        start: 128,
                        length: 32
                    },
                    ImmutableRange {
                        start: 64,
                        length: 32
                    },
                ]
            ),
            Err("ranges are not sorted and disjoint")
        );

        // A range at offset zero has no opcode in front of it to be an operand
        // of, so it cannot be an immutable slot.
        assert_eq!(
            ranges_are_immutable_slots(
                &code,
                &[ImmutableRange {
                    start: 0,
                    length: 32
                }]
            ),
            Err("a range is not the immediate operand of a PUSH32")
        );
    }

    // ── Inert until deployed ─────────────────────────────────────────────────

    /// Nothing is deployed, so the registry step must change nothing at all.
    ///
    /// This is the property the whole change is measured against: shipping a
    /// lookup that quietly altered the refusal path before there was anything to
    /// look up would be the expensive version of this work. Three assertions,
    /// because the interesting failures differ:
    ///
    /// 1. no chain carries a registry address, which is what makes 2 and 3 a
    ///    statement about the shipped build rather than about a fixture;
    /// 2. a table miss costs no extra chain read, so the purchase path makes the
    ///    same one request it made before;
    /// 3. the refusal reads exactly as it did, down to the sentence, so nothing
    ///    downstream of it - an operator's alerting, a buyer's screen - moved.
    #[test]
    fn an_unpublished_registry_changes_nothing_about_the_gate() {
        for deployment in REGISTRIES {
            assert_eq!(
                deployment.address, None,
                "{} carries a published registry address. That is a real deploy, and this test \
                 stops being a statement about the shipped build - update it deliberately, \
                 alongside contracts/deployments.json, rather than deleting it.",
                deployment.name
            );
        }

        let code = fake_code(256, 0x10);
        let chain = ScriptedChain::new();
        let refusal = decide(&chain, 8453, &code)
            .expect_err("synthetic code is not a canonical rub3 contract");

        assert!(
            chain.calls().is_empty(),
            "with no registry published a miss must cost no extra chain read, got {:?}",
            chain.calls()
        );

        match refusal {
            Refusal::Unrecognised(finding) => {
                assert_eq!(finding.registry, RegistryOutcome::NotConsulted);
                assert_eq!(
                    finding.to_string(),
                    "256 bytes of code matching no canonical rub3 contract this build knows",
                    "the refusal a buyer and an orchestrator see must be the one they saw before \
                     the registry step existed"
                );
            }
            other => panic!("expected an unrecognised refusal, got {other:?}"),
        }
    }

    /// An address with no code reads the same way it always has, registry step
    /// or not. It is the one refusal a person can usually fix themselves, and
    /// the wording is what tells them so.
    #[test]
    fn an_empty_address_still_says_the_address_is_empty() {
        let chain = ScriptedChain::new();
        let refusal = decide(&chain, 8453, &[]).expect_err("no code is not a licence");
        match refusal {
            Refusal::Unrecognised(finding) => {
                assert_eq!(finding.to_string(), "the address holds no contract code")
            }
            other => panic!("expected an unrecognised refusal, got {other:?}"),
        }
    }

    /// And it still reads that way once a registry exists to ask.
    ///
    /// An empty address is not a version question: there is no release for an
    /// authority to recognise, so the reads could only ever return `Unknown`
    /// about nothing. The wording matters as much as the saved round trips.
    /// "The address holds no contract code; the on-chain code registry has no
    /// record of it either" turns the one refusal a person can usually fix
    /// themselves into a verdict on something that is not there.
    #[test]
    fn an_empty_address_is_refused_without_asking_the_registry() {
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let chain = ScriptedChain::new()
            .with_code(registry_address(), registry_code)
            .with_tables(vec![layout()]);

        let refusal = decide_verdict(
            classify_against(&[], table),
            &chain,
            Some(registry_address()),
            &[],
            table,
        )
        .expect_err("no code is not a licence");

        assert!(
            chain.calls().is_empty(),
            "an address holding no code has nothing to look up, got {:?}",
            chain.calls()
        );
        match refusal {
            Refusal::Unrecognised(finding) => {
                assert_eq!(finding.registry, RegistryOutcome::NotConsulted);
                assert_eq!(finding.to_string(), "the address holds no contract code");
                assert_eq!(
                    finding.shareable_detail(),
                    "the address holds no contract code"
                );
            }
            other => panic!("expected an unrecognised refusal, got {other:?}"),
        }
    }

    /// Registry-supplied text is the only text this module repeats that it did
    /// not write, and the surfaces that quote it have a grammar.
    ///
    /// `publish` requires a record's `contractName` and `version` to be
    /// non-empty and requires nothing else of them. The agent door's
    /// machine-readable detail line is space-separated `key=value` pairs and
    /// carries the name as `canonical=<name>`, so a published space or `=`
    /// invents a field an orchestrator then reads; both doors print the
    /// attestation as a single `rub3:` line on stderr, so a published newline
    /// forges a line of the wrapper's own output. Neither is possible with the
    /// pinned table, whose names are Solidity identifiers fixed at build time,
    /// which is why the reduction lives where the registry's answer enters.
    #[test]
    fn registry_supplied_text_cannot_break_the_line_that_quotes_it() {
        let code = code_with_immutables(256, &layout(), 0x10);
        let registry_code = fake_code(300, 0x77);
        let table = registry_only_table(&registry_code);
        let hash = masked_code_digest(&code, &layout()).expect("ranges fit");

        let hostile = |role: Option<Role>| {
            let mut record = record_for(
                "Rub3Access sells_licences=true\nrub3: forged",
                role,
                RecordStatus::Deprecated,
            );
            record.version = format!("{}\nrub3: 0xdead verified as canonical", "9".repeat(200));
            ScriptedChain::new()
                .with_code(registry_address(), registry_code.clone())
                .with_tables(vec![layout()])
                .with_record(hash, record)
        };

        let chain = hostile(Some(Role::Licence));
        let attested = decide_verdict(
            classify_against(&code, table),
            &chain,
            Some(registry_address()),
            &code,
            table,
        )
        .expect("a published licence release is still buyable, whatever it called itself");

        assert!(
            !attested.contract.contains([' ', '=', '\n']),
            "a published name reaches a key=value field and must stay one token, got {:?}",
            attested.contract
        );
        for line in [
            attested.to_string(),
            attested.release.clone(),
            attested
                .advisory()
                .expect("the record is deprecated, so it advises"),
        ] {
            assert!(
                !line.contains(['\n', '\r']),
                "published text must never add a line to the wrapper's own output, got {line:?}"
            );
        }
        assert!(
            attested.release.chars().count() <= MAX_REGISTRY_TEXT + 3,
            "a record's label is the publisher's choice of length and must be bounded, got {} \
             characters",
            attested.release.chars().count()
        );

        // The same text on the refusal surface, which is where `canonical=` is
        // actually written.
        let chain = hostile(Some(Role::Factory));
        match decide_verdict(
            classify_against(&code, table),
            &chain,
            Some(registry_address()),
            &code,
            table,
        )
        .expect_err("canonical code that sells nothing is the wrong address")
        {
            Refusal::NotALicence { contract, .. } => assert!(
                !contract.contains([' ', '=', '\n']),
                "`canonical={contract}` has to survive a space-separated parse"
            ),
            other => panic!("expected a not-a-licence refusal, got {other:?}"),
        }
    }

    // ── Drift protection for the registry's own contracts ────────────────────

    /// [`CANONICAL`]'s accumulate-only rule is not live yet, and this is the
    /// check that says so rather than the doc comment claiming it.
    ///
    /// The rule is that a contract's row becomes permanent once that contract is
    /// deployed, because a deployed contract goes on selling and validating its
    /// own tokens forever. Its precondition is a fact about
    /// `contracts/deployments.json` and not about anyone's memory: while every
    /// `factory` and every `code_registry` there is `null`, no row protects a
    /// holder and a fingerprint may still be corrected in place. The two records
    /// are independent deploys, so either one going live arms the rule for that
    /// chain and for whatever it put on chain.
    ///
    /// This fails the moment a deploy is recorded, which is the point: the
    /// sentence in [`CANONICAL`]'s doc stops being true at that moment, and a
    /// permanence rule whose precondition nobody re-checks is how a row that
    /// protects a holder gets overwritten by somebody following a stale comment.
    #[test]
    fn nothing_is_deployed_so_the_accumulate_only_rule_is_not_live_yet() {
        let manifest: serde_json::Value =
            serde_json::from_str(DEPLOYMENTS).expect("contracts/deployments.json is not JSON");
        let chains = manifest["chains"]
            .as_object()
            .expect("contracts/deployments.json has a chains object");

        for (id, chain) in chains {
            for record in ["factory", "code_registry"] {
                assert!(
                    chain[record].is_null(),
                    "chain {id} publishes a {record} at {}, so attest::CANONICAL's \
                     accumulate-only rule is now live for whatever that deploy put on \
                     chain: rows for deployed contracts may never be overwritten or \
                     dropped again, only added to. Update CANONICAL's doc comment to say \
                     the rule is live rather than pending, then update this test to assert \
                     what is still unpublished.",
                    chain[record]
                );
            }
        }
    }

    /// The per-chain registry table is the deployment manifest, compiled in.
    ///
    /// `contracts/deployments.json` is the one committed place that answers
    /// "which registry is canonical here", and the wrapper cannot read a file at
    /// runtime, so the answer is pinned. This is what stops the pinned copy from
    /// drifting: a chain added there and not here would leave agents on that
    /// chain silently refusing every release newer than their binary, and an
    /// address here that is not there would be an authority nobody published.
    #[test]
    fn registry_table_mirrors_the_deployment_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(DEPLOYMENTS).expect("contracts/deployments.json is not JSON");
        let chains = manifest["chains"]
            .as_object()
            .expect("deployments.json has no `chains` object");

        assert_eq!(
            chains.len(),
            REGISTRIES.len(),
            "contracts/deployments.json answers for {} chain(s) and attest::REGISTRIES for {}. \
             Add the missing chain here, or this build refuses every release newer than its own \
             table on it.",
            chains.len(),
            REGISTRIES.len()
        );

        for (id, chain) in chains {
            let chain_id: u64 = id.parse().expect("a chain key is a decimal chain id");
            let pinned = REGISTRIES
                .iter()
                .find(|deployment| deployment.chain_id == chain_id)
                .unwrap_or_else(|| {
                    panic!(
                        "contracts/deployments.json answers for chain {chain_id}, and \
                            attest::REGISTRIES does not"
                    )
                });

            assert_eq!(
                pinned.name,
                chain["name"].as_str().expect("a chain has a name"),
                "chain {chain_id}: the pinned name and the manifest's disagree"
            );

            // `null` is the only marker for "not deployed", in the manifest and
            // here alike: no placeholder, no zero address, and nothing
            // substituted for it. The manifest's own note says a consumer must
            // stop rather than guess, and `None` is what stopping looks like.
            let published = chain["code_registry"].as_str();
            match (published, pinned.address) {
                (None, None) => {}
                (Some(address), Some(pinned_address)) => assert_eq!(
                    pinned_address.to_string().to_lowercase(),
                    address.to_lowercase(),
                    "chain {chain_id}: the pinned registry address is not the published one"
                ),
                (Some(address), None) => panic!(
                    "chain {chain_id}: contracts/deployments.json publishes a code registry at \
                     {address} and attest::REGISTRIES still says None, so this build will refuse \
                     every release newer than its own table on that chain"
                ),
                (None, Some(address)) => panic!(
                    "chain {chain_id}: attest::REGISTRIES pins a registry at {address} that \
                     contracts/deployments.json does not publish. An address trusted because \
                     somebody wrote it down here is exactly what this module exists not to do"
                ),
            }
        }
    }

    /// A role number this build has no name for is never guessed at.
    ///
    /// A registry newer than this binary can publish a role this build has
    /// never heard of, and the numbering is a wire encoding: guessing at the
    /// first variant would make an unknown role a *licence*, which is the one
    /// purchase target the gate exists to be careful about. The values this
    /// build does name are checked where they can actually break, over the wire
    /// in `tests/code_registry_e2e.rs`; a local mapping cannot confirm its own
    /// numbering.
    #[test]
    fn an_unknown_role_number_is_never_guessed_at() {
        assert_eq!(
            Role::from_u8(4),
            None,
            "a role this build has no name for must stay unnamed rather than becoming the first \
             variant, which is a licence"
        );
    }
}
