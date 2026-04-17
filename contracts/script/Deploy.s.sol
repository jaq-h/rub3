// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {Rub3Access}       from "../src/Rub3Access.sol";
import {Rub3Subscription} from "../src/Rub3Subscription.sol";

/// @notice Deploys either Rub3Access or Rub3Subscription from environment variables.
///
/// Required env vars:
///   CONTRACT_TYPE   — "access" | "subscription"
///   TOKEN_NAME      — ERC-721 name  (e.g. "My App License")
///   TOKEN_SYMBOL    — ERC-721 symbol (e.g. "MAL")
///   IDENTITY_MODEL  — 0 (access: user_id = wallet) | 1 (account: user_id = TBA)
///   WRAPPER_HASH    — bytes32 hex of the distributed wrapper binary SHA-256
///   PRICE           — purchase price in wei
///
/// Optional env vars:
///   SUPPLY_CAP      — max mintable tokens; 0 = uncapped (default: 0)
///   OWNER           — contract owner address; defaults to the broadcaster
///   COOLDOWN_BLOCKS — blocks between activations per token (default: 1800, ~1hr on Base;
///                     floor is 15 ≈ 30s, enforced in the contract)
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
        bytes32        wrapperHash   = vm.envBytes32("WRAPPER_HASH");
        uint256        price         = vm.envUint("PRICE");

        // ── Optional params ───────────────────────────────────────────────────
        uint256 supplyCap      = vm.envOr("SUPPLY_CAP",      uint256(0));
        uint256 cooldownBlocks = vm.envOr("COOLDOWN_BLOCKS", uint256(1800));
        address owner_         = vm.envOr("OWNER",           msg.sender);
        // period is only required for "subscription"; default 0 for "access"
        uint256 period         = _eq(contractType, "subscription") ? vm.envUint("PERIOD") : 0;

        // ── Deploy ────────────────────────────────────────────────────────────
        vm.startBroadcast();

        address deployed;

        if (_eq(contractType, "access")) {
            deployed = address(new Rub3Access(
                name_, symbol_, identityModel, wrapperHash, price, supplyCap, cooldownBlocks, owner_
            ));
        } else if (_eq(contractType, "subscription")) {
            deployed = address(new Rub3Subscription(
                name_, symbol_, identityModel, wrapperHash, price, supplyCap, period, cooldownBlocks, owner_
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
        console.log("  price:         %d wei", price);
        console.log("  supplyCap:     %d  (%s)", supplyCap, supplyCap == 0 ? "uncapped" : "capped");
        console.log("  cooldown:      %d blocks (~%d sec on Base)", cooldownBlocks, cooldownBlocks * 2);
        console.log("  owner:         %s", owner_);
        if (_eq(contractType, "subscription")) {
            console.log("  period:        %d sec", period);
            console.log("                 (~%d days)", period / 86400);
        }
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
