// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {Rub3Access} from "../src/Rub3Access.sol";
import {Rub3License} from "../src/Rub3License.sol";

/// @notice Concurrent seats - implementation.md §3.4.
///
/// A licence stops being one seat and becomes K. This suite holds the two
/// properties that make that safe rather than merely useful:
///
/// 1. **K sessions run at once**, seats are freed by {Rub3License-release} and
///    by a TTL lapse, and a full fleet reports itself as a full fleet rather
///    than as something a caller could wait out in blocks.
/// 2. **The churn defence is untouched.** A token lands at most `seatsPerToken`
///    activations in any window of `cooldownBlocks` blocks - exactly
///    `seatsPerToken` times the single-seat rate and not one more, whatever
///    anybody releases or lets lapse. The tests that hold this are marked
///    `churnDefence` and they are the ones that must never be relaxed to make a
///    seat easier to get.
contract Rub3SeatsTest is Test {
    address internal owner = address(0xA11CE);
    address internal alice = address(0xA);
    address internal bob = address(0xB);

    bytes32 internal constant WRAPPER_HASH = keccak256("test-wrapper-v1");
    uint256 internal constant PRICE = 0.05 ether;
    uint256 internal constant COOLDOWN_BLOCKS = 15; // == MIN_COOLDOWN_BLOCKS
    uint256 internal constant SESSION_TTL = 24 hours;
    uint256 internal constant SEATS = 4;

    Rub3Access internal nft;

    function setUp() public {
        nft = _deploy(SEATS, COOLDOWN_BLOCKS, SESSION_TTL);
        vm.deal(alice, 10 ether);
        vm.deal(bob, 10 ether);
        // Start clear of block 0 and timestamp 0 so "never activated" is a
        // state the fixtures reach on purpose rather than by accident.
        vm.roll(1_000);
        vm.warp(1_000_000);
    }

    // ── Fixtures ──────────────────────────────────────────────────────────────

    function _deploy(uint256 seats, uint256 cooldown, uint256 ttl) internal returns (Rub3Access) {
        bytes32[] memory hashes = new bytes32[](1);
        hashes[0] = WRAPPER_HASH;
        return new Rub3Access(
            "Rub3 Seats",
            "R3S",
            Rub3License.IdentityTerms({model: 0, tbaImplementation: address(0)}),
            hashes,
            Rub3License.SaleTerms({price: PRICE, priceToken: address(0), priceAmount: 0}),
            Rub3License.FeeTerms({feeBps: 0, treasury: address(0)}),
            0,
            Rub3License.SessionTerms({
                cooldownBlocks: cooldown,
                seatsPerToken: seats,
                sessionTtlSeconds: ttl
            }),
            address(0),
            owner
        );
    }

    function _mint(address to) internal returns (uint256 id) {
        return _mintOn(nft, to);
    }

    function _mintOn(Rub3Access on, address to) internal returns (uint256 id) {
        vm.prank(to);
        id = on.purchase{value: PRICE}(to);
    }

    /// Activates as `holder` and returns the session id.
    function _activate(uint256 tokenId, address holder) internal returns (uint256) {
        vm.prank(holder);
        return nft.activate(tokenId);
    }

    /// How many activations succeed right now, releasing each seat straight back
    /// so that occupancy can never be what stops the next one. Whatever is left
    /// standing in the way is the cooldown, which is the point.
    function _greedyActivationsNow(uint256 tokenId, address holder)
        internal
        returns (uint256 count)
    {
        while (true) {
            vm.prank(holder);
            try nft.activate(tokenId) returns (uint256 sessionId) {
                count++;
                vm.prank(holder);
                nft.release(tokenId, sessionId);
            } catch {
                return count;
            }
        }
    }

    /// A seat that has never been taken is ready the moment it exists, so a
    /// test about *retaking* has to leave no virgin seats behind. This stamps
    /// every seat on `on` and hands them all back.
    function _stampAndReleaseEverySeat(Rub3Access on, uint256 tokenId, address holder) internal {
        uint256 seats = on.seatsPerToken();
        uint256[] memory sessions = new uint256[](seats);
        for (uint256 i = 0; i < seats; i++) {
            vm.prank(holder);
            sessions[i] = on.activate(tokenId);
        }
        for (uint256 i = 0; i < seats; i++) {
            vm.prank(holder);
            on.release(tokenId, sessions[i]);
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 1. K concurrent sessions
    // ══════════════════════════════════════════════════════════════════════════

    /// The whole point of §3.4: a fleet of K instances comes up together, in one
    /// block, on one token. A token-level cooldown would have serialised this
    /// into K cooldown windows, which at the ~1hr default is hours of start-up.
    function test_seats_kSessionsActivateInOneBlock() public {
        uint256 id = _mint(alice);

        uint256[] memory sessions = new uint256[](SEATS);
        for (uint256 i = 0; i < SEATS; i++) {
            sessions[i] = _activate(id, alice);
            assertEq(nft.seatsInUse(id), i + 1, "each activation takes one more seat");
        }

        // All K are live at once, each on a seat of its own.
        bool[] memory seen = new bool[](SEATS);
        for (uint256 i = 0; i < SEATS; i++) {
            (bool live, uint256 index) = nft.sessionSeat(id, sessions[i]);
            assertTrue(live, "every session opened must still hold its seat");
            assertFalse(seen[index], "two sessions must never share a seat");
            seen[index] = true;
        }
    }

    /// The K+1th instance is told the fleet is full, with the numbers an
    /// orchestrator needs to decide what to do: how many seats are in use, and
    /// how many there are.
    function test_seats_beyondCapacityRevertsFleetExhausted() public {
        uint256 id = _mint(alice);
        for (uint256 i = 0; i < SEATS; i++) {
            _activate(id, alice);
        }

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.FleetExhausted.selector, id, SEATS, SEATS)
        );
        nft.activate(id);
    }

    /// Seats are per token, so a second token is a second fleet. This is §3.4's
    /// own scaling story - "buy another token to scale" - held to by test.
    function test_seats_areCountedPerToken() public {
        uint256 first = _mint(alice);
        uint256 second = _mint(alice);

        for (uint256 i = 0; i < SEATS; i++) {
            _activate(first, alice);
        }
        assertEq(nft.seatsInUse(first), SEATS);
        assertEq(nft.seatsInUse(second), 0, "a full token must not fill its neighbour");

        // The second token's seats are all still there.
        for (uint256 i = 0; i < SEATS; i++) {
            _activate(second, alice);
        }
        assertEq(nft.seatsInUse(second), SEATS);
    }

    /// K is a property of the contract and of nothing else, so it cannot be
    /// lowered for a token somebody already holds - there is no per-token seat
    /// count to lower. See `Rub3Invariants.t.sol` for the bytecode half.
    function test_seats_perTokenCountIsWhateverTheContractSays() public {
        Rub3Access wide = _deploy(9, COOLDOWN_BLOCKS, SESSION_TTL);
        assertEq(wide.seatsPerToken(), 9);
        assertEq(nft.seatsPerToken(), SEATS);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 2. Freeing a seat: release, and the TTL lapse
    // ══════════════════════════════════════════════════════════════════════════

    function test_release_freesTheSeatImmediately() public {
        uint256 id = _mint(alice);
        uint256[] memory sessions = new uint256[](SEATS);
        for (uint256 i = 0; i < SEATS; i++) {
            sessions[i] = _activate(id, alice);
        }

        vm.expectEmit(true, true, false, true);
        emit Rub3License.Released(id, alice, sessions[1], 1);
        vm.prank(alice);
        nft.release(id, sessions[1]);

        assertEq(nft.seatsInUse(id), SEATS - 1);
        (bool live,) = nft.sessionSeat(id, sessions[1]);
        assertFalse(live, "a released session no longer holds a seat");
    }

    /// The failure the TTL exists for: a fleet instance dies without releasing
    /// anything. Without a lapse the licence would degrade, one crash at a time,
    /// to zero usable seats - which is a grant silently becoming nothing.
    function test_ttl_lapseFreesASeatWithoutAnybodyCallingAnything() public {
        uint256 id = _mint(alice);
        for (uint256 i = 0; i < SEATS; i++) {
            _activate(id, alice);
        }
        assertEq(nft.seatsInUse(id), SEATS);

        // Nobody calls anything. Time passes.
        vm.warp(block.timestamp + SESSION_TTL);
        vm.roll(block.number + COOLDOWN_BLOCKS);

        assertEq(nft.seatsInUse(id), 0, "every seat lapses on its own");
        _activate(id, alice); // and the fleet can come back up
        assertEq(nft.seatsInUse(id), 1);
    }

    /// A seat is occupied right up to its expiry and free from it, and nothing
    /// writes to storage to make the second true.
    function test_ttl_boundaryIsExactAndLazy() public {
        uint256 id = _mint(alice);
        uint256 session = _activate(id, alice);
        uint256 expiresAt = block.timestamp + SESSION_TTL;

        vm.warp(expiresAt - 1);
        (bool live,) = nft.sessionSeat(id, session);
        assertTrue(live, "still live one second before expiry");
        assertEq(nft.seatsInUse(id), 1);

        vm.warp(expiresAt);
        (live,) = nft.sessionSeat(id, session);
        assertFalse(live, "lapsed at its expiry");
        assertEq(nft.seatsInUse(id), 0);

        // The seat's own record still names the session that had it: nothing
        // swept, nothing rewritten.
        Rub3License.Seat memory seat = nft.seatAt(id, 0);
        assertEq(seat.sessionId, session);
        assertEq(seat.expiresAt, expiresAt);
    }

    function test_release_isTokenHolderOnly() public {
        uint256 id = _mint(alice);
        uint256 session = _activate(id, alice);

        vm.prank(bob);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, bob, alice));
        nft.release(id, session);

        // Including the contract owner, who is not the holder.
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.NotTokenOwner.selector, owner, alice));
        nft.release(id, session);

        (bool live,) = nft.sessionSeat(id, session);
        assertTrue(live, "nobody but the holder moved the seat");
    }

    function test_release_ofSomethingNotHeld_reverts() public {
        uint256 id = _mint(alice);
        uint256 session = _activate(id, alice);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.SeatNotHeld.selector, id, session + 1));
        nft.release(id, session + 1);

        // Session id 0 is never issued, so it never holds a seat either.
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.SeatNotHeld.selector, id, 0));
        nft.release(id, 0);
    }

    /// A lapsed seat is already free, so there is nothing to hand back. Saying
    /// so is better than succeeding silently: an orchestrator that thinks it
    /// released something is entitled to be told it did not.
    function test_release_afterLapse_reverts() public {
        uint256 id = _mint(alice);
        uint256 session = _activate(id, alice);

        vm.warp(block.timestamp + SESSION_TTL);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.SeatNotHeld.selector, id, session));
        nft.release(id, session);
    }

    function test_release_twice_reverts() public {
        uint256 id = _mint(alice);
        uint256 session = _activate(id, alice);

        vm.prank(alice);
        nft.release(id, session);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.SeatNotHeld.selector, id, session));
        nft.release(id, session);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 3. churnDefence - seats multiply concurrency, never the churn rate
    //
    //    Every test in this section exists because the obvious wrong
    //    implementation passes everything above it. Do not relax one to make a
    //    seat easier to get.
    // ══════════════════════════════════════════════════════════════════════════

    /// **The headline property.** Over any window of `cooldownBlocks` blocks a
    /// token lands at most `seatsPerToken` activations, and releasing every seat
    /// the instant it is taken does not buy a single extra one.
    ///
    /// Measured greedily rather than argued: at each block the holder activates
    /// and releases until the contract refuses, so occupancy is never what stops
    /// it and the count is the churn budget alone.
    function test_churnDefence_atMostKActivationsPerCooldownWindow() public {
        uint256 id = _mint(alice);

        // Window 1: K activations are available, and the K+1th is not, at this
        // block or at any block for the rest of the window.
        assertEq(_greedyActivationsNow(id, alice), SEATS, "K seats, K activations");
        for (uint256 b = 1; b < COOLDOWN_BLOCKS; b++) {
            vm.roll(block.number + 1);
            assertEq(
                _greedyActivationsNow(id, alice),
                0,
                "no seat may be retaken before its own cooldown has run"
            );
        }

        // Window 2 opens exactly `cooldownBlocks` after the activations that
        // filled window 1, and offers exactly K again.
        vm.roll(block.number + 1);
        assertEq(_greedyActivationsNow(id, alice), SEATS, "and K again, never more");
    }

    /// **The wrong implementation, pinned.** Freeing a seat's *occupancy* must
    /// never free its cooldown stamp. A {Rub3License-release} that cleared
    /// `activatedAt` would make release-then-activate an unlimited churn
    /// bypass - every test above this one would still pass, and this is the one
    /// that goes red.
    function test_churnDefence_releaseDoesNotClearTheCooldownStamp() public {
        // One seat, so releasing it leaves nothing else to fall back on and the
        // stamp is the only thing that can refuse the retake.
        Rub3Access single = _deploy(1, COOLDOWN_BLOCKS, SESSION_TTL);
        uint256 id = _mintOn(single, alice);

        vm.prank(alice);
        uint256 session = single.activate(id);
        uint256 stampedAt = block.number;

        vm.prank(alice);
        single.release(id, session);

        // The seat is free - and still refuses, for the full remaining cooldown.
        assertEq(single.seatsInUse(id), 0);
        Rub3License.Seat memory seat = single.seatAt(id, 0);
        assertEq(seat.expiresAt, 0, "released seats are free");
        assertEq(seat.activatedAt, stampedAt, "and keep their stamp");

        vm.roll(block.number + COOLDOWN_BLOCKS - 1);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.CooldownActive.selector, 1));
        single.activate(id);

        vm.roll(block.number + 1);
        vm.prank(alice);
        single.activate(id); // and only now
    }

    /// The same for the other way a seat comes free. A lapse is a clock running
    /// out, not a reset: the stamp is where the activation left it.
    function test_churnDefence_ttlLapseDoesNotClearTheCooldownStamp() public {
        // A TTL shorter than the cooldown is the case that would expose a lapse
        // as a churn bypass, so that is the one to build.
        Rub3Access quick = _deploy(1, 600, 5 minutes);
        vm.prank(alice);
        uint256 id = quick.purchase{value: PRICE}(alice);

        vm.prank(alice);
        quick.activate(id);
        uint256 stampedAt = block.number;

        // The session lapses long before the seat may be retaken.
        vm.warp(block.timestamp + 5 minutes);
        vm.roll(block.number + 30);
        assertEq(quick.seatsInUse(id), 0, "the seat is free");

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.CooldownActive.selector, 600 - 30));
        quick.activate(id);

        vm.roll(stampedAt + 600);
        vm.prank(alice);
        quick.activate(id);
    }

    /// Each seat carries its own stamp, so filling a fleet does not spend one
    /// shared allowance - and emptying it does not refill one either.
    function test_churnDefence_stampsArePerSeatNotPerToken() public {
        uint256 id = _mint(alice);

        // Stagger the fleet: one instance per block.
        uint256[] memory sessions = new uint256[](SEATS);
        uint256[] memory stampedAt = new uint256[](SEATS);
        for (uint256 i = 0; i < SEATS; i++) {
            sessions[i] = _activate(id, alice);
            stampedAt[i] = block.number;
            vm.roll(block.number + 1);
        }

        // Release them all. Each seat becomes takeable again exactly
        // `cooldownBlocks` after *its own* activation, in the order they were
        // taken - never all at once, and never earlier.
        for (uint256 i = 0; i < SEATS; i++) {
            vm.prank(alice);
            nft.release(id, sessions[i]);
        }

        for (uint256 i = 0; i < SEATS; i++) {
            uint256 seatReadyAt = stampedAt[i] + COOLDOWN_BLOCKS;
            vm.roll(seatReadyAt - 1);
            assertEq(_greedyActivationsNow(id, alice), 0, "not one block early");
            vm.roll(seatReadyAt);
            assertEq(_greedyActivationsNow(id, alice), 1, "and exactly one seat, not the fleet");
        }
    }

    /// A single-seat contract is still the tier-3 licence it always was: one
    /// session at a time, one activation per cooldown window.
    function test_churnDefence_singleSeatMatchesTheTierThreeRate() public {
        Rub3Access single = _deploy(1, COOLDOWN_BLOCKS, SESSION_TTL);
        vm.prank(alice);
        uint256 id = single.purchase{value: PRICE}(alice);

        for (uint256 window = 0; window < 3; window++) {
            vm.prank(alice);
            uint256 session = single.activate(id);
            vm.prank(alice);
            single.release(id, session);

            vm.prank(alice);
            vm.expectRevert(
                abi.encodeWithSelector(Rub3License.CooldownActive.selector, COOLDOWN_BLOCKS)
            );
            single.activate(id);

            vm.roll(block.number + COOLDOWN_BLOCKS);
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 4. Reporting: an orchestrator must never have to guess which refusal
    // ══════════════════════════════════════════════════════════════════════════

    /// The status view and the transaction are two readings of one scan, so they
    /// cannot disagree about which seat is next or why there is none.
    function test_activationStatus_agreesWithActivate() public {
        uint256 id = _mint(alice);

        for (uint256 i = 0; i < SEATS; i++) {
            Rub3License.ActivationStatus memory status = nft.activationStatus(id);
            assertTrue(status.ready);
            assertFalse(status.fleetExhausted);
            assertEq(status.seatIndex, i, "the view names the seat activate() takes");
            assertEq(status.seatsInUse, i);
            assertEq(status.seats, SEATS);
            assertEq(status.blocksRemaining, 0);
            _activate(id, alice);
        }

        Rub3License.ActivationStatus memory full = nft.activationStatus(id);
        assertFalse(full.ready);
        assertTrue(full.fleetExhausted);
        assertEq(full.seatsInUse, SEATS);
        assertEq(full.seats, SEATS);
        assertEq(full.secondsRemaining, SESSION_TTL, "when the first seat lapses");
        assertEq(full.blocksRemaining, 0, "no number of blocks frees a full fleet");
    }

    /// The two refusals are told apart by the `fleetExhausted` field, which is
    /// what lets a caller branch without reading a revert string.
    ///
    /// A field rather than the `seatsInUse == seats` comparison it looks like:
    /// a single-seat licence's one occupied seat is its holder's to retake, so
    /// the counts say "full" about a seat that is not, and a client deriving
    /// the answer from them refuses an activation the contract would accept.
    function test_activationStatus_tellsCooldownApartFromExhaustion() public {
        Rub3Access pair = _deploy(2, COOLDOWN_BLOCKS, SESSION_TTL);
        uint256 id = _mintOn(pair, alice);
        vm.prank(alice);
        pair.activate(id);
        vm.prank(alice);
        uint256 second = pair.activate(id);

        // Full fleet is not a cooldown: nothing to wait for in blocks.
        Rub3License.ActivationStatus memory exhausted = pair.activationStatus(id);
        assertFalse(exhausted.ready);
        assertTrue(exhausted.fleetExhausted);
        assertEq(exhausted.seatsInUse, 2);
        assertEq(exhausted.seats, 2);
        assertEq(exhausted.blocksRemaining, 0, "no number of blocks frees a full fleet");
        assertGt(exhausted.secondsRemaining, 0);

        // Release one seat and the same token is now a cooldown case: a seat is
        // free, and blocks are what stand in the way.
        vm.prank(alice);
        pair.release(id, second);
        vm.roll(block.number + 4);

        Rub3License.ActivationStatus memory cooling = pair.activationStatus(id);
        assertFalse(cooling.ready);
        assertFalse(cooling.fleetExhausted, "a free seat means this is not exhaustion");
        assertEq(cooling.seatsInUse, 1);
        assertEq(cooling.blocksRemaining, COOLDOWN_BLOCKS - 4);
        assertGt(cooling.secondsRemaining, 0, "the other seat is still occupied");
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 6. The single-seat rule: `seatsPerToken == 1` is the tier-3 licence
    // ══════════════════════════════════════════════════════════════════════════

    /// **A sole holder is never locked out of their own licence.** With one
    /// seat there is no fleet and no other instance the seat could belong to,
    /// so `activate()` retakes it - which is what tier 3 has always done, and
    /// what stops a lost session record costing a holder a whole TTL.
    function test_singleSeat_retakesItsOwnSeatAfterTheCooldown() public {
        Rub3Access single = _deploy(1, COOLDOWN_BLOCKS, SESSION_TTL);
        uint256 id = _mintOn(single, alice);

        vm.prank(alice);
        uint256 first = single.activate(id);
        assertEq(single.seatsInUse(id), 1);

        // Nothing is released, and the session is nowhere near its TTL.
        vm.roll(block.number + COOLDOWN_BLOCKS);
        vm.prank(alice);
        uint256 second = single.activate(id);

        assertGt(second, first);
        (bool live,) = single.sessionSeat(id, first);
        assertFalse(live, "opening a session ends the previous one");
        (live,) = single.sessionSeat(id, second);
        assertTrue(live);
        assertEq(single.seatsInUse(id), 1, "still one seat, still one session");
    }

    /// The retake buys no churn: it is refused for exactly as long as any other
    /// activation on that seat would be.
    function test_singleSeat_retakeIsRefusedInsideTheCooldown() public {
        Rub3Access single = _deploy(1, COOLDOWN_BLOCKS, SESSION_TTL);
        uint256 id = _mintOn(single, alice);

        vm.prank(alice);
        single.activate(id);
        vm.roll(block.number + 1);

        Rub3License.ActivationStatus memory status = single.activationStatus(id);
        assertFalse(status.ready);
        assertFalse(status.fleetExhausted, "a single-seat licence has no fleet to exhaust");
        assertEq(status.blocksRemaining, COOLDOWN_BLOCKS - 1);

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.CooldownActive.selector, COOLDOWN_BLOCKS - 1)
        );
        single.activate(id);
    }

    /// **And the rule stops at one seat.** With a fleet, the occupied seat you
    /// cannot account for is somebody else's, so it is reported rather than
    /// taken - which is the signal §3.4 exists to give an orchestrator.
    function test_seats_aboveOneNeverRetakeAnOccupiedSeat() public {
        Rub3Access pair = _deploy(2, COOLDOWN_BLOCKS, SESSION_TTL);
        uint256 id = _mintOn(pair, alice);

        vm.prank(alice);
        uint256 first = pair.activate(id);
        vm.prank(alice);
        uint256 second = pair.activate(id);

        // Long past every cooldown, and still refused: a live session is not a
        // seat to take.
        vm.roll(block.number + COOLDOWN_BLOCKS * 10);
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(Rub3License.FleetExhausted.selector, id, 2, 2));
        pair.activate(id);

        (bool live,) = pair.sessionSeat(id, first);
        assertTrue(live, "neither instance was evicted");
        (live,) = pair.sessionSeat(id, second);
        assertTrue(live);
    }

    /// The single-seat rate is unchanged by the retake, measured the same
    /// greedy way as the fleet rate: one activation per cooldown window and no
    /// more, whether the holder releases first or simply takes the seat back.
    function test_churnDefence_singleSeatRetakeKeepsTheTierThreeRate() public {
        Rub3Access single = _deploy(1, COOLDOWN_BLOCKS, SESSION_TTL);
        uint256 id = _mintOn(single, alice);

        for (uint256 window = 0; window < 3; window++) {
            uint256 taken = 0;
            while (true) {
                vm.prank(alice);
                try single.activate(id) returns (uint256) {
                    taken++;
                } catch {
                    break;
                }
            }
            assertEq(taken, 1, "one activation per window, retake or not");
            vm.roll(block.number + COOLDOWN_BLOCKS);
        }
    }

    /// With several free seats cooling at different blocks, the wait reported is
    /// the shortest one - the block the *next* seat becomes takeable.
    function test_activationStatus_reportsTheEarliestFreeSeat() public {
        // Exactly two seats, so both are stamped and neither is a virgin seat
        // that would be ready immediately.
        Rub3Access pair = _deploy(2, COOLDOWN_BLOCKS, SESSION_TTL);
        uint256 id = _mintOn(pair, alice);

        vm.prank(alice);
        uint256 first = pair.activate(id);
        vm.roll(block.number + 5);
        vm.prank(alice);
        uint256 second = pair.activate(id);

        vm.prank(alice);
        pair.release(id, first);
        vm.prank(alice);
        pair.release(id, second);

        Rub3License.ActivationStatus memory status = pair.activationStatus(id);
        assertFalse(status.ready);
        assertEq(status.blocksRemaining, COOLDOWN_BLOCKS - 5, "the older seat frees first");
    }

    /// `lastActivationBlock` is what an auto-detect watch polls, and under seats
    /// it means the most recent activation on any of them.
    function test_lastActivationBlock_tracksTheMostRecentSeat() public {
        uint256 id = _mint(alice);
        assertEq(nft.lastActivationBlock(id), 0, "never activated");

        _activate(id, alice);
        uint256 first = block.number;
        assertEq(nft.lastActivationBlock(id), first);

        vm.roll(block.number + 3);
        _activate(id, alice);
        assertEq(nft.lastActivationBlock(id), block.number);
        assertGt(nft.lastActivationBlock(id), first);
    }

    function test_seatAt_outOfRange_reverts() public {
        uint256 id = _mint(alice);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.SeatsOutOfRange.selector, SEATS, 0, SEATS - 1)
        );
        nft.seatAt(id, SEATS);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 5. Deploy-time bounds
    // ══════════════════════════════════════════════════════════════════════════

    function test_deploy_rejectsZeroSeats() public {
        vm.expectRevert(abi.encodeWithSelector(Rub3License.SeatsOutOfRange.selector, 0, 1, 64));
        _deploy(0, COOLDOWN_BLOCKS, SESSION_TTL);
    }

    function test_deploy_rejectsMoreThanMaxSeats() public {
        vm.expectRevert(abi.encodeWithSelector(Rub3License.SeatsOutOfRange.selector, 65, 1, 64));
        _deploy(65, COOLDOWN_BLOCKS, SESSION_TTL);
    }

    function test_deploy_acceptsExactlyMaxSeats() public {
        Rub3Access wide = _deploy(64, COOLDOWN_BLOCKS, SESSION_TTL);
        assertEq(wide.seatsPerToken(), 64);
    }

    function test_deploy_rejectsTtlBelowTheFloor() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3License.SessionTtlOutOfRange.selector, 299, 5 minutes, 90 days
            )
        );
        _deploy(SEATS, COOLDOWN_BLOCKS, 299);
    }

    function test_deploy_rejectsTtlAboveTheCeiling() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3License.SessionTtlOutOfRange.selector, 90 days + 1, 5 minutes, 90 days
            )
        );
        _deploy(SEATS, COOLDOWN_BLOCKS, 90 days + 1);
    }

    /// A widest-allowed fleet has to stay affordable to activate on, because the
    /// scan is what makes lazy expiry work. This is the bound {MAX_SEATS} exists
    /// to hold, measured rather than asserted.
    function test_activate_gasStaysBoundedAtMaxSeats() public {
        Rub3Access wide = _deploy(64, COOLDOWN_BLOCKS, SESSION_TTL);
        vm.prank(alice);
        uint256 id = wide.purchase{value: PRICE}(alice);

        for (uint256 i = 0; i < 63; i++) {
            vm.prank(alice);
            wide.activate(id);
        }

        // The 64th scans all 63 taken seats before it finds the last one.
        vm.prank(alice);
        uint256 before = gasleft();
        wide.activate(id);
        uint256 spent = before - gasleft();
        assertLt(spent, 400_000, "a full-width scan must stay well inside a block");
    }
}
