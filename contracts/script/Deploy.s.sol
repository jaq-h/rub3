// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {Rub3Access} from "../src/Rub3Access.sol";
import {Rub3Factory, Rub3LicenseParams} from "../src/Rub3Factory.sol";
import {Rub3License} from "../src/Rub3License.sol";

/// @notice Deploys a Rub3Access licence contract from environment variables.
///
/// Required env vars:
///   TOKEN_NAME         - ERC-721 name  (e.g. "My App License")
///   TOKEN_SYMBOL       - ERC-721 symbol (e.g. "MAL")
///   IDENTITY_MODEL     - 0 (access: user_id = wallet) | 1 (account: user_id = TBA)
///   PRICE              - purchase price in wei
///
/// Conditionally required:
///   TBA_IMPLEMENTATION - ERC-6551 account implementation address. Required
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
///   SUPPLY_CAP      - max mintable tokens; 0 = uncapped (default: 0)
///   OWNER           - contract owner address; defaults to the broadcaster
///   COOLDOWN_BLOCKS - blocks a *seat* must wait between activations (default: 1800,
///                     ~1hr on Base; floor is 15 ≈ 30s, enforced in the contract)
///   SEATS           - concurrent sessions one token grants (default: 1, which is the
///                     single-session tier-3 licence; ceiling is 64, enforced in the
///                     contract). A token lands at most SEATS activations per cooldown
///                     window, so seats multiply concurrency and never the churn rate
///   SESSION_TTL     - seconds a seat stays taken when nobody releases it (default:
///                     86400 = 24h; range 300 to 7776000, enforced in the contract).
///                     This is what frees a seat when a fleet instance dies without
///                     calling `release`. A wrapper takes the shorter of this and its
///                     own packed session TTL, so packaging can only shorten a session
///   PREDECESSOR     - address of a license contract whose holders may migrate onto
///                     this one via `claimFromPredecessor` (default: 0x0 = no
///                     migrations accepted). Frozen at deploy. The predecessor's
///                     owner must also point its `successor` here for claims to work.
///                     With FACTORY set, it must additionally be a contract that
///                     factory (or one in its `previousFactory` chain) recorded,
///                     or the deploy reverts `PredecessorNotCanonical(address)` -
///                     see contracts.md -> "A factory deploy may only succeed a
///                     canonical predecessor".
///   FACTORY         - address of a deployed Rub3Factory to deploy through
///                     (default: 0x0 = deploy directly). Going through a factory
///                     is what stamps the protocol fee and records the contract
///                     in `isDeployed`, which is what the registry and the
///                     marketplace list. Deploying directly still works and
///                     carries no fee; it is simply unrecorded. The fee terms
///                     are the factory's own immutables and cannot be chosen
///                     here - see contracts.md -> "Protocol fee".
///
/// Usage - dry run (no broadcast):
///   source .env && forge script script/Deploy.s.sol \
///     --rpc-url $BASE_SEPOLIA_RPC_URL
///
/// Usage - broadcast + verify:
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
        string name;
        string symbol;
        Rub3License.IdentityTerms identity;
        address predecessor;
        address owner;
        uint256 supplyCap;
        Rub3License.SessionTerms session;
        address factory;
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
        p.name = vm.envString("TOKEN_NAME");
        p.symbol = vm.envString("TOKEN_SYMBOL");
        p.identity.model = uint8(vm.envUint("IDENTITY_MODEL"));

        // ── Optional params ───────────────────────────────────────────────────
        p.supplyCap = vm.envOr("SUPPLY_CAP", uint256(0));
        p.session = Rub3License.SessionTerms({
            cooldownBlocks: vm.envOr("COOLDOWN_BLOCKS", uint256(1800)),
            seatsPerToken: vm.envOr("SEATS", uint256(1)),
            sessionTtlSeconds: vm.envOr("SESSION_TTL", uint256(24 hours))
        });
        p.owner = vm.envOr("OWNER", msg.sender);
        // TBA implementation - required for account model, forbidden for access model.
        p.identity.tbaImplementation = vm.envOr("TBA_IMPLEMENTATION", address(0));
        // Contract whose holders may migrate onto this one. Immutable once deployed.
        p.predecessor = vm.envOr("PREDECESSOR", address(0));
        // Deploy through a factory (fee-stamped and recorded) or directly (no fee).
        p.factory = vm.envOr("FACTORY", address(0));
        // Launch release binary hashes. The set is append-only from here on.
        p.wrapperHashes = _wrapperHashes();
        // Both rails in one value; `priceAmount` must be 0 when no token is set.
        p.sale = Rub3License.SaleTerms({
            price: vm.envUint("PRICE"),
            priceToken: vm.envOr("PRICE_TOKEN", address(0)),
            priceAmount: vm.envOr("PRICE_AMOUNT", uint256(0))
        });
    }

    /// Deploys the licence contract. Must run inside a broadcast.
    function _deploy(DeployParams memory p) internal returns (address) {
        if (p.factory != address(0)) return _deployViaFactory(p);

        // Direct deploy: no protocol fee, and no row in any factory's
        // `isDeployed`. Both halves of that are deliberate - see
        // {Rub3Factory}.
        Rub3License.FeeTerms memory noFee = Rub3License.FeeTerms({feeBps: 0, treasury: address(0)});

        return address(
            new Rub3Access(
                p.name,
                p.symbol,
                p.identity,
                p.wrapperHashes,
                p.sale,
                noFee,
                p.supplyCap,
                p.session,
                p.predecessor,
                p.owner
            )
        );
    }

    /// Deploys through `FACTORY`, which stamps its own immutable fee terms and
    /// records the result. The fee is not an input here and cannot be: the
    /// factory reads it off itself.
    function _deployViaFactory(DeployParams memory p) internal returns (address) {
        Rub3LicenseParams memory lp = Rub3LicenseParams({
            name: p.name,
            symbol: p.symbol,
            identity: p.identity,
            wrapperHashes: p.wrapperHashes,
            sale: p.sale,
            supplyCap: p.supplyCap,
            session: p.session,
            predecessor: p.predecessor,
            owner: p.owner
        });

        return Rub3Factory(p.factory).deployAccess(lp);
    }

    /// Prints what was deployed and on what terms.
    // This function prints a fixed-width, label-aligned block, and the source
    // is laid out to mirror that output: one `console.log` per printed line,
    // with the label column aligned inside the format strings. `forge fmt`
    // splits the handful of calls that run past `line_length` across four lines
    // each, which breaks the one-line-per-row correspondence and makes the
    // printed layout unreadable from the source. No `[fmt]` setting expresses
    // "leave this block as written", so the formatter's own marker does it
    // here, narrowly, rather than by loosening `line_length` for the whole tree.
    // forgefmt: disable-next-item
    function _summary(DeployParams memory p, address deployed) internal view {
        console.log("");
        console.log("Deployed Rub3Access%s", block.chainid == 1 ? "" : " (not mainnet)");
        console.log("  address:       %s", deployed);
        console.log("  chain:         %d", block.chainid);
        console.log("  name:          %s", p.name);
        console.log("  symbol:        %s", p.symbol);
        console.log("  identityModel: %d  (%s)",
            p.identity.model,
            p.identity.model == 0 ? "access" : "account"
        );
        if (p.identity.model == 1) {
            console.log("  tbaImpl:       %s", p.identity.tbaImplementation);
        }
        console.log("  price:         %d wei", p.sale.price);
        if (p.sale.priceToken == address(0)) {
            console.log("  priceToken:    none  (ETH only)");
        } else {
            console.log("  priceToken:    %s", p.sale.priceToken);
            console.log("  priceAmount:   %d  (token's smallest unit)", p.sale.priceAmount);
        }
        console.log("  supplyCap:     %d  (%s)", p.supplyCap, p.supplyCap == 0 ? "uncapped" : "capped");
        console.log("  cooldown:      %d blocks (~%d sec on Base), per seat", p.session.cooldownBlocks, p.session.cooldownBlocks * 2);
        console.log("  seats:         %d concurrent session%s per token", p.session.seatsPerToken, p.session.seatsPerToken == 1 ? "" : "s");
        console.log("  sessionTtl:    %d sec  (a seat frees itself after this)", p.session.sessionTtlSeconds);
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
        if (p.factory == address(0)) {
            console.log("  factory:       none  (direct deploy: no protocol fee, not registry-listable)");
        } else {
            console.log("  factory:       %s", p.factory);
            console.log("  feeBps:        %d  (frozen: %d bps to %s)",
                Rub3License(deployed).feeBps(),
                Rub3License(deployed).feeBps(),
                Rub3License(deployed).treasury()
            );
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
}
