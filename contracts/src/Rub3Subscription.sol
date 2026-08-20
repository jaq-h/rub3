// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IRub3Predecessor, Rub3License} from "./Rub3License.sol";

/// @notice Time-bounded subscription license. Each token carries an `expiresAt`
///         timestamp; the holder extends it by paying that token's own
///         `renewPrice` once per period.
///
/// A subscription is the one billing model with terms that outlive the sale, so
/// those terms are frozen per token: `period` is immutable for the whole
/// contract and `renewPrice[tokenId]` is snapshotted at mint. {setPrice} moves
/// the price for *new* buyers only - a developer cannot reprice a subscription
/// somebody already holds, and there is no function that could.
contract Rub3Subscription is Rub3License {
    /// @notice Subscription length in seconds (e.g. 30 days). Immutable - the
    ///         other half of "renewal terms are frozen per token".
    uint256 public immutable period;

    /// @notice Expiry timestamp per token. `0` for non-existent tokens.
    mapping(uint256 => uint256) public expiresAt;

    /// @notice Renewal price in wei, snapshotted from `price` when the token was
    ///         minted. This - not the current `price` - is what {renew} charges.
    ///
    ///         Write-once at mint. There is no setter, and nothing else in the
    ///         contract writes it after the token exists.
    mapping(uint256 => uint256) public renewPrice;

    /// @notice The ERC-20 this token renews in, snapshotted from `priceToken` at
    ///         mint. `address(0)` means the token was minted while the contract
    ///         offered no stablecoin rail, and so renews in ETH only.
    ///
    ///         Write-once at mint, like {renewPrice}, and for the same reason:
    ///         it is a renewal *term*, and renewal terms are frozen per token.
    mapping(uint256 => address) public renewPriceToken;

    /// @notice What this token costs to renew on the stablecoin rail, in
    ///         `renewPriceToken`'s smallest unit, snapshotted from `priceAmount`
    ///         at mint.
    ///
    ///         **This is a second snapshot, not a conversion of {renewPrice}.**
    ///         The contract has no oracle and never derives one rail's price
    ///         from the other's; the developer quotes both, and a mint freezes
    ///         whichever were listed at that instant. The two are therefore
    ///         independent, and a token minted when `priceAmount` was `0` and
    ///         `priceToken` unset simply has no stablecoin renewal - it renews
    ///         in ETH at {renewPrice}, which every token always can.
    mapping(uint256 => uint256) public renewPriceAmount;

    event Purchased(
        uint256 indexed tokenId,
        address indexed recipient,
        address indexed payer,
        uint256 expiresAt,
        uint256 renewPrice,
        address renewPriceToken,
        uint256 renewPriceAmount
    );

    /// @notice A renewal. `priceToken` is `address(0)` when paid in ETH, in
    ///         which case `pricePaid` is wei; otherwise `pricePaid` is in
    ///         `priceToken`'s smallest unit.
    event Renewed(
        uint256 indexed tokenId,
        uint256 expiresAt,
        address priceToken,
        uint256 pricePaid
    );

    constructor(
        string memory name_,
        string memory symbol_,
        IdentityTerms memory identity_,
        bytes32[] memory wrapperHashes_,
        SaleTerms memory sale_,
        FeeTerms memory fee_,
        uint256 supplyCap_,
        uint256 period_,
        uint256 cooldownBlocks_,
        address predecessor_,
        address owner_
    )
        Rub3License(
            name_,
            symbol_,
            identity_,
            wrapperHashes_,
            sale_,
            fee_,
            supplyCap_,
            cooldownBlocks_,
            predecessor_,
            owner_
        )
    {
        period = period_;

        // {Rub3License} has already established that a non-zero predecessor is a
        // live contract answering the base read slice. A subscription carries
        // more across in {_afterClaim}, so it additionally requires the whole
        // subscription slice: `period()` discriminates a subscription from an
        // access license, and `expiresAt` / `renewPrice` are what {_afterClaim}
        // itself reads. A predecessor missing any of them would brick every
        // holder's claim forever, so it is rejected here instead.
        //
        // Token id 0 need not exist: both are mapping getters and answer `0`
        // for an unset key rather than reverting the way `ownerOf` would.
        if (predecessor_ != address(0)) {
            try IRub3Predecessor(predecessor_).period() returns (uint256) {}
            catch {
                revert IncompatiblePredecessor(predecessor_);
            }

            try IRub3Predecessor(predecessor_).expiresAt(0) returns (uint256) {}
            catch {
                revert IncompatiblePredecessor(predecessor_);
            }

            try IRub3Predecessor(predecessor_).renewPrice(0) returns (uint256) {}
            catch {
                revert IncompatiblePredecessor(predecessor_);
            }
        }
    }

    /// @notice Mint a fresh subscription token to `recipient`, starting now,
    ///         paying in ETH.
    ///
    /// `msg.value` must equal {price} exactly; see {Rub3License-_payEth}.
    ///
    /// Freezes this token's renewal terms - both rails - at whatever is listed
    /// right now.
    ///
    /// @dev    Passing `address(0)` mints to `msg.sender`.
    function purchase(address recipient) external payable returns (uint256 tokenId) {
        _payEth(price);
        return _mintSubscription(_resolveRecipient(recipient), msg.sender);
    }

    /// @notice Mint a fresh subscription token, paying `priceAmount` of
    ///         `priceToken` with an EIP-3009 authorization the buyer signed
    ///         off-chain. Anyone may submit it; see
    ///         {Rub3Access-purchaseWithAuthorization} for why that is safe.
    ///
    /// @dev Passing `address(0)` as `recipient` mints to `auth.from`, the buyer.
    function purchaseWithAuthorization(address recipient, PaymentAuthorization calldata auth)
        external
        nonReentrant
        returns (uint256 tokenId)
    {
        address to = _resolveAuthorizedRecipient(recipient, auth.from);
        _payWithAuthorization(
            auth, priceToken, priceAmount, purchaseAuthorizationNonce(to, auth.salt)
        );
        return _mintSubscription(to, auth.from);
    }

    /// @dev The one mint, reached by both rails, and the only place a
    ///      subscription token's terms are ever written.
    ///
    ///      Snapshots the *listed* prices, which on the ETH rail is also the
    ///      only amount that can have been paid: {Rub3License-_payEth} takes
    ///      the exact price, so what a buyer sent cannot inflate what they
    ///      renew at. Every per-token mapping is written against the reserved
    ///      id before {_safeMint} hands control to a contract recipient, so
    ///      `onERC721Received` can never observe a token whose terms are still
    ///      at their defaults (§2.4).
    function _mintSubscription(address to, address payer) private returns (uint256 tokenId) {
        tokenId = _reserveNextId();

        uint256 newExpiry = block.timestamp + period;
        uint256 dueEth = price;
        address dueToken = priceToken;
        uint256 dueAmount = priceAmount;

        expiresAt[tokenId] = newExpiry;
        renewPrice[tokenId] = dueEth;
        renewPriceToken[tokenId] = dueToken;
        renewPriceAmount[tokenId] = dueAmount;

        _safeMint(to, tokenId);

        emit Purchased(tokenId, to, payer, newExpiry, dueEth, dueToken, dueAmount);
    }

    /// @notice Extend `tokenId` by one period at that token's snapshotted price.
    ///
    /// If the token is still valid, the new period is appended to its current
    /// expiry. If it has already lapsed, the period starts from `block.timestamp`.
    /// Reverts if the token does not exist.
    ///
    /// Charges `renewPrice[tokenId]`, never the current `price` - a holder's
    /// cost to stay subscribed is fixed at the moment they bought. `msg.value`
    /// must equal that snapshot exactly; see {Rub3License-_payEth}.
    function renew(uint256 tokenId) external payable {
        _requireOwned(tokenId);
        _payEth(renewPrice[tokenId]);
        _extend(tokenId, address(0), renewPrice[tokenId]);
    }

    /// @notice Extend `tokenId` by one period, paying its snapshotted
    ///         `renewPriceAmount` of `renewPriceToken` with an EIP-3009
    ///         authorization. Anyone may submit it, so a subscription stays
    ///         renewable by an agent that holds no ETH at all.
    ///
    /// Charges the token's own snapshot, never the current `priceAmount` - the
    /// stablecoin rail is frozen per token exactly like the ETH one, so a
    /// developer cannot reprice a held subscription on either. A token minted
    /// before this contract offered a stablecoin rail has no snapshot to charge
    /// and reverts with `TokenPaymentUnavailable`; it renews in ETH, which it
    /// always can.
    ///
    /// The nonce is derived from `tokenId`, so a submitter cannot redirect a
    /// renewal onto some other holder's token, and the renewal tag makes a
    /// purchase authorization unusable here.
    function renewWithAuthorization(uint256 tokenId, PaymentAuthorization calldata auth)
        external
        nonReentrant
    {
        _requireOwned(tokenId);
        address token = renewPriceToken[tokenId];
        uint256 amount = renewPriceAmount[tokenId];
        _payWithAuthorization(auth, token, amount, renewAuthorizationNonce(tokenId, auth.salt));
        _extend(tokenId, token, amount);
    }

    /// @notice The EIP-3009 nonce a renewal authorization must carry.
    ///
    /// The renewal counterpart of
    /// {Rub3License-purchaseAuthorizationNonce}: it binds the signature to one
    /// token id under a distinct domain tag.
    function renewAuthorizationNonce(uint256 tokenId, bytes32 salt) public view returns (bytes32) {
        return keccak256(abi.encode(_RENEW_AUTHORIZATION, address(this), tokenId, salt));
    }

    /// @dev The one extension, reached by both rails.
    function _extend(uint256 tokenId, address token, uint256 amountPaid) private {
        uint256 current = expiresAt[tokenId];
        uint256 base = current > block.timestamp ? current : block.timestamp;
        uint256 newExpiry = base + period;
        expiresAt[tokenId] = newExpiry;
        emit Renewed(tokenId, newExpiry, token, amountPaid);
    }

    /// @notice True iff `tokenId` exists and has not yet expired.
    ///
    /// Reads one mapping and the clock. Not the wrapper hash set, not
    /// `successor`, not `price`, and nothing the contract owner can write.
    function isValid(uint256 tokenId) external view returns (bool) {
        return expiresAt[tokenId] > block.timestamp;
    }

    /// @dev Carry the migrating holder's remaining time and their frozen renewal
    ///      price across to the successor token.
    ///
    ///      `period` does not carry: it is immutable per contract, so *this*
    ///      contract's `period` governs what the carried price buys from here
    ///      on. A successor with a shorter period therefore changes the
    ///      effective rate. Nothing granted is taken: claiming is opt-in and the
    ///      holder's original token keeps validating on the old contract at its
    ///      original terms forever, so a holder inspects this contract's
    ///      `period` and `price` before claiming.
    ///
    ///      Reads go through {IRub3Predecessor}, the view-only slice a successor
    ///      is allowed to touch, so migration can never disturb the old contract.
    function _afterClaim(uint256 tokenId, uint256 predecessorTokenId) internal override {
        IRub3Predecessor pred = IRub3Predecessor(predecessor);
        expiresAt[tokenId] = pred.expiresAt(predecessorTokenId);
        renewPrice[tokenId] = pred.renewPrice(predecessorTokenId);

        // The stablecoin rail is *this* contract's own listing, not the
        // predecessor's. `IRub3Predecessor` is the view slice frozen at §2.4 and
        // a predecessor deployed before §2.2 cannot answer for a rail it never
        // had, so reading one across would brick the claim for exactly the
        // holders migration exists to serve. A claimed token therefore renews in
        // ETH at the carried price - which is what the predecessor granted - and
        // in this contract's listed token at its listed amount if it offers one.
        renewPriceToken[tokenId] = priceToken;
        renewPriceAmount[tokenId] = priceAmount;
    }
}
