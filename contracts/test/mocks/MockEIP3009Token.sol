// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @notice A faithful, minimal EIP-3009 token, standing in for USDC in tests.
///
/// **Why a mock rather than a fork or a deployed token.** `forge test` and the
/// anvil-gated end-to-end job both run with no network and no `.env`, so a fork
/// test would make the suite depend on an RPC endpoint and a pinned block, and
/// there is no USDC on a fresh anvil to deploy against. What the tests actually
/// need to exercise is the *authorization protocol* - the EIP-712 domain, the
/// canonical typehashes, `msg.sender == to` on the receive path, and single-use
/// nonces - and all of that is specified by EIP-3009 rather than by USDC. This
/// implements exactly that surface, with the same domain shape and the same
/// typehash strings as Circle's FiatTokenV2, so a signature built for this token
/// is built the same way as one for real USDC. What it deliberately does not
/// model is USDC's blocklist, pausing, and upgrade proxy, none of which the
/// license contracts touch.
contract MockEIP3009Token is ERC20 {
    // keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
    bytes32 private constant _EIP712_DOMAIN_TYPEHASH =
        0x8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f;

    // keccak256("TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 public constant TRANSFER_WITH_AUTHORIZATION_TYPEHASH =
        0x7c7c6cdb67a18743f49ec6fa9b35f50d52ed05cbed4cc592e13b44501c1a2267;

    // keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 public constant RECEIVE_WITH_AUTHORIZATION_TYPEHASH =
        0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;

    // keccak256("CancelAuthorization(address authorizer,bytes32 nonce)")
    bytes32 public constant CANCEL_AUTHORIZATION_TYPEHASH =
        0x158b0a9edf7a828aad02f63cd515c68ef2f50ba807396f6d12842833a1597429;

    /// @notice `true` once an authorization has been used or cancelled.
    mapping(address => mapping(bytes32 => bool)) public authorizationState;

    event AuthorizationUsed(address indexed authorizer, bytes32 indexed nonce);
    event AuthorizationCanceled(address indexed authorizer, bytes32 indexed nonce);

    error AuthorizationNotYetValid();
    error AuthorizationExpired();
    error AuthorizationUsedOrCanceled();
    error InvalidSignature();
    error CallerMustBePayee();

    constructor() ERC20("USD Coin", "USDC") {}

    /// USDC has 6 decimals, and getting that wrong in a fixture hides a whole
    /// class of amount bug.
    function decimals() public pure override returns (uint8) {
        return 6;
    }

    /// Test-only faucet. Real USDC mints through a minter role; nothing in the
    /// license contracts can tell the difference.
    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function DOMAIN_SEPARATOR() public view returns (bytes32) {
        return keccak256(
            abi.encode(
                _EIP712_DOMAIN_TYPEHASH,
                keccak256(bytes(name())),
                keccak256(bytes("2")),
                block.chainid,
                address(this)
            )
        );
    }

    /// @notice The EIP-3009 path anyone may submit. Present so tests can prove
    ///         it is *not* what the license contracts use: a signature for the
    ///         receive typehash is rejected here, and vice versa.
    function transferWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        uint8   v,
        bytes32 r,
        bytes32 s
    ) external {
        _requireValidAuthorization(from, nonce, validAfter, validBefore);
        _requireValidSignature(
            from,
            keccak256(
                abi.encode(
                    TRANSFER_WITH_AUTHORIZATION_TYPEHASH,
                    from, to, value, validAfter, validBefore, nonce
                )
            ),
            v, r, s
        );
        _markUsed(from, nonce);
        _transfer(from, to, value);
    }

    /// @notice The front-running-proof path: only the payee may submit it.
    function receiveWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        uint8   v,
        bytes32 r,
        bytes32 s
    ) external {
        if (to != msg.sender) revert CallerMustBePayee();
        _requireValidAuthorization(from, nonce, validAfter, validBefore);
        _requireValidSignature(
            from,
            keccak256(
                abi.encode(
                    RECEIVE_WITH_AUTHORIZATION_TYPEHASH,
                    from, to, value, validAfter, validBefore, nonce
                )
            ),
            v, r, s
        );
        _markUsed(from, nonce);
        _transfer(from, to, value);
    }

    /// @notice Burn an unused authorization. A buyer's escape hatch, and the
    ///         second way an authorization can stop being spendable.
    function cancelAuthorization(address authorizer, bytes32 nonce, uint8 v, bytes32 r, bytes32 s)
        external
    {
        if (authorizationState[authorizer][nonce]) revert AuthorizationUsedOrCanceled();
        _requireValidSignature(
            authorizer,
            keccak256(abi.encode(CANCEL_AUTHORIZATION_TYPEHASH, authorizer, nonce)),
            v, r, s
        );
        authorizationState[authorizer][nonce] = true;
        emit AuthorizationCanceled(authorizer, nonce);
    }

    function _requireValidAuthorization(
        address from,
        bytes32 nonce,
        uint256 validAfter,
        uint256 validBefore
    ) private view {
        if (block.timestamp <= validAfter) revert AuthorizationNotYetValid();
        if (block.timestamp >= validBefore) revert AuthorizationExpired();
        if (authorizationState[from][nonce]) revert AuthorizationUsedOrCanceled();
    }

    function _requireValidSignature(
        address signer,
        bytes32 structHash,
        uint8   v,
        bytes32 r,
        bytes32 s
    ) private view {
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR(), structHash));
        address recovered = ecrecover(digest, v, r, s);
        if (recovered == address(0) || recovered != signer) revert InvalidSignature();
    }

    function _markUsed(address from, bytes32 nonce) private {
        authorizationState[from][nonce] = true;
        emit AuthorizationUsed(from, nonce);
    }
}

/// @notice An ERC-20 that answers the EIP-3009 read slice - so it passes the
///         constructor probe - but moves no money when an authorization is
///         spent. Stands in for a broken or hostile payment token.
///
/// The license contracts' balance-delta check is what catches it: the mint is
/// conditional on the funds actually arriving, not on the token not reverting.
contract SilentEIP3009Token is ERC20 {
    mapping(address => mapping(bytes32 => bool)) public authorizationState;

    constructor() ERC20("Silent", "SIL") {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function receiveWithAuthorization(
        address, address, uint256, uint256, uint256, bytes32, uint8, bytes32, bytes32
    ) external {}
}

/// @notice A contract with code that is not a token at all. The constructor
///         probe must reject it rather than deploying a rail that reverts for
///         every buyer.
contract NotAToken {
    function hello() external pure returns (uint256) {
        return 1;
    }
}
