// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Rub3License} from "./Rub3License.sol";

/// @notice Time-bounded subscription license. Each token carries an `expiresAt`
///         timestamp; callers extend it by paying `price` once per period.
contract Rub3Subscription is Rub3License {
    /// @notice Subscription length in seconds (e.g. 30 days).
    uint256 public immutable period;

    /// @notice Expiry timestamp per token. `0` for non-existent tokens.
    mapping(uint256 => uint256) public expiresAt;

    event Purchased(uint256 indexed tokenId, address indexed recipient, address indexed payer, uint256 expiresAt);
    event Renewed  (uint256 indexed tokenId,                                                      uint256 expiresAt);

    constructor(
        string memory name_,
        string memory symbol_,
        uint8         identityModel_,
        bytes32       wrapperHash_,
        uint256       price_,
        uint256       supplyCap_,
        uint256       period_,
        uint256       cooldownBlocks_,
        address       owner_
    ) Rub3License(
        name_, symbol_, identityModel_, wrapperHash_, price_, supplyCap_, cooldownBlocks_, owner_
    ) {
        period = period_;
    }

    /// @notice Mint a fresh subscription token to `recipient`, starting now.
    /// @dev    Passing `address(0)` mints to `msg.sender`.
    function purchase(address recipient) external payable returns (uint256 tokenId) {
        if (msg.value < price) revert InsufficientPayment(msg.value, price);
        address to = _resolveRecipient(recipient);
        tokenId = _mintNext(to);
        uint256 newExpiry = block.timestamp + period;
        expiresAt[tokenId] = newExpiry;
        emit Purchased(tokenId, to, msg.sender, newExpiry);
    }

    /// @notice Extend `tokenId` by one period.
    ///
    /// If the token is still valid, the new period is appended to its current
    /// expiry. If it has already lapsed, the period starts from `block.timestamp`.
    /// Reverts if the token does not exist.
    function renew(uint256 tokenId) external payable {
        if (msg.value < price) revert InsufficientPayment(msg.value, price);
        _requireOwned(tokenId);

        uint256 current = expiresAt[tokenId];
        uint256 base    = current > block.timestamp ? current : block.timestamp;
        uint256 newExpiry = base + period;
        expiresAt[tokenId] = newExpiry;
        emit Renewed(tokenId, newExpiry);
    }

    /// @notice True iff `tokenId` exists and has not yet expired.
    function isValid(uint256 tokenId) external view returns (bool) {
        return expiresAt[tokenId] > block.timestamp;
    }
}
