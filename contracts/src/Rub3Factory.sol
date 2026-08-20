// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Rub3Access} from "./Rub3Access.sol";
import {Rub3License} from "./Rub3License.sol";
import {Rub3Subscription} from "./Rub3Subscription.sol";

/// @notice Everything a license contract needs at deploy except its economics,
///         which the factory stamps rather than accepts.
///
/// The fee terms are deliberately *not* in here. A caller who could name them
/// would be naming rub3's revenue, so `feeBps` and `treasury` come from the
/// factory's own immutables and from nowhere else - see {Rub3Factory}.
struct Rub3LicenseParams {
    string name;
    string symbol;
    Rub3License.IdentityTerms identity;
    bytes32[] wrapperHashes;
    Rub3License.SaleTerms sale;
    uint256 supplyCap;
    uint256 cooldownBlocks;
    address predecessor;
    /// Contract owner. `address(0)` means the caller, which is the common case.
    address owner;
}

/// @notice Deploys {Rub3Access}. Split out of {Rub3Factory} for one reason: a
///         contract's runtime code has to carry the creation code of everything
///         it can `new`, and the two license contracts together are over 30 KB -
///         comfortably past the 24,576-byte runtime limit, so one contract
///         cannot hold both.
///
/// A `new` reached only from a *constructor* lands in the creation code instead,
/// which is discarded after deployment. {Rub3Factory} therefore builds one of
/// each of these in its own constructor and keeps the addresses as immutables:
/// the factory's runtime stays small, and which license implementation it
/// deploys is fixed by the same transaction that created it.
///
/// **Callable by anyone, and that is not a hole.** Calling it directly yields a
/// perfectly good license contract that {Rub3Factory} never recorded, which is
/// exactly what deploying the open-source template directly already gets you.
/// Trust comes from `isDeployed`, never from having been created by this
/// address, so there is nothing here for access control to protect.
contract Rub3AccessDeployer {
    function deploy(Rub3LicenseParams memory params, Rub3License.FeeTerms memory fee)
        external
        returns (address license)
    {
        return address(
            new Rub3Access(
                params.name,
                params.symbol,
                params.identity,
                params.wrapperHashes,
                params.sale,
                fee,
                params.supplyCap,
                params.cooldownBlocks,
                params.predecessor,
                params.owner
            )
        );
    }
}

/// @notice Deploys {Rub3Subscription}. The subscription half of
///         {Rub3AccessDeployer}; see it for why the split exists.
contract Rub3SubscriptionDeployer {
    function deploy(
        Rub3LicenseParams memory params,
        Rub3License.FeeTerms memory fee,
        uint256 period
    ) external returns (address license) {
        return address(
            new Rub3Subscription(
                params.name,
                params.symbol,
                params.identity,
                params.wrapperHashes,
                params.sale,
                fee,
                params.supplyCap,
                period,
                params.cooldownBlocks,
                params.predecessor,
                params.owner
            )
        );
    }
}

/// @notice The canonical deployment path for rub3 license contracts, and the
///         protocol's revenue mechanism (implementation.md §2.3).
///
/// It does two things, and they are the same thing seen from two sides:
///
/// 1. **It stamps the economics.** Every contract it deploys is constructed with
///    this factory's `feeBps` and `treasury`, which become `immutable` on the
///    license contract. From then on the split runs on-chain inside `purchase()`
///    and `renew()` on both payment rails - see {Rub3License-_accrueFee}.
/// 2. **It records what it deployed.** `isDeployed` is the registry's and the
///    marketplace's whole trust rule (§3.2, §4.3): they list what this mapping
///    records and nothing else.
///
/// # What immutability means here, precisely
///
/// **A developer's economics can never change after their contract is
/// deployed.** Not by them, not by rub3, not by this factory. There is no setter
/// on either side: `feeBps` and `treasury` are `immutable` on this contract *and*
/// on every contract it deploys, so the terms a developer read before deploying
/// are the terms that contract has for as long as it exists. rub3 changing its
/// take means deploying a *new factory* at a new address, which affects contracts
/// deployed by that new factory and nothing that already exists.
///
/// This factory holds no owner, no admin, and no privileged caller. It cannot
/// touch a contract it deployed - the license contract's owner is the developer,
/// and `isDeployed` is write-once from `deployAccess` / `deploySubscription`.
/// Un-recording a deployment is not implemented, because a listing that could be
/// withdrawn is a revocation surface pointed at the registry.
///
/// # Deploying directly is still fine
///
/// The templates are open source and nothing here penalises going around it: a
/// direct deploy passes `FeeTerms(0, address(0))` and pays no fee at all. What it
/// does not get is a row in `isDeployed`, so it is not listable in the registry
/// or the marketplace. The fee buys distribution, verification, and liquidity.
///
/// # A factory deploy may only succeed a canonical predecessor
///
/// `claimFromPredecessor` is free by design, because migration must never be
/// taxed. That makes an unconstrained `predecessor` a way to launder an entire
/// sale through the registry: sell every licence on a fee-free direct deploy,
/// then deploy a successor *through this factory* naming it as predecessor, and
/// every holder claims onto a fee-bearing, `isDeployed`-listed contract with the
/// treasury never paid. So this factory deploys only successors to contracts a
/// rub3 factory already recorded - see {isCanonicalPredecessor}.
///
/// The check lives here and **not** on the deployer helpers. They are
/// permissionless and record nothing, so a licence they produce has no
/// `isDeployed` row and none of the standing the laundering route was after;
/// constraining them would restrict a path that is already equivalent to
/// deploying the template yourself. The rule guards the registry row, so it
/// belongs where the registry row is granted.
contract Rub3Factory {
    /// @notice Lower bound on a factory's fee, in basis points (2.00%).
    ///         implementation.md §2.3 fixes the protocol's range at 200-300; the
    ///         exact rate within it is chosen per factory deploy.
    uint16 public constant MIN_FEE_BPS = 200;

    /// @notice Upper bound on a factory's fee, in basis points (3.00%). A
    ///         constant, not a parameter: it is the promise that bounds what any
    ///         rub3 factory can ever charge, and it is checked in the
    ///         constructor where the rate is still choosable.
    uint16 public constant MAX_FEE_BPS = 300;

    /// @notice How many *earlier* factories {isCanonicalPredecessor} consults
    ///         beyond this one, walking {previousFactory}. Nine registries in
    ///         total are therefore reachable: this factory and the eight
    ///         generations before it.
    ///
    ///         The walk is bounded because it is an unbounded loop on the deploy
    ///         path otherwise, and the chain's length is decided by whoever
    ///         deploys the factories rather than by this contract. Eight
    ///         generations is far past any plausible rate change; a chain longer
    ///         than that stops recognising its oldest registries, and a
    ///         predecessor there migrates onto a directly deployed successor
    ///         instead - see contracts.md -> "Migrating holders to a new
    ///         contract".
    uint256 public constant MAX_PREDECESSOR_FACTORY_HOPS = 8;

    /// @notice The factory this one supersedes, or `address(0)` for the first.
    ///         Set once, at construction, and never rewritten: a factory that
    ///         could repoint its chain could grant a laundered contract standing
    ///         after the fact.
    ///
    ///         rub3 changes its take by deploying a *new* factory, so the
    ///         contracts an earlier factory recorded must stay migratable onto
    ///         the new one. This pointer is what carries that recognition
    ///         forward; {isCanonicalPredecessor} walks it.
    address public immutable previousFactory;

    /// @notice Protocol fee stamped into every contract this factory deploys, in
    ///         basis points. Frozen for this factory version.
    uint16 public immutable feeBps;

    /// @notice Fee recipient stamped into every contract this factory deploys.
    address public immutable treasury;

    /// @notice The {Rub3AccessDeployer} this factory created for itself. Public
    ///         so an auditor can fetch its code and confirm which
    ///         {Rub3Access} implementation this factory actually deploys - the
    ///         factory's own runtime code does not contain it.
    address public immutable accessDeployer;

    /// @notice The {Rub3SubscriptionDeployer} counterpart of {accessDeployer}.
    address public immutable subscriptionDeployer;

    /// @notice True for license contracts this factory deployed. The registry's
    ///         and the marketplace's entire trust rule; write-once, never
    ///         cleared.
    mapping(address => bool) public isDeployed;

    /// @dev Insertion-ordered list of every deployment, so an agent can
    ///      enumerate the canonical set without replaying logs - the same reason
    ///      {Rub3License} keeps `_wrapperHashList`.
    address[] private _deployments;

    /// @notice A license contract was deployed and recorded. `model` is
    ///         `0` for {Rub3Access} and `1` for {Rub3Subscription}, matching the
    ///         order of the two deploy functions.
    ///
    ///         The fee terms are logged with each deployment because they are
    ///         what that contract is frozen at: a later factory version charging
    ///         something else does not change this row.
    event LicenseDeployed(
        address indexed license,
        address indexed owner,
        address indexed deployer,
        uint8 model,
        uint16 feeBps,
        address treasury
    );

    error FeeBpsOutOfRange(uint16 feeBps, uint16 minimum, uint16 maximum);
    error TreasuryRequired();

    /// @notice `params.predecessor` was not deployed by this factory or by any
    ///         factory reachable through {previousFactory}. See
    ///         {isCanonicalPredecessor}.
    error PredecessorNotCanonical(address predecessor);

    /// @notice `previousFactory_` cannot answer the two views the chain walk
    ///         reads off it, so it is not a rub3 factory.
    error IncompatiblePreviousFactory(address previousFactory);

    /// @param feeBps_          Protocol fee in basis points. Must be within
    ///                         [{MIN_FEE_BPS}, {MAX_FEE_BPS}].
    /// @param treasury_        Fee recipient. Must be set: a fee with nowhere to
    ///                         go would strand every buyer's money in the license
    ///                         contract, and the terms are frozen from here.
    /// @param previousFactory_ The factory this one supersedes, or `address(0)`
    ///                         for the first factory. Its deployments become
    ///                         acceptable predecessors here, and so do those of
    ///                         the factories it in turn supersedes, up to
    ///                         {MAX_PREDECESSOR_FACTORY_HOPS}.
    constructor(uint16 feeBps_, address treasury_, address previousFactory_) {
        if (feeBps_ < MIN_FEE_BPS || feeBps_ > MAX_FEE_BPS) {
            revert FeeBpsOutOfRange(feeBps_, MIN_FEE_BPS, MAX_FEE_BPS);
        }
        if (treasury_ == address(0)) revert TreasuryRequired();

        // `previousFactory` is immutable and every deploy through this factory
        // walks it, so an address that cannot answer the walk would revert every
        // deploy that names a predecessor - checked here, the only moment it can
        // still be corrected, exactly as {Rub3License} probes its predecessor.
        // Probing both views is what makes the whole chain well-formed by
        // induction: each factory validated its own link when it was built.
        if (previousFactory_ != address(0)) {
            if (previousFactory_.code.length == 0) {
                revert IncompatiblePreviousFactory(previousFactory_);
            }
            try Rub3Factory(previousFactory_).isDeployed(address(0)) returns (bool) {}
            catch {
                revert IncompatiblePreviousFactory(previousFactory_);
            }
            try Rub3Factory(previousFactory_).previousFactory() returns (address) {}
            catch {
                revert IncompatiblePreviousFactory(previousFactory_);
            }
        }

        feeBps = feeBps_;
        treasury = treasury_;
        previousFactory = previousFactory_;

        // Created here rather than passed in, so the implementations this
        // factory deploys are settled by the transaction that created it and
        // cannot be substituted afterwards or mis-supplied at construction. The
        // `new` sits in a constructor, so their creation code lives in this
        // factory's initcode and never in its runtime code.
        accessDeployer = address(new Rub3AccessDeployer());
        subscriptionDeployer = address(new Rub3SubscriptionDeployer());
    }

    /// @notice Deploy a {Rub3Access} carrying this factory's fee terms, and
    ///         record it.
    /// @dev    `params.owner` of `address(0)` means `msg.sender`.
    ///         `params.predecessor` must satisfy {isCanonicalPredecessor}.
    function deployAccess(Rub3LicenseParams calldata params) external returns (address license) {
        _requireCanonicalPredecessor(params.predecessor);
        license = Rub3AccessDeployer(accessDeployer).deploy(_withOwner(params), _fee());
        _record(license, 0);
    }

    /// @notice Deploy a {Rub3Subscription} carrying this factory's fee terms,
    ///         and record it.
    /// @param  period Subscription length in seconds. Immutable on the deployed
    ///         contract, like every other renewal term.
    /// @dev    `params.owner` of `address(0)` means `msg.sender`.
    ///         `params.predecessor` must satisfy {isCanonicalPredecessor}.
    function deploySubscription(Rub3LicenseParams calldata params, uint256 period)
        external
        returns (address license)
    {
        _requireCanonicalPredecessor(params.predecessor);
        license = Rub3SubscriptionDeployer(subscriptionDeployer)
            .deploy(_withOwner(params), _fee(), period);
        _record(license, 1);
    }

    /// @notice Number of contracts this factory has deployed.
    function deploymentCount() external view returns (uint256) {
        return _deployments.length;
    }

    /// @notice Deployment at `index`, in the order they were deployed.
    function deploymentAt(uint256 index) external view returns (address) {
        return _deployments[index];
    }

    /// @notice Every contract this factory has deployed, in order.
    function deployments() external view returns (address[] memory) {
        return _deployments;
    }

    /// @notice Whether `predecessor` may be named as the predecessor of a
    ///         contract deployed through this factory. True for `address(0)`
    ///         (no migrations accepted), for anything in this factory's
    ///         `isDeployed`, and for anything in the `isDeployed` of a factory
    ///         reachable through {previousFactory} within
    ///         {MAX_PREDECESSOR_FACTORY_HOPS} hops.
    ///
    ///         Call it before deploying: it is the whole rule, and it is the
    ///         only reason `deployAccess` / `deploySubscription` revert with
    ///         {PredecessorNotCanonical}.
    /// @dev    A developer whose predecessor is not canonical - a pre-factory
    ///         contract, or one deployed directly - can still deploy the
    ///         successor directly and migrate holders onto it. What the factory
    ///         path will not do is put a registry row at the far end of a
    ///         fee-free sale.
    function isCanonicalPredecessor(address predecessor) public view returns (bool) {
        if (predecessor == address(0)) return true;
        if (isDeployed[predecessor]) return true;

        address factory = previousFactory;
        for (uint256 hops = 0; hops < MAX_PREDECESSOR_FACTORY_HOPS; hops++) {
            if (factory == address(0)) return false;
            if (Rub3Factory(factory).isDeployed(predecessor)) return true;
            factory = Rub3Factory(factory).previousFactory();
        }
        return false;
    }

    /// @dev This factory's terms, in the shape the license constructor takes.
    function _fee() private view returns (Rub3License.FeeTerms memory) {
        return Rub3License.FeeTerms({feeBps: feeBps, treasury: treasury});
    }

    /// @dev Resolve the owner default. Done here rather than in the license
    ///      contract because `Ownable(address(0))` reverts, so "mine" has to be
    ///      spelled out by the time the constructor runs.
    function _withOwner(Rub3LicenseParams calldata params)
        private
        view
        returns (Rub3LicenseParams memory resolved)
    {
        resolved = params;
        if (resolved.owner == address(0)) resolved.owner = msg.sender;
    }

    /// @dev The laundering guard, run before the licence is built so a rejected
    ///      deploy costs the caller nothing but gas.
    function _requireCanonicalPredecessor(address predecessor) private view {
        if (!isCanonicalPredecessor(predecessor)) revert PredecessorNotCanonical(predecessor);
    }

    function _record(address license, uint8 model) private {
        isDeployed[license] = true;
        _deployments.push(license);
        emit LicenseDeployed(
            license,
            Rub3License(license).owner(),
            msg.sender,
            model,
            feeBps,
            treasury
        );
    }
}
