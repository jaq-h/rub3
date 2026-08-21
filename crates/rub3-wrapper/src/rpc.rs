use alloy::primitives::{b256, Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use alloy::sol_types::SolCall;

// ── Contract interface ────────────────────────────────────────────────────────

// Minimal ABI surface needed for activation + session flow (tiers 2-3):
//   ownerOf(tokenId)              - ERC-721 standard
//   price()                       - rub3 license contract
//   balanceOf(owner)              - ERC-721 standard
//   tokenOfOwnerByIndex(...)      - ERC-721Enumerable
//   activate(tokenId)             - tier-3 session activation (returns sessionId)
//   release(tokenId, sessionId)   - hands a seat back before its TTL (§3.4)
//   activationStatus(tokenId)     - what activate() would do, in one call (§3.4)
//   lastActivationBlock(tokenId)  - tier-3 read
//   cooldownBlocks()              - tier-3 read
//   seatsPerToken()               - concurrent sessions one token grants (§3.4)
//   sessionSeat(tokenId, id)      - whether a session still holds a seat
//   seatAt(tokenId, index)        - one seat's raw state
//   identityModel()               - 0 = access, 1 = account (read at session creation)
//   tbaImplementation()           - ERC-6551 impl for account-model TBA derivation
//   supplyCap()                   - immutable mint cap (0 = unlimited)
//   nextTokenId()                 - next id to be minted
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
        function release(uint256 tokenId, uint256 sessionId) external;
        function lastActivationBlock(uint256 tokenId) external view returns (uint256 blockNumber);
        function cooldownBlocks() external view returns (uint256 blocks);
        function seatsPerToken() external view returns (uint256 seats);
        function sessionTtlSeconds() external view returns (uint256 secs);
        function sessionSeat(uint256 tokenId, uint256 sessionId)
            external view returns (bool live, uint256 index);

        /// Mirrors `Rub3License.Seat`. `activatedAt` is the seat's cooldown
        /// stamp and is never cleared; `expiresAt` at or before the current
        /// block timestamp means the seat is free.
        struct Seat {
            uint64  activatedAt;
            uint64  expiresAt;
            uint256 sessionId;
        }

        function seatAt(uint256 tokenId, uint256 index) external view returns (Seat memory seat);

        /// Mirrors `Rub3License.ActivationStatus` (§3.4). Field order is part of
        /// the ABI encoding, so it must match the Solidity struct exactly.
        ///
        /// The two refusals are told apart by `fleetExhausted`: a full fleet
        /// waits `secondsRemaining` for a seat to lapse, while a cooldown waits
        /// `blocksRemaining`. Neither number is meaningful for the other case,
        /// which is why the contract answers both in one call rather than
        /// leaving a caller to infer which it is looking at. The flag is the
        /// contract's own answer rather than something derived from the seat
        /// counts, because a single-seat licence's one occupied seat is
        /// retakeable and so is not exhaustion.
        struct ActivationStatus {
            bool    ready;
            bool    fleetExhausted;
            uint256 seatIndex;
            uint256 seatsInUse;
            uint256 seats;
            uint256 blocksRemaining;
            uint256 secondsRemaining;
        }

        function activationStatus(uint256 tokenId)
            external view returns (ActivationStatus memory status);

        /// Emitted by `activate(tokenId)`. Declared here for its topic0 *and*
        /// its body: `watch_for_activate` uses the topic to tell which
        /// transaction in a block was the activation it is waiting for, and
        /// `activation_from_receipt` decodes the body to learn which seat and
        /// session id that activation got. Taking both from the ABI is what
        /// stops a hand-copied constant drifting from the contract.
        event Activated(
            uint256 indexed tokenId,
            address indexed owner,
            uint256 sessionId,
            uint256 seatIndex,
            uint256 expiresAt
        );

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

/// `keccak256("Transfer(address,address,uint256)")` - the ERC-721 Transfer
/// event topic0. Mint events have `from == address(0)`.
const ERC721_TRANSFER_SIG: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

// ── Receipt ───────────────────────────────────────────────────────────────────

/// Minimal tx receipt - the fields the wrapper cares about.
#[derive(Debug, Clone)]
pub struct TxReceipt {
    pub status: bool,
    pub block_number: u64,
    pub block_hash: String,
    /// `to` address from the receipt, lowercased hex. Used by tier-3
    /// on-chain re-verification to confirm the tx hit the license contract.
    pub to: Option<String>,
    /// Logs this transaction emitted, in order.
    ///
    /// Carried because a seat activation's own receipt is the only unambiguous
    /// record of which seat it got (§3.4): a second read of contract state
    /// under a fleet spinning up would just as happily return somebody else's
    /// activation.
    pub logs: Vec<ReceiptLog>,
}

/// One log from a [`TxReceipt`], in the raw form an ABI decoder wants.
#[derive(Debug, Clone)]
pub struct ReceiptLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Vec<u8>,
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
    /// A watch (§5.1a) stopped without seeing what it was waiting for. The
    /// node never failed: it answered, and the answer was "not yet".
    WatchEnded(WatchEnd),
}

/// Why a watch stopped without a match.
///
/// Separate from every other [`RpcError`] because nothing went wrong. The
/// transaction may still land a second later, which is exactly why the screen
/// that started the watch falls back to the manual paste rather than reporting
/// a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEnd {
    /// The budget ran out.
    Timeout,
    /// The screen that started the watch asked it to stop: the user switched
    /// to another tab, moved on, or closed the window.
    Cancelled,
}

impl std::fmt::Display for WatchEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchEnd::Timeout => f.write_str("the watch ran out of time"),
            WatchEnd::Cancelled => f.write_str("the watch was cancelled"),
        }
    }
}

impl RpcError {
    /// Builds a [`RpcError::Transport`] from a network-layer error, with any
    /// URL in its message reduced to `scheme://host[:port]`.
    ///
    /// Sanitizing at construction, rather than at a display helper, is
    /// deliberate. The packed `RPC_URL` can carry a provider API key in its
    /// userinfo, its path or its query, and alloy builds these messages from
    /// the request, so the key rides in the error value itself. A formatter on
    /// the webview's error path would cover the window only: the same string
    /// reaches an operator through the agent door, which turns
    /// `attest::GateError::Fetch` into a printed `HeadlessError::Rpc`, and
    /// through `show_purchase`'s `eprintln!`. Redacting here means the URL
    /// never enters the value, so every surface that exists now or later
    /// inherits it and no call site has to remember.
    ///
    /// Distinct from the webview's kind-only `detail`, which is a separate
    /// decision about how much a non-technical buyer should be invited to
    /// paste into a support channel. This one is the floor under every
    /// surface; that one is a choice about one screen. Do not collapse them.
    pub fn transport(e: impl std::fmt::Display) -> RpcError {
        RpcError::Transport(redact_urls(&e.to_string()))
    }

    /// Builds a [`RpcError::Contract`] from a contract-call error, redacted the
    /// same way as [`RpcError::transport`].
    ///
    /// Needed as much as the transport constructor: alloy reports an
    /// unreachable node during an `eth_call` as a contract-layer error, so the
    /// most-travelled leak is here rather than in `Transport`. The window's
    /// "ownership check failed" message, which every buyer meets before a
    /// purchase screen exists, is built from a `tokens_of_owner` failure of
    /// exactly this kind.
    pub fn contract(e: impl std::fmt::Display) -> RpcError {
        RpcError::Contract(redact_urls(&e.to_string()))
    }

    /// Whether repeating the identical call could plausibly succeed later.
    ///
    /// Only transport failures qualify: a 502, a rate limit or a dropped
    /// connection says nothing about the request. A reverted call, a malformed
    /// argument and an unimplemented feature are all settled answers.
    pub fn is_retryable(&self) -> bool {
        match self {
            // A watch that ran out of time asked a question the chain had not
            // answered yet; asking again is the whole remedy.
            RpcError::Transport(_) | RpcError::WatchEnded(WatchEnd::Timeout) => true,
            RpcError::Contract(_)
            | RpcError::InvalidInput(_)
            | RpcError::EnsNotSupported
            | RpcError::WatchEnded(WatchEnd::Cancelled) => false,
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

/// Every character that cannot appear inside a URL, and so ends one.
///
/// `)` is in the set because reqwest wraps the URL in parentheses, and every
/// other member is excluded by RFC 3986 outright. `,`, `.` and `;` are
/// deliberately absent: they are legal in a query string, and cutting the token
/// short at one would leave the tail of the URL - which is where a key sits -
/// standing in the message. `[` and `]` are absent for the same reason and a
/// sharper one: they open and close an IPv6 host, so treating them as
/// terminators would cut the token to `scheme://` and leave the entire
/// authority, path and query behind as prose.
const URL_TERMINATORS: &[char] = &[
    ' ', '\t', '\n', '\r', '"', '\'', '(', ')', '{', '}', '<', '>', '`', '|', '\\', '^',
];

/// What replaces a URL whose authority could not be read.
const UNREADABLE_URL: &str = "[redacted url]";

/// Rewrites every URL in `message` to `scheme://host[:port]`.
///
/// The host and port stay: an operator chasing a dead endpoint needs to know
/// which one failed, and neither is the secret. Userinfo, path, query and
/// fragment all go, because a provider key is put in one of those three.
///
/// Fails closed. A token whose authority will not parse is replaced by
/// [`UNREADABLE_URL`] and then consumed to the next whitespace, because the
/// alternative - emitting the placeholder and letting the unparsed tail through
/// as prose - would print the key while claiming to have redacted it, which is
/// worse than not redacting at all.
pub(crate) fn redact_urls(message: &str) -> String {
    let bytes = message.as_bytes();
    let mut out = String::with_capacity(message.len());
    let mut cursor = 0;

    while let Some(offset) = message[cursor..].find("://") {
        let separator = cursor + offset;

        // Walk back over the scheme. Stopping where the scheme does means a
        // bare "://" in prose is left alone rather than swallowing the words
        // in front of it.
        let mut start = separator;
        while start > cursor {
            let c = bytes[start - 1] as char;
            if c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.' {
                start -= 1;
            } else {
                break;
            }
        }
        if start == separator {
            out.push_str(&message[cursor..separator + 3]);
            cursor = separator + 3;
            continue;
        }

        out.push_str(&message[cursor..start]);

        let authority_start = separator + 3;
        let mut end = url_token_end(message, authority_start);
        // Sentence punctuation trailing the URL belongs to the sentence, so it
        // is handed back rather than parsed as part of the address.
        while end > authority_start && matches!(bytes[end - 1], b'.' | b',' | b';' | b':') {
            end -= 1;
        }

        match redact_url(&message[start..end]) {
            Some(origin) => {
                out.push_str(&origin);
                cursor = end;
            }
            None => {
                out.push_str(UNREADABLE_URL);
                let tail = &message[end..];
                cursor = end + tail.find(char::is_whitespace).unwrap_or(tail.len());
            }
        }
    }

    out.push_str(&message[cursor..]);
    out
}

/// Where the URL beginning at `authority_start` stops.
///
/// An IPv6 host is bracketed, and the brackets belong to it, so the scan skips
/// past the matching `]` before looking for a terminator. Its port, path and
/// query then terminate exactly as any other URL's do. An opening bracket with
/// no closing one is not an authority this can read, so the token is cut to
/// nothing and the caller's fail-closed branch takes it.
fn url_token_end(message: &str, authority_start: usize) -> usize {
    let rest = &message[authority_start..];
    let scan_from = if rest.starts_with('[') {
        match rest.find(']') {
            Some(bracket) => authority_start + bracket + 1,
            None => return authority_start,
        }
    } else {
        authority_start
    };
    let tail = &message[scan_from..];
    scan_from + tail.find(URL_TERMINATORS).unwrap_or(tail.len())
}

/// One URL token, reduced to its origin, or `None` when there is no authority
/// to reduce it to. An address this cannot read is an address it cannot promise
/// carries no key, so the caller drops it whole rather than passing any of it
/// on.
fn redact_url(token: &str) -> Option<String> {
    let url = token.parse::<url::Url>().ok()?;
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => Some(format!("{}://{host}:{port}", url.scheme())),
        (Some(host), None) => Some(format!("{}://{host}", url.scheme())),
        (None, _) => None,
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
            RpcError::WatchEnded(end) => write!(f, "{end}"),
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
            .map_err(RpcError::contract)?;
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
        let result = instance.price().call().await.map_err(RpcError::contract)?;
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
            .map_err(RpcError::contract)?;

        let count = balance.to::<u64>();
        let mut tokens = Vec::with_capacity(count as usize);

        for i in 0..count {
            let token_id = instance
                .tokenOfOwnerByIndex(owner, U256::from(i))
                .call()
                .await
                .map_err(RpcError::contract)?;
            tokens.push(token_id.to::<u64>());
        }

        Ok(tokens)
    })
}

/// Resolves an ENS name to an Ethereum address.
///
/// Stub - full implementation in Phase 1.6.
pub fn resolve_ens(_rpc_url: &str, _name: &str) -> Result<Address, RpcError> {
    Err(RpcError::EnsNotSupported)
}

// ── Tier-3: activation / cooldown ─────────────────────────────────────────────

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
            .map_err(RpcError::contract)?;
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
            .map_err(RpcError::contract)?;
        Ok(r.to::<u64>())
    })
}

/// Returns the 0x-prefixed ABI-encoded calldata for `activate(tokenId)`.
///
/// Pure - no RPC. The wrapper shows this to the user so they can paste it
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
            .map_err(RpcError::transport)?;

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

        let logs = receipt
            .inner
            .logs()
            .iter()
            .map(|log| ReceiptLog {
                address: log.address(),
                topics: log.topics().to_vec(),
                data: log.data().data.to_vec(),
            })
            .collect();

        Ok(Some(TxReceipt {
            status: receipt.status(),
            block_number,
            block_hash,
            to,
            logs,
        }))
    })
}

/// Everything an `activate()` transaction settled about the session that
/// follows it, decoded from the `Activated` log in its **own** receipt.
///
/// The receipt rather than a follow-up state read, and that is the whole point
/// under seats (§3.4): with a fleet coming up, `activate()` calls from several
/// instances land in the same block, so any view read afterwards may honestly
/// answer with a seat somebody else just took. A transaction's own log cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationRecord {
    /// The session id the contract assigned.
    pub session_id: u64,
    /// Which of the token's seats it landed on.
    pub seat_index: u64,
    /// Unix seconds at which that seat frees itself if nobody releases it.
    pub seat_expires_at: u64,
}

/// Decodes the `Activated` log this receipt emitted for `token_id`.
///
/// Pure - no RPC. Errors when the receipt carries no such log, which means the
/// transaction was not the activation it was taken for.
pub fn activation_from_receipt(
    receipt: &TxReceipt,
    contract: Address,
    token_id: u64,
) -> Result<ActivationRecord, RpcError> {
    use alloy::sol_types::SolEvent;

    let wanted = U256::from(token_id);
    for log in &receipt.logs {
        if log.address != contract {
            continue;
        }
        let decoded = match IRub3License::Activated::decode_raw_log(log.topics.clone(), &log.data) {
            Ok(event) => event,
            Err(_) => continue,
        };
        if decoded.tokenId != wanted {
            continue;
        }
        return Ok(ActivationRecord {
            session_id: saturating_u64(decoded.sessionId),
            seat_index: saturating_u64(decoded.seatIndex),
            seat_expires_at: saturating_u64(decoded.expiresAt),
        });
    }

    Err(RpcError::Contract(format!(
        "the activate() receipt carries no Activated log for token {token_id} from {contract}"
    )))
}

/// A `uint256` the contract has bounded well inside 64 bits, read as a `u64`.
///
/// Saturating rather than wrapping: every field this is used on is a counter, a
/// seat index, or a Unix timestamp, and for all three a clamp at `u64::MAX` is
/// a value that fails loudly downstream, while a wrap is one that reads as
/// plausible.
fn saturating_u64(value: U256) -> u64 {
    value.try_into().unwrap_or(u64::MAX)
}

/// What `activate(tokenId)` would do right now (§3.4), read in one `eth_call`.
///
/// Mirrors the contract's own scan, so a wrapper that reads this and a wrapper
/// that sends the transaction cannot disagree about which seat is next or why
/// there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationStatus {
    /// Whether `activate()` would succeed at the block this was read at.
    pub ready: bool,
    /// Whether `activate()` would be refused with nothing to wait for in
    /// blocks: every seat holds a live session that this caller may not retake.
    ///
    /// The contract's own answer. Not derived from `seats_in_use == seats`,
    /// because a single-seat licence's one occupied seat *is* retakeable - a
    /// sole holder is never locked out of their own licence - so the counts
    /// alone do not settle it.
    pub fleet_exhausted: bool,
    /// The seat `activate()` would take. Meaningful only when `ready`.
    pub seat_index: u64,
    /// Seats holding a session that has neither lapsed nor been released.
    pub seats_in_use: u64,
    /// Seats this contract grants per token.
    pub seats: u64,
    /// Blocks until the earliest free seat leaves its cooldown. Meaningful only
    /// when `!ready && seats_in_use < seats`.
    pub blocks_remaining: u64,
    /// Seconds until the earliest occupied seat lapses. This is the wait when
    /// the fleet is full.
    pub seconds_remaining: u64,
}

/// Calls `activationStatus(tokenId)`.
///
/// Classified rather than blanket-labelled a contract error, for the reason
/// `poll_for_activate` gives: the auto-detect watch reads this before it starts
/// and retries it on `retry_read`, so a rate limit reported as a revert would be
/// a settled answer that ends the watch on the first hiccup.
pub fn activation_status(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
) -> Result<ActivationStatus, RpcError> {
    block_on(read_activation_status(rpc_url, contract, token_id))
}

/// The same read, abandoned if the endpoint has not answered within `limit`.
///
/// For the watch path, which cannot afford an unbounded request; see
/// [`WATCH_REQUEST_TIMEOUT`].
#[cfg(feature = "onchain-write")]
pub fn activation_status_within(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
    limit: std::time::Duration,
) -> Result<ActivationStatus, RpcError> {
    block_on_within(read_activation_status(rpc_url, contract, token_id), limit)
}

async fn read_activation_status(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
) -> Result<ActivationStatus, RpcError> {
    let provider = build_provider(rpc_url)?;
    let instance = IRub3License::new(contract, provider);
    let r = instance
        .activationStatus(U256::from(token_id))
        .call()
        .await
        .map_err(|e| classify_call_error(&e))?;
    Ok(ActivationStatus {
        ready: r.ready,
        fleet_exhausted: r.fleetExhausted,
        seat_index: saturating_u64(r.seatIndex),
        seats_in_use: saturating_u64(r.seatsInUse),
        seats: saturating_u64(r.seats),
        blocks_remaining: saturating_u64(r.blocksRemaining),
        seconds_remaining: saturating_u64(r.secondsRemaining),
    })
}

impl ActivationStatus {
    /// How long before the contract would take an `activate()`, given a
    /// block-time estimate for the cooldown case.
    ///
    /// One place for the arithmetic, because the two refusals are measured in
    /// different units and a caller that got the unit wrong would arm a watch
    /// for a transaction the chain is guaranteed not to see.
    #[cfg(feature = "onchain-write")]
    pub fn wait_before_activate(
        &self,
        block_wait: impl Fn(u64) -> std::time::Duration,
    ) -> std::time::Duration {
        if self.ready {
            std::time::Duration::ZERO
        } else if self.fleet_exhausted {
            std::time::Duration::from_secs(self.seconds_remaining)
        } else {
            block_wait(self.blocks_remaining)
        }
    }
}

/// Calls `sessionSeat(tokenId, sessionId)`: whether that session still holds a
/// seat, and which one.
pub fn session_seat(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
    session_id: u64,
) -> Result<(bool, u64), RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .sessionSeat(U256::from(token_id), U256::from(session_id))
            .call()
            .await
            .map_err(|e| classify_call_error(&e))?;
        Ok((r.live, saturating_u64(r.index)))
    })
}

/// Calls `seatAt(tokenId, index)`: the session id holding that seat, and the
/// Unix second at which it frees itself.
///
/// `None` when the seat is free - never taken, released, or lapsed - so a
/// caller cannot mistake a stale record for an occupant.
///
/// **A lapsed seat is free, and only the chain's clock can say so.** `seatAt`
/// returns the raw record, and a seat whose TTL ran out with nobody calling
/// `release` still carries the session id and expiry it was written with; the
/// contract's own `activate()` treats it as free from `expiresAt` onwards
/// (§3.4). This compares against the head block's timestamp for that reason,
/// rather than against the local clock, which the contract never consults.
pub fn occupied_seat(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
    index: u64,
) -> Result<Option<(u64, u64)>, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, &provider);
        let r = instance
            .seatAt(U256::from(token_id), U256::from(index))
            .call()
            .await
            .map_err(|e| classify_call_error(&e))?;
        if r.expiresAt == 0 || r.sessionId.is_zero() {
            return Ok(None);
        }
        let now = provider
            .get_block(alloy::eips::BlockId::latest())
            .await
            .map_err(RpcError::transport)?
            .ok_or_else(|| RpcError::transport("the node returned no latest block"))?
            .header
            .timestamp;
        if r.expiresAt <= now {
            return Ok(None);
        }
        Ok(Some((saturating_u64(r.sessionId), r.expiresAt)))
    })
}

/// Returns the 0x-prefixed ABI-encoded calldata for
/// `release(tokenId, sessionId)`.
///
/// Pure - no RPC, the same shape [`encode_activate_calldata`] has.
pub fn encode_release_calldata(token_id: u64, session_id: u64) -> String {
    let call = IRub3License::releaseCall {
        tokenId: U256::from(token_id),
        sessionId: U256::from(session_id),
    };
    format!("0x{}", hex::encode(call.abi_encode()))
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
            .map_err(RpcError::contract)?;
        Ok(r)
    })
}

/// Reads the contract's `tbaImplementation()` getter - the ERC-6551 account
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
            .map_err(RpcError::contract)?;
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
        provider.get_chain_id().await.map_err(RpcError::transport)
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
            .map_err(RpcError::transport)?;
        Ok(code.to_vec())
    })
}

/// Returns the current block number on the target chain.
pub fn get_block_number(rpc_url: &str) -> Result<u64, RpcError> {
    block_on(head_block(rpc_url))
}

/// The same read, abandoned if the endpoint has not answered within `limit`.
///
/// For the watch path, which cannot afford an unbounded request; see
/// [`WATCH_REQUEST_TIMEOUT`].
#[cfg(feature = "onchain-write")]
pub fn get_block_number_within(rpc_url: &str, limit: std::time::Duration) -> Result<u64, RpcError> {
    block_on_within(head_block(rpc_url), limit)
}

async fn head_block(rpc_url: &str) -> Result<u64, RpcError> {
    let provider = build_provider(rpc_url)?;
    provider
        .get_block_number()
        .await
        .map_err(RpcError::transport)
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
            .map_err(RpcError::contract)?;
        Ok(r.to::<u64>())
    })
}

/// Reads `nextTokenId()` - the id the next `purchase()` will mint.
pub fn next_token_id(rpc_url: &str, contract: Address) -> Result<u64, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3License::new(contract, provider);
        let r = instance
            .nextTokenId()
            .call()
            .await
            .map_err(RpcError::contract)?;
        Ok(r.to::<u64>())
    })
}

/// Returns the 0x-prefixed ABI-encoded calldata for `purchase(recipient)`.
///
/// Pure - no RPC. The wrapper shows this to the user so they can paste it
/// into their wallet to send the tx themselves. `msg.value` is handled
/// separately in the UI.
pub fn encode_purchase_calldata(recipient: Address) -> String {
    let call = IRub3License::purchaseCall { recipient };
    format!("0x{}", hex::encode(call.abi_encode()))
}

// ── Auto-detect watchers (§5.1a) ──────────────────────────────────────────────
//
// The manual path asks a person to paste a transaction hash back into the
// window after sending from their wallet. These two functions do that step for
// them: they watch the chain for the transaction the screen just asked for and
// return its hash, which is fed to the very same handler the paste feeds. The
// only thing that changes is where the hash comes from, which is why nothing
// downstream of the hash exists twice.
//
// Chain RPC only. No JS bundle, no relay, no project id, no external service -
// tier 3 already has an endpoint, and that endpoint is the whole dependency.

/// How long a watch sleeps between polls.
///
/// Matches the receipt poller's cadence ([`RECEIPT_POLL_INTERVAL_SECS`]): a
/// watch and a receipt wait sit back to back in the same flow, and two
/// different rhythms in one wait would only make the window feel arrhythmic.
#[cfg(feature = "onchain-write")]
pub const WATCH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// The default total budget for a watch.
///
/// Longer than the receipt wait's 30s because the clock starts before the user
/// has done anything: they are still finding the transaction in their wallet,
/// reading it and approving it while this runs. The budget is a parameter, so a
/// caller that knows better may say so; this is what the window uses.
#[cfg(feature = "onchain-write")]
pub const WATCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// How long one request a watch makes may take before it is abandoned.
///
/// A watch consults its deadline *between* polls, so a request that never
/// returns is a request the deadline never gets to end: the thread parks inside
/// it, the fallback to the manual tab never fires, and the cancel flag the
/// screen raised on its way out is never read again. An endpoint that accepts
/// the connection and then answers nothing is an ordinary overload mode for
/// public RPC, so the bound belongs on every request a watch makes rather than
/// on the watch as a whole.
///
/// Two poll intervals. One would cut off an endpoint that is slow but working,
/// turning a delay into a failure; two still puts twenty deadline checks inside
/// the default budget, which is all the deadline needs to be reachable. An
/// expired request is [`RpcError::Transport`], so it is absorbed and retried
/// like any other bad answer and only a sustained run of them ends the watch.
///
/// Scoped to the watch deliberately: the pre-existing pollers share
/// `build_provider` and are not part of this change.
#[cfg(feature = "onchain-write")]
pub const WATCH_REQUEST_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(WATCH_POLL_INTERVAL.as_secs() * 2);

/// How many polls in a row may fail before a watch gives up.
///
/// A watch is long, and a single 502 or rate-limit inside it says nothing, so
/// failures are absorbed and retried. A sustained run of them is different: it
/// is an endpoint that is not going to answer, and spending the rest of the
/// budget on it only delays the manual paste that will actually work. Five
/// polls is fifteen seconds of silence, which no transient blip reaches.
#[cfg(feature = "onchain-write")]
const WATCH_MAX_CONSECUTIVE_ERRORS: u32 = 5;

/// A flag a watch checks between polls, so the screen that started it can stop
/// it.
///
/// Cheap to clone and cheap to raise, because both halves are held across a
/// thread boundary: the watch owns one clone and polls it, the IPC handler
/// owns the other and raises it. A watch that could only end on its own budget
/// would go on hammering an endpoint for two minutes after the screen that
/// wanted the answer is gone, which is a defect and not a cosmetic one.
#[cfg(feature = "onchain-write")]
#[derive(Clone, Default)]
pub struct Cancel(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(feature = "onchain-write")]
impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Asks every watch holding a clone of this handle to stop. Idempotent.
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// When a watch may run: a wall-clock budget, the flag that can end it sooner,
/// and the moment before which there is nothing worth asking about.
///
/// The budget and the flag travel together deliberately. They are the same
/// question - "should this still be running?" - asked of a clock and of a
/// person, and a watch that consulted only one of them would either outlive its
/// screen or ignore its budget. Bundling them also means a call site cannot pass
/// a budget and forget the cancellation.
///
/// The hold answers the other half of the same question, "should this be running
/// *yet*", which the cooldown screen is the reason for: see
/// [`Deadline::starting_in`].
#[cfg(feature = "onchain-write")]
#[derive(Clone)]
pub struct Deadline {
    at: std::time::Instant,
    not_before: Option<std::time::Instant>,
    cancel: Cancel,
}

#[cfg(feature = "onchain-write")]
impl Deadline {
    /// A budget of `budget` from now, which nothing can cut short.
    pub fn after(budget: std::time::Duration) -> Deadline {
        Deadline {
            at: std::time::Instant::now() + budget,
            not_before: None,
            cancel: Cancel::new(),
        }
    }

    /// The same budget, endable early through `cancel`.
    pub fn cancelled_by(self, cancel: Cancel) -> Deadline {
        Deadline { cancel, ..self }
    }

    /// The same budget, but not spent until `delay` has passed.
    ///
    /// The budget is the answer to "how long will we look", so a wait the chain
    /// imposes before there is anything to look at has to be added to it rather
    /// than taken out of it - which is why `at` moves too. The cooldown screen
    /// is the case: the contract reverts an `activate()` until the cooldown runs
    /// out, so a watch armed the moment that screen renders would spend its
    /// whole budget polling for a transaction the chain is guaranteed not to
    /// have, and hand back to the manual paste before the user could legally
    /// send one. On this project's default cooldown of 1800 blocks that is an
    /// hour early.
    ///
    /// Holding loses nothing, because a watch reads state rather than events: a
    /// transaction broadcast during the hold is still there to be found on the
    /// first poll after it. Cancellation is honoured throughout the hold, so a
    /// held watch is no more able to outlive its screen than a polling one.
    ///
    /// `delay` is derived from what the contract says is left of the cooldown,
    /// which is a `uint256` the wrapper does not bound, and `Instant + Duration`
    /// panics on overflow rather than saturating. That panic would land on the
    /// watcher thread, where nothing reports it: no `onAutoWatchEnded` is
    /// emitted and the page spins forever with no fallback to the manual paste -
    /// the one failure this whole path exists to prevent. A delay that cannot be
    /// represented therefore degrades to no hold at all, so the watch spends its
    /// budget and hands back to the manual tab in the ordinary way.
    pub fn starting_in(self, delay: std::time::Duration) -> Deadline {
        if delay.is_zero() {
            return self;
        }
        let now = std::time::Instant::now();
        let (Some(at), Some(not_before)) = (self.at.checked_add(delay), now.checked_add(delay))
        else {
            return self;
        };
        Deadline {
            at,
            not_before: Some(not_before),
            ..self
        }
    }

    /// Sleeps out the hold, or says why the watch must stop instead of polling.
    fn hold(&self) -> Option<WatchEnd> {
        let left = self
            .not_before?
            .saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        self.sleep(left)
    }

    /// Why the watch must stop right now, or `None` while it may continue.
    fn reached(&self) -> Option<WatchEnd> {
        if self.cancel.is_cancelled() {
            Some(WatchEnd::Cancelled)
        } else if std::time::Instant::now() >= self.at {
            Some(WatchEnd::Timeout)
        } else {
            None
        }
    }

    /// Sleeps up to `interval`, waking early if the deadline is reached.
    ///
    /// Sliced rather than slept in one go: a cancellation that took a full poll
    /// interval to be noticed would leave a request in flight against an
    /// endpoint nobody is waiting on, and the whole point of the flag is that
    /// the watch stops when the screen does.
    fn sleep(&self, interval: std::time::Duration) -> Option<WatchEnd> {
        const SLICE: std::time::Duration = std::time::Duration::from_millis(50);

        let until = std::time::Instant::now() + interval;
        loop {
            if let Some(end) = self.reached() {
                return Some(end);
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            std::thread::sleep(left.min(SLICE));
        }
    }
}

/// Polls `poll` on `interval` until it yields a hash, the deadline is reached,
/// or the endpoint stops answering.
///
/// The loop behind both watchers, with the query passed in so the retry, the
/// timeout and the cancellation can all be exercised without a node - the same
/// arrangement [`poll_for_receipt`] uses, and for the same reason.
///
/// A retryable failure never ends the watch on its own: the budget is long, and
/// one bad response inside it is noise. [`WATCH_MAX_CONSECUTIVE_ERRORS`] in a
/// row is not noise, and is reported as the failure it is rather than as a
/// timeout, so the screen can say which of the two happened.
#[cfg(feature = "onchain-write")]
fn watch<T, F>(
    mut poll: F,
    deadline: &Deadline,
    interval: std::time::Duration,
) -> Result<T, RpcError>
where
    F: FnMut() -> Result<Option<T>, RpcError>,
{
    // Nothing is asked of the endpoint until the deadline says there is
    // something to ask about; see `Deadline::starting_in`.
    if let Some(end) = deadline.hold() {
        return Err(RpcError::WatchEnded(end));
    }

    let mut consecutive_errors = 0u32;
    loop {
        if let Some(end) = deadline.reached() {
            return Err(RpcError::WatchEnded(end));
        }

        match poll() {
            Ok(Some(found)) => return Ok(found),
            // The node answered, so whatever failed before it was transient.
            Ok(None) => consecutive_errors = 0,
            Err(e) if !e.is_retryable() => return Err(e),
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors >= WATCH_MAX_CONSECUTIVE_ERRORS {
                    return Err(e);
                }
            }
        }

        if let Some(end) = deadline.sleep(interval) {
            return Err(RpcError::WatchEnded(end));
        }
    }
}

/// Makes one read on the same terms [`watch`] polls on: the same cadence, the
/// same tolerance for a run of retryable failures, the same deadline and the
/// same cancellation.
///
/// For the reads a watch has to make *before* it can start - the head block it
/// counts from, and the cooldown it may have to wait out. Those are ordinary
/// calls against the same endpoint the poll loop is about to be forgiving with,
/// and without this they are the only requests in the flow with no tolerance at
/// all: one 429 from a public endpoint on the first of them ends auto-detect a
/// second after the screen rendered, switches the tab and takes the focus, while
/// the identical 429 one poll later is absorbed in silence. The asymmetry is the
/// bug; the retry policy lives in one place so it cannot come back.
#[cfg(feature = "onchain-write")]
pub fn retry_read<T, F>(mut read: F, deadline: &Deadline) -> Result<T, RpcError>
where
    F: FnMut() -> Result<T, RpcError>,
{
    watch(|| read().map(Some), deadline, WATCH_POLL_INTERVAL)
}

/// Watches for the ERC-721 mint that a `purchase()` sent from the user's wallet
/// produces, and returns its transaction hash.
///
/// Polls `eth_getLogs` for `Transfer(0x0, recipient, *)` emitted by `contract`,
/// from `from_block` - the block the screen was opened at - to the head. First
/// match wins: a wallet that owns nothing is buying one licence, and the hash
/// goes to the same handler a pasted hash goes to.
///
/// Ends with [`RpcError::WatchEnded`] when the budget runs out or the screen
/// cancels; neither means the purchase failed, only that this way of learning
/// about it did.
#[cfg(feature = "onchain-write")]
pub fn watch_for_mint(
    rpc_url: &str,
    contract: Address,
    recipient: Address,
    from_block: u64,
    deadline: Deadline,
) -> Result<String, RpcError> {
    watch(
        || poll_for_mint(rpc_url, contract, recipient, from_block),
        &deadline,
        WATCH_POLL_INTERVAL,
    )
}

/// One `eth_getLogs`. `Ok(None)` means no mint yet.
#[cfg(feature = "onchain-write")]
fn poll_for_mint(
    rpc_url: &str,
    contract: Address,
    recipient: Address,
    from_block: u64,
) -> Result<Option<String>, RpcError> {
    use alloy::rpc::types::Filter;

    block_on_within(
        async move {
            let provider = build_provider(rpc_url)?;
            // The emitter, the mint and the recipient are all pinned, so the node
            // does the matching.
            let filter = Filter::new()
                .address(contract)
                .from_block(from_block)
                .event_signature(ERC721_TRANSFER_SIG)
                .topic1(B256::ZERO)
                .topic2(recipient.into_word());

            let logs = provider
                .get_logs(&filter)
                .await
                .map_err(RpcError::transport)?;

            let recipient_topic = recipient.into_word();

            // And every term of it is checked again on what comes back, because the
            // hash this returns is the one a licence is claimed against. An endpoint
            // that honours `address` and degrades on `topics`, which is a real
            // shape under rate limiting, would otherwise answer with any transfer
            // this contract emitted: a resale, or a stranger's mint. The arity is
            // part of that check rather than beside it, since ERC-20 shares this
            // topic0 and differs only in how many topics it carries.
            for log in logs {
                let topics = log.topics();
                if log.address() != contract
                    || topics.len() != 4
                    || topics[0] != ERC721_TRANSFER_SIG
                    || topics[1] != B256::ZERO
                    || topics[2] != recipient_topic
                {
                    continue;
                }
                if let Some(hash) = log.transaction_hash {
                    return Ok(Some(format!("0x{}", hex::encode(hash.as_slice()))));
                }
            }
            Ok(None)
        },
        WATCH_REQUEST_TIMEOUT,
    )
}

/// Watches for the `activate(tokenId)` the user sent from their wallet, and
/// returns its transaction hash.
///
/// Polls `lastActivationBlock(tokenId)`. While it sits at or below
/// `from_block`, which is the head when the screen was opened, nothing has
/// happened; the moment it passes, the activation is in that block and that one
/// block is searched for the transaction that did it.
///
/// **Why not the receipt scan §5.1a specifies.** The section asks for
/// `eth_getBlockByNumber` plus a scan of the block's receipts, picking the one
/// whose `to` is the contract and whose `from` is the wallet. That was abandoned
/// during the build, for one blocking reason and one cost:
///
///   * **Blocking.** Reading `to` without a receipt means asking for the block
///     with its transactions inlined, and a Base block carries the OP-stack
///     deposit transaction, whose `0x7e` type this provider's Ethereum
///     transaction types refuse to decode. The workspace has no `op-alloy`
///     dependency, so the block cannot be deserialized at all and the scan never
///     gets as far as reading a `to`.
///   * **Cost.** Falling back to one `eth_getTransactionReceipt` per
///     transaction, a Base block holds hundreds, so the poll that finally sees
///     the activation would fire hundreds of sequential requests at the user's
///     endpoint, and a single failure among them would end the poll and start
///     the whole scan over.
///
/// What is used instead is the `Activated(uint256 indexed tokenId, ...)` log the
/// contract already emits, fetched with one `eth_getLogs` pinned to the single
/// block `lastActivationBlock` named - the same shape [`watch_for_mint`] uses,
/// and one request on any chain.
///
/// **What that log carries in place of the two receipt fields.** The specified
/// `from == wallet` half has no counterpart in the signature §5.1a gives this
/// function, which carries no wallet; the indexed token id it does carry is the
/// sharper discriminator, naming the token this screen is waiting on rather than
/// the account that paid for the gas. The `to == contract` half is not dropped
/// but moved: the filter pins `address == contract`, so a match is an event this
/// contract emitted, which is a stronger statement than a transaction merely
/// addressed to it. A reverted activation emits no log, so the revert check
/// comes for free.
#[cfg(feature = "onchain-write")]
pub fn watch_for_activate(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
    from_block: u64,
    deadline: Deadline,
) -> Result<String, RpcError> {
    watch(
        || poll_for_activate(rpc_url, contract, token_id, from_block),
        &deadline,
        WATCH_POLL_INTERVAL,
    )
}

/// One `lastActivationBlock` read, plus the one-block log query when it has
/// moved. `Ok(None)` means no activation yet, or one whose transaction could
/// not be pinned down - a reorg between the two reads, which the next poll
/// resolves.
#[cfg(feature = "onchain-write")]
fn poll_for_activate(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
    from_block: u64,
) -> Result<Option<String>, RpcError> {
    use alloy::rpc::types::Filter;
    use alloy::sol_types::SolEvent;

    block_on_within(
        async move {
            let provider = build_provider(rpc_url)?;
            let instance = IRub3License::new(contract, &provider);

            // Classified rather than blanket-labelled a contract error, unlike
            // `last_activation_block` beside it. Inside a watch the distinction is
            // load-bearing: a rate limit or a 502 has to be absorbed and retried,
            // and reading one as a revert would end the watch on the first hiccup
            // and send the user to the manual tab for nothing.
            let last = instance
                .lastActivationBlock(U256::from(token_id))
                .call()
                .await
                .map_err(|e| classify_call_error(&e))?
                .to::<u64>();
            if last <= from_block {
                return Ok(None);
            }

            let token_topic: B256 = U256::from(token_id).into();

            // Pinned to the one block `lastActivationBlock` named, so the query
            // stays a single bounded request whatever the node's range limits are.
            let filter = Filter::new()
                .address(contract)
                .from_block(last)
                .to_block(last)
                .event_signature(IRub3License::Activated::SIGNATURE_HASH)
                .topic1(token_topic);

            let logs = provider
                .get_logs(&filter)
                .await
                .map_err(RpcError::transport)?;

            let mut found = None;

            // Checked again on the way back, term for term, for the same reason
            // `poll_for_mint` rechecks its own filter: what comes back decides
            // which transaction a session is issued against, and a filter honoured
            // loosely would hand this screen somebody else's activation.
            for log in logs {
                let topics = log.topics();
                if log.address() != contract
                    || topics.len() != 3
                    || topics[0] != IRub3License::Activated::SIGNATURE_HASH
                    || topics[1] != token_topic
                {
                    continue;
                }
                // Last one wins: if a block somehow holds two activations of this
                // token, the one that left `lastActivationBlock` where it is now is
                // the later of them.
                if let Some(hash) = log.transaction_hash {
                    found = Some(format!("0x{}", hex::encode(hash.as_slice())));
                }
            }

            Ok(found)
        },
        WATCH_REQUEST_TIMEOUT,
    )
}

// ── The code registry (§2.9) ──────────────────────────────────────────────────

/// One byte range a comparator zeroes before hashing, as the code registry
/// publishes it.
///
/// A plain pair rather than the comparator's own range type: this module reads
/// the chain and owns none of the attestation policy, and the two must not grow
/// a dependency on each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeRange {
    pub start: u32,
    pub length: u32,
}

/// A release record as `Rub3CodeRegistry.record(bytes32)` returns it, reduced to
/// the fields a wrapper acts on.
///
/// `status` and `role` are the raw `uint8`s the enums encode as. They are not
/// interpreted here: what a status or a role *means* is a policy question, and
/// this module's job ends at "here is what the chain said". An unrecognised
/// value therefore reaches the caller intact rather than being flattened into
/// something plausible on the way.
///
/// The record's `sourceCommit` and `solcVersion` are decoded and dropped. They
/// exist so a human can reproduce the fingerprint from a checkout, which is
/// work no wrapper does; carrying them further would put two more strings on a
/// refusal path that has nothing to do with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRegistryRecord {
    /// `0` unknown, `1` active, `2` deprecated.
    pub status: u8,
    /// `0` licence, `1` factory, `2` deployer helper, `3` code registry,
    /// `4` discovery registry.
    pub role: u8,
    /// Solidity contract name.
    pub contract_name: String,
    /// Human-readable release label.
    pub version: String,
    /// The block the record was published in.
    pub registered_at_block: u64,
    /// The ranges this release declares as its immutables.
    pub offsets: Vec<CodeRange>,
}

// The slice of `Rub3CodeRegistry` a wrapper reads. Both are `view`, and neither
// has a write counterpart here: publishing is the registry owner's, and nothing
// in this crate has any business calling it.
//
// The struct mirrors `Rub3CodeRegistry.Release` field for field and in order,
// because that order is the ABI encoding. Its two enums are declared as the
// `uint8` they encode as, so this mirror stays honest about what arrives on the
// wire rather than asserting a meaning at the decoder.
sol! {
    #[sol(rpc)]
    interface IRub3CodeRegistry {
        struct ByteRange {
            uint32 start;
            uint32 length;
        }

        struct Release {
            uint8   status;
            uint8   role;
            string  contractName;
            string  version;
            bytes32 sourceCommit;
            string  solcVersion;
            uint64  registeredAtBlock;
            ByteRange[] offsets;
        }

        function record(bytes32 maskedCodeHash) external view returns (Release memory);
        function latestOffsetTables(uint256 count)
            external
            view
            returns (ByteRange[][] memory);
    }
}

/// Reads up to `limit` of the distinct immutable-offset tables the code registry
/// publishes, newest first.
///
/// The bootstrap for a masked-hash lookup: computing the hash needs a table, and
/// finding the record needs the hash, so the candidate tables are fetched first
/// in one call. The canonical set spans four layouts today: one each for
/// `Rub3Access`, `Rub3Factory` and `Rub3Registry`, plus the empty one
/// `Rub3AccessDeployer` and `Rub3CodeRegistry` share.
///
/// The registry's own `latestOffsetTables` does the bounding, so the response
/// this pays to transfer and decode is capped by the caller's `limit` rather
/// than by how many tables the registry's owner key has published. `limit` is
/// clamped by the contract, so asking for more than exists returns what exists.
///
/// **Newest first, because a registry is only ever consulted about code newer
/// than this binary.** A lookup happens on a pinned-table miss, so spending a
/// fixed budget of candidates on the oldest layouts would make every release
/// published under a layout past that budget unreadable to this build while the
/// first releases stayed readable forever. This is reachability and latency
/// only: reading the wrong end, or the whole set, could never produce a wrong
/// verdict.
pub fn code_registry_offset_tables(
    rpc_url: &str,
    registry: Address,
    limit: usize,
) -> Result<Vec<Vec<CodeRange>>, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3CodeRegistry::new(registry, provider);
        let tables = instance
            .latestOffsetTables(U256::from(limit))
            .call()
            .await
            .map_err(RpcError::contract)?;
        Ok(tables
            .into_iter()
            .map(|table| {
                table
                    .into_iter()
                    .map(|r| CodeRange {
                        start: r.start,
                        length: r.length,
                    })
                    .collect()
            })
            .collect())
    })
}

/// Reads the release the code registry published for `masked_code_hash`, or
/// `None` when it has none.
///
/// A registry that has never seen a hash answers with a zeroed record, whose
/// `status` is the `Unknown` variant. That is translated to `None` here so a
/// caller cannot mistake the zero value for a published record - which would
/// read as role `0`, a licence, and is exactly the confusion worth removing at
/// the boundary.
pub fn code_registry_record(
    rpc_url: &str,
    registry: Address,
    masked_code_hash: [u8; 32],
) -> Result<Option<CodeRegistryRecord>, RpcError> {
    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let instance = IRub3CodeRegistry::new(registry, provider);
        let release = instance
            .record(B256::from(masked_code_hash))
            .call()
            .await
            .map_err(RpcError::contract)?;

        if release.status == 0 {
            return Ok(None);
        }
        Ok(Some(CodeRegistryRecord {
            status: release.status,
            role: release.role,
            contract_name: release.contractName,
            version: release.version,
            registered_at_block: release.registeredAtBlock,
            offsets: release
                .offsets
                .into_iter()
                .map(|r| CodeRange {
                    start: r.start,
                    length: r.length,
                })
                .collect(),
        }))
    })
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
            .map_err(RpcError::contract)
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
        | ContractError::UnknownSelector(_) => RpcError::contract(e),
        ContractError::TransportError(JsonRpcError::ErrorResp(payload)) => {
            // Geth and reth answer a reverted `eth_call` with code 3; anvil and
            // several hosted nodes use -32000 and say so in the message. Revert
            // data present at all is conclusive on its own.
            let reverted = payload.code == 3
                || payload.as_revert_data().is_some()
                || payload.message.to_ascii_lowercase().contains("revert");
            if reverted {
                RpcError::contract(e)
            } else {
                RpcError::transport(e)
            }
        }
        _ => RpcError::transport(e),
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
            .map_err(RpcError::transport)?
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
    let url: url::Url = rpc_url.parse().map_err(RpcError::transport)?;
    Ok(ProviderBuilder::new().connect_http(url))
}

/// Runs a request to completion, or gives up on it after `limit`.
///
/// The bound is on the whole future rather than on a client's read timeout, so
/// it covers name resolution and the connect as well as the answer: all three
/// are places a black-holing endpoint parks a caller, and only the last of them
/// a read timeout would reach.
///
/// Reported as a transport failure, which is what it is, and which the watch
/// loop already knows to absorb a few of and then give up on. The message
/// carries no URL, so nothing has to be redacted out of it.
#[cfg(feature = "onchain-write")]
fn block_on_within<T>(
    f: impl std::future::Future<Output = Result<T, RpcError>>,
    limit: std::time::Duration,
) -> Result<T, RpcError> {
    block_on(async move {
        match tokio::time::timeout(limit, f).await {
            Ok(answered) => answered,
            Err(_) => Err(RpcError::Transport(format!(
                "the endpoint did not answer within {}s",
                limit.as_secs()
            ))),
        }
    })
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

    /// A provider key lives in one of three places in an endpoint URL, and an
    /// error built from the request carries whichever one it is. All three have
    /// to be gone by the time the error value exists, because from there it is
    /// printed by the agent door, by `show_purchase`'s stderr line and by the
    /// window's error box, none of which can put back what construction did not
    /// strip.
    #[test]
    fn a_key_in_the_endpoint_url_never_survives_into_the_error() {
        const KEY: &str = "9f3c1d7ab24e4a1e8c05f6d2b7e19a44";
        const HOST: &str = "base-mainnet.example-provider.io";

        for url in [
            format!("https://{HOST}/v2/{KEY}"),
            format!("https://{HOST}/rpc?apiKey={KEY}"),
            format!("https://{KEY}@{HOST}/rpc"),
            format!("https://apikey:{KEY}@{HOST}/rpc"),
        ] {
            let message = format!("error sending request for url ({url}): dispatch failure");
            for rendered in [
                RpcError::transport(&message).to_string(),
                RpcError::contract(&message).to_string(),
            ] {
                assert!(
                    !rendered.contains(KEY),
                    "the key survived into {rendered:?} (from {url})"
                );
                assert!(
                    rendered.contains(HOST),
                    "the host is not the secret and an operator needs it: {rendered:?}"
                );
                assert!(
                    rendered.contains("dispatch failure"),
                    "redaction ate the failure itself: {rendered:?}"
                );
            }
        }
    }

    /// Redaction is a rewrite of the address, not a truncation of the message:
    /// the port stays because it identifies the endpoint, and prose on both
    /// sides of the URL has to come through intact or the error stops being
    /// readable.
    #[test]
    fn redaction_keeps_the_origin_and_the_words_around_it() {
        assert_eq!(
            redact_urls("error sending request for url (http://127.0.0.1:8547/v2/k?t=k): refused"),
            "error sending request for url (http://127.0.0.1:8547): refused"
        );
        assert_eq!(
            redact_urls("could not reach https://node.example.com/rpc/secret."),
            "could not reach https://node.example.com."
        );
        assert_eq!(
            redact_urls("relative URL without a base"),
            "relative URL without a base"
        );
        assert_eq!(
            redact_urls("the scheme :// on its own is prose"),
            "the scheme :// on its own is prose"
        );
    }

    /// An IPv6 endpoint is bracketed, and the brackets are part of the host.
    /// Reading them as delimiters used to cut the token to `scheme://`, which
    /// failed to parse, so the placeholder was emitted and then the whole
    /// authority, path and query were appended verbatim as prose - destroying
    /// the host it meant to keep while publishing the key it meant to strip.
    #[test]
    fn a_bracketed_ipv6_endpoint_keeps_its_host_and_loses_its_key() {
        const KEY: &str = "9f3c1d7ab24e4a1e8c05f6d2b7e19a44";

        assert_eq!(
            redact_urls(&format!(
                "error sending request for url (http://[::1]:8545/v2/{KEY})"
            )),
            "error sending request for url (http://[::1]:8545)"
        );
        assert_eq!(
            redact_urls(&format!(
                "could not reach https://[2001:db8::ff00:42:8329]/rpc?apiKey={KEY}"
            )),
            "could not reach https://[2001:db8::ff00:42:8329]"
        );
        // The trailing-punctuation loop is what could not advance when the
        // token was cut to `scheme://`, so it gets a bracketed case of its own.
        assert_eq!(
            redact_urls(&format!(
                "node http://[::1]:8545/v2/{KEY}, and then nothing."
            )),
            "node http://[::1]:8545, and then nothing."
        );

        for message in [
            format!("http://[::1]:8545/v2/{KEY}"),
            format!("https://[2001:db8::1]:443/rpc?apiKey={KEY}"),
            format!("http://user:{KEY}@[::1]:8545/rpc"),
        ] {
            let rendered = redact_urls(&message);
            assert!(!rendered.contains(KEY), "the key survived: {rendered}");
        }
    }

    /// A URL whose authority cannot be read must fail closed. Emitting the
    /// placeholder and then letting the unparsed tail through as prose is the
    /// one outcome worse than no redaction at all: it prints the key while
    /// claiming to have removed it.
    #[test]
    fn an_unreadable_url_is_dropped_whole_rather_than_half_printed() {
        const KEY: &str = "8b21e5c0f7a94d63b0e2417cf5da9e38";

        for message in [
            // An opening bracket with no closing one: no authority to read.
            format!("failed: http://[::1:8545/v2/{KEY} and stopped"),
            // No host at all, so nothing survives redaction.
            format!("failed: file:///var/keys/{KEY} and stopped"),
        ] {
            let rendered = redact_urls(&message);
            assert!(!rendered.contains(KEY), "the key survived: {rendered}");
            assert!(
                rendered.contains(UNREADABLE_URL),
                "an unreadable address must say so: {rendered}"
            );
            assert!(
                rendered.ends_with("and stopped"),
                "only the address is dropped, not the rest of the message: {rendered}"
            );
        }
    }

    /// The window's "ownership check failed" box is the error surface every
    /// buyer meets first, and alloy reports an unreachable node during that
    /// `eth_call` as a *contract* error rather than a transport one. Driven
    /// through the real call so the classification cannot be assumed.
    #[test]
    fn an_unreachable_node_leaks_no_key_through_the_ownership_check() {
        const KEY: &str = "0d41a9b7c8e24f6db3157ae9c2f80b16";
        // Port 1 is reserved and never listening, so the connection is refused
        // rather than left to time out.
        let url = format!("http://127.0.0.1:1/v2/{KEY}?apiKey={KEY}");

        let err = tokens_of_owner(&url, Address::ZERO, Address::ZERO).unwrap_err();

        let rendered = err.to_string();
        assert!(
            !rendered.contains(KEY),
            "the key reached the window: {rendered}"
        );
        assert!(
            rendered.contains("127.0.0.1:1"),
            "the endpoint that failed still has to be nameable: {rendered}"
        );
    }

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
    /// not a Transport error. Requires network access - skipped in offline CI.
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

    // ── Seats (§3.4) ─────────────────────────────────────────────────────────

    #[test]
    fn encode_release_calldata_matches_selector() {
        use alloy::sol_types::SolCall;
        let data = encode_release_calldata(7, 12);
        let selector = hex::encode(IRub3License::releaseCall::SELECTOR);
        assert!(data.starts_with(&format!("0x{selector}")), "got {data}");
        // selector (4) + two 32-byte arguments = 68 bytes.
        assert_eq!(data.len(), 2 + 136);
        assert!(
            data.ends_with("000000000000000000000000000000000000000000000000000000000000000c"),
            "the session id is the second argument: {data}"
        );
    }

    #[test]
    fn encode_release_calldata_differs_by_session_id() {
        assert_ne!(encode_release_calldata(1, 1), encode_release_calldata(1, 2));
    }

    /// Builds the `Activated` log a real `activate()` emits, from the ABI
    /// mirror rather than from a hand-written topic - the same reason
    /// `watch_for_activate` takes its topic0 from the `sol!` block.
    fn activated_receipt_log(
        emitter: Address,
        token_id: u64,
        session_id: u64,
        seat_index: u64,
        expires_at: u64,
    ) -> ReceiptLog {
        use alloy::sol_types::{SolEvent, SolValue};
        let owner: Address = SAMPLE_CONTRACT.parse().unwrap();
        ReceiptLog {
            address: emitter,
            topics: vec![
                IRub3License::Activated::SIGNATURE_HASH,
                B256::from(U256::from(token_id)),
                B256::from(U256::from_be_slice(owner.as_slice())),
            ],
            data: (
                U256::from(session_id),
                U256::from(seat_index),
                U256::from(expires_at),
            )
                .abi_encode_sequence(),
        }
    }

    fn receipt_with(logs: Vec<ReceiptLog>) -> TxReceipt {
        TxReceipt {
            status: true,
            block_number: 100,
            block_hash: "0xblock".into(),
            to: None,
            logs,
        }
    }

    /// The seat an activation got comes from that transaction's own log. This
    /// is the read a fleet coming up makes correct: several `activate()` calls
    /// land in one block, and a state read afterwards cannot say which was
    /// yours.
    #[test]
    fn activation_from_receipt_decodes_the_seat_the_tx_took() {
        let contract: Address = SAMPLE_CONTRACT.parse().unwrap();
        let receipt = receipt_with(vec![activated_receipt_log(
            contract,
            4,
            91,
            3,
            1_700_000_000,
        )]);

        let record = activation_from_receipt(&receipt, contract, 4).expect("decodes");
        assert_eq!(record.session_id, 91);
        assert_eq!(record.seat_index, 3);
        assert_eq!(record.seat_expires_at, 1_700_000_000);
    }

    /// The fleet case, held explicitly: one receipt carrying several tokens'
    /// activations must answer with the one asked for.
    #[test]
    fn activation_from_receipt_picks_the_log_for_this_token() {
        let contract: Address = SAMPLE_CONTRACT.parse().unwrap();
        let receipt = receipt_with(vec![
            activated_receipt_log(contract, 1, 10, 0, 111),
            activated_receipt_log(contract, 2, 11, 1, 222),
        ]);

        assert_eq!(
            activation_from_receipt(&receipt, contract, 2)
                .expect("decodes")
                .session_id,
            11
        );
    }

    /// A log from somewhere else is not this contract's activation, whatever it
    /// looks like. Without the address check any contract could hand a wrapper
    /// a session id to sign over.
    #[test]
    fn activation_from_receipt_ignores_another_contracts_log() {
        let contract: Address = SAMPLE_CONTRACT.parse().unwrap();
        let impostor: Address = "0x000000000000000000000000000000000000dead"
            .parse()
            .unwrap();
        let receipt = receipt_with(vec![activated_receipt_log(impostor, 4, 91, 0, 1)]);

        assert!(activation_from_receipt(&receipt, contract, 4).is_err());
    }

    #[test]
    fn activation_from_receipt_without_the_log_is_an_error() {
        let contract: Address = SAMPLE_CONTRACT.parse().unwrap();
        assert!(activation_from_receipt(&receipt_with(Vec::new()), contract, 4).is_err());
    }

    /// The distinction the whole seat-aware path turns on, and why it is a
    /// field rather than a comparison of the two counts.
    #[test]
    fn fleet_exhaustion_is_the_contracts_answer_not_a_count_comparison() {
        let full = ActivationStatus {
            ready: false,
            fleet_exhausted: true,
            seat_index: 0,
            seats_in_use: 4,
            seats: 4,
            blocks_remaining: 0,
            seconds_remaining: 900,
        };
        assert!(full.fleet_exhausted);

        // A single-seat licence with its one seat live: the counts read as a
        // full fleet and it is not one, because the holder may retake their own
        // seat. Taking the flag from the contract rather than deriving it from
        // the counts is what makes this case right.
        let sole_holder = ActivationStatus {
            fleet_exhausted: false,
            seats_in_use: 1,
            seats: 1,
            blocks_remaining: 12,
            seconds_remaining: 900,
            ..full
        };
        assert!(!sole_holder.fleet_exhausted);
        assert_eq!(sole_holder.seats_in_use, sole_holder.seats);
    }

    /// The two refusals are measured in different units, so one place does the
    /// arithmetic. A caller that got the unit wrong would arm a watch for a
    /// transaction the chain is guaranteed not to see.
    #[test]
    #[cfg(feature = "onchain-write")]
    fn wait_before_activate_reads_the_unit_that_applies() {
        let block_wait = |blocks: u64| std::time::Duration::from_secs(blocks * 2);

        let full = ActivationStatus {
            ready: false,
            fleet_exhausted: true,
            seat_index: 0,
            seats_in_use: 4,
            seats: 4,
            blocks_remaining: 0,
            seconds_remaining: 900,
        };
        assert_eq!(
            full.wait_before_activate(block_wait),
            std::time::Duration::from_secs(900),
            "a full fleet waits out a lapse, which is already in seconds"
        );

        let cooling = ActivationStatus {
            fleet_exhausted: false,
            seats_in_use: 1,
            blocks_remaining: 30,
            seconds_remaining: 0,
            ..full
        };
        assert_eq!(
            cooling.wait_before_activate(block_wait),
            std::time::Duration::from_secs(60),
            "a cooldown is in blocks and is converted"
        );

        let ready = ActivationStatus {
            ready: true,
            fleet_exhausted: false,
            blocks_remaining: 30,
            seconds_remaining: 900,
            ..full
        };
        assert_eq!(
            ready.wait_before_activate(block_wait),
            std::time::Duration::ZERO,
            "nothing to wait for, whatever the other numbers say"
        );
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
                logs: Vec::new(),
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

// ── Auto-detect watch tests (§5.1a) ───────────────────────────────────────────

/// The watch loop, its budget, and its cancellation, driven without a node.
///
/// The loop is the part that has to be right whatever the chain is doing: it
/// decides how long a person waits, when a bad endpoint stops being retried,
/// and whether a thread stops when the screen that started it does. Injecting
/// the poll is what makes all three assertable in milliseconds - the same
/// arrangement `poll_for_receipt` has, and for the same reason.
#[cfg(all(test, feature = "onchain-write"))]
mod watch_loop_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    const TICK: Duration = Duration::from_millis(20);
    const HASH: &str = "0xfeed";

    /// The loop under test, pinned to the `String` a real watch returns.
    ///
    /// `watch` is generic over what a poll finds, because `retry_read` reuses it
    /// for the tuple `cooldownReady` answers with. A test whose poll never
    /// matches never names that type, so it is named once here.
    fn watch_hashes<F>(poll: F, deadline: &Deadline, interval: Duration) -> Result<String, RpcError>
    where
        F: FnMut() -> Result<Option<String>, RpcError>,
    {
        watch(poll, deadline, interval)
    }

    /// A budget long enough that only the assertion under test can end a watch.
    fn generous() -> Deadline {
        Deadline::after(Duration::from_secs(30))
    }

    fn end_of(e: &RpcError) -> WatchEnd {
        match e {
            RpcError::WatchEnded(end) => *end,
            other => panic!("expected the watch to end, got {other}"),
        }
    }

    #[test]
    fn a_match_on_the_first_poll_is_returned_at_once() {
        let hash = watch(|| Ok(Some(HASH.to_string())), &generous(), TICK)
            .expect("a match must be returned");
        assert_eq!(hash, HASH);
    }

    /// The normal shape of a watch: the transaction is not there, and then it
    /// is. Nothing about the earlier empty answers may change the result.
    #[test]
    fn a_match_after_several_empty_polls_is_still_returned() {
        let polls = AtomicU32::new(0);
        let hash = watch_hashes(
            || {
                if polls.fetch_add(1, Ordering::Relaxed) < 3 {
                    Ok(None)
                } else {
                    Ok(Some(HASH.to_string()))
                }
            },
            &generous(),
            TICK,
        )
        .expect("a match must be returned");
        assert_eq!(hash, HASH);
        assert_eq!(polls.load(Ordering::Relaxed), 4);
    }

    /// The budget runs out and the transaction never appeared. This is the
    /// common ending, not a failure: the window falls back to the manual paste
    /// and says so, which is why it must be distinguishable from an endpoint
    /// that stopped answering.
    #[test]
    fn an_exhausted_budget_ends_the_watch_as_a_timeout() {
        let deadline = Deadline::after(Duration::from_millis(120));
        let err = watch_hashes(|| Ok(None), &deadline, TICK).expect_err("the budget must run out");
        assert_eq!(end_of(&err), WatchEnd::Timeout);
        assert!(
            err.is_retryable(),
            "a timeout says the chain had not answered yet, which is worth asking again",
        );
    }

    /// A cancelled watch stops, and stops promptly.
    ///
    /// Promptness is the assertion that matters. A watch that only noticed
    /// cancellation at the end of its poll interval would leave a request in
    /// flight against an endpoint nobody is waiting on, and the interval in
    /// production is three seconds.
    #[test]
    fn a_cancelled_watch_stops_inside_one_poll_interval() {
        let cancel = Cancel::new();
        let deadline = Deadline::after(Duration::from_secs(30)).cancelled_by(cancel.clone());

        let polls = AtomicU32::new(0);
        let started = Instant::now();
        let interval = Duration::from_secs(5);
        let err = watch_hashes(
            || {
                // Cancelled from inside the first sleep, which is where a real
                // one arrives: the IPC handler raises the flag while the watch
                // is parked between polls.
                if polls.fetch_add(1, Ordering::Relaxed) == 0 {
                    let flag = cancel.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(50));
                        flag.cancel();
                    });
                }
                Ok(None)
            },
            &deadline,
            interval,
        )
        .expect_err("a cancelled watch must not return a hash");

        assert_eq!(end_of(&err), WatchEnd::Cancelled);
        assert!(
            started.elapsed() < interval,
            "cancellation waited out the poll interval ({:?})",
            started.elapsed(),
        );
        assert!(
            !err.is_retryable(),
            "cancellation is a decision, not a condition to retry",
        );
        assert_eq!(
            polls.load(Ordering::Relaxed),
            1,
            "a cancelled watch must not poll again",
        );
    }

    /// One bad response inside a two-minute watch is noise. It must not end
    /// the watch, and it must not be reported once the chain answers again.
    #[test]
    fn a_transient_failure_is_absorbed_and_the_watch_goes_on() {
        let polls = AtomicU32::new(0);
        let hash = watch_hashes(
            || match polls.fetch_add(1, Ordering::Relaxed) {
                0 | 2 => Err(RpcError::Transport("502 bad gateway".into())),
                1 => Ok(None),
                _ => Ok(Some(HASH.to_string())),
            },
            &generous(),
            TICK,
        )
        .expect("transient failures must not end the watch");
        assert_eq!(hash, HASH);
    }

    /// A sustained run of failures is not noise: it is an endpoint that will
    /// not answer, and spending the rest of the budget on it only delays the
    /// manual paste that would have worked.
    #[test]
    fn a_dead_endpoint_ends_the_watch_before_the_budget_does() {
        let polls = AtomicU32::new(0);
        // Long enough that reaching it would take a thousand ticks: if this
        // test ends on the budget rather than on the error count, it hangs
        // rather than passing by accident.
        let deadline = Deadline::after(Duration::from_secs(30));
        let err = watch_hashes(
            || {
                polls.fetch_add(1, Ordering::Relaxed);
                Err(RpcError::Transport("connection refused".into()))
            },
            &deadline,
            TICK,
        )
        .expect_err("a dead endpoint must end the watch");

        assert!(
            matches!(err, RpcError::Transport(_)),
            "the endpoint's own failure is what the screen reports, got {err}",
        );
        assert_eq!(
            polls.load(Ordering::Relaxed),
            WATCH_MAX_CONSECUTIVE_ERRORS,
            "the watch must give up on the failure that crossed the threshold",
        );
    }

    /// A run of failures broken by a single good answer starts over. Otherwise
    /// a flaky endpoint would accumulate its way to a give-up across a whole
    /// two minutes of otherwise working polls.
    #[test]
    fn a_single_good_answer_resets_the_failure_run() {
        let polls = AtomicU32::new(0);
        let total = WATCH_MAX_CONSECUTIVE_ERRORS * 2;
        let hash = watch_hashes(
            || {
                let n = polls.fetch_add(1, Ordering::Relaxed);
                if n >= total {
                    Ok(Some(HASH.to_string()))
                } else if n.is_multiple_of(2) {
                    Err(RpcError::Transport("rate limited".into()))
                } else {
                    Ok(None)
                }
            },
            &generous(),
            TICK,
        )
        .expect("alternating failures must never reach the threshold");
        assert_eq!(hash, HASH);
    }

    /// A malformed request will not parse on the tenth try either, so it is
    /// reported at once rather than retried into the budget.
    #[test]
    fn an_unretryable_failure_ends_the_watch_immediately() {
        let polls = AtomicU32::new(0);
        let err = watch_hashes(
            || {
                polls.fetch_add(1, Ordering::Relaxed);
                Err(RpcError::InvalidInput("not an address".into()))
            },
            &generous(),
            TICK,
        )
        .expect_err("a settled failure must end the watch");
        assert!(matches!(err, RpcError::InvalidInput(_)), "got {err}");
        assert_eq!(polls.load(Ordering::Relaxed), 1);
    }

    /// A watch already cancelled before it starts never touches the network.
    /// The window can cancel between spawning a thread and the thread running.
    #[test]
    fn a_watch_cancelled_before_it_starts_never_polls() {
        let cancel = Cancel::new();
        cancel.cancel();
        let deadline = Deadline::after(Duration::from_secs(30)).cancelled_by(cancel);

        let polls = AtomicU32::new(0);
        let err = watch_hashes(
            || {
                polls.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            },
            &deadline,
            TICK,
        )
        .expect_err("a cancelled watch must not return a hash");
        assert_eq!(end_of(&err), WatchEnd::Cancelled);
        assert_eq!(polls.load(Ordering::Relaxed), 0);
    }

    /// A held watch asks nothing until the hold is over, and then gets its whole
    /// budget.
    ///
    /// Both halves matter and only together. Without the first, the cooldown
    /// screen polls an endpoint for an hour for a transaction the contract
    /// refuses to accept. Without the second, the hold would be spent out of the
    /// budget and the watch would give up at the very moment it became able to
    /// see anything.
    #[test]
    fn a_held_watch_polls_nothing_until_the_hold_is_over() {
        const HOLD: Duration = Duration::from_millis(300);

        let deadline = Deadline::after(Duration::from_millis(200)).starting_in(HOLD);
        let polls = AtomicU32::new(0);
        let started = Instant::now();
        let hash = watch_hashes(
            || {
                polls.fetch_add(1, Ordering::Relaxed);
                Ok(Some(HASH.to_string()))
            },
            &deadline,
            TICK,
        )
        .expect("the budget must start when the hold ends, not before it");

        assert_eq!(hash, HASH);
        assert_eq!(polls.load(Ordering::Relaxed), 1);
        assert!(
            started.elapsed() >= HOLD,
            "the watch polled inside its hold ({:?})",
            started.elapsed(),
        );
    }

    /// A hold the clock cannot represent falls back to no hold, rather than
    /// killing the thread that was going to take it.
    ///
    /// The delay comes from `cooldownReady`, a `uint256` no part of the wrapper
    /// bounds, and `Instant + Duration` panics on overflow. On the watcher
    /// thread that panic is silent: `auto_watch_ended` never runs, the page is
    /// never told, and it spins on the auto-detect tab with no way back to the
    /// manual paste. Giving up the hold is the safe degradation - the watch
    /// still runs, still times out, and still hands back.
    #[test]
    fn an_unrepresentable_hold_does_not_kill_the_watch() {
        let deadline = Deadline::after(Duration::from_millis(120)).starting_in(Duration::MAX);

        let err = watch_hashes(|| Ok(None), &deadline, TICK)
            .expect_err("a watch that finds nothing must end, not hang");
        assert_eq!(
            end_of(&err),
            WatchEnd::Timeout,
            "the watch must reach the manual-tab fallback the ordinary way",
        );
    }

    /// Cancellation reaches a watch that is holding, as promptly as it reaches
    /// one that is polling.
    ///
    /// A hold is the longest a watch is ever asleep, so it is also the longest a
    /// leaked thread would go unnoticed. A screen the user has left must stop it
    /// there too.
    #[test]
    fn a_cancelled_watch_stops_inside_its_hold() {
        let cancel = Cancel::new();
        let deadline = Deadline::after(Duration::from_secs(30))
            .starting_in(Duration::from_secs(30))
            .cancelled_by(cancel.clone());

        let polls = AtomicU32::new(0);
        let started = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        let err = watch_hashes(
            || {
                polls.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            },
            &deadline,
            TICK,
        )
        .expect_err("a cancelled watch must not return a hash");

        assert_eq!(end_of(&err), WatchEnd::Cancelled);
        assert_eq!(
            polls.load(Ordering::Relaxed),
            0,
            "a watch cancelled while holding never had anything to ask",
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancellation waited out the hold ({:?})",
            started.elapsed(),
        );
    }
}

/// The two watchers against a mocked node: what they ask for, and what they
/// make of the answer.
///
/// The loop above is exercised with the network taken out; this is the other
/// half - the filter that goes out and the decoding that comes back, which is
/// where a wrong topic or a mis-read receipt would live.
#[cfg(all(test, feature = "onchain-write"))]
mod watch_rpc_tests {
    use super::*;
    use crate::test_support::StubNode;
    use std::time::Duration;

    const CONTRACT: &str = "0x000000000000000000000000000000000000dEaD";
    const BUYER: &str = "0x00000000000000000000000000000000000B0B0b";
    const MINT_TX: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
    const ACTIVATE_TX: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";
    const OTHER_TX: &str = "0x3333333333333333333333333333333333333333333333333333333333333333";
    const TOKEN_ID: u64 = 137;
    /// Somebody who is not the buyer this screen belongs to.
    const STRANGER: &str = "0x00000000000000000000000000000000000Beef0";
    /// A contract that is not the licence this screen belongs to.
    const OTHER_CONTRACT: &str = "0x000000000000000000000000000000000000BEEF";
    const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

    fn contract() -> Address {
        CONTRACT.parse().expect("test contract address")
    }

    fn buyer() -> Address {
        BUYER.parse().expect("test buyer address")
    }

    /// A hex topic for a 20-byte address, left-padded the way a log carries it.
    fn address_topic(address: &str) -> String {
        format!("0x{:0>64}", address.trim_start_matches("0x").to_lowercase())
    }

    /// A budget short enough that a test asserting "nothing was found" ends in
    /// well under a second.
    fn brief() -> Deadline {
        Deadline::after(Duration::from_millis(150))
    }

    fn end_of(e: &RpcError) -> WatchEnd {
        match e {
            RpcError::WatchEnded(end) => *end,
            other => panic!("expected the watch to end, got {other}"),
        }
    }

    /// One ERC-721 mint log, as a node returns it.
    fn mint_log(to: &str, tx: &str) -> serde_json::Value {
        transfer_log(CONTRACT, ZERO_ADDRESS, to, tx)
    }

    /// One ERC-721 `Transfer`, as `emitter` emitted it. A mint is the case
    /// where `from` is the zero address; every other `from` is a transfer of a
    /// token that already existed.
    fn transfer_log(emitter: &str, from: &str, to: &str, tx: &str) -> serde_json::Value {
        event_log(
            emitter,
            &[
                format!("0x{}", hex::encode(ERC721_TRANSFER_SIG.as_slice())),
                address_topic(from),
                address_topic(to),
                format!("0x{TOKEN_ID:064x}"),
            ],
            tx,
        )
    }

    /// One log carrying `topics`, with the block fields every one of these
    /// shares.
    fn event_log(emitter: &str, topics: &[String], tx: &str) -> serde_json::Value {
        serde_json::json!({
            "address": emitter.to_lowercase(),
            "topics": topics,
            "data": "0x",
            "blockNumber": "0x2a",
            "blockHash": "0x0000000000000000000000000000000000000000000000000000000000000042",
            "transactionHash": tx,
            "transactionIndex": "0x0",
            "logIndex": "0x0",
            "removed": false,
        })
    }

    // ── watch_for_mint ───────────────────────────────────────────────────────

    /// The happy path, and the assertion that matters most: the hash the watch
    /// hands back is the one the manual paste would have carried, in the same
    /// spelling.
    #[test]
    fn a_matching_mint_log_yields_its_transaction_hash() {
        let node = StubNode::routed(|method, _params| match method {
            "eth_getLogs" => serde_json::json!([mint_log(BUYER, MINT_TX)]),
            _ => serde_json::json!(null),
        });

        let hash = watch_for_mint(&node.url, contract(), buyer(), 40, brief())
            .expect("a matching mint log must resolve");
        assert_eq!(hash, MINT_TX);
    }

    /// The filter the node is asked for is the one §5.1a specifies. Asserted on
    /// the request rather than the response because a filter that is too wide
    /// would still pass the test above while picking up a stranger's mint.
    #[test]
    fn the_filter_pins_the_contract_the_mint_and_the_recipient() {
        let seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&seen);

        let node = StubNode::routed(move |method, params| {
            if method == "eth_getLogs" {
                recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(params[0].clone());
            }
            serde_json::json!([])
        });

        let err = watch_for_mint(&node.url, contract(), buyer(), 40, brief())
            .expect_err("an empty log set means nothing has landed yet");
        assert_eq!(end_of(&err), WatchEnd::Timeout);

        let filters = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let filter = filters
            .first()
            .expect("the watch must have asked at least once");
        assert_eq!(
            filter["address"].as_str().map(str::to_lowercase),
            Some(CONTRACT.to_lowercase()),
            "an unpinned address would match any contract's transfers: {filter}",
        );
        assert_eq!(
            filter["fromBlock"].as_str(),
            Some("0x28"),
            "the watch must start at the block the screen opened at: {filter}",
        );
        let topics = &filter["topics"];
        assert_eq!(
            topics[0].as_str(),
            Some(format!("0x{}", hex::encode(ERC721_TRANSFER_SIG.as_slice())).as_str()),
            "topic0 must be the ERC-721 Transfer signature: {filter}",
        );
        assert_eq!(
            topics[1].as_str(),
            Some("0x0000000000000000000000000000000000000000000000000000000000000000"),
            "a mint is a transfer from the zero address: {filter}",
        );
        assert_eq!(
            topics[2].as_str().map(str::to_lowercase),
            Some(address_topic(BUYER)),
            "topic2 must pin the recipient, or another buyer's mint would win: {filter}",
        );
    }

    /// A node that answers the address filter and degrades on the topics is a
    /// real shape under rate limiting, and everything below is a log the
    /// licence contract could genuinely have emitted in the range. None of them
    /// is this screen's mint, and reading one as such would hand the purchase
    /// poller a stranger's transaction: it went to the right contract, so the
    /// poller accepts it, and then finds no mint to this wallet in it and drops
    /// the person on an error screen instead of the manual paste that works.
    #[test]
    fn only_this_wallets_mint_from_this_contract_is_a_mint() {
        for (what, log) in [
            (
                "a resale of an existing token to this wallet",
                transfer_log(CONTRACT, STRANGER, BUYER, OTHER_TX),
            ),
            (
                "another wallet's mint",
                transfer_log(CONTRACT, ZERO_ADDRESS, STRANGER, OTHER_TX),
            ),
            (
                "a mint from another contract",
                transfer_log(OTHER_CONTRACT, ZERO_ADDRESS, BUYER, OTHER_TX),
            ),
            (
                "a four-topic event that is not a Transfer at all",
                event_log(
                    CONTRACT,
                    &[
                        format!("0x{:064x}", 9),
                        address_topic(ZERO_ADDRESS),
                        address_topic(BUYER),
                        format!("0x{TOKEN_ID:064x}"),
                    ],
                    OTHER_TX,
                ),
            ),
        ] {
            let node = StubNode::routed(move |method, _params| match method {
                "eth_getLogs" => serde_json::json!([log.clone()]),
                _ => serde_json::json!(null),
            });

            let err = watch_for_mint(&node.url, contract(), buyer(), 40, brief())
                .expect_err("only this wallet's mint resolves");
            assert_eq!(
                end_of(&err),
                WatchEnd::Timeout,
                "{what} must not be read as this screen's mint",
            );
        }
    }

    /// And the mint is still found when a loose answer carries it alongside
    /// them, rather than the first thing in the list winning.
    #[test]
    fn the_mint_is_found_among_unrelated_transfers() {
        let node = StubNode::routed(|method, _params| match method {
            "eth_getLogs" => serde_json::json!([
                transfer_log(CONTRACT, STRANGER, BUYER, OTHER_TX),
                transfer_log(CONTRACT, ZERO_ADDRESS, STRANGER, OTHER_TX),
                mint_log(BUYER, MINT_TX),
            ]),
            _ => serde_json::json!(null),
        });

        let hash = watch_for_mint(&node.url, contract(), buyer(), 40, brief())
            .expect("the mint must be found beside unrelated traffic");
        assert_eq!(hash, MINT_TX);
    }

    /// ERC-20 shares topic0 with ERC-721 and differs only in arity, so a
    /// three-topic log from the same address must not be read as a mint. A node
    /// that ignores the topic filter is exactly how this arrives in the wild.
    #[test]
    fn a_three_topic_transfer_is_not_a_mint() {
        let node = StubNode::routed(|method, _params| match method {
            "eth_getLogs" => serde_json::json!([{
                "address": CONTRACT.to_lowercase(),
                "topics": [
                    format!("0x{}", hex::encode(ERC721_TRANSFER_SIG.as_slice())),
                    format!("0x{:0>64}", ""),
                    address_topic(BUYER),
                ],
                "data": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "blockNumber": "0x2a",
                "blockHash": "0x0000000000000000000000000000000000000000000000000000000000000042",
                "transactionHash": OTHER_TX,
                "transactionIndex": "0x0",
                "logIndex": "0x0",
                "removed": false,
            }]),
            _ => serde_json::json!(null),
        });

        let err = watch_for_mint(&node.url, contract(), buyer(), 40, brief())
            .expect_err("an ERC-20 transfer is not a licence mint");
        assert_eq!(end_of(&err), WatchEnd::Timeout);
    }

    /// A node that will not answer must surface as a failure, not as "nothing
    /// has landed yet". The loop treats the two completely differently - one is
    /// absorbed and retried, the other counts towards giving up - and the
    /// screen says different things about them, so a poll that swallowed a 502
    /// into `Ok(None)` would keep a broken endpoint spinning for the whole
    /// budget and then blame the chain for it.
    ///
    /// Asserted on the poll rather than through [`watch_for_mint`], which would
    /// have to sit out five real poll intervals to reach the same conclusion.
    #[test]
    fn a_refusing_node_makes_the_mint_poll_fail_rather_than_come_back_empty() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32005,"message":"rate limit exceeded"}}"#,
        );
        let err = poll_for_mint(&node.url, contract(), buyer(), 40)
            .expect_err("a refused request is not an empty log set");
        assert!(err.is_retryable(), "a rate limit is worth retrying: {err}");
    }

    // ── watch_for_activate ───────────────────────────────────────────────────

    /// A node whose `lastActivationBlock` sits at `last`, and whose activation
    /// block answers a log query with `logs`.
    fn activation_node(last: u64, logs: serde_json::Value) -> StubNode {
        StubNode::routed(move |method, _params| match method {
            // The only `eth_call` this flow makes.
            "eth_call" => serde_json::json!(format!("0x{last:064x}")),
            "eth_getLogs" => logs.clone(),
            _ => serde_json::json!(null),
        })
    }

    /// The `Activated(tokenId, owner, sessionId)` log the wrapper looks for, as
    /// `emitter` emitted it.
    fn activated_log(emitter: &str, token_id: u64, tx: &str) -> serde_json::Value {
        use alloy::sol_types::SolEvent;
        serde_json::json!({
            "address": emitter.to_lowercase(),
            "topics": [
                format!("0x{}", hex::encode(IRub3License::Activated::SIGNATURE_HASH.as_slice())),
                format!("0x{token_id:064x}"),
                address_topic(BUYER),
            ],
            "data": format!("0x{:064x}", 7),
            "blockNumber": "0x2a",
            "blockHash": "0x0000000000000000000000000000000000000000000000000000000000000042",
            "transactionHash": tx,
            "transactionIndex": "0x0",
            "logIndex": "0x0",
            "removed": false,
        })
    }

    /// The happy path: the activation block moves past the one the screen
    /// opened at, and the transaction that moved it is found in that block.
    #[test]
    fn an_advanced_activation_block_resolves_to_its_transaction() {
        let node = activation_node(
            42,
            serde_json::json!([activated_log(CONTRACT, TOKEN_ID, ACTIVATE_TX)]),
        );

        let hash = watch_for_activate(&node.url, contract(), TOKEN_ID, 40, brief())
            .expect("an advanced activation block must resolve");
        assert_eq!(hash, ACTIVATE_TX);
    }

    /// Resolving the activation costs one request, whatever the block holds.
    ///
    /// The receipt scan this replaced read every transaction in the block to
    /// learn its `to`, which on Base is hundreds of sequential requests inside
    /// one poll; any one of them failing ended the poll, and the next one
    /// started the scan again against an endpoint that was already rate
    /// limiting. What a poll is allowed to ask for is therefore asserted here
    /// rather than left to a comment above the function.
    #[test]
    fn resolving_the_activation_never_fans_out_over_the_block() {
        use std::sync::{Arc, Mutex};

        let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&asked);
        let node = StubNode::routed(move |method, _params| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(method.to_string());
            match method {
                "eth_call" => serde_json::json!(format!("0x{:064x}", 42)),
                "eth_getLogs" => {
                    serde_json::json!([activated_log(CONTRACT, TOKEN_ID, ACTIVATE_TX)])
                }
                _ => serde_json::json!(null),
            }
        });

        let hash = poll_for_activate(&node.url, contract(), TOKEN_ID, 40)
            .expect("the poll must reach the node")
            .expect("the activation is in the block it named");
        assert_eq!(hash, ACTIVATE_TX);

        let asked = asked.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(
            asked,
            ["eth_call", "eth_getLogs"],
            "a poll asks where the activation is, then what is in that block, \
             and nothing else",
        );
    }

    /// The block the token was last activated in is where it already sat when
    /// the screen opened - that is what a cooldown is. Reading it as a fresh
    /// activation would hand the flow a transaction from minutes ago and issue
    /// a session against it.
    #[test]
    fn an_activation_block_at_or_before_the_start_is_not_a_new_activation() {
        for last in [39, 40] {
            let node = activation_node(
                last,
                serde_json::json!([activated_log(CONTRACT, TOKEN_ID, ACTIVATE_TX)]),
            );
            let err = watch_for_activate(&node.url, contract(), TOKEN_ID, 40, brief())
                .expect_err("an activation at or before the start block is the old one");
            assert_eq!(
                end_of(&err),
                WatchEnd::Timeout,
                "lastActivationBlock={last} against a start block of 40",
            );
        }
    }

    /// Somebody else's activation of a different token must not be mistaken for
    /// this screen's. The token id is indexed on the event, which is what makes
    /// the distinction available at all.
    ///
    /// The filter that goes out already pins that topic, so a node answering
    /// this way is a node answering loosely. That is the case worth asserting:
    /// what comes back decides which transaction a session is issued against,
    /// so the wrapper checks it again rather than trusting the endpoint.
    #[test]
    fn another_tokens_activation_in_the_same_block_is_ignored() {
        let node = activation_node(
            42,
            serde_json::json!([activated_log(CONTRACT, TOKEN_ID + 1, OTHER_TX)]),
        );

        let err = watch_for_activate(&node.url, contract(), TOKEN_ID, 40, brief())
            .expect_err("another token's activation is not this one");
        assert_eq!(end_of(&err), WatchEnd::Timeout);
    }

    /// And neither is an `Activated` some other contract emitted, however
    /// loosely the endpoint reads the address the filter pins.
    #[test]
    fn an_activation_from_another_contract_is_ignored() {
        let node = activation_node(
            42,
            serde_json::json!([activated_log(
                "0x000000000000000000000000000000000000beef",
                TOKEN_ID,
                OTHER_TX
            )]),
        );

        let err = watch_for_activate(&node.url, contract(), TOKEN_ID, 40, brief())
            .expect_err("another contract's Activated is not this licence's");
        assert_eq!(end_of(&err), WatchEnd::Timeout);
    }

    /// A block with unrelated traffic in it: only the log that came from the
    /// licence contract and named this token is picked out.
    #[test]
    fn the_activation_is_found_among_unrelated_logs() {
        let node = activation_node(
            42,
            serde_json::json!([
                activated_log(
                    "0x000000000000000000000000000000000000beef",
                    TOKEN_ID,
                    OTHER_TX
                ),
                activated_log(CONTRACT, TOKEN_ID + 1, OTHER_TX),
                activated_log(CONTRACT, TOKEN_ID, ACTIVATE_TX),
            ]),
        );

        let hash = watch_for_activate(&node.url, contract(), TOKEN_ID, 40, brief())
            .expect("the activation must be found beside unrelated traffic");
        assert_eq!(hash, ACTIVATE_TX);
    }

    /// The same for the activate poll, and for the same reason.
    #[test]
    fn a_refusing_node_makes_the_activate_poll_fail_rather_than_come_back_empty() {
        let node = StubNode::serving(
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32005,"message":"rate limit exceeded"}}"#,
        );
        let err = poll_for_activate(&node.url, contract(), TOKEN_ID, 40)
            .expect_err("a refused request is not a quiet chain");
        assert!(err.is_retryable(), "a rate limit is worth retrying: {err}");
    }

    /// An endpoint that takes the request and answers nothing is given up on,
    /// rather than parking the thread that asked.
    ///
    /// This is what makes the deadline reachable at all. A watch consults it
    /// between polls, so an unbounded request is one the budget never gets to
    /// end: the spinner turns for the full two minutes, the fallback to the
    /// manual tab never fires, and the cancel flag the screen raised on its way
    /// out is never read again - which is also the one way a watcher thread can
    /// outlive its window. Accepting a connection and then answering nothing is
    /// an ordinary overload mode for a public endpoint, not a contrived one.
    ///
    /// The failure is reported as retryable, so a watch absorbs a few and gives
    /// up on a run of them, exactly as it does for a 502.
    #[test]
    fn a_request_that_is_never_answered_is_abandoned() {
        const LIMIT: Duration = Duration::from_millis(300);
        // Comfortably above the limit and far below anything an unbounded wait
        // would reach, so neither a slow machine nor a lost bound is ambiguous.
        const TOO_LONG: Duration = Duration::from_secs(3);

        let node = StubNode::hanging();

        let started = std::time::Instant::now();
        let err = get_block_number_within(&node.url, LIMIT)
            .expect_err("a request that is never answered must not return a block");

        assert!(
            started.elapsed() < TOO_LONG,
            "the request was not abandoned: it waited {:?}",
            started.elapsed(),
        );
        assert!(
            err.is_retryable(),
            "an endpoint that went quiet is worth asking again: {err}",
        );
    }
}
