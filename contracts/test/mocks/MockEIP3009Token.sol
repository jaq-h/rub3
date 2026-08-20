// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {SignatureChecker} from "@openzeppelin/contracts/utils/cryptography/SignatureChecker.sol";

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
///
/// The authorization entry points take a `bytes signature`, the FiatTokenV2_2
/// form the license contracts require, and validate it through OpenZeppelin's
/// {SignatureChecker} - ECDSA recovery for a 65-byte EOA signature, falling
/// through to EIP-1271 `isValidSignature` for a contract signer. That is the
/// same signature checker Circle's implementation uses, and it is what lets a
/// smart-contract wallet buy a licence.
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
        bytes calldata signature
    ) external {
        _requireValidAuthorization(from, nonce, validAfter, validBefore);
        _requireValidSignature(
            from,
            keccak256(
                abi.encode(
                    TRANSFER_WITH_AUTHORIZATION_TYPEHASH,
                    from,
                    to,
                    value,
                    validAfter,
                    validBefore,
                    nonce
                )
            ),
            signature
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
        bytes calldata signature
    ) external {
        if (to != msg.sender) revert CallerMustBePayee();
        _requireValidAuthorization(from, nonce, validAfter, validBefore);
        _requireValidSignature(
            from,
            keccak256(
                abi.encode(
                    RECEIVE_WITH_AUTHORIZATION_TYPEHASH,
                    from,
                    to,
                    value,
                    validAfter,
                    validBefore,
                    nonce
                )
            ),
            signature
        );
        _markUsed(from, nonce);
        _transfer(from, to, value);
    }

    /// @notice Burn an unused authorization. A buyer's escape hatch, and the
    ///         second way an authorization can stop being spendable.
    function cancelAuthorization(address authorizer, bytes32 nonce, bytes calldata signature)
        external
    {
        if (authorizationState[authorizer][nonce]) revert AuthorizationUsedOrCanceled();
        _requireValidSignature(
            authorizer,
            keccak256(abi.encode(CANCEL_AUTHORIZATION_TYPEHASH, authorizer, nonce)),
            signature
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

    /// Mirrors FiatTokenV2_2: one signature checker for both kinds of signer,
    /// with no branch on signature length in the token's own code.
    function _requireValidSignature(address signer, bytes32 structHash, bytes calldata signature)
        private
        view
    {
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR(), structHash));
        if (!SignatureChecker.isValidSignatureNowCalldata(signer, digest, signature)) {
            revert InvalidSignature();
        }
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
        address,
        address,
        uint256,
        uint256,
        uint256,
        bytes32,
        bytes calldata
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

/// @notice An EIP-3009 token that answers the constructor probe and holds real
///         balances, but exposes no `DOMAIN_SEPARATOR()` getter.
///
/// EIP-3009 mandates the authorization functions and `authorizationState`, which
/// is the whole of what {Rub3License-_setTokenPrice}'s probe can check. The
/// `DOMAIN_SEPARATOR()` getter is an EIP-2612 convention layered on top, and a
/// token can be a conforming EIP-3009 token without it. A buyer then cannot
/// build the EIP-712 digest for it off-chain.
///
/// That is a fact about the token, not about the network, so the wrapper falls
/// back to the ETH rail rather than ending a purchase the ETH rail would have
/// completed. This fixture is what lets that be proven end to end.
contract NoDomainSeparatorEIP3009Token is ERC20 {
    mapping(address => mapping(bytes32 => bool)) public authorizationState;

    constructor() ERC20("No Domain", "NODOM") {}

    /// Matches the USDC stand-in, so a price quoted for one is quoted the same
    /// way for the other.
    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    /// Present so the token is spendable in principle; no test reaches it,
    /// because no authorization for it can be signed in the first place.
    function receiveWithAuthorization(
        address,
        address,
        uint256,
        uint256,
        uint256,
        bytes32,
        bytes calldata
    ) external {}
}

/// @notice A spec-conformant EIP-3009 token that implements **only** the
///         `(uint8 v, bytes32 r, bytes32 s)` form of `receiveWithAuthorization`.
///
/// EIP-3009 as written specifies exactly this form, so a token like this is
/// conforming, holds real balances, and passes the license contract's
/// `authorizationState` constructor probe. What it does not have is the
/// FiatTokenV2_2 `bytes signature` overload the license contracts call, and it
/// has no fallback, so that call reverts.
///
/// The contract cannot detect this at deploy time - a staticcall probe cannot
/// tell "no such function" from "bad signature", since both revert. The wrapper
/// detects it instead, by pre-flighting the real `purchaseWithAuthorization`
/// call before broadcasting, and falls back to the ETH rail. This fixture is
/// what lets that be proven end to end.
contract NoSignatureOverloadEIP3009Token is ERC20 {
    // keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
    bytes32 private constant _EIP712_DOMAIN_TYPEHASH =
        0x8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f;

    // keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 public constant RECEIVE_WITH_AUTHORIZATION_TYPEHASH =
        0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;

    mapping(address => mapping(bytes32 => bool)) public authorizationState;

    error AuthorizationNotYetValid();
    error AuthorizationExpired();
    error AuthorizationUsedOrCanceled();
    error InvalidSignature();
    error CallerMustBePayee();

    constructor() ERC20("Split Sig", "SPLIT") {}

    /// Matches the USDC stand-in, so a price quoted for one is quoted the same
    /// way for the other.
    function decimals() public pure override returns (uint8) {
        return 6;
    }

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

    /// The EIP-3009 signature form, and the only one this token has.
    function receiveWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external {
        if (to != msg.sender) revert CallerMustBePayee();
        if (block.timestamp <= validAfter) revert AuthorizationNotYetValid();
        if (block.timestamp >= validBefore) revert AuthorizationExpired();
        if (authorizationState[from][nonce]) revert AuthorizationUsedOrCanceled();

        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                DOMAIN_SEPARATOR(),
                keccak256(
                    abi.encode(
                        RECEIVE_WITH_AUTHORIZATION_TYPEHASH,
                        from,
                        to,
                        value,
                        validAfter,
                        validBefore,
                        nonce
                    )
                )
            )
        );
        address recovered = ecrecover(digest, v, r, s);
        if (recovered == address(0) || recovered != from) revert InvalidSignature();

        authorizationState[from][nonce] = true;
        _transfer(from, to, value);
    }
}

/// @notice A smart-contract wallet: the ERC-4337-shaped buyer the `bytes`
///         signature form exists to admit.
///
/// It holds no key of its own. `isValidSignature` delegates to whatever its
/// `owner` says, exactly as a Safe-style wallet delegates to its signer set, so
/// a signature by the owner is valid for the wallet and a signature by anyone
/// else is not.
///
/// `onERC721Received` is not optional decoration: the license contracts mint
/// with `_safeMint`, which reverts against a contract recipient that does not
/// implement it.
contract SmartWallet is IERC1271 {
    /// bytes4(keccak256("isValidSignature(bytes32,bytes)"))
    bytes4 private constant _MAGIC = 0x1626ba7e;

    address public immutable owner;

    constructor(address owner_) {
        owner = owner_;
    }

    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4) {
        return
            SignatureChecker.isValidSignatureNow(owner, hash, signature)
                ? _MAGIC
                : bytes4(0xffffffff);
    }

    function onERC721Received(address, address, uint256, bytes calldata)
        external
        pure
        returns (bytes4)
    {
        return this.onERC721Received.selector;
    }
}
