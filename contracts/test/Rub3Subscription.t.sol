// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test}              from "forge-std/Test.sol";
import {Rub3Subscription}  from "../src/Rub3Subscription.sol";
import {Rub3License}       from "../src/Rub3License.sol";

contract Rub3SubscriptionTest is Test {
    Rub3Subscription internal nft;

    address internal owner = address(0xA11CE);
    address internal alice = address(0xA);

    bytes32 internal constant WRAPPER_HASH    = keccak256("sub-wrapper-v1");
    uint256 internal constant PRICE           = 0.01 ether;
    uint256 internal constant SUPPLY_CAP      = 0;            // uncapped
    uint256 internal constant PERIOD          = 30 days;
    uint256 internal constant COOLDOWN_BLOCKS = 15;
    uint8   internal constant IDENTITY        = 1;            // account (TBA)
    address internal constant TBA_IMPL        = address(0xBEEF); // any non-zero impl
    address internal constant NO_PREDECESSOR  = address(0);

    function _hashes(bytes32 h) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](1);
        out[0] = h;
    }

    function setUp() public {
        nft = new Rub3Subscription(
            "Rub3 Sub", "R3S", IDENTITY, TBA_IMPL,
            _hashes(WRAPPER_HASH), PRICE, SUPPLY_CAP, PERIOD, COOLDOWN_BLOCKS,
            NO_PREDECESSOR, owner
        );
        vm.deal(alice, 10 ether);
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    function test_metadata() public view {
        assertEq(nft.period(),            PERIOD);
        assertEq(nft.identityModel(),     IDENTITY);
        assertEq(nft.tbaImplementation(), TBA_IMPL);
    }

    // ── Purchase ──────────────────────────────────────────────────────────────

    function test_purchase_setsExpiresAt() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);

        assertEq(nft.expiresAt(id), block.timestamp + PERIOD);
        assertTrue(nft.isValid(id));
    }

    function test_isValid_falseAfterExpiry() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);

        vm.warp(block.timestamp + PERIOD + 1);
        assertFalse(nft.isValid(id));
    }

    // ── Renew ─────────────────────────────────────────────────────────────────

    function test_renew_stillValid_extendsFromCurrentExpiry() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);
        uint256 originalExpiry = nft.expiresAt(id);

        // Advance half a period, renew — expiry should be original + PERIOD.
        vm.warp(block.timestamp + PERIOD / 2);
        vm.prank(alice);
        nft.renew{value: PRICE}(id);

        assertEq(nft.expiresAt(id), originalExpiry + PERIOD);
    }

    function test_renew_afterExpiry_resetsFromNow() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);

        vm.warp(block.timestamp + PERIOD + 100);
        vm.prank(alice);
        nft.renew{value: PRICE}(id);

        assertEq(nft.expiresAt(id), block.timestamp + PERIOD);
    }

    function test_renew_nonexistentToken_reverts() public {
        vm.prank(alice);
        vm.expectRevert();
        nft.renew{value: PRICE}(999);
    }

    function test_renew_underpay_reverts() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.InsufficientPayment.selector, PRICE - 1, PRICE));
        nft.renew{value: PRICE - 1}(id);
    }

    // ── Per-token renewal snapshot (implementation.md §2.4) ───────────────────
    //
    // Renewal terms are the one thing a subscription carries past the sale, so
    // they are frozen per token at mint: `period` is immutable contract-wide and
    // `renewPrice[tokenId]` is written once, at mint, by `purchase`. `renew`
    // charges that snapshot. A developer can move `price` freely — it reaches
    // new buyers only.

    function test_renewPrice_snapshottedAtMint() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);
        assertEq(nft.renewPrice(id), PRICE);
        assertEq(nft.renewPrice(999), 0, "unminted tokens carry no snapshot");
    }

    /// A held subscription cannot be repriced by the contract owner.
    function test_renewPrice_ownerCannotRepriceHeldToken() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);
        uint256 expiry = nft.expiresAt(id);

        // The developer hikes the price 100x for new buyers.
        vm.prank(owner);
        nft.setPrice(PRICE * 100);

        assertEq(nft.price(), PRICE * 100);
        assertEq(nft.renewPrice(id), PRICE, "the held token's terms did not move");

        // Alice renews at her original price, and it goes through.
        vm.prank(alice);
        nft.renew{value: PRICE}(id);
        assertEq(nft.expiresAt(id), expiry + PERIOD);
    }

    /// The snapshot is what `renew` reads, not `price` — proved from the other
    /// direction: after a price *cut*, paying the new lower price is rejected
    /// against the token's own frozen price. Frozen terms are a fixed contract,
    /// not a best-price guarantee; a developer who wants to pass a cut on to
    /// existing holders deploys a successor and lets them claim onto it.
    function test_renewPrice_priceCutDoesNotRepriceHeldToken() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);

        vm.prank(owner);
        nft.setPrice(PRICE / 10);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(
            Rub3License.InsufficientPayment.selector, PRICE / 10, PRICE
        ));
        nft.renew{value: PRICE / 10}(id);

        vm.prank(alice);
        nft.renew{value: PRICE}(id);
        assertTrue(nft.isValid(id));
    }

    /// Two tokens minted either side of a price change each keep their own.
    function test_renewPrice_isPerToken() public {
        vm.prank(alice);
        uint256 cheap = nft.purchase{value: PRICE}(alice);

        vm.prank(owner);
        nft.setPrice(PRICE * 5);

        vm.prank(alice);
        uint256 dear = nft.purchase{value: PRICE * 5}(alice);

        assertEq(nft.renewPrice(cheap), PRICE);
        assertEq(nft.renewPrice(dear),  PRICE * 5);

        vm.startPrank(alice);
        nft.renew{value: PRICE}(cheap);

        vm.expectRevert(abi.encodeWithSelector(
            Rub3License.InsufficientPayment.selector, PRICE, PRICE * 5
        ));
        nft.renew{value: PRICE}(dear);

        nft.renew{value: PRICE * 5}(dear);
        vm.stopPrank();
    }

    /// Overpaying `price` at mint does not inflate the snapshot — it tracks the
    /// listed price, not what happened to be sent.
    function test_renewPrice_snapshotTracksListedPriceNotAmountPaid() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE * 3}(alice);
        assertEq(nft.renewPrice(id), PRICE);
    }

    function test_purchase_emitsSnapshotInEvent() public {
        vm.expectEmit(true, true, true, true);
        emit Rub3Subscription.Purchased(0, alice, alice, block.timestamp + PERIOD, PRICE);
        vm.prank(alice);
        nft.purchase{value: PRICE}(alice);
    }

    function test_renew_emitsPricePaid() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE}(alice);

        vm.expectEmit(true, false, false, true);
        emit Rub3Subscription.Renewed(id, nft.expiresAt(id) + PERIOD, PRICE);
        vm.prank(alice);
        nft.renew{value: PRICE}(id);
    }
}
