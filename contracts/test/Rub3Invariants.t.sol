// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Rub3Access} from "../src/Rub3Access.sol";
import {Rub3Factory} from "../src/Rub3Factory.sol";
import {Rub3License} from "../src/Rub3License.sol";
import {MockEIP3009Token} from "./mocks/MockEIP3009Token.sol";

/// @notice The ownership-invariant suite for implementation.md §2.4.
///
/// Every test here exists to fail loudly if somebody later reintroduces a way to
/// take back something already granted. Three groups:
///
///   1. **Append-only wrapper hash set** - old releases stay verifiable forever,
///      compromised builds are flagged with a stated reason, and hash status is
///      structurally unable to reach token validity.
///   2. **Successor pattern** - the three hard guarantees, each with a test that
///      fails if the guarantee is removed.
///   3. **No-revocation audit** - the machine-checkable claim itself: the burn /
///      admin-transfer / pause selectors are absent from the deployed bytecode,
///      and the contract owner exercising every power it *does* have cannot
///      disturb an issued token.
contract Rub3InvariantsTest is Test {
    Rub3Access internal nft;

    address internal owner = address(0xA11CE);
    address internal alice = address(0xA);
    address internal bob = address(0xB);
    address internal attacker = address(0xBAD);

    bytes32 internal constant HASH_V1 = keccak256("wrapper-v1-darwin-arm64");
    bytes32 internal constant HASH_V2 = keccak256("wrapper-v1-linux-x86_64");
    bytes32 internal constant HASH_V3 = keccak256("wrapper-v2-darwin-arm64");

    uint256 internal constant PRICE = 0.05 ether;
    uint256 internal constant COOLDOWN_BLOCKS = 15;

    /// Stands in for USDC wherever an owner power over the stablecoin rail has
    /// to be shown not to reach an issued token. The rail's own behaviour is
    /// covered in `Rub3TokenPurchase.t.sol`.
    MockEIP3009Token internal usdc;

    /// ETH-only sale terms - what every fixture below except the stablecoin
    /// suite deploys with.
    function _sale(uint256 price) internal pure returns (Rub3License.SaleTerms memory) {
        return Rub3License.SaleTerms({price: price, priceToken: address(0), priceAmount: 0});
    }

    function setUp() public {
        usdc = new MockEIP3009Token();
        nft = new Rub3Access(
            "Rub3 Test",
            "R3T",
            _identity(0, address(0)),
            _hashes(HASH_V1, HASH_V2),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            owner
        );
        vm.deal(alice, 10 ether);
        vm.deal(bob, 10 ether);
        vm.deal(attacker, 10 ether);
    }

    // ── Fixtures ──────────────────────────────────────────────────────────────

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

    function _hashes(bytes32 a) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](1);
        out[0] = a;
    }

    function _hashes(bytes32 a, bytes32 b) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](2);
        out[0] = a;
        out[1] = b;
    }

    function _mint(address to) internal returns (uint256 id) {
        vm.prank(to);
        id = nft.purchase{value: PRICE}(to);
    }

    /// A second `Rub3Access` that declares `predecessor` and therefore accepts
    /// migrations from it. Both sides opt in: the successor at deploy, the
    /// predecessor via {Rub3License-setSuccessor}.
    function _deploySuccessor(address predecessor_) internal returns (Rub3Access) {
        return new Rub3Access(
            "Rub3 Test v2",
            "R3T2",
            _identity(0, address(0)),
            _hashes(HASH_V3),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            predecessor_,
            owner
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 1. Append-only wrapper hash set
    // ══════════════════════════════════════════════════════════════════════════

    function test_hashSet_seededByConstructor() public view {
        assertEq(nft.wrapperHashCount(), 2);
        assertEq(nft.wrapperHashAt(0), HASH_V1);
        assertEq(nft.wrapperHashAt(1), HASH_V2);
        assertTrue(nft.isWrapperHashValid(HASH_V1));
        assertTrue(nft.isWrapperHashValid(HASH_V2));
        assertEq(uint8(nft.wrapperHashes(HASH_V3)), uint8(Rub3License.HashStatus.Unknown));
    }

    function test_hashSet_constructorRejectsZeroHash() public {
        vm.expectRevert(Rub3License.ZeroWrapperHash.selector);
        new Rub3Access(
            "x",
            "x",
            _identity(0, address(0)),
            _hashes(bytes32(0)),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            owner
        );
    }

    function test_hashSet_constructorRejectsDuplicate() public {
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.WrapperHashAlreadyKnown.selector, HASH_V1)
        );
        new Rub3Access(
            "x",
            "x",
            _identity(0, address(0)),
            _hashes(HASH_V1, HASH_V1),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            owner
        );
    }

    function test_hashSet_emptySetIsAllowed() public {
        Rub3Access bare = new Rub3Access(
            "x",
            "x",
            _identity(0, address(0)),
            new bytes32[](0),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            address(0),
            owner
        );
        assertEq(bare.wrapperHashCount(), 0);
        assertEq(bare.wrapperHashList().length, 0);
    }

    /// The whole point of a set: shipping v2 does not un-verify v1.
    function test_hashSet_olderReleasesStayValidForever() public {
        vm.prank(owner);
        nft.addWrapperHash(HASH_V3);

        assertTrue(nft.isWrapperHashValid(HASH_V1), "v1 must survive a v2 release");
        assertTrue(nft.isWrapperHashValid(HASH_V2), "v1/linux must survive a v2 release");
        assertTrue(nft.isWrapperHashValid(HASH_V3));
        assertEq(nft.wrapperHashCount(), 3);

        bytes32[] memory list = nft.wrapperHashList();
        assertEq(list.length, 3);
        assertEq(list[0], HASH_V1);
        assertEq(list[2], HASH_V3);
    }

    function test_addWrapperHash_emitsEvent() public {
        vm.expectEmit(true, false, false, false);
        emit Rub3License.WrapperHashAdded(HASH_V3);
        vm.prank(owner);
        nft.addWrapperHash(HASH_V3);
    }

    function test_addWrapperHash_rejectsZero() public {
        vm.prank(owner);
        vm.expectRevert(Rub3License.ZeroWrapperHash.selector);
        nft.addWrapperHash(bytes32(0));
    }

    function test_addWrapperHash_rejectsDuplicate() public {
        vm.prank(owner);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.WrapperHashAlreadyKnown.selector, HASH_V1)
        );
        nft.addWrapperHash(HASH_V1);
    }

    /// Append-only means status is monotone: `Revoked` is terminal. Allowing a
    /// re-add would make the set rewritable and the audit meaningless.
    function test_addWrapperHash_cannotResurrectRevokedHash() public {
        vm.startPrank(owner);
        nft.revokeWrapperHash(HASH_V1, "key material leaked in CI logs");

        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.WrapperHashAlreadyKnown.selector, HASH_V1)
        );
        nft.addWrapperHash(HASH_V1);
        vm.stopPrank();

        assertEq(uint8(nft.wrapperHashes(HASH_V1)), uint8(Rub3License.HashStatus.Revoked));
    }

    function test_addWrapperHash_notOwner_reverts() public {
        vm.prank(attacker);
        vm.expectRevert();
        nft.addWrapperHash(HASH_V3);
    }

    function test_revokeWrapperHash_recordsStatusAndReason() public {
        string memory reason = "build server compromised 2026-08-01; rebuild from tag v1.0.1";

        vm.expectEmit(true, false, false, true);
        emit Rub3License.WrapperHashRevoked(HASH_V1, reason);

        vm.prank(owner);
        nft.revokeWrapperHash(HASH_V1, reason);

        assertEq(uint8(nft.wrapperHashes(HASH_V1)), uint8(Rub3License.HashStatus.Revoked));
        assertFalse(nft.isWrapperHashValid(HASH_V1));
        assertEq(nft.revocationReason(HASH_V1), reason);

        // Still enumerable - revocation is a flag, never a deletion.
        assertEq(nft.wrapperHashCount(), 2);
        assertEq(nft.wrapperHashAt(0), HASH_V1);
    }

    function test_revokeWrapperHash_requiresAReason() public {
        vm.prank(owner);
        vm.expectRevert(Rub3License.RevocationReasonRequired.selector);
        nft.revokeWrapperHash(HASH_V1, "");
    }

    function test_revokeWrapperHash_unknownHash_reverts() public {
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.WrapperHashNotValid.selector, HASH_V3));
        nft.revokeWrapperHash(HASH_V3, "never shipped");
    }

    function test_revokeWrapperHash_twice_reverts() public {
        vm.startPrank(owner);
        nft.revokeWrapperHash(HASH_V1, "compromised");
        vm.expectRevert(abi.encodeWithSelector(Rub3License.WrapperHashNotValid.selector, HASH_V1));
        nft.revokeWrapperHash(HASH_V1, "compromised again");
        vm.stopPrank();
    }

    function test_revokeWrapperHash_notOwner_reverts() public {
        vm.prank(attacker);
        vm.expectRevert();
        nft.revokeWrapperHash(HASH_V1, "not yours to revoke");
    }

    /// ── Acceptance criterion ──────────────────────────────────────────────────
    /// Revoking a *binary hash* must never touch *token validity*. Revoke every
    /// hash in the set - including the one the holder bought under - and the
    /// token is untouched: still owned, still activatable, still valid.
    function test_revokedHash_doesNotAffectIssuedToken() public {
        uint256 id = _mint(alice);

        vm.prank(alice);
        uint256 firstSession = nft.activate(id);

        vm.startPrank(owner);
        nft.revokeWrapperHash(HASH_V1, "supply chain compromise");
        nft.revokeWrapperHash(HASH_V2, "supply chain compromise");
        vm.stopPrank();

        // Every hash the contract knows about is now revoked.
        assertFalse(nft.isWrapperHashValid(HASH_V1));
        assertFalse(nft.isWrapperHashValid(HASH_V2));

        // ownerOf: unchanged.
        assertEq(nft.ownerOf(id), alice);
        assertEq(nft.balanceOf(alice), 1);

        // activate: still works, still cooldown-gated on nothing but time.
        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        uint256 secondSession = nft.activate(id);
        assertGt(secondSession, firstSession);

        // transfer: still works - the entitlement remains a tradable asset.
        vm.prank(alice);
        nft.transferFrom(alice, bob, id);
        assertEq(nft.ownerOf(id), bob);
    }

    /// A token can be purchased and activated on a contract whose entire hash
    /// set is revoked - the chain never gates issuance or activation on binary
    /// trust. (Binary trust is the wrapper's job, and it fails closed locally.)
    function test_revokedHash_doesNotBlockNewPurchaseOrActivation() public {
        vm.startPrank(owner);
        nft.revokeWrapperHash(HASH_V1, "compromised");
        nft.revokeWrapperHash(HASH_V2, "compromised");
        vm.stopPrank();

        uint256 id = _mint(bob);
        vm.prank(bob);
        assertEq(nft.activate(id), 1);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 2. Successor pattern - three hard guarantees
    // ══════════════════════════════════════════════════════════════════════════

    // ── Guarantee 1: the old contract validates its tokens forever ────────────

    /// Fails if anyone ever makes validation consult `successor`.
    function test_successor_oldContractValidatesForever() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));

        vm.prank(owner);
        nft.setSuccessor(address(v2));

        // Pointer set - nothing about the old token moved.
        assertEq(nft.successor(), address(v2));
        assertEq(nft.ownerOf(id), alice);
        assertTrue(nft.honorsContract(address(nft), id));

        (bool ready,) = nft.cooldownReady(id);
        assertTrue(ready);

        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        assertEq(nft.activate(id), 1, "old contract must still activate its tokens");

        // The holder migrates. The old token is still theirs and still works.
        vm.prank(alice);
        v2.claimFromPredecessor(id);

        assertEq(nft.ownerOf(id), alice, "claiming must not move the old token");
        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        assertEq(nft.activate(id), 2, "old contract must still activate after migration");

        // Even repointed at a dead address, or cleared entirely.
        vm.prank(owner);
        nft.setSuccessor(address(0xDEAD));
        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        nft.activate(id);

        vm.prank(owner);
        nft.setSuccessor(address(0));
        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        nft.activate(id);
        assertEq(nft.ownerOf(id), alice);
    }

    /// Vendor death: the owner walks away entirely and the token still works.
    function test_successor_survivesRenouncedOwnership() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));

        vm.startPrank(owner);
        nft.setSuccessor(address(v2));
        nft.renounceOwnership();
        vm.stopPrank();

        assertEq(nft.owner(), address(0));
        assertEq(nft.ownerOf(id), alice);

        vm.prank(alice);
        assertEq(nft.activate(id), 1);

        // And the migration route stays open, because the successor pointer was
        // already set and nothing about a claim needs a live owner.
        vm.prank(alice);
        uint256 newId = v2.claimFromPredecessor(id);
        assertEq(v2.ownerOf(newId), alice);
    }

    function test_setSuccessor_onlyOwner() public {
        Rub3Access v2 = _deploySuccessor(address(nft));

        vm.prank(attacker);
        vm.expectRevert();
        nft.setSuccessor(address(v2));

        vm.prank(owner);
        nft.setSuccessor(address(v2));
        assertEq(nft.successor(), address(v2));
    }

    function test_setSuccessor_rejectsSelf() public {
        vm.prank(owner);
        vm.expectRevert(Rub3License.SelfReference.selector);
        nft.setSuccessor(address(nft));
    }

    function test_constructor_rejectsSelfPredecessor() public {
        // Predict the address this deploy will land at, then hand it to itself
        // as `predecessor` - a cycle that would make `honorsContract` answer for
        // a contract that is its own ancestor.
        address next = vm.computeCreateAddress(address(this), vm.getNonce(address(this)));

        vm.expectRevert(Rub3License.SelfReference.selector);
        new Rub3Access(
            "x",
            "x",
            _identity(0, address(0)),
            _hashes(HASH_V1),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            next,
            owner
        );
    }

    // ── Guarantee 2: migration is holder-initiated, never forced ──────────────

    /// Fails if anyone adds a push-migration path. Nobody but the holder - not
    /// the old contract's owner, not the new contract's owner, not a third
    /// party - can cause a claim.
    function test_migration_isHolderInitiatedOnly() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        // The predecessor's owner cannot migrate on Alice's behalf.
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, owner, alice));
        v2.claimFromPredecessor(id);

        // Neither can a stranger.
        vm.prank(attacker);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, attacker, alice));
        v2.claimFromPredecessor(id);

        // Nor can the successor's owner (same address here - same rejection).
        vm.prank(bob);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, bob, alice));
        v2.claimFromPredecessor(id);

        // Only Alice, and only by calling it herself.
        vm.prank(alice);
        uint256 newId = v2.claimFromPredecessor(id);
        assertEq(v2.ownerOf(newId), alice);
        assertTrue(v2.wasClaimed(newId));
        assertEq(v2.claimedFromTokenId(newId), id);
    }

    /// Snapshot-claim, not burn-to-mint: the old token is neither destroyed nor
    /// moved, because the old contract exposes no way to do either. The holder
    /// ends up with both.
    function test_migration_leavesPredecessorTokenUntouched() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        uint256 supplyBefore = nft.totalSupply();

        vm.prank(alice);
        uint256 newId = v2.claimFromPredecessor(id);

        assertEq(nft.ownerOf(id), alice);
        assertEq(nft.balanceOf(alice), 1);
        assertEq(nft.totalSupply(), supplyBefore, "no burn on the predecessor");
        assertEq(v2.ownerOf(newId), alice);
        assertEq(v2.balanceOf(alice), 1);
        assertTrue(v2.predecessorTokenClaimed(id));
    }

    /// After a holder transfers away, the *new* holder is the one who may claim.
    function test_migration_followsCurrentHolder() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        vm.prank(alice);
        nft.transferFrom(alice, bob, id);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, alice, bob));
        v2.claimFromPredecessor(id);

        vm.prank(bob);
        uint256 newId = v2.claimFromPredecessor(id);
        assertEq(v2.ownerOf(newId), bob);
    }

    function test_migration_requiresPredecessorToOptIn() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));

        // The predecessor has not pointed at v2 yet.
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.SuccessorNotDeclared.selector, address(0))
        );
        v2.claimFromPredecessor(id);

        vm.prank(owner);
        nft.setSuccessor(address(v2));
        vm.prank(alice);
        v2.claimFromPredecessor(id);
    }

    function test_migration_requiresSuccessorToOptIn() public {
        uint256 id = _mint(alice);
        // Deployed without declaring a predecessor - a paid major version, say.
        Rub3Access v2 = _deploySuccessor(address(0));

        vm.prank(owner);
        nft.setSuccessor(address(v2));

        vm.prank(alice);
        vm.expectRevert(Rub3License.NoPredecessor.selector);
        v2.claimFromPredecessor(id);
    }

    function test_migration_oncePerPredecessorToken() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        vm.startPrank(alice);
        v2.claimFromPredecessor(id);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.PredecessorTokenAlreadyClaimed.selector, id)
        );
        v2.claimFromPredecessor(id);
        vm.stopPrank();

        // Nor may a later holder of the same token claim it a second time.
        vm.prank(alice);
        nft.transferFrom(alice, bob, id);
        vm.prank(bob);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.PredecessorTokenAlreadyClaimed.selector, id)
        );
        v2.claimFromPredecessor(id);
    }

    function test_migration_emitsClaimedEvent() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        vm.expectEmit(true, true, true, true);
        emit Rub3License.Claimed(address(nft), id, 0, alice);
        vm.prank(alice);
        v2.claimFromPredecessor(id);
    }

    // ── Guarantee 3: the wrapper's trust rule ─────────────────────────────────

    /// "contract X, or X's successor holding a token claimed from X" - the whole
    /// rule, evaluated on-chain in one call.
    function test_trustRule_honorsContract() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        // Arm one: the configured contract itself.
        assertTrue(nft.honorsContract(address(nft), id));

        vm.prank(alice);
        uint256 claimedId = v2.claimFromPredecessor(id);

        // Arm two: the successor, holding a token claimed from the configured
        // contract.
        assertTrue(v2.honorsContract(address(nft), claimedId));
        assertTrue(v2.honorsContract(address(v2), claimedId));

        // A token *bought* on the successor is not a claim, so a wrapper pinned
        // to the old contract does not accept it.
        vm.prank(bob);
        uint256 boughtId = v2.purchase{value: PRICE}(bob);
        assertFalse(v2.honorsContract(address(nft), boughtId));
        assertTrue(v2.honorsContract(address(v2), boughtId));

        // Unrelated contracts, the zero address, and tokens that do not exist.
        assertFalse(v2.honorsContract(address(0xC0FFEE), claimedId));
        assertFalse(v2.honorsContract(address(0), claimedId));
        assertFalse(v2.honorsContract(address(nft), 999));
        assertFalse(nft.honorsContract(address(nft), 999));
    }

    /// A claim, once made, is a grant. Repointing the predecessor's successor
    /// afterwards must not retroactively unmake it - which is why the successor
    /// records the check at claim time rather than re-reading it here.
    function test_trustRule_survivesSuccessorRepoint() public {
        uint256 id = _mint(alice);
        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        vm.prank(alice);
        uint256 claimedId = v2.claimFromPredecessor(id);
        assertTrue(v2.honorsContract(address(nft), claimedId));

        // The developer changes its mind and points somewhere else entirely.
        vm.prank(owner);
        nft.setSuccessor(address(0xDEAD));
        assertTrue(v2.honorsContract(address(nft), claimedId), "a claim already made is a grant");

        vm.prank(owner);
        nft.setSuccessor(address(0));
        assertTrue(v2.honorsContract(address(nft), claimedId));

        // And the claimed token keeps working on its own terms.
        vm.prank(alice);
        assertEq(v2.activate(claimedId), 1);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 3. No-revocation audit
    // ══════════════════════════════════════════════════════════════════════════

    /// @dev Scans deployed runtime bytecode for a 4-byte selector constant.
    ///      solc emits every external function's selector as a literal PUSH4 in
    ///      the dispatcher, so absence from the code is absence from the ABI.
    function _bytecodeHasSelector(address target, bytes4 sel) internal view returns (bool) {
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

    /// @dev Asserts `signature` is not callable on `target`, two independent ways:
    ///      the selector is absent from the runtime bytecode, and a raw call
    ///      carrying it reverts (no fallback exists to swallow it).
    function _assertNoFunction(address target, string memory signature) internal {
        bytes4 sel = bytes4(keccak256(bytes(signature)));
        assertFalse(
            _bytecodeHasSelector(target, sel),
            string.concat("selector present in bytecode: ", signature)
        );
        (bool ok,) = target.call(abi.encodePacked(sel, new bytes(128)));
        assertFalse(ok, string.concat("call did not revert: ", signature));
    }

    /// Positive control for the scanner: functions that *do* exist are found,
    /// and an unknown selector really does revert rather than hitting a
    /// fallback. Without this, the absence assertions below prove nothing.
    function test_audit_scannerIsSound() public {
        assertTrue(_bytecodeHasSelector(address(nft), bytes4(keccak256("activate(uint256)"))));
        assertTrue(_bytecodeHasSelector(address(nft), bytes4(keccak256("purchase(address)"))));
        assertTrue(_bytecodeHasSelector(address(nft), bytes4(keccak256("setPrice(uint256)"))));
        assertTrue(
            _bytecodeHasSelector(
                address(nft),
                bytes4(
                    keccak256(
                        "purchaseWithAuthorization(address,(address,uint256,uint256,bytes32,bytes))"
                    )
                )
            )
        );
        assertTrue(_bytecodeHasSelector(address(nft), bytes4(keccak256("ownerOf(uint256)"))));
        // The fee getters exist while every setter for them is absent, which is
        // the shape "immutable per contract" has in the bytecode.
        assertTrue(_bytecodeHasSelector(address(nft), bytes4(keccak256("feeBps()"))));
        assertTrue(_bytecodeHasSelector(address(nft), bytes4(keccak256("treasury()"))));

        // The same shape on the factory: the chain the canonical-predecessor
        // rule walks is readable, and nothing can repoint it.
        address factory = address(new Rub3Factory(250, address(0x7EA5), address(0)));
        assertTrue(_bytecodeHasSelector(factory, bytes4(keccak256("previousFactory()"))));
        assertTrue(
            _bytecodeHasSelector(factory, bytes4(keccak256("isCanonicalPredecessor(address)")))
        );

        // No fallback, no receive: an unknown selector and plain ETH both revert.
        (bool ok,) = address(nft).call(hex"deadbeef");
        assertFalse(ok, "unknown selector must revert");
        (bool paid,) = address(nft).call{value: 1 wei}("");
        assertFalse(paid, "contract must not accept bare ETH");
    }

    /// The machine-checkable claim itself: an agent can run exactly this list
    /// against a candidate contract's runtime bytecode before buying.
    function test_audit_noRevocationSurface() public {
        // The factory is audited alongside the licence contracts: it is where
        // the fee terms are chosen, so a setter there would unfreeze the
        // economics of every contract it has ever deployed just as surely as
        // one on the licence would.
        address[3] memory targets = [
            address(nft),
            address(_deploySuccessor(address(nft))),
            address(new Rub3Factory(250, address(0x7EA5), address(0)))
        ];

        string[25] memory forbidden = [
            // Burn - nothing may destroy an issued token.
            "burn(uint256)",
            "burn(address,uint256)",
            "burnFrom(address,uint256)",
            // Admin transfer / seizure - nothing may move a token its holder did
            // not consent to move.
            "adminTransfer(address,address,uint256)",
            "forceTransfer(address,address,uint256)",
            "seize(uint256)",
            "clawback(uint256)",
            // Pause - validation reads must never be switchable off.
            "pause()",
            "unpause()",
            "paused()",
            "setPaused(bool)",
            // Direct invalidation of a token.
            "revoke(uint256)",
            "revokeToken(uint256)",
            "invalidate(uint256)",
            // Proxies / upgrade hooks - code is frozen at deploy.
            "upgradeTo(address)",
            "upgradeToAndCall(address,bytes)",
            "initialize()",
            // The rotatable hash slot and any way to rewrite the set.
            "setWrapperHash(bytes32)",
            "removeWrapperHash(bytes32)",
            "unrevokeWrapperHash(bytes32)",
            // Forced migration, and repointing an immutable predecessor - on
            // the licence contract, and on the factory whose registry decides
            // which predecessors a canonical deploy may name at all.
            "forceMigrate(uint256,address)",
            "setPredecessor(address)",
            "setPreviousFactory(address)",
            // The protocol fee, on both sides of it. `feeBps` and `treasury`
            // are immutable on the licence contract *and* on the factory that
            // stamped them, which is what makes "a developer's economics can
            // never change after deploy" a property of the bytecode rather than
            // a promise.
            "setFeeBps(uint16)",
            "setTreasury(address)"
        ];

        for (uint256 t = 0; t < targets.length; t++) {
            for (uint256 i = 0; i < forbidden.length; i++) {
                _assertNoFunction(targets[t], forbidden[i]);
            }
        }
    }

    /// ERC-721 approval is the only transfer authority, and being the *contract*
    /// owner grants none of it.
    function test_audit_contractOwnerCannotMoveAHeldToken() public {
        uint256 id = _mint(alice);

        vm.prank(owner);
        vm.expectRevert();
        nft.transferFrom(alice, owner, id);

        vm.prank(owner);
        vm.expectRevert();
        nft.safeTransferFrom(alice, owner, id);

        vm.prank(owner);
        vm.expectRevert();
        nft.approve(owner, id);

        assertEq(nft.ownerOf(id), alice);
    }

    /// The owner exercises every power it has, at once, as hostilely as the ABI
    /// permits - and the issued token is untouched by all of it.
    function test_audit_ownerDoesItsWorst_tokenSurvives() public {
        uint256 id = _mint(alice);
        vm.prank(alice);
        uint256 s1 = nft.activate(id);

        Rub3Access v2 = _deploySuccessor(address(nft));

        vm.startPrank(owner);
        nft.setPrice(type(uint256).max); // price out every future buyer
        nft.addWrapperHash(HASH_V3); // ship a new build
        nft.revokeWrapperHash(HASH_V1, "burned"); // flag every old one
        nft.revokeWrapperHash(HASH_V2, "burned");
        nft.setSuccessor(address(v2)); // point elsewhere
        nft.setTokenPrice(address(usdc), type(uint256).max); // and on the other rail
        nft.withdraw(payable(owner)); // drain the balance
        nft.withdrawToken(address(usdc), owner); // both balances
        nft.transferOwnership(attacker); // hand the keys to an attacker
        vm.stopPrank();

        // The attacker now owns the contract and repeats the exercise.
        vm.startPrank(attacker);
        nft.setPrice(0);
        nft.setSuccessor(address(0xDEAD));
        nft.renounceOwnership();
        vm.stopPrank();

        assertEq(nft.ownerOf(id), alice);
        assertEq(nft.balanceOf(alice), 1);
        assertEq(nft.activeSessionId(id), s1);

        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        assertGt(nft.activate(id), s1, "activation must survive everything above");

        vm.prank(alice);
        nft.transferFrom(alice, bob, id);
        assertEq(nft.ownerOf(id), bob);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 4. Mint ordering and the predecessor probe
    //
    //    Both protect the same thing from a different side: a grant must be
    //    whole the moment it exists, and a migration promised at deploy must
    //    still be redeemable years later.
    // ══════════════════════════════════════════════════════════════════════════

    /// `_safeMint` hands control to a contract recipient while the token already
    /// exists, so any per-token state a mint path writes must be written first.
    /// The claim path is the one that writes some: a migrating holder's
    /// `wasClaimed` provenance and the predecessor token it came from are both
    /// in place before the callback, so `onERC721Received` can never observe a
    /// claimed token that does not yet say it was claimed.
    function test_mintOrdering_claimStateExistsBeforeRecipientCallback() public {
        MintCallbackProbe probe = new MintCallbackProbe();
        vm.deal(address(probe), 10 ether);
        probe.watch(nft);
        uint256 id = probe.buy(PRICE);
        assertTrue(probe.fired(), "the recipient callback must have run");

        Rub3Access v2 = _deploySuccessor(address(nft));
        vm.prank(owner);
        nft.setSuccessor(address(v2));

        uint256 newId = probe.claim(v2, id);

        assertTrue(probe.seenWasClaimed(), "claim provenance is recorded before the callback");
        assertEq(
            probe.seenClaimedFromTokenId(),
            id,
            "and so is the predecessor token it was claimed against"
        );
        assertTrue(v2.wasClaimed(newId));
    }

    /// The probe lives on the base contract, so an access license is guarded
    /// too: a `PREDECESSOR` typo pointing at an EOA or any codeless address is
    /// rejected at deploy rather than bricking every holder's claim on an
    /// immutable pointer.
    function test_predecessorProbe_accessRejectsCodelessPredecessor() public {
        assertEq(alice.code.length, 0);

        vm.expectRevert(abi.encodeWithSelector(Rub3License.IncompatiblePredecessor.selector, alice));
        new Rub3Access(
            "x",
            "x",
            _identity(0, address(0)),
            _hashes(HASH_V3),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            alice,
            owner
        );
    }

    /// And a contract that has code but is not a license contract: it cannot
    /// answer `successor()`, which is what {Rub3License-claimFromPredecessor}
    /// reads.
    function test_predecessorProbe_accessRejectsNonLicensePredecessor() public {
        address notALicense = address(new MintCallbackProbe());

        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncompatiblePredecessor.selector, notALicense)
        );
        new Rub3Access(
            "x",
            "x",
            _identity(0, address(0)),
            _hashes(HASH_V3),
            _sale(PRICE),
            _noFee(),
            0,
            COOLDOWN_BLOCKS,
            notALicense,
            owner
        );
    }

    /// A well-typed access predecessor still deploys, and still completes a
    /// claim end to end. The probe never reads the *value* of `successor()`,
    /// because the predecessor points here only after this deploy.
    function test_predecessorProbe_acceptsAccessPredecessor() public {
        uint256 id = _mint(alice);

        Rub3Access v2 = _deploySuccessor(address(nft));
        assertEq(v2.predecessor(), address(nft));
        assertEq(nft.successor(), address(0), "predecessor still points nowhere at deploy");

        vm.prank(owner);
        nft.setSuccessor(address(v2));

        vm.prank(alice);
        uint256 newId = v2.claimFromPredecessor(id);
        assertEq(v2.ownerOf(newId), alice);
        assertTrue(v2.wasClaimed(newId));
    }
}

/// @notice Records what a token looks like from inside `onERC721Received`, i.e.
///         while the mint that created it is still executing. Any per-token
///         state a mint path writes *after* `_safeMint` is invisible here.
contract MintCallbackProbe {
    Rub3Access public nft;

    bool public seenWasClaimed;
    uint256 public seenClaimedFromTokenId;
    bool public fired;

    function watch(Rub3Access nft_) external {
        nft = nft_;
    }

    function buy(uint256 value) external returns (uint256) {
        return nft.purchase{value: value}(address(this));
    }

    function claim(Rub3Access successor_, uint256 predecessorTokenId) external returns (uint256) {
        nft = successor_;
        return successor_.claimFromPredecessor(predecessorTokenId);
    }

    function onERC721Received(address, address, uint256 tokenId, bytes calldata)
        external
        returns (bytes4)
    {
        fired = true;
        seenWasClaimed = nft.wasClaimed(tokenId);
        seenClaimedFromTokenId = nft.claimedFromTokenId(tokenId);
        return this.onERC721Received.selector;
    }

    receive() external payable {}
}
