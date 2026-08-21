// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {Rub3Factory} from "../src/Rub3Factory.sol";

/// @notice Deploys a {Rub3Factory}, the canonical deployment path and the point
///         at which the protocol's economics are chosen.
///
/// **This script is where rub3's take is decided, and it is decided once.** Both
/// values become `immutable` on the factory and are then stamped, immutably
/// again, into every license contract it deploys. Nothing afterwards can change
/// what a contract deployed by this factory charges. Shipping a different rate
/// means deploying a *new* factory, which affects only what it deploys.
///
/// Required env vars:
///   FEE_BPS  - protocol fee in basis points. Must be within the range the
///              factory enforces (MIN_FEE_BPS..MAX_FEE_BPS, currently 200-300,
///              i.e. 2.00% to 3.00%). There is no default: this is a decision,
///              not a setting to fall back on.
///   TREASURY - fee recipient. Must be non-zero.
///
/// Optional env var:
///   PREVIOUS_FACTORY - the Rub3Factory this one supersedes. Unset (or 0x0) for
///                      the *first* factory only. Set it on every later one:
///                      contracts recorded by the old factory are acceptable
///                      predecessors on the new one only through this pointer,
///                      and it is immutable, so a factory deployed without it
///                      can never be given one. See contracts.md ->
///                      "A factory deploy may only succeed a canonical
///                      predecessor".
///
/// Usage - dry run (no broadcast):
///   FEE_BPS=250 TREASURY=0x... forge script script/DeployFactory.s.sol \
///     --rpc-url $BASE_SEPOLIA_RPC_URL
///
/// Usage - broadcast + verify, superseding an existing factory:
///   FEE_BPS=250 TREASURY=0x... PREVIOUS_FACTORY=0x... \
///   forge script script/DeployFactory.s.sol \
///     --rpc-url $BASE_SEPOLIA_RPC_URL \
///     --private-key $DEPLOYER_KEY \
///     --broadcast --verify --etherscan-api-key $BASESCAN_API_KEY
///
/// Pass the resulting address to `script/Deploy.s.sol` as `FACTORY` to deploy a
/// license contract through it.
contract DeployFactory is Script {
    function run() external {
        uint16 feeBps = uint16(vm.envUint("FEE_BPS"));
        address treasury = vm.envAddress("TREASURY");
        address previousFactory = vm.envOr("PREVIOUS_FACTORY", address(0));

        vm.startBroadcast();
        Rub3Factory factory = new Rub3Factory(feeBps, treasury, previousFactory);
        vm.stopBroadcast();

        console.log("");
        console.log("Deployed Rub3Factory%s", block.chainid == 1 ? "" : " (not mainnet)");
        console.log("  address:              %s", address(factory));
        console.log("  chain:                %d", block.chainid);
        console.log("  feeBps:               %d  (of every payment, immutable)", factory.feeBps());
        console.log("  treasury:             %s", factory.treasury());
        console.log(
            "  previousFactory:      %s%s",
            factory.previousFactory(),
            factory.previousFactory() == address(0)
                ? "  (first factory: its deploys are the only canonical predecessors)"
                : ""
        );
        console.log("  accessDeployer:       %s", factory.accessDeployer());
        console.log("");
        console.log("  Deploy through it with FACTORY=%s", address(factory));
    }
}
