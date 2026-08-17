// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Rub3Access} from "../../src/Rub3Access.sol";

/// @notice A DELIBERATELY NON-CANONICAL licence contract, and a test fixture
///         only. It must never be deployed to any real network, and it must
///         never move into `contracts/src/`.
///
/// **Why it exists.** The wrapper refuses to buy from a contract whose deployed
/// code does not match a fingerprint it was packed with
/// (`implementation.md` §2.6, `crates/rub3-wrapper/src/attest.rs`). The
/// interesting adversary is not an unrelated contract - it is a modified copy
/// of this project's own licence template: the whole rub3 ABI, answering every
/// read an agent makes with the values a canonical deploy of the same
/// arguments would, carrying its one extra power under a name no blacklist
/// guessed. Such a contract passes {Rub3Invariants}'s forbidden-signature scan
/// in silence and fails the masked code hash, and that asymmetry is the whole
/// justification for the fingerprint check. Until this fixture existed the asymmetry was asserted
/// only at the unit level, against synthetic bytes.
///
/// It is also the fixture for the other half of the posture. A launch is a
/// program somebody already paid for, so the launch path never attests at all -
/// refusing to *start* it because a check could not complete would be the
/// revocation surface §2.4 rules out. Proving that needs one contract the
/// purchase gate refuses and the launch path serves, and this is it.
///
/// **The modification.** {reconcileLedger} is an owner-only seizure wearing an
/// accounting name: it moves any token to any address with no consent from the
/// holder. Everything else is inherited unchanged, so the contract sells,
/// activates and validates exactly as {Rub3Access} does - which is the point.
/// The name is chosen precisely because it is not in
/// `attest::FORBIDDEN_SIGNATURES`, and so is invisible to the selector scan.
///
/// **Never move this to `contracts/src/`.**
/// `scripts/canonical-bytecode-hashes.sh` derives its deployable set from every
/// artifact whose `compilationTarget` sits under the resolved source directory.
/// Under `contracts/src/` this contract would be fingerprinted and published
/// into `contracts/canonical-bytecode.json` as canonical rub3 code, and
/// `attest::tests::pinned_table_mirrors_the_canonical_manifest` would then
/// demand a row for it in `attest::CANONICAL` - making the wrapper accept the
/// very contract these tests exist to prove it refuses. Test artifacts are
/// excluded structurally, which is the only thing keeping that from happening.
contract NonCanonicalRub3Access is Rub3Access {
    /// @notice Emitted by {reconcileLedger}, under the same innocuous name.
    event LedgerReconciled(uint256 indexed tokenId, address indexed from, address indexed to);

    constructor(
        string        memory name_,
        string        memory symbol_,
        IdentityTerms memory identity_,
        bytes32[]     memory wrapperHashes_,
        SaleTerms     memory sale_,
        FeeTerms      memory fee_,
        uint256              supplyCap_,
        uint256              cooldownBlocks_,
        address              predecessor_,
        address              owner_
    ) Rub3Access(
        name_, symbol_, identity_, wrapperHashes_,
        sale_, fee_, supplyCap_, cooldownBlocks_, predecessor_, owner_
    ) {}

    /// @notice Move `tokenId` to `to`, whatever its holder wants. The seizure
    ///         `attest::FORBIDDEN_SIGNATURES` does not name.
    ///
    /// @dev Passing `address(0)` as `auth` to {ERC721-_update} skips the
    ///      authorization check entirely, so no approval from the holder is
    ///      involved. This is the whole modification.
    function reconcileLedger(uint256 tokenId, address to) external onlyOwner {
        address from = _update(to, tokenId, address(0));
        emit LedgerReconciled(tokenId, from, to);
    }
}
