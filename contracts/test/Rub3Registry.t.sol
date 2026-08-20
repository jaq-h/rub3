// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test}             from "forge-std/Test.sol";
import {Ownable}          from "@openzeppelin/contracts/access/Ownable.sol";

import {Rub3Access}       from "../src/Rub3Access.sol";
import {Rub3CodeRegistry} from "../src/Rub3CodeRegistry.sol";
import {Rub3License}      from "../src/Rub3License.sol";
import {Rub3Registry}     from "../src/Rub3Registry.sol";
import {Rub3Subscription} from "../src/Rub3Subscription.sol";
import {Rub3Factory, Rub3LicenseParams} from "../src/Rub3Factory.sol";
import {MockEIP3009Token} from "./mocks/MockEIP3009Token.sol";

/// @notice Answers `isDeployed` but has no `previousFactory`, so the generation
///         walk could not continue through it.
///
///         The fixture for the constructor probe. A half-factory deploys fine
///         and then rejects every registration afterwards, at a point where the
///         pointer is immutable and can no longer be corrected. Mirrors the
///         `HalfFactory` in `Rub3Factory.t.sol`, which exists for the same
///         reason on the deploy path.
contract HalfFactory {
    mapping(address => bool) public isDeployed;

    function record(address license) external {
        isDeployed[license] = true;
    }
}

/// @notice The discovery registry (implementation.md §3.2).
///
/// **This suite is about `Rub3Registry`, the discovery registry, and not about
/// `Rub3CodeRegistry`.** They are different contracts answering different
/// questions - "which apps exist and which are listable" against "is this
/// bytecode a genuine rub3 release" - and `test_neitherRegistryCanStandInForTheOther`
/// is the guard that keeps them from drifting into each other. `Rub3CodeRegistry`
/// has its own suite in `Rub3CodeRegistry.t.sol`.
///
/// Five claims are under test, in the order they matter:
///
///   1. **Discovery, never validity.** Group 4. Delisting, suspending and
///      un-recognising a payment token are all discovery acts: a held token, its
///      validation, its live session, a fresh activation and a fresh purchase
///      survive every one of them. This is the invariant §2.4 draws for the
///      whole project, tested on the one contract that plausibly could have
///      crossed it.
///   2. **Only canonical deploys are listable, and by their owner only.**
///      Group 2, including the `previousFactory` walk that keeps an older
///      generation's deploys listable and the hop bound that terminates it.
///   3. **The ranking reads the quote live.** Group 5. A contract registered
///      while priced in a recognised token and repriced afterwards ranks on what
///      it quotes now, not on what it quoted then. A snapshot taken at
///      registration passes every other test in this file and fails this one.
///   4. **The recognised-token list is the registry's only judgement, and the
///      native rail is outside it.** Group 6.
///   5. **The card is assembled live.** Group 7. Nothing on it except the two
///      presentation fields is stored here, so a card cannot describe terms the
///      licence contract no longer offers.
contract Rub3RegistryTest is Test {
    bytes32 internal constant WRAPPER_HASH    = keccak256("registry-test-wrapper-v1");
    uint256 internal constant PRICE           = 1 ether;
    uint256 internal constant USDC_PRICE      = 5_000_000; // 5 USDC, 6 decimals
    uint256 internal constant COOLDOWN_BLOCKS = 15;
    uint256 internal constant PERIOD          = 30 days;
    uint16  internal constant FEE_BPS         = 250;

    address internal treasury     = address(0x7EA5);
    address internal developer    = address(0xDE7);
    address internal otherDev     = address(0xDE72);
    address internal registryOwner = address(0xC0FFEE);
    address internal alice        = address(0xA11CE);
    address internal stranger     = address(0x5747A);

    Rub3Factory      internal factory;
    Rub3Registry     internal registry;
    MockEIP3009Token internal usdc;
    MockEIP3009Token internal shiba; // a token the registry does not recognise

    function setUp() public {
        usdc    = new MockEIP3009Token();
        shiba   = new MockEIP3009Token();
        factory = new Rub3Factory(FEE_BPS, treasury, address(0));

        registry = new Rub3Registry(address(factory), registryOwner);

        vm.prank(registryOwner);
        registry.setTokenRecognised(address(usdc), true);

        vm.deal(alice, 100 ether);
        vm.deal(developer, 100 ether);
    }

    // ── Fixtures ─────────────────────────────────────────────────────────────

    function _hashes(bytes32 h) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](1);
        out[0] = h;
    }

    function _identity() internal pure returns (Rub3License.IdentityTerms memory) {
        return Rub3License.IdentityTerms({model: 0, tbaImplementation: address(0)});
    }

    function _sale(address token, uint256 amount)
        internal
        pure
        returns (Rub3License.SaleTerms memory)
    {
        return Rub3License.SaleTerms({price: PRICE, priceToken: token, priceAmount: amount});
    }

    function _params(Rub3License.SaleTerms memory sale)
        internal
        pure
        returns (Rub3LicenseParams memory)
    {
        return Rub3LicenseParams({
            name:           "Rub3 Registry Test",
            symbol:         "R3R",
            identity:       _identity(),
            wrapperHashes:  _hashes(WRAPPER_HASH),
            sale:           sale,
            supplyCap:      0,
            cooldownBlocks: COOLDOWN_BLOCKS,
            predecessor:    address(0),
            owner:          address(0) // defaults to the caller
        });
    }

    /// An access licence deployed through `f`, owned by `owner_`, quoting
    /// `token`/`amount` on the stablecoin rail.
    function _deployThrough(Rub3Factory f, address owner_, address token, uint256 amount)
        internal
        returns (Rub3Access)
    {
        vm.prank(owner_);
        return Rub3Access(f.deployAccess(_params(_sale(token, amount))));
    }

    /// An ETH-only access licence deployed through the canonical factory.
    function _deployEthOnly(address owner_) internal returns (Rub3Access) {
        return _deployThrough(factory, owner_, address(0), 0);
    }

    /// A licence anyone may deploy from the open-source template. Perfectly
    /// valid software, no `isDeployed` row, and therefore not listable.
    function _deployDirect(address owner_) internal returns (Rub3Access) {
        return new Rub3Access(
            "Direct", "DIR", _identity(), _hashes(WRAPPER_HASH),
            _sale(address(0), 0), Rub3License.FeeTerms({feeBps: 0, treasury: address(0)}),
            0, COOLDOWN_BLOCKS, address(0), owner_
        );
    }

    function _register(Rub3Access license, address owner_) internal {
        vm.prank(owner_);
        registry.register(address(license), "Test App", "ipfs://bafyTestApp");
    }

    // ── Group 1: construction ────────────────────────────────────────────────

    function test_constructor_recordsTheFactoryAndOwner() public view {
        assertEq(registry.factory(), address(factory));
        assertEq(registry.owner(), registryOwner);
        assertEq(registry.MAX_FACTORY_GENERATION_HOPS(), 8);
    }

    function test_constructor_rejectsZeroFactory() public {
        vm.expectRevert(Rub3Registry.FactoryRequired.selector);
        new Rub3Registry(address(0), registryOwner);
    }

    function test_constructor_rejectsCodelessFactory() public {
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.IncompatibleFactory.selector, address(0xBEEF))
        );
        new Rub3Registry(address(0xBEEF), registryOwner);
    }

    /// The probe is the whole point: a half-factory would deploy and then refuse
    /// every registration forever, with `factory` immutable.
    function test_constructor_rejectsFactoryThatCannotAnswerTheWalk() public {
        HalfFactory half = new HalfFactory();
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.IncompatibleFactory.selector, address(half))
        );
        new Rub3Registry(address(half), registryOwner);
    }

    /// Curation must stay transferable and must not be abandonable: the
    /// recognised-token list has to move as tokens do.
    function test_ownership_cannotBeRenounced() public {
        vm.prank(registryOwner);
        vm.expectRevert(Rub3Registry.OwnershipCannotBeRenounced.selector);
        registry.renounceOwnership();
        assertEq(registry.owner(), registryOwner);
    }

    function test_ownership_transfersInTwoSteps() public {
        address successorOwner = address(0x5CC0);

        vm.prank(registryOwner);
        registry.transferOwnership(successorOwner);
        assertEq(registry.owner(), registryOwner, "not yet");

        vm.prank(successorOwner);
        registry.acceptOwnership();
        assertEq(registry.owner(), successorOwner);
    }

    // ── Group 2: the register gate ───────────────────────────────────────────

    function test_register_listsACanonicalDeploy() public {
        Rub3Access license = _deployEthOnly(developer);

        vm.expectEmit(true, true, false, true, address(registry));
        emit Rub3Registry.Registered(
            address(license), developer, "Test App", "ipfs://bafyTestApp"
        );
        _register(license, developer);

        assertTrue(registry.isListed(address(license)));
        assertEq(registry.registeredCount(), 1);
        assertEq(registry.registeredAt(0), address(license));

        Rub3Registry.Listing memory entry = registry.listing(address(license));
        assertEq(uint8(entry.status), uint8(Rub3Registry.Status.Listed));
        assertFalse(entry.suspended);
        assertEq(entry.appName, "Test App");
        assertEq(entry.contentURI, "ipfs://bafyTestApp");
        assertEq(entry.registeredAtBlock, uint64(block.number));
    }

    /// The trade the fee-free direct-deploy path makes, stated as a test: the
    /// software works, it is simply not listable.
    function test_register_refusesAContractTheFactoryDidNotDeploy() public {
        Rub3Access direct = _deployDirect(developer);
        assertFalse(registry.isCanonicalDeploy(address(direct)));

        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.NotCanonicalDeploy.selector, address(direct))
        );
        registry.register(address(direct), "Direct App", "ipfs://bafyDirect");
    }

    function test_register_refusesANonOwner() public {
        Rub3Access license = _deployEthOnly(developer);

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.NotLicenseOwner.selector, address(license), developer, stranger
            )
        );
        registry.register(address(license), "Stolen App", "ipfs://bafyStolen");
    }

    /// Authority follows the licence contract's owner without the registry being
    /// told, which is why no `registrant` is stored.
    function test_register_authorityFollowsLicenseOwnership() public {
        Rub3Access license = _deployEthOnly(developer);

        vm.prank(developer);
        Ownable(address(license)).transferOwnership(otherDev);

        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.NotLicenseOwner.selector, address(license), otherDev, developer
            )
        );
        registry.register(address(license), "Test App", "ipfs://bafyTestApp");

        vm.prank(otherDev);
        registry.register(address(license), "Test App", "ipfs://bafyTestApp");
        assertTrue(registry.isListed(address(license)));
    }

    function test_register_refusesZeroAddress() public {
        vm.prank(developer);
        vm.expectRevert(Rub3Registry.LicenseRequired.selector);
        registry.register(address(0), "Nothing", "ipfs://bafyNothing");
    }

    function test_register_refusesTwice() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.AlreadyRegistered.selector, address(license))
        );
        registry.register(address(license), "Test App", "ipfs://bafyTestApp");
    }

    function test_register_requiresAName() public {
        Rub3Access license = _deployEthOnly(developer);

        vm.prank(developer);
        vm.expectRevert(abi.encodeWithSelector(Rub3Registry.TextRequired.selector, "appName"));
        registry.register(address(license), "", "ipfs://bafyTestApp");

        _register(license, developer);
        vm.prank(developer);
        vm.expectRevert(abi.encodeWithSelector(Rub3Registry.TextRequired.selector, "appName"));
        registry.updateListing(address(license), "", "ipfs://bafyTestApp");
    }

    /// An empty `contentURI` means "nothing published yet", which is the honest
    /// state while §3.1 is unbuilt. Requiring the field would only buy a
    /// placeholder that reads like a URI, which is the failure
    /// `contracts/deployments.json` refuses for addresses.
    function test_register_acceptsAnEmptyContentUri() public {
        Rub3Access license = _deployEthOnly(developer);

        vm.prank(developer);
        registry.register(address(license), "Unpublished App", "");

        assertTrue(registry.isListed(address(license)));
        assertEq(registry.card(address(license)).contentURI, "");

        vm.prank(developer);
        registry.updateListing(address(license), "Unpublished App", "ipfs://bafyLater");
        assertEq(registry.card(address(license)).contentURI, "ipfs://bafyLater");
    }

    /// rub3 changes its take by deploying a new factory. The applications the
    /// old one recorded must not fall out of discovery when it does.
    function test_register_honoursAnOlderGenerationsDeploy() public {
        Rub3Access older = _deployEthOnly(developer);

        Rub3Factory generation2 = new Rub3Factory(FEE_BPS, treasury, address(factory));
        Rub3Registry newRegistry = new Rub3Registry(address(generation2), registryOwner);

        assertFalse(generation2.isDeployed(address(older)), "gen 2 did not deploy it");
        assertTrue(
            newRegistry.isCanonicalDeploy(address(older)),
            "a registry on the newer factory must still honour the older generation"
        );

        vm.prank(developer);
        newRegistry.register(address(older), "Older App", "ipfs://bafyOlder");
        assertTrue(newRegistry.isListed(address(older)));
    }

    /// The walk terminates, and where it terminates is the published constant.
    /// Nine generations are reachable: the registry's own factory and the eight
    /// before it.
    function test_register_walkStopsAfterTheHopBound() public {
        Rub3Access oldest = _deployEthOnly(developer);

        Rub3Factory current = factory;
        Rub3Factory[] memory chain = new Rub3Factory[](9);
        for (uint256 i = 0; i < 9; i++) {
            current = new Rub3Factory(FEE_BPS, treasury, address(current));
            chain[i] = current;
        }

        // chain[7] is generation 9, so the first factory sits 8 hops back and is
        // still reachable.
        Rub3Registry reachable = new Rub3Registry(address(chain[7]), registryOwner);
        assertTrue(reachable.isCanonicalDeploy(address(oldest)), "8 hops back is reachable");

        // chain[8] is generation 10, one hop too far.
        Rub3Registry outOfReach = new Rub3Registry(address(chain[8]), registryOwner);
        assertFalse(outOfReach.isCanonicalDeploy(address(oldest)), "9 hops back is not");

        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.NotCanonicalDeploy.selector, address(oldest))
        );
        outOfReach.register(address(oldest), "Ancient App", "ipfs://bafyAncient");
    }

    function test_isCanonicalDeploy_isFalseForZero() public view {
        assertFalse(registry.isCanonicalDeploy(address(0)));
    }

    // ── Group 3: the listing lifecycle ───────────────────────────────────────

    function test_delist_hidesTheEntryAndKeepsTheRecord() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);
        uint64 registeredAt = registry.listing(address(license)).registeredAtBlock;

        vm.expectEmit(true, false, false, false, address(registry));
        emit Rub3Registry.Delisted(address(license));
        vm.prank(developer);
        registry.delist(address(license));

        assertFalse(registry.isListed(address(license)));
        assertEq(registry.rankedListings().length, 0);
        assertEq(registry.registeredCount(), 1, "the record stays");
        assertEq(
            registry.listing(address(license)).registeredAtBlock,
            registeredAt,
            "placement is kept, so relisting is not a demotion"
        );
    }

    function test_relist_restoresTheEntry() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.startPrank(developer);
        registry.delist(address(license));
        registry.relist(address(license));
        vm.stopPrank();

        assertTrue(registry.isListed(address(license)));
    }

    function test_delistAndRelist_refuseANonOwner() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.NotLicenseOwner.selector, address(license), developer, stranger
            )
        );
        registry.delist(address(license));

        vm.prank(developer);
        registry.delist(address(license));

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.NotLicenseOwner.selector, address(license), developer, stranger
            )
        );
        registry.relist(address(license));
    }

    function test_delistAndRelist_refuseARepeatedState() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.AlreadyInStatus.selector, address(license), Rub3Registry.Status.Listed
            )
        );
        registry.relist(address(license));

        vm.startPrank(developer);
        registry.delist(address(license));
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.AlreadyInStatus.selector,
                address(license),
                Rub3Registry.Status.Delisted
            )
        );
        registry.delist(address(license));
        vm.stopPrank();
    }

    function test_updateListing_replacesThePresentationFields() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.expectEmit(true, false, false, true, address(registry));
        emit Rub3Registry.ListingUpdated(address(license), "Renamed", "ipfs://bafyRenamed");
        vm.prank(developer);
        registry.updateListing(address(license), "Renamed", "ipfs://bafyRenamed");

        Rub3Registry.Listing memory entry = registry.listing(address(license));
        assertEq(entry.appName, "Renamed");
        assertEq(entry.contentURI, "ipfs://bafyRenamed");
    }

    /// So an owner can correct an entry before putting it back.
    function test_updateListing_worksWhileDelisted() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.startPrank(developer);
        registry.delist(address(license));
        registry.updateListing(address(license), "Corrected", "ipfs://bafyCorrected");
        vm.stopPrank();

        assertEq(registry.listing(address(license)).appName, "Corrected");
    }

    function test_writes_refuseAnUnregisteredAddress() public {
        Rub3Access license = _deployEthOnly(developer);
        bytes memory expected =
            abi.encodeWithSelector(Rub3Registry.NotRegistered.selector, address(license));

        vm.startPrank(developer);
        vm.expectRevert(expected);
        registry.delist(address(license));
        vm.expectRevert(expected);
        registry.relist(address(license));
        vm.expectRevert(expected);
        registry.updateListing(address(license), "X", "ipfs://x");
        vm.stopPrank();

        vm.startPrank(registryOwner);
        vm.expectRevert(expected);
        registry.suspend(address(license), "because");
        vm.expectRevert(expected);
        registry.reinstate(address(license));
        vm.stopPrank();
    }

    function test_suspend_withholdsTheBadgeAndBlocksRelisting() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.expectEmit(true, false, false, true, address(registry));
        emit Rub3Registry.Suspended(address(license), "impersonates another app");
        vm.prank(registryOwner);
        registry.suspend(address(license), "impersonates another app");

        assertFalse(registry.isListed(address(license)));
        assertTrue(registry.listing(address(license)).suspended);
        assertEq(
            uint8(registry.listing(address(license)).status),
            uint8(Rub3Registry.Status.Listed),
            "the owner's own flag is untouched"
        );

        // A suspension the listing's owner could undo would not be one.
        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.ListingSuspended.selector, address(license))
        );
        registry.relist(address(license));
    }

    function test_suspend_requiresAReasonAndTheRegistryOwner() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, developer)
        );
        registry.suspend(address(license), "no standing");

        vm.prank(registryOwner);
        vm.expectRevert(abi.encodeWithSelector(Rub3Registry.TextRequired.selector, "reason"));
        registry.suspend(address(license), "");
    }

    function test_reinstate_restoresVisibilityOnlyWhenTheOwnerAlsoWantsIt() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.prank(developer);
        registry.delist(address(license));
        vm.prank(registryOwner);
        registry.suspend(address(license), "under review");

        vm.expectEmit(true, false, false, false, address(registry));
        emit Rub3Registry.Reinstated(address(license));
        vm.prank(registryOwner);
        registry.reinstate(address(license));

        assertFalse(
            registry.isListed(address(license)),
            "lifting a suspension must not override an owner who withdrew their listing"
        );

        vm.prank(developer);
        registry.relist(address(license));
        assertTrue(registry.isListed(address(license)));
    }

    function test_reinstate_refusesAnEntryThatIsNotSuspended() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.prank(registryOwner);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.NotSuspended.selector, address(license))
        );
        registry.reinstate(address(license));
    }

    function test_suspend_refusesTwice() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.startPrank(registryOwner);
        registry.suspend(address(license), "under review");
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Registry.ListingSuspended.selector, address(license))
        );
        registry.suspend(address(license), "still under review");
        vm.stopPrank();
    }

    // ── Group 4: discovery, never validity ───────────────────────────────────

    /// The acceptance test for the invariant the whole contract is built around.
    ///
    /// One holder, one paid token, one live session. Then every discovery lever
    /// this registry has is pulled at once - the owner delists, the registry
    /// suspends, and the payment token stops being recognised - and the token,
    /// its validation, its session, its ability to activate again and the
    /// contract's ability to sell another one are all measured afterwards.
    function test_delisting_cannotTouchAHeldTokenOrALiveSession() public {
        vm.prank(developer);
        Rub3Subscription license = Rub3Subscription(
            factory.deploySubscription(_params(_sale(address(usdc), USDC_PRICE)), PERIOD)
        );

        vm.prank(developer);
        registry.register(address(license), "Subscribed App", "ipfs://bafySub");

        vm.prank(alice);
        uint256 tokenId = license.purchase{value: PRICE}(address(0));

        vm.prank(alice);
        uint256 sessionId = license.activate(tokenId);

        assertTrue(registry.isListed(address(license)));
        assertTrue(license.isValid(tokenId));

        // Pull every lever.
        vm.prank(developer);
        registry.delist(address(license));
        vm.prank(registryOwner);
        registry.suspend(address(license), "delisted for the test");
        vm.prank(registryOwner);
        registry.setTokenRecognised(address(usdc), false);

        assertFalse(registry.isListed(address(license)), "the badge is gone");
        assertEq(registry.rankedListings().length, 0, "and so is the listing");

        // Nothing else moved.
        assertEq(license.ownerOf(tokenId), alice, "the token is still owned");
        assertTrue(license.isValid(tokenId), "the token is still valid");
        assertEq(license.activeSessionId(tokenId), sessionId, "the session is still live");
        assertEq(license.expiresAt(tokenId) > block.timestamp, true, "the term is untouched");

        // A fresh activation still works, which is the read a wrapper makes on
        // every launch.
        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        uint256 nextSession = license.activate(tokenId);
        assertGt(nextSession, sessionId);

        // And the contract still sells: delisting is not a pause.
        vm.deal(stranger, 10 ether);
        vm.prank(stranger);
        uint256 secondToken = license.purchase{value: PRICE}(address(0));
        assertEq(license.ownerOf(secondToken), stranger);

        // Renewal, the other thing a licence contract owes a holder over time.
        vm.warp(block.timestamp + 1 days);
        vm.prank(alice);
        license.renew{value: license.renewPrice(tokenId)}(tokenId);
        assertTrue(license.isValid(tokenId));
    }

    /// The same claim from the other side: every registry write leaves the
    /// licence contract's state byte for byte where it was.
    ///
    /// Snapshotted rather than argued, because the interesting failure is a
    /// future edit that gives this registry a non-`view` call into a licence.
    function test_registryWrites_leaveTheLicenseContractUntouched() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        vm.prank(alice);
        uint256 tokenId = license.purchase{value: PRICE}(address(0));
        vm.prank(alice);
        uint256 sessionId = license.activate(tokenId);

        address ownerBefore     = license.ownerOf(tokenId);
        uint256 priceBefore     = license.price();
        address tokenBefore     = license.priceToken();
        uint256 amountBefore    = license.priceAmount();
        uint256 nextIdBefore    = license.nextTokenId();
        address successorBefore = license.successor();
        address licOwnerBefore  = license.owner();
        uint8   hashStatus      = uint8(license.wrapperHashes(WRAPPER_HASH));

        vm.prank(developer);
        registry.updateListing(address(license), "Renamed", "ipfs://bafyRenamed");
        vm.prank(developer);
        registry.delist(address(license));
        vm.prank(registryOwner);
        registry.suspend(address(license), "under review");
        vm.prank(registryOwner);
        registry.reinstate(address(license));
        vm.prank(developer);
        registry.relist(address(license));
        vm.prank(registryOwner);
        registry.setTokenRecognised(address(shiba), true);

        assertEq(license.ownerOf(tokenId), ownerBefore);
        assertEq(license.activeSessionId(tokenId), sessionId);
        assertEq(license.price(), priceBefore);
        assertEq(license.priceToken(), tokenBefore);
        assertEq(license.priceAmount(), amountBefore);
        assertEq(license.nextTokenId(), nextIdBefore);
        assertEq(license.successor(), successorBefore);
        assertEq(license.owner(), licOwnerBefore);
        assertEq(uint8(license.wrapperHashes(WRAPPER_HASH)), hashStatus);
    }

    /// **The structural half of "discovery, never validity", read off the
    /// opcodes.**
    ///
    /// The registry reads a licence contract on every card and every ranking, so
    /// "it cannot touch one" has to be more than a reading of the source. It is:
    /// every external call this contract makes is to a `view`, which solc
    /// compiles to `STATICCALL`, and the EVM refuses any state change under one.
    /// So the deployed runtime code contains no `CALL`, `CALLCODE`,
    /// `DELEGATECALL`, `CREATE`, `CREATE2` or `SELFDESTRUCT` at all, and there
    /// is no opcode left in it that could write to another contract, hold ETH,
    /// deploy anything, or destroy itself.
    ///
    /// This is what turns the invariant from a commitment into a property of the
    /// bytecode, and it is what fails if a future edit gives this contract a
    /// non-`view` call into a licence.
    function test_audit_registryHoldsNoStateChangingExternalCall() public view {
        bytes memory code = address(registry).code;
        assertGt(code.length, 0, "the registry is deployed");
        assertFalse(
            _hasStateChangingCall(code),
            "the discovery registry must reach other contracts through STATICCALL only"
        );
    }

    /// Positive control for the opcode walk above: the licence contracts, which
    /// really do move money, are found to contain a `CALL`.
    ///
    /// Without this, the assertion above would pass just as happily on a walk
    /// that had silently stopped scanning.
    function test_audit_opcodeWalkFindsACallWhereOneExists() public {
        Rub3Access license = _deployEthOnly(developer);
        assertTrue(_hasStateChangingCall(address(license).code), "a licence sends ETH");
        assertFalse(_hasStateChangingCall(address(registry).code), "the registry does not");
    }

    /// The two registries cannot stand in for each other, asserted on their
    /// bytecode rather than left to naming discipline.
    ///
    /// `Rub3Registry` answers "which apps exist and which are listable";
    /// `Rub3CodeRegistry` answers "is this bytecode a genuine rub3 release". A
    /// future edit that gave either one the other's surface would make the
    /// distinction the docs draw untrue, and this is what fails when it does.
    function test_neitherRegistryCanStandInForTheOther() public {
        Rub3CodeRegistry codeRegistry = new Rub3CodeRegistry(registryOwner);

        assertFalse(
            _hasSelector(address(registry), "record(bytes32)"),
            "the discovery registry must not answer the code registry's question"
        );
        assertFalse(
            _hasSelector(
                address(registry),
                "publish(bytes32,uint8,string,string,bytes32,string,(uint32,uint32)[])"
            ),
            "the discovery registry must not publish releases"
        );
        assertFalse(
            _hasSelector(address(codeRegistry), "register(address,string,string)"),
            "the code registry must not list applications"
        );
        assertFalse(
            _hasSelector(address(codeRegistry), "rankedListings()"),
            "the code registry must not rank anything"
        );

        // Positive control: the scanner finds selectors that really are there.
        assertTrue(_hasSelector(address(registry), "rankedListings()"));
        assertTrue(_hasSelector(address(codeRegistry), "record(bytes32)"));
    }

    // ── Group 5: ranking, read live ──────────────────────────────────────────

    /// The ETH-only shape ranks in the upper group: it quotes no token at all,
    /// and its protocol fee accrues in ETH.
    function test_rank_ethOnlyCountsAsRecognised() public {
        Rub3Access ethOnly = _deployEthOnly(developer);
        _register(ethOnly, developer);

        assertEq(registry.priceTokenOf(address(ethOnly)), address(0));
        assertTrue(registry.isRecognisedToken(address(0)));
        assertTrue(registry.isRecognisedRail(address(ethOnly)));
    }

    function test_rank_unrecognisedTokenRailRanksBelow() public {
        Rub3Access recognised   = _deployThrough(factory, developer, address(usdc), USDC_PRICE);
        Rub3Access unrecognised = _deployThrough(factory, otherDev, address(shiba), 1e18);
        Rub3Access ethOnly      = _deployEthOnly(developer);

        _register(recognised, developer);
        _register(unrecognised, otherDev);
        _register(ethOnly, developer);

        assertTrue(registry.isRecognisedRail(address(recognised)));
        assertFalse(registry.isRecognisedRail(address(unrecognised)));

        address[] memory ranked = registry.rankedListings();
        assertEq(ranked.length, 3);
        assertEq(ranked[0], address(recognised), "recognised token rail first");
        assertEq(ranked[1], address(ethOnly), "then the native rail, in registration order");
        assertEq(ranked[2], address(unrecognised), "the unrecognised rail is last");
    }

    /// **The test the whole ranking rule exists for.**
    ///
    /// `setTokenPrice` stays owner-callable for the life of a licence contract,
    /// so a contract registered while priced in a recognised token can switch
    /// afterwards. A rank snapshotted at registration would keep advertising it
    /// on a quote it no longer honours; this asserts the order follows the
    /// chain instead. Both entries swap quotes, so a snapshot implementation
    /// returns the registration order and fails here while passing every other
    /// test in this file.
    function test_rank_followsAPostRegistrationTokenPriceChange() public {
        Rub3Access first  = _deployThrough(factory, developer, address(usdc), USDC_PRICE);
        Rub3Access second = _deployThrough(factory, otherDev, address(shiba), 1e18);

        _register(first, developer);
        _register(second, otherDev);

        address[] memory before = registry.rankedListings();
        assertEq(before[0], address(first), "registered on a recognised quote");
        assertEq(before[1], address(second), "registered on an unrecognised one");

        // Neither contract touches the registry. Both change what they quote.
        vm.prank(developer);
        first.setTokenPrice(address(shiba), 1e18);
        vm.prank(otherDev);
        second.setTokenPrice(address(usdc), USDC_PRICE);

        address[] memory nowRanked = registry.rankedListings();
        assertEq(
            nowRanked[0],
            address(second),
            "the entry that moved onto a recognised quote must rank first now"
        );
        assertEq(
            nowRanked[1],
            address(first),
            "and the entry that moved off one must rank last, on the live quote"
        );

        assertFalse(registry.isRecognisedRail(address(first)));
        assertTrue(registry.isRecognisedRail(address(second)));
        assertFalse(registry.card(address(first)).recognisedRail, "the card follows too");
    }

    /// A contract that switches to the native rail is promoted for the same
    /// reason: `priceToken == address(0)` is the recognised shape.
    function test_rank_followsASwitchToTheNativeRail() public {
        Rub3Access license = _deployThrough(factory, developer, address(shiba), 1e18);
        _register(license, developer);
        assertFalse(registry.isRecognisedRail(address(license)));

        vm.prank(developer);
        license.setTokenPrice(address(0), 0);

        assertTrue(registry.isRecognisedRail(address(license)));
    }

    /// The other direction of "live": recognising a token moves every entry
    /// quoting it, with no write against any of those entries.
    function test_rank_followsTheRecognisedTokenList() public {
        Rub3Access license = _deployThrough(factory, developer, address(shiba), 1e18);
        Rub3Access ethOnly = _deployEthOnly(otherDev);
        _register(license, developer);
        _register(ethOnly, otherDev);

        address[] memory before = registry.rankedListings();
        assertEq(before[0], address(ethOnly));
        assertEq(before[1], address(license));

        vm.prank(registryOwner);
        registry.setTokenRecognised(address(shiba), true);

        address[] memory nowRanked = registry.rankedListings();
        assertEq(nowRanked[0], address(license), "registration order inside the promoted group");
        assertEq(nowRanked[1], address(ethOnly));
    }

    function test_rank_omitsDelistedAndSuspendedEntries() public {
        Rub3Access listed    = _deployEthOnly(developer);
        Rub3Access delisted  = _deployEthOnly(otherDev);
        Rub3Access suspended = _deployThrough(factory, developer, address(usdc), USDC_PRICE);

        _register(listed, developer);
        _register(delisted, otherDev);
        _register(suspended, developer);

        vm.prank(otherDev);
        registry.delist(address(delisted));
        vm.prank(registryOwner);
        registry.suspend(address(suspended), "under review");

        address[] memory ranked = registry.rankedListings();
        assertEq(ranked.length, 1);
        assertEq(ranked[0], address(listed));
    }

    function test_rankedListingWindow_clampsRatherThanReverting() public {
        Rub3Access a = _deployThrough(factory, developer, address(usdc), USDC_PRICE);
        Rub3Access b = _deployEthOnly(otherDev);
        Rub3Access c = _deployThrough(factory, developer, address(shiba), 1e18);
        _register(a, developer);
        _register(b, otherDev);
        _register(c, developer);

        assertEq(registry.rankedListingWindow(0, 2).length, 2);
        assertEq(registry.rankedListingWindow(0, 2)[0], address(a));
        assertEq(registry.rankedListingWindow(2, 99).length, 1, "count past the end is clamped");
        assertEq(registry.rankedListingWindow(2, 99)[0], address(c));
        assertEq(registry.rankedListingWindow(3, 1).length, 0, "start past the end is empty");
        assertEq(registry.rankedListingWindow(99, 99).length, 0);
    }

    function test_rankedListings_isEmptyBeforeAnythingIsRegistered() public view {
        assertEq(registry.rankedListings().length, 0);
        assertEq(registry.registered().length, 0);
    }

    // ── Group 5b: reads whose cost the caller controls ───────────────────────

    function test_registeredWindow_clampsRatherThanReverting() public {
        Rub3Access a = _deployEthOnly(developer);
        Rub3Access b = _deployEthOnly(otherDev);
        Rub3Access c = _deployEthOnly(developer);
        _register(a, developer);
        _register(b, otherDev);
        _register(c, developer);

        address[] memory first = registry.registeredWindow(0, 2);
        assertEq(first.length, 2);
        assertEq(first[0], address(a));
        assertEq(first[1], address(b), "registration order, not rank");

        assertEq(registry.registeredWindow(2, 99).length, 1, "count past the end is clamped");
        assertEq(registry.registeredWindow(2, 99)[0], address(c));
        assertEq(registry.registeredWindow(3, 1).length, 0, "start past the end is empty");
        assertEq(registry.registeredWindow(99, 99).length, 0);
    }

    /// The bounded window includes only entries carrying the badge, exactly as
    /// {rankedListings} does, and ranks them by rail inside the window.
    function test_rankedRegistrationWindow_ranksInsideTheWindow() public {
        Rub3Access unrecognised = _deployThrough(factory, developer, address(shiba), 1e18);
        Rub3Access recognised = _deployThrough(factory, otherDev, address(usdc), USDC_PRICE);
        Rub3Access delisted = _deployEthOnly(developer);
        _register(unrecognised, developer);
        _register(recognised, otherDev);
        _register(delisted, developer);

        vm.prank(developer);
        registry.delist(address(delisted));

        address[] memory window = registry.rankedRegistrationWindow(0, 3);
        assertEq(window.length, 2, "the delisted entry is scanned and dropped");
        assertEq(window[0], address(recognised), "the recognised rail leads its window");
        assertEq(window[1], address(unrecognised));
    }

    /// **The tradeoff the NatSpec promises, asserted rather than described.**
    ///
    /// Paging through {rankedRegistrationWindow} does not reconstruct
    /// {rankedListings}: an unrecognised entry in an earlier window still comes
    /// back before a recognised one in a later window, because no window can
    /// know what the others hold without reading them - which is the whole point
    /// of not reading them. An integrator that assumed otherwise would
    /// allowlist in the wrong order, so it is worth a test that fails if this
    /// ever quietly starts being globally sorted.
    function test_rankedRegistrationWindow_isNotAPageOfTheGlobalRanking() public {
        Rub3Access early = _deployThrough(factory, developer, address(shiba), 1e18);
        Rub3Access late = _deployThrough(factory, otherDev, address(usdc), USDC_PRICE);
        _register(early, developer);
        _register(late, otherDev);

        address[] memory global = registry.rankedListings();
        assertEq(global[0], address(late), "globally, the recognised rail leads");
        assertEq(global[1], address(early));

        address[] memory firstPage = registry.rankedRegistrationWindow(0, 1);
        address[] memory secondPage = registry.rankedRegistrationWindow(1, 1);
        assertEq(firstPage.length, 1);
        assertEq(secondPage.length, 1);
        assertEq(firstPage[0], address(early), "window-local: registration order wins across pages");
        assertEq(secondPage[0], address(late));
    }

    function test_rankedRegistrationWindow_clampsRatherThanReverting() public {
        Rub3Access a = _deployEthOnly(developer);
        Rub3Access b = _deployEthOnly(otherDev);
        _register(a, developer);
        _register(b, otherDev);

        assertEq(registry.rankedRegistrationWindow(1, 99).length, 1, "count is clamped");
        assertEq(registry.rankedRegistrationWindow(1, 99)[0], address(b));
        assertEq(registry.rankedRegistrationWindow(2, 1).length, 0, "start past the end is empty");
        assertEq(registry.rankedRegistrationWindow(99, 99).length, 0);
        assertEq(registry.rankedRegistrationWindow(0, 0).length, 0, "a zero window scans nothing");
    }

    /// A window shorter than `count` is what a cursor walking a set with
    /// delisted entries in it looks like, so the docs tell callers to advance by
    /// `count`. This is that instruction, executed.
    function test_rankedRegistrationWindow_cursorAdvancesByCountNotByLength() public {
        Rub3Access[] memory all = new Rub3Access[](4);
        for (uint256 i = 0; i < 4; i++) {
            all[i] = _deployEthOnly(developer);
            _register(all[i], developer);
        }
        vm.startPrank(developer);
        registry.delist(address(all[0]));
        registry.delist(address(all[1]));
        vm.stopPrank();

        address[] memory seen = new address[](4);
        uint256 found;
        for (uint256 start = 0; start < registry.registeredCount(); start += 2) {
            address[] memory page = registry.rankedRegistrationWindow(start, 2);
            for (uint256 i = 0; i < page.length; i++) seen[found++] = page[i];
        }

        assertEq(found, 2, "every listed entry is reached exactly once");
        assertEq(seen[0], address(all[2]));
        assertEq(seen[1], address(all[3]));
    }

    /// **The bound is on the work, not only on the response.**
    ///
    /// `_registered` only grows and registration is permissionless, so a read
    /// that scans all of it is on a clock. This measures the difference the
    /// bounded read exists to make: over the same populated registry, a
    /// two-entry {rankedRegistrationWindow} costs a fraction of
    /// {rankedListings}, while {rankedListingWindow} - which cuts a page out of
    /// the global ranking - costs what the whole scan costs. Both halves matter:
    /// the first is the fix, and the second is why the docs on
    /// {rankedListingWindow} say plainly that it bounds only the response.
    function test_rankedRegistrationWindow_costDoesNotFollowTheSetSize() public {
        for (uint256 i = 0; i < 24; i++) {
            Rub3Access license = _deployEthOnly(developer);
            _register(license, developer);
        }

        uint256 before = gasleft();
        registry.rankedListings();
        uint256 wholeSet = before - gasleft();

        before = gasleft();
        registry.rankedListingWindow(0, 2);
        uint256 globalPage = before - gasleft();

        before = gasleft();
        registry.rankedRegistrationWindow(0, 2);
        uint256 boundedPage = before - gasleft();

        assertLt(boundedPage * 4, wholeSet, "the bounded read must not pay for the whole set");
        assertGt(globalPage * 2, wholeSet, "a global page still pays for the whole set");
    }

    // ── Group 6: the recognised-token list ───────────────────────────────────

    function test_recognisedTokens_areEnumerable() public {
        assertEq(registry.recognisedTokenCount(), 1);
        assertEq(registry.recognisedTokens()[0], address(usdc));

        vm.prank(registryOwner);
        registry.setTokenRecognised(address(shiba), true);
        assertEq(registry.recognisedTokenCount(), 2);
        assertTrue(registry.isRecognisedToken(address(shiba)));

        vm.prank(registryOwner);
        registry.setTokenRecognised(address(usdc), false);
        assertEq(registry.recognisedTokenCount(), 1);
        assertEq(registry.recognisedTokens()[0], address(shiba), "swap-and-pop keeps the set");
        assertFalse(registry.isRecognisedToken(address(usdc)));
    }

    /// The native rail's recognition is a rule, not a setting. Allowing it as a
    /// key would put every ETH-only contract one owner transaction away from the
    /// bottom of the list.
    function test_setTokenRecognised_refusesTheNativeRailInBothDirections() public {
        vm.startPrank(registryOwner);
        vm.expectRevert(Rub3Registry.NativeRailIsAlwaysRecognised.selector);
        registry.setTokenRecognised(address(0), false);
        vm.expectRevert(Rub3Registry.NativeRailIsAlwaysRecognised.selector);
        registry.setTokenRecognised(address(0), true);
        vm.stopPrank();

        assertTrue(registry.isRecognisedToken(address(0)), "and it stays recognised");
    }

    function test_setTokenRecognised_isOwnerOnly() public {
        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, developer)
        );
        registry.setTokenRecognised(address(shiba), true);
    }

    function test_setTokenRecognised_refusesANoOp() public {
        vm.startPrank(registryOwner);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.TokenAlreadyRecognised.selector, address(usdc), true
            )
        );
        registry.setTokenRecognised(address(usdc), true);

        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Registry.TokenAlreadyRecognised.selector, address(shiba), false
            )
        );
        registry.setTokenRecognised(address(shiba), false);
        vm.stopPrank();
    }

    function test_setTokenRecognised_emits() public {
        vm.expectEmit(true, false, false, true, address(registry));
        emit Rub3Registry.TokenRecognitionChanged(address(shiba), true);
        vm.prank(registryOwner);
        registry.setTokenRecognised(address(shiba), true);
    }

    // ── Group 7: the agent card ──────────────────────────────────────────────

    function test_card_isAssembledFromLiveReads() public {
        Rub3Access license = _deployThrough(factory, developer, address(usdc), USDC_PRICE);
        _register(license, developer);

        Rub3Registry.AgentCard memory c = registry.card(address(license));

        assertEq(c.license, address(license));
        assertEq(c.licenseOwner, developer);
        assertEq(c.appName, "Test App");
        assertEq(c.contentURI, "ipfs://bafyTestApp");
        assertEq(uint8(c.status), uint8(Rub3Registry.Status.Listed));
        assertFalse(c.suspended);
        assertTrue(c.listed);
        assertEq(c.price, PRICE);
        assertEq(c.priceToken, address(usdc));
        assertEq(c.priceAmount, USDC_PRICE);
        assertTrue(c.recognisedRail);
        assertEq(c.identityModel, 0);
        assertEq(c.tbaImplementation, address(0));
        assertEq(c.feeBps, FEE_BPS);
        assertEq(c.treasury, treasury);
        assertEq(c.registeredAtBlock, uint64(block.number));
        assertEq(c.wrapperHashes.length, 1);
        assertEq(c.wrapperHashes[0].hash, WRAPPER_HASH);
        assertEq(c.wrapperHashes[0].status, uint8(Rub3License.HashStatus.Valid));
    }

    /// A card that listed a revoked hash beside valid ones with no way to tell
    /// them apart would be worse than one that listed no hashes at all.
    function test_card_carriesEachWrapperHashStatus() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        bytes32 second = keccak256("registry-test-wrapper-v2");
        vm.startPrank(developer);
        license.addWrapperHash(second);
        license.revokeWrapperHash(WRAPPER_HASH, "key compromise");
        vm.stopPrank();

        Rub3Registry.AgentCard memory c = registry.card(address(license));
        assertEq(c.wrapperHashes.length, 2);
        assertEq(c.wrapperHashes[0].hash, WRAPPER_HASH);
        assertEq(c.wrapperHashes[0].status, uint8(Rub3License.HashStatus.Revoked));
        assertEq(c.wrapperHashes[1].hash, second);
        assertEq(c.wrapperHashes[1].status, uint8(Rub3License.HashStatus.Valid));
    }

    /// A useful answer to an agent holding an address from somewhere else,
    /// rather than a revert that sends it back to whatever it was told.
    function test_card_answersForAnUnregisteredContract() public {
        Rub3Access direct = _deployDirect(developer);

        Rub3Registry.AgentCard memory c = registry.card(address(direct));
        assertEq(uint8(c.status), uint8(Rub3Registry.Status.Unknown));
        assertFalse(c.listed);
        assertEq(c.appName, "");
        assertEq(c.price, PRICE, "the live reads still answer");
        assertEq(c.feeBps, 0, "and report a direct deploy's absent fee honestly");
    }

    function test_card_followsThePriceTheContractNowQuotes() public {
        Rub3Access license = _deployThrough(factory, developer, address(usdc), USDC_PRICE);
        _register(license, developer);

        vm.startPrank(developer);
        license.setPrice(2 ether);
        license.setTokenPrice(address(shiba), 7e18);
        vm.stopPrank();

        Rub3Registry.AgentCard memory c = registry.card(address(license));
        assertEq(c.price, 2 ether);
        assertEq(c.priceToken, address(shiba));
        assertEq(c.priceAmount, 7e18);
        assertFalse(c.recognisedRail);
    }

    function test_cards_returnsARankedPage() public {
        Rub3Access a = _deployThrough(factory, developer, address(shiba), 1e18);
        Rub3Access b = _deployThrough(factory, otherDev, address(usdc), USDC_PRICE);
        _register(a, developer);
        _register(b, otherDev);

        Rub3Registry.AgentCard[] memory page = registry.cards(0, 10);
        assertEq(page.length, 2);
        assertEq(page[0].license, address(b), "the recognised rail comes first");
        assertTrue(page[0].recognisedRail);
        assertEq(page[1].license, address(a));
        assertFalse(page[1].recognisedRail);

        assertEq(registry.cards(2, 10).length, 0);
    }

    function test_cardWindow_returnsTheCardsForItsOwnWindow() public {
        Rub3Access a = _deployThrough(factory, developer, address(shiba), 1e18);
        Rub3Access b = _deployThrough(factory, otherDev, address(usdc), USDC_PRICE);
        Rub3Access c = _deployEthOnly(developer);
        _register(a, developer);
        _register(b, otherDev);
        _register(c, developer);

        Rub3Registry.AgentCard[] memory page = registry.cardWindow(0, 2);
        assertEq(page.length, 2);
        assertEq(page[0].license, address(b), "ranked inside the window");
        assertEq(page[1].license, address(a));

        Rub3Registry.AgentCard[] memory tail = registry.cardWindow(2, 99);
        assertEq(tail.length, 1, "clamped, like every other window here");
        assertEq(tail[0].license, address(c));

        assertEq(registry.cardWindow(99, 99).length, 0);
    }

    /// A complete hash set reports itself as complete, so `wrapperHashCount` is
    /// meaningful on every card rather than only on capped ones.
    function test_card_reportsAnUncappedHashSetAsComplete() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        Rub3Registry.AgentCard memory c = registry.card(address(license));
        assertEq(c.wrapperHashes.length, 1);
        assertEq(c.wrapperHashCount, 1);
        assertFalse(c.wrapperHashesTruncated);
    }

    /// **The griefing vector, closed and then measured.**
    ///
    /// `addWrapperHash` is append-only and uncapped, so before the cap one
    /// licence owner could decide what reading *their* card cost - and with it
    /// what any page of cards containing them cost, which is a reach into
    /// unrelated listings' discoverability. The card now takes the newest
    /// {MAX_CARD_WRAPPER_HASHES} and says how many there really are.
    function test_card_capsTheHashSetAndReportsTheTrueTotal() public {
        Rub3Access license = _deployEthOnly(developer);
        _register(license, developer);

        uint256 cap = registry.MAX_CARD_WRAPPER_HASHES();
        uint256 total = cap + 5;
        bytes32[] memory published = new bytes32[](total);
        published[0] = WRAPPER_HASH;
        vm.startPrank(developer);
        for (uint256 i = 1; i < total; i++) {
            published[i] = keccak256(abi.encodePacked("registry-test-wrapper", i));
            license.addWrapperHash(published[i]);
        }
        vm.stopPrank();

        Rub3Registry.AgentCard memory c = registry.card(address(license));
        assertEq(c.wrapperHashes.length, cap, "the card is capped");
        assertEq(c.wrapperHashCount, total, "and reports what the contract really holds");
        assertTrue(c.wrapperHashesTruncated);

        // The newest end is kept: a buyer checking the build it just downloaded
        // is asking about the most recently published hash.
        for (uint256 i = 0; i < cap; i++) {
            assertEq(c.wrapperHashes[i].hash, published[total - cap + i]);
            assertEq(c.wrapperHashes[i].status, uint8(Rub3License.HashStatus.Valid));
        }
        assertEq(license.wrapperHashCount(), total, "nothing was capped on the licence itself");
    }

    /// The cap's reason for existing, executed: one listing's publishing history
    /// must not decide what a page of cards costs everybody sharing it.
    function test_cardWindow_costDoesNotFollowOneListingsHashSet() public {
        uint256 cap = registry.MAX_CARD_WRAPPER_HASHES();
        Rub3Access atTheCap = _deployEthOnly(developer);
        Rub3Access wellPast = _deployEthOnly(otherDev);
        _register(atTheCap, developer);
        _register(wellPast, otherDev);

        // One licence publishes exactly what a card can carry, the other eight
        // times that. Past the cap the card must stop getting more expensive,
        // which is the whole of the property.
        for (uint256 i = 1; i < cap; i++) {
            vm.prank(developer);
            atTheCap.addWrapperHash(keccak256(abi.encodePacked("at-cap", i)));
        }
        for (uint256 i = 1; i < cap * 8; i++) {
            vm.prank(otherDev);
            wellPast.addWrapperHash(keccak256(abi.encodePacked("well-past", i)));
        }

        uint256 before = gasleft();
        registry.card(address(atTheCap));
        uint256 atTheCapGas = before - gasleft();

        before = gasleft();
        registry.card(address(wellPast));
        uint256 wellPastGas = before - gasleft();

        assertLt(
            wellPastGas,
            (atTheCapGas * 11) / 10,
            "a card's cost must not follow how much its owner has published"
        );
        assertEq(registry.cardWindow(0, 2).length, 2, "and the shared page still answers");
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// @dev Whether `code` contains any opcode that can change another
    ///      contract's state, deploy, or self-destruct. Walks opcodes rather
    ///      than scanning bytes, so a push immediate is never mistaken for an
    ///      instruction.
    function _hasStateChangingCall(bytes memory code) internal pure returns (bool) {
        for (uint256 i = 0; i < code.length; ) {
            uint8 op = uint8(code[i]);
            if (op >= 0x60 && op <= 0x7F) {
                i += 1 + (uint256(op) - 0x5F);
                continue;
            }
            if (op == 0xF1 || op == 0xF2 || op == 0xF4 || op == 0xF0 || op == 0xF5 || op == 0xFF) {
                return true;
            }
            i += 1;
        }
        return false;
    }

    /// @dev Whether `signature`'s selector appears in `target`'s runtime code.
    ///      The same scan `Rub3Invariants.t.sol` runs, kept local because this
    ///      file asks a different question of it.
    function _hasSelector(address target, string memory signature) internal view returns (bool) {
        bytes4 sel = bytes4(keccak256(bytes(signature)));
        bytes memory code = target.code;
        if (code.length < 4) return false;
        for (uint256 i = 0; i + 4 <= code.length; i++) {
            if (code[i] != sel[0]) continue;
            if (code[i + 1] == sel[1] && code[i + 2] == sel[2] && code[i + 3] == sel[3]) {
                return true;
            }
        }
        return false;
    }
}
