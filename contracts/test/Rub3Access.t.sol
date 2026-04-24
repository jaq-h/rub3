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

    bytes32 internal constant WRAPPER_HASH    = keccak256("test-wrapper-v1");
    uint256 internal constant PRICE           = 0.05 ether;
    uint256 internal constant SUPPLY_CAP      = 3;
    uint256 internal constant COOLDOWN_BLOCKS = 15; // == MIN_COOLDOWN_BLOCKS
    uint8   internal constant IDENTITY        = 0; // access
    address internal constant TBA_IMPL        = address(0); // unused for access model

    function setUp() public {
        nft = new Rub3Access(
            "Rub3 Test", "R3T", IDENTITY, TBA_IMPL,
            WRAPPER_HASH, PRICE, SUPPLY_CAP, COOLDOWN_BLOCKS, owner
        );
        vm.deal(alice, 10 ether);
        vm.deal(bob,   10 ether);
    }

    // ── Metadata ──────────────────────────────────────────────────────────────

    function test_metadata() public view {
        assertEq(nft.identityModel(),     IDENTITY);
        assertEq(nft.tbaImplementation(), TBA_IMPL);
        assertEq(nft.wrapperHash(),       WRAPPER_HASH);
        assertEq(nft.price(),             PRICE);
        assertEq(nft.supplyCap(),         SUPPLY_CAP);
        assertEq(nft.owner(),             owner);
    }

    function test_invalidIdentityModel_reverts() public {
        vm.expectRevert(abi.encodeWithSelector(Rub3License.InvalidIdentityModel.selector, 2));
        new Rub3Access("x", "x", 2, TBA_IMPL, WRAPPER_HASH, PRICE, SUPPLY_CAP, COOLDOWN_BLOCKS, owner);
    }

    function test_cooldownTooSmall_reverts() public {
        vm.expectRevert(abi.encodeWithSelector(Rub3License.CooldownTooSmall.selector, 14, 15));
        new Rub3Access("x", "x", IDENTITY, TBA_IMPL, WRAPPER_HASH, PRICE, SUPPLY_CAP, 14, owner);
    }

    function test_accessModel_rejectsNonZeroTbaImpl() public {
        vm.expectRevert(Rub3License.TbaImplementationForbidden.selector);
        new Rub3Access(
            "x", "x", 0, address(0xBEEF),
            WRAPPER_HASH, PRICE, SUPPLY_CAP, COOLDOWN_BLOCKS, owner
        );
    }

    function test_accountModel_requiresTbaImpl() public {
        vm.expectRevert(Rub3License.TbaImplementationRequired.selector);
        new Rub3Access(
            "x", "x", 1, address(0),
            WRAPPER_HASH, PRICE, SUPPLY_CAP, COOLDOWN_BLOCKS, owner
        );
    }

    function test_accountModel_acceptsTbaImpl() public {
        address impl = address(0xDEAD);
        Rub3Access acct = new Rub3Access(
            "Rub3 Acct", "R3A", 1, impl,
            WRAPPER_HASH, PRICE, SUPPLY_CAP, COOLDOWN_BLOCKS, owner
        );
        assertEq(acct.identityModel(),     1);
        assertEq(acct.tbaImplementation(), impl);
    }

    function test_metadata_cooldownBlocks() public view {
        assertEq(nft.cooldownBlocks(),      COOLDOWN_BLOCKS);
        assertEq(nft.MIN_COOLDOWN_BLOCKS(), 15);
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

    // ── Activation / cooldown (tier 3) ────────────────────────────────────────

    function _mint(address to) internal returns (uint256 id) {
        vm.prank(to);
        id = nft.purchase{value: PRICE}(to);
    }

    function test_activate_firstCall_succeeds() public {
        uint256 id = _mint(alice);

        vm.expectEmit(true, true, false, true);
        emit Rub3License.Activated(id, alice, 1);

        vm.prank(alice);
        uint256 sessionId = nft.activate(id);

        assertEq(sessionId, 1);
        assertEq(nft.activeSessionId(id), 1);
        assertEq(nft.lastActivationBlock(id), block.number);
    }

    function test_activate_incrementsSessionId_acrossTokens() public {
        uint256 a = _mint(alice);
        uint256 b = _mint(bob);

        vm.prank(alice); uint256 s1 = nft.activate(a);
        vm.prank(bob);   uint256 s2 = nft.activate(b);

        assertEq(s1, 1);
        assertEq(s2, 2);
    }

    function test_activate_duringCooldown_reverts() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        nft.activate(id);

        // Advance one block — still inside cooldown window.
        vm.roll(block.number + 1);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(
            Rub3License.CooldownActive.selector, COOLDOWN_BLOCKS - 1
        ));
        nft.activate(id);
    }

    function test_activate_afterCooldown_succeeds() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        uint256 s1 = nft.activate(id);

        vm.roll(block.number + COOLDOWN_BLOCKS);

        vm.prank(alice);
        uint256 s2 = nft.activate(id);

        assertGt(s2, s1);
        assertEq(nft.activeSessionId(id), s2);
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

        // Bob is, and gets a fresh session id.
        vm.prank(bob);
        uint256 s = nft.activate(id);
        assertEq(nft.activeSessionId(id), s);
    }

    // ── activateDevice / registeredDevice (tier 4) ────────────────────────────

    bytes32 internal constant DEVICE_A = keccak256("device-a");
    bytes32 internal constant DEVICE_B = keccak256("device-b");

    function test_registeredDevice_initiallyZero() public {
        uint256 id = _mint(alice);
        assertEq(nft.registeredDevice(id), bytes32(0));
    }

    function test_activateDevice_firstCall_recordsKeyAndSession() public {
        uint256 id = _mint(alice);

        vm.expectEmit(true, true, false, true);
        emit Rub3License.Activated(id, alice, 1);
        vm.expectEmit(true, false, false, true);
        emit Rub3License.DeviceRegistered(id, DEVICE_A);

        vm.prank(alice);
        uint256 sessionId = nft.activateDevice(id, DEVICE_A);

        assertEq(sessionId, 1);
        assertEq(nft.activeSessionId(id),     1);
        assertEq(nft.lastActivationBlock(id), block.number);
        assertEq(nft.registeredDevice(id),    DEVICE_A);
    }

    function test_activateDevice_zeroKey_reverts() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        vm.expectRevert(Rub3License.InvalidDevicePubKey.selector);
        nft.activateDevice(id, bytes32(0));
    }

    function test_activateDevice_notOwner_reverts() public {
        uint256 id = _mint(alice);

        vm.prank(bob);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, bob, alice));
        nft.activateDevice(id, DEVICE_A);
    }

    function test_activateDevice_duringCooldown_reverts() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        nft.activateDevice(id, DEVICE_A);

        vm.roll(block.number + 1);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(
            Rub3License.CooldownActive.selector, COOLDOWN_BLOCKS - 1
        ));
        nft.activateDevice(id, DEVICE_B);

        // Previous registration stands.
        assertEq(nft.registeredDevice(id), DEVICE_A);
    }

    function test_activateDevice_afterCooldown_overwritesPreviousKey() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        uint256 s1 = nft.activateDevice(id, DEVICE_A);

        vm.roll(block.number + COOLDOWN_BLOCKS);

        vm.prank(alice);
        uint256 s2 = nft.activateDevice(id, DEVICE_B);

        assertGt(s2, s1);
        assertEq(nft.registeredDevice(id), DEVICE_B);
    }

    function test_activate_thenActivateDevice_upgradesToTier4() public {
        // A token activated via tier-3 `activate()` should be upgradable to
        // tier 4 by the next `activateDevice()` call (after cooldown).
        uint256 id = _mint(alice);

        vm.prank(alice);
        nft.activate(id);
        assertEq(nft.registeredDevice(id), bytes32(0));

        vm.roll(block.number + COOLDOWN_BLOCKS);

        vm.prank(alice);
        nft.activateDevice(id, DEVICE_A);
        assertEq(nft.registeredDevice(id), DEVICE_A);
    }

    function test_activateDevice_thenActivate_doesNotClearKey() public {
        // Intentional: plain `activate()` leaves the device binding in place.
        // If the holder wanted to tear it down they would `activateDevice`
        // with a new key (or transfer away and the new owner would overwrite).
        uint256 id = _mint(alice);

        vm.prank(alice);
        nft.activateDevice(id, DEVICE_A);

        vm.roll(block.number + COOLDOWN_BLOCKS);

        vm.prank(alice);
        nft.activate(id);

        assertEq(nft.registeredDevice(id), DEVICE_A);
    }

    function test_activateDevice_sessionCounter_sharedWithActivate() public {
        // Both entry points feed the same monotonic `_sessionCounter`, so
        // mixing them across tokens must still yield distinct ids.
        uint256 a = _mint(alice);
        uint256 b = _mint(bob);

        vm.prank(alice); uint256 s1 = nft.activate(a);
        vm.prank(bob);   uint256 s2 = nft.activateDevice(b, DEVICE_A);

        assertEq(s1, 1);
        assertEq(s2, 2);
    }

    function test_activateDevice_afterTransfer_newOwnerReplacesKey() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        nft.activateDevice(id, DEVICE_A);

        vm.roll(block.number + COOLDOWN_BLOCKS);

        vm.prank(alice);
        nft.transferFrom(alice, bob, id);

        // Old holder no longer authorized.
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, alice, bob));
        nft.activateDevice(id, DEVICE_B);

        // New holder registers their own device, overwriting alice's.
        vm.prank(bob);
        nft.activateDevice(id, DEVICE_B);
        assertEq(nft.registeredDevice(id), DEVICE_B);
    }
}
