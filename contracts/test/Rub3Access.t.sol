// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test}           from "forge-std/Test.sol";
import {Rub3Access}     from "../src/Rub3Access.sol";
import {Rub3License}    from "../src/Rub3License.sol";

contract Rub3AccessTest is Test {
    Rub3Access internal nft;

    address internal owner = address(0xA11CE);
    address internal alice = address(0xA);
    address internal bob   = address(0xB);

    bytes32 internal constant WRAPPER_HASH = keccak256("test-wrapper-v1");
    uint256 internal constant PRICE        = 0.05 ether;
    uint256 internal constant SUPPLY_CAP   = 3;
    uint8   internal constant IDENTITY     = 0; // access

    function setUp() public {
        nft = new Rub3Access("Rub3 Test", "R3T", IDENTITY, WRAPPER_HASH, PRICE, SUPPLY_CAP, owner);
        vm.deal(alice, 10 ether);
        vm.deal(bob,   10 ether);
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    function test_metadata() public view {
        assertEq(nft.identityModel(), IDENTITY);
        assertEq(nft.wrapperHash(),   WRAPPER_HASH);
        assertEq(nft.price(),         PRICE);
        assertEq(nft.supplyCap(),     SUPPLY_CAP);
        assertEq(nft.owner(),         owner);
    }

    function test_invalidIdentityModel_reverts() public {
        vm.expectRevert(abi.encodeWithSelector(Rub3License.InvalidIdentityModel.selector, 2));
        new Rub3Access("x", "x", 2, WRAPPER_HASH, PRICE, SUPPLY_CAP, owner);
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

    function test_purchase_underpay_reverts() public {
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.InsufficientPayment.selector, PRICE - 1, PRICE));
        nft.purchase{value: PRICE - 1}(alice);
    }

    function test_purchase_overpay_accepted() public {
        vm.prank(alice);
        uint256 id = nft.purchase{value: PRICE * 2}(alice);
        assertEq(nft.ownerOf(id), alice);
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

    function test_setWrapperHash_onlyOwner() public {
        bytes32 newHash = keccak256("v2");
        vm.prank(owner);
        nft.setWrapperHash(newHash);
        assertEq(nft.wrapperHash(), newHash);
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
}
