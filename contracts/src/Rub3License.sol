// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC721}           from "@openzeppelin/contracts/token/ERC721/ERC721.sol";
import {ERC721Enumerable} from "@openzeppelin/contracts/token/ERC721/extensions/ERC721Enumerable.sol";
import {Ownable}          from "@openzeppelin/contracts/access/Ownable.sol";

/// @notice Abstract base shared by {Rub3Access} and {Rub3Subscription}.
///
/// Holds the ERC-721 + ERC-721Enumerable wiring, the two pieces of metadata the
/// wrapper reads at activation (`identityModel`, `wrapperHash`), the sale
/// configuration (`price`, `supplyCap`), and the sequential mint helper.
abstract contract Rub3License is ERC721, ERC721Enumerable, Ownable {
    /// @notice 0 = access (user_id = wallet), 1 = account (user_id = TBA).
    uint8 public immutable identityModel;

    /// @notice SHA-256 of the distributed wrapper binary. Mutable so the
    ///         developer can rotate after rebuilds without redeploying.
    bytes32 public wrapperHash;

    /// @notice Purchase price in wei. Set by {setPrice}.
    uint256 public price;

    /// @notice Max mintable tokens. `0` disables the cap.
    uint256 public immutable supplyCap;

    /// @notice Next token id to be minted. Tokens are minted sequentially from 0.
    uint256 public nextTokenId;

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
    event WrapperHashUpdated(bytes32 oldHash, bytes32 newHash);
    event Activated(uint256 indexed tokenId, address indexed owner, uint256 sessionId);

    // ── Errors ────────────────────────────────────────────────────────────────

    error InvalidIdentityModel(uint8 value);
    error CooldownTooSmall(uint256 value, uint256 minimum);
    error SoldOut();
    error InsufficientPayment(uint256 sent, uint256 required);
    error WithdrawFailed();
    error NotTokenOwner(address caller, address owner);
    error CooldownActive(uint256 blocksRemaining);

    constructor(
        string memory name_,
        string memory symbol_,
        uint8         identityModel_,
        bytes32       wrapperHash_,
        uint256       price_,
        uint256       supplyCap_,
        uint256       cooldownBlocks_,
        address       owner_
    ) ERC721(name_, symbol_) Ownable(owner_) {
        if (identityModel_ > 1) revert InvalidIdentityModel(identityModel_);
        if (cooldownBlocks_ < MIN_COOLDOWN_BLOCKS) {
            revert CooldownTooSmall(cooldownBlocks_, MIN_COOLDOWN_BLOCKS);
        }
        identityModel  = identityModel_;
        wrapperHash    = wrapperHash_;
        price          = price_;
        supplyCap      = supplyCap_;
        cooldownBlocks = cooldownBlocks_;
    }

    // ── Owner controls ────────────────────────────────────────────────────────

    function setPrice(uint256 newPrice) external onlyOwner {
        emit PriceUpdated(price, newPrice);
        price = newPrice;
    }

    function setWrapperHash(bytes32 newHash) external onlyOwner {
        emit WrapperHashUpdated(wrapperHash, newHash);
        wrapperHash = newHash;
    }

    function withdraw(address payable to) external onlyOwner {
        (bool ok, ) = to.call{value: address(this).balance}("");
        if (!ok) revert WithdrawFailed();
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

    /// @dev Mints the next sequential id to `to`. Reverts if supply is capped.
    function _mintNext(address to) internal returns (uint256 tokenId) {
        if (supplyCap != 0 && nextTokenId >= supplyCap) revert SoldOut();
        tokenId = nextTokenId;
        unchecked { nextTokenId = tokenId + 1; }
        _safeMint(to, tokenId);
    }

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
