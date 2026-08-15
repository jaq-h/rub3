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
        string        memory name_,
        string        memory symbol_,
        IdentityTerms memory identity_,
        bytes32[]     memory wrapperHashes_,
        SaleTerms     memory sale_,
        FeeTerms      memory fee_,
        uint256              supplyCap_,
        uint256              cooldownBlocks_,
        address              predecessor_,
        address              owner_
    ) Rub3License(
        name_, symbol_, identity_, wrapperHashes_,
        sale_, fee_, supplyCap_, cooldownBlocks_, predecessor_, owner_
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

    /// @notice Mint a fresh license token to `recipient`, paying in ETH.
    /// @dev    Passing `address(0)` mints to `msg.sender`.
    function purchase(address recipient) external payable returns (uint256 tokenId) {
        _payEth(price);
        return _mintPurchased(_resolveRecipient(recipient), msg.sender);
    }

    /// @notice Mint a fresh license token to `recipient`, paying `priceAmount`
    ///         of `priceToken` with an EIP-3009 authorization the buyer signed
    ///         off-chain.
    ///
    /// **Anyone may call this** - the developer, a facilitator, or the buyer.
    /// That is what makes the purchase gasless for the buyer, and it is safe
    /// because the authorization pins down everything that matters: the token
    /// pins `from`, `value`, and the validity window; `to` is this contract, so
    /// the funds can only be spent here (see
    /// {Rub3License-_payWithAuthorization}); and the nonce is derived from the
    /// recipient, so the submitter cannot redirect the mint
    /// (see {Rub3License-purchaseAuthorizationNonce}).
    ///
    /// @dev Passing `address(0)` as `recipient` mints to `auth.from`, the buyer
    ///      - *not* to `msg.sender`, who is merely carrying the message.
    function purchaseWithAuthorization(address recipient, PaymentAuthorization calldata auth)
        external
        nonReentrant
        returns (uint256 tokenId)
    {
        address to = _resolveAuthorizedRecipient(recipient, auth.from);
        _payWithAuthorization(
            auth,
            priceToken,
            priceAmount,
            purchaseAuthorizationNonce(to, auth.salt)
        );
        return _mintPurchased(to, auth.from);
    }

    /// @dev The one mint, reached by both rails. Whatever paid for it, the token
    ///      that comes out is the same token and announces itself the same way.
    ///      `payer` is whoever's money it was: `msg.sender` on the ETH rail,
    ///      `auth.from` on the authorization rail, where `msg.sender` may be a
    ///      facilitator who paid nothing but gas.
    function _mintPurchased(address to, address payer) private returns (uint256 tokenId) {
        tokenId = _mintNext(to);
        emit Purchased(tokenId, to, payer);
    }
}
