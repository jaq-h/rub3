// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Rub3Access} from "../src/Rub3Access.sol";
import {Rub3License} from "../src/Rub3License.sol";

contract Rub3AccessTest is Test {
    Rub3Access internal nft;

    address internal owner = address(0xA11CE);
    address internal alice = address(0xA);
    address internal bob = address(0xB);

    bytes32 internal constant WRAPPER_HASH = keccak256("test-wrapper-v1");
    uint256 internal constant PRICE = 0.05 ether;
    uint256 internal constant SUPPLY_CAP = 3;
    uint256 internal constant COOLDOWN_BLOCKS = 15; // == MIN_COOLDOWN_BLOCKS
    uint256 internal constant SESSION_TTL = 24 hours;
    uint8 internal constant IDENTITY = 0; // access
    address internal constant TBA_IMPL = address(0); // unused for access model
    address internal constant NO_PREDECESSOR = address(0); // accepts no migrations

    /// Session terms with the single-seat default: one concurrent session per
    /// token, which is the tier-3 licence seats generalise (§3.4).
    function _session() internal pure returns (Rub3License.SessionTerms memory) {
        return _session(1);
    }

    /// Session terms granting `seats` concurrent sessions per token.
    function _session(uint256 seats) internal pure returns (Rub3License.SessionTerms memory) {
        return Rub3License.SessionTerms({
            cooldownBlocks: COOLDOWN_BLOCKS,
            seatsPerToken: seats,
            sessionTtlSeconds: SESSION_TTL
        });
    }

    function _identity(uint8 model, address tbaImplementation)
        internal
        pure
        returns (Rub3License.IdentityTerms memory)
    {
        return Rub3License.IdentityTerms({model: model, tbaImplementation: tbaImplementation});
    }

    /// No protocol fee - what a direct (non-factory) deploy carries, and what
    /// every fixture in this suite uses. The fee split has its own suite in
    /// `Rub3Factory.t.sol`.
    function _noFee() internal pure returns (Rub3License.FeeTerms memory) {
        return Rub3License.FeeTerms({feeBps: 0, treasury: address(0)});
    }

    /// The constructor seeds the append-only hash set from an array; most
    /// fixtures want exactly one launch hash.
    function _hashes(bytes32 h) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](1);
        out[0] = h;
    }

    /// ETH-only sale terms - what every fixture below except the stablecoin
    /// suite deploys with.
    function _sale(uint256 price) internal pure returns (Rub3License.SaleTerms memory) {
        return Rub3License.SaleTerms({price: price, priceToken: address(0), priceAmount: 0});
    }

    function setUp() public {
        nft = new Rub3Access(
            "Rub3 Test",
            "R3T",
            _identity(IDENTITY, TBA_IMPL),
            _hashes(WRAPPER_HASH),
            _sale(PRICE),
            _noFee(),
            SUPPLY_CAP,
            _session(),
            NO_PREDECESSOR,
            owner
        );
        vm.deal(alice, 10 ether);
        vm.deal(bob, 10 ether);
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    function test_metadata() public view {
        assertEq(nft.identityModel(), IDENTITY);
        assertEq(nft.tbaImplementation(), TBA_IMPL);
        assertEq(uint8(nft.wrapperHashes(WRAPPER_HASH)), uint8(Rub3License.HashStatus.Valid));
        assertEq(nft.wrapperHashCount(), 1);
        assertEq(nft.wrapperHashAt(0), WRAPPER_HASH);
        assertEq(nft.price(), PRICE);
        assertEq(nft.supplyCap(), SUPPLY_CAP);
        assertEq(nft.owner(), owner);
    }

    function test_invalidIdentityModel_reverts() public {
        vm.expectRevert(abi.encodeWithSelector(Rub3License.InvalidIdentityModel.selector, 2));
        new Rub3Access(
            "x",
            "x",
            _identity(2, TBA_IMPL),
            _hashes(WRAPPER_HASH),
            _sale(PRICE),
            _noFee(),
            SUPPLY_CAP,
            _session(),
            NO_PREDECESSOR,
            owner
        );
    }

    function test_cooldownTooSmall_reverts() public {
        vm.expectRevert(abi.encodeWithSelector(Rub3License.CooldownTooSmall.selector, 14, 15));
        new Rub3Access(
            "x",
            "x",
            _identity(IDENTITY, TBA_IMPL),
            _hashes(WRAPPER_HASH),
            _sale(PRICE),
            _noFee(),
            SUPPLY_CAP,
            Rub3License.SessionTerms({
                cooldownBlocks: 14,
                seatsPerToken: 1,
                sessionTtlSeconds: SESSION_TTL
            }),
            NO_PREDECESSOR,
            owner
        );
    }

    function test_accessModel_rejectsNonZeroTbaImpl() public {
        vm.expectRevert(Rub3License.TbaImplementationForbidden.selector);
        new Rub3Access(
            "x",
            "x",
            _identity(0, address(0xBEEF)),
            _hashes(WRAPPER_HASH),
            _sale(PRICE),
            _noFee(),
            SUPPLY_CAP,
            _session(),
            NO_PREDECESSOR,
            owner
        );
    }

    function test_accountModel_requiresTbaImpl() public {
        vm.expectRevert(Rub3License.TbaImplementationRequired.selector);
        new Rub3Access(
            "x",
            "x",
            _identity(1, address(0)),
            _hashes(WRAPPER_HASH),
            _sale(PRICE),
            _noFee(),
            SUPPLY_CAP,
            _session(),
            NO_PREDECESSOR,
            owner
        );
    }

    function test_accountModel_acceptsTbaImpl() public {
        address impl = address(0xDEAD);
        Rub3Access acct = new Rub3Access(
            "Rub3 Acct",
            "R3A",
            _identity(1, impl),
            _hashes(WRAPPER_HASH),
            _sale(PRICE),
            _noFee(),
            SUPPLY_CAP,
            _session(),
            NO_PREDECESSOR,
            owner
        );
        assertEq(acct.identityModel(), 1);
        assertEq(acct.tbaImplementation(), impl);
    }

    function test_metadata_cooldownBlocks() public view {
        assertEq(nft.cooldownBlocks(), COOLDOWN_BLOCKS);
        assertEq(nft.MIN_COOLDOWN_BLOCKS(), 15);
    }

    function test_metadata_sessionTerms() public view {
        assertEq(nft.seatsPerToken(), 1);
        assertEq(nft.sessionTtlSeconds(), SESSION_TTL);
        assertEq(nft.MAX_SEATS(), 64);
        assertEq(nft.MIN_SESSION_TTL_SECONDS(), 5 minutes);
        assertEq(nft.MAX_SESSION_TTL_SECONDS(), 90 days);
    }

    // ── Purchase ──────────────────────────────────────────────────────────────

    function test_purchase_mintsSequentialIds() public {
        vm.prank(alice);
        uint256 id0 = nft.purchase{value: PRICE}(alice);
        vm.prank(bob);
        uint256 id1 = nft.purchase{value: PRICE}(bob);

        assertEq(id0, 0);
        assertEq(id1, 1);
        assertEq(nft.ownerOf(id0), alice);
        assertEq(nft.ownerOf(id1), bob);
        assertEq(nft.nextTokenId(), 2);
    }

    function test_purchase_zeroRecipientDefaultsToSender() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(address(0));
        assertEq(nft.ownerOf(id), alice);
    }

    /// The ETH rail takes the listed price exactly. Under is rejected, as it
    /// always was.
    function test_purchase_underpay_reverts() public {
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, PRICE - 1, PRICE)
        );
        nft.purchase{value: PRICE - 1}(alice);
    }

    /// And over is rejected too, with no refund path: a buyer whose price read
    /// went stale gets a failed transaction rather than a silent overpayment.
    /// See {Rub3License-_payEth}.
    function test_purchase_overpay_reverts() public {
        uint256 balanceBefore = alice.balance;

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, PRICE * 2, PRICE)
        );
        nft.purchase{value: PRICE * 2}(alice);

        assertEq(nft.nextTokenId(), 0);
        assertEq(address(nft).balance, 0);
        assertEq(alice.balance, balanceBefore);
    }

    /// The exact amount, and only the exact amount, mints.
    function test_purchase_exactPayment_succeeds() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);

        assertEq(nft.ownerOf(id), alice);
        assertEq(address(nft).balance, PRICE);
    }

    /// A one-wei tolerance either side would be a tolerance; there is none.
    function testFuzz_purchase_onlyTheListedPriceIsAccepted(uint256 sent) public {
        sent = bound(sent, 0, 1 ether);
        vm.deal(alice, 1 ether);

        vm.prank(alice);
        if (sent == PRICE) {
            nft.purchase{value: sent}(alice);
            assertEq(nft.nextTokenId(), 1);
        } else {
            vm.expectRevert(
                abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, sent, PRICE)
            );
            nft.purchase{value: sent}(alice);
            assertEq(nft.nextTokenId(), 0);
        }
    }

    function test_supplyCap_enforced() public {
        vm.startPrank(alice);
        nft.purchase{value: PRICE}(alice);
        nft.purchase{value: PRICE}(alice);
        nft.purchase{value: PRICE}(alice);
        vm.expectRevert(Rub3License.SoldOut.selector);
        nft.purchase{value: PRICE}(alice);
        vm.stopPrank();
    }

    // ── Enumeration (sanity check that ERC-721Enumerable wiring holds) ─────────

    function test_enumerable_tokensOfOwner() public {
        vm.startPrank(alice);
        uint256 a = nft.purchase{value: PRICE}(alice);
        uint256 b = nft.purchase{value: PRICE}(alice);
        vm.stopPrank();

        assertEq(nft.balanceOf(alice), 2);
        assertEq(nft.tokenOfOwnerByIndex(alice, 0), a);
        assertEq(nft.tokenOfOwnerByIndex(alice, 1), b);
    }

    // ── Owner controls ────────────────────────────────────────────────────────

    function test_setPrice_onlyOwner() public {
        vm.prank(alice);
        vm.expectRevert();
        nft.setPrice(1 ether);

        vm.prank(owner);
        nft.setPrice(1 ether);
        assertEq(nft.price(), 1 ether);
    }

    function test_addWrapperHash_onlyOwner() public {
        bytes32 newHash = keccak256("v2");

        vm.prank(alice);
        vm.expectRevert();
        nft.addWrapperHash(newHash);

        vm.prank(owner);
        nft.addWrapperHash(newHash);
        assertEq(uint8(nft.wrapperHashes(newHash)), uint8(Rub3License.HashStatus.Valid));

        // Appended, not replaced - the launch hash is still verifiable.
        assertEq(uint8(nft.wrapperHashes(WRAPPER_HASH)), uint8(Rub3License.HashStatus.Valid));
        assertEq(nft.wrapperHashCount(), 2);
    }

    function test_withdraw_transfersBalance() public {
        vm.prank(alice);
        nft.purchase{value: PRICE}(alice);

        uint256 before = owner.balance;
        vm.prank(owner);
        nft.withdraw(payable(owner));
        assertEq(owner.balance - before, PRICE);
        assertEq(address(nft).balance, 0);
    }

    // ── Activation / cooldown (tier 3) ────────────────────────────────────────

    function _mint(address to) internal returns (uint256 id) {
        vm.prank(to);
        id = nft.purchase{value: PRICE}(to);
    }

    function test_activate_firstCall_succeeds() public {
        uint256 id = _mint(alice);

        vm.expectEmit(true, true, false, true);
        emit Rub3License.Activated(id, alice, 1, 0, block.timestamp + SESSION_TTL);

        vm.prank(alice);
        uint256 sessionId = nft.activate(id);

        assertEq(sessionId, 1);
        assertEq(nft.lastActivationBlock(id), block.number);

        (bool live, uint256 index) = nft.sessionSeat(id, sessionId);
        assertTrue(live);
        assertEq(index, 0);
        assertEq(nft.seatsInUse(id), 1);
    }

    function test_activate_incrementsSessionId_acrossTokens() public {
        uint256 a = _mint(alice);
        uint256 b = _mint(bob);

        vm.prank(alice);
        uint256 s1 = nft.activate(a);
        vm.prank(bob);
        uint256 s2 = nft.activate(b);

        assertEq(s1, 1);
        assertEq(s2, 2);
    }

    /// **A single-seat licence is the tier-3 licence seats generalise**: its one
    /// seat is always the holder's to retake, so a second activation inside the
    /// window is refused by the cooldown and by nothing else.
    function test_activate_duringCooldown_reverts() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        nft.activate(id);

        // Advance one block - still inside cooldown window.
        vm.roll(block.number + 1);

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.CooldownActive.selector, COOLDOWN_BLOCKS - 1)
        );
        nft.activate(id);
    }

    /// And once the window has run, the new session replaces the old one on the
    /// same seat - which is exactly what `activate()` did before seats existed.
    function test_activate_afterCooldown_succeeds() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        uint256 s1 = nft.activate(id);

        vm.roll(block.number + COOLDOWN_BLOCKS);

        vm.prank(alice);
        uint256 s2 = nft.activate(id);

        assertGt(s2, s1);
        (bool live,) = nft.sessionSeat(id, s1);
        assertFalse(live, "the retaken seat no longer holds the old session");
        (live,) = nft.sessionSeat(id, s2);
        assertTrue(live, "it holds the new one");
        assertEq(nft.seatsInUse(id), 1, "one seat, still one session");
    }

    /// Releasing early gets the seat back at once, and it still costs the
    /// seat's own cooldown to take it again.
    function test_activate_afterReleaseInsideCooldown_reverts() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        uint256 s1 = nft.activate(id);

        vm.roll(block.number + 1);
        vm.prank(alice);
        nft.release(id, s1);
        assertEq(nft.seatsInUse(id), 0);

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.CooldownActive.selector, COOLDOWN_BLOCKS - 1)
        );
        nft.activate(id);
    }

    function test_activate_notOwner_reverts() public {
        uint256 id = _mint(alice);

        vm.prank(bob);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, bob, alice));
        nft.activate(id);
    }

    function test_activate_nonexistentToken_reverts() public {
        vm.prank(alice);
        vm.expectRevert(); // ERC721NonexistentToken from ownerOf
        nft.activate(999);
    }

    function test_cooldownReady_beforeFirstActivation() public {
        uint256 id = _mint(alice);
        (bool ready, uint256 remaining) = nft.cooldownReady(id);
        assertTrue(ready);
        assertEq(remaining, 0);
    }

    function test_cooldownReady_duringCooldown() public {
        uint256 id = _mint(alice);
        vm.prank(alice);
        nft.activate(id);

        vm.roll(block.number + 3);
        (bool ready, uint256 remaining) = nft.cooldownReady(id);
        assertFalse(ready);
        assertEq(remaining, COOLDOWN_BLOCKS - 3);
    }

    function test_cooldownReady_afterCooldown() public {
        uint256 id = _mint(alice);
        vm.prank(alice);
        nft.activate(id);

        vm.roll(block.number + COOLDOWN_BLOCKS);
        (bool ready, uint256 remaining) = nft.cooldownReady(id);
        assertTrue(ready);
        assertEq(remaining, 0);
    }

    function test_activate_afterTransfer_newOwnerIsAuthorized() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        nft.transferFrom(alice, bob, id);

        // Alice no longer authorized.
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, alice, bob));
        nft.activate(id);

        // Bob is, and gets a fresh session id on a seat of his own.
        vm.prank(bob);
        uint256 s = nft.activate(id);
        (bool live,) = nft.sessionSeat(id, s);
        assertTrue(live);
    }
}
