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

    // ── Events ────────────────────────────────────────────────────────────────

    event PriceUpdated(uint256 oldPrice, uint256 newPrice);
    event WrapperHashUpdated(bytes32 oldHash, bytes32 newHash);

    // ── Errors ────────────────────────────────────────────────────────────────

    error InvalidIdentityModel(uint8 value);
    error SoldOut();
    error InsufficientPayment(uint256 sent, uint256 required);
    error WithdrawFailed();

    constructor(
        string memory name_,
        string memory symbol_,
        uint8         identityModel_,
        bytes32       wrapperHash_,
        uint256       price_,
        uint256       supplyCap_,
        address       owner_
    ) ERC721(name_, symbol_) Ownable(owner_) {
        if (identityModel_ > 1) revert InvalidIdentityModel(identityModel_);
        identityModel = identityModel_;
        wrapperHash   = wrapperHash_;
        price         = price_;
        supplyCap     = supplyCap_;
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
