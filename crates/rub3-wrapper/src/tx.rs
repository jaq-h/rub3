//! Building, signing, and broadcasting transactions for headless activation.
//!
//! The interactive flow encodes calldata and hands it to a human's wallet.
//! Headless has no wallet to hand it to, so this module closes the gap: it
//! fills in nonce/gas/fees, hands the EIP-1559 sighash to a [`Signer`], and
//! pushes the signed envelope out via `eth_sendRawTransaction`.
//!
//! No key material passes through here — the only signing operation is
//! [`Signer::sign_prehash`], which a KMS or enclave backend serves without
//! releasing a key. Calldata construction stays in [`crate::rpc`]; this module
//! only wraps it in an envelope.

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, TxKind, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;

use crate::signer::{Signer, SignerError};

/// Multiplier applied to the node's `eth_estimateGas` result, in percent.
///
/// `purchase()` and `activate()` both write storage whose cost can shift
/// between the estimate and inclusion (a first-touch slot, a changed cooldown
/// slot). 25% is the usual headroom for that without materially raising the
/// balance an agent must hold.
const GAS_LIMIT_BUFFER_PCT: u64 = 125;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum TxError {
    /// URL parse, connection, or JSON-RPC transport failure.
    Rpc(String),
    /// The signing backend refused.
    Signer(SignerError),
    /// The sender cannot cover `value + gas_limit × max_fee_per_gas`.
    InsufficientFunds { required: U256, available: U256 },
    /// The node rejected the transaction (revert on estimation, nonce clash,
    /// underpriced, …).
    Rejected(String),
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxError::Rpc(e) => write!(f, "rpc error: {e}"),
            TxError::Signer(e) => write!(f, "{e}"),
            TxError::InsufficientFunds { required, available } => write!(
                f,
                "insufficient funds: need {required} wei (value + gas), wallet holds {available} wei"
            ),
            TxError::Rejected(e) => write!(f, "transaction rejected: {e}"),
        }
    }
}

impl From<SignerError> for TxError {
    fn from(e: SignerError) -> Self {
        TxError::Signer(e)
    }
}

/// Nodes report an unaffordable transaction as an ordinary JSON-RPC error, and
/// the wording differs between clients. Recognising it lets the CLI return the
/// dedicated "insufficient funds" exit code instead of a generic RPC failure.
fn classify(msg: String) -> TxError {
    if msg.to_lowercase().contains("insufficient funds") {
        // The node knows the shortfall but does not report it in a parseable
        // form; the caller's pre-flight balance check reports the numbers when
        // it can, so zeroes here mean "the node said no, amounts unknown".
        return TxError::InsufficientFunds { required: U256::ZERO, available: U256::ZERO };
    }
    TxError::Rejected(msg)
}

// ── Request ───────────────────────────────────────────────────────────────────

/// One contract call to broadcast.
#[derive(Debug, Clone)]
pub struct TxPlan {
    pub to: Address,
    pub value: U256,
    /// ABI-encoded calldata, as produced by `rpc::encode_*_calldata`.
    pub input: Vec<u8>,
}

// ── Broadcast ─────────────────────────────────────────────────────────────────

/// Signs and broadcasts `plan`, returning the 0x-prefixed transaction hash.
///
/// Returns as soon as the node accepts the transaction — it does not wait for
/// inclusion. Callers pair this with [`crate::rpc::wait_for_receipt`], which is
/// the same poller the webview flow uses.
///
/// Steps: read nonce + chain id + fee estimate → estimate gas (+ buffer) →
/// verify the balance covers `value + gas` → sign the EIP-1559 sighash →
/// `eth_sendRawTransaction`.
pub fn send(rpc_url: &str, signer: &dyn Signer, plan: &TxPlan) -> Result<String, TxError> {
    let from = signer.address();

    // Everything that needs the network happens in one block_on so the caller
    // stays synchronous, matching the rest of the wrapper.
    let prepared = block_on(async {
        let provider = build_provider(rpc_url)?;

        let chain_id = provider.get_chain_id().await.map_err(|e| TxError::Rpc(e.to_string()))?;
        let nonce = provider
            .get_transaction_count(from)
            .await
            .map_err(|e| TxError::Rpc(e.to_string()))?;
        let fees = provider
            .estimate_eip1559_fees()
            .await
            .map_err(|e| TxError::Rpc(e.to_string()))?;

        // Pre-flight: a wallet that cannot even cover `value` will make
        // `eth_estimateGas` fail with an opaque error. Checking first lets us
        // report the actual shortfall.
        let balance =
            provider.get_balance(from).await.map_err(|e| TxError::Rpc(e.to_string()))?;
        if balance < plan.value {
            return Err(TxError::InsufficientFunds { required: plan.value, available: balance });
        }

        let request = TransactionRequest::default()
            .with_from(from)
            .with_to(plan.to)
            .with_value(plan.value)
            .with_input(plan.input.clone());

        let estimate = provider
            .estimate_gas(request)
            .await
            .map_err(|e| classify(e.to_string()))?;
        let gas_limit = estimate.saturating_mul(GAS_LIMIT_BUFFER_PCT) / 100;

        let max_cost = plan.value
            + U256::from(gas_limit).saturating_mul(U256::from(fees.max_fee_per_gas));
        if balance < max_cost {
            return Err(TxError::InsufficientFunds { required: max_cost, available: balance });
        }

        Ok(Prepared {
            chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas: fees.max_fee_per_gas,
            max_priority_fee_per_gas: fees.max_priority_fee_per_gas,
        })
    })?;

    let tx = TxEip1559 {
        chain_id: prepared.chain_id,
        nonce: prepared.nonce,
        gas_limit: prepared.gas_limit,
        max_fee_per_gas: prepared.max_fee_per_gas,
        max_priority_fee_per_gas: prepared.max_priority_fee_per_gas,
        to: TxKind::Call(plan.to),
        value: plan.value,
        access_list: Default::default(),
        input: plan.input.clone().into(),
    };

    let signature = signer.sign_prehash(tx.signature_hash())?;
    let raw = TxEnvelope::Eip1559(tx.into_signed(signature)).encoded_2718();

    block_on(async move {
        let provider = build_provider(rpc_url)?;
        let pending = provider
            .send_raw_transaction(&raw)
            .await
            .map_err(|e| classify(e.to_string()))?;
        Ok(format!("0x{}", hex::encode(pending.tx_hash().as_slice())))
    })
}

struct Prepared {
    chain_id: u64,
    nonce: u64,
    gas_limit: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
}

// ── Internals ─────────────────────────────────────────────────────────────────

fn build_provider(rpc_url: &str) -> Result<impl Provider, TxError> {
    let url: url::Url =
        rpc_url.parse().map_err(|e: url::ParseError| TxError::Rpc(e.to_string()))?;
    Ok(alloy::providers::ProviderBuilder::new().connect_http(url))
}

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
    use crate::signer::LocalSigner;

    const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn plan() -> TxPlan {
        TxPlan { to: Address::ZERO, value: U256::ZERO, input: vec![] }
    }

    #[test]
    fn send_invalid_url_is_rpc_error() {
        let signer = LocalSigner::from_hex(ANVIL_KEY).unwrap();
        let err = send("not-a-url", &signer, &plan()).unwrap_err();
        assert!(matches!(err, TxError::Rpc(_)), "got {err:?}");
    }

    #[test]
    fn classify_maps_node_insufficient_funds() {
        let err = classify("err: insufficient funds for gas * price + value".into());
        assert!(matches!(err, TxError::InsufficientFunds { .. }), "got {err:?}");
    }

    #[test]
    fn classify_is_case_insensitive() {
        let err = classify("Insufficient Funds".into());
        assert!(matches!(err, TxError::InsufficientFunds { .. }), "got {err:?}");
    }

    #[test]
    fn classify_passes_other_errors_through() {
        let err = classify("execution reverted: CooldownActive".into());
        match err {
            TxError::Rejected(m) => assert!(m.contains("CooldownActive")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn insufficient_funds_message_reports_both_amounts() {
        let err = TxError::InsufficientFunds {
            required: U256::from(1_000u64),
            available: U256::from(7u64),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("1000"), "{rendered}");
        assert!(rendered.contains('7'), "{rendered}");
    }
}
