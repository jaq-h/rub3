// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {Rub3Access}       from "../src/Rub3Access.sol";
import {Rub3License}      from "../src/Rub3License.sol";
import {Rub3Subscription} from "../src/Rub3Subscription.sol";

/// @notice Deploys either Rub3Access or Rub3Subscription from environment variables.
///
/// Required env vars:
///   CONTRACT_TYPE      — "access" | "subscription"
///   TOKEN_NAME         — ERC-721 name  (e.g. "My App License")
///   TOKEN_SYMBOL       — ERC-721 symbol (e.g. "MAL")
///   IDENTITY_MODEL     — 0 (access: user_id = wallet) | 1 (account: user_id = TBA)
///   PRICE              — purchase price in wei
///
/// Conditionally required:
///   TBA_IMPLEMENTATION — ERC-6551 account implementation address. Required
///                        when IDENTITY_MODEL=1 (account); must be 0x0 when
///                        IDENTITY_MODEL=0 (access).
///
/// Optional env vars:
///   WRAPPER_HASHES  - comma-separated bytes32 SHA-256 hashes of the launch
///                     release's wrapper binaries (one per platform). Seeds the
///                     append-only hash set; later builds are added on-chain with
///                     `addWrapperHash(bytes32)`. Empty = deploy with no hashes yet.
///   WRAPPER_HASH    - single-hash shorthand for WRAPPER_HASHES. Ignored when
///                     WRAPPER_HASHES is set; a zero hash is treated as "none".
///   PRICE_TOKEN     - ERC-20 (USDC or any EIP-3009 token) accepted alongside ETH
///                     (default: 0x0 = ETH only). Must implement EIP-3009; the
///                     constructor probes it and reverts if it does not.
///   PRICE_AMOUNT    - purchase price in PRICE_TOKEN's smallest unit (USDC has 6
///                     decimals, so 5000000 = 5 USDC). Must be 0 when PRICE_TOKEN
///                     is unset. 0 with a token set is a free stablecoin tier.
///   SUPPLY_CAP      — max mintable tokens; 0 = uncapped (default: 0)
///   OWNER           — contract owner address; defaults to the broadcaster
///   COOLDOWN_BLOCKS — blocks between activations per token (default: 1800, ~1hr on Base;
///                     floor is 15 ≈ 30s, enforced in the contract)
///   PREDECESSOR     - address of a license contract whose holders may migrate onto
///                     this one via `claimFromPredecessor` (default: 0x0 = no
///                     migrations accepted). Frozen at deploy. The predecessor's
///                     owner must also point its `successor` here for claims to work.
///   PERIOD          — subscription length in seconds (required for "subscription")
///
/// Usage — dry run (no broadcast):
///   source .env && forge script script/Deploy.s.sol \
///     --rpc-url $BASE_SEPOLIA_RPC_URL
///
/// Usage — broadcast + verify:
///   source .env && forge script script/Deploy.s.sol \
///     --rpc-url $BASE_SEPOLIA_RPC_URL \
///     --private-key $DEPLOYER_KEY \
///     --broadcast --verify --etherscan-api-key $BASESCAN_API_KEY
contract Deploy is Script {
    function run() external {
        // ── Required params ───────────────────────────────────────────────────
        string  memory contractType  = vm.envString("CONTRACT_TYPE");
        string  memory name_         = vm.envString("TOKEN_NAME");
        string  memory symbol_       = vm.envString("TOKEN_SYMBOL");
        uint8          identityModel = uint8(vm.envUint("IDENTITY_MODEL"));
        uint256        price         = vm.envUint("PRICE");

        // ── Optional params ───────────────────────────────────────────────────
        address priceToken     = vm.envOr("PRICE_TOKEN",     address(0));
        uint256 priceAmount    = vm.envOr("PRICE_AMOUNT",    uint256(0));
        uint256 supplyCap      = vm.envOr("SUPPLY_CAP",      uint256(0));
        uint256 cooldownBlocks = vm.envOr("COOLDOWN_BLOCKS", uint256(1800));
        address owner_         = vm.envOr("OWNER",           msg.sender);
        // period is only required for "subscription"; default 0 for "access"
        uint256 period         = _eq(contractType, "subscription") ? vm.envUint("PERIOD") : 0;
        // TBA implementation — required for account model, forbidden for access model.
        address tbaImpl        = vm.envOr("TBA_IMPLEMENTATION", address(0));
        // Contract whose holders may migrate onto this one. Immutable once deployed.
        address predecessor    = vm.envOr("PREDECESSOR", address(0));
        // Launch release binary hashes. The set is append-only from here on.
        bytes32[] memory wrapperHashes = _wrapperHashes();
        // Both rails in one value; `priceAmount` must be 0 when no token is set.
        Rub3License.SaleTerms memory sale = Rub3License.SaleTerms({
            price:       price,
            priceToken:  priceToken,
            priceAmount: priceAmount
        });

        // ── Deploy ────────────────────────────────────────────────────────────
        vm.startBroadcast();

        address deployed;

        if (_eq(contractType, "access")) {
            deployed = address(new Rub3Access(
                name_, symbol_, identityModel, tbaImpl, wrapperHashes,
                sale, supplyCap, cooldownBlocks, predecessor, owner_
            ));
        } else if (_eq(contractType, "subscription")) {
            deployed = address(new Rub3Subscription(
                name_, symbol_, identityModel, tbaImpl, wrapperHashes,
                sale, supplyCap, period, cooldownBlocks, predecessor, owner_
            ));
        } else {
            revert(string.concat("Deploy: unknown CONTRACT_TYPE '", contractType, "' (expected 'access' or 'subscription')"));
        }

        vm.stopBroadcast();

        // ── Summary ───────────────────────────────────────────────────────────
        console.log("");
        console.log("Deployed Rub3%s%s",
            _capitalize(contractType),
            block.chainid == 1 ? "" : " (not mainnet)"
        );
        console.log("  address:       %s", deployed);
        console.log("  chain:         %d", block.chainid);
        console.log("  name:          %s", name_);
        console.log("  symbol:        %s", symbol_);
        console.log("  identityModel: %d  (%s)", identityModel, identityModel == 0 ? "access" : "account");
        if (identityModel == 1) {
            console.log("  tbaImpl:       %s", tbaImpl);
        }
        console.log("  price:         %d wei", price);
        if (priceToken == address(0)) {
            console.log("  priceToken:    none  (ETH only)");
        } else {
            console.log("  priceToken:    %s", priceToken);
            console.log("  priceAmount:   %d  (token's smallest unit)", priceAmount);
        }
        console.log("  supplyCap:     %d  (%s)", supplyCap, supplyCap == 0 ? "uncapped" : "capped");
        console.log("  cooldown:      %d blocks (~%d sec on Base)", cooldownBlocks, cooldownBlocks * 2);
        console.log("  wrapperHashes: %d seeded%s",
            wrapperHashes.length,
            wrapperHashes.length == 0 ? "  (add later with addWrapperHash)" : ""
        );
        for (uint256 i = 0; i < wrapperHashes.length; i++) {
            console.log("                 %s", vm.toString(wrapperHashes[i]));
        }
        console.log("  predecessor:   %s%s",
            predecessor,
            predecessor == address(0) ? "  (no migrations accepted)" : ""
        );
        console.log("  owner:         %s", owner_);
        if (_eq(contractType, "subscription")) {
            console.log("  period:        %d sec", period);
            console.log("                 (~%d days)", period / 86400);
        }
    }

    /// Reads the launch release's wrapper hashes: `WRAPPER_HASHES` (comma-separated)
    /// wins, `WRAPPER_HASH` is the single-hash shorthand, and a zero or absent hash
    /// deploys with an empty set - the contract rejects `bytes32(0)` as a member
    /// because it is the `Unknown` sentinel.
    function _wrapperHashes() internal view returns (bytes32[] memory) {
        bytes32[] memory many = vm.envOr("WRAPPER_HASHES", ",", new bytes32[](0));
        if (many.length != 0) return many;

        bytes32 one = vm.envOr("WRAPPER_HASH", bytes32(0));
        if (one == bytes32(0)) return new bytes32[](0);

        bytes32[] memory single = new bytes32[](1);
        single[0] = one;
        return single;
    }

    function _eq(string memory a, string memory b) internal pure returns (bool) {
        return keccak256(bytes(a)) == keccak256(bytes(b));
    }

    // Returns a copy of `s` with the first character uppercased.
    // Must copy — `bytes(s)` aliases the original memory and would mutate the caller's string.
    function _capitalize(string memory s) internal pure returns (string memory) {
        bytes memory src = bytes(s);
        if (src.length == 0) return s;
        bytes memory dst = new bytes(src.length);
        for (uint256 i = 0; i < src.length; i++) dst[i] = src[i];
        if (dst[0] >= 0x61 && dst[0] <= 0x7a) dst[0] = bytes1(uint8(dst[0]) - 32);
        return string(dst);
    }
}
