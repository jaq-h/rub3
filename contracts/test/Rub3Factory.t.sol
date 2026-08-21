// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {Rub3Access} from "../src/Rub3Access.sol";
import {Rub3License} from "../src/Rub3License.sol";
import {Rub3Factory, Rub3LicenseParams, Rub3AccessDeployer} from "../src/Rub3Factory.sol";
import {MockEIP3009Token} from "./mocks/MockEIP3009Token.sol";

/// @notice A treasury that refuses ETH.
///
/// The fixture for the reason the fee is accrued rather than pushed at payment
/// time: `treasury` is immutable, so if the split transferred on the money path,
/// a recipient like this would brick every purchase on that contract forever.
contract RejectingTreasury {
    // No receive, no fallback: a plain ETH transfer reverts.
    function ping() external pure returns (uint256) {
        return 1;
    }
}

/// @notice Answers `isDeployed` like a factory but has no `previousFactory`, so
///         the chain walk could not continue through it.
///
/// The fixture for the constructor probe: a half-factory is exactly the shape
/// that would deploy fine and then revert every predecessor-bearing deploy
/// afterwards, when the pointer is immutable and can no longer be corrected.
contract HalfFactory {
    mapping(address => bool) public isDeployed;

    function record(address license) external {
        isDeployed[license] = true;
    }
}

/// @notice The protocol fee and the factory that stamps it (implementation.md
///         §2.3).
///
/// Three claims are under test here, in the order they matter:
///
///   1. **The terms are frozen.** `feeBps` and `treasury` are settled at
///      construction on the factory *and* on every contract it deploys, and no
///      path of any kind moves either afterwards - including deploying a newer
///      factory at a different rate, which must leave existing contracts alone.
///   2. **The arithmetic is exact, on both rails.** The protocol's share and the
///      developer's share sum to what was paid, to the wei, at the smallest
///      non-zero payment, at the payment where the fee rounds away, and at an
///      absurdly large one. The two rails run the same rule, proven by giving
///      them the same number and comparing.
///   3. **Direct deployment still works and is simply unrecorded.** Nothing here
///      penalises deploying the open-source template yourself; it just carries
///      no fee and gets no row in `isDeployed`.
///   4. **A factory deploy may only succeed a canonical predecessor.** Migration
///      is free by design, so an unconstrained `predecessor` would let a whole
///      fee-free sale be laundered onto a registry-listed contract. Group 8.
contract Rub3FactoryTest is Test {
    // keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 internal constant RECEIVE_TYPEHASH =
        0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;

    uint256 internal constant BUYER_PK = 0xA11CE5E;

    bytes32 internal constant WRAPPER_HASH = keccak256("test-wrapper-v1");
    uint256 internal constant PRICE = 1 ether;
    uint256 internal constant USDC_PRICE = 5_000_000; // 5 USDC, 6 decimals
    uint256 internal constant COOLDOWN_BLOCKS = 15;

    /// Within [MIN_FEE_BPS, MAX_FEE_BPS]. Deliberately not a round 2% or 3%: the
    /// rate is a deploy-time decision and the tests must not read as if one
    /// value inside the range were the settled one.
    uint16 internal constant FEE_BPS = 250;

    address internal treasury = address(0x7EA5);
    address internal developer = address(0xDE7);
    address internal submitter = address(0x5B417);
    address internal buyer;

    Rub3Factory internal factory;
    MockEIP3009Token internal usdc;
    Rub3Access internal nft;
    /// A second deploy through the same factory. The factory's record-keeping -
    /// `isDeployed`, the ordered enumeration, the per-deploy log - is only
    /// interesting with more than one row in it.
    Rub3Access internal second;

    function setUp() public {
        buyer = vm.addr(BUYER_PK);
        usdc = new MockEIP3009Token();
        factory = new Rub3Factory(FEE_BPS, treasury, address(0));

        vm.startPrank(developer);
        nft = Rub3Access(factory.deployAccess(_params(_sale(PRICE))));
        second = Rub3Access(factory.deployAccess(_params(_sale(PRICE))));
        vm.stopPrank();

        usdc.mint(buyer, 1_000_000_000); // 1000 USDC
        vm.deal(buyer, 100 ether);
        vm.deal(submitter, 10 ether);
    }

    // ── Fixtures ──────────────────────────────────────────────────────────────

    function _hashes(bytes32 h) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](1);
        out[0] = h;
    }

    function _sale(uint256 price) internal view returns (Rub3License.SaleTerms memory) {
        return
            Rub3License.SaleTerms({
                price: price,
                priceToken: address(usdc),
                priceAmount: USDC_PRICE
            });
    }

    function _saleEthOnly(uint256 price) internal pure returns (Rub3License.SaleTerms memory) {
        return Rub3License.SaleTerms({price: price, priceToken: address(0), priceAmount: 0});
    }

    function _identity() internal pure returns (Rub3License.IdentityTerms memory) {
        return Rub3License.IdentityTerms({model: 0, tbaImplementation: address(0)});
    }

    /// The deploy inputs a developer supplies. Note what is *not* in here: the
    /// fee terms, which the factory reads off itself.
    function _params(Rub3License.SaleTerms memory sale)
        internal
        view
        returns (Rub3LicenseParams memory)
    {
        return Rub3LicenseParams({
            name: "Rub3 Test",
            symbol: "R3T",
            identity: _identity(),
            wrapperHashes: _hashes(WRAPPER_HASH),
            sale: sale,
            supplyCap: 0,
            cooldownBlocks: COOLDOWN_BLOCKS,
            predecessor: address(0),
            owner: address(0) // defaults to the caller
        });
    }

    /// The same deploy inputs, naming a predecessor. Group 8 is the only place
    /// that matters, because it is the only thing the factory now checks beyond
    /// the fee terms it stamps itself.
    function _paramsWithPredecessor(Rub3License.SaleTerms memory sale, address predecessor)
        internal
        view
        returns (Rub3LicenseParams memory out)
    {
        out = _params(sale);
        out.predecessor = predecessor;
    }

    function _noFee() internal pure returns (Rub3License.FeeTerms memory) {
        return Rub3License.FeeTerms({feeBps: 0, treasury: address(0)});
    }

    /// A directly deployed access licence, the way anyone may deploy the
    /// open-source template.
    function _deployDirect(uint256 price) internal returns (Rub3Access) {
        return new Rub3Access(
            "Direct",
            "DIR",
            _identity(),
            _hashes(WRAPPER_HASH),
            _saleEthOnly(price),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            developer
        );
    }

    /// A fee-bearing access licence at an arbitrary price and rate, deployed
    /// through a factory so the terms are stamped rather than chosen.
    function _deployWithFee(uint256 price, uint16 feeBps, address treasury_)
        internal
        returns (Rub3Access)
    {
        Rub3Factory f = new Rub3Factory(feeBps, treasury_, address(0));
        vm.prank(developer);
        return Rub3Access(f.deployAccess(_params(_saleEthOnly(price))));
    }

    /// The protocol's share of `amount` at `feeBps`, computed independently of
    /// the contract so a mistake in one is not mirrored in the other.
    function _expectedFee(uint256 amount, uint256 feeBps) internal pure returns (uint256) {
        return (amount * feeBps) / 10_000;
    }

    // ── Authorization helpers (mirrors Rub3TokenPurchase.t.sol) ───────────────

    function _purchaseAuth(Rub3License target, address recipient, uint256 value, bytes32 salt)
        internal
        view
        returns (Rub3License.PaymentAuthorization memory auth)
    {
        auth.from = buyer;
        auth.validAfter = 0;
        auth.validBefore = block.timestamp + 1 hours;
        auth.salt = salt;
        auth.signature = _sign(
            address(target),
            value,
            auth.validAfter,
            auth.validBefore,
            target.purchaseAuthorizationNonce(recipient, salt)
        );
    }

    function _sign(
        address payee,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce
    ) internal view returns (bytes memory) {
        bytes32 structHash = keccak256(
            abi.encode(RECEIVE_TYPEHASH, buyer, payee, value, validAfter, validBefore, nonce)
        );
        bytes32 digest =
            keccak256(abi.encodePacked("\x19\x01", usdc.DOMAIN_SEPARATOR(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(BUYER_PK, digest);
        return abi.encodePacked(r, s, v);
    }

    // ══ 1. The factory ═══════════════════════════════════════════════════════

    function test_factory_stampsItsOwnTermsOnEveryDeploy() public view {
        assertEq(factory.feeBps(), FEE_BPS);
        assertEq(factory.treasury(), treasury);

        assertEq(nft.feeBps(), FEE_BPS);
        assertEq(nft.treasury(), treasury);
        assertEq(second.feeBps(), FEE_BPS);
        assertEq(second.treasury(), treasury);
    }

    function test_factory_recordsWhatItDeployed() public view {
        assertTrue(factory.isDeployed(address(nft)));
        assertTrue(factory.isDeployed(address(second)));

        assertEq(factory.deploymentCount(), 2);
        assertEq(factory.deploymentAt(0), address(nft));
        assertEq(factory.deploymentAt(1), address(second));

        address[] memory all = factory.deployments();
        assertEq(all.length, 2);
        assertEq(all[0], address(nft));
        assertEq(all[1], address(second));
    }

    function test_factory_ownerDefaultsToCaller() public view {
        assertEq(nft.owner(), developer);
        assertEq(second.owner(), developer);
    }

    function test_factory_explicitOwnerIsHonored() public {
        Rub3LicenseParams memory p = _params(_saleEthOnly(PRICE));
        p.owner = address(0xB0B);

        vm.prank(developer);
        Rub3Access deployed = Rub3Access(factory.deployAccess(p));

        assertEq(deployed.owner(), address(0xB0B));
        assertTrue(factory.isDeployed(address(deployed)));
    }

    function test_factory_emitsDeploymentWithItsTerms() public {
        vm.recordLogs();
        vm.prank(developer);
        address deployed = factory.deployAccess(_params(_saleEthOnly(PRICE)));

        Vm.Log[] memory logs = vm.getRecordedLogs();
        bytes32 topic = keccak256("LicenseDeployed(address,address,address,uint16,address)");

        bool found;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != topic) continue;
            found = true;
            assertEq(address(uint160(uint256(logs[i].topics[1]))), deployed);
            assertEq(address(uint160(uint256(logs[i].topics[2]))), developer); // owner
            assertEq(address(uint160(uint256(logs[i].topics[3]))), developer); // deployer
            (uint16 feeBps, address treasury_) = abi.decode(logs[i].data, (uint16, address));
            assertEq(feeBps, FEE_BPS);
            assertEq(treasury_, treasury);
        }
        assertTrue(found, "LicenseDeployed not emitted");
    }

    function test_factory_rejectsFeeBelowRange() public {
        uint16 tooLow = factory.MIN_FEE_BPS() - 1;
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Factory.FeeBpsOutOfRange.selector, tooLow, uint16(200), uint16(300)
            )
        );
        new Rub3Factory(tooLow, treasury, address(0));
    }

    function test_factory_rejectsFeeAboveRange() public {
        uint16 tooHigh = factory.MAX_FEE_BPS() + 1;
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Factory.FeeBpsOutOfRange.selector, tooHigh, uint16(200), uint16(300)
            )
        );
        new Rub3Factory(tooHigh, treasury, address(0));
    }

    function test_factory_acceptsBothEndsOfTheRange() public {
        assertEq(new Rub3Factory(200, treasury, address(0)).feeBps(), 200);
        assertEq(new Rub3Factory(300, treasury, address(0)).feeBps(), 300);
    }

    function test_factory_rejectsZeroTreasury() public {
        vm.expectRevert(Rub3Factory.TreasuryRequired.selector);
        new Rub3Factory(FEE_BPS, address(0), address(0));
    }

    /// The factory's initcode carries its deployer helper, which carries the
    /// licence implementation, so growing the contracts eats into the EIP-3860
    /// limit. Left unguarded, the first sign of trouble would be an undeployable
    /// factory on mainnet.
    function test_factory_initcodeFitsUnderEip3860() public pure {
        assertLt(type(Rub3Factory).creationCode.length, 49_152);
    }

    /// The runtime limit is the reason the deployer is a separate contract at
    /// all: `Rub3Access`'s creation code alone is over 16 KB, and a factory
    /// holding it in its own runtime would have almost nothing left.
    function test_factory_runtimeFitsUnderTheCodeSizeLimit() public view {
        assertLt(address(factory).code.length, 24_576);
        assertLt(factory.accessDeployer().code.length, 24_576);
    }

    // ══ 2. Immutability: the product promise ═════════════════════════════════

    /// A newer factory at a different rate changes what is *offered* from then
    /// on and nothing that already exists. This is the whole "rub3 changes its
    /// take only by shipping a new factory version" claim, checked rather than
    /// asserted.
    function test_immutable_olderFactoryDeployKeepsItsOriginalTerms() public {
        address newTreasury = address(0xFEE2);
        Rub3Factory v2 = new Rub3Factory(300, newTreasury, address(0));

        vm.prank(developer);
        Rub3Access fresh = Rub3Access(v2.deployAccess(_params(_saleEthOnly(PRICE))));

        // The new factory's deploy carries the new terms...
        assertEq(fresh.feeBps(), 300);
        assertEq(fresh.treasury(), newTreasury);

        // ...and the old one is untouched, terms and money alike.
        assertEq(nft.feeBps(), FEE_BPS);
        assertEq(nft.treasury(), treasury);

        vm.prank(buyer);
        nft.purchase{value: PRICE}(address(0));
        assertEq(nft.feesAccrued(), _expectedFee(PRICE, FEE_BPS));

        nft.withdrawFees();
        assertEq(treasury.balance, _expectedFee(PRICE, FEE_BPS));
        assertEq(newTreasury.balance, 0);
    }

    /// The old factory does not learn about the new one either: `isDeployed` is
    /// per factory, and neither can write the other's.
    function test_immutable_registriesAreDisjointPerFactory() public {
        Rub3Factory v2 = new Rub3Factory(300, address(0xFEE2), address(0));

        vm.prank(developer);
        address fresh = v2.deployAccess(_params(_saleEthOnly(PRICE)));

        assertTrue(v2.isDeployed(fresh));
        assertFalse(factory.isDeployed(fresh));
        assertFalse(v2.isDeployed(address(nft)));
    }

    /// The contract owner has every power the contract grants and none of them
    /// reaches the fee. This is the behavioural companion to the bytecode audit
    /// in `Rub3Invariants.t.sol`.
    function test_immutable_contractOwnerCannotTouchTheFee() public {
        vm.startPrank(developer);
        nft.setPrice(2 ether);
        nft.setTokenPrice(address(usdc), 9_000_000);
        nft.addWrapperHash(keccak256("v2"));
        nft.setSuccessor(address(0xDEAD));
        nft.transferOwnership(address(0xBAD));
        vm.stopPrank();

        vm.prank(address(0xBAD));
        nft.renounceOwnership();

        assertEq(nft.feeBps(), FEE_BPS);
        assertEq(nft.treasury(), treasury);

        // And the split still runs on an ownerless contract.
        vm.prank(buyer);
        nft.purchase{value: 2 ether}(address(0));
        assertEq(nft.feesAccrued(), _expectedFee(2 ether, FEE_BPS));
    }

    /// The fee terms survive a migration: a successor is a separate deploy with
    /// its own terms, and claiming onto it cannot rewrite the predecessor's.
    function test_immutable_feeSurvivesSuccessorPointer() public {
        vm.prank(developer);
        nft.setSuccessor(address(0xB0B));

        assertEq(nft.feeBps(), FEE_BPS);
        assertEq(nft.treasury(), treasury);
    }

    // ══ 3. Fee arithmetic: the ETH rail ══════════════════════════════════════

    function test_eth_purchaseSplitsExactly() public {
        uint256 fee = _expectedFee(PRICE, FEE_BPS);

        vm.prank(buyer);
        nft.purchase{value: PRICE}(address(0));

        assertEq(nft.feesAccrued(), fee);
        assertEq(address(nft).balance, PRICE);

        nft.withdrawFees();
        assertEq(treasury.balance, fee);
        assertEq(nft.feesAccrued(), 0);
        assertEq(address(nft).balance, PRICE - fee);

        vm.prank(developer);
        nft.withdraw(payable(developer));
        assertEq(developer.balance, PRICE - fee);
        assertEq(address(nft).balance, 0);

        // The whole point, stated as one equation.
        assertEq(treasury.balance + developer.balance, PRICE);
    }

    /// Order must not matter: the developer sweeping first cannot take the fee.
    function test_eth_developerWithdrawingFirstCannotTakeTheFee() public {
        uint256 fee = _expectedFee(PRICE, FEE_BPS);

        vm.prank(buyer);
        nft.purchase{value: PRICE}(address(0));

        vm.prank(developer);
        nft.withdraw(payable(developer));

        assertEq(developer.balance, PRICE - fee);
        assertEq(address(nft).balance, fee);

        nft.withdrawFees();
        assertEq(treasury.balance, fee);
        assertEq(address(nft).balance, 0);
    }

    /// And the reverse: sweeping the fee twice yields nothing the second time,
    /// so the treasury cannot reach into the developer's share either.
    function test_eth_feeCannotBeSweptTwice() public {
        vm.prank(buyer);
        nft.purchase{value: PRICE}(address(0));

        nft.withdrawFees();
        uint256 afterFirst = treasury.balance;

        nft.withdrawFees();
        assertEq(treasury.balance, afterFirst);

        vm.prank(developer);
        nft.withdraw(payable(developer));
        assertEq(developer.balance, PRICE - afterFirst);
    }

    function test_eth_feesAccumulateAcrossPurchases() public {
        uint256 fee = _expectedFee(PRICE, FEE_BPS);

        vm.startPrank(buyer);
        nft.purchase{value: PRICE}(address(0));
        nft.purchase{value: PRICE}(address(0));
        nft.purchase{value: PRICE}(address(0));
        vm.stopPrank();

        assertEq(nft.feesAccrued(), fee * 3);

        nft.withdrawFees();
        vm.prank(developer);
        nft.withdraw(payable(developer));

        assertEq(treasury.balance, fee * 3);
        assertEq(developer.balance, 3 * PRICE - fee * 3);
        assertEq(treasury.balance + developer.balance, 3 * PRICE);
    }

    /// There is no such thing as an overpayment to charge a fee on: the ETH
    /// rail takes the listed price exactly, so the fee base and the listed
    /// price are the same number and no fee can ever accrue on an amount above
    /// the price. This is the inverted form of a test that used to send more
    /// than the listed price and assert the excess was taxed.
    function test_eth_feeIsChargedOnTheListedPriceBecauseNothingElseArrives() public {
        uint256 over = PRICE + 0.37 ether;

        vm.prank(buyer);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, over, PRICE));
        nft.purchase{value: over}(address(0));

        assertEq(nft.feesAccrued(), 0, "a rejected payment accrues nothing");

        // The exact price is what mints, and the fee is charged on that.
        vm.prank(buyer);
        nft.purchase{value: PRICE}(address(0));
        assertEq(nft.feesAccrued(), _expectedFee(PRICE, FEE_BPS));

        nft.withdrawFees();
        vm.prank(developer);
        nft.withdraw(payable(developer));
        assertEq(treasury.balance + developer.balance, PRICE);
    }

    /// The evasion route the old fee-on-what-arrived rule existed to close -
    /// list at zero, collect the real price as "overpayment", pay no fee - is
    /// now closed one step earlier, at the payment itself. A zero-price listing
    /// takes zero and nothing else, so there is no revenue to hide in it.
    function test_eth_zeroPriceListingCannotCollectByOverpaying() public {
        Rub3Access free = _deployWithFee(0, FEE_BPS, treasury);

        vm.prank(buyer);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, 10 ether, 0));
        free.purchase{value: 10 ether}(address(0));

        assertEq(address(free).balance, 0);
        assertEq(free.feesAccrued(), 0);

        // Zero is the only amount it accepts, and it is genuinely free.
        vm.prank(buyer);
        free.purchase{value: 0}(address(0));
        assertEq(free.feesAccrued(), 0);
    }

    /// Boundary: the smallest non-zero payment there is. The fee rounds to zero,
    /// which means the developer gets the whole wei and nothing is stranded.
    function test_eth_smallestNonZeroPayment() public {
        Rub3Access tiny = _deployWithFee(1, FEE_BPS, treasury);

        vm.prank(buyer);
        tiny.purchase{value: 1}(address(0));

        assertEq(tiny.feesAccrued(), 0);
        assertEq(address(tiny).balance, 1);

        vm.prank(developer);
        tiny.withdraw(payable(developer));
        assertEq(developer.balance, 1);
        assertEq(address(tiny).balance, 0);
    }

    /// Boundary: the exact wei at which the fee stops rounding away. At
    /// 250 bps that is 40 wei (40 * 250 / 10000 == 1), and 39 is still zero.
    function test_eth_roundingBoundary() public {
        uint256 justUnder = (10_000 / FEE_BPS) - 1; // 39
        Rub3Access a = _deployWithFee(justUnder, FEE_BPS, treasury);
        vm.prank(buyer);
        a.purchase{value: justUnder}(address(0));
        assertEq(a.feesAccrued(), 0);
        assertEq(_expectedFee(justUnder, FEE_BPS), 0);

        uint256 exactly = 10_000 / FEE_BPS; // 40
        Rub3Access b = _deployWithFee(exactly, FEE_BPS, treasury);
        vm.prank(buyer);
        b.purchase{value: exactly}(address(0));
        assertEq(b.feesAccrued(), 1);
    }

    /// Rounding is integer division, so the remainder always falls to the
    /// developer. Never the other way: a fee that rounded *up* could exceed the
    /// payment at the smallest amounts.
    function test_eth_roundingFavoursTheDeveloper() public {
        uint256 paid = 1_234_567_891_234_567_891; // deliberately not divisible
        Rub3Access odd = _deployWithFee(paid, FEE_BPS, treasury);

        vm.prank(buyer);
        odd.purchase{value: paid}(address(0));

        uint256 fee = odd.feesAccrued();
        assertEq(fee, (paid * FEE_BPS) / 10_000);
        assertLt(fee * 10_000, paid * FEE_BPS + 10_000); // fee <= exact share
        assertEq(fee + (paid - fee), paid);
    }

    /// Boundary: an amount far past anything a licence will ever cost. It must
    /// not overflow and must still sum exactly.
    function test_eth_largestRealisticPayment() public {
        uint256 huge = 1_000_000 ether;
        Rub3Access big = _deployWithFee(huge, FEE_BPS, treasury);

        vm.deal(buyer, huge);
        vm.prank(buyer);
        big.purchase{value: huge}(address(0));

        uint256 fee = _expectedFee(huge, FEE_BPS);
        assertEq(big.feesAccrued(), fee);

        big.withdrawFees();
        vm.prank(developer);
        big.withdraw(payable(developer));
        assertEq(treasury.balance + developer.balance, huge);
    }

    /// The two shares can never exceed the payment, at any rate the constructor
    /// accepts and any amount that fits.
    function testFuzz_eth_sharesNeverExceedThePayment(uint96 amount, uint16 bps) public {
        bps = uint16(bound(bps, 200, 300));
        uint256 paid = bound(uint256(amount), 1, 1_000_000 ether);

        Rub3Access f = _deployWithFee(paid, bps, treasury);
        vm.deal(buyer, paid);
        vm.prank(buyer);
        f.purchase{value: paid}(address(0));

        uint256 fee = f.feesAccrued();
        assertLe(fee, paid);
        assertEq(fee, _expectedFee(paid, bps));

        f.withdrawFees();
        vm.prank(developer);
        f.withdraw(payable(developer));
        assertEq(treasury.balance + developer.balance, paid);
        assertEq(address(f).balance, 0);
    }

    /// The fee is taken on *every* payment the contract receives, not once per
    /// contract: a second sale accrues a second fee, and the two shares still
    /// sum to what arrived.
    function test_eth_everyPaymentIsChargedNotJustTheFirst() public {
        vm.prank(buyer);
        second.purchase{value: PRICE}(address(0));

        uint256 afterFirst = second.feesAccrued();
        assertEq(afterFirst, _expectedFee(PRICE, FEE_BPS));

        vm.prank(buyer);
        second.purchase{value: PRICE}(address(0));

        assertEq(second.feesAccrued(), afterFirst * 2);

        second.withdrawFees();
        vm.prank(developer);
        second.withdraw(payable(developer));
        assertEq(treasury.balance + developer.balance, 2 * PRICE);
    }

    function test_eth_accrualEventStatesBothShares() public {
        uint256 fee = _expectedFee(PRICE, FEE_BPS);

        vm.expectEmit(true, false, false, true, address(nft));
        emit Rub3License.ProtocolFeeAccrued(address(0), PRICE, fee, PRICE - fee);

        vm.prank(buyer);
        nft.purchase{value: PRICE}(address(0));
    }

    // ══ 4. Fee arithmetic: the stablecoin rail ═══════════════════════════════

    function test_token_purchaseSplitsExactly() public {
        uint256 fee = _expectedFee(USDC_PRICE, FEE_BPS);

        vm.prank(submitter);
        nft.purchaseWithAuthorization(buyer, _purchaseAuth(nft, buyer, USDC_PRICE, keccak256("s1")));

        assertEq(usdc.balanceOf(address(nft)), USDC_PRICE);
        assertEq(nft.tokenFeesAccrued(address(usdc)), fee);

        nft.withdrawTokenFees(address(usdc));
        assertEq(usdc.balanceOf(treasury), fee);
        assertEq(nft.tokenFeesAccrued(address(usdc)), 0);

        vm.prank(developer);
        nft.withdrawToken(address(usdc), developer);
        assertEq(usdc.balanceOf(developer), USDC_PRICE - fee);
        assertEq(usdc.balanceOf(address(nft)), 0);

        assertEq(usdc.balanceOf(treasury) + usdc.balanceOf(developer), USDC_PRICE);
    }

    function test_token_developerWithdrawingFirstCannotTakeTheFee() public {
        uint256 fee = _expectedFee(USDC_PRICE, FEE_BPS);

        vm.prank(submitter);
        nft.purchaseWithAuthorization(buyer, _purchaseAuth(nft, buyer, USDC_PRICE, keccak256("s1")));

        vm.prank(developer);
        nft.withdrawToken(address(usdc), developer);

        assertEq(usdc.balanceOf(developer), USDC_PRICE - fee);
        assertEq(usdc.balanceOf(address(nft)), fee);

        nft.withdrawTokenFees(address(usdc));
        assertEq(usdc.balanceOf(treasury), fee);
    }

    /// The stablecoin counterpart of
    /// {test_eth_everyPaymentIsChargedNotJustTheFirst}.
    function test_token_everyPaymentIsChargedNotJustTheFirst() public {
        vm.prank(submitter);
        second.purchaseWithAuthorization(
            buyer, _purchaseAuth(second, buyer, USDC_PRICE, keccak256("s1"))
        );

        uint256 fee = _expectedFee(USDC_PRICE, FEE_BPS);
        assertEq(second.tokenFeesAccrued(address(usdc)), fee);

        vm.prank(submitter);
        second.purchaseWithAuthorization(
            buyer, _purchaseAuth(second, buyer, USDC_PRICE, keccak256("s2"))
        );

        assertEq(second.tokenFeesAccrued(address(usdc)), fee * 2);

        second.withdrawTokenFees(address(usdc));
        vm.prank(developer);
        second.withdrawToken(address(usdc), developer);
        assertEq(usdc.balanceOf(treasury) + usdc.balanceOf(developer), USDC_PRICE * 2);
    }

    /// **The two rails run one rule.** Same numeric amount, same rate, same
    /// split - so a fee that is right on one rail cannot be wrong on the other.
    function test_bothRails_chargeIdenticallyForTheSameAmount() public {
        // A contract priced at the *same number* in wei and in the token's
        // smallest unit, so the two fees are directly comparable.
        uint256 amount = USDC_PRICE;

        Rub3Factory f = new Rub3Factory(FEE_BPS, treasury, address(0));
        Rub3LicenseParams memory p = _params(
            Rub3License.SaleTerms({price: amount, priceToken: address(usdc), priceAmount: amount})
        );
        vm.prank(developer);
        Rub3Access twin = Rub3Access(f.deployAccess(p));

        vm.prank(buyer);
        twin.purchase{value: amount}(address(0));

        vm.prank(submitter);
        twin.purchaseWithAuthorization(buyer, _purchaseAuth(twin, buyer, amount, keccak256("s1")));

        assertEq(twin.feesAccrued(), twin.tokenFeesAccrued(address(usdc)));
        assertEq(twin.feesAccrued(), _expectedFee(amount, FEE_BPS));
    }

    /// Balance that never arrived through a payment function has no fee
    /// reserved against it, so the developer sweeps it whole. This asserts two
    /// readings at once, and both are intended:
    ///
    /// 1. A token nobody paid in - a mistaken transfer, an airdrop - is the
    ///    developer's, because rub3 took no cut on money it never handled.
    /// 2. It is therefore also the accepted boundary of what the fee captures:
    ///    a developer who lists at zero and takes payment by direct transfer
    ///    pays no fee. That is documented (contracts/contracts.md -> "The
    ///    protocol fee", architecture.md -> "Rub3Factory") and deliberate, not
    ///    an oversight. The fee applies to value arriving through the payment
    ///    functions; payment arranged outside them is out of scope, and
    ///    charging on unaccounted balance was rejected because it would take a
    ///    cut of transfers that are not revenue.
    ///
    /// Do not "fix" this by tightening the sweep.
    function test_token_unaccruedBalanceSweepsEntirelyToTheDeveloper() public {
        MockEIP3009Token other = new MockEIP3009Token();
        other.mint(address(nft), 777);

        assertEq(nft.tokenFeesAccrued(address(other)), 0);

        vm.prank(developer);
        nft.withdrawToken(address(other), developer);
        assertEq(other.balanceOf(developer), 777);
    }

    function test_token_accrualEventStatesBothShares() public {
        uint256 fee = _expectedFee(USDC_PRICE, FEE_BPS);

        vm.expectEmit(true, false, false, true, address(nft));
        emit Rub3License.ProtocolFeeAccrued(address(usdc), USDC_PRICE, fee, USDC_PRICE - fee);

        vm.prank(submitter);
        nft.purchaseWithAuthorization(buyer, _purchaseAuth(nft, buyer, USDC_PRICE, keccak256("s1")));
    }

    // ══ 5. Direct deployment stays possible ══════════════════════════════════

    function test_direct_deployWorksAndCarriesNoFee() public {
        Rub3Access direct = _deployDirect(PRICE);

        assertEq(direct.feeBps(), 0);
        assertEq(direct.treasury(), address(0));
        assertFalse(factory.isDeployed(address(direct)));

        vm.prank(buyer);
        direct.purchase{value: PRICE}(address(0));

        assertEq(direct.feesAccrued(), 0);

        vm.prank(developer);
        direct.withdraw(payable(developer));
        assertEq(developer.balance, PRICE);
    }

    /// No fee means no fee surface: there is nowhere for {withdrawFees} to send
    /// to, and it says so rather than burning the balance to `address(0)`.
    function test_direct_withdrawFeesReverts() public {
        Rub3Access direct = _deployDirect(PRICE);

        vm.expectRevert(Rub3License.NoFeeConfigured.selector);
        direct.withdrawFees();

        vm.expectRevert(Rub3License.NoFeeConfigured.selector);
        direct.withdrawTokenFees(address(usdc));
    }

    function test_direct_isNotPenalisedOnEitherRail() public {
        Rub3Access direct = new Rub3Access(
            "Direct",
            "DIR",
            _identity(),
            _hashes(WRAPPER_HASH),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            developer
        );

        vm.prank(submitter);
        direct.purchaseWithAuthorization(
            buyer, _purchaseAuth(direct, buyer, USDC_PRICE, keccak256("s1"))
        );

        assertEq(direct.tokenFeesAccrued(address(usdc)), 0);
        vm.prank(developer);
        direct.withdrawToken(address(usdc), developer);
        assertEq(usdc.balanceOf(developer), USDC_PRICE);
    }

    // ══ 6. Fee terms are validated where they are still choosable ════════════

    function test_license_rejectsFeeAbove100Percent() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3License.FeeBpsTooHigh.selector, uint16(10_001), uint256(10_000)
            )
        );
        new Rub3Access(
            "x",
            "x",
            _identity(),
            _hashes(WRAPPER_HASH),
            _saleEthOnly(PRICE),
            Rub3License.FeeTerms({feeBps: 10_001, treasury: treasury}),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            developer
        );
    }

    function test_license_rejectsFeeWithNoTreasury() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3License.FeeTermsInconsistent.selector, uint16(250), address(0)
            )
        );
        new Rub3Access(
            "x",
            "x",
            _identity(),
            _hashes(WRAPPER_HASH),
            _saleEthOnly(PRICE),
            Rub3License.FeeTerms({feeBps: 250, treasury: address(0)}),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            developer
        );
    }

    function test_license_rejectsTreasuryWithNoFee() public {
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.FeeTermsInconsistent.selector, uint16(0), treasury)
        );
        new Rub3Access(
            "x",
            "x",
            _identity(),
            _hashes(WRAPPER_HASH),
            _saleEthOnly(PRICE),
            Rub3License.FeeTerms({feeBps: 0, treasury: treasury}),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            developer
        );
    }

    // ══ 7. Why the fee is accrued and not pushed ═════════════════════════════

    /// The failure this design exists to prevent. `treasury` is immutable, so a
    /// recipient that cannot receive ETH would, under a push-at-payment split,
    /// revert every purchase on this contract for as long as it exists. Here it
    /// costs the treasury its own withdrawal and nothing else: buyers still buy,
    /// and the developer is still paid in full.
    function test_accrual_rejectingTreasuryCannotBlockPurchases() public {
        address rejecting = address(new RejectingTreasury());
        Rub3Access hostile = _deployWithFee(PRICE, FEE_BPS, rejecting);

        vm.prank(buyer);
        hostile.purchase{value: PRICE}(address(0));
        assertEq(hostile.ownerOf(0), buyer);

        vm.prank(buyer);
        hostile.purchase{value: PRICE}(address(0));
        assertEq(hostile.ownerOf(1), buyer);

        // The developer's share is unaffected and withdrawable.
        uint256 fee = _expectedFee(PRICE, FEE_BPS) * 2;
        vm.prank(developer);
        hostile.withdraw(payable(developer));
        assertEq(developer.balance, 2 * PRICE - fee);
        assertEq(address(hostile).balance, fee);

        // Only rub3's own collection fails, which is rub3's problem to fix by
        // choosing a treasury that can hold ETH.
        vm.expectRevert(Rub3License.WithdrawFailed.selector);
        hostile.withdrawFees();
    }

    /// Anyone may settle the fee, because the destination is immutable and the
    /// caller decides nothing but the timing.
    function test_accrual_withdrawFeesIsPermissionless() public {
        vm.prank(buyer);
        nft.purchase{value: PRICE}(address(0));

        vm.prank(address(0xA11CE)); // no relation to anything
        nft.withdrawFees();

        assertEq(treasury.balance, _expectedFee(PRICE, FEE_BPS));
    }

    // ══ 8. A factory deploy may only succeed a canonical predecessor ═════════

    /// The predecessor-laundering route, closed at the factory door.
    ///
    /// Sell every licence on a fee-free direct deploy, then deploy a successor
    /// *through the factory* naming it as predecessor: `claimFromPredecessor`
    /// charges nothing by design, because migration must never be taxed, so
    /// every holder would land on a fee-bearing, `isDeployed`-listed contract
    /// with the treasury never paid. The deploy is the step that has to fail,
    /// and it fails before the licence is even built.
    function test_predecessor_launderingThroughTheFactoryReverts() public {
        vm.prank(developer);
        Rub3Access shadow = new Rub3Access(
            "Shadow",
            "SHD",
            _identity(),
            _hashes(WRAPPER_HASH),
            _saleEthOnly(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            developer
        );

        // The sale itself is legitimate and stays legitimate: a direct deploy is
        // fee-free and nothing here penalises it.
        vm.prank(buyer);
        shadow.purchase{value: PRICE}(address(0));
        assertEq(shadow.feesAccrued(), 0);

        // The laundering step. The factory will not put its registry row at the
        // far end of that sale.
        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Factory.PredecessorNotCanonical.selector, address(shadow))
        );
        factory.deployAccess(_paramsWithPredecessor(_saleEthOnly(PRICE), address(shadow)));

        assertFalse(factory.isCanonicalPredecessor(address(shadow)));
        assertEq(factory.deploymentCount(), 2, "and nothing was recorded");
    }

    /// The counterweight, so the guard is not just a refusal: a contract this
    /// factory recorded is accepted, and the migration the feature exists for
    /// still runs end to end.
    function test_predecessor_canonicalIsAcceptedAndHoldersStillMigrate() public {
        vm.prank(buyer);
        uint256 id = nft.purchase{value: PRICE}(address(0));

        vm.prank(developer);
        Rub3Access v2 = Rub3Access(
            factory.deployAccess(_paramsWithPredecessor(_saleEthOnly(PRICE), address(nft)))
        );
        assertTrue(factory.isDeployed(address(v2)));
        assertEq(v2.predecessor(), address(nft));

        vm.prank(developer);
        nft.setSuccessor(address(v2));

        vm.prank(buyer);
        uint256 claimed = v2.claimFromPredecessor(id);
        assertEq(v2.ownerOf(claimed), buyer);
        assertTrue(v2.honorsContract(address(nft), claimed));
    }

    /// No predecessor is always canonical - the common case, and the one the
    /// guard must never touch.
    function test_predecessor_zeroIsAlwaysCanonical() public view {
        assertTrue(factory.isCanonicalPredecessor(address(0)));
        assertEq(nft.predecessor(), address(0));
        assertEq(second.predecessor(), address(0));
    }

    /// rub3 changes its take by deploying a *new* factory, so contracts the old
    /// one recorded have to stay migratable onto the new one. That is the whole
    /// reason `previousFactory` exists.
    function test_predecessor_previousFactoryChainIsAccepted() public {
        Rub3Factory v2Factory = new Rub3Factory(300, address(0xFEE2), address(factory));
        assertEq(v2Factory.previousFactory(), address(factory));

        // The registries stay disjoint - the new factory never learns the old
        // one's rows - and the predecessor is accepted anyway, through the chain.
        assertFalse(v2Factory.isDeployed(address(nft)));
        assertTrue(v2Factory.isCanonicalPredecessor(address(nft)));

        vm.prank(developer);
        address v2 =
            v2Factory.deployAccess(_paramsWithPredecessor(_saleEthOnly(PRICE), address(nft)));

        assertTrue(v2Factory.isDeployed(v2));
        assertEq(Rub3Access(v2).feeBps(), 300, "and it carries the NEW factory's terms");
    }

    /// Being a factory is not what carries recognition forward - the pointer is.
    /// An unlinked new factory recognises nothing the old one deployed.
    function test_predecessor_unlinkedFactoryInheritsNothing() public {
        Rub3Factory unlinked = new Rub3Factory(300, address(0xFEE2), address(0));
        assertFalse(unlinked.isCanonicalPredecessor(address(nft)));

        vm.prank(developer);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Factory.PredecessorNotCanonical.selector, address(nft))
        );
        unlinked.deployAccess(_paramsWithPredecessor(_saleEthOnly(PRICE), address(nft)));
    }

    /// The walk is bounded, and the bound is checked at its edge rather than
    /// asserted: the factory exactly `MAX_PREDECESSOR_FACTORY_HOPS` generations
    /// later still reaches the oldest registry, and the next one does not.
    function test_predecessor_chainWalkStopsAtTheDocumentedBound() public {
        uint256 hops = factory.MAX_PREDECESSOR_FACTORY_HOPS();

        // `factory` is generation 0; build one generation past the bound on it.
        Rub3Factory[] memory chain = new Rub3Factory[](hops + 2);
        chain[0] = factory;
        for (uint256 i = 1; i < chain.length; i++) {
            chain[i] = new Rub3Factory(FEE_BPS, treasury, address(chain[i - 1]));
        }

        assertTrue(
            chain[hops].isCanonicalPredecessor(address(nft)),
            "the oldest registry is still reachable at exactly the bound"
        );
        assertFalse(
            chain[hops + 1].isCanonicalPredecessor(address(nft)),
            "one generation further back it is not"
        );

        // What the bound drops is only the far end: the nearer generations are
        // untouched by it.
        vm.prank(developer);
        address recent = chain[hops].deployAccess(_params(_saleEthOnly(PRICE)));
        assertTrue(chain[hops + 1].isCanonicalPredecessor(recent));
    }

    function test_factory_rejectsAPreviousFactoryThatIsNotAContract() public {
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Factory.IncompatiblePreviousFactory.selector, developer)
        );
        new Rub3Factory(FEE_BPS, treasury, developer);
    }

    /// Half a factory is worse than no factory: it would construct fine and then
    /// revert every predecessor-bearing deploy for as long as it existed, with
    /// the pointer immutable and no remedy but redeploying. The probe reads both
    /// views the walk uses, at the only moment either can still be corrected -
    /// the same reasoning that makes {Rub3License} probe its predecessor.
    function test_factory_rejectsAPreviousFactoryMissingHalfTheWalk() public {
        HalfFactory half = new HalfFactory();
        vm.expectRevert(
            abi.encodeWithSelector(Rub3Factory.IncompatiblePreviousFactory.selector, address(half))
        );
        new Rub3Factory(FEE_BPS, treasury, address(half));

        // And a contract that answers neither view.
        RejectingTreasury notAFactory = new RejectingTreasury();
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3Factory.IncompatiblePreviousFactory.selector, address(notAFactory)
            )
        );
        new Rub3Factory(FEE_BPS, treasury, address(notAFactory));
    }

    /// Direct deploys are untouched and may still name any predecessor at all.
    /// They get no registry row, so there is nothing to launder onto.
    function test_predecessor_directDeployMayStillNameAnything() public {
        Rub3Access shadow = _deployDirect(PRICE);

        vm.prank(developer);
        Rub3Access successor = new Rub3Access(
            "Direct v2",
            "DIR2",
            _identity(),
            _hashes(WRAPPER_HASH),
            _saleEthOnly(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(shadow),
            developer
        );

        assertEq(successor.predecessor(), address(shadow));
        assertFalse(factory.isDeployed(address(successor)));
        assertFalse(factory.isCanonicalPredecessor(address(shadow)));
    }

    /// So is the deployer helper, deliberately: it is permissionless and
    /// records nothing, so a licence it produces carries no `isDeployed` row and
    /// none of the standing the laundering route was after. The guard belongs
    /// where the registry row is granted, which is the factory.
    function test_predecessor_deployerHelperIsUnconstrainedAndUnrecorded() public {
        Rub3Access shadow = _deployDirect(PRICE);
        Rub3AccessDeployer helper = Rub3AccessDeployer(factory.accessDeployer());

        Rub3LicenseParams memory p = _paramsWithPredecessor(_saleEthOnly(PRICE), address(shadow));
        p.owner = developer; // the owner default lives on the factory, not here

        vm.prank(developer);
        address viaHelper = helper.deploy(p, _noFee());

        assertEq(Rub3Access(viaHelper).predecessor(), address(shadow));
        assertFalse(factory.isDeployed(viaHelper), "and it gets no registry row");
    }
}
