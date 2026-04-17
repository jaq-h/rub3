use alloy::primitives::{Address, B256, U256};
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
    }
}

// ── Receipt ───────────────────────────────────────────────────────────────────

/// Minimal tx receipt — the fields the wrapper cares about.
#[derive(Debug, Clone)]
pub struct TxReceipt {
    pub status:       bool,
    pub block_number: u64,
    pub block_hash:   String,
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
    /// ENS resolution is not yet implemented (Phase 1.6).
    EnsNotSupported,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Transport(e) => write!(f, "transport error: {e}"),
            RpcError::Contract(e) => write!(f, "contract error: {e}"),
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

/// Returns the purchase price (in wei) from the license contract's `price()` function.
pub fn token_price(rpc_url: &str, contract: Address) -> Result<U256, RpcError> {
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
pub fn active_session_id(
    rpc_url: &str,
    contract: Address,
    token_id: u64,
) -> Result<u64, RpcError> {
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
    let call = IRub3License::activateCall { tokenId: U256::from(token_id) };
    format!("0x{}", hex::encode(call.abi_encode()))
}

/// Fetches the receipt for `tx_hash`. Returns `Ok(None)` while the tx is still
/// pending; `Ok(Some(receipt))` once mined.
pub fn get_tx_receipt(rpc_url: &str, tx_hash: &str) -> Result<Option<TxReceipt>, RpcError> {
    let hash: B256 = tx_hash
        .trim_start_matches("0x")
        .parse::<B256>()
        .map_err(|e| RpcError::Transport(format!("invalid tx hash: {e}")))?;

    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let maybe = provider
            .get_transaction_receipt(hash)
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?;

        let receipt = match maybe {
            Some(r) => r,
            None    => return Ok(None),
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
            status:       receipt.status(),
            block_number,
            block_hash,
            to,
        }))
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

// ── Internals ─────────────────────────────────────────────────────────────────

fn build_provider(
    rpc_url: &str,
) -> Result<impl alloy::providers::Provider, RpcError> {
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
    fn token_price_invalid_url_returns_transport_error() {
        let err = token_price("not-a-url", Address::ZERO).unwrap_err();
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

    #[test]
    fn get_tx_receipt_invalid_hash_returns_transport_error() {
        let err = get_tx_receipt(VALID_RPC, "not-a-hash").unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }

    #[test]
    fn get_block_number_invalid_url_returns_transport_error() {
        let err = get_block_number("not-a-url").unwrap_err();
        assert!(matches!(err, RpcError::Transport(_)));
    }
}
