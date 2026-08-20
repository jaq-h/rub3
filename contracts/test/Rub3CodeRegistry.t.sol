// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Rub3CodeRegistry} from "../src/Rub3CodeRegistry.sol";

/// The append-only properties of {Rub3CodeRegistry}, asserted as behaviour.
///
/// The contract's guarantee is a negative one - nothing can ever unpublish,
/// overwrite, or invalidate a record - and a negative guarantee written only in
/// a comment is worth nothing. Each property below is either driven through a
/// call that must revert, or asserted against the deployed runtime bytecode for
/// the surfaces that must not exist at all.
contract Rub3CodeRegistryTest is Test {
    Rub3CodeRegistry internal registry;

    address internal owner = address(0xA11CE);
    address internal stranger = address(0xBAD);
    address internal nextKey = address(0xC0FFEE);

    bytes32 internal constant MCH_A = keccak256("masked code hash A");
    bytes32 internal constant MCH_B = keccak256("masked code hash B");
    bytes32 internal constant COMMIT = keccak256("a source commit");
    string internal constant SOLC = "0.8.28+commit.7893614a";
    string internal constant VERSION = "2026-08, exact payment on the ETH rail";

    event Published(
        bytes32 indexed maskedCodeHash,
        Rub3CodeRegistry.Role indexed role,
        uint256 indexed offsetTable,
        string contractName,
        string version,
        bytes32 sourceCommit,
        string solcVersion
    );
    event Deprecated(bytes32 indexed maskedCodeHash, string reason);
    event OffsetTableAdded(uint256 indexed index, Rub3CodeRegistry.ByteRange[] ranges);

    function setUp() public {
        registry = new Rub3CodeRegistry(owner);
    }

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// `count` well-formed 32-byte ranges, `stride` bytes apart. The shape of a
    /// real immutable table: one word each, ascending, disjoint.
    function _ranges(uint32 count, uint32 stride)
        internal
        pure
        returns (Rub3CodeRegistry.ByteRange[] memory out)
    {
        out = new Rub3CodeRegistry.ByteRange[](count);
        for (uint32 i = 0; i < count; i++) {
            out[i] = Rub3CodeRegistry.ByteRange({start: 64 + i * stride, length: 32});
        }
    }

    function _publish(bytes32 mch, Rub3CodeRegistry.Role role) internal {
        vm.prank(owner);
        registry.publish(mch, role, "Rub3Access", VERSION, COMMIT, SOLC, _ranges(3, 64));
    }

    /// `count` releases, each with an offset table no other release shares, so
    /// interning cannot collapse them. One 32-byte range per table, `i` words
    /// apart, which is the shape {publish} accepts and the shape a reader has to
    /// decode.
    function _publishDistinctTables(uint256 count) internal {
        for (uint256 i = 0; i < count; i++) {
            Rub3CodeRegistry.ByteRange[] memory table = new Rub3CodeRegistry.ByteRange[](1);
            table[0] = Rub3CodeRegistry.ByteRange({start: uint32(64 + i * 64), length: 32});
            vm.prank(owner);
            registry.publish(
                keccak256(abi.encode("a distinctly shaped release", i)),
                Rub3CodeRegistry.Role.Licence,
                "Rub3Access",
                VERSION,
                COMMIT,
                SOLC,
                table
            );
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 1. Publishing works, and records what it was told
    // ══════════════════════════════════════════════════════════════════════════

    function test_publish_recordsTheRelease() public {
        Rub3CodeRegistry.ByteRange[] memory ranges = _ranges(3, 64);

        vm.roll(1234);
        vm.prank(owner);
        registry.publish(
            MCH_A, Rub3CodeRegistry.Role.Licence, "Rub3Access", VERSION, COMMIT, SOLC, ranges
        );

        Rub3CodeRegistry.Release memory r = registry.record(MCH_A);
        assertEq(uint8(r.status), uint8(Rub3CodeRegistry.Status.Active));
        assertEq(uint8(r.role), uint8(Rub3CodeRegistry.Role.Licence));
        assertEq(r.contractName, "Rub3Access");
        assertEq(r.version, VERSION);
        assertEq(r.sourceCommit, COMMIT);
        assertEq(r.solcVersion, SOLC);
        assertEq(r.registeredAtBlock, 1234);
        assertEq(r.offsets.length, ranges.length);
        for (uint256 i = 0; i < ranges.length; i++) {
            assertEq(r.offsets[i].start, ranges[i].start);
            assertEq(r.offsets[i].length, ranges[i].length);
        }

        assertEq(registry.publishedCount(), 1);
        assertEq(registry.published()[0], MCH_A);
    }

    /// An unpublished hash reads as unknown rather than as anything actionable.
    /// The zero value of the enum is what a caller gets from a mapping miss, so
    /// this is the property that keeps a miss from looking like an answer.
    function test_record_unknownHashReadsAsUnknown() public view {
        Rub3CodeRegistry.Release memory r = registry.record(keccak256("never published"));
        assertEq(uint8(r.status), uint8(Rub3CodeRegistry.Status.Unknown));
        assertEq(r.offsets.length, 0);
        assertEq(r.registeredAtBlock, 0);
        assertEq(bytes(r.contractName).length, 0);
    }

    /// The block a record was published in is a fact the chain knows, so the
    /// caller never supplies it. Publishing the same release twice from two
    /// different blocks would otherwise be indistinguishable from backdating.
    function test_publish_blockIsRecordedNotSupplied() public {
        vm.roll(500);
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
        vm.roll(900);
        _publish(MCH_B, Rub3CodeRegistry.Role.Licence);

        assertEq(registry.record(MCH_A).registeredAtBlock, 500);
        assertEq(registry.record(MCH_B).registeredAtBlock, 900);
    }

    function test_publish_emitsThePermanentEvent() public {
        vm.expectEmit(true, true, true, true, address(registry));
        emit Published(MCH_A, Rub3CodeRegistry.Role.Licence, 0, "Rub3Access", VERSION, COMMIT, SOLC);
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
    }

    /// A record carries what a contract is *for*, because canonical rub3 code is
    /// not the same thing as a contract that sells licences. Without the role, a
    /// buyer pointed at the factory would read "canonical" and stop there.
    function test_publish_carriesTheRoleThroughUnchanged() public {
        // Enumerated by wire value up to the enum's own bound rather than from a
        // hand-written list, so a `Role` added later is covered the moment it
        // exists. A fixed-size literal was what let `DiscoveryRegistry` go
        // uncovered here while this test still passed.
        uint8 roleCount = uint8(type(Rub3CodeRegistry.Role).max) + 1;
        assertEq(roleCount, 5, "a new Role needs its wire number settled in code_registry_e2e too");

        for (uint8 raw = 0; raw < roleCount; raw++) {
            Rub3CodeRegistry.Role role = Rub3CodeRegistry.Role(raw);
            bytes32 mch = keccak256(abi.encode("role fixture", raw));
            _publish(mch, role);
            assertEq(uint8(registry.record(mch).role), raw);
        }
    }

    /// A contract with no immutables publishes an empty table, and that is a
    /// real answer rather than a missing one: the deployer helpers have none,
    /// and their code hashes directly.
    function test_publish_acceptsAnEmptyOffsetTable() public {
        Rub3CodeRegistry.ByteRange[] memory none = new Rub3CodeRegistry.ByteRange[](0);
        vm.prank(owner);
        registry.publish(
            MCH_A, Rub3CodeRegistry.Role.Deployer, "Rub3AccessDeployer", VERSION, COMMIT, SOLC, none
        );

        assertEq(registry.record(MCH_A).offsets.length, 0);
        assertEq(registry.offsetTableCount(), 1);
        assertEq(registry.offsetTables()[0].length, 0);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 2. Append-only: no republish, no overwrite, no removal
    // ══════════════════════════════════════════════════════════════════════════

    /// The load-bearing property. A record written once can never say something
    /// different later, so an agent that acted on an answer cannot be told
    /// afterwards that the answer was something else.
    function test_publish_rejectsRepublishingTheSameHash() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);

        vm.expectRevert(abi.encodeWithSelector(Rub3CodeRegistry.AlreadyPublished.selector, MCH_A));
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
    }

    /// Republishing with *different* content is the interesting case: it is what
    /// a rewrite would look like if one were possible, and the record must be
    /// untouched afterwards.
    function test_publish_cannotOverwriteAnExistingRecord() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);

        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(Rub3CodeRegistry.AlreadyPublished.selector, MCH_A));
        registry.publish(
            MCH_A,
            Rub3CodeRegistry.Role.Factory,
            "SomethingElse",
            "a different label",
            keccak256("a different commit"),
            "0.9.0",
            _ranges(1, 64)
        );

        Rub3CodeRegistry.Release memory r = registry.record(MCH_A);
        assertEq(uint8(r.role), uint8(Rub3CodeRegistry.Role.Licence));
        assertEq(r.contractName, "Rub3Access");
        assertEq(r.sourceCommit, COMMIT);
        assertEq(r.offsets.length, 3);
    }

    /// A deprecated record cannot be republished either. Deprecation is not a
    /// slot being freed up.
    function test_publish_rejectsRepublishingADeprecatedHash() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
        vm.prank(owner);
        registry.deprecate(MCH_A, "superseded");

        vm.expectRevert(abi.encodeWithSelector(Rub3CodeRegistry.AlreadyPublished.selector, MCH_A));
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
    }

    /// Removal is absent from the bytecode, not merely unused - the same
    /// standard `test/Rub3Invariants.t.sol` holds the licence contracts to.
    ///
    /// The list is this contract's own, because the licence contracts' forbidden
    /// list is about tokens and says nothing about a registry. What would undo
    /// this contract's guarantee is a way to remove a record, to rewrite one, or
    /// to move a status backwards, so those are the names asserted absent.
    function test_audit_noRemovalOrRewriteSurfaceExists() public {
        string[10] memory forbidden = [
            // Removal in any spelling.
            "remove(bytes32)",
            "unpublish(bytes32)",
            "revoke(bytes32)",
            "invalidate(bytes32)",
            "delete(bytes32)",
            // Rewriting a record in place.
            "setRecord(bytes32,uint8)",
            "setStatus(bytes32,uint8)",
            "setOffsets(bytes32,(uint32,uint32)[])",
            "republish(bytes32)",
            // Undoing a deprecation, which is a status moving backwards.
            "undeprecate(bytes32)"
        ];
        for (uint256 i = 0; i < forbidden.length; i++) {
            _assertNoFunction(address(registry), forbidden[i]);
        }
    }

    /// Positive control for the scan above: the functions that *do* exist are
    /// found, and an unknown selector really reverts rather than hitting a
    /// fallback. Without this the absence assertions prove nothing.
    function test_audit_scannerIsSound() public view {
        assertTrue(
            _bytecodeHasSelector(
                address(registry),
                bytes4(
                    keccak256(
                        "publish(bytes32,uint8,string,string,bytes32,string,(uint32,uint32)[])"
                    )
                )
            )
        );
        assertTrue(
            _bytecodeHasSelector(address(registry), bytes4(keccak256("deprecate(bytes32,string)")))
        );
        assertTrue(_bytecodeHasSelector(address(registry), bytes4(keccak256("record(bytes32)"))));
        assertTrue(
            _bytecodeHasSelector(
                address(registry), bytes4(keccak256("offsetTableWindow(uint256,uint256)"))
            )
        );
        assertTrue(
            _bytecodeHasSelector(
                address(registry), bytes4(keccak256("latestOffsetTables(uint256)"))
            )
        );
        assertFalse(
            _bytecodeHasSelector(address(registry), bytes4(keccak256("thisIsNotAFunction()")))
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 3. Deprecation warns; it never invalidates
    // ══════════════════════════════════════════════════════════════════════════

    /// The whole record survives a deprecation, offsets included, so an agent
    /// that meets deprecated code still recognises it as genuine and still knows
    /// how to hash it. "Not recommended" is the entire content of the change.
    function test_deprecate_invalidatesNothing() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
        Rub3CodeRegistry.Release memory before = registry.record(MCH_A);

        vm.prank(owner);
        registry.deprecate(MCH_A, "superseded by the 2026-09 release");

        Rub3CodeRegistry.Release memory r = registry.record(MCH_A);
        assertEq(uint8(r.status), uint8(Rub3CodeRegistry.Status.Deprecated));
        assertEq(uint8(r.role), uint8(before.role));
        assertEq(r.contractName, before.contractName);
        assertEq(r.version, before.version);
        assertEq(r.sourceCommit, before.sourceCommit);
        assertEq(r.solcVersion, before.solcVersion);
        assertEq(r.registeredAtBlock, before.registeredAtBlock);
        assertEq(r.offsets.length, before.offsets.length);
        for (uint256 i = 0; i < before.offsets.length; i++) {
            assertEq(r.offsets[i].start, before.offsets[i].start);
            assertEq(r.offsets[i].length, before.offsets[i].length);
        }

        // Still enumerable, and its offset table is still on offer, so nothing
        // about a comparator's ability to reach this release changed.
        assertEq(registry.publishedCount(), 1);
        assertEq(registry.published()[0], MCH_A);
        assertEq(registry.offsetTableCount(), 1);
    }

    function test_deprecate_emitsItsReason() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);

        vm.expectEmit(true, false, false, true, address(registry));
        emit Deprecated(MCH_A, "superseded");
        vm.prank(owner);
        registry.deprecate(MCH_A, "superseded");
    }

    /// One-way. There is no undo, and the reason there is no undo is that an
    /// un-deprecate is a second writable transition on a record meant to have
    /// exactly one.
    function test_deprecate_cannotBeRepeated() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
        vm.prank(owner);
        registry.deprecate(MCH_A, "superseded");

        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(Rub3CodeRegistry.AlreadyDeprecated.selector, MCH_A));
        registry.deprecate(MCH_A, "again");
    }

    function test_deprecate_rejectsAnUnpublishedHash() public {
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(Rub3CodeRegistry.NotPublished.selector, MCH_B));
        registry.deprecate(MCH_B, "never existed");
    }

    function test_deprecate_requiresAReason() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(Rub3CodeRegistry.TextRequired.selector, "reason"));
        registry.deprecate(MCH_A, "");
    }

    /// Deprecating one release says nothing about any other.
    function test_deprecate_touchesOnlyItsOwnRecord() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
        _publish(MCH_B, Rub3CodeRegistry.Role.Licence);

        vm.prank(owner);
        registry.deprecate(MCH_A, "superseded");

        assertEq(uint8(registry.record(MCH_B).status), uint8(Rub3CodeRegistry.Status.Active));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 4. Only the owner writes
    // ══════════════════════════════════════════════════════════════════════════

    function test_publish_revertsForANonOwner() public {
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, stranger)
        );
        registry.publish(
            MCH_A,
            Rub3CodeRegistry.Role.Licence,
            "Rub3Access",
            VERSION,
            COMMIT,
            SOLC,
            _ranges(1, 64)
        );

        assertEq(uint8(registry.record(MCH_A).status), uint8(Rub3CodeRegistry.Status.Unknown));
        assertEq(registry.publishedCount(), 0);
    }

    function test_deprecate_revertsForANonOwner() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, stranger)
        );
        registry.deprecate(MCH_A, "not yours to say");

        assertEq(uint8(registry.record(MCH_A).status), uint8(Rub3CodeRegistry.Status.Active));
    }

    /// Two-step ownership transfer: the new key has to accept before it holds
    /// anything. A one-step transfer to a mistyped address would freeze
    /// publishing forever, which is the one unrecoverable outcome here.
    function test_ownership_transfersOnlyOnAcceptance() public {
        vm.prank(owner);
        registry.transferOwnership(nextKey);

        // Not yet: the old key still owns it and the new one still cannot write.
        assertEq(registry.owner(), owner);
        vm.prank(nextKey);
        vm.expectRevert(
            abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, nextKey)
        );
        registry.publish(
            MCH_A,
            Rub3CodeRegistry.Role.Licence,
            "Rub3Access",
            VERSION,
            COMMIT,
            SOLC,
            _ranges(1, 64)
        );

        vm.prank(nextKey);
        registry.acceptOwnership();
        assertEq(registry.owner(), nextKey);

        vm.prank(nextKey);
        registry.publish(
            MCH_A,
            Rub3CodeRegistry.Role.Licence,
            "Rub3Access",
            VERSION,
            COMMIT,
            SOLC,
            _ranges(1, 64)
        );
        assertEq(uint8(registry.record(MCH_A).status), uint8(Rub3CodeRegistry.Status.Active));
    }

    /// Ownership here is the right to *add*. Giving it up to nobody would freeze
    /// the answer to "is this release newer than my binary" for every release
    /// after that point, permanently, with no recovery - so it is refused.
    function test_ownership_cannotBeRenounced() public {
        vm.prank(owner);
        vm.expectRevert(Rub3CodeRegistry.OwnershipCannotBeRenounced.selector);
        registry.renounceOwnership();

        assertEq(registry.owner(), owner);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 5. Offset tables: the bootstrap, and the bound on the blind spot
    // ══════════════════════════════════════════════════════════════════════════

    /// The bootstrap an agent depends on: it needs a table to compute the hash
    /// it is about to look up, so the distinct tables are fetchable in one call
    /// before any lookup happens. Identical tables intern to one entry, which is
    /// what keeps that list short as releases accumulate.
    function test_offsetTables_areInternedNotDuplicated() public {
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);
        _publish(MCH_B, Rub3CodeRegistry.Role.Licence);
        assertEq(registry.offsetTableCount(), 1, "the same table twice is one table");

        vm.prank(owner);
        registry.publish(
            keccak256("a differently shaped release"),
            Rub3CodeRegistry.Role.Licence,
            "Rub3Subscription",
            VERSION,
            COMMIT,
            SOLC,
            _ranges(4, 96)
        );
        assertEq(registry.offsetTableCount(), 2);

        Rub3CodeRegistry.ByteRange[][] memory tables = registry.offsetTables();
        assertEq(tables.length, 2);
        assertEq(tables[0].length, 3);
        assertEq(tables[1].length, 4);
        assertEq(tables[1][3].start, 64 + 3 * 96);
    }

    /// The bootstrap on a path with a deadline. A verifier tries a fixed number
    /// of candidates, so it must be able to *read* that many rather than read
    /// every table published and throw the rest away: the published set is the
    /// owner key's to grow, and the wrapper's own cap cannot bound a response it
    /// has already paid to receive. This is latency and nothing else - reading
    /// the whole set could only ever be slow, never wrong.
    ///
    /// Published here is four times what the wrapper's cap of 16 reads.
    function test_offsetTableWindow_readsABoundedSliceOfALargerSet() public {
        uint256 published = 64;
        _publishDistinctTables(published);
        assertEq(registry.offsetTableCount(), published, "each table is distinct");

        Rub3CodeRegistry.ByteRange[][] memory window = registry.offsetTableWindow(0, 16);
        assertEq(window.length, 16, "a bounded read returns the bound, not the set");
        assertEq(registry.offsetTables().length, published, "the whole set is still readable");

        // First-use order, and the ranges themselves, so a window is the same
        // answer as the full read rather than a differently shaped one.
        for (uint256 i = 0; i < window.length; i++) {
            assertEq(window[i].length, 1);
            assertEq(window[i][0].start, uint32(64 + i * 64));
            assertEq(window[i][0].length, 32);
        }
    }

    /// A window anywhere in the set, so a reader that wants the tables past its
    /// first bound can walk them without ever asking for the whole thing.
    function test_offsetTableWindow_startsWhereItIsAskedTo() public {
        _publishDistinctTables(8);

        Rub3CodeRegistry.ByteRange[][] memory window = registry.offsetTableWindow(5, 2);
        assertEq(window.length, 2);
        assertEq(window[0][0].start, uint32(64 + 5 * 64));
        assertEq(window[1][0].start, uint32(64 + 6 * 64));
    }

    /// Clamped, not strict: a reader asks for the bound it can afford and needs
    /// no count first, and no window can revert a purchase's only bootstrap read.
    function test_offsetTableWindow_clampsRatherThanReverting() public {
        assertEq(registry.offsetTableWindow(0, 16).length, 0, "nothing published yet");

        _publishDistinctTables(3);
        assertEq(registry.offsetTableWindow(0, 16).length, 3, "a count past the end is clamped");
        assertEq(registry.offsetTableWindow(2, 16).length, 1);
        assertEq(registry.offsetTableWindow(3, 16).length, 0, "a start at the end is empty");
        assertEq(registry.offsetTableWindow(99, 16).length, 0, "and past it, too");
        assertEq(registry.offsetTableWindow(0, 0).length, 0, "a zero window asks for nothing");
        assertEq(
            registry.offsetTableWindow(1, type(uint256).max).length,
            2,
            "start + count must not be computed, so the largest count cannot overflow"
        );
    }

    /// The read a purchase path makes, and the end of the set it has to be. A
    /// registry is consulted only when the reader's own pinned table missed, and
    /// a miss is by definition about code *newer* than that build, so a fixed
    /// budget of candidates spent on the oldest layouts would leave every
    /// release published under a later layout unreadable to every fielded binary
    /// while the first releases stayed readable forever.
    ///
    /// Published here is four times the sixteen the wrapper reads.
    function test_latestOffsetTables_readsTheNewestEndOfALargerSet() public {
        uint256 published = 64;
        _publishDistinctTables(published);
        assertEq(registry.offsetTableCount(), published, "each table is distinct");

        Rub3CodeRegistry.ByteRange[][] memory window = registry.latestOffsetTables(16);
        assertEq(window.length, 16, "a bounded read returns the bound, not the set");

        // Newest first: element zero is the last table interned, so the budget
        // lands on the layouts a release published after a binary was packed
        // would use.
        for (uint256 i = 0; i < window.length; i++) {
            assertEq(window[i].length, 1);
            assertEq(window[i][0].start, uint32(64 + (published - 1 - i) * 64));
            assertEq(window[i][0].length, 32);
        }

        // And the layouts it does not reach are the oldest, which is the trade
        // being made on purpose.
        assertEq(
            registry.offsetTables()[0][0].start,
            uint32(64),
            "the full set still starts at the first table ever interned"
        );
    }

    /// Clamped, so one call is enough on a path that cannot afford a count call
    /// first, and no argument a caller can pass makes it revert.
    function test_latestOffsetTables_clampsRatherThanReverting() public {
        assertEq(registry.latestOffsetTables(16).length, 0, "nothing published yet");

        _publishDistinctTables(3);
        assertEq(registry.latestOffsetTables(16).length, 3, "a count past the end is clamped");
        assertEq(registry.latestOffsetTables(0).length, 0, "a zero window asks for nothing");
        assertEq(
            registry.latestOffsetTables(type(uint256).max).length,
            3,
            "the largest count a caller can pass is still just the whole set"
        );

        Rub3CodeRegistry.ByteRange[][] memory one = registry.latestOffsetTables(1);
        assertEq(one.length, 1);
        assertEq(one[0][0].start, uint32(64 + 2 * 64), "one table means the newest one");
    }

    function test_offsetTables_emitOnFirstUseOnly() public {
        Rub3CodeRegistry.ByteRange[] memory ranges = _ranges(3, 64);

        vm.expectEmit(true, false, false, true, address(registry));
        emit OffsetTableAdded(0, ranges);
        _publish(MCH_A, Rub3CodeRegistry.Role.Licence);

        vm.recordLogs();
        _publish(MCH_B, Rub3CodeRegistry.Role.Licence);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i = 0; i < logs.length; i++) {
            assertTrue(
                logs[i].topics[0] != OffsetTableAdded.selector,
                "an already-interned table must not be announced again"
            );
        }
    }

    /// A Solidity immutable occupies one EVM word and sits in a `PUSH32`
    /// immediate. A wider range would be a blind spot larger than the thing it
    /// masks, so the width is not negotiable.
    function test_publish_rejectsARangeThatIsNotOneWord() public {
        Rub3CodeRegistry.ByteRange[] memory bad = new Rub3CodeRegistry.ByteRange[](1);
        bad[0] = Rub3CodeRegistry.ByteRange({start: 64, length: 64});

        vm.prank(owner);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3CodeRegistry.OffsetsMalformed.selector, "a range is not one 32-byte word"
            )
        );
        registry.publish(
            MCH_A, Rub3CodeRegistry.Role.Licence, "Rub3Access", VERSION, COMMIT, SOLC, bad
        );
    }

    function test_publish_rejectsUnsortedOrOverlappingRanges() public {
        Rub3CodeRegistry.ByteRange[] memory unsorted = new Rub3CodeRegistry.ByteRange[](2);
        unsorted[0] = Rub3CodeRegistry.ByteRange({start: 128, length: 32});
        unsorted[1] = Rub3CodeRegistry.ByteRange({start: 64, length: 32});

        vm.prank(owner);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3CodeRegistry.OffsetsMalformed.selector, "ranges must be sorted and disjoint"
            )
        );
        registry.publish(
            MCH_A, Rub3CodeRegistry.Role.Licence, "Rub3Access", VERSION, COMMIT, SOLC, unsorted
        );

        Rub3CodeRegistry.ByteRange[] memory overlapping = new Rub3CodeRegistry.ByteRange[](2);
        overlapping[0] = Rub3CodeRegistry.ByteRange({start: 64, length: 32});
        overlapping[1] = Rub3CodeRegistry.ByteRange({start: 95, length: 32});

        vm.prank(owner);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3CodeRegistry.OffsetsMalformed.selector, "ranges must be sorted and disjoint"
            )
        );
        registry.publish(
            MCH_A, Rub3CodeRegistry.Role.Licence, "Rub3Access", VERSION, COMMIT, SOLC, overlapping
        );
    }

    /// A range outside the largest runtime code the EVM will accept cannot
    /// describe any real deploy, so it is refused rather than published as an
    /// instruction nothing could follow.
    function test_publish_rejectsARangeOutsideAnyPossibleRuntimeCode() public {
        uint32 limit = registry.MAX_RUNTIME_CODE_SIZE();
        Rub3CodeRegistry.ByteRange[] memory beyond = new Rub3CodeRegistry.ByteRange[](1);
        beyond[0] = Rub3CodeRegistry.ByteRange({start: limit - 31, length: 32});

        vm.prank(owner);
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3CodeRegistry.OffsetsMalformed.selector,
                "a range falls outside the largest possible runtime code"
            )
        );
        registry.publish(
            MCH_A, Rub3CodeRegistry.Role.Licence, "Rub3Access", VERSION, COMMIT, SOLC, beyond
        );

        // One byte earlier is the last range that can exist, and it is accepted.
        Rub3CodeRegistry.ByteRange[] memory last = new Rub3CodeRegistry.ByteRange[](1);
        last[0] = Rub3CodeRegistry.ByteRange({start: limit - 32, length: 32});
        vm.prank(owner);
        registry.publish(
            MCH_A, Rub3CodeRegistry.Role.Licence, "Rub3Access", VERSION, COMMIT, SOLC, last
        );
        assertEq(registry.record(MCH_A).offsets[0].start, limit - 32);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 6. A record is permanent, so it may not be published half-written
    // ══════════════════════════════════════════════════════════════════════════

    function test_publish_rejectsTheZeroHash() public {
        vm.prank(owner);
        vm.expectRevert(Rub3CodeRegistry.MaskedCodeHashRequired.selector);
        registry.publish(
            bytes32(0),
            Rub3CodeRegistry.Role.Licence,
            "Rub3Access",
            VERSION,
            COMMIT,
            SOLC,
            _ranges(1, 64)
        );
    }

    function test_publish_rejectsAMissingSourceCommit() public {
        vm.prank(owner);
        vm.expectRevert(Rub3CodeRegistry.SourceCommitRequired.selector);
        registry.publish(
            MCH_A,
            Rub3CodeRegistry.Role.Licence,
            "Rub3Access",
            VERSION,
            bytes32(0),
            SOLC,
            _ranges(1, 64)
        );
    }

    function test_publish_rejectsEmptyText() public {
        string[3] memory fields = ["contractName", "version", "solcVersion"];
        for (uint256 i = 0; i < fields.length; i++) {
            vm.prank(owner);
            vm.expectRevert(
                abi.encodeWithSelector(Rub3CodeRegistry.TextRequired.selector, fields[i])
            );
            registry.publish(
                MCH_A,
                Rub3CodeRegistry.Role.Licence,
                i == 0 ? "" : "Rub3Access",
                i == 1 ? "" : VERSION,
                COMMIT,
                i == 2 ? "" : SOLC,
                _ranges(1, 64)
            );
        }
        assertEq(registry.publishedCount(), 0);
    }

    /// A publish that reverts leaves nothing behind - not an enumeration entry,
    /// not an interned offset table. Interning happens after validation for
    /// exactly this reason.
    function test_publish_leavesNothingBehindWhenItReverts() public {
        vm.prank(owner);
        vm.expectRevert(Rub3CodeRegistry.SourceCommitRequired.selector);
        registry.publish(
            MCH_A,
            Rub3CodeRegistry.Role.Licence,
            "Rub3Access",
            VERSION,
            bytes32(0),
            SOLC,
            _ranges(3, 64)
        );

        assertEq(registry.publishedCount(), 0);
        assertEq(registry.offsetTableCount(), 0);
        assertEq(uint8(registry.record(MCH_A).status), uint8(Rub3CodeRegistry.Status.Unknown));
    }

    // ── Bytecode scanning helpers ────────────────────────────────────────────

    /// @dev Scans deployed runtime bytecode for a 4-byte selector constant.
    ///      solc emits every external function's selector as a literal PUSH4 in
    ///      the dispatcher, so absence from the code is absence from the ABI.
    ///      Same helper, same reasoning, as `test/Rub3Invariants.t.sol`.
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

    /// @dev Asserts `signature` is not callable on `target`, two independent
    ///      ways: the selector is absent from the runtime bytecode, and a raw
    ///      call carrying it reverts (no fallback exists to swallow it).
    function _assertNoFunction(address target, string memory signature) internal {
        bytes4 sel = bytes4(keccak256(bytes(signature)));
        assertFalse(
            _bytecodeHasSelector(target, sel),
            string.concat("selector present in bytecode: ", signature)
        );
        (bool ok,) = target.call(abi.encodePacked(sel, new bytes(128)));
        assertFalse(ok, string.concat("call did not revert: ", signature));
    }
}
