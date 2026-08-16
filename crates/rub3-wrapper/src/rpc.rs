use alloy::primitives::{b256, Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use alloy::sol_types::SolCall;

// ── Contract interface ────────────────────────────────────────────────────────

// Minimal ABI surface needed for activation + session flow (tiers 2-3):
//   ownerOf(tokenId)              — ERC-721 standard
//   price()                       — rub3 license contract
//   balanceOf(owner)              — ERC-721 standard
//   tokenOfOwnerByIndex(...)      — ERC-721Enumerable
//   activate(tokenId)             — tier-3 session activation (returns sessionId)
//   cooldownReady(tokenId)        — tier-3 view helper
//   lastActivationBlock(tokenId)  — tier-3 read
//   cooldownBlocks()              — tier-3 read
//   activeSessionId(tokenId)      — tier-3 revocation check
//   identityModel()               — 0 = access, 1 = account (read at session creation)
//   tbaImplementation()           — ERC-6551 impl for account-model TBA derivation
//   supplyCap()                   — immutable mint cap (0 = unlimited)
//   nextTokenId()                 — next id to be minted
//   purchase(recipient)           - payable; calldata only in interactive mode
//   priceToken() / priceAmount()  - the EIP-3009 stablecoin rail (§2.2), and how
//                                   a contract advertises it: a zero token, or a
//                                   getter that is not there at all, means ETH only
//   purchaseAuthorizationNonce(…) - the nonce a purchase authorization must carry
//   purchaseWithAuthorization(…)  - the stablecoin rail's mint
sol! {
    #[sol(rpc)]
    interface IRub3License {
        function ownerOf(uint256 tokenId) external view returns (address owner);
        function price() external view returns (uint256 amount);
        function balanceOf(address owner) external view returns (uint256 balance);
        function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256 tokenId);

        function activate(uint256 tokenId) external returns (uint256 sessionId);
        function cooldownReady(uint256 tokenId) external view returns (bool ready, uint256 blocksRemaining);
        function lastActivationBlock(uint256 tokenId) external view returns (uint256 blockNumber);
        function cooldownBlocks() external view returns (uint256 blocks);
        function activeSessionId(uint256 tokenId) external view returns (uint256 sessionId);

        function identityModel() external view returns (uint8 model);
        function tbaImplementation() external view returns (address impl);

        function supplyCap() external view returns (uint256 cap);
        function nextTokenId() external view returns (uint256 id);
        function purchase(address recipient) external payable returns (uint256 tokenId);

        function priceToken() external view returns (address token);
        function priceAmount() external view returns (uint256 amount);
        function purchaseAuthorizationNonce(address recipient, bytes32 salt)
            external view returns (bytes32 nonce);

        /// Mirrors `Rub3License.PaymentAuthorization`. Field order is part of
        /// the ABI encoding, so it must match the Solidity struct exactly.
        ///
        /// `signature` is opaque to the licence contract, which hands it
        /// straight to the payment token: 65 bytes of `r || s || v` for an EOA
        /// signer, or an EIP-1271 signature for a smart-contract wallet.
        struct PaymentAuthorization {
            address from;
            uint256 validAfter;
            uint256 validBefore;
            bytes32 salt;
            bytes   signature;
        }

        function purchaseWithAuthorization(address recipient, PaymentAuthorization calldata auth)
            external returns (uint256 tokenId);
    }
}

// The slice of an EIP-3009 payment token the wrapper reads. `DOMAIN_SEPARATOR`
// is read rather than rebuilt from name/version/chainId: it is the one value the
// token itself agrees is its EIP-712 domain, so a signature built against it
// cannot drift from whatever version of USDC is actually deployed.
sol! {
    #[sol(rpc)]
    interface IEip3009Token {
        function balanceOf(address owner) external view returns (uint256 balance);
        function DOMAIN_SEPARATOR() external view returns (bytes32 separator);
    }
}

/// `keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")`
///
/// The receive variant, not the transfer one. Only the payee may submit a
/// `receiveWithAuthorization`, which is what stops a third party spending the
/// buyer's authorization outside the licence contract - see
/// `Rub3License._payWithAuthorization`.
const RECEIVE_WITH_AUTHORIZATION_TYPEHASH: B256 =
    b256!("d099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8");

/// `keccak256("Transfer(address,address,uint256)")` — the ERC-721 Transfer
/// event topic0. Mint events have `from == address(0)`.
const ERC721_TRANSFER_SIG: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

// ── Receipt ───────────────────────────────────────────────────────────────────

/// Minimal tx receipt — the fields the wrapper cares about.
#[derive(Debug, Clone)]
pub struct TxReceipt {
    pub status: bool,
    pub block_number: u64,
    pub block_hash: String,
    /// `to` address from the receipt, lowercased hex. Used by tier-3
    /// on-chain re-verification to confirm the tx hit the license contract.
    pub to: Option<String>,
}

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum RpcError {
    /// URL parse failure or network-level error.
    Transport(String),
    /// Contract call reverted or returned unexpected data.
    Contract(String),
    /// An argument was malformed: the call was never made and never will
    /// succeed as given. Kept apart from [`RpcError::Transport`] so callers
    /// that retry do not retry a request that cannot become valid.
    InvalidInput(String),
    /// ENS resolution is not yet implemented (Phase 1.6).
    EnsNotSupported,
}

impl RpcError {
    /// Whether repeating the identical call could plausibly succeed later.
    ///
    /// Only transport failures qualify: a 502, a rate limit or a dropped
    /// connection says nothing about the request. A reverted call, a malformed
    /// argument and an unimplemented feature are all settled answers.
    pub fn is_retryable(&self) -> bool {
        match self {
            RpcError::Transport(_) => true,
            RpcError::Contract(_) | RpcError::InvalidInput(_) | RpcError::EnsNotSupported => false,
        }
    }

    /// Whether the node failed to answer, as opposed to the chain answering
    /// something the caller did not want.
    ///
    /// Callers that read a contract-level failure as information - the
    /// stablecoin-rail reads, where "this token has no such function" selects
    /// the ETH rail - branch on this so that a node failure never reaches the
    /// same conclusion.
    pub fn is_transport(&self) -> bool {
        matches!(self, RpcError::Transport(_))
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Transport(e) => write!(f, "transport error: {e}"),
            RpcError::Contract(e) => write!(f, "contract error: {e}"),
            RpcError::InvalidInput(e) => write!(f, "invalid argument: {e}"),
            RpcError::EnsNotSupported => {
                write!(f, "ENS resolution not yet supported (planned Phase 1.6)")
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns the address that owns `token_id` on the given ERC-721 contract.
///
/// Calls `ownerOf(uint256)` via JSON-RPC. Returns `RpcError::Contract` if the
/// token does not exist (contract reverts for unminted tokens).
pub fn owner_of(rpc_url: &str, contract: Address, token_id: u64) -> Result<Address, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let result = instance
            .ownerOf(U256::from(token_id))
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(result)
    })
}

/// Returns the purchase price (in wei) from the license contract's `price()`
/// function - the ETH rail.
///
/// Named for the currency, not the licence: the stablecoin rail's price is
/// [`stablecoin_rail`] returning a [`StablecoinPrice`], and the two are
/// denominated in different units on the same money path.
pub fn eth_price(rpc_url: &str, contract: Address) -> Result<U256, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let result = instance
            .price()
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(result)
    })
}

/// Returns all token IDs owned by `owner` on the given ERC-721Enumerable contract.
///
/// Uses `balanceOf` + `tokenOfOwnerByIndex`. Returns `RpcError::Contract` if the
/// contract does not implement ERC-721Enumerable.
pub fn tokens_of_owner(
    rpc_url: &str,
    contract: Address,
    owner: Address,
) -> Result<Vec<u64>, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);

        let balance = instance
            .balanceOf(owner)
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;

        let count = balance.to::<u64>();
        let mut tokens = Vec::with_capacity(count as usize);

        for i in 0..count {
            let token_id = instance
                .tokenOfOwnerByIndex(owner, U256::from(i))
                .call()
                .await
                .map_err(|e| RpcError::Contract(e.to_string()))?;
            tokens.push(token_id.to::<u64>());
        }

        Ok(tokens)
    })
}

/// Resolves an ENS name to an Ethereum address.
///
/// Stub — full implementation in Phase 1.6.
pub fn resolve_ens(_rpc_url: &str, _name: &str) -> Result<Address, RpcError> {
    Err(RpcError::EnsNotSupported)
}

// ── Tier-3: activation / cooldown ─────────────────────────────────────────────

/// Calls `cooldownReady(tokenId)` view; returns `(ready, blocks_remaining)`.
pub fn cooldown_ready(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
) -> Result<(bool, u64), RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .cooldownReady(U256::from(token_id))
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok((r.ready, r.blocksRemaining.to::<u64>()))
    })
}

/// Calls `lastActivationBlock(tokenId)` view.
pub fn last_activation_block(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .lastActivationBlock(U256::from(token_id))
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(r.to::<u64>())
    })
}

/// Calls `cooldownBlocks()` view (returns the contract's configured cooldown).
pub fn cooldown_blocks(rpc_url: &str, contract: Address) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .cooldownBlocks()
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(r.to::<u64>())
    })
}

/// Calls `activeSessionId(tokenId)` view. Used after an `activate()` tx lands
/// to read the authoritative session id the contract assigned.
pub fn active_session_id(rpc_url: &str, contract: Address, token_id: u64) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .activeSessionId(U256::from(token_id))
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(r.to::<u64>())
    })
}

/// Returns the 0x-prefixed ABI-encoded calldata for `activate(tokenId)`.
///
/// Pure — no RPC. The wrapper shows this to the user so they can paste it
/// into their wallet to send the tx themselves.
pub fn encode_activate_calldata(token_id: u64) -> String {
    let call = IRub3License::activateCall {
        tokenId: U256::from(token_id),
    };
    format!("0x{}", hex::encode(call.abi_encode()))
}

/// Fetches the receipt for `tx_hash`. Returns `Ok(None)` while the tx is still
/// pending; `Ok(Some(receipt))` once mined.
pub fn get_tx_receipt(rpc_url: &str, tx_hash: &str) -> Result<Option<TxReceipt>, RpcError> {
    let hash: B256 = tx_hash
        .trim_start_matches("0x")
        .parse::<B256>()
        .map_err(|e| RpcError::InvalidInput(format!("invalid tx hash: {e}")))?;

    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let maybe = provider
            .get_transaction_receipt(hash)
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?;

        let receipt = match maybe {
            Some(r) => r,
            None => return Ok(None),
        };

        let block_hash = receipt
            .block_hash
            .map(|h| format!("0x{}", hex::encode(h.as_slice())))
            .unwrap_or_default();
        let block_number = receipt.block_number.unwrap_or_default();
        let to = receipt
            .to
            .map(|a| format!("0x{}", hex::encode(a.as_slice())));

        Ok(Some(TxReceipt {
            status: receipt.status(),
            block_number,
            block_hash,
            to,
        }))
    })
}

/// Tx receipt polling budget - attempts × interval = total wait.
///
/// 30s covers Base's ~2s soft finality with a wide margin; it is also the
/// budget the interactive flow has always used, kept identical here so the
/// human and agent front doors behave the same when a chain is congested.
#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
pub const RECEIPT_POLL_ATTEMPTS: u32 = 10;
#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
pub const RECEIPT_POLL_INTERVAL_SECS: u64 = 3;

/// Why [`wait_for_receipt`] gave up.
///
/// Both cases mean the same thing to a caller that just spent money: the
/// transaction may still be mined and cannot be assumed dead. They are kept
/// apart only so the failure can be reported honestly - a timeout says the
/// node answered and the tx was simply not mined yet, a transport failure says
/// the node stopped answering at all, and neither is a reason to re-broadcast.
#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
#[derive(Debug)]
pub enum ReceiptWaitError {
    /// The budget ran out with the transaction still unmined.
    Timeout { after_secs: u64 },
    /// The receipt query failed: either it kept failing until the budget ran
    /// out, or it failed in a way no retry could fix.
    Transport { after_secs: u64, message: String },
}

// `after_secs` on both variants is wall-clock time actually spent waiting, not
// the nominal budget: it is what an operator reads off the exit-21 detail line
// to reconstruct how long a committed purchase went unresolved, and a dead
// endpoint that blocks on every call can burn far more than the budget.

#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
impl ReceiptWaitError {
    /// Wall-clock seconds actually spent waiting before giving up.
    ///
    /// Measured, not derived from `attempts * interval`: the loop sleeps only
    /// between attempts, and a slow endpoint can make each attempt cost far
    /// more than the interval.
    pub fn after_secs(&self) -> u64 {
        match self {
            ReceiptWaitError::Timeout { after_secs }
            | ReceiptWaitError::Transport { after_secs, .. } => *after_secs,
        }
    }

    /// The transport failure that ended the wait, or `None` when the node kept
    /// answering and the transaction simply had not been mined.
    pub fn transport_message(&self) -> Option<&str> {
        match self {
            ReceiptWaitError::Timeout { .. } => None,
            ReceiptWaitError::Transport { message, .. } => Some(message),
        }
    }
}

#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
impl std::fmt::Display for ReceiptWaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiptWaitError::Timeout { after_secs } => {
                write!(f, "tx not confirmed within {after_secs}s")
            }
            ReceiptWaitError::Transport {
                after_secs,
                message,
            } => {
                write!(f, "receipt query failed after {after_secs}s: {message}")
            }
        }
    }
}

#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
impl std::error::Error for ReceiptWaitError {}

/// Polls `get_tx_receipt` until the transaction is mined or the budget runs out.
///
/// Shared by both front doors: the webview poller thread and the headless
/// flow call this rather than each keeping their own loop.
///
/// A *transient* failed poll never ends the wait early. Once a transaction is
/// broadcast the only thing that resolves it is a receipt, so a 502, a rate
/// limit or a dropped connection is retried inside the same budget, and is
/// reported only when the budget is exhausted with the last poll still
/// failing. A poll that fails because the request itself is malformed is
/// reported at once: waiting out the budget would not make it parse.
#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
pub fn wait_for_receipt(rpc_url: &str, tx_hash: &str) -> Result<TxReceipt, ReceiptWaitError> {
    poll_for_receipt(
        || get_tx_receipt(rpc_url, tx_hash),
        RECEIPT_POLL_ATTEMPTS,
        std::time::Duration::from_secs(RECEIPT_POLL_INTERVAL_SECS),
    )
}

/// The polling loop behind [`wait_for_receipt`], with the query and the clock
/// passed in so the retry behaviour can be exercised without a node.
#[cfg(any(feature = "onchain-write", feature = "cooldown"))]
fn poll_for_receipt<F>(
    mut poll: F,
    attempts: u32,
    interval: std::time::Duration,
) -> Result<TxReceipt, ReceiptWaitError>
where
    F: FnMut() -> Result<Option<TxReceipt>, RpcError>,
{
    let started = std::time::Instant::now();
    let mut last_transport_error: Option<String> = None;
    for attempt in 0..attempts {
        match poll() {
            Ok(Some(r)) => return Ok(r),
            // The node answered, so whatever failed earlier was transient and
            // the transaction is merely unmined so far.
            Ok(None) => last_transport_error = None,
            Err(e) if !e.is_retryable() => {
                return Err(ReceiptWaitError::Transport {
                    after_secs: started.elapsed().as_secs(),
                    message: e.to_string(),
                })
            }
            Err(e) => last_transport_error = Some(e.to_string()),
        }
        if attempt + 1 < attempts {
            std::thread::sleep(interval);
        }
    }
    let after_secs = started.elapsed().as_secs();
    match last_transport_error {
        Some(message) => Err(ReceiptWaitError::Transport {
            after_secs,
            message,
        }),
        None => Err(ReceiptWaitError::Timeout { after_secs }),
    }
}

// ── Identity model ────────────────────────────────────────────────────────────

/// Reads the contract's `identityModel()` getter. Returns the raw `uint8`:
/// `0` = access (user_id = wallet), `1` = account (user_id = TBA).
pub fn identity_model(rpc_url: &str, contract: Address) -> Result<u8, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .identityModel()
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(r)
    })
}

/// Reads the contract's `tbaImplementation()` getter — the ERC-6551 account
/// implementation address used to derive token-bound account addresses.
///
/// Returns `Address::ZERO` for access-model deploys (enforced on-chain).
pub fn tba_implementation(rpc_url: &str, contract: Address) -> Result<Address, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .tbaImplementation()
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(r)
    })
}

/// Returns the chain id the RPC endpoint is serving.
///
/// Headless activation compares this against the chain id the binary was
/// packed for, before it signs anything: a wrapper pointed at the wrong
/// endpoint would otherwise produce a perfectly valid transaction for the
/// wrong network.
pub fn chain_id(rpc_url: &str) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        provider
            .get_chain_id()
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))
    })
}

/// Returns the runtime code deployed at `contract`.
///
/// One `eth_getCode`. The pre-purchase attestation in [`crate::attest`] runs
/// entirely on the bytes this returns, so a whole verification costs a single
/// round trip. An address holding no contract returns an empty vector rather
/// than an error: "nothing is deployed here" is an answer, and the caller is
/// the one that decides what it means.
pub fn get_code(rpc_url: &str, contract: Address) -> Result<Vec<u8>, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let code = provider
            .get_code_at(contract)
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?;
        Ok(code.to_vec())
    })
}

/// Returns the current block number on the target chain.
pub fn get_block_number(rpc_url: &str) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        provider
            .get_block_number()
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))
    })
}

// ── Purchase / supply (tier 3) ────────────────────────────────────────────────

/// Reads `supplyCap()`. `0` means unlimited.
pub fn supply_cap(rpc_url: &str, contract: Address) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .supplyCap()
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(r.to::<u64>())
    })
}

/// Reads `nextTokenId()` — the id the next `purchase()` will mint.
pub fn next_token_id(rpc_url: &str, contract: Address) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .nextTokenId()
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))?;
        Ok(r.to::<u64>())
    })
}

/// Returns the 0x-prefixed ABI-encoded calldata for `purchase(recipient)`.
///
/// Pure — no RPC. The wrapper shows this to the user so they can paste it
/// into their wallet to send the tx themselves. `msg.value` is handled
/// separately in the UI.
pub fn encode_purchase_calldata(recipient: Address) -> String {
    let call = IRub3License::purchaseCall { recipient };
    format!("0x{}", hex::encode(call.abi_encode()))
}

// ── Stablecoin rail (EIP-3009, §2.2) ──────────────────────────────────────────

/// What a contract charges on its stablecoin rail.
///
/// Never wei: `amount` is denominated in `token`'s own smallest unit. The ETH
/// rail's price is a bare `U256` of wei from [`eth_price`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StablecoinPrice {
    /// The ERC-20 the contract accepts. Never the zero address - a contract
    /// with no rail is reported as `None` rather than a zero token.
    pub token: Address,
    /// The price in that token's own smallest unit.
    pub amount: U256,
}

/// Reads the stablecoin rail a contract advertises, or `None` when it has none.
///
/// This is the whole detection mechanism, and it is deliberately one ordinary
/// `eth_call`: `priceToken()`. Three answers mean "ETH only" and are treated
/// identically -
///
///   * the zero address, from a contract that could offer a rail but does not;
///   * a revert, from a contract deployed before §2.2, which has no such
///     function and no fallback to swallow the call;
///   * empty return data, from an address that is not a licence contract.
///
/// A *transport* failure is none of those and is propagated: falling back to
/// ETH because the node blinked would spend the wrong currency for the wrong
/// reason.
pub fn stablecoin_rail(
    rpc_url: &str,
    contract: Address,
) -> Result<Option<StablecoinPrice>, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);

        let token = match instance.priceToken().call().await {
            Ok(token) => token,
            Err(e) => match classify_call_error(&e) {
                transport @ RpcError::Transport(_) => return Err(transport),
                _ => return Ok(None),
            },
        };
        if token.is_zero() {
            return Ok(None);
        }

        let amount = instance
            .priceAmount()
            .call()
            .await
            .map_err(|e| classify_call_error(&e))?;

        Ok(Some(StablecoinPrice { token, amount }))
    })
}

/// Reads an ERC-20 balance. Used to check the buyer can actually cover the
/// stablecoin price before choosing that rail.
pub fn erc20_balance_of(rpc_url: &str, token: Address, owner: Address) -> Result<U256, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IEip3009Token::new(token, provider);
        instance
            .balanceOf(owner)
            .call()
            .await
            .map_err(|e| classify_call_error(&e))
    })
}

/// Reads a payment token's EIP-712 `DOMAIN_SEPARATOR()`.
pub fn token_domain_separator(rpc_url: &str, token: Address) -> Result<B256, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IEip3009Token::new(token, provider);
        instance
            .DOMAIN_SEPARATOR()
            .call()
            .await
            .map_err(|e| classify_call_error(&e))
    })
}

/// Reads the nonce a purchase authorization must carry, from the contract that
/// will check it.
///
/// Derived on-chain rather than recomputed here on purpose: the nonce is what
/// binds the buyer's signature to the mint recipient, and a wrapper that
/// derived it independently could drift from the contract and produce
/// signatures that are silently unspendable.
pub fn purchase_authorization_nonce(
    rpc_url: &str,
    contract: Address,
    recipient: Address,
    salt: B256,
) -> Result<B256, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        instance
            .purchaseAuthorizationNonce(recipient, salt)
            .call()
            .await
            .map_err(|e| RpcError::Contract(e.to_string()))
    })
}

/// The EIP-712 digest a buyer signs to authorize `value` of a payment token to
/// `payee`. Pure - no RPC.
///
/// `domain_separator` comes from the token itself
/// ([`token_domain_separator`]), so this function never has to know the
/// token's name or version.
#[allow(clippy::too_many_arguments)]
pub fn receive_authorization_digest(
    domain_separator: B256,
    from: Address,
    payee: Address,
    value: U256,
    valid_after: U256,
    valid_before: U256,
    nonce: B256,
) -> B256 {
    use alloy::sol_types::SolValue;

    let struct_hash = alloy::primitives::keccak256(
        (
            RECEIVE_WITH_AUTHORIZATION_TYPEHASH,
            from,
            payee,
            value,
            valid_after,
            valid_before,
            nonce,
        )
            .abi_encode(),
    );

    let mut preimage = Vec::with_capacity(66);
    preimage.extend_from_slice(&[0x19, 0x01]);
    preimage.extend_from_slice(domain_separator.as_slice());
    preimage.extend_from_slice(struct_hash.as_slice());
    alloy::primitives::keccak256(preimage)
}

/// Returns the 0x-prefixed calldata for
/// `purchaseWithAuthorization(recipient, auth)`.
pub fn encode_purchase_with_authorization_calldata(
    recipient: Address,
    auth: IRub3License::PaymentAuthorization,
) -> String {
    let call = IRub3License::purchaseWithAuthorizationCall { recipient, auth };
    format!("0x{}", hex::encode(call.abi_encode()))
}

/// Runs `purchaseWithAuthorization(recipient, auth)` as an `eth_call` from
/// `buyer`, broadcasting nothing.
///
/// This is how the wrapper finds out, before spending gas, whether the
/// contract's configured payment token implements the `bytes signature`
/// overload of `receiveWithAuthorization` that the licence contract calls. A
/// token that implements only EIP-3009's `(v, r, s)` form is conforming and
/// passes the licence contract's constructor probe, but reverts here.
///
/// It has to be this rather than something cheaper. The contract cannot check
/// it at deploy time, because a staticcall probe cannot tell "no such function"
/// from "bad signature" - both revert. Scanning the token's runtime bytecode
/// for the overload's selector cannot do it either: USDC sits behind a proxy,
/// so its own code carries none of its selectors, and the scan would report a
/// false negative on the very token this rail targets. Executing the call the
/// wrapper is about to send is the one check that answers the question for
/// whatever is really deployed at that address.
///
/// The calldata is built by
/// [`encode_purchase_with_authorization_calldata`], the same encoder that
/// builds the broadcast one, and the sender is the account that would broadcast
/// it, so success here means the same call would succeed against the same
/// state. The authorization passed here carries a shorter `validBefore` than
/// the one that will be broadcast, and nothing this establishes turns on that
/// field: the token compares it against `block.timestamp`, both copies are in
/// date when they execute, and each signature is checked by the same code
/// path. A contract-level failure is a settled answer
/// about the token; a transport failure is not, and is propagated.
pub fn preflight_purchase_with_authorization(
    rpc_url: &str,
    contract: Address,
    buyer: Address,
    recipient: Address,
    auth: IRub3License::PaymentAuthorization,
) -> Result<(), RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        instance
            .purchaseWithAuthorization(recipient, auth)
            .from(buyer)
            .call()
            .await
            .map(|_| ())
            .map_err(|e| classify_call_error(&e))
    })
}

/// Splits a failed `eth_call` into "the chain answered" and "the node did not".
///
/// Every stablecoin-rail read turns on this distinction: a chain answer is a
/// settled fact about the contract and lets the caller fall back to ETH, while
/// a node failure must stop the run rather than silently change the currency.
/// Getting it wrong in the permissive direction is the dangerous one, so only
/// two shapes count as answers:
///
///   * a decode failure - `ZeroData` (the address returned `0x`, so it has no
///     such function), an unknown function or selector, or return data the ABI
///     cannot decode;
///   * a JSON-RPC error body that says the call *reverted*, which nodes report
///     as code `3`, as revert data, or as a message naming the revert.
///
/// Everything else a node can put in an error body - a rate limit, an execution
/// timeout, a missing trie node - describes the node's own state and says
/// nothing about the contract. Those are transport, as are a truncated
/// response, a null response, and a dead socket.
fn classify_call_error(e: &alloy::contract::Error) -> RpcError {
    use alloy::contract::Error as ContractError;
    use alloy::transports::RpcError as JsonRpcError;

    match e {
        ContractError::ZeroData(..)
        | ContractError::AbiError(_)
        | ContractError::UnknownFunction(_)
        | ContractError::UnknownSelector(_) => RpcError::Contract(e.to_string()),
        ContractError::TransportError(JsonRpcError::ErrorResp(payload)) => {
            // Geth and reth answer a reverted `eth_call` with code 3; anvil and
            // several hosted nodes use -32000 and say so in the message. Revert
            // data present at all is conclusive on its own.
            let reverted = payload.code == 3
                || payload.as_revert_data().is_some()
                || payload.message.to_ascii_lowercase().contains("revert");
            if reverted {
                RpcError::Contract(e.to_string())
            } else {
                RpcError::Transport(e.to_string())
            }
        }
        _ => RpcError::Transport(e.to_string()),
    }
}

/// Fetches the receipt for `tx_hash` and returns the token id minted to
/// `recipient` by the matching ERC-721 `Transfer(0x0, recipient, tokenId)` log
/// emitted from `contract`.
///
/// Used after a `purchase()` tx lands to discover the id the contract assigned.
pub fn mint_token_id(
    rpc_url: &str,
    tx_hash: &str,
    contract: Address,
    recipient: Address,
) -> Result<u64, RpcError> {
    let hash: B256 = tx_hash
        .trim_start_matches("0x")
        .parse::<B256>()
        .map_err(|e| RpcError::InvalidInput(format!("invalid tx hash: {e}")))?;

    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let receipt = provider
            .get_transaction_receipt(hash)
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?
            .ok_or_else(|| RpcError::Contract("receipt not found".into()))?;

        for log in receipt.inner.logs() {
            if log.address() != contract {
                continue;
            }
            let topics = log.topics();
            if topics.len() != 4 || topics[0] != ERC721_TRANSFER_SIG {
                continue;
            }
            // Mint: `from == address(0)`. Topic is the 32-byte-padded address.
            if !topics[1].is_zero() {
                continue;
            }
            // `to` must equal recipient (compare the 20 low bytes of the topic).
            let to_bytes: &[u8] = &topics[2].as_slice()[12..];
            if to_bytes != recipient.as_slice() {
                continue;
            }
            let token_id = U256::from_be_bytes::<32>(topics[3].0);
            return Ok(token_id.to::<u64>());
        }

        Err(RpcError::Contract(
            "no ERC-721 mint Transfer log for recipient found in receipt".into(),
        ))
    })
}

// ── Internals ─────────────────────────────────────────────────────────────────

fn build_provider(rpc_url: &str) -> Result<impl alloy::providers::Provider, RpcError> {
    let url: url::Url = rpc_url
        .parse()
        .map_err(|e: url::ParseError| RpcError::Transport(e.to_string()))?;
    Ok(ProviderBuilder::new().connect_http(url))
}

/// Runs a future to completion on a single-threaded tokio runtime.
///
/// Isolated here so the rest of the wrapper stays synchronous.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime init failed")
        .block_on(f)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_RPC: &str = "https://mainnet.base.org";
    // A well-known contract on Base mainnet (verified, non-zero supply).
    // Used only to confirm the RPC path reaches the network in integration tests.
    const SAMPLE_CONTRACT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";

    #[test]
    fn resolve_ens_returns_not_supported() {
        let err = resolve_ens(VALID_RPC, "myapp.eth").unwrap_err();
        assert!(matches!(err, RpcError::EnsNotSupported));
    }

    #[test]
    fn owner_of_invalid_url_returns_transport_error() {
        let err = owner_of("not-a-url", Address::ZERO, 1).unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn eth_price_invalid_url_returns_transport_error() {
        let err = eth_price("not-a-url", Address::ZERO).unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    /// Verifies that a non-existent token_id produces a Contract error (revert),
    /// not a Transport error. Requires network access — skipped in offline CI.
    #[test]
    #[ignore = "requires network"]
    fn owner_of_unminted_token_returns_contract_error() {
        let contract: Address = SAMPLE_CONTRACT.parse().unwrap();
        let err = owner_of(VALID_RPC, contract, u64::MAX).unwrap_err();
        assert!(matches!(err, RpcError::Contract(_)));
    }

    #[test]
    fn encode_activate_calldata_matches_selector() {
        // keccak256("activate(uint256)")[..4] = 0xb260c42a
        let data = encode_activate_calldata(42);
        assert!(data.starts_with("0xb260c42a"), "got {data}");
        // selector (4) + 32-byte argument = 36 bytes = 72 hex chars, plus "0x" prefix.
        assert_eq!(data.len(), 2 + 72);
        // Last 64 chars encode tokenId = 42 = 0x2a, left-padded.
        assert!(data.ends_with("000000000000000000000000000000000000000000000000000000000000002a"));
    }

    #[test]
    fn encode_activate_calldata_differs_by_token_id() {
        let a = encode_activate_calldata(1);
        let b = encode_activate_calldata(2);
        assert_ne!(a, b);
    }

    /// A hash that cannot parse is a settled answer, not a network hiccup:
    /// classified as retryable it would make every poller wait out its budget.
    #[test]
    fn get_tx_receipt_invalid_hash_is_not_retryable() {
        let err = get_tx_receipt(VALID_RPC, "not-a-hash").unwrap_err();
        assert!(matches!(err, RpcError::InvalidInput(_)), "{err}");
        assert!(!err.is_retryable(), "{err}");
    }

    #[test]
    fn chain_id_invalid_url_returns_transport_error() {
        let err = chain_id("not-a-url").unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    /// A code read that cannot even reach a node is a transport failure, not an
    /// empty answer. The distinction is what lets the pre-purchase gate fail
    /// closed on the first and refuse the address on the second.
    #[test]
    fn get_code_invalid_url_returns_transport_error() {
        let err = get_code("not-a-url", Address::ZERO).unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn get_block_number_invalid_url_returns_transport_error() {
        let err = get_block_number("not-a-url").unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn identity_model_invalid_url_returns_transport_error() {
        let err = identity_model("not-a-url", Address::ZERO).unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn tba_implementation_invalid_url_returns_transport_error() {
        let err = tba_implementation("not-a-url", Address::ZERO).unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn supply_cap_invalid_url_returns_transport_error() {
        let err = supply_cap("not-a-url", Address::ZERO).unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn next_token_id_invalid_url_returns_transport_error() {
        let err = next_token_id("not-a-url", Address::ZERO).unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn mint_token_id_invalid_url_returns_transport_error() {
        let err = mint_token_id(
            "not-a-url",
            "0x1111111111111111111111111111111111111111111111111111111111111111",
            Address::ZERO,
            Address::ZERO,
        )
        .unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn mint_token_id_invalid_hash_is_not_retryable() {
        let err = mint_token_id(VALID_RPC, "not-a-hash", Address::ZERO, Address::ZERO).unwrap_err();
        assert!(matches!(err, RpcError::InvalidInput(_)), "{err}");
        assert!(!err.is_retryable(), "{err}");
    }

    /// The typehash is the difference between the front-runnable EIP-3009 path
    /// and the safe one, so it is pinned to its literal preimage here rather
    /// than trusted to a copied constant.
    #[test]
    fn receive_with_authorization_typehash_matches_its_preimage() {
        assert_eq!(
            RECEIVE_WITH_AUTHORIZATION_TYPEHASH,
            alloy::primitives::keccak256(
                // The `\` line continuation drops the newline and the leading
                // whitespace, so this is one unbroken type string.
                "ReceiveWithAuthorization(address from,address to,uint256 value,\
                 uint256 validAfter,uint256 validBefore,bytes32 nonce)"
                    .as_bytes()
            ),
            "the typehash must be keccak256 of the exact EIP-3009 type string",
        );
    }

    /// The digest a buyer signs is what authorises the money to move, so it is
    /// checked against a vector computed independently with `cast`:
    ///
    /// ```text
    /// ENC=$(cast abi-encode "f(bytes32,address,address,uint256,uint256,uint256,bytes32)" \
    ///        0xd099...3de8 0x1111...11 0x2222...22 5000000 0 1000000 0x3333...33)
    /// cast keccak 0x1901<domainSeparator><cast keccak $ENC>
    /// ```
    #[test]
    fn receive_authorization_digest_matches_an_independent_vector() {
        let digest = receive_authorization_digest(
            b256!("0101010101010101010101010101010101010101010101010101010101010101"),
            "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            "0x2222222222222222222222222222222222222222"
                .parse()
                .unwrap(),
            U256::from(5_000_000u64),
            U256::ZERO,
            U256::from(1_000_000u64),
            b256!("3333333333333333333333333333333333333333333333333333333333333333"),
        );

        assert_eq!(
            digest,
            b256!("139fd1f41f7fd692a669a5017ad6158e4642e7b11c3432e802cdde30faa473d6"),
        );
    }

    /// Every field is signed, so changing any one of them must change the
    /// digest. A field silently dropped from the struct hash would let a
    /// submitter alter it after the buyer signed.
    #[test]
    fn receive_authorization_digest_covers_every_field() {
        let ds = b256!("0101010101010101010101010101010101010101010101010101010101010101");
        let from: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let payee: Address = "0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap();
        let nonce = b256!("3333333333333333333333333333333333333333333333333333333333333333");
        let base = receive_authorization_digest(
            ds,
            from,
            payee,
            U256::from(5u64),
            U256::ZERO,
            U256::from(100u64),
            nonce,
        );

        let other: Address = "0x4444444444444444444444444444444444444444"
            .parse()
            .unwrap();
        let variants = [
            receive_authorization_digest(
                b256!("0202020202020202020202020202020202020202020202020202020202020202"),
                from,
                payee,
                U256::from(5u64),
                U256::ZERO,
                U256::from(100u64),
                nonce,
            ),
            receive_authorization_digest(
                ds,
                other,
                payee,
                U256::from(5u64),
                U256::ZERO,
                U256::from(100u64),
                nonce,
            ),
            receive_authorization_digest(
                ds,
                from,
                other,
                U256::from(5u64),
                U256::ZERO,
                U256::from(100u64),
                nonce,
            ),
            receive_authorization_digest(
                ds,
                from,
                payee,
                U256::from(6u64),
                U256::ZERO,
                U256::from(100u64),
                nonce,
            ),
            receive_authorization_digest(
                ds,
                from,
                payee,
                U256::from(5u64),
                U256::from(1u64),
                U256::from(100u64),
                nonce,
            ),
            receive_authorization_digest(
                ds,
                from,
                payee,
                U256::from(5u64),
                U256::ZERO,
                U256::from(101u64),
                nonce,
            ),
            receive_authorization_digest(
                ds,
                from,
                payee,
                U256::from(5u64),
                U256::ZERO,
                U256::from(100u64),
                b256!("4444444444444444444444444444444444444444444444444444444444444444"),
            ),
        ];

        for (i, v) in variants.iter().enumerate() {
            assert_ne!(base, *v, "field {i} is not covered by the digest");
        }
    }

    #[test]
    fn encode_purchase_with_authorization_matches_selector() {
        // keccak256("purchaseWithAuthorization(address,(address,uint256,uint256,bytes32,bytes))")[..4]
        let recipient: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let data = encode_purchase_with_authorization_calldata(recipient, sample_auth());
        assert!(
            data.starts_with("0x6a0221cb"),
            "unexpected selector in {data}",
        );
    }

    /// The signature is opaque bytes end to end: whatever the signer produced
    /// reaches the payment token byte for byte, which is what lets an EIP-1271
    /// smart-wallet signature of any length through the same entry point.
    #[test]
    fn encode_purchase_with_authorization_carries_the_signature_verbatim() {
        let recipient: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();

        for signature in [vec![0xABu8; 65], vec![0xCDu8; 200]] {
            let auth = IRub3License::PaymentAuthorization {
                signature: signature.clone().into(),
                ..sample_auth()
            };
            let data = encode_purchase_with_authorization_calldata(recipient, auth);
            assert!(
                data.contains(&hex::encode(&signature)),
                "signature of {} bytes was not encoded verbatim",
                signature.len(),
            );
        }
    }

    fn sample_auth() -> IRub3License::PaymentAuthorization {
        IRub3License::PaymentAuthorization {
            from: "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            validAfter: U256::ZERO,
            validBefore: U256::from(1u64),
            salt: B256::ZERO,
            signature: vec![0x11u8; 65].into(),
        }
    }

    #[test]
    fn encode_purchase_calldata_matches_selector() {
        // keccak256("purchase(address)")[..4] = 0x25b31a97
        let recipient: Address = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
            .parse()
            .unwrap();
        let data = encode_purchase_calldata(recipient);
        assert!(data.starts_with("0x25b31a97"), "got {data}");
        // selector (4) + 32-byte argument = 36 bytes = 72 hex chars, plus "0x" prefix.
        assert_eq!(data.len(), 2 + 72);
        // Last 40 chars are the recipient address hex, left-padded with zeros.
        assert!(
            data.ends_with("f39fd6e51aad88f6f4ce6ab8827279cfffb92266"),
            "recipient address not present: {data}",
        );
    }

    #[test]
    fn encode_purchase_calldata_differs_by_recipient() {
        let a: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let b: Address = "0x0000000000000000000000000000000000000002"
            .parse()
            .unwrap();
        assert_ne!(encode_purchase_calldata(a), encode_purchase_calldata(b));
    }

    // ── Receipt polling ───────────────────────────────────────────────────────

    #[cfg(any(feature = "onchain-write", feature = "cooldown"))]
    mod receipt_polling {
        use super::*;
        use std::cell::RefCell;
        use std::time::Duration;

        fn receipt() -> TxReceipt {
            TxReceipt {
                status: true,
                block_number: 42,
                block_hash: "0xblock".to_string(),
                to: None,
            }
        }

        /// Runs the real polling loop over a scripted sequence of answers,
        /// with a zero interval so no test waits on the wall clock.
        fn poll_over(
            answers: Vec<Result<Option<TxReceipt>, RpcError>>,
        ) -> (Result<TxReceipt, ReceiptWaitError>, usize) {
            let attempts = answers.len() as u32;
            let queue = RefCell::new(answers.into_iter());
            let calls = RefCell::new(0usize);
            let out = poll_for_receipt(
                || {
                    *calls.borrow_mut() += 1;
                    queue.borrow_mut().next().expect("polled past the budget")
                },
                attempts,
                Duration::ZERO,
            );
            let calls = *calls.borrow();
            (out, calls)
        }

        /// The regression: a purchase receipt poll that hits a 502 must keep
        /// polling inside its budget. Bailing on the first transport error
        /// abandons a transaction whose funds are already committed.
        #[test]
        fn a_transient_transport_failure_does_not_end_the_wait() {
            let (out, calls) = poll_over(vec![
                Err(RpcError::Transport("502 Bad Gateway".into())),
                Ok(None),
                Ok(Some(receipt())),
            ]);
            assert_eq!(calls, 3, "the loop stopped polling early");
            assert_eq!(out.expect("receipt").block_number, 42);
        }

        /// Only a budget that runs out while polling is still failing may be
        /// reported as a transport failure.
        #[test]
        fn a_transport_failure_is_reported_once_the_budget_is_exhausted() {
            let (out, _) = poll_over(vec![
                Ok(None),
                Err(RpcError::Transport("connection reset".into())),
                Err(RpcError::Transport("connection reset".into())),
            ]);
            let err = out.expect_err("the budget ran out");
            assert_eq!(
                err.transport_message(),
                Some("transport error: connection reset"),
                "{err}",
            );
        }

        /// A node that answers again after a wobble leaves an unmined tx, not
        /// a transport failure: the caller is waiting, not disconnected.
        #[test]
        fn a_recovered_poll_ends_as_a_timeout_not_a_transport_failure() {
            let (out, _) = poll_over(vec![
                Err(RpcError::Transport("502 Bad Gateway".into())),
                Ok(None),
                Ok(None),
            ]);
            let err = out.expect_err("the budget ran out");
            assert!(err.transport_message().is_none(), "{err}");
            assert!(matches!(err, ReceiptWaitError::Timeout { .. }), "{err}");
        }

        /// Both outcomes report the budget they consumed, so a caller can say
        /// how long it waited regardless of which one it got.
        /// A malformed request is a settled answer: the caller waited out the
        /// whole budget for a hash that could never parse, which the webview
        /// door showed as a "waiting for the tx to land" window for 27s before
        /// admitting the hash was junk.
        #[test]
        fn a_request_that_can_never_succeed_is_reported_at_once() {
            let (out, calls) = poll_over(vec![
                Err(RpcError::InvalidInput("invalid tx hash".into())),
                Ok(Some(receipt())),
                Ok(Some(receipt())),
            ]);
            assert_eq!(calls, 1, "a malformed request was retried");
            let err = out.expect_err("a hash that cannot parse cannot resolve");
            assert!(err.to_string().contains("invalid tx hash"), "{err}");
        }

        /// The same property through the public entry point, which is what the
        /// webview poller thread actually calls. No node is reached: the hash
        /// fails to parse first.
        #[test]
        fn wait_for_receipt_rejects_an_unparseable_hash_without_waiting() {
            let started = std::time::Instant::now();
            let err = wait_for_receipt("http://127.0.0.1:1", "not-a-hash")
                .expect_err("a hash that cannot parse cannot resolve");
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "sat on the retry budget for {:?}",
                started.elapsed(),
            );
            assert!(err.to_string().contains("invalid tx hash"), "{err}");
        }

        /// `waited_secs` on the exit-21 detail line is how an operator sizes
        /// up an unresolved purchase, so it has to be the time really spent,
        /// not `attempts * interval`. Both directions are wrong under the
        /// nominal figure: the loop sleeps one interval fewer than it has,
        /// and a slow endpoint can outrun the whole budget in one attempt.
        #[test]
        fn both_outcomes_report_wall_clock_time_not_the_nominal_budget() {
            let slow = || {
                std::thread::sleep(Duration::from_millis(1_100));
                Ok(None)
            };
            // Nominal budget 1 x 3600s; one attempt that really takes ~1.1s.
            let timeout =
                poll_for_receipt(slow, 1, Duration::from_secs(3600)).expect_err("never mined");
            assert!(
                (1..60).contains(&timeout.after_secs()),
                "reported the budget, not the wait: {}",
                timeout.after_secs(),
            );

            let slow_and_broken = || {
                std::thread::sleep(Duration::from_millis(1_100));
                Err(RpcError::Transport("offline".into()))
            };
            // Nominal budget 0s; the endpoint still burned ~1.1s answering.
            let transport =
                poll_for_receipt(slow_and_broken, 1, Duration::ZERO).expect_err("never answered");
            assert!(
                transport.after_secs() >= 1,
                "a dead endpoint reported no wait at all: {}",
                transport.after_secs(),
            );
        }
    }
}

// ── Stub-node tests for the call classifier ───────────────────────────────────

/// How a node answers matters as much as what it says, and the split between
/// "the chain answered" and "the node did not" decides whether the wrapper
/// falls back to the ETH rail or stops. These drive [`stablecoin_rail`] against a
/// local socket that returns one fixed body, so each classification is
/// exercised through the real public function rather than asserted about the
/// classifier in isolation.
#[cfg(test)]
mod stub_node_tests {
    use super::*;
    use crate::test_support::StubNode;

    /// A rate limit is the node's own state, not the contract's. Reading it as
    /// "this contract offers no stablecoin rail" would pay in the wrong
    /// currency because a public endpoint was busy.
    #[test]
    fn a_json_rpc_error_body_is_a_node_failure_not_an_absent_rail() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32005,"message":"rate limit exceeded"}}"#,
        );
        let err = stablecoin_rail(&node.url, Address::ZERO)
            .expect_err("a rate-limited node must not answer the rail question");
        assert!(
            err.is_transport(),
            "a rate limit must classify as transport, got {err}"
        );
        assert!(err.is_retryable(), "and must stay retryable, got {err}");
    }

    /// An execution timeout is likewise the node, not the chain.
    #[test]
    fn an_execution_timeout_is_a_node_failure_not_an_absent_rail() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"execution timeout"}}"#,
        );
        let err = stablecoin_rail(&node.url, Address::ZERO)
            .expect_err("an execution timeout must not answer the rail question");
        assert!(
            err.is_transport(),
            "an execution timeout must classify as transport, got {err}"
        );
    }

    /// A truncated or garbled body says nothing about the contract either.
    #[test]
    fn a_malformed_response_body_is_a_node_failure_not_an_absent_rail() {
        let node = StubNode::serving("not json at all");
        let err = stablecoin_rail(&node.url, Address::ZERO)
            .expect_err("an undeserializable response must not answer the rail question");
        assert!(
            err.is_transport(),
            "a deserialization failure must classify as transport, got {err}"
        );
    }

    /// The one node-side error that *is* an answer: a revert means the address
    /// has no `priceToken()`, which is exactly how a pre-§2.2 contract reads.
    #[test]
    fn a_reverted_call_means_the_contract_offers_no_stablecoin_rail() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":3,"message":"execution reverted"}}"#,
        );
        assert_eq!(
            stablecoin_rail(&node.url, Address::ZERO).expect("a revert is a settled answer"),
            None,
            "a reverted priceToken() means the ETH rail",
        );
    }

    /// Nodes that report a revert as -32000 with revert wording must read the
    /// same way as the ones that use code 3.
    #[test]
    fn a_revert_reported_as_minus_32000_also_means_no_stablecoin_rail() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"execution reverted"}}"#,
        );
        assert_eq!(
            stablecoin_rail(&node.url, Address::ZERO).expect("a revert is a settled answer"),
            None,
        );
    }

    /// An address with no code answers `0x`, which decodes to nothing.
    #[test]
    fn empty_return_data_means_the_contract_offers_no_stablecoin_rail() {
        let node = StubNode::serving(r#"{"jsonrpc":"2.0","id":0,"result":"0x"}"#);
        assert_eq!(
            stablecoin_rail(&node.url, Address::ZERO)
                .expect("empty return data is a settled answer"),
            None,
        );
    }

    fn preflight_against(node: &StubNode) -> RpcError {
        preflight_purchase_with_authorization(
            &node.url,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            IRub3License::PaymentAuthorization {
                from: Address::ZERO,
                validAfter: U256::ZERO,
                validBefore: U256::from(1u64),
                salt: B256::ZERO,
                signature: vec![0x11u8; 65].into(),
            },
        )
        .expect_err("the stub node never lets the call succeed")
    }

    /// The pre-flight decides whether a payment token is usable, so it splits
    /// failures the same way every other token-side read does. A revert is the
    /// answer it is looking for: the call the wrapper was about to broadcast
    /// would have reverted too.
    #[test]
    fn a_reverted_preflight_is_a_contract_answer_not_a_node_failure() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":3,"message":"execution reverted"}}"#,
        );
        let err = preflight_against(&node);
        assert!(
            !err.is_transport(),
            "a revert must not read as transport: {err}"
        );
    }

    /// And a node that will not answer must never be read as "this token is
    /// unusable", which would silently change the currency.
    #[test]
    fn a_preflight_against_a_failing_node_is_a_transport_failure() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32005,"message":"rate limit exceeded"}}"#,
        );
        let err = preflight_against(&node);
        assert!(
            err.is_transport(),
            "a rate limit must read as transport: {err}"
        );
    }
}
