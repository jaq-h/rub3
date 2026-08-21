// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";

/// @notice The version authority for rub3 contract code: which masked code
///         hashes are genuine rub3 releases, and which byte ranges a comparator
///         must zero to compute one (implementation.md §2.9).
///
/// # The problem it exists to solve
///
/// A buyer's agent verifies a contract before spending by fetching its runtime
/// code, zeroing the byte ranges solc reserved for immutables, hashing, and
/// comparing against a table compiled into its own binary. That table answers
/// "is this the code I was built against" perfectly and "is this a genuine rub3
/// release" not at all: a contract deployed from a template release *newer*
/// than the binary is a table miss, and so is a modified copy. Both are refused,
/// which is safe and wrong - the agent-first roadmap has several legitimate
/// releases live at once by design.
///
/// This contract is what tells the two apart. It maps a masked code hash to the
/// release that produced it, so an agent whose table misses can ask the chain
/// rather than refuse. Nothing else about the pipeline changes: the pinned table
/// stays the fast path, and a registry that cannot be reached leaves the agent
/// exactly where it was.
///
/// # The masked code hash is the identity
///
/// A record is keyed by `sha256` of the contract's runtime code with every
/// immutable slot zeroed - the same number `contracts/canonical-bytecode.json`
/// publishes as `deployed_bytecode_sha256`, and the same one solc emits directly
/// as `deployedBytecode.object`. The hash *is* the version. `version` here is a
/// label for an audit log to quote and for a human to read; nothing may branch
/// on it, and it is never evidence of anything.
///
/// # Append-only, and what that bounds
///
/// The owner can add records and can mark one deprecated. There is no removal,
/// no overwrite, no way back from {Status.Deprecated} to {Status.Active}, and no
/// proxy: the code here is frozen at deploy like every other contract in this
/// project. So a compromise of the owner key is bounded to *additions* - each of
/// which is a permanent public {Published} event a watcher can alarm on - and
/// can never take away a record an agent already relies on.
///
/// **{Status.Deprecated} means "not recommended for new purchases". It never
/// means "stop honouring".** An agent that meets a deprecated hash warns and
/// proceeds; a held token is untouched and its validation is untouched. A
/// deprecation that could strand a paid licence
/// would be a revocation surface by the back door, which this project rules out
/// for the whole of its contract surface (architecture.md -> "Ownership
/// invariants"). It is unreachable here because nothing reads this contract on
/// the launch path, and because the wrapper treats a deprecated record as a
/// purchase that proceeds with a warning.
///
/// The one-way status is deliberate and it does have a cost: a deprecation made
/// by mistake cannot be undone, because an un-deprecate is a second mutation on
/// a record that is supposed to be written once. The cost is a warning that
/// keeps appearing beside purchases that still complete, which is a small price
/// for a record with exactly one writable transition.
///
/// # It vouches for nothing about a deployment
///
/// A record says "this code is a genuine rub3 release". It says nothing about
/// which address runs it, who deployed it, what the immutables behind the mask
/// were set to, or how that contract's owner will behave. Anyone may deploy the
/// open-source templates, and their code is canonical by construction; whether a
/// *deployment* is canonical is what `Rub3Factory.isDeployed` answers, on a
/// specific factory address, and the two questions must not be confused.
///
/// # Why an agent may believe it
///
/// Because it verifies this contract's own code first. A wrapper carries one
/// registry address and this contract's own masked code hash, both frozen into
/// the binary at pack time, and refuses to believe an answer from an address
/// whose code does not hash to it. That is what terminates the trust recursion:
/// one address and one hash the user chose when they chose to run the binary,
/// and a deployer trusted for nothing.
contract Rub3CodeRegistry is Ownable2Step {
    /// @notice What a record says about a release. `Unknown` is the zero value,
    ///         so an unpublished hash reads as unknown rather than as anything
    ///         an agent might act on.
    ///
    ///         There is no revoked or invalid state, and there will not be one.
    ///         See the contract docs: a status that could invalidate a hash
    ///         would invalidate the licences deployed at it.
    enum Status {
        Unknown,
        Active,
        Deprecated
    }

    /// @notice What a contract carrying this code is *for*, which decides what
    ///         may be done with it. Canonical rub3 code is not the same thing as
    ///         a contract that sells licences: a buyer pointed at the factory,
    ///         at its deployer helper, or at either registry has the wrong
    ///         address, not the wrong code.
    ///
    ///         The order is part of the ABI - the wrapper mirrors these values -
    ///         so new roles are appended and existing ones never renumbered.
    enum Role {
        Licence,
        Factory,
        Deployer,
        /// This contract: the version authority, keyed by masked code hash.
        CodeRegistry,
        /// {Rub3Registry}: the discovery registry, keyed by licence contract
        /// address. A separate role from {Role.CodeRegistry} because they are
        /// separate contracts answering separate questions, and a buyer that
        /// cannot tell them apart is exactly the confusion this enum exists to
        /// remove.
        DiscoveryRegistry
    }

    /// @notice One byte range of the runtime code that solc reserved for an
    ///         immutable, and that a comparator must zero before hashing.
    ///
    ///         Always 32 bytes: a Solidity immutable occupies one EVM word, and
    ///         the slot is the immediate operand of a `PUSH32`. {publish}
    ///         enforces the width, which is what stops a published table from
    ///         describing a blind spot wider than the thing it is masking.
    struct ByteRange {
        uint32 start;
        uint32 length;
    }

    /// @notice A published release, as {record} returns it.
    struct Release {
        /// Whether this release is recommended for new purchases. Never a
        /// statement about tokens already held - see the contract docs.
        Status status;
        /// What a contract carrying this code is for.
        Role role;
        /// Solidity contract name, as `canonical-bytecode.json` keys it.
        string contractName;
        /// Human-readable release label. Explanation only, never security.
        string version;
        /// The commit of this repository the release was built from, so a third
        /// party can reproduce the hash rather than take it on faith.
        bytes32 sourceCommit;
        /// The full solc version string the release was compiled with, commit
        /// suffix included, because the compiler build is part of the build
        /// identity.
        string solcVersion;
        /// The block this record was published in. Recorded by this contract,
        /// never supplied by the caller.
        uint64 registeredAtBlock;
        /// The ranges a comparator zeroes before hashing.
        ByteRange[] offsets;
    }

    /// @notice EIP-170's cap on deployed runtime code, and therefore the bound
    ///         on where an immutable range can be. Published rather than
    ///         invented: it is what makes {publish}'s range check a statement
    ///         about real bytecode rather than an arbitrary limit, and combined
    ///         with the 32-byte width and the disjointness rule it bounds a
    ///         table to 768 ranges without naming a number of its own.
    uint32 public constant MAX_RUNTIME_CODE_SIZE = 24576;

    /// @dev Records by masked code hash. Written once by {publish}; only
    ///      `status` ever changes afterwards, and only Active -> Deprecated.
    mapping(bytes32 => Release) private _records;

    /// @dev Every published hash, in publication order, so a watcher or an agent
    ///      can enumerate the set without replaying logs - the same reason
    ///      {Rub3Factory} keeps its `deployments` list.
    bytes32[] private _published;

    /// @dev The distinct offset tables in use, in first-use order. An agent
    ///      needs a table to compute the hash it is about to look up, so the set
    ///      of candidates has to be fetchable in one call before any lookup -
    ///      this is the answer to that bootstrap. Today's canonical set spans
    ///      four tables: one each for {Rub3Access}, {Rub3Factory} and
    ///      {Rub3Registry}, plus the empty one {Rub3AccessDeployer} and this
    ///      registry share.
    ByteRange[][] private _offsetTables;

    /// @dev `keccak256(abi.encode(table)) => index + 1`, so `0` means absent.
    ///      Interning is what keeps {offsetTables} the set of *distinct* tables
    ///      rather than one entry per release.
    mapping(bytes32 => uint256) private _offsetTableIndex;

    /// @notice A release was published. Permanent, and the event a watcher
    ///         alarms on: every addition an owner key can ever make appears here
    ///         exactly once and can never be taken back.
    event Published(
        bytes32 indexed maskedCodeHash,
        Role indexed role,
        uint256 indexed offsetTable,
        string contractName,
        string version,
        bytes32 sourceCommit,
        string solcVersion
    );

    /// @notice A release stopped being recommended for new purchases. It did not
    ///         stop being honoured, and nothing about a held token changed.
    event Deprecated(bytes32 indexed maskedCodeHash, string reason);

    /// @notice A previously unseen offset table was interned at `index`.
    event OffsetTableAdded(uint256 indexed index, ByteRange[] ranges);

    /// @notice The masked code hash may not be zero: it is the record's
    ///         identity, and the zero hash is how an absent record reads.
    error MaskedCodeHashRequired();

    /// @notice `maskedCodeHash` already has a record. Records are written once;
    ///         a corrected release is a new hash, not a rewritten row.
    error AlreadyPublished(bytes32 maskedCodeHash);

    /// @notice `maskedCodeHash` has no record, so there is nothing to deprecate.
    error NotPublished(bytes32 maskedCodeHash);

    /// @notice `maskedCodeHash` is already deprecated. Status moves once, in one
    ///         direction.
    error AlreadyDeprecated(bytes32 maskedCodeHash);

    /// @notice The offsets a release publishes do not describe Solidity
    ///         immutables, so no comparator could use them honestly.
    error OffsetsMalformed(string reason);

    /// @notice A text field a refusal or an audit log quotes was left empty, and
    ///         a record is permanent, so a blank one is a permanent gap.
    error TextRequired(string field);

    /// @notice The commit a release was built from is what makes the hash
    ///         reproducible by somebody who is not the publisher.
    error SourceCommitRequired();

    /// @notice Ownership here is the right to *add*, and giving it up would
    ///         freeze the answer to "is this release newer than my binary" for
    ///         every release after this one, permanently and with no recovery.
    ///         Handing it to a new key is supported ({Ownable2Step}); handing it
    ///         to nobody is not.
    error OwnershipCannotBeRenounced();

    /// @param owner_ The key allowed to {publish} and {deprecate}. Who that is,
    ///               and how it is custodied, is a deployment decision and is
    ///               deliberately not made anywhere in this repository.
    constructor(address owner_) Ownable(owner_) {}

    // ── Reads ────────────────────────────────────────────────────────────────

    /// @notice The release published for `maskedCodeHash`, or a record whose
    ///         `status` is {Status.Unknown} when there is none.
    ///
    ///         An unknown hash is not an accusation. It means this registry has
    ///         no record of that code, which is what a modified copy looks like
    ///         and also what a release published somewhere else would look like.
    function record(bytes32 maskedCodeHash) external view returns (Release memory) {
        return _records[maskedCodeHash];
    }

    /// @notice Every distinct offset table any published release uses.
    ///
    ///         The bootstrap: computing a masked code hash needs a table, and
    ///         finding the record needs the hash. A reader fetches the candidate
    ///         tables in one call, computes the hash under each candidate, and
    ///         looks up each result. Today there is exactly one table.
    ///
    ///         This returns the whole set, which is what a watcher or an indexer
    ///         wants and what no caller should use on a path with a deadline: the
    ///         set only grows, and the owner key decides how fast. A verifier
    ///         with money on the line reads {latestOffsetTables} instead.
    function offsetTables() external view returns (ByteRange[][] memory) {
        return _offsetTables;
    }

    /// @notice At most `count` offset tables, starting at `start`, in the same
    ///         first-use order {offsetTables} returns them in.
    ///
    ///         The bounded form of the bootstrap, and the one a purchase path
    ///         uses. A verifier only ever tries a fixed number of candidates, so
    ///         it asks for that many and the response it pays to transfer and
    ///         decode is bounded by its own limit rather than by how many tables
    ///         the owner key has published. This is a latency bound: an
    ///         unbounded read could only ever be slow, never wrong.
    ///
    ///         Clamped rather than strict, so a reader needs no second call to
    ///         find out how many exist: a `start` past the end returns nothing
    ///         and a `count` past the end returns what is left. {offsetTableCount}
    ///         is there for a reader that wants the total anyway.
    ///
    ///         This walks the set from an arbitrary point in first-use order,
    ///         which is what an indexer backfilling wants. A verifier on a
    ///         purchase path wants the newest layouts instead and reads
    ///         {latestOffsetTables}.
    function offsetTableWindow(uint256 start, uint256 count)
        external
        view
        returns (ByteRange[][] memory window)
    {
        uint256 total = _offsetTables.length;
        if (start >= total) return new ByteRange[][](0);

        uint256 available = total - start;
        uint256 taken = count < available ? count : available;
        window = new ByteRange[][](taken);
        for (uint256 i = 0; i < taken; i++) {
            window[i] = _offsetTables[start + i];
        }
    }

    /// @notice At most `count` offset tables, **newest first**: the most
    ///         recently interned layout is element zero.
    ///
    ///         The read a purchase path makes, and the end of the set it has to
    ///         be. A verifier only consults this registry when its own pinned
    ///         table missed, and a pinned-table miss is by definition about code
    ///         *newer* than the binary asking. A verifier that can afford `n`
    ///         candidates and spends them on the oldest `n` therefore protects
    ///         the wrong end: once an `n+1`th layout is interned, every release
    ///         published under it becomes unreadable to every fielded binary
    ///         carrying that bound, while the very first releases stay readable
    ///         forever. That would blind fielded binaries to exactly the new
    ///         releases the registry exists to vouch for.
    ///
    ///         Newest first for the same reason: the likeliest answer is tried
    ///         with the fewest lookups, and a bound applied to this answer keeps
    ///         the newest layouts rather than dropping them.
    ///
    ///         Reachability and latency, never correctness: a verifier reading
    ///         the wrong end, or the whole set, could only ever miss a record or
    ///         be slow, and refusing a release nobody could look up is what a
    ///         verifier without any registry at all already does.
    ///
    ///         Clamped, so one call is enough: a `count` past the end returns
    ///         every table there is, and no reader needs {offsetTableCount}
    ///         first to avoid a revert.
    function latestOffsetTables(uint256 count) external view returns (ByteRange[][] memory window) {
        uint256 total = _offsetTables.length;
        uint256 taken = count < total ? count : total;
        window = new ByteRange[][](taken);
        for (uint256 i = 0; i < taken; i++) {
            window[i] = _offsetTables[total - 1 - i];
        }
    }

    /// @notice How many distinct offset tables {offsetTables} would return.
    function offsetTableCount() external view returns (uint256) {
        return _offsetTables.length;
    }

    /// @notice Every published masked code hash, in publication order.
    function published() external view returns (bytes32[] memory) {
        return _published;
    }

    /// @notice How many releases have been published. Only ever grows.
    function publishedCount() external view returns (uint256) {
        return _published.length;
    }

    // ── Writes: add, and mark not-recommended. Nothing else. ─────────────────

    /// @notice Records `maskedCodeHash` as a genuine rub3 release.
    ///
    ///         Write-once. Publishing a hash that already has a record reverts
    ///         rather than replacing it, so no sequence of owner calls can
    ///         change what this registry said about code an agent has already
    ///         acted on. A release published in error is superseded by
    ///         {deprecate}, never by a correction in place.
    ///
    /// @param maskedCodeHash `sha256` of the runtime code with every range in
    ///                       `offsets` zeroed.
    /// @param role           What a contract carrying this code is for.
    /// @param contractName   Solidity contract name.
    /// @param version        Human-readable label. Explanation, not security.
    /// @param sourceCommit   Repository commit the release was built from.
    /// @param solcVersion    Full compiler version string, commit suffix included.
    /// @param offsets        Ranges to zero, sorted ascending and disjoint. Empty
    ///                       for a contract with no immutables.
    function publish(
        bytes32 maskedCodeHash,
        Role role,
        string calldata contractName,
        string calldata version,
        bytes32 sourceCommit,
        string calldata solcVersion,
        ByteRange[] calldata offsets
    ) external onlyOwner {
        if (maskedCodeHash == bytes32(0)) {
            revert MaskedCodeHashRequired();
        }
        if (_records[maskedCodeHash].status != Status.Unknown) {
            revert AlreadyPublished(maskedCodeHash);
        }
        if (bytes(contractName).length == 0) revert TextRequired("contractName");
        if (bytes(version).length == 0) revert TextRequired("version");
        if (bytes(solcVersion).length == 0) revert TextRequired("solcVersion");
        if (sourceCommit == bytes32(0)) revert SourceCommitRequired();

        _validateOffsets(offsets);
        uint256 table = _internOffsets(offsets);

        Release storage release = _records[maskedCodeHash];
        release.status = Status.Active;
        release.role = role;
        release.contractName = contractName;
        release.version = version;
        release.sourceCommit = sourceCommit;
        release.solcVersion = solcVersion;
        release.registeredAtBlock = uint64(block.number);
        // Copied into the record rather than referenced by table index:
        // {record} is the one call an agent makes, and it must answer with the
        // ranges themselves so a comparator never has to correlate two reads.
        for (uint256 i = 0; i < offsets.length; i++) {
            release.offsets.push(offsets[i]);
        }

        _published.push(maskedCodeHash);

        emit Published(
            maskedCodeHash,
            role,
            table,
            contractName,
            version,
            sourceCommit,
            solcVersion
        );
    }

    /// @notice Marks `maskedCodeHash` as no longer recommended for new
    ///         purchases.
    ///
    ///         **This does not invalidate anything.** The record stays, the
    ///         offsets stay, and an agent that meets this code still recognises
    ///         it as genuine rub3 code - it is told to prefer a newer release,
    ///         not to refuse this one. Tokens already held and their validation
    ///         are both untouched, because nothing on any path that serves a
    ///         held licence reads this contract at all.
    ///
    ///         One-way: there is no undo, and a hash cannot be deprecated twice.
    ///
    /// @param reason Why, for the log. Mirrors
    ///               {Rub3License-revokeWrapperHash}: a permanent public act
    ///               carries its explanation with it.
    function deprecate(bytes32 maskedCodeHash, string calldata reason) external onlyOwner {
        Status status = _records[maskedCodeHash].status;
        if (status == Status.Unknown) revert NotPublished(maskedCodeHash);
        if (status == Status.Deprecated) revert AlreadyDeprecated(maskedCodeHash);
        if (bytes(reason).length == 0) revert TextRequired("reason");

        _records[maskedCodeHash].status = Status.Deprecated;
        emit Deprecated(maskedCodeHash, reason);
    }

    /// @inheritdoc Ownable
    /// @dev Always reverts. See {OwnershipCannotBeRenounced}.
    function renounceOwnership() public view override onlyOwner {
        revert OwnershipCannotBeRenounced();
    }

    // ── Internals ────────────────────────────────────────────────────────────

    /// @dev Ranges are 32 bytes wide, strictly ascending, disjoint, and inside
    ///      the largest runtime code the EVM will accept.
    ///
    ///      Every one of those is a property of a real Solidity immutable, and
    ///      each rules out a table that would mask more of a contract than its
    ///      immutables occupy. The blind spot a comparator accepts is bounded
    ///      here, at the point where the table is written down for good.
    ///
    ///      What this cannot check is that the ranges describe *this* code's
    ///      immutables: the code is not on chain to look at, and the same table
    ///      is shared by several contracts. That half is the agent's, which
    ///      confirms each range is the immediate of a `PUSH32` in the code it
    ///      fetched before it masks anything.
    function _validateOffsets(ByteRange[] calldata offsets) private pure {
        uint256 previousEnd = 0;
        for (uint256 i = 0; i < offsets.length; i++) {
            ByteRange calldata range = offsets[i];
            if (range.length != 32) revert OffsetsMalformed("a range is not one 32-byte word");
            if (range.start < previousEnd) {
                revert OffsetsMalformed("ranges must be sorted and disjoint");
            }
            uint256 end = uint256(range.start) + uint256(range.length);
            if (end > MAX_RUNTIME_CODE_SIZE) {
                revert OffsetsMalformed("a range falls outside the largest possible runtime code");
            }
            previousEnd = end;
        }
    }

    /// @dev The index of `offsets` in {offsetTables}, appending it first if this
    ///      is the first release to use it.
    function _internOffsets(ByteRange[] calldata offsets) private returns (uint256) {
        bytes32 key = keccak256(abi.encode(offsets));
        uint256 existing = _offsetTableIndex[key];
        if (existing != 0) return existing - 1;

        uint256 index = _offsetTables.length;
        _offsetTables.push();
        for (uint256 i = 0; i < offsets.length; i++) {
            _offsetTables[index].push(offsets[i]);
        }
        _offsetTableIndex[key] = index + 1;

        emit OffsetTableAdded(index, offsets);
        return index;
    }
}
