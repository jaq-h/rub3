//! Is this contract the code we think it is?
//!
//! An agent about to spend money on a contract it discovered has one question
//! it cannot answer from the contract's name, its ABI, or its own good
//! intentions: is the code at that address the rub3 template this binary was
//! built against, or a modified copy of it? This module answers exactly that,
//! from bytes it fetches once, against a table compiled into the binary.
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
//! Every masked range is the immediate operand of a `PUSH32`, and EVM
//! jump-destination analysis excludes bytes inside push immediates, so no
//! control flow can reach a masked byte as an instruction. A masked-hash match
//! is therefore a complete statement about the contract's **executable code**:
//! there is nowhere in the blind spot for hidden code to live.
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
//!   reduces to `eth_getCode` being answered truthfully by a single endpoint
//!   baked in at pack time. An endpoint that lies returns canonical code for a
//!   hostile contract. The honest claim is "an honest view of chain state
//!   implies canonical code", and no stronger one should be made anywhere.
//!
//! The check is sound against replacement because `evm_version = "cancun"` and
//! Base has been on Cancun since Ecotone: under EIP-6780 `SELFDESTRUCT` only
//! deletes an account created in the same transaction, so code at an address
//! cannot be destroyed and replaced between the agent's `eth_getCode` and its
//! `purchase()`.
//!
//! ## Failure posture: closed on purchase, open on launch
//!
//! [`verify_before_purchase`] is a gate: anything other than a table hit on a
//! licence contract stops the run before a transaction is signed, including a
//! chain read that failed. Refusing to spend money on code that could not be
//! verified is the correct default.
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
/// licences. The factory and its two deployer helpers are canonical rub3 code
/// and are pinned here so the table stays a total mirror of the published
/// manifest, but buying from one is a category error, and
/// [`verify_before_purchase`] refuses it as such rather than letting a
/// transaction find out on-chain.
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
    /// Which contract release produced this fingerprint. Entries accumulate:
    /// an older release stays canonical forever, because the contract it
    /// describes goes on validating its own tokens forever.
    pub release: &'static str,
    /// `sha256` of the runtime code with `immutable_ranges` zeroed, lowercase
    /// hex, no `0x` prefix.
    pub masked_sha256: &'static str,
    /// The ranges to zero, sorted by offset.
    pub immutable_ranges: &'static [ImmutableRange],
}

/// The contract release every entry below was generated from.
///
/// A label, not a version the code branches on. It exists so a refusal or an
/// audit log can say *which* canonical contract matched once this table holds
/// more than one release of the same contract.
const RELEASE: &str = "2026-08, contracts at implementation.md §2.3 (factory + protocol fee)";

/// Every contract this binary accepts as canonical rub3 code.
///
/// Generated from `contracts/canonical-bytecode.json`, which the blocking
/// `bytecode-fingerprints` CI job regenerates and diffs on every pull request.
/// To refresh after a legitimate contract change: run
/// `scripts/canonical-bytecode-hashes.sh update`, then add the new rows here.
/// **Add, do not overwrite** - a deployed contract stays canonical after its
/// source moves on, and dropping its row would refuse a contract this project
/// itself deployed and sold from.
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
        masked_sha256: "d876353e538bb876a3eb3e6e870242986c6bdde6f7bcf930ee5815c9e91a0b54",
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
            ImmutableRange { start: 8660, length: 32 },
            ImmutableRange { start: 8702, length: 32 },
            ImmutableRange { start: 10010, length: 32 },
        ],
    },
    CanonicalContract {
        contract: "Rub3AccessDeployer",
        source: "contracts/src/Rub3Factory.sol",
        role: Role::Deployer,
        release: RELEASE,
        masked_sha256: "32bfebacb709f0bfab9126ac9ebf8a9164064c8c22363a98b7af5aeb3ab888ae",
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
        masked_sha256: "64a4c7bb65dc980c1de33dd9c9a0eae42cb017257162f7e0d8ab5520b2c7ccb4",
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
            ImmutableRange { start: 7985, length: 32 },
            ImmutableRange { start: 8727, length: 32 },
            ImmutableRange { start: 9737, length: 32 },
            ImmutableRange { start: 9779, length: 32 },
            ImmutableRange { start: 9883, length: 32 },
            ImmutableRange { start: 11421, length: 32 },
        ],
    },
    CanonicalContract {
        contract: "Rub3SubscriptionDeployer",
        source: "contracts/src/Rub3Factory.sol",
        role: Role::Deployer,
        release: RELEASE,
        masked_sha256: "eb6ba50265e8d6f4c97e87a4c8d022a32436c8eabea5fe08d997df7efd9ed27d",
        immutable_ranges: &[],
    },
];

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
    use sha2::{Digest, Sha256};
    let masked = mask(code, ranges)?;
    Some(hex::encode(Sha256::digest(&masked)))
}

// ── The verdict ───────────────────────────────────────────────────────────────

/// What the fetched code was, when no pinned entry claimed it.
///
/// Carries only what a human or an orchestrator needs to act: how much code was
/// there, and whether any known-forbidden name showed up in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrecognised {
    /// Length of the fetched runtime code. Zero means the address holds no
    /// contract at all.
    pub code_len: usize,
    /// Forbidden signatures found by the pre-filter. A diagnostic: an empty
    /// list is not a clean bill of health, it just means the refusal has
    /// nothing specific to name.
    pub exposed: Vec<&'static str>,
}

impl std::fmt::Display for Unrecognised {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
}

/// The result of comparing fetched runtime code against the pinned table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The masked code hash matched a pinned entry. The strongest available
    /// statement about the contract's executable code; see the module docs for
    /// what it deliberately does not cover.
    Canonical(&'static CanonicalContract),
    /// No pinned entry matched.
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
    })
}

// ── The purchase gate ─────────────────────────────────────────────────────────

/// Why a candidate contract may not be bought from.
///
/// Both variants are refusals of the address, not of the network: nothing about
/// them is retryable, and neither leaves a transaction behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The code at the address matched no pinned entry.
    Unrecognised(Unrecognised),
    /// Canonical rub3 code, but not a licence contract - the factory, or one of
    /// its deployer helpers. It sells nothing, so buying from it is a mistake
    /// about the address rather than about the code.
    NotALicence { contract: &'static str, role: Role },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unrecognised(u) => write!(f, "{u}"),
            Refusal::NotALicence { contract, role } => write!(
                f,
                "the code is canonical {contract}, which is a {} and sells no licences",
                match role {
                    Role::Licence => "licence contract",
                    Role::Factory => "deploy factory",
                    Role::Deployer => "factory-internal deployer helper",
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
/// Returns the pinned entry the contract matched. Every other outcome is an
/// error, including a chain read that failed - see the module docs on failure
/// posture, and do not add a caller on the launch path.
pub fn verify_before_purchase(
    rpc_url: &str,
    contract: Address,
) -> Result<&'static CanonicalContract, GateError> {
    let code = rpc::get_code(rpc_url, contract).map_err(GateError::Fetch)?;
    decide(classify(&code)).map_err(GateError::Refused)
}

/// The decision the gate makes once the bytes are in hand, separated from the
/// chain read so it can be exercised without a network.
fn decide(verdict: Verdict) -> Result<&'static CanonicalContract, Refusal> {
    match verdict {
        Verdict::Canonical(entry) if entry.role == Role::Licence => Ok(entry),
        Verdict::Canonical(entry) => Err(Refusal::NotALicence {
            contract: entry.contract,
            role: entry.role,
        }),
        // The miss path. An on-chain `Rub3CodeRegistry` lookup (the report's
        // Option A) belongs exactly here: consult it for a release newer than
        // this binary's table, and turn a hit into `Ok`. Deliberately not
        // implemented - it is separate work, and until it exists a miss is a
        // refusal.
        Verdict::Unrecognised(finding) => Err(Refusal::Unrecognised(finding)),
    }
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
                         \x20       role: Role::Licence,   // check this\n\
                         \x20       release: RELEASE,      // and this\n\
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
    #[test]
    fn forbidden_signatures_mirror_the_solidity_audit() {
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

    #[test]
    fn the_gate_accepts_a_licence_contract() {
        let code = fake_code(256, 0x10);
        let table = entry_for(&code, vec![], Role::Licence);
        let entry = decide(classify_against(&code, table)).expect("a licence entry is buyable");
        assert_eq!(entry.contract, "FakeLicence");
    }

    #[test]
    fn the_gate_refuses_canonical_code_that_sells_nothing() {
        let code = fake_code(256, 0x10);
        for role in [Role::Factory, Role::Deployer] {
            let table = entry_for(&code, vec![], role);
            match decide(classify_against(&code, table)) {
                Err(Refusal::NotALicence { role: refused, .. }) => assert_eq!(refused, role),
                other => panic!("{role:?} must not be a purchase target, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_gate_refuses_code_it_does_not_recognise() {
        let refusal = decide(classify(&fake_code(256, 0x10)))
            .expect_err("synthetic code is not a canonical rub3 contract");
        assert!(matches!(refusal, Refusal::Unrecognised(_)));
    }

    /// The gate is wired into the purchase path and into nothing else.
    ///
    /// This is the subtlest property in the module and the one a later change
    /// is most likely to break by accident, because adding "verify the contract
    /// before launching too" looks like an improvement. It is not: a launch is
    /// a program the user has already paid for, and a check that can refuse to
    /// start it is a revocation surface wearing an integrity check's clothes.
    /// The posture is structural - there is no shared helper and no flag - so
    /// this test guards the structure rather than a default value.
    #[test]
    fn the_gate_is_wired_into_the_purchase_path_and_nowhere_else() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut callers: Vec<(String, usize)> = std::fs::read_dir(&src)
            .expect("the crate's own src directory is readable")
            .map(|entry| entry.expect("readable directory entry").path())
            .filter(|path| path.extension().is_some_and(|e| e == "rs"))
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                if name == "attest.rs" {
                    return None;
                }
                let body = std::fs::read_to_string(&path).ok()?;
                let mentions = body.matches("verify_before_purchase").count();
                (mentions > 0).then_some((name, mentions))
            })
            .collect();
        callers.sort();

        assert_eq!(
            callers,
            vec![("activation.rs".to_string(), 1)],
            "the pre-purchase gate must be called exactly once, from the purchase path in \
             activation.rs, and from nowhere else.\n\n\
             If this failed because a launch path now calls it: that is the one thing this \
             module must not do. Refusing to start an already-paid-for licence because an \
             integrity check could not complete is a de-facto revocation surface, which this \
             project has ruled out. Fail closed on purchase, fail open on launch."
        );
    }

    /// Every pinned licence contract really is buyable, and every other role
    /// really is not - asserted against the shipped table, not a synthetic one.
    #[test]
    fn only_licence_roles_are_purchase_targets() {
        let licences = CANONICAL.iter().filter(|e| e.role == Role::Licence).count();
        assert!(
            licences >= 2,
            "the table should pin both licence templates, found {licences}"
        );
    }
}
