use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy::sol;

// ── Contract interface ────────────────────────────────────────────────────────

// Minimal ABI surface needed for activation:
//   ownerOf(tokenId)         — ERC-721 standard
//   price()                  — rub3 license contract
//   balanceOf(owner)         — ERC-721 standard
//   tokenOfOwnerByIndex(...) — ERC-721Enumerable
sol! {
    #[sol(rpc)]
    interface IRub3License {
        function ownerOf(uint256 tokenId) external view returns (address owner);
        function price() external view returns (uint256 amount);
        function balanceOf(address owner) external view returns (uint256 balance);
        function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256 tokenId);
    }
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
}
