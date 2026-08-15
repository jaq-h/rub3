// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC721}                   from "@openzeppelin/contracts/token/ERC721/ERC721.sol";
import {ERC721Enumerable}         from "@openzeppelin/contracts/token/ERC721/extensions/ERC721Enumerable.sol";
import {Ownable}                  from "@openzeppelin/contracts/access/Ownable.sol";
import {IERC20}                   from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20}                from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";

/// @notice The EIP-3009 slice of a payment token (USDC and every other
///         `transferWithAuthorization` token) that a license contract calls.
///
/// Only {receiveWithAuthorization} is used, never `transferWithAuthorization`,
/// and that choice is the whole front-running defence - see
/// {Rub3License-_payWithAuthorization}.
///
/// **The `bytes signature` overload, not the `(v, r, s)` one.** This is a
/// deliberate narrowing of which payment tokens work. EIP-3009 as written
/// specifies the split form, and a token that implements only that form cannot
/// be used as a `priceToken` here at all. Circle's FiatTokenV2_2 also exposes
/// the `bytes` form, which validates through a signature checker: ECDSA
/// recovery for a 65-byte signature, falling through to EIP-1271
/// `isValidSignature` for a contract signer. Taking that form is what lets an
/// ERC-4337 smart account buy a licence, and agent wallets are increasingly
/// smart accounts - the buyers this rail exists for. Since the `bytes` form
/// already accepts a 65-byte EOA signature unchanged, one entry point serves
/// both kinds of buyer and there is no second payment path to drift.
interface IERC3009 {
    function receiveWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        bytes calldata signature
    ) external;

    /// True once an authorization has been used or cancelled. Read by the
    /// constructor probe as the cheapest view every EIP-3009 token answers.
    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool);
}

/// @notice The slice of a predecessor license contract that a successor reads
///         during {Rub3License-claimFromPredecessor}. Deliberately tiny: a
///         successor never calls anything on its predecessor that could mutate
///         state, so migration can never disturb the old contract.
interface IRub3Predecessor {
    function ownerOf(uint256 tokenId) external view returns (address);
    function successor() external view returns (address);

    /// The subscription slice, absent on a predecessor that is not a
    /// subscription. `period()` is read only by the constructor probes, as the
    /// discriminator between a subscription and an access license: both
    /// concrete contracts probe it, {Rub3Subscription} requiring a predecessor
    /// to answer it and {Rub3Access} requiring one to fail it, so cross-model
    /// succession cannot be deployed at all. It is immutable per contract and
    /// never carries across a claim. `expiresAt` / `renewPrice` are the
    /// per-token terms `_afterClaim` actually carries.
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
///
/// # Payment rails
///
/// Two, and they mint identically. ETH is the `payable` path a human wallet
/// uses; `priceToken` / `priceAmount` is the stablecoin path an agent uses, paid
/// with an EIP-3009 authorization the buyer signs off-chain and *anyone* may
/// submit (implementation.md §2.2). Neither is privileged: both take payment
/// through one of the two helpers at the bottom of this contract and then reach
/// the same single mint in the concrete contract, so a token bought with USDC is
/// indistinguishable from one bought with ETH in state, events, and terms.
abstract contract Rub3License is ERC721, ERC721Enumerable, Ownable, ReentrancyGuardTransient {
    /// @notice What a licence costs, on both rails.
    ///
    ///         Grouped rather than passed as three loose constructor arguments:
    ///         it names the concept, keeps the two rails visibly parallel, and
    ///         keeps the concrete constructors inside solc's stack limit.
    struct SaleTerms {
        /// Price in wei. The ETH rail, always available.
        uint256 price;
        /// EIP-3009 ERC-20 accepted alongside ETH, or `address(0)` for ETH only.
        address priceToken;
        /// Price in `priceToken`'s smallest unit. Must be `0` when there is no
        /// token; independent of `price`, never converted from it.
        uint256 priceAmount;
    }

    /// @notice How this collection derives a user identity, and what backs it.
    ///
    ///         Grouped because the two fields are not independent: the
    ///         constructor requires a TBA implementation for the account model
    ///         and forbids one for the access model, so "which model" and "which
    ///         implementation" are one decision made once. Grouping also keeps
    ///         {Rub3Subscription}'s constructor inside solc's stack limit, which
    ///         a twelfth loose argument would push it past.
    struct IdentityTerms {
        /// 0 = access (user_id = wallet), 1 = account (user_id = TBA).
        uint8 model;
        /// ERC-6551 account implementation. Required iff `model == 1`.
        address tbaImplementation;
    }

    /// @notice The protocol's cut of every payment this contract takes, frozen
    ///         at deploy.
    ///
    ///         Grouped for the same reasons as {SaleTerms}: it names the
    ///         concept, and it costs the concrete constructors one stack slot
    ///         rather than two. Both fields become `immutable`, so what a
    ///         developer's economics are is settled before the first buyer
    ///         looks and can never move afterwards - see {feeBps}.
    ///
    ///         A direct (non-factory) deploy passes `FeeTerms(0, address(0))`
    ///         and carries no fee at all. That is deliberate: the templates are
    ///         open source and deploying one directly stays possible. What a
    ///         factory deploy buys is being *listable* in the registry and
    ///         marketplace, which trust only what {Rub3Factory} recorded.
    struct FeeTerms {
        /// Protocol fee in basis points of every payment received. `0` disables
        /// the fee, which requires `treasury` to be `address(0)` too.
        uint16 feeBps;
        /// Where the fee accrues to. Must be set iff `feeBps` is non-zero.
        address treasury;
    }

    /// @notice The EIP-3009 authorization a buyer signs, minus the three fields
    ///         this contract derives rather than accepts.
    ///
    ///         `to` is always this contract, `value` is always the listed price
    ///         at execution time, and `nonce` is always derived from *what is
    ///         being bought* (see {purchaseAuthorizationNonce}). All three are
    ///         covered by the buyer's EIP-712 signature on the token, so a
    ///         submitter who alters any of them produces a digest the token
    ///         refuses. `salt` is the buyer's own randomness, the only free
    ///         input to the nonce.
    ///
    ///         `signature` is opaque bytes rather than split `(v, r, s)` so the
    ///         payment token decides what a valid signature is: 65 bytes of
    ///         `r || s || v` from an EOA, or an EIP-1271 signature from a
    ///         smart-contract wallet. This contract never inspects it, never
    ///         branches on its length, and never recovers a signer from it -
    ///         see {IERC3009}.
    struct PaymentAuthorization {
        address from;
        uint256 validAfter;
        uint256 validBefore;
        bytes32 salt;
        bytes   signature;
    }

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

    /// @notice ERC-20 accepted for purchase alongside ETH, or `address(0)` when
    ///         this contract sells for ETH only. Must implement EIP-3009.
    ///
    ///         This is also how the rail is *advertised*: the wrapper reads
    ///         `priceToken()` in one `eth_call` and takes a zero (or a revert,
    ///         on a contract deployed before §2.2) as "ETH only".
    ///
    ///         Set by {setTokenPrice}, and like {setPrice} it moves what is
    ///         offered to future buyers only - `Rub3Subscription` snapshots both
    ///         rails per token at mint.
    address public priceToken;

    /// @notice Purchase price denominated in `priceToken`, in that token's own
    ///         smallest unit (USDC has 6 decimals, so `5000000` is 5 USDC).
    ///
    ///         Independent of `price`, not converted from it: the contract holds
    ///         no oracle, so the developer quotes each rail separately.
    uint256 public priceAmount;

    // ── Protocol fee (§2.3) ───────────────────────────────────────────────────

    /// @notice Basis-point denominator. A fee of `feeBps` on `amount` is
    ///         `amount * feeBps / BPS_DENOMINATOR`.
    uint256 public constant BPS_DENOMINATOR = 10_000;

    /// @notice The protocol's share of every payment this contract takes, in
    ///         basis points. `0` on a directly deployed contract.
    ///
    ///         **Immutable, and that is the product promise rather than an
    ///         implementation detail.** A developer's economics are settled the
    ///         moment their contract is deployed: there is no setter here, none
    ///         in {Rub3Factory}, and no path of any kind - owner, factory, or
    ///         protocol - that can raise, lower, or redirect this contract's fee
    ///         afterwards. rub3 changes its take only by shipping a new factory
    ///         version, which affects deploys made after it and nothing else.
    uint16 public immutable feeBps;

    /// @notice Where the protocol fee accrues. `address(0)` iff `feeBps == 0`.
    ///         Immutable for the same reason as {feeBps}: a redirectable
    ///         recipient is a changed deal.
    address public immutable treasury;

    /// @notice Protocol fee in wei taken from ETH payments and not yet swept to
    ///         {treasury}. Held here rather than pushed on the money path -
    ///         see {_accrueFee}.
    ///
    ///         {withdraw} pays the developer `address(this).balance` *minus*
    ///         this, so the two shares are disjoint and neither can be spent
    ///         twice.
    uint256 public feesAccrued;

    /// @notice Per-ERC-20 protocol fee taken on the stablecoin rail and not yet
    ///         swept to {treasury}. The ETH counterpart of {feesAccrued}, and
    ///         subtracted from {withdrawToken} exactly the same way.
    mapping(address => uint256) public tokenFeesAccrued;

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
    event TokenPriceUpdated(
        address oldToken,
        uint256 oldAmount,
        address newToken,
        uint256 newAmount
    );
    /// @notice A payment was split. `token` is `address(0)` when the payment was
    ///         in ETH and `amount`/`fee` are wei; otherwise both are in that
    ///         ERC-20's smallest unit.
    ///
    ///         `developerAmount` is stated rather than left to be derived, so
    ///         the two shares and their sum are all readable from one log.
    event ProtocolFeeAccrued(
        address indexed token,
        uint256 amount,
        uint256 fee,
        uint256 developerAmount
    );

    /// @notice Accrued fee swept to {treasury}. `token` is `address(0)` for ETH.
    event ProtocolFeeWithdrawn(address indexed token, address indexed treasury, uint256 amount);

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
    error TokenPaymentUnavailable();
    error IncompatiblePriceToken(address token);
    error TokenPriceInconsistent(address token, uint256 amount);
    error FeeBpsTooHigh(uint16 feeBps, uint256 maximum);
    error FeeTermsInconsistent(uint16 feeBps, address treasury);
    error NoFeeConfigured();
    error WithdrawFailed();
    error NotTokenOwner(address caller, address owner);
    error CooldownActive(uint256 blocksRemaining);
    error ZeroWrapperHash();
    error WrapperHashAlreadyKnown(bytes32 hash);
    error WrapperHashNotValid(bytes32 hash);
    error RevocationReasonRequired();
    error SelfReference();
    error IncompatiblePredecessor(address predecessor);
    error NoPredecessor();
    error SuccessorNotDeclared(address declared);
    error PredecessorTokenAlreadyClaimed(uint256 predecessorTokenId);

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
    ) ERC721(name_, symbol_) Ownable(owner_) {
        if (identity_.model > 1) revert InvalidIdentityModel(identity_.model);
        if (cooldownBlocks_ < MIN_COOLDOWN_BLOCKS) {
            revert CooldownTooSmall(cooldownBlocks_, MIN_COOLDOWN_BLOCKS);
        }
        // Account model must pick a TBA implementation; access model must not.
        if (identity_.model == 1 && identity_.tbaImplementation == address(0)) {
            revert TbaImplementationRequired();
        }
        if (identity_.model == 0 && identity_.tbaImplementation != address(0)) {
            revert TbaImplementationForbidden();
        }
        if (predecessor_ == address(this)) revert SelfReference();

        // The fee is frozen from here on, so both ways of getting it wrong are
        // rejected at the only moment they can still be corrected.
        //
        // The bound is arithmetic, not economic: at or below 100% the fee can
        // never exceed the payment it is taken from, which is what makes "the
        // two shares sum to what arrived" true by construction rather than by
        // argument. The *protocol's* range - 200 to 300 bps - is a narrower rule
        // about what rub3 charges, and it belongs to {Rub3Factory}, which is the
        // thing that has an economics policy. This base is the open-source
        // template anyone may deploy, and a deployer who charges themselves is
        // only reducing their own take.
        if (fee_.feeBps > BPS_DENOMINATOR) revert FeeBpsTooHigh(fee_.feeBps, BPS_DENOMINATOR);
        // Both or neither. A fee with no recipient would strand every buyer's
        // money in the contract with no one able to move it; a recipient with no
        // fee advertises a claim on revenue that does not exist.
        if ((fee_.feeBps == 0) != (fee_.treasury == address(0))) {
            revert FeeTermsInconsistent(fee_.feeBps, fee_.treasury);
        }

        // `predecessor` is immutable and {claimFromPredecessor} reads the
        // {IRub3Predecessor} slice off it, so an address that cannot answer that
        // slice would brick every holder's claim forever with redeployment the
        // only remedy. Probe `successor()`, the rub3-specific view getter every
        // Rub3License answers. Not `ownerOf`, which reverts for an unminted id
        // on a perfectly good predecessor; and not the returned value, because
        // the predecessor points its `successor` here only after this deploy.
        if (predecessor_ != address(0)) {
            if (predecessor_.code.length == 0) revert IncompatiblePredecessor(predecessor_);
            try IRub3Predecessor(predecessor_).successor() returns (address) {}
            catch { revert IncompatiblePredecessor(predecessor_); }
        }

        _setTokenPrice(sale_.priceToken, sale_.priceAmount);

        identityModel     = identity_.model;
        tbaImplementation = identity_.tbaImplementation;
        price             = sale_.price;
        feeBps            = fee_.feeBps;
        treasury          = fee_.treasury;
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

    /// @notice Set (or withdraw) the ERC-20 rail offered to *future* buyers.
    ///
    /// `token == address(0)` (with `amount == 0`) stops offering the rail. It
    /// reaches nothing already issued, exactly like {setPrice}: an access token
    /// is paid for once, and a subscription snapshots *both* rails at mint, so a
    /// holder keeps renewing in the token they bought under at the amount they
    /// bought under even after this is repointed or cleared.
    function setTokenPrice(address token, uint256 amount) external onlyOwner {
        _setTokenPrice(token, amount);
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

    /// @notice Sweep the developer's ETH balance to `to`.
    ///
    /// That balance is everything the contract holds *except* the protocol fee
    /// already accrued against it. The subtraction is what keeps the two shares
    /// disjoint: fees are held here rather than pushed at payment time, so
    /// without it the developer would be able to withdraw rub3's cut, and rub3
    /// the developer's. Anything force-sent to this contract (a `selfdestruct`
    /// beneficiary, a coinbase payout) is the developer's, since no fee was ever
    /// taken on it.
    function withdraw(address payable to) external onlyOwner {
        uint256 amount = address(this).balance - feesAccrued;
        (bool ok, ) = to.call{value: amount}("");
        if (!ok) revert WithdrawFailed();
    }

    /// @notice Sweep the accrued ETH protocol fee to {treasury}.
    ///
    /// **Permissionless on purpose.** The destination is immutable, so the only
    /// thing a caller decides is *when*, and rub3 collecting should not depend
    /// on rub3 sending a transaction on every contract that ever sold a licence.
    /// A developer, an indexer, or a keeper may settle it. It cannot be aimed
    /// anywhere else, and it cannot touch the developer's share.
    function withdrawFees() external returns (uint256 amount) {
        address to = treasury;
        if (to == address(0)) revert NoFeeConfigured();

        amount = feesAccrued;
        // Zeroed before the call: checks-effects-interactions, so a treasury
        // that re-enters finds nothing left to claim.
        feesAccrued = 0;

        (bool ok, ) = payable(to).call{value: amount}("");
        if (!ok) revert WithdrawFailed();

        emit ProtocolFeeWithdrawn(address(0), to, amount);
    }

    /// @notice Sweep the accrued protocol fee in `token` to {treasury}. The
    ///         stablecoin counterpart of {withdrawFees}, permissionless for the
    ///         same reason.
    function withdrawTokenFees(address token) external returns (uint256 amount) {
        address to = treasury;
        if (to == address(0)) revert NoFeeConfigured();

        amount = tokenFeesAccrued[token];
        tokenFeesAccrued[token] = 0;

        SafeERC20.safeTransfer(IERC20(token), to, amount);

        emit ProtocolFeeWithdrawn(token, to, amount);
    }

    /// @notice Sweep the contract's whole balance of an ERC-20 to `to`.
    ///
    /// The counterpart of {withdraw} for the stablecoin rail - without it,
    /// everything paid through {_payWithAuthorization} would be stranded. It
    /// moves ERC-20 balances only and cannot touch a license token: this
    /// contract's own ERC-721 exposes no `transfer(address,uint256)`, so passing
    /// its address reverts rather than doing anything.
    /// Like {withdraw}, it moves the developer's share only: whatever this
    /// contract holds of `token` less the protocol fee accrued in it. For a
    /// token nobody ever paid in - one somebody transferred here by mistake -
    /// nothing is reserved and the whole balance sweeps, since no fee was taken.
    function withdrawToken(address token, address to) external onlyOwner {
        IERC20 erc20 = IERC20(token);
        uint256 amount = erc20.balanceOf(address(this)) - tokenFeesAccrued[token];
        SafeERC20.safeTransfer(erc20, to, amount);
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

    // ── Payment rails (ETH and EIP-3009) ──────────────────────────────────────

    /// @dev Domain tag for a *purchase* authorization nonce. Distinct from
    ///      {_RENEW_AUTHORIZATION} so an authorization signed to buy a token can
    ///      never be replayed to renew one, or the reverse.
    bytes32 internal constant _PURCHASE_AUTHORIZATION = keccak256("rub3.PurchaseAuthorization.v1");

    /// @dev Domain tag for a *renewal* authorization nonce. Used only by
    ///      {Rub3Subscription}; declared here so both derivations sit side by
    ///      side and are visibly disjoint.
    bytes32 internal constant _RENEW_AUTHORIZATION = keccak256("rub3.RenewAuthorization.v1");

    /// @notice The EIP-3009 nonce a purchase authorization must carry.
    ///
    /// EIP-3009 signs six fields and no more, and `recipient` is not one of
    /// them. Left unbound, a submitter watching the mempool could take a
    /// buyer's authorization, pass their own address as `recipient`, and mint
    /// the license to themselves with the buyer's money. Binding the recipient
    /// *into the nonce* closes that: the nonce is signed, this contract derives
    /// it rather than accepting it, and a changed recipient derives a different
    /// nonce, which yields a digest the buyer never signed. The token rejects
    /// it and the whole transaction reverts.
    ///
    /// `address(this)` is in the preimage as well. The token already binds the
    /// contract through `to`, so this is belt and braces - it means the nonce
    /// itself is worthless anywhere but here.
    ///
    /// A buyer calls this, signs `ReceiveWithAuthorization` over the returned
    /// nonce, and hands the signature to whoever is submitting.
    function purchaseAuthorizationNonce(address recipient, bytes32 salt)
        public
        view
        returns (bytes32)
    {
        return keccak256(abi.encode(_PURCHASE_AUTHORIZATION, address(this), recipient, salt));
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

    /// @dev The authorization path's recipient default, which is *not*
    ///      {_resolveRecipient}: `msg.sender` there is whoever submitted the
    ///      authorization - a facilitator, or an attacker - and defaulting to
    ///      them would hand the license to the submitter. It defaults to the
    ///      buyer who signed instead. Both spellings ("0" and the buyer's own
    ///      address) resolve to the same recipient and so to the same nonce, so
    ///      a submitter gains nothing by choosing between them.
    function _resolveAuthorizedRecipient(address recipient, address from)
        internal
        pure
        returns (address)
    {
        return recipient == address(0) ? from : recipient;
    }

    /// @dev The ETH leg of a payment: the buyer's own transaction carries the
    ///      money, so there is nothing to move, only a floor to check.
    ///
    ///      This and {_payWithAuthorization} are the only two places in the
    ///      contracts where a payment is taken, which is why the §2.3 protocol
    ///      fee lands in exactly these two functions: no entry point and no mint
    ///      path changed for it, and neither rail can acquire a payment the
    ///      other's fee rule does not reach.
    function _payEth(uint256 due) internal {
        if (msg.value < due) revert InsufficientPayment(msg.value, due);
        _accrueFee(address(0), msg.value);
    }

    /// @dev The stablecoin leg: pull exactly `amount` of `token` from
    ///      `auth.from` using the EIP-3009 authorization they signed.
    ///
    ///      **`receiveWithAuthorization`, never `transferWithAuthorization`.**
    ///      The two carry the same six signed fields under different typehashes,
    ///      and the difference is the whole safety story. Any address may submit
    ///      a `transferWithAuthorization` straight to the token, so an attacker
    ///      watching the mempool could move a buyer's USDC into this contract
    ///      *without* the mint, burning the nonce and leaving the buyer paid-up
    ///      and licence-less with no way to recover. `receiveWithAuthorization`
    ///      requires `msg.sender == to`, and `to` is this contract, so the only
    ///      way to spend the authorization at all is through this function -
    ///      which mints. Payment and mint are inseparable. EIP-3009 added the
    ///      variant for exactly this reason, and USDC implements it.
    ///
    ///      Anyone may still submit, which is what keeps the purchase gasless
    ///      for the buyer: they sign, a facilitator pays the gas, and the token
    ///      goes to the buyer regardless.
    ///
    ///      Replay is the token's own job and it does it: the authorization is
    ///      recorded against `(from, nonce)` and a second use reverts. The
    ///      balance delta below is the independent check - it holds even against
    ///      a payment token that fails silently, and it is what makes "the mint
    ///      happened" mean "the money arrived".
    ///
    ///      `value` is not a parameter. It is the listed price read at execution
    ///      time, so a buyer cannot be made to pay more than the amount their
    ///      signature covers: if the price moved after they signed, the digest
    ///      no longer matches and the token rejects it.
    function _payWithAuthorization(
        PaymentAuthorization calldata auth,
        address token,
        uint256 amount,
        bytes32 nonce
    ) internal {
        if (token == address(0)) revert TokenPaymentUnavailable();

        IERC20 erc20 = IERC20(token);
        uint256 balanceBefore = erc20.balanceOf(address(this));

        IERC3009(token).receiveWithAuthorization(
            auth.from,
            address(this),
            amount,
            auth.validAfter,
            auth.validBefore,
            nonce,
            auth.signature
        );

        uint256 received = erc20.balanceOf(address(this)) - balanceBefore;
        if (received < amount) revert InsufficientPayment(received, amount);

        // The fee is taken on what measurably arrived, exactly as the ETH rail
        // takes it on `msg.value`, so the split is the same rule on both rails
        // rather than two rules that happen to agree.
        _accrueFee(token, received);
    }

    /// @dev Split a payment that has just arrived: `feeBps` of it to
    ///      {treasury}, everything else to the developer.
    ///
    ///      **The fee is charged on the amount received, not on the listed
    ///      price.** Charging the listed price would leave a hole wide enough to
    ///      drive the whole protocol fee through: a developer lists at 0 (or at
    ///      1 wei), publishes a client that pays the real price as
    ///      "overpayment", and the fee on every sale is zero while the money
    ///      still lands in `withdraw`. Charging what arrived closes it, and it
    ///      is also the reading that makes the arithmetic exact - `fee` plus the
    ///      developer's share is the payment, with nothing left over and no
    ///      rounding to account for anywhere else.
    ///
    ///      Rounding is integer division, so it favours the developer: a fee
    ///      that comes to less than one wei (or one of the token's smallest
    ///      units) is zero, never one. The whole payment is then the
    ///      developer's, which is the correct total either way.
    ///
    ///      **Accrued, not pushed.** The alternative - transferring the fee to
    ///      {treasury} inside the purchase - puts an external call the protocol
    ///      chose on every buyer's money path, and `treasury` is immutable: a
    ///      recipient that reverts on receipt, or that one day costs more gas
    ///      than a buyer sent, would break every purchase on that contract
    ///      forever with no way to fix it. Accruing keeps the money path free of
    ///      calls out and leaves collection to {withdrawFees}, where a failure
    ///      is rub3's problem and not the buyer's.
    function _accrueFee(address token, uint256 amount) private {
        uint256 bps = feeBps;
        if (bps == 0) return;

        // `bps <= BPS_DENOMINATOR` is frozen by the constructor, so the fee can
        // never exceed `amount` and the developer's share can never underflow.
        uint256 fee = (amount * bps) / BPS_DENOMINATOR;
        if (token == address(0)) {
            feesAccrued += fee;
        } else {
            tokenFeesAccrued[token] += fee;
        }

        emit ProtocolFeeAccrued(token, amount, fee, amount - fee);
    }

    /// @dev Shared by the constructor and {setTokenPrice}.
    ///
    ///      A price token that cannot answer the EIP-3009 read slice would
    ///      advertise a rail that reverts for every buyer, so it is rejected
    ///      where it is set rather than discovered by the first agent that
    ///      tries to pay. `authorizationState` is the probe: a view every
    ///      EIP-3009 token answers, for any argument, with no token minted and
    ///      no state touched.
    ///
    ///      It deliberately does **not** probe for the `bytes signature`
    ///      overload of `receiveWithAuthorization` that {_payWithAuthorization}
    ///      calls, and nothing here should be changed to try. A staticcall
    ///      probe cannot tell "no such function" from "bad signature": both
    ///      revert, so the probe would either reject conforming tokens or
    ///      accept non-conforming ones, and being wrong in either direction at
    ///      deploy time is frozen forever. Detecting a missing overload belongs
    ///      off-chain, where the wrapper pre-flights the real call before
    ///      broadcasting and falls back to the ETH rail if it fails.
    ///
    ///      An amount without a token is a misconfiguration in the other
    ///      direction - it reads as "5 USDC of nothing" - and is rejected too. A
    ///      token with a zero amount is *not*: a free tier is legitimate, and it
    ///      still takes the buyer's signature to mint.
    function _setTokenPrice(address token, uint256 amount) private {
        if (token == address(0)) {
            if (amount != 0) revert TokenPriceInconsistent(token, amount);
        } else {
            if (token.code.length == 0) revert IncompatiblePriceToken(token);
            try IERC3009(token).authorizationState(address(0), bytes32(0)) returns (bool) {}
            catch { revert IncompatiblePriceToken(token); }
        }

        emit TokenPriceUpdated(priceToken, priceAmount, token, amount);
        priceToken  = token;
        priceAmount = amount;
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
