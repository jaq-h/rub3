// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IRub3Predecessor, Rub3License} from "./Rub3License.sol";

/// @notice One-time-purchase access license. The NFT grants permanent access to
///         the wrapped application for its owner.
///
/// Paid once, so there are no ongoing terms to freeze - `ownerOf` is the whole
/// entitlement, and nothing in this contract or its base can take it away. See
/// {Rub3License} for the ownership invariants that hold across both models.
contract Rub3Access is Rub3License {
    event Purchased(uint256 indexed tokenId, address indexed recipient, address indexed payer);

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
    ) Rub3License(
        name_, symbol_, identityModel_, tbaImplementation_, wrapperHashes_,
        price_, supplyCap_, cooldownBlocks_, predecessor_, owner_
    ) {
        // Succession is same-model only, and that is enforced here rather than
        // left to the deployer. {Rub3License} has already established that a
        // non-zero predecessor is a live contract answering the base read slice;
        // this is the mirror of the {Rub3Subscription} probe over the same
        // discriminator. A subscription predecessor answers `period()`, and an
        // access license carries no terms across in `_afterClaim`, so pointing
        // one here would let a lapsed subscriber mint a perpetual license for
        // free. Rejected at deploy, by name, rather than silently later.
        if (predecessor_ != address(0)) {
            bool answersPeriod = true;
            try IRub3Predecessor(predecessor_).period() returns (uint256) {}
            catch { answersPeriod = false; }
            if (answersPeriod) revert IncompatiblePredecessor(predecessor_);
        }
    }

    /// @notice Mint a fresh license token to `recipient`.
    /// @dev    Passing `address(0)` mints to `msg.sender`.
    function purchase(address recipient) external payable returns (uint256 tokenId) {
        if (msg.value < price) revert InsufficientPayment(msg.value, price);
        address to = _resolveRecipient(recipient);
        tokenId = _mintNext(to);
        emit Purchased(tokenId, to, msg.sender);
    }
}
