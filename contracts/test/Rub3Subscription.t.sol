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

    function setUp() public {
        nft = new Rub3Subscription(
            "Rub3 Sub", "R3S", IDENTITY, WRAPPER_HASH, PRICE, SUPPLY_CAP, PERIOD, COOLDOWN_BLOCKS, owner
        );
        vm.deal(alice, 10 ether);
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    function test_metadata() public view {
        assertEq(nft.period(),        PERIOD);
        assertEq(nft.identityModel(), IDENTITY);
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
}
