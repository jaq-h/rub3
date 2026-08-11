// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Rub3License} from "./Rub3License.sol";

/// @notice Time-bounded subscription license. Each token carries an `expiresAt`
///         timestamp; the holder extends it by paying that token's own
///         `renewPrice` once per period.
///
/// A subscription is the one billing model with terms that outlive the sale, so
/// those terms are frozen per token: `period` is immutable for the whole
/// contract and `renewPrice[tokenId]` is snapshotted at mint. {setPrice} moves
/// the price for *new* buyers only — a developer cannot reprice a subscription
/// somebody already holds, and there is no function that could.
contract Rub3Subscription is Rub3License {
    /// @notice Subscription length in seconds (e.g. 30 days). Immutable — the
    ///         other half of "renewal terms are frozen per token".
    uint256 public immutable period;

    /// @notice Expiry timestamp per token. `0` for non-existent tokens.
    mapping(uint256 => uint256) public expiresAt;

    /// @notice Renewal price in wei, snapshotted from `price` when the token was
    ///         minted. This — not the current `price` — is what {renew} charges.
    ///
    ///         Write-once at mint. There is no setter, and nothing else in the
    ///         contract writes it after the token exists.
    mapping(uint256 => uint256) public renewPrice;

    event Purchased(
        uint256 indexed tokenId,
        address indexed recipient,
        address indexed payer,
        uint256 expiresAt,
        uint256 renewPrice
    );
    event Renewed(uint256 indexed tokenId, uint256 expiresAt, uint256 pricePaid);

    constructor(
        string    memory name_,
        string    memory symbol_,
        uint8            identityModel_,
        address          tbaImplementation_,
        bytes32[] memory wrapperHashes_,
        uint256          price_,
        uint256          supplyCap_,
        uint256          period_,
        uint256          cooldownBlocks_,
        address          predecessor_,
        address          owner_
    ) Rub3License(
        name_, symbol_, identityModel_, tbaImplementation_, wrapperHashes_,
        price_, supplyCap_, cooldownBlocks_, predecessor_, owner_
    ) {
        period = period_;
    }

    /// @notice Mint a fresh subscription token to `recipient`, starting now.
    /// @dev    Passing `address(0)` mints to `msg.sender`.
    ///
    /// Freezes this token's renewal price at whatever `price` is right now.
    function purchase(address recipient) external payable returns (uint256 tokenId) {
        uint256 due = price;
        if (msg.value < due) revert InsufficientPayment(msg.value, due);
        address to = _resolveRecipient(recipient);
        tokenId = _mintNext(to);

        uint256 newExpiry = block.timestamp + period;
        expiresAt[tokenId]  = newExpiry;
        renewPrice[tokenId] = due;

        emit Purchased(tokenId, to, msg.sender, newExpiry, due);
    }

    /// @notice Extend `tokenId` by one period at that token's snapshotted price.
    ///
    /// If the token is still valid, the new period is appended to its current
    /// expiry. If it has already lapsed, the period starts from `block.timestamp`.
    /// Reverts if the token does not exist.
    ///
    /// Charges `renewPrice[tokenId]`, never the current `price` — a holder's
    /// cost to stay subscribed is fixed at the moment they bought.
    function renew(uint256 tokenId) external payable {
        _requireOwned(tokenId);

        uint256 due = renewPrice[tokenId];
        if (msg.value < due) revert InsufficientPayment(msg.value, due);

        uint256 current = expiresAt[tokenId];
        uint256 base    = current > block.timestamp ? current : block.timestamp;
        uint256 newExpiry = base + period;
        expiresAt[tokenId] = newExpiry;
        emit Renewed(tokenId, newExpiry, due);
    }

    /// @notice True iff `tokenId` exists and has not yet expired.
    ///
    /// Reads one mapping and the clock. Not the wrapper hash set, not
    /// `successor`, not `price`, and nothing the contract owner can write.
    function isValid(uint256 tokenId) external view returns (bool) {
        return expiresAt[tokenId] > block.timestamp;
    }

    /// @dev Carry the migrating holder's frozen terms across to the successor
    ///      token: they keep their remaining time and their original renewal
    ///      price. Migration must not become a repricing event.
    function _afterClaim(uint256 tokenId, uint256 predecessorTokenId) internal override {
        Rub3Subscription pred = Rub3Subscription(predecessor);
        expiresAt[tokenId]  = pred.expiresAt(predecessorTokenId);
        renewPrice[tokenId] = pred.renewPrice(predecessorTokenId);
    }
}
