// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Test}              from "forge-std/Test.sol";
import {Rub3Access}        from "../src/Rub3Access.sol";
import {Rub3License}       from "../src/Rub3License.sol";
import {Rub3Subscription}  from "../src/Rub3Subscription.sol";
import {
    MockEIP3009Token,
    NoSignatureOverloadEIP3009Token,
    NotAToken,
    SilentEIP3009Token,
    SmartWallet
} from "./mocks/MockEIP3009Token.sol";

/// @notice The stablecoin rail: `purchaseWithAuthorization` /
///         `renewWithAuthorization` (implementation.md §2.2).
///
/// The premise under test is that an agent holds USDC and no ETH, so the buyer
/// in every test below is funded with stablecoin and *deliberately left with a
/// zero ETH balance*. Someone else submits the transaction, which is what makes
/// the purchase gasless, and the tests check that being able to submit gets the
/// submitter nothing they were not given.
contract Rub3TokenPurchaseTest is Test {
    // keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 internal constant RECEIVE_TYPEHASH =
        0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;
    // keccak256("CancelAuthorization(address authorizer,bytes32 nonce)")
    bytes32 internal constant CANCEL_TYPEHASH =
        0x158b0a9edf7a828aad02f63cd515c68ef2f50ba807396f6d12842833a1597429;

    uint256 internal constant BUYER_PK    = 0xA11CE5E;
    uint256 internal constant OUTSIDER_PK = 0xDECAFBAD;
    /// The key a smart-contract wallet's `isValidSignature` defers to. The
    /// wallet holds the money; this key only says yes.
    uint256 internal constant WALLET_OWNER_PK = 0x5A1E70;

    bytes32 internal constant WRAPPER_HASH    = keccak256("test-wrapper-v1");
    uint256 internal constant PRICE           = 0.05 ether;
    uint256 internal constant USDC_PRICE      = 5_000_000; // 5 USDC, 6 decimals
    uint256 internal constant COOLDOWN_BLOCKS = 15;
    uint256 internal constant PERIOD          = 30 days;

    address internal owner     = address(0x00E);
    address internal submitter = address(0x5B417); // facilitator: pays gas, nothing else
    address internal attacker  = address(0xBAD);
    address internal buyer;
    address internal outsider;

    MockEIP3009Token  internal usdc;
    Rub3Access        internal nft;
    Rub3Subscription  internal sub;

    function setUp() public {
        buyer    = vm.addr(BUYER_PK);
        outsider = vm.addr(OUTSIDER_PK);

        usdc = new MockEIP3009Token();
        nft  = _deployAccess(_sale(PRICE, address(usdc), USDC_PRICE));
        sub  = _deploySubscription(_sale(PRICE, address(usdc), USDC_PRICE));

        usdc.mint(buyer, 1_000_000_000); // 1000 USDC
        vm.deal(submitter, 10 ether);
        vm.deal(attacker,  10 ether);

        // The whole point: the buyer holds stablecoin and not one wei.
        vm.deal(buyer, 0);
    }

    // ── Fixtures ──────────────────────────────────────────────────────────────

    function _identity(uint8 model, address tbaImplementation)
        internal
        pure
        returns (Rub3License.IdentityTerms memory)
    {
        return Rub3License.IdentityTerms({model: model, tbaImplementation: tbaImplementation});
    }

    /// No protocol fee - what a direct (non-factory) deploy carries, and what
    /// every fixture in this suite uses. The fee split has its own suite in
    /// `Rub3Factory.t.sol`.
    function _noFee() internal pure returns (Rub3License.FeeTerms memory) {
        return Rub3License.FeeTerms({feeBps: 0, treasury: address(0)});
    }

    function _hashes(bytes32 h) internal pure returns (bytes32[] memory out) {
        out = new bytes32[](1);
        out[0] = h;
    }

    function _sale(uint256 price, address token, uint256 amount)
        internal
        pure
        returns (Rub3License.SaleTerms memory)
    {
        return Rub3License.SaleTerms({price: price, priceToken: token, priceAmount: amount});
    }

    function _deployAccess(Rub3License.SaleTerms memory sale) internal returns (Rub3Access) {
        return new Rub3Access(
            "Rub3 Test", "R3T", _identity(0, address(0)),
            _hashes(WRAPPER_HASH), sale, _noFee(), 0, COOLDOWN_BLOCKS, address(0), owner
        );
    }

    function _deploySubscription(Rub3License.SaleTerms memory sale)
        internal
        returns (Rub3Subscription)
    {
        return new Rub3Subscription(
            "Rub3 Sub", "R3S", _identity(0, address(0)),
            _hashes(WRAPPER_HASH), sale, _noFee(), 0, PERIOD, COOLDOWN_BLOCKS, address(0), owner
        );
    }

    // ── Authorization helpers ─────────────────────────────────────────────────

    /// Signs a `ReceiveWithAuthorization` exactly as a buyer's wallet would:
    /// EIP-712 over the *token's* domain, never the licence contract's.
    ///
    /// `signer` is the address the authorization is *from*, which is the wallet
    /// for a smart-contract buyer and `vm.addr(pk)` for an EOA. Returns the
    /// standard 65-byte `r || s || v` packing, which is what an EOA signature
    /// is and what a signature checker recovers from.
    function _sign(
        uint256 pk,
        address signer,
        address payee,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce
    ) internal view returns (bytes memory) {
        return _signFor(
            usdc.DOMAIN_SEPARATOR(), pk, signer, payee, value, validAfter, validBefore, nonce
        );
    }

    /// The same, against an explicitly named EIP-712 domain.
    ///
    /// Every authorization is domain-separated by the token that will check it,
    /// so a test that targets a payment token other than {usdc} has to sign
    /// against *that* token's domain or it proves nothing but a bad signature.
    function _signFor(
        bytes32 domainSeparator,
        uint256 pk,
        address signer,
        address payee,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce
    ) internal pure returns (bytes memory) {
        bytes32 structHash = keccak256(
            abi.encode(
                RECEIVE_TYPEHASH, signer, payee, value, validAfter, validBefore, nonce
            )
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", domainSeparator, structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, digest);
        return abi.encodePacked(r, s, v);
    }

    /// Unpacks `r || s || v` so a signature built for the `bytes` form can be
    /// handed to a token that only implements the split form.
    function _split(bytes memory signature) internal pure returns (uint8 v, bytes32 r, bytes32 s) {
        require(signature.length == 65, "signature must be 65 bytes");
        assembly {
            r := mload(add(signature, 0x20))
            s := mload(add(signature, 0x40))
            v := byte(0, mload(add(signature, 0x60)))
        }
    }

    /// A purchase authorization for `recipient` on `target`, signed by `pk`.
    function _purchaseAuth(uint256 pk, Rub3License target, address recipient, uint256 value, bytes32 salt)
        internal
        view
        returns (Rub3License.PaymentAuthorization memory auth)
    {
        return _purchaseAuthFrom(pk, vm.addr(pk), target, recipient, value, salt);
    }

    /// The same, for a buyer whose `from` is not the address that signs: a
    /// smart-contract wallet pays out of its own balance while its owner key
    /// produces the signature.
    function _purchaseAuthFrom(
        uint256 pk,
        address from,
        Rub3License target,
        address recipient,
        uint256 value,
        bytes32 salt
    ) internal view returns (Rub3License.PaymentAuthorization memory auth) {
        auth.from        = from;
        auth.validAfter  = 0;
        auth.validBefore = block.timestamp + 1 hours;
        auth.salt        = salt;
        auth.signature   = _sign(
            pk,
            from,
            address(target),
            value,
            auth.validAfter,
            auth.validBefore,
            target.purchaseAuthorizationNonce(recipient, salt)
        );
    }

    function _renewAuth(uint256 pk, Rub3Subscription target, uint256 tokenId, uint256 value, bytes32 salt)
        internal
        view
        returns (Rub3License.PaymentAuthorization memory auth)
    {
        auth.from        = vm.addr(pk);
        auth.validAfter  = 0;
        auth.validBefore = block.timestamp + 1 hours;
        auth.salt        = salt;
        auth.signature   = _sign(
            pk,
            vm.addr(pk),
            address(target),
            value,
            auth.validAfter,
            auth.validBefore,
            target.renewAuthorizationNonce(tokenId, salt)
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 1. The premise: USDC in, licence out, no ETH from the buyer
    // ══════════════════════════════════════════════════════════════════════════

    /// The §2.2 thesis in one test. A buyer with a zero ETH balance ends up
    /// holding the licence, and the only account that spent gas is the
    /// submitter.
    function test_purchase_buyerWithNoEthGetsTheLicence() public {
        assertEq(buyer.balance, 0, "premise: the buyer holds no ETH");

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        uint256 tokenId = nft.purchaseWithAuthorization(address(0), auth);

        assertEq(nft.ownerOf(tokenId), buyer, "the licence goes to the buyer");
        assertEq(buyer.balance, 0, "the buyer spent no ETH at all");
        assertEq(usdc.balanceOf(buyer), 1_000_000_000 - USDC_PRICE);
        assertEq(usdc.balanceOf(address(nft)), USDC_PRICE, "the contract holds the payment");
        assertEq(nft.balanceOf(submitter), 0, "the submitter gets nothing");
    }

    /// `recipient == address(0)` means the buyer, never the submitter. Getting
    /// this backwards would hand every gasless purchase to the facilitator.
    function test_purchase_zeroRecipientMintsToTheBuyerNotTheSubmitter() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        uint256 tokenId = nft.purchaseWithAuthorization(address(0), auth);

        assertEq(nft.ownerOf(tokenId), buyer);
    }

    /// Buying for somebody else is allowed - the buyer signs for that recipient,
    /// so the intent is theirs.
    function test_purchase_buyerCanPayForAnotherRecipient() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, outsider, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        uint256 tokenId = nft.purchaseWithAuthorization(outsider, auth);

        assertEq(nft.ownerOf(tokenId), outsider);
        assertEq(usdc.balanceOf(buyer), 1_000_000_000 - USDC_PRICE, "the buyer paid");
    }

    /// Both rails, one mint: a token bought with USDC is indistinguishable from
    /// one bought with ETH.
    function test_purchase_bothRailsMintIdentically() public {
        vm.deal(outsider, 1 ether);
        vm.prank(outsider);
        uint256 ethId = nft.purchase{value: PRICE}(outsider);

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");
        vm.prank(submitter);
        uint256 usdcId = nft.purchaseWithAuthorization(address(0), auth);

        assertEq(usdcId, ethId + 1, "same sequential id allocation");
        assertEq(nft.ownerOf(ethId),  outsider);
        assertEq(nft.ownerOf(usdcId), buyer);
        assertEq(nft.nextTokenId(), 2);

        // Same downstream capability: both activate on the same terms.
        vm.prank(outsider);
        assertEq(nft.activate(ethId), 1);
        vm.prank(buyer);
        assertEq(nft.activate(usdcId), 2);
    }

    /// The `Purchased` event is the same event on both rails, and names whoever
    /// actually paid - not the facilitator who merely carried the message.
    function test_purchase_emitsTheSameEventNamingTheRealPayer() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.expectEmit(true, true, true, true);
        emit Rub3Access.Purchased(0, buyer, buyer);
        vm.prank(submitter);
        nft.purchaseWithAuthorization(address(0), auth);
    }

    /// The supply cap is a property of the licence, not of the rail.
    function test_purchase_respectsSupplyCap() public {
        Rub3Access capped = new Rub3Access(
            "Capped", "CAP", _identity(0, address(0)),
            _hashes(WRAPPER_HASH), _sale(PRICE, address(usdc), USDC_PRICE), _noFee(),
            1, COOLDOWN_BLOCKS, address(0), owner
        );

        Rub3License.PaymentAuthorization memory first =
            _purchaseAuth(BUYER_PK, capped, buyer, USDC_PRICE, "salt-1");
        vm.prank(submitter);
        capped.purchaseWithAuthorization(address(0), first);

        Rub3License.PaymentAuthorization memory second =
            _purchaseAuth(BUYER_PK, capped, buyer, USDC_PRICE, "salt-2");
        vm.prank(submitter);
        vm.expectRevert(Rub3License.SoldOut.selector);
        capped.purchaseWithAuthorization(address(0), second);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 1b. Smart-contract wallets (EIP-1271)
    // ══════════════════════════════════════════════════════════════════════════

    /// The reason the authorization carries opaque `bytes` rather than split
    /// `(v, r, s)`: an ERC-4337-shaped agent wallet holds the USDC and has no
    /// key of its own, so its signature is an EIP-1271 signature its own code
    /// validates. It buys a licence on the same one entry point an EOA uses.
    function test_smartWallet_eip1271BuyerGetsTheLicence() public {
        SmartWallet wallet = new SmartWallet(vm.addr(WALLET_OWNER_PK));
        usdc.mint(address(wallet), 1_000_000_000);

        Rub3License.PaymentAuthorization memory auth = _purchaseAuthFrom(
            WALLET_OWNER_PK, address(wallet), nft, address(wallet), USDC_PRICE, "salt-1"
        );

        vm.prank(submitter);
        uint256 tokenId = nft.purchaseWithAuthorization(address(0), auth);

        assertEq(nft.ownerOf(tokenId), address(wallet), "the smart wallet holds the licence");
        assertEq(address(wallet).balance, 0, "and never held a wei of ETH");
        assertEq(usdc.balanceOf(address(wallet)), 1_000_000_000 - USDC_PRICE);
        assertEq(usdc.balanceOf(address(nft)), USDC_PRICE);
    }

    /// The same wallet, a signature it does not accept. The token's signature
    /// checker asks the wallet, the wallet says no, and nothing happens: no
    /// licence, and not a cent moved.
    function test_smartWallet_aSignatureTheWalletRejectsBuysNothing() public {
        SmartWallet wallet = new SmartWallet(vm.addr(WALLET_OWNER_PK));
        usdc.mint(address(wallet), 1_000_000_000);

        Rub3License.PaymentAuthorization memory auth = _purchaseAuthFrom(
            OUTSIDER_PK, address(wallet), nft, address(wallet), USDC_PRICE, "salt-1"
        );

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        nft.purchaseWithAuthorization(address(0), auth);

        assertEq(nft.nextTokenId(), 0, "no licence was minted");
        assertEq(usdc.balanceOf(address(wallet)), 1_000_000_000, "the wallet keeps its money");
        assertEq(usdc.balanceOf(address(nft)), 0);
    }

    /// A smart wallet renews on the same terms, so the rail is not
    /// purchase-only for the buyers it was widened for.
    function test_smartWallet_renewsItsOwnSubscription() public {
        SmartWallet wallet = new SmartWallet(vm.addr(WALLET_OWNER_PK));
        usdc.mint(address(wallet), 1_000_000_000);

        Rub3License.PaymentAuthorization memory buy = _purchaseAuthFrom(
            WALLET_OWNER_PK, address(wallet), sub, address(wallet), USDC_PRICE, "salt-1"
        );
        vm.prank(submitter);
        uint256 tokenId = sub.purchaseWithAuthorization(address(0), buy);
        uint256 before  = sub.expiresAt(tokenId);

        Rub3License.PaymentAuthorization memory renewal;
        renewal.from        = address(wallet);
        renewal.validAfter  = 0;
        renewal.validBefore = block.timestamp + 1 hours;
        renewal.salt        = "salt-2";
        renewal.signature   = _sign(
            WALLET_OWNER_PK,
            address(wallet),
            address(sub),
            USDC_PRICE,
            renewal.validAfter,
            renewal.validBefore,
            sub.renewAuthorizationNonce(tokenId, renewal.salt)
        );

        vm.prank(submitter);
        sub.renewWithAuthorization(tokenId, renewal);

        assertEq(sub.expiresAt(tokenId), before + PERIOD);
        assertEq(usdc.balanceOf(address(sub)), USDC_PRICE * 2);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 2. Replay, front-running, and misdirection
    // ══════════════════════════════════════════════════════════════════════════

    /// The money path's first rule: an authorization is spendable once.
    function test_replay_secondSubmissionOfTheSameAuthorizationReverts() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        nft.purchaseWithAuthorization(address(0), auth);

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.AuthorizationUsedOrCanceled.selector);
        nft.purchaseWithAuthorization(address(0), auth);

        assertEq(nft.nextTokenId(), 1, "no second token was minted");
        assertEq(usdc.balanceOf(address(nft)), USDC_PRICE, "the buyer was charged once");
    }

    /// Replaying against a *different* recipient is the same authorization with
    /// a different derived nonce, so it is not a replay the token even sees: the
    /// digest no longer matches.
    function test_replay_sameSaltDifferentRecipientIsNotSignedFor() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(attacker);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        nft.purchaseWithAuthorization(attacker, auth);
    }

    /// The front-run that binding the recipient into the nonce exists to stop:
    /// an attacker watching the mempool takes the buyer's authorization and
    /// tries to have the licence minted to themselves with the buyer's money.
    /// It fails, and the honest submission afterwards still works.
    function test_frontRun_cannotDivertTheMintToTheSubmitter() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(attacker);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        nft.purchaseWithAuthorization(attacker, auth);

        assertEq(usdc.balanceOf(buyer), 1_000_000_000, "not a cent moved");

        vm.prank(submitter);
        uint256 tokenId = nft.purchaseWithAuthorization(address(0), auth);
        assertEq(nft.ownerOf(tokenId), buyer);
    }

    /// The other half of the front-running defence, and the reason these
    /// contracts call `receiveWithAuthorization` rather than
    /// `transferWithAuthorization`: the authorization cannot be spent *outside*
    /// the licence contract to strip the mint off the payment.
    ///
    /// The two share their six signed fields but not their typehash, so a
    /// signature for one is not a signature for the other. Attempting it takes
    /// nothing from the buyer, and leaves the authorization spendable.
    function test_frontRun_cannotStripTheMintByCallingTheTokenDirectly() public {
        bytes32 nonce = nft.purchaseAuthorizationNonce(buyer, "salt-1");
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(attacker);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        usdc.transferWithAuthorization(
            buyer, address(nft), USDC_PRICE,
            auth.validAfter, auth.validBefore, nonce,
            auth.signature
        );

        assertEq(usdc.balanceOf(buyer), 1_000_000_000, "the buyer keeps their money");
        assertFalse(usdc.authorizationState(buyer, nonce), "the nonce is still unspent");

        vm.prank(submitter);
        assertEq(nft.ownerOf(nft.purchaseWithAuthorization(address(0), auth)), buyer);
    }

    /// Even the licence contract's own payee-only path is closed to a third
    /// party: `receiveWithAuthorization` requires `msg.sender == to`.
    function test_frontRun_attackerCannotCallReceiveWithAuthorizationItself() public {
        bytes32 nonce = nft.purchaseAuthorizationNonce(buyer, "salt-1");
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(attacker);
        vm.expectRevert(MockEIP3009Token.CallerMustBePayee.selector);
        usdc.receiveWithAuthorization(
            buyer, address(nft), USDC_PRICE,
            auth.validAfter, auth.validBefore, nonce,
            auth.signature
        );
    }

    /// An authorization names one licence contract and is worthless at any
    /// other, even one selling at the same price in the same token.
    function test_wrongContract_authorizationDoesNotTransplant() public {
        Rub3Access other = _deployAccess(_sale(PRICE, address(usdc), USDC_PRICE));

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        other.purchaseWithAuthorization(address(0), auth);
    }

    /// A purchase authorization is not a renewal authorization. The domain tags
    /// keep the two derivations disjoint even for the same salt and signer.
    function test_wrongIntent_purchaseAuthorizationCannotRenew() public {
        uint256 tokenId = _buySubscriptionWithUsdc("salt-1");

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, sub, buyer, USDC_PRICE, "salt-2");

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        sub.renewWithAuthorization(tokenId, auth);
    }

    /// And a renewal authorization is bound to one token id, so it cannot be
    /// redirected onto somebody else's subscription.
    function test_wrongIntent_renewalAuthorizationIsBoundToItsToken() public {
        uint256 mine   = _buySubscriptionWithUsdc("salt-1");
        uint256 theirs = _buySubscriptionWithUsdc("salt-2");

        Rub3License.PaymentAuthorization memory auth =
            _renewAuth(BUYER_PK, sub, mine, USDC_PRICE, "salt-3");

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        sub.renewWithAuthorization(theirs, auth);
    }

    /// The validity window is the token's to enforce, and it does.
    function test_window_expiredAuthorizationReverts() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.warp(auth.validBefore + 1);

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.AuthorizationExpired.selector);
        nft.purchaseWithAuthorization(address(0), auth);
    }

    function test_window_notYetValidAuthorizationReverts() public {
        Rub3License.PaymentAuthorization memory auth;
        auth.from        = buyer;
        auth.validAfter  = block.timestamp + 1 hours;
        auth.validBefore = block.timestamp + 2 hours;
        auth.salt        = "salt-1";
        auth.signature   = _sign(
            BUYER_PK, buyer, address(nft), USDC_PRICE, auth.validAfter, auth.validBefore,
            nft.purchaseAuthorizationNonce(buyer, auth.salt)
        );

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.AuthorizationNotYetValid.selector);
        nft.purchaseWithAuthorization(address(0), auth);
    }

    /// A buyer who changes their mind cancels the authorization on the token,
    /// and the pending purchase becomes unspendable.
    function test_window_cancelledAuthorizationCannotBeSpent() public {
        bytes32 nonce = nft.purchaseAuthorizationNonce(buyer, "salt-1");
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        bytes32 digest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                usdc.DOMAIN_SEPARATOR(),
                keccak256(abi.encode(CANCEL_TYPEHASH, buyer, nonce))
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(BUYER_PK, digest);
        usdc.cancelAuthorization(buyer, nonce, abi.encodePacked(r, s, v));

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.AuthorizationUsedOrCanceled.selector);
        nft.purchaseWithAuthorization(address(0), auth);
    }

    /// An authorization is signed for one amount. If the developer reprices
    /// between signature and submission, the buyer is not charged the new
    /// amount - the digest simply stops matching.
    function test_priceMove_afterSigningRejectsRatherThanOvercharging() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");

        vm.prank(owner);
        nft.setTokenPrice(address(usdc), USDC_PRICE * 10);

        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        nft.purchaseWithAuthorization(address(0), auth);

        assertEq(usdc.balanceOf(buyer), 1_000_000_000);
    }

    /// The ETH counterpart of the test above, and the reason both are here:
    /// **both rails fail loudly when the price moves between the read and the
    /// transaction.** The stablecoin rail does it through the signed digest;
    /// the ETH rail does it because {Rub3License-_payEth} requires the exact
    /// listed price. The direction that matters most is the *cut* - before
    /// exact payment it was the one case that went through silently, charging
    /// the buyer a stale price and taxing the excess.
    function test_priceMove_afterReadingRejectsOnTheEthRailToo() public {
        vm.deal(outsider, 100 ether);

        // The agent reads the listed price.
        uint256 read = nft.price();
        assertEq(read, PRICE);

        // The developer cuts it before the agent's transaction lands.
        vm.prank(owner);
        nft.setPrice(PRICE / 10);

        vm.prank(outsider);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, read, PRICE / 10)
        );
        nft.purchase{value: read}(outsider);

        // And the same on a raise, which always reverted but now says so with
        // the same error rather than a different one.
        vm.prank(owner);
        nft.setPrice(PRICE * 10);

        vm.prank(outsider);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, read, PRICE * 10)
        );
        nft.purchase{value: read}(outsider);

        // Nothing was minted and nothing was kept on either attempt.
        assertEq(nft.nextTokenId(), 0);
        assertEq(address(nft).balance, 0);
        assertEq(outsider.balance, 100 ether);

        // Re-reading the price is all it takes to succeed, and the agent is
        // the one who pays it. The read has to happen before the prank: the
        // `{value: ...}` expression is evaluated first, so an inline `price()`
        // would consume the one-shot prank and leave this test paying from the
        // test contract instead of from `outsider`.
        uint256 fresh = nft.price();
        assertEq(fresh, PRICE * 10);

        vm.prank(outsider);
        uint256 tokenId = nft.purchase{value: fresh}(outsider);

        assertEq(nft.ownerOf(tokenId),  outsider);
        assertEq(outsider.balance,      100 ether - fresh, "the agent paid it");
        assertEq(address(nft).balance,  fresh);
    }

    /// The independent check on top of the token's own accounting: the mint
    /// happens only if the money actually arrived.
    function test_silentToken_mintDoesNotHappenWithoutFunds() public {
        SilentEIP3009Token silent = new SilentEIP3009Token();
        Rub3Access quiet = _deployAccess(_sale(PRICE, address(silent), USDC_PRICE));

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, quiet, buyer, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.InsufficientPayment.selector, 0, USDC_PRICE)
        );
        quiet.purchaseWithAuthorization(address(0), auth);

        assertEq(quiet.nextTokenId(), 0);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 3. Configuration: advertising the rail, and taking it down
    // ══════════════════════════════════════════════════════════════════════════

    /// How the rail is advertised, and how the wrapper detects it: two reads.
    function test_advertisement_isReadableOnChain() public view {
        assertEq(nft.priceToken(),  address(usdc));
        assertEq(nft.priceAmount(), USDC_PRICE);
        assertEq(nft.price(),       PRICE, "the ETH rail is still listed too");
    }

    function test_advertisement_ethOnlyContractRejectsTheTokenPath() public {
        Rub3Access ethOnly = _deployAccess(_sale(PRICE, address(0), 0));
        assertEq(ethOnly.priceToken(), address(0), "advertises no token rail");

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, ethOnly, buyer, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        vm.expectRevert(Rub3License.TokenPaymentUnavailable.selector);
        ethOnly.purchaseWithAuthorization(address(0), auth);
    }

    function test_config_priceTokenMustAnswerTheEip3009ReadSlice() public {
        NotAToken junk = new NotAToken();
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncompatiblePriceToken.selector, address(junk))
        );
        _deployAccess(_sale(PRICE, address(junk), USDC_PRICE));
    }

    /// The narrowing this rail accepts, stated as a test. A token implementing
    /// only EIP-3009's `(v, r, s)` form is conforming and passes the
    /// constructor probe - the probe reads `authorizationState`, which it has -
    /// but the licence contract calls the `bytes signature` overload, which it
    /// does not, so the rail cannot be spent. The contract cannot detect this
    /// at deploy time, which is why the wrapper pre-flights the call before
    /// broadcasting and buys in ETH instead.
    ///
    /// The missing overload has to be the *only* difference, or this proves
    /// nothing: the authorization is signed against the split token's own
    /// EIP-712 domain, the revert carries no data (there is no such function
    /// and no fallback, rather than a rejected signature), the very same signed
    /// fields are then spent successfully through the form the token *does*
    /// implement, and the identical authorization shape mints against
    /// {MockEIP3009Token}. Give the fixture the overload and this test fails.
    function test_config_tokenWithoutTheSignatureOverloadDeploysButCannotBeSpent() public {
        NoSignatureOverloadEIP3009Token split = new NoSignatureOverloadEIP3009Token();

        Rub3Access strict = _deployAccess(_sale(PRICE, address(split), USDC_PRICE));
        assertEq(strict.priceToken(), address(split), "the constructor probe accepts it");

        split.mint(buyer, 1_000_000_000);

        uint256 validBefore = block.timestamp + 1 hours;
        bytes32 nonce = strict.purchaseAuthorizationNonce(buyer, "salt-1");
        bytes memory signature = _signFor(
            split.DOMAIN_SEPARATOR(),
            BUYER_PK,
            buyer,
            address(strict),
            USDC_PRICE,
            0,
            validBefore,
            nonce
        );
        Rub3License.PaymentAuthorization memory auth = Rub3License.PaymentAuthorization({
            from: buyer,
            validAfter: 0,
            validBefore: validBefore,
            salt: "salt-1",
            signature: signature
        });

        // Empty revert data: the call found no such function, rather than a
        // signature check that said no.
        vm.prank(submitter);
        vm.expectRevert(bytes(""));
        strict.purchaseWithAuthorization(address(0), auth);

        assertEq(strict.nextTokenId(), 0, "nothing was minted");
        assertEq(split.balanceOf(buyer), 1_000_000_000, "and nothing moved");

        // The signature was good all along: the token spends these exact fields
        // through the form it implements, so only the overload was missing.
        (uint8 v, bytes32 r, bytes32 s) = _split(signature);
        vm.prank(address(strict));
        split.receiveWithAuthorization(
            buyer, address(strict), USDC_PRICE, 0, validBefore, nonce, v, r, s
        );
        assertEq(
            split.balanceOf(address(strict)),
            USDC_PRICE,
            "the authorization the licence contract could not spend was valid"
        );

        // And the identical authorization shape mints against a token that has
        // the overload.
        Rub3License.PaymentAuthorization memory sameShape =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");
        vm.prank(submitter);
        assertEq(
            nft.ownerOf(nft.purchaseWithAuthorization(address(0), sameShape)),
            buyer,
            "the same shape of authorization buys against a FiatTokenV2_2-style token"
        );

        // The licence contract itself is fine: the ETH rail sells as always.
        vm.deal(outsider, 1 ether);
        vm.prank(outsider);
        assertEq(strict.ownerOf(strict.purchase{value: PRICE}(outsider)), outsider);
    }

    function test_config_priceTokenMustHaveCode() public {
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncompatiblePriceToken.selector, attacker)
        );
        _deployAccess(_sale(PRICE, attacker, USDC_PRICE));
    }

    function test_config_amountWithoutATokenIsRejected() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                Rub3License.TokenPriceInconsistent.selector, address(0), USDC_PRICE
            )
        );
        _deployAccess(_sale(PRICE, address(0), USDC_PRICE));
    }

    function test_config_setTokenPriceIsOwnerOnly() public {
        vm.prank(attacker);
        vm.expectRevert();
        nft.setTokenPrice(address(usdc), 1);
    }

    function test_config_ownerCanWithdrawTheRail() public {
        vm.prank(owner);
        nft.setTokenPrice(address(0), 0);

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");
        vm.prank(submitter);
        vm.expectRevert(Rub3License.TokenPaymentUnavailable.selector);
        nft.purchaseWithAuthorization(address(0), auth);
    }

    function test_withdrawToken_ownerSweepsTheStablecoinBalance() public {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, nft, buyer, USDC_PRICE, "salt-1");
        vm.prank(submitter);
        nft.purchaseWithAuthorization(address(0), auth);

        vm.prank(owner);
        nft.withdrawToken(address(usdc), owner);

        assertEq(usdc.balanceOf(owner), USDC_PRICE);
        assertEq(usdc.balanceOf(address(nft)), 0);
    }

    function test_withdrawToken_isOwnerOnly() public {
        vm.prank(attacker);
        vm.expectRevert();
        nft.withdrawToken(address(usdc), attacker);
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 4. Subscriptions: two frozen rails per token
    // ══════════════════════════════════════════════════════════════════════════

    function _buySubscriptionWithUsdc(bytes32 salt) internal returns (uint256 tokenId) {
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, sub, buyer, USDC_PRICE, salt);
        vm.prank(submitter);
        tokenId = sub.purchaseWithAuthorization(address(0), auth);
    }

    function test_subscription_purchaseSnapshotsBothRails() public {
        uint256 tokenId = _buySubscriptionWithUsdc("salt-1");

        assertEq(sub.ownerOf(tokenId), buyer);
        assertEq(sub.expiresAt(tokenId), block.timestamp + PERIOD);
        assertEq(sub.renewPrice(tokenId),       PRICE,           "ETH rail frozen");
        assertEq(sub.renewPriceToken(tokenId),  address(usdc),   "token rail frozen");
        assertEq(sub.renewPriceAmount(tokenId), USDC_PRICE,      "token amount frozen");
        assertEq(buyer.balance, 0);
    }

    function test_subscription_renewsOnTheStablecoinRail() public {
        uint256 tokenId = _buySubscriptionWithUsdc("salt-1");
        uint256 before  = sub.expiresAt(tokenId);

        Rub3License.PaymentAuthorization memory auth =
            _renewAuth(BUYER_PK, sub, tokenId, USDC_PRICE, "salt-2");

        vm.expectEmit(true, false, false, true);
        emit Rub3Subscription.Renewed(tokenId, before + PERIOD, address(usdc), USDC_PRICE);
        vm.prank(submitter);
        sub.renewWithAuthorization(tokenId, auth);

        assertEq(sub.expiresAt(tokenId), before + PERIOD);
        assertEq(buyer.balance, 0, "renewed without ever holding ETH");
        assertEq(usdc.balanceOf(address(sub)), USDC_PRICE * 2);
    }

    /// The renewal snapshot is the whole point of §2.4, and it now covers both
    /// rails: repricing reaches new buyers only.
    function test_subscription_ownerCannotRepriceAHeldTokenOnEitherRail() public {
        uint256 tokenId = _buySubscriptionWithUsdc("salt-1");

        vm.prank(owner);
        sub.setTokenPrice(address(usdc), USDC_PRICE * 100);

        // Renewal still charges the snapshot: an authorization for the *new*
        // amount is not what the contract asks the token for, so it fails.
        Rub3License.PaymentAuthorization memory tooMuch =
            _renewAuth(BUYER_PK, sub, tokenId, USDC_PRICE * 100, "salt-2");
        vm.prank(submitter);
        vm.expectRevert(MockEIP3009Token.InvalidSignature.selector);
        sub.renewWithAuthorization(tokenId, tooMuch);

        // The frozen amount is what goes through.
        Rub3License.PaymentAuthorization memory snapshot =
            _renewAuth(BUYER_PK, sub, tokenId, USDC_PRICE, "salt-3");
        vm.prank(submitter);
        sub.renewWithAuthorization(tokenId, snapshot);

        assertEq(usdc.balanceOf(address(sub)), USDC_PRICE * 2);
    }

    /// A token minted before the contract offered a stablecoin rail has no
    /// stablecoin renewal - and does not need one, because the ETH rail is
    /// always there.
    function test_subscription_tokenMintedBeforeTheRailRenewsInEthOnly() public {
        Rub3Subscription ethOnly = _deploySubscription(_sale(PRICE, address(0), 0));

        vm.deal(outsider, 10 ether);
        vm.prank(outsider);
        uint256 tokenId = ethOnly.purchase{value: PRICE}(outsider);

        vm.prank(owner);
        ethOnly.setTokenPrice(address(usdc), USDC_PRICE);

        assertEq(ethOnly.renewPriceToken(tokenId), address(0), "no rail was snapshotted");

        Rub3License.PaymentAuthorization memory auth =
            _renewAuth(OUTSIDER_PK, ethOnly, tokenId, USDC_PRICE, "salt-1");
        vm.prank(submitter);
        vm.expectRevert(Rub3License.TokenPaymentUnavailable.selector);
        ethOnly.renewWithAuthorization(tokenId, auth);

        // ETH renewal is unaffected.
        vm.prank(outsider);
        ethOnly.renew{value: PRICE}(tokenId);
        assertEq(ethOnly.expiresAt(tokenId), block.timestamp + PERIOD * 2);
    }

    /// A new buyer gets the new rate; the held token does not. Both frozen
    /// rails are independent per token.
    function test_subscription_snapshotsAreIndependentAcrossAPriceChange() public {
        uint256 first = _buySubscriptionWithUsdc("salt-1");

        vm.prank(owner);
        sub.setTokenPrice(address(usdc), USDC_PRICE * 2);

        // The second buyer signs for the *new* listed amount, which is what the
        // contract now asks the token for.
        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, sub, buyer, USDC_PRICE * 2, "salt-2");
        vm.prank(submitter);
        uint256 second = sub.purchaseWithAuthorization(address(0), auth);

        assertEq(sub.renewPriceAmount(first),  USDC_PRICE);
        assertEq(sub.renewPriceAmount(second), USDC_PRICE * 2);
    }

    function test_subscription_ethAndTokenRailsMintIdentically() public {
        vm.deal(outsider, 10 ether);
        vm.prank(outsider);
        uint256 ethId = sub.purchase{value: PRICE}(outsider);
        uint256 usdcId = _buySubscriptionWithUsdc("salt-1");

        assertEq(sub.expiresAt(ethId),         sub.expiresAt(usdcId));
        assertEq(sub.renewPrice(ethId),        sub.renewPrice(usdcId));
        assertEq(sub.renewPriceToken(ethId),   sub.renewPriceToken(usdcId));
        assertEq(sub.renewPriceAmount(ethId),  sub.renewPriceAmount(usdcId));
    }

    function test_subscription_renewalOfAnUnknownTokenReverts() public {
        Rub3License.PaymentAuthorization memory auth =
            _renewAuth(BUYER_PK, sub, 42, USDC_PRICE, "salt-1");

        vm.prank(submitter);
        vm.expectRevert();
        sub.renewWithAuthorization(42, auth);
    }

    /// §2.4's mint-ordering guarantee, re-checked on the new rail: a contract
    /// recipient reading its own token from inside `onERC721Received` finds both
    /// frozen rails already written. The payment happens first, so by the time
    /// the callback runs the money is in and the terms are set.
    function test_subscription_recipientCallbackSeesFrozenTermsOnTheTokenRail() public {
        AuthMintProbe probe = new AuthMintProbe(sub);

        Rub3License.PaymentAuthorization memory auth =
            _purchaseAuth(BUYER_PK, sub, address(probe), USDC_PRICE, "salt-1");
        vm.prank(submitter);
        uint256 tokenId = sub.purchaseWithAuthorization(address(probe), auth);

        assertTrue(probe.fired(), "the recipient callback must have run");
        assertEq(probe.seenRenewPrice(),       PRICE);
        assertEq(probe.seenRenewPriceToken(),  address(usdc));
        assertEq(probe.seenRenewPriceAmount(), USDC_PRICE);
        assertEq(sub.ownerOf(tokenId), address(probe));
    }

    // ══════════════════════════════════════════════════════════════════════════
    // 5. The ETH rail is untouched
    // ══════════════════════════════════════════════════════════════════════════

    /// Adding a rail does not make the old one second-class: a contract that
    /// advertises USDC still sells for ETH at the listed price.
    function test_ethRail_stillWorksOnAContractAdvertisingUsdc() public {
        vm.deal(outsider, 10 ether);

        vm.prank(outsider);
        uint256 tokenId = nft.purchase{value: PRICE}(outsider);
        assertEq(nft.ownerOf(tokenId), outsider);

        vm.prank(outsider);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, PRICE - 1, PRICE)
        );
        nft.purchase{value: PRICE - 1}(outsider);

        vm.prank(outsider);
        vm.expectRevert(
            abi.encodeWithSelector(Rub3License.IncorrectPayment.selector, PRICE + 1, PRICE)
        );
        nft.purchase{value: PRICE + 1}(outsider);
    }
}

/// @notice Reads its own subscription's terms from inside `onERC721Received`,
///         so a test can prove they were frozen before the token existed for
///         anyone else to see.
contract AuthMintProbe {
    Rub3Subscription public sub;

    bool    public fired;
    uint256 public seenRenewPrice;
    address public seenRenewPriceToken;
    uint256 public seenRenewPriceAmount;

    constructor(Rub3Subscription sub_) {
        sub = sub_;
    }

    function onERC721Received(address, address, uint256 tokenId, bytes calldata)
        external
        returns (bytes4)
    {
        fired                = true;
        seenRenewPrice       = sub.renewPrice(tokenId);
        seenRenewPriceToken  = sub.renewPriceToken(tokenId);
        seenRenewPriceAmount = sub.renewPriceAmount(tokenId);
        return this.onERC721Received.selector;
    }
}
