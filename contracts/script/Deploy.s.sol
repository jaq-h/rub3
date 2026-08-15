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
    /// @dev Every deploy input in one value.
    ///
    ///      Grouped for the same reason {Rub3License-SaleTerms} is: `run()`
    ///      reads more than a dozen environment variables, and holding each one
    ///      in its own local put the function past solc's stack limit once the
    ///      stablecoin rail added two more. A memory pointer costs one slot, and
    ///      the reading, the deploying, and the summary each get their own
    ///      frame.
    struct DeployParams {
        string  contractType;
        string  name;
        string  symbol;
        uint8   identityModel;
        address tbaImpl;
        address predecessor;
        address owner;
        uint256 supplyCap;
        uint256 cooldownBlocks;
        uint256 period;
        Rub3License.SaleTerms sale;
        bytes32[] wrapperHashes;
    }

    function run() external {
        DeployParams memory p = _params();

        vm.startBroadcast();
        address deployed = _deploy(p);
        vm.stopBroadcast();

        _summary(p, deployed);
    }

    /// Reads every deploy input from the environment.
    function _params() internal view returns (DeployParams memory p) {
        // ── Required params ───────────────────────────────────────────────────
        p.contractType  = vm.envString("CONTRACT_TYPE");
        p.name          = vm.envString("TOKEN_NAME");
        p.symbol        = vm.envString("TOKEN_SYMBOL");
        p.identityModel = uint8(vm.envUint("IDENTITY_MODEL"));

        // ── Optional params ───────────────────────────────────────────────────
        p.supplyCap      = vm.envOr("SUPPLY_CAP",      uint256(0));
        p.cooldownBlocks = vm.envOr("COOLDOWN_BLOCKS", uint256(1800));
        p.owner          = vm.envOr("OWNER",           msg.sender);
        // period is only required for "subscription"; default 0 for "access"
        p.period         = _eq(p.contractType, "subscription") ? vm.envUint("PERIOD") : 0;
        // TBA implementation - required for account model, forbidden for access model.
        p.tbaImpl        = vm.envOr("TBA_IMPLEMENTATION", address(0));
        // Contract whose holders may migrate onto this one. Immutable once deployed.
        p.predecessor    = vm.envOr("PREDECESSOR", address(0));
        // Launch release binary hashes. The set is append-only from here on.
        p.wrapperHashes  = _wrapperHashes();
        // Both rails in one value; `priceAmount` must be 0 when no token is set.
        p.sale = Rub3License.SaleTerms({
            price:       vm.envUint("PRICE"),
            priceToken:  vm.envOr("PRICE_TOKEN",  address(0)),
            priceAmount: vm.envOr("PRICE_AMOUNT", uint256(0))
        });
    }

    /// Deploys the contract `CONTRACT_TYPE` names. Must run inside a broadcast.
    function _deploy(DeployParams memory p) internal returns (address) {
        if (_eq(p.contractType, "access")) {
            return address(new Rub3Access(
                p.name, p.symbol, p.identityModel, p.tbaImpl, p.wrapperHashes,
                p.sale, p.supplyCap, p.cooldownBlocks, p.predecessor, p.owner
            ));
        }
        if (_eq(p.contractType, "subscription")) {
            return address(new Rub3Subscription(
                p.name, p.symbol, p.identityModel, p.tbaImpl, p.wrapperHashes,
                p.sale, p.supplyCap, p.period, p.cooldownBlocks, p.predecessor, p.owner
            ));
        }
        revert(string.concat("Deploy: unknown CONTRACT_TYPE '", p.contractType, "' (expected 'access' or 'subscription')"));
    }

    /// Prints what was deployed and on what terms.
    function _summary(DeployParams memory p, address deployed) internal view {
        console.log("");
        console.log("Deployed Rub3%s%s",
            _capitalize(p.contractType),
            block.chainid == 1 ? "" : " (not mainnet)"
        );
        console.log("  address:       %s", deployed);
        console.log("  chain:         %d", block.chainid);
        console.log("  name:          %s", p.name);
        console.log("  symbol:        %s", p.symbol);
        console.log("  identityModel: %d  (%s)", p.identityModel, p.identityModel == 0 ? "access" : "account");
        if (p.identityModel == 1) {
            console.log("  tbaImpl:       %s", p.tbaImpl);
        }
        console.log("  price:         %d wei", p.sale.price);
        if (p.sale.priceToken == address(0)) {
            console.log("  priceToken:    none  (ETH only)");
        } else {
            console.log("  priceToken:    %s", p.sale.priceToken);
            console.log("  priceAmount:   %d  (token's smallest unit)", p.sale.priceAmount);
        }
        console.log("  supplyCap:     %d  (%s)", p.supplyCap, p.supplyCap == 0 ? "uncapped" : "capped");
        console.log("  cooldown:      %d blocks (~%d sec on Base)", p.cooldownBlocks, p.cooldownBlocks * 2);
        console.log("  wrapperHashes: %d seeded%s",
            p.wrapperHashes.length,
            p.wrapperHashes.length == 0 ? "  (add later with addWrapperHash)" : ""
        );
        for (uint256 i = 0; i < p.wrapperHashes.length; i++) {
            console.log("                 %s", vm.toString(p.wrapperHashes[i]));
        }
        console.log("  predecessor:   %s%s",
            p.predecessor,
            p.predecessor == address(0) ? "  (no migrations accepted)" : ""
        );
        console.log("  owner:         %s", p.owner);
        if (_eq(p.contractType, "subscription")) {
            console.log("  period:        %d sec", p.period);
            console.log("                 (~%d days)", p.period / 86400);
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
