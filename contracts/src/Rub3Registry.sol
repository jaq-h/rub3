// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Ownable}      from "@openzeppelin/contracts/access/Ownable.sol";
import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";

import {Rub3Factory} from "./Rub3Factory.sol";
import {Rub3License} from "./Rub3License.sol";

/// @notice The discovery registry: which rub3 applications exist, which of them
///         are listable, and in what order a buyer should be shown them
///         (implementation.md §3.2).
///
/// # This is not {Rub3CodeRegistry}, and the two are never interchangeable
///
/// Two contracts in this repository have "registry" in the name and they answer
/// different questions. Confusing them is the single most likely mistake a
/// reader of this file will make, so it is worth stating flatly:
///
/// | Contract | Question it answers | Keyed by |
/// |---|---|---|
/// | {Rub3CodeRegistry} | "is this bytecode a genuine rub3 release?" | masked code hash |
/// | `Rub3Registry` (this one) | "which apps exist, and which are listable?" | licence contract address |
///
/// A wrapper on the purchase path consults {Rub3CodeRegistry} and never this
/// contract. An agent shopping for an application reads this contract and never
/// needs the other one until it has an address to verify. Neither is evidence
/// for the other's question: canonical *code* says nothing about which address
/// runs it, and a listing here says nothing about whether the code at that
/// address is genuine. An agent that wants both asks both.
///
/// # Discovery, never validity
///
/// **Nothing this contract can do affects a token, a session, or a payment.**
/// Delisting removes the badge and the listing. It does not, and structurally
/// cannot, invalidate an issued token, end a live session, block a renewal, or
/// change what a licence contract charges. The proof is an absence rather than a
/// promise: no licence contract in this project reads this contract, holds its
/// address, or has any function that could be made to. `ownerOf`, `isValid` and
/// `activate` run on state that lives in the licence contract, and this contract
/// has no path into it - every external call it makes is a `view`.
///
/// That is the same boundary implementation.md §2.4 draws for the whole project,
/// applied to the one contract that plausibly *could* have crossed it: a
/// discovery surface with a delist button is exactly where a revocation surface
/// would be smuggled in, if it were going to be. `test/Rub3Registry.t.sol`
/// asserts it behaviourally - a held token, its validation, its live session and
/// a fresh activation all survive a delist, a suspension, and an un-recognised
/// payment token - rather than leaving it as a claim in a comment.
///
/// The corollary is what bounds a compromise of the owner key here: it can hide
/// listings, restore them, and reorder them. It cannot take away anything anyone
/// paid for. There is no state in this contract whose worst case is worse than
/// "the discovery surface is wrong until it is fixed".
///
/// # What may be listed
///
/// {register} gates on two things, and both are checked live:
///
/// 1. **The licence contract was deployed by a canonical factory** -
///    {isCanonicalDeploy}, which walks {previousFactory} from the factory this
///    registry was constructed with, so an older generation's deploys stay
///    listable when rub3 ships a new factory. A directly deployed licence is
///    perfectly valid software and is simply not listable here; that is the
///    trade the fee-free direct-deploy path makes (contracts.md -> "The accepted
///    position on fee-free deployment").
/// 2. **The caller owns that licence contract** - `Rub3License.owner()`, read at
///    the moment of the call. Authority over a listing therefore follows the
///    licence contract's ownership without this registry being told: transfer
///    the licence contract and the new owner controls the listing, with no
///    stale `registrant` field here to contradict them.
///
/// # The entry is an agent card
///
/// {card} returns one machine-readable record per listing: the contract address,
/// both price rails, the identity model, the wrapper hash set with each hash's
/// status, the content URI, and the frozen fee terms. It is assembled from live
/// reads of the licence contract at call time, never from a copy taken at
/// registration, so a card cannot describe terms the contract no longer offers.
/// Only {Listing-appName} and {Listing-contentURI} are held here, because they
/// are the two facts the chain does not carry yet - §3.1 puts `contentURI` on
/// the licence contract, and this field is what a listing quotes until it does.
///
/// The one field a card does not carry whole is the wrapper hash set, which the
/// licence contract lets its owner grow without limit: {card} takes the newest
/// {MAX_CARD_WRAPPER_HASHES} and reports the true total beside them, so a
/// truncated answer is always distinguishable from a complete one and no
/// listing's own publishing history can decide what reading a page of cards
/// costs anybody else.
///
/// # Every read is available in a form whose cost the caller controls
///
/// Registration is permissionless for anyone holding a factory deploy, and
/// nothing is ever removed from {registered} - delisting and suspension change
/// an entry's flags. The set therefore only grows, and it grows at a rate
/// strangers decide, so any read that scans all of it is on a clock that runs
/// out. This contract cannot be redeployed to fix that later, so both forms of
/// every such read exist from the start:
///
/// | Whole set | Bounded | What the bounded one gives up |
/// |---|---|---|
/// | {registered} | {registeredWindow} | nothing |
/// | {rankedListings}, {rankedListingWindow} | {rankedRegistrationWindow} | a globally correct order |
/// | {cards} | {cardWindow} | the same |
///
/// {rankedListingWindow} and {cards} are in the left column deliberately. They
/// take a `start` and a `count` and look bounded, but those index into the
/// global ranking, which has to be computed over everything before a page of it
/// can be cut: they bound the response and not the work. That is a legitimate
/// read - a globally correct page is worth paying for - and it is written on
/// both of them so nobody has to find it out from a gas limit.
///
/// The bounded reads take their cursor over registration order instead, and rank
/// only inside the window they were given. Paging through them does not
/// reconstruct {rankedListings}; a caller that wants the global order from
/// bounded calls collects the windows and ranks them off-chain, where
/// {isRecognisedRail} is the same input this contract uses.
///
/// # Ranking, and why it must not be a snapshot
///
/// The protocol fee accrues in whatever asset a licence contract lists as its
/// `priceToken`, and the licence contracts deliberately hold no policy about
/// which assets count (architecture.md -> "Why the fee split is shaped this
/// way"). That judgement lives here: {setTokenRecognised} maintains the
/// recognised-token list, and {rankedListings} puts entries priced in a
/// recognised token above entries that are not.
///
/// The native rail is always recognised and cannot be un-recognised. An ETH-only
/// contract quotes no token at all (`priceToken == address(0)`) and its fee
/// accrues in ETH, so the only entries that rank below are those quoting a token
/// rail in an asset this registry does not recognise.
///
/// **The rank reads `priceToken` live, on every call, and this is the part that
/// would be wrong if it were done the obvious way.** `setTokenPrice(address,uint256)`
/// stays owner-callable on a licence contract for its whole life, so a contract
/// registered while priced in a recognised token can switch to something else
/// the block afterwards. A rank frozen at registration would go on advertising
/// that contract on a quote it no longer honours, and no event this registry
/// emits would say so. Reading the quote at call time is the only form of this
/// rule that cannot go stale. It costs one `eth_call` per entry inside a `view`,
/// which is the right place to spend it.
///
/// An off-chain indexer that would rather not re-read everything has the
/// equivalent: re-validate an entry whenever the licence contract emits
/// `TokenPriceUpdated`. What it must not do is read the quote once at
/// registration and keep it.
///
/// Ranking is discovery, so it is bound by the same invariant as delisting: an
/// entry that drops to the bottom of the list has lost placement and nothing
/// else. Its tokens, its sessions and its renewals are untouched.
contract Rub3Registry is Ownable2Step {
    /// @notice Where a listing is in its lifecycle. `Unknown` is the zero value,
    ///         so an address nobody registered reads as unknown rather than as
    ///         anything a caller might act on.
    ///
    ///         Every transition here is reversible, which is the shape a
    ///         discovery record is allowed to have and a licence record is not.
    ///         {Rub3CodeRegistry.Status} is one-way for the opposite reason: it
    ///         describes code an agent may already have acted on.
    enum Status {
        Unknown,
        Listed,
        Delisted
    }

    /// @notice One wrapper binary hash and what the licence contract says about
    ///         it, as {card} reports the set.
    ///
    ///         The status travels with the hash rather than being left for the
    ///         caller to fetch, because a card listing a revoked hash beside
    ///         valid ones with no way to tell them apart is worse than not
    ///         listing hashes at all.
    struct WrapperHash {
        bytes32 hash;
        /// Raw {Rub3License.HashStatus}: 0 unknown, 1 valid, 2 revoked.
        uint8 status;
    }

    /// @notice What this registry stores about one listing. Everything else on
    ///         an entry's {card} is read off the licence contract at call time.
    struct Listing {
        Status status;
        /// True while the registry owner has withheld the badge. Independent of
        /// {status}: a suspended entry that its owner also delisted must not
        /// become visible again just because one of the two was cleared.
        bool suspended;
        /// The block {register} ran in. Recorded by this contract, never
        /// supplied by the caller.
        uint64 registeredAtBlock;
        /// Human-readable application name. Presentation only, never security:
        /// two listings may carry the same name and neither is evidence of
        /// anything about the other.
        string appName;
        /// Where the wrapped binary lives (IPFS/Arweave). Held here because the
        /// licence contract does not carry it yet - see the contract docs.
        ///
        /// May be empty, which means "nothing published yet" and is the honest
        /// state while §3.1 is unbuilt. Deliberately not required: a mandatory
        /// field a developer has no value for is filled with a placeholder, and
        /// a placeholder that reads like a URI is worse than an empty string
        /// that reads like nothing - the same position
        /// `contracts/deployments.json` takes on unpublished addresses.
        string contentURI;
    }

    /// @notice One listing, in the machine-readable form an agent's spend policy
    ///         consumes. Assembled fresh on every call; see the contract docs.
    struct AgentCard {
        /// The licence contract. This is the address a buyer transacts with, and
        /// the only field here that is an identity rather than a description.
        address license;
        /// Current owner of the licence contract, read live.
        address licenseOwner;
        string appName;
        string contentURI;
        Status status;
        bool suspended;
        /// True when this entry is visible in {rankedListings}: listed by its
        /// owner and not suspended by this registry.
        bool listed;
        /// Purchase price on the native rail, in wei. Always available.
        uint256 price;
        /// ERC-20 accepted alongside ETH, or `address(0)` for ETH only.
        address priceToken;
        /// Price in `priceToken`'s own smallest unit. Meaningless when
        /// `priceToken` is `address(0)`.
        uint256 priceAmount;
        /// Whether this entry's quote ranks in the upper group - see
        /// {isRecognisedRail}.
        bool recognisedRail;
        /// 0 = access (user_id = wallet), 1 = account (user_id = TBA).
        uint8 identityModel;
        /// ERC-6551 implementation token-bound accounts resolve to. Zero unless
        /// `identityModel == 1`.
        address tbaImplementation;
        /// Protocol fee stamped into the licence contract at deploy, in basis
        /// points. Frozen there, so a buyer auditing the §2.4 economics reads it
        /// here and finds the same number forever.
        uint16 feeBps;
        /// Where that fee accrues. Frozen with {feeBps}.
        address treasury;
        /// The most recently published wrapper hashes, in publication order,
        /// with each one's status. At most {MAX_CARD_WRAPPER_HASHES} of them -
        /// compare against {wrapperHashCount} to find out whether that is all
        /// of them, or read {wrapperHashesTruncated}, which says the same thing
        /// without arithmetic.
        WrapperHash[] wrapperHashes;
        /// How many wrapper hashes the licence contract actually holds, whether
        /// or not they fit above.
        ///
        /// Reported next to the capped array so a partial answer is never
        /// mistaken for a complete one. A card that quietly returned a short
        /// list would tell an agent that a hash it holds was never published,
        /// which is the one wrong answer this field exists to prevent.
        uint256 wrapperHashCount;
        /// True when {wrapperHashes} is a suffix of the published set rather
        /// than the whole of it.
        bool wrapperHashesTruncated;
        uint64 registeredAtBlock;
    }

    /// @notice How many *earlier* factories {isCanonicalDeploy} consults beyond
    ///         {factory}, walking {previousFactory}. Nine generations are
    ///         therefore listable: this registry's factory and the eight before
    ///         it.
    ///
    ///         The bound exists because the chain's length is decided by whoever
    ///         deploys the factories rather than by this contract, and an
    ///         unbounded loop inside a `view` that a listing page calls for every
    ///         entry is a denial of service waiting for a long enough chain.
    ///
    ///         It matches {Rub3Factory.MAX_PREDECESSOR_FACTORY_HOPS} in value
    ///         and is deliberately a separate constant. The two answer different
    ///         questions - "may this contract be named as a predecessor" and "may
    ///         this contract be listed" - and a future factory that tightened its
    ///         own rule must not silently delist nine generations of apps here.
    uint256 public constant MAX_FACTORY_GENERATION_HOPS = 8;

    /// @notice How many wrapper hashes {card} will assemble onto one card.
    ///
    ///         `Rub3License.addWrapperHash` is append-only and uncapped, and the
    ///         status of each hash is a separate read. Without a bound here, a
    ///         licence owner who published a large enough hash set could make
    ///         their own card - and therefore any {cardWindow} or {cards} page
    ///         containing it - run out of gas. That would be one listing's owner
    ///         deciding that unrelated listings sharing their page stop being
    ///         readable, which is a reach into other people's discoverability
    ///         that no listing owner gets to have.
    ///
    ///         The cap is on the card, never on the licence contract: the full
    ///         set stays published and readable there through
    ///         `wrapperHashCount()` and `wrapperHashAt(index)`, and a card says
    ///         how many there are ({AgentCard-wrapperHashCount}) so a caller can
    ///         always tell a capped answer from a complete one.
    ///
    ///         The *newest* hashes are the ones kept, for the same reason
    ///         {Rub3CodeRegistry.latestOffsetTables} spends its budget from that
    ///         end: a buyer checking the build it just downloaded is asking
    ///         about the most recently published hash, so a card truncated from
    ///         the old end would drop exactly the entries the question is about.
    uint256 public constant MAX_CARD_WRAPPER_HASHES = 32;

    /// @notice The canonical {Rub3Factory} this registry trusts, frozen at
    ///         deploy.
    ///
    ///         Which address that is, per chain, is published in
    ///         `contracts/deployments.json` - the one committed place that gives
    ///         "deployed through the factory" a referent, keyed by chain id and
    ///         carrying the deploy block an indexer starts from and the
    ///         generation in the {previousFactory} chain. A deploy script reads
    ///         it from there rather than from an address somebody was handed.
    ///
    ///         Immutable because it is the whole trust rule: a registry that
    ///         could be repointed at another factory could list contracts that
    ///         no rub3 factory ever deployed, which is the only thing a listing
    ///         here asserts.
    address public immutable factory;

    /// @dev Listings by licence contract address.
    mapping(address => Listing) private _listings;

    /// @dev Every address ever registered, in registration order, so a reader
    ///      can enumerate without replaying logs - the same reason
    ///      {Rub3Factory} keeps its `deployments` list. Delisting never removes
    ///      an entry from here; it changes that entry's {Listing-status}.
    address[] private _registered;

    /// @dev ERC-20s the owner has recognised. `address(0)` is never a key: the
    ///      native rail's recognition is a rule rather than a setting, and is
    ///      answered by {isRecognisedToken} directly.
    mapping(address => bool) private _recognisedToken;

    /// @dev Currently recognised ERC-20s, so the policy can be read rather than
    ///      probed one address at a time. Maintained as a set: recognising a
    ///      token appends, un-recognising swaps the last element into its place.
    address[] private _recognisedTokens;

    /// @dev `token => index + 1` into {_recognisedTokens}, so `0` means absent.
    mapping(address => uint256) private _recognisedTokenIndex;

    /// @notice An application was listed for the first time.
    ///
    ///         `registrant` is whoever called, which is the licence contract's
    ///         owner at that moment and need not be its owner later. It is
    ///         recorded in the log and deliberately not in storage: authority
    ///         over this listing is `Rub3License.owner()` read live, and a
    ///         stored copy could only ever disagree with it.
    event Registered(
        address indexed license,
        address indexed registrant,
        string appName,
        string contentURI
    );

    /// @notice A listing's presentation fields changed. The licence contract,
    ///         and everything read off it, are untouched.
    event ListingUpdated(address indexed license, string appName, string contentURI);

    /// @notice The licence contract's owner withdrew their own listing.
    ///         Discovery only - see the contract docs.
    event Delisted(address indexed license);

    /// @notice The licence contract's owner put their listing back.
    event Relisted(address indexed license);

    /// @notice This registry withheld the badge from a listing.
    ///
    ///         Discovery only, and permanent in the log even though the state is
    ///         reversible: a curation decision that can be taken back silently
    ///         is not one anybody can audit.
    event Suspended(address indexed license, string reason);

    /// @notice This registry restored a suspended listing. The entry becomes
    ///         visible again only if its owner has it {Status.Listed}.
    event Reinstated(address indexed license);

    /// @notice An ERC-20 started or stopped counting as a recognised rail, which
    ///         moves every entry quoting it between the two ranking groups on
    ///         the next read. Nothing about those contracts changed.
    event TokenRecognitionChanged(address indexed token, bool recognised);

    /// @notice The factory a registry trusts is what every listing here rests
    ///         on, so there is no "no factory" mode.
    error FactoryRequired();

    /// @notice `factory_` cannot answer the two views {isCanonicalDeploy} reads
    ///         off it, so it is not a rub3 factory. Checked at construction, the
    ///         only moment it can still be corrected.
    error IncompatibleFactory(address factory);

    /// @notice The zero address is not a licence contract.
    error LicenseRequired();

    /// @notice `license` was not deployed by {factory} or by any factory
    ///         reachable through {previousFactory} within
    ///         {MAX_FACTORY_GENERATION_HOPS} hops. See {isCanonicalDeploy}.
    error NotCanonicalDeploy(address license);

    /// @notice The caller does not own `license`, so it is not theirs to list.
    error NotLicenseOwner(address license, address owner, address caller);

    /// @notice `license` already has an entry. A withdrawn listing is restored
    ///         with {relist} and edited with {updateListing}; there is no second
    ///         registration.
    error AlreadyRegistered(address license);

    /// @notice `license` has no entry, so there is nothing to change.
    error NotRegistered(address license);

    /// @notice `license` is already in the state the call would put it in.
    error AlreadyInStatus(address license, Status status);

    /// @notice `license` is suspended by this registry, so its owner cannot put
    ///         it back. {reinstate} first.
    error ListingSuspended(address license);

    /// @notice `license` is not suspended, so there is nothing to reinstate.
    error NotSuspended(address license);

    /// @notice The native rail is recognised as a rule rather than as a setting,
    ///         so `address(0)` cannot be passed to {setTokenRecognised} in
    ///         either direction. See the contract docs.
    error NativeRailIsAlwaysRecognised();

    /// @notice `token` is already in the state the call would put it in.
    error TokenAlreadyRecognised(address token, bool recognised);

    /// @notice A text field a listing is read by was left empty.
    error TextRequired(string field);

    /// @notice Ownership here is the right to curate: to maintain the
    ///         recognised-token list as tokens move, and to withhold the badge.
    ///         Handing it to a new key is supported ({Ownable2Step}); handing it
    ///         to nobody would freeze the token list at whatever it happened to
    ///         say, permanently and with no recovery, on a chain where the
    ///         assets it names can be deprecated or migrated.
    error OwnershipCannotBeRenounced();

    /// @param factory_ The canonical {Rub3Factory} whose deploys are listable
    ///                 here, taken from `contracts/deployments.json` for the
    ///                 chain being deployed to. Immutable; see {factory}.
    /// @param owner_   The key allowed to curate. Who that is, and how it is
    ///                 custodied, is a deployment decision and is deliberately
    ///                 not made anywhere in this repository.
    constructor(address factory_, address owner_) Ownable(owner_) {
        if (factory_ == address(0)) revert FactoryRequired();
        if (factory_.code.length == 0) revert IncompatibleFactory(factory_);

        // Probed here for the same reason {Rub3Factory} probes its own
        // `previousFactory`: this pointer is immutable, and an address that
        // cannot answer the walk would reject every registration forever with
        // no way to correct it. Probing both views is also what lets
        // {isCanonicalDeploy} walk the chain without a `try` at every hop - each
        // factory in it validated its own link when it was built, so the chain
        // is well-formed by induction.
        try Rub3Factory(factory_).isDeployed(address(0)) returns (bool) {}
        catch { revert IncompatibleFactory(factory_); }
        try Rub3Factory(factory_).previousFactory() returns (address) {}
        catch { revert IncompatibleFactory(factory_); }

        factory = factory_;
    }

    // ── Reads: what exists, and in what order ────────────────────────────────

    /// @notice Whether `license` was deployed by {factory} or by a factory
    ///         reachable from it through {previousFactory} within
    ///         {MAX_FACTORY_GENERATION_HOPS} hops.
    ///
    ///         This is the whole listability rule, and it is a `view` so a
    ///         developer can check it before spending gas on {register}.
    ///
    ///         **The walk is why there is no second list.** rub3 changes its
    ///         take by deploying a new factory, and the contracts an earlier
    ///         factory recorded must not fall out of discovery when it does. The
    ///         chain those factories already maintain is the record of which
    ///         generations count, so this registry reads it rather than keeping
    ///         a set of its own that somebody would have to remember to extend.
    ///
    /// @dev    Deliberately not {Rub3Factory.isCanonicalPredecessor}, which
    ///         performs the same walk for a different question. That function
    ///         answers "may this be named as a predecessor of a new deploy", it
    ///         returns true for `address(0)`, and its rule belongs to the deploy
    ///         path. Binding discovery to it would mean a future factory
    ///         tightening its predecessor rule silently delisted applications
    ///         here, which is a validity decision reaching into discovery by the
    ///         back door.
    function isCanonicalDeploy(address license) public view returns (bool) {
        if (license == address(0)) return false;
        if (Rub3Factory(factory).isDeployed(license)) return true;

        address current = Rub3Factory(factory).previousFactory();
        for (uint256 hops = 0; hops < MAX_FACTORY_GENERATION_HOPS; hops++) {
            if (current == address(0)) return false;
            if (Rub3Factory(current).isDeployed(license)) return true;
            current = Rub3Factory(current).previousFactory();
        }
        return false;
    }

    /// @notice The stored half of `license`'s entry, or a {Listing} whose
    ///         `status` is {Status.Unknown} when there is none.
    ///
    ///         An unknown address is not an accusation. It means nobody has
    ///         listed that contract here, which is what a directly deployed
    ///         licence looks like and also what a perfectly canonical one whose
    ///         owner never registered it looks like.
    function listing(address license) external view returns (Listing memory) {
        return _listings[license];
    }

    /// @notice Whether `license` currently carries the badge: listed by its
    ///         owner and not suspended by this registry.
    ///
    ///         This is the only place the two flags are combined, so a caller
    ///         never has to know that both exist.
    function isListed(address license) public view returns (bool) {
        Listing storage entry = _listings[license];
        return entry.status == Status.Listed && !entry.suspended;
    }

    /// @notice Every address ever registered, in registration order, listed or
    ///         not. Only grows.
    ///
    ///         **Unbounded, and it is the caller's job to know that.**
    ///         Registration is permissionless for anyone holding a factory
    ///         deploy and nothing is ever removed, so this response grows
    ///         without limit and eventually stops fitting a node's `eth_call`
    ///         budget. That is what an indexer walking the whole set wants and
    ///         what a caller with a deadline must not use; {registeredWindow} is
    ///         the bounded form.
    function registered() external view returns (address[] memory) {
        return _registered;
    }

    /// @notice At most `count` registered addresses starting at `start`, in
    ///         registration order.
    ///
    ///         The bounded form of {registered}: the work and the response are
    ///         both the caller's `count`, never the size of the set.
    ///
    ///         Clamped rather than strict, so one call is enough and no reader
    ///         needs {registeredCount} first: a `start` past the end returns
    ///         nothing and a `count` past the end returns what is left.
    function registeredWindow(uint256 start, uint256 count)
        public
        view
        returns (address[] memory window)
    {
        uint256 total = _registered.length;
        if (start >= total) return new address[](0);

        uint256 available = total - start;
        uint256 taken = count < available ? count : available;
        window = new address[](taken);
        for (uint256 i = 0; i < taken; i++) {
            window[i] = _registered[start + i];
        }
    }

    /// @notice How many addresses {registered} would return.
    function registeredCount() external view returns (uint256) {
        return _registered.length;
    }

    /// @notice The address registered at `index`, in registration order.
    function registeredAt(uint256 index) external view returns (address) {
        return _registered[index];
    }

    /// @notice The payment token `license` currently quotes, or `address(0)`
    ///         when it sells for ETH only.
    ///
    ///         Exposed so the ranking below can be checked against its own
    ///         input: an entry's group is a pure function of this answer and
    ///         {isRecognisedToken}, and both are readable.
    ///
    /// @dev    A licence contract that cannot answer at all is read as ETH only,
    ///         which is the same rule the wrapper applies - `priceToken()`
    ///         returning zero *or reverting* means "no token rail" (see
    ///         {Rub3License-priceToken}). Every contract listable here is a
    ///         factory deploy whose getter cannot revert, so this is a
    ///         consistency guarantee rather than a live path: what it rules out
    ///         is one unreachable entry making the whole of {rankedListings}
    ///         revert.
    function priceTokenOf(address license) public view returns (address) {
        try Rub3License(license).priceToken() returns (address token) {
            return token;
        } catch {
            return address(0);
        }
    }

    /// @notice Whether `token` counts as a recognised rail.
    ///
    ///         True for `address(0)`, the native rail, always and unconditionally
    ///         - see {setTokenRecognised}. True for an ERC-20 the owner has
    ///         recognised. False otherwise.
    ///
    /// @dev    A function rather than a public mapping precisely so there is one
    ///         answer to this question. A `mapping(address => bool) public`
    ///         would report `false` for `address(0)` while every other read here
    ///         treated the native rail as recognised, and the disagreement would
    ///         only surface in whichever caller happened to ask the mapping.
    function isRecognisedToken(address token) public view returns (bool) {
        return token == address(0) || _recognisedToken[token];
    }

    /// @notice Whether `license`'s current quote puts it in the upper ranking
    ///         group.
    ///
    ///         Read live on every call, which is the point - see the contract
    ///         docs. An entry that switches its quote with
    ///         `setTokenPrice(address,uint256)` moves group on the very next
    ///         read, with no action required here and no snapshot to go stale.
    function isRecognisedRail(address license) public view returns (bool) {
        return isRecognisedToken(priceTokenOf(license));
    }

    /// @notice Every ERC-20 the owner currently recognises, in no meaningful
    ///         order.
    ///
    ///         The native rail is not in here and never will be: it is
    ///         recognised by rule rather than by membership, so a caller reading
    ///         this list as "the assets that rank" must add ETH to it. That is
    ///         what {isRecognisedToken} is for.
    function recognisedTokens() external view returns (address[] memory) {
        return _recognisedTokens;
    }

    /// @notice How many ERC-20s {recognisedTokens} would return.
    function recognisedTokenCount() external view returns (uint256) {
        return _recognisedTokens.length;
    }

    /// @notice Every listed application, ranked: entries quoting a recognised
    ///         rail first, then entries that do not, each group in registration
    ///         order.
    ///
    ///         Stable within a group on purpose. The recognised-token list is
    ///         the only judgement this registry applies, and inventing a second
    ///         ordering inside a group - by price, by age, by anything - would
    ///         be a ranking policy nobody asked for and nobody could audit.
    ///
    ///         **Unbounded in work as well as in response.** Each entry costs
    ///         one `priceToken()` read taken at call time, and this scans every
    ///         registered address to produce a globally correct order. That is
    ///         what an indexer wants and what a caller with a deadline must not
    ///         use: registration is permissionless and nothing is ever removed,
    ///         so the cost of this call grows without limit.
    ///         {rankedRegistrationWindow} is the form whose *work* is bounded.
    ///         {rankedListingWindow} is not - it bounds only the response, and
    ///         its doc says so.
    function rankedListings() public view returns (address[] memory ranked) {
        uint256 total = _registered.length;

        // One pass to read every quote, so the partition below costs no further
        // external calls. Reading each contract twice would be both slower and,
        // worse, capable of producing a self-inconsistent order if a quote could
        // move between the two passes.
        bool[] memory recognised = new bool[](total);
        uint256 listedCount;
        uint256 recognisedCount;
        for (uint256 i = 0; i < total; i++) {
            address license = _registered[i];
            if (!isListed(license)) continue;
            listedCount++;
            if (isRecognisedRail(license)) {
                recognised[i] = true;
                recognisedCount++;
            }
        }

        ranked = new address[](listedCount);
        uint256 top;
        uint256 bottom = recognisedCount;
        for (uint256 i = 0; i < total; i++) {
            address license = _registered[i];
            if (!isListed(license)) continue;
            if (recognised[i]) {
                ranked[top++] = license;
            } else {
                ranked[bottom++] = license;
            }
        }
    }

    /// @notice At most `count` listed applications starting at `start`, in the
    ///         order {rankedListings} returns them.
    ///
    ///         **This bounds the response, not the cost.** `start` and `count`
    ///         index into the global ranking, so producing them means computing
    ///         that ranking first: this call reads every registered entry's
    ///         quote no matter how small a page is asked for, and costs exactly
    ///         what {rankedListings} costs. It is a globally correct page, and
    ///         it is not a defence against the set growing.
    ///
    ///         A caller that needs its cost bounded wants
    ///         {rankedRegistrationWindow}, which pays for a globally correct
    ///         order by not providing one. Both exist because that trade is
    ///         real and neither answer is right for everybody: pick the one
    ///         whose failure mode you can live with.
    ///
    ///         Clamped rather than strict, so a reader needs no second call to
    ///         find out how many there are: a `start` past the end returns
    ///         nothing and a `count` past the end returns what is left.
    ///
    /// @dev    The rank is computed over the whole set before the window is cut,
    ///         because a page of a ranking that was only ranked within the page
    ///         is not a page of that ranking.
    function rankedListingWindow(uint256 start, uint256 count)
        public
        view
        returns (address[] memory window)
    {
        address[] memory ranked = rankedListings();
        if (start >= ranked.length) return new address[](0);

        uint256 available = ranked.length - start;
        uint256 taken = count < available ? count : available;
        window = new address[](taken);
        for (uint256 i = 0; i < taken; i++) {
            window[i] = ranked[start + i];
        }
    }

    /// @notice The listed applications among the `count` addresses registered
    ///         from position `start`, ranked **within that window**: entries
    ///         quoting a recognised rail first, then entries that do not, each
    ///         group in registration order.
    ///
    ///         The read whose cost a caller controls. `start` and `count` are a
    ///         cursor over {registered} - registration order, the one ordering
    ///         this contract stores - so this scans at most `count` entries and
    ///         makes at most one `priceToken()` read per listed entry among
    ///         them. Nothing it costs depends on how large the registry has
    ///         grown, which is what makes it the read a purchase path and a
    ///         listing page can keep using.
    ///
    ///         **The order is local to the window, and cannot be assembled into
    ///         a global one.** Reading this is not a page of {rankedListings}
    ///         and paging through it does not reconstruct that list: an entry
    ///         quoting an unrecognised rail in an early window still comes back
    ///         before a recognised entry from a later one, because no window can
    ///         know what the others hold without reading them. A caller that
    ///         needs a globally correct order has two honest options and no
    ///         third: pay for it with {rankedListings} or {rankedListingWindow},
    ///         or collect the windows and rank them itself off-chain, where
    ///         {isRecognisedRail} gives it the same input this uses.
    ///
    ///         The returned array is shorter than `count` whenever the window
    ///         contains delisted, suspended or never-listed entries, so its
    ///         length says nothing about how far the cursor moved. Advance by
    ///         `count`, not by `window.length`.
    ///
    ///         Clamped rather than strict, exactly like {registeredWindow}: a
    ///         `start` past the end returns nothing and a `count` past the end
    ///         stops at the end, so one call is enough and no caller needs
    ///         {registeredCount} first.
    function rankedRegistrationWindow(uint256 start, uint256 count)
        public
        view
        returns (address[] memory window)
    {
        uint256 total = _registered.length;
        if (start >= total) return new address[](0);

        uint256 available = total - start;
        uint256 scanned = count < available ? count : available;

        // One pass to read every quote in the window, for the same reason
        // {rankedListings} does it: reading a contract twice could otherwise
        // produce a self-inconsistent order if a quote moved between passes.
        bool[] memory recognised = new bool[](scanned);
        uint256 listedCount;
        uint256 recognisedCount;
        for (uint256 i = 0; i < scanned; i++) {
            address license = _registered[start + i];
            if (!isListed(license)) continue;
            listedCount++;
            if (isRecognisedRail(license)) {
                recognised[i] = true;
                recognisedCount++;
            }
        }

        window = new address[](listedCount);
        uint256 top;
        uint256 bottom = recognisedCount;
        for (uint256 i = 0; i < scanned; i++) {
            address license = _registered[start + i];
            if (!isListed(license)) continue;
            if (recognised[i]) {
                window[top++] = license;
            } else {
                window[bottom++] = license;
            }
        }
    }

    /// @notice The full agent card for `license`, assembled from live reads.
    ///
    ///         Answers for an unregistered address too, with
    ///         `status == Status.Unknown` and the stored fields empty. That is
    ///         deliberate: "this contract exists and is not listed here" is a
    ///         useful answer to an agent holding an address from somewhere else,
    ///         and refusing it would push that agent back onto whatever it was
    ///         told the price was.
    ///
    ///         Bounded work: {MAX_CARD_WRAPPER_HASHES} caps the hash set, so no
    ///         listing's own published history can decide what one of these
    ///         costs. The cap is reported next to the true total rather than
    ///         applied silently - see {AgentCard-wrapperHashCount}.
    ///
    /// @dev    Reverts if `license` is not a licence contract, because every
    ///         field below one is read off it. Card assembly is not the place to
    ///         guess at a contract that cannot answer.
    function card(address license) public view returns (AgentCard memory) {
        Listing storage entry = _listings[license];
        Rub3License lic = Rub3License(license);

        // Deliberately not `wrapperHashList()`: that returns the whole set, and
        // the point of the cap is that this call's cost does not follow it.
        uint256 hashCount = lic.wrapperHashCount();
        uint256 taken = hashCount < MAX_CARD_WRAPPER_HASHES ? hashCount : MAX_CARD_WRAPPER_HASHES;
        uint256 skipped = hashCount - taken;

        WrapperHash[] memory published = new WrapperHash[](taken);
        for (uint256 i = 0; i < taken; i++) {
            bytes32 hash = lic.wrapperHashAt(skipped + i);
            published[i] = WrapperHash({hash: hash, status: uint8(lic.wrapperHashes(hash))});
        }

        address token = priceTokenOf(license);

        return AgentCard({
            license:           license,
            licenseOwner:      lic.owner(),
            appName:           entry.appName,
            contentURI:        entry.contentURI,
            status:            entry.status,
            suspended:         entry.suspended,
            listed:            isListed(license),
            price:             lic.price(),
            priceToken:        token,
            priceAmount:       lic.priceAmount(),
            recognisedRail:    isRecognisedToken(token),
            identityModel:     lic.identityModel(),
            tbaImplementation: lic.tbaImplementation(),
            feeBps:            lic.feeBps(),
            treasury:          lic.treasury(),
            wrapperHashes:          published,
            wrapperHashCount:       hashCount,
            wrapperHashesTruncated: skipped > 0,
            registeredAtBlock:      entry.registeredAtBlock
        });
    }

    /// @notice At most `count` agent cards starting at `start`, in the order
    ///         {rankedListings} returns them: a **globally** ranked page of
    ///         allowlistable contracts with the terms an agent's spend policy
    ///         needs to decide.
    ///
    ///         **This bounds the response, not the cost**, because
    ///         {rankedListingWindow} does not: the global rank is computed over
    ///         every registered entry before the page is cut, so the smallest
    ///         page here costs what the whole set costs. Card assembly itself is
    ///         bounded per entry ({MAX_CARD_WRAPPER_HASHES}), which is a
    ///         different bound and does not rescue this one.
    ///
    ///         An agent that must not have its cost decided by how many
    ///         applications strangers have registered reads {cardWindow}
    ///         instead, and accepts a window-local order in exchange. Clamped
    ///         exactly like {rankedListingWindow}.
    function cards(uint256 start, uint256 count) external view returns (AgentCard[] memory page) {
        return _cardsFor(rankedListingWindow(start, count));
    }

    /// @notice The agent cards for {rankedRegistrationWindow}`(start, count)`:
    ///         the listed applications among the `count` addresses registered
    ///         from position `start`, ranked within that window.
    ///
    ///         The bounded card read, and the one an agent walking the registry
    ///         should use. Every part of its cost is the caller's: at most
    ///         `count` entries scanned, at most one card assembled per listed
    ///         entry among them, and at most {MAX_CARD_WRAPPER_HASHES} hashes on
    ///         each card. None of the three follows the size of the registry or
    ///         the publishing history of any listing in it.
    ///
    ///         **The ranking is local to the window** and paging through does
    ///         not reconstruct the global order - see
    ///         {rankedRegistrationWindow}, which says exactly what that costs
    ///         and what it does not.
    ///
    ///         Clamped, and the returned page is shorter than `count` whenever
    ///         the window holds entries that are not listed, so advance the
    ///         cursor by `count` rather than by `page.length`.
    function cardWindow(uint256 start, uint256 count)
        external
        view
        returns (AgentCard[] memory page)
    {
        return _cardsFor(rankedRegistrationWindow(start, count));
    }

    /// @dev One card per address, in the order given.
    function _cardsFor(address[] memory window) private view returns (AgentCard[] memory page) {
        page = new AgentCard[](window.length);
        for (uint256 i = 0; i < window.length; i++) {
            page[i] = card(window[i]);
        }
    }

    // ── Writes by the licence contract's owner ───────────────────────────────

    /// @notice List `license` for discovery.
    ///
    ///         Requires that a canonical factory deployed it
    ///         ({isCanonicalDeploy}) and that the caller owns it right now. Both
    ///         are read live; neither is recorded, because a recorded copy of
    ///         either could disagree with the chain later.
    ///
    /// @param license    The licence contract to list.
    /// @param appName    Human-readable name. Presentation, never security.
    ///                   Required: a listing nobody can name is not a listing.
    /// @param contentURI Where the wrapped binary lives. Held here until §3.1
    ///                   puts it on the licence contract, and may be empty for
    ///                   an application that has published nothing yet.
    function register(address license, string calldata appName, string calldata contentURI)
        external
    {
        if (license == address(0)) revert LicenseRequired();
        if (_listings[license].status != Status.Unknown) revert AlreadyRegistered(license);
        if (!isCanonicalDeploy(license)) revert NotCanonicalDeploy(license);
        _requireLicenseOwner(license);
        _requireText(appName, "appName");

        Listing storage entry = _listings[license];
        entry.status = Status.Listed;
        entry.registeredAtBlock = uint64(block.number);
        entry.appName = appName;
        entry.contentURI = contentURI;

        _registered.push(license);

        emit Registered(license, msg.sender, appName, contentURI);
    }

    /// @notice Replace a listing's presentation fields.
    ///
    ///         Works while delisted, so an owner can correct an entry before
    ///         putting it back. Everything else on the card is read off the
    ///         licence contract and is changed there, not here.
    function updateListing(address license, string calldata appName, string calldata contentURI)
        external
    {
        _requireRegistered(license);
        _requireLicenseOwner(license);
        _requireText(appName, "appName");

        Listing storage entry = _listings[license];
        entry.appName = appName;
        entry.contentURI = contentURI;

        emit ListingUpdated(license, appName, contentURI);
    }

    /// @notice Withdraw your own listing.
    ///
    ///         **Discovery only.** Every token this contract has issued stays
    ///         owned, valid, activatable and renewable, and every live session
    ///         stays live. See the contract docs for why that is structural
    ///         rather than a promise.
    ///
    ///         The entry stays in {registered} and keeps its
    ///         {Listing-registeredAtBlock}, so {relist} restores placement
    ///         rather than sending the application to the back of the queue for
    ///         having been away.
    function delist(address license) external {
        _requireRegistered(license);
        _requireLicenseOwner(license);

        Listing storage entry = _listings[license];
        if (entry.status == Status.Delisted) revert AlreadyInStatus(license, Status.Delisted);

        entry.status = Status.Delisted;
        emit Delisted(license);
    }

    /// @notice Put your own listing back.
    ///
    ///         Refuses while this registry has the entry suspended, which is
    ///         what makes a suspension a decision rather than a suggestion.
    ///
    /// @dev    {isCanonicalDeploy} is deliberately not re-checked. It was true
    ///         when the entry was registered and `isDeployed` is write-once on
    ///         every factory in the chain, so it cannot have become false; a
    ///         re-check would only add a way for a relist to fail for a reason
    ///         no caller could act on.
    function relist(address license) external {
        _requireRegistered(license);
        _requireLicenseOwner(license);

        Listing storage entry = _listings[license];
        if (entry.suspended) revert ListingSuspended(license);
        if (entry.status == Status.Listed) revert AlreadyInStatus(license, Status.Listed);

        entry.status = Status.Listed;
        emit Relisted(license);
    }

    // ── Writes by this registry's owner: curation, and nothing else ──────────

    /// @notice Start or stop counting `token` as a recognised rail.
    ///
    ///         This is the judgement the licence contracts deliberately do not
    ///         hold. It lives here rather than in a licence contract precisely
    ///         so it can move as tokens do - deprecated, migrated, or newly
    ///         worth accruing a protocol fee in - without touching anything
    ///         already deployed.
    ///
    ///         **`address(0)` is rejected in both directions.** The native rail
    ///         is recognised by rule: an ETH-only contract quotes no token at
    ///         all and its fee accrues in ETH, so there is no asset here to
    ///         judge. Allowing it as a key would put the entire ETH-only
    ///         population one owner transaction away from the bottom of the
    ///         list, which is a power this registry has no reason to hold.
    ///
    ///         Un-recognising a token demotes every entry quoting it on the next
    ///         read. It takes nothing away from any of them: their tokens, their
    ///         sessions and their renewals are untouched, and their owners can
    ///         quote something else whenever they like.
    function setTokenRecognised(address token, bool recognised) external onlyOwner {
        if (token == address(0)) revert NativeRailIsAlwaysRecognised();
        if (_recognisedToken[token] == recognised) {
            revert TokenAlreadyRecognised(token, recognised);
        }

        _recognisedToken[token] = recognised;
        if (recognised) {
            _recognisedTokens.push(token);
            _recognisedTokenIndex[token] = _recognisedTokens.length;
        } else {
            uint256 index = _recognisedTokenIndex[token] - 1;
            uint256 last = _recognisedTokens.length - 1;
            if (index != last) {
                address moved = _recognisedTokens[last];
                _recognisedTokens[index] = moved;
                _recognisedTokenIndex[moved] = index + 1;
            }
            _recognisedTokens.pop();
            _recognisedTokenIndex[token] = 0;
        }

        emit TokenRecognitionChanged(token, recognised);
    }

    /// @notice Withhold the badge from `license`.
    ///
    ///         **Discovery only, and this is the function where that matters
    ///         most.** It hides an entry from {rankedListings} and marks its
    ///         card. It does not, and cannot, touch a token, a session, a
    ///         renewal or a price - nothing in a licence contract reads this
    ///         one. The worst a compromised owner key does here is make the
    ///         discovery surface wrong until it is corrected.
    ///
    ///         Reversible with {reinstate}, unlike {Rub3CodeRegistry.deprecate}.
    ///         The asymmetry is the point: a deprecation describes code an agent
    ///         may already have acted on and so is written once, while a listing
    ///         decision describes only what a shopper is shown next.
    ///
    /// @param reason Why, for the log. Mirrors
    ///               {Rub3License-revokeWrapperHash} and
    ///               {Rub3CodeRegistry-deprecate}: a public curation act carries
    ///               its explanation with it, so the record stays auditable even
    ///               though the state is reversible.
    function suspend(address license, string calldata reason) external onlyOwner {
        _requireRegistered(license);
        _requireText(reason, "reason");

        Listing storage entry = _listings[license];
        if (entry.suspended) revert ListingSuspended(license);

        entry.suspended = true;
        emit Suspended(license, reason);
    }

    /// @notice Lift a suspension.
    ///
    ///         The entry becomes visible again only if its own owner has it
    ///         {Status.Listed}: the two flags are independent, so lifting a
    ///         suspension never overrides an owner who withdrew their listing in
    ///         the meantime.
    function reinstate(address license) external onlyOwner {
        _requireRegistered(license);

        Listing storage entry = _listings[license];
        if (!entry.suspended) revert NotSuspended(license);

        entry.suspended = false;
        emit Reinstated(license);
    }

    /// @inheritdoc Ownable
    /// @dev Always reverts. See {OwnershipCannotBeRenounced}.
    function renounceOwnership() public view override onlyOwner {
        revert OwnershipCannotBeRenounced();
    }

    // ── Internals ────────────────────────────────────────────────────────────

    function _requireRegistered(address license) private view {
        if (_listings[license].status == Status.Unknown) revert NotRegistered(license);
    }

    /// @dev Authority is the licence contract's current owner, read at the
    ///      moment of the call and never cached. Transferring the licence
    ///      contract therefore transfers control of its listing, with nothing to
    ///      update here.
    function _requireLicenseOwner(address license) private view {
        address licenseOwner = Rub3License(license).owner();
        if (licenseOwner != msg.sender) {
            revert NotLicenseOwner(license, licenseOwner, msg.sender);
        }
    }

    function _requireText(string calldata value, string memory field) private pure {
        if (bytes(value).length == 0) revert TextRequired(field);
    }
}
