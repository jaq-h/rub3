// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC721}           from "@openzeppelin/contracts/token/ERC721/ERC721.sol";
import {ERC721Enumerable} from "@openzeppelin/contracts/token/ERC721/extensions/ERC721Enumerable.sol";
import {Ownable}          from "@openzeppelin/contracts/access/Ownable.sol";

/// @notice The slice of a predecessor license contract that a successor reads
///         during {Rub3License-claimFromPredecessor}. Deliberately tiny: a
///         successor never calls anything on its predecessor that could mutate
///         state, so migration can never disturb the old contract.
interface IRub3Predecessor {
    function ownerOf(uint256 tokenId) external view returns (address);
    function successor() external view returns (address);

    /// Subscription terms a successor carries across in `_afterClaim`. Absent on
    /// a predecessor that is not a subscription, which is what the
    /// {Rub3Subscription} constructor probe detects.
    function period() external view returns (uint256);
    function expiresAt(uint256 tokenId) external view returns (uint256);
    function renewPrice(uint256 tokenId) external view returns (uint256);
}

/// @notice Abstract base shared by {Rub3Access} and {Rub3Subscription}.
///
/// Holds the ERC-721 + ERC-721Enumerable wiring, the metadata the wrapper reads
/// at activation (`identityModel`, the wrapper hash set), the sale configuration
/// (`price`, `supplyCap`), the sequential mint helper, the tier-3 activation /
/// cooldown machinery, and the ownership invariants of implementation.md §2.4.
///
/// # Ownership invariants
///
/// **The token is the invariant; everything else is versioned.** What a holder
/// was *granted* can never be taken back; only what is *offered* to future
/// buyers may change. Enforced by construction, not by policy:
///
/// - **No revocation surface.** There is no burn, no admin transfer, no pause,
///   and no owner-callable function of any kind that can change `ownerOf`,
///   `isValid`, or the outcome of `activate` for an already-issued token. The
///   selectors are absent from the bytecode - see `test/Rub3Invariants.t.sol`.
/// - **No proxies.** Contract code, and therefore license terms, are frozen at
///   deploy. There is no upgrade hook, no delegatecall, no initializer.
/// - **Append-only wrapper hash set.** Binary hashes are added, never replaced;
///   a compromised build is flagged `Revoked` with an on-chain reason. Hash
///   status governs *binary* trust only and is never consulted by `ownerOf`,
///   `isValid`, or `activate`.
/// - **Opt-in succession.** `successor` is a pointer, not a switch. This
///   contract validates its own tokens forever regardless of what it points at,
///   and migration onto a successor is initiated by the holder alone.
abstract contract Rub3License is ERC721, ERC721Enumerable, Ownable {
    /// @notice 0 = access (user_id = wallet), 1 = account (user_id = TBA).
    uint8 public immutable identityModel;

    /// @notice ERC-6551 account implementation that token-bound accounts for
    ///         this collection resolve to. Only meaningful when
    ///         `identityModel == 1` — the wrapper derives each token's TBA
    ///         address by CREATE2 against the canonical ERC-6551 registry with
    ///         this implementation and `salt = 0`. Immutable so the developer
    ///         cannot silently reassign every user's identity.
    ///
    ///         Must be `address(0)` for access-model deploys.
    address public immutable tbaImplementation;

    /// @notice Purchase price in wei for *new* mints. Set by {setPrice}.
    ///
    ///         Changing it never touches an issued token: `Rub3Access` tokens
    ///         are paid for once, and `Rub3Subscription` snapshots each token's
    ///         renewal price at mint (`renewPrice[tokenId]`).
    uint256 public price;

    /// @notice Max mintable tokens. `0` disables the cap.
    uint256 public immutable supplyCap;

    /// @notice Next token id to be minted. Tokens are minted sequentially from 0.
    uint256 public nextTokenId;

    // ── Wrapper hash set (append-only) ────────────────────────────────────────

    /// @notice Lifecycle of a wrapper binary hash.
    ///
    ///         `Unknown` → `Valid` → `Revoked`, and only in that direction.
    ///         Status never becomes *less* severe: a revoked hash can never be
    ///         re-added, which is what makes the set auditable as append-only.
    ///         A mistaken revocation is corrected by publishing a fresh build,
    ///         not by rewriting history.
    enum HashStatus { Unknown, Valid, Revoked }

    /// @notice SHA-256 of a distributed wrapper binary → its status.
    ///
    ///         A *set*, not a slot: one release ships several binaries (one per
    ///         platform), and rotating a single slot would retroactively strip
    ///         verifiability from every binary already downloaded.
    mapping(bytes32 => HashStatus) public wrapperHashes;

    /// @notice Stated reason a hash was revoked. Empty for hashes that are not
    ///         `Revoked`. Revocation without a reason is not possible.
    mapping(bytes32 => string) public revocationReason;

    /// @dev Insertion-ordered list of every hash ever added, so an agent can
    ///      enumerate the full set without replaying logs.
    bytes32[] private _wrapperHashList;

    // ── Succession (opt-in migration) ─────────────────────────────────────────

    /// @notice The contract this one honors claims from, frozen at deploy.
    ///         `address(0)` means this contract accepts no migrations.
    ///
    ///         Immutable on purpose: "which contract's holders do I honor" is
    ///         part of what a buyer audits before paying, so it must not move
    ///         after they have looked.
    address public immutable predecessor;

    /// @notice Where this contract's holders *may* migrate to. A signpost, not
    ///         a switch - see {setSuccessor}.
    address public successor;

    /// @notice True for tokens minted by {claimFromPredecessor} rather than sold.
    mapping(uint256 => bool) public wasClaimed;

    /// @notice For a claimed token, the predecessor token id it was claimed
    ///         against. Meaningless unless `wasClaimed[tokenId]`.
    mapping(uint256 => uint256) public claimedFromTokenId;

    /// @notice Predecessor token ids that have already been migrated here.
    ///         One claim per predecessor token.
    mapping(uint256 => bool) public predecessorTokenClaimed;

    // ── Cooldown / session state (tiers 3-4) ──────────────────────────────────

    /// @notice Floor on `cooldownBlocks`. ~30s on Base — one TOTP window.
    ///         Anything smaller reduces the contract to tier 2 (no rate limit).
    uint256 public constant MIN_COOLDOWN_BLOCKS = 15;

    /// @notice Blocks that must elapse between activations for a single token.
    ///         Immutable so the owner cannot silently defeat rate limiting.
    uint256 public immutable cooldownBlocks;

    /// @notice Block number of the last `activate()` call per token. `0` means
    ///         never activated — the first call is always allowed.
    mapping(uint256 => uint256) public lastActivationBlock;

    /// @notice Current active session id per token. Incremented on every
    ///         `activate()`. Cached sessions whose `session_id` no longer
    ///         matches are considered revoked.
    mapping(uint256 => uint256) public activeSessionId;

    /// @dev Monotonic counter feeding `activeSessionId` on each activation.
    uint256 private _sessionCounter;

    // ── Events ────────────────────────────────────────────────────────────────

    event PriceUpdated(uint256 oldPrice, uint256 newPrice);
    event WrapperHashAdded(bytes32 indexed hash);
    event WrapperHashRevoked(bytes32 indexed hash, string reason);
    event SuccessorUpdated(address indexed oldSuccessor, address indexed newSuccessor);
    event Claimed(
        address indexed predecessor,
        uint256 indexed predecessorTokenId,
        uint256 indexed tokenId,
        address holder
    );
    event Activated(uint256 indexed tokenId, address indexed owner, uint256 sessionId);

    // ── Errors ────────────────────────────────────────────────────────────────

    error InvalidIdentityModel(uint8 value);
    error CooldownTooSmall(uint256 value, uint256 minimum);
    error TbaImplementationRequired();
    error TbaImplementationForbidden();
    error SoldOut();
    error InsufficientPayment(uint256 sent, uint256 required);
    error WithdrawFailed();
    error NotTokenOwner(address caller, address owner);
    error CooldownActive(uint256 blocksRemaining);
    error ZeroWrapperHash();
    error WrapperHashAlreadyKnown(bytes32 hash);
    error WrapperHashNotValid(bytes32 hash);
    error RevocationReasonRequired();
    error SelfReference();
    error NoPredecessor();
    error SuccessorNotDeclared(address declared);
    error PredecessorTokenAlreadyClaimed(uint256 predecessorTokenId);

    constructor(
        string    memory name_,
        string    memory symbol_,
        uint8            identityModel_,
        address          tbaImplementation_,
        bytes32[] memory wrapperHashes_,
        uint256          price_,
        uint256          supplyCap_,
        uint256          cooldownBlocks_,
        address          predecessor_,
        address          owner_
    ) ERC721(name_, symbol_) Ownable(owner_) {
        if (identityModel_ > 1) revert InvalidIdentityModel(identityModel_);
        if (cooldownBlocks_ < MIN_COOLDOWN_BLOCKS) {
            revert CooldownTooSmall(cooldownBlocks_, MIN_COOLDOWN_BLOCKS);
        }
        // Account model must pick a TBA implementation; access model must not.
        if (identityModel_ == 1 && tbaImplementation_ == address(0)) {
            revert TbaImplementationRequired();
        }
        if (identityModel_ == 0 && tbaImplementation_ != address(0)) {
            revert TbaImplementationForbidden();
        }
        if (predecessor_ == address(this)) revert SelfReference();

        identityModel     = identityModel_;
        tbaImplementation = tbaImplementation_;
        price             = price_;
        supplyCap         = supplyCap_;
        cooldownBlocks    = cooldownBlocks_;
        predecessor       = predecessor_;

        // The launch release seeds the set; later builds append to it.
        for (uint256 i = 0; i < wrapperHashes_.length; i++) {
            _addWrapperHash(wrapperHashes_[i]);
        }
    }

    // ── Owner controls ────────────────────────────────────────────────────────

    /// @notice Set the price of *future* mints.
    ///
    /// Affects nothing already issued. `Rub3Subscription` renewals are charged
    /// against each token's own `renewPrice` snapshot, so a price change cannot
    /// reach a subscription somebody already holds.
    function setPrice(uint256 newPrice) external onlyOwner {
        emit PriceUpdated(price, newPrice);
        price = newPrice;
    }

    /// @notice Append a wrapper binary hash to the valid set.
    ///
    /// Append-only: a hash already in the set - valid *or* revoked - is
    /// rejected. There is no removal and no un-revoke.
    function addWrapperHash(bytes32 hash) external onlyOwner {
        _addWrapperHash(hash);
    }

    /// @notice Flag a previously valid build as compromised, with a reason.
    ///
    /// This is a statement about a *binary*, and nothing else. It cannot change
    /// `ownerOf`, `isValid`, `activate`, or any other token state - none of them
    /// read {wrapperHashes}. The holder downloads a patched build and their
    /// same license keeps working.
    ///
    /// Honest limit: revocation informs new downloads and future activations.
    /// It cannot disable a compromised binary that is already running. A switch
    /// that could would be a revocation surface, and it must not exist.
    function revokeWrapperHash(bytes32 hash, string calldata reason) external onlyOwner {
        if (wrapperHashes[hash] != HashStatus.Valid) revert WrapperHashNotValid(hash);
        if (bytes(reason).length == 0) revert RevocationReasonRequired();

        wrapperHashes[hash]    = HashStatus.Revoked;
        revocationReason[hash] = reason;
        emit WrapperHashRevoked(hash, reason);
    }

    /// @notice Point holders at a contract they *may* migrate to.
    ///
    /// Setting, changing, or clearing this pointer has no effect on any token
    /// issued here: this contract keeps validating its own tokens forever, and
    /// nothing in {ownerOf}, {activate}, or a subclass's `isValid` reads it.
    /// Migration only ever happens because a holder calls
    /// {claimFromPredecessor} on the successor themselves.
    function setSuccessor(address newSuccessor) external onlyOwner {
        if (newSuccessor == address(this)) revert SelfReference();
        emit SuccessorUpdated(successor, newSuccessor);
        successor = newSuccessor;
    }

    function withdraw(address payable to) external onlyOwner {
        (bool ok, ) = to.call{value: address(this).balance}("");
        if (!ok) revert WithdrawFailed();
    }

    // ── Wrapper hash views ────────────────────────────────────────────────────

    /// @notice Whether `hash` is a currently trusted wrapper binary.
    function isWrapperHashValid(bytes32 hash) external view returns (bool) {
        return wrapperHashes[hash] == HashStatus.Valid;
    }

    /// @notice Number of hashes ever added (valid + revoked).
    function wrapperHashCount() external view returns (uint256) {
        return _wrapperHashList.length;
    }

    /// @notice Hash at `index` in insertion order.
    function wrapperHashAt(uint256 index) external view returns (bytes32) {
        return _wrapperHashList[index];
    }

    /// @notice Every hash ever added, in insertion order, for agents auditing
    ///         the contract before purchase.
    function wrapperHashList() external view returns (bytes32[] memory) {
        return _wrapperHashList;
    }

    // ── Succession ────────────────────────────────────────────────────────────

    /// @notice Migrate a predecessor token onto this contract. Holder-initiated,
    ///         and the only way a token is ever created outside a sale.
    ///
    /// Requires all three of:
    ///   1. this contract was deployed declaring `predecessor` (frozen at deploy),
    ///   2. the predecessor's owner has pointed `successor` at this contract,
    ///   3. `msg.sender` currently holds `predecessorTokenId` on the predecessor.
    ///
    /// **Snapshot-claim, not burn-to-mint.** The predecessor token is neither
    /// burned nor moved - the predecessor exposes no way to do either, which is
    /// precisely the no-revocation invariant. The holder ends up with both
    /// tokens, and the old contract keeps validating its own forever.
    ///
    /// Requirement 2 is checked here, once, and the result recorded permanently:
    /// a later `setSuccessor` on the predecessor cannot retroactively unmake a
    /// claim that already happened.
    ///
    /// Claims mint through {_mintNext} and so respect `supplyCap`. A successor
    /// that intends to honor migrations sizes its cap accordingly; a holder who
    /// cannot claim has lost nothing, because their original token never stops
    /// working.
    function claimFromPredecessor(uint256 predecessorTokenId) external returns (uint256 tokenId) {
        address pred = predecessor;
        if (pred == address(0)) revert NoPredecessor();

        // Holder-initiated: only the current holder of the predecessor token
        // may claim, and only by calling this themselves. Neither contract
        // owner can move a token on anyone's behalf.
        address holder = IRub3Predecessor(pred).ownerOf(predecessorTokenId);
        if (holder != msg.sender) revert NotTokenOwner(msg.sender, holder);

        address declared = IRub3Predecessor(pred).successor();
        if (declared != address(this)) revert SuccessorNotDeclared(declared);

        if (predecessorTokenClaimed[predecessorTokenId]) {
            revert PredecessorTokenAlreadyClaimed(predecessorTokenId);
        }
        predecessorTokenClaimed[predecessorTokenId] = true;

        tokenId                     = _reserveNextId();
        wasClaimed[tokenId]         = true;
        claimedFromTokenId[tokenId] = predecessorTokenId;

        _afterClaim(tokenId, predecessorTokenId);

        _safeMint(msg.sender, tokenId);

        emit Claimed(pred, predecessorTokenId, tokenId, msg.sender);
    }

    /// @notice The wrapper's trust rule, evaluated on-chain in one call.
    ///
    /// A wrapper pinned to `configuredContract` honors `tokenId` on *this*
    /// contract when either:
    ///   - this contract **is** the configured contract, or
    ///   - this contract is the configured contract's successor **and**
    ///     `tokenId` was claimed from it by its holder.
    ///
    /// Note the second arm requires an actual claim. A successor that was
    /// deployed without declaring a predecessor - a paid major version, say -
    /// mints no claimed tokens, so a wrapper pinned to the old contract will
    /// not accept its tokens. Both sides opt in explicitly, both at deploy.
    function honorsContract(address configuredContract, uint256 tokenId)
        external
        view
        returns (bool)
    {
        if (_ownerOf(tokenId) == address(0)) return false;
        if (configuredContract == address(this)) return true;
        if (configuredContract == address(0)) return false;
        if (configuredContract != predecessor) return false;
        return wasClaimed[tokenId];
    }

    // ── Activation (tier 3) ───────────────────────────────────────────────────

    /// @notice View helper — returns whether `tokenId` can be activated now,
    ///         and how many blocks remain if not.
    function cooldownReady(uint256 tokenId)
        external
        view
        returns (bool ready, uint256 blocksRemaining)
    {
        uint256 last = lastActivationBlock[tokenId];
        if (last == 0) return (true, 0);
        uint256 elapsed = block.number - last;
        if (elapsed >= cooldownBlocks) return (true, 0);
        return (false, cooldownBlocks - elapsed);
    }

    /// @notice Record a fresh activation for `tokenId` and bump its session id.
    ///
    /// Must be called by the token's current owner. Reverts if the previous
    /// activation was fewer than `cooldownBlocks` ago. The first activation
    /// (`lastActivationBlock == 0`) bypasses the cooldown check.
    ///
    /// Reads exactly two things: who owns the token, and when it last
    /// activated. Not the wrapper hash set, not `successor`, not `price`, and
    /// nothing the contract owner can reach. There is no pause.
    function activate(uint256 tokenId) external returns (uint256 sessionId) {
        address tokenOwner = ownerOf(tokenId);
        if (tokenOwner != msg.sender) revert NotTokenOwner(msg.sender, tokenOwner);

        uint256 last = lastActivationBlock[tokenId];
        if (last != 0) {
            uint256 elapsed = block.number - last;
            if (elapsed < cooldownBlocks) revert CooldownActive(cooldownBlocks - elapsed);
        }

        lastActivationBlock[tokenId] = block.number;
        unchecked { sessionId = ++_sessionCounter; }
        activeSessionId[tokenId] = sessionId;

        emit Activated(tokenId, msg.sender, sessionId);
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// @dev Resolves `recipient == address(0)` to `msg.sender`. Used by both
    ///      concrete contracts so callers can omit the argument.
    function _resolveRecipient(address recipient) internal view returns (address) {
        return recipient == address(0) ? msg.sender : recipient;
    }

    /// @dev Claims the next sequential id without minting it. Reverts if supply
    ///      is capped.
    ///
    ///      `_safeMint` hands control to a contract recipient through
    ///      `onERC721Received` while the token already exists, so every
    ///      per-token mapping a mint path writes is written against the reserved
    ///      id *before* {_safeMint} runs. A reentrant caller therefore sees
    ///      either no token at all or a fully initialized one, never a token
    ///      with default terms.
    function _reserveNextId() internal returns (uint256 tokenId) {
        if (supplyCap != 0 && nextTokenId >= supplyCap) revert SoldOut();
        tokenId = nextTokenId;
        unchecked { nextTokenId = tokenId + 1; }
    }

    /// @dev Reserves and mints the next sequential id to `to`. Only for mint
    ///      paths that write no per-token state; anything that does must use
    ///      {_reserveNextId} and call `_safeMint` last.
    function _mintNext(address to) internal returns (uint256 tokenId) {
        tokenId = _reserveNextId();
        _safeMint(to, tokenId);
    }

    /// @dev Appends `hash` to the set. Rejects the zero hash (it is the
    ///      `Unknown` sentinel) and any hash already recorded.
    function _addWrapperHash(bytes32 hash) private {
        if (hash == bytes32(0)) revert ZeroWrapperHash();
        if (wrapperHashes[hash] != HashStatus.Unknown) revert WrapperHashAlreadyKnown(hash);

        wrapperHashes[hash] = HashStatus.Valid;
        _wrapperHashList.push(hash);
        emit WrapperHashAdded(hash);
    }

    /// @dev Hook for subclasses to carry a migrating holder's terms across from
    ///      the predecessor token. Runs inside {claimFromPredecessor} against the
    ///      reserved id, before the token is minted. Default: nothing to carry.
    function _afterClaim(uint256 tokenId, uint256 predecessorTokenId) internal virtual {}

    // ── Required overrides (ERC721 + ERC721Enumerable) ────────────────────────

    function _update(address to, uint256 tokenId, address auth)
        internal
        override(ERC721, ERC721Enumerable)
        returns (address)
    {
        return super._update(to, tokenId, auth);
    }

    function _increaseBalance(address account, uint128 value)
        internal
        override(ERC721, ERC721Enumerable)
    {
        super._increaseBalance(account, value);
    }

    function supportsInterface(bytes4 interfaceId)
        public
        view
        override(ERC721, ERC721Enumerable)
        returns (bool)
    {
        return super.supportsInterface(interfaceId);
    }
}
