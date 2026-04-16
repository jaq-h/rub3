// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Rub3License} from "./Rub3License.sol";

/// @notice One-time-purchase access license. The NFT grants permanent access to
///         the wrapped application for its owner.
contract Rub3Access is Rub3License {
    event Purchased(uint256 indexed tokenId, address indexed recipient, address indexed payer);

    constructor(
        string memory name_,
        string memory symbol_,
        uint8         identityModel_,
        bytes32       wrapperHash_,
        uint256       price_,
        uint256       supplyCap_,
        address       owner_
    ) Rub3License(name_, symbol_, identityModel_, wrapperHash_, price_, supplyCap_, owner_) {}

    /// @notice Mint a fresh license token to `recipient`.
    /// @dev    Passing `address(0)` mints to `msg.sender`.
    function purchase(address recipient) external payable returns (uint256 tokenId) {
        if (msg.value < price) revert InsufficientPayment(msg.value, price);
        address to = _resolveRecipient(recipient);
        tokenId = _mintNext(to);
        emit Purchased(tokenId, to, msg.sender);
    }
}
