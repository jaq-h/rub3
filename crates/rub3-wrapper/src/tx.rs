//! Building, signing, and broadcasting transactions for headless activation.
//!
//! The interactive flow encodes calldata and hands it to a human's wallet.
//! Headless has no wallet to hand it to, so this module closes the gap: it
//! fills in nonce/gas/fees, hands the EIP-1559 sighash to a [`Signer`], and
//! pushes the signed envelope out via `eth_sendRawTransaction`.
//!
//! No key material passes through here - the only signing operation is
//! [`Signer::sign_prehash`], which a KMS or enclave backend serves without
//! releasing a key. Calldata construction stays in [`crate::rpc`]; this module
//! only wraps it in an envelope.
//!
//! Every error built from an alloy provider here goes through
//! [`crate::rpc::redact_urls`], the one sanitiser, because these messages are
//! built from the request and carry the packed `RPC_URL` with whatever key is
//! embedded in it. `TxError::Rpc` and `TxError::Rejected` reach an operator
//! through the agent door verbatim, so the redaction has to happen where the
//! error is made rather than where it is printed.

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, TxKind, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;

use crate::rpc::redact_urls;
use crate::signer::{Signer, SignerError};

/// Multiplier applied to the node's `eth_estimateGas` result, in percent.
///
/// `purchase()` and `activate()` both write storage whose cost can shift
/// between the estimate and inclusion (a first-touch slot, a changed cooldown
/// slot). 25% is the usual headroom for that without materially raising the
/// balance an agent must hold.
const GAS_LIMIT_BUFFER_PCT: u64 = 125;

// ── Errors ────────────────────────────────────────────────────────────────────

/// What a [`Shortfall`]'s `required` figure accounts for.
///
/// The pre-flight balance check runs before `eth_estimateGas`, because a wallet
/// that cannot cover the value alone makes the estimate fail with an opaque
/// error. Its figure therefore excludes gas, and an orchestrator topping up
/// against it needs to know that rather than being told "price + gas".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Covers {
    /// The transaction value alone, measured before gas could be estimated.
    PriceOnly,
    /// The value plus `gas_limit * max_fee_per_gas`, the full cost ceiling.
    PriceAndGas,
}

impl Covers {
    /// The token emitted as `required_covers=` on the machine-detail line.
    pub fn as_str(self) -> &'static str {
        match self {
            Covers::PriceOnly => "price",
            Covers::PriceAndGas => "price_plus_gas",
        }
    }
}

/// A measured funding gap: what the transaction costs, and what the wallet has.
///
/// Only ever constructed from numbers the wrapper read itself. A node that
/// rejects a transaction for lack of balance does not report either amount, and
/// that case carries no `Shortfall` rather than a pair of zeroes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortfall {
    pub required: U256,
    pub available: U256,
    pub covers: Covers,
}

#[derive(Debug)]
pub enum TxError {
    /// URL parse, connection, or JSON-RPC transport failure.
    Rpc(String),
    /// The signing backend refused.
    Signer(SignerError),
    /// The sender cannot cover `value + gas_limit × max_fee_per_gas`. `None`
    /// when the node reported the shortfall and the amounts are unknown.
    InsufficientFunds(Option<Shortfall>),
    /// The node rejected the transaction (revert on estimation, nonce clash,
    /// underpriced, …).
    Rejected(String),
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxError::Rpc(e) => write!(f, "rpc error: {e}"),
            TxError::Signer(e) => write!(f, "{e}"),
            TxError::InsufficientFunds(Some(s)) => match s.covers {
                Covers::PriceAndGas => write!(
                    f,
                    "insufficient funds: need {} wei (value + gas), wallet holds {} wei",
                    s.required, s.available
                ),
                Covers::PriceOnly => write!(
                    f,
                    "insufficient funds: need {} wei for the value alone, before gas, \
                     wallet holds {} wei",
                    s.required, s.available
                ),
            },
            TxError::InsufficientFunds(None) => write!(
                f,
                "insufficient funds: the node rejected the transaction for lack of \
                 balance, without reporting the amounts"
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
        // form. The pre-flight balance check below carries the real numbers
        // when it fires; here there are none, and inventing zeroes would tell
        // an orchestrator the wallet needs nothing.
        return TxError::InsufficientFunds(None);
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
/// Returns as soon as the node accepts the transaction - it does not wait for
/// inclusion. Callers pair this with [`crate::rpc::wait_for_receipt`], which is
/// the same poller the webview flow uses.
///
/// Steps: read nonce + chain id + fee estimate → estimate gas (+ buffer) →
/// verify the balance covers `value + gas` → sign the EIP-1559 sighash →
/// `eth_sendRawTransaction`.
pub fn send(rpc_url: &str, signer: &dyn Signer, plan: &TxPlan) -> Result<String, TxError> {
    let from = signer.address();

    // One runtime and one provider for the whole transaction: signing is
    // synchronous, so it sits inside the async block rather than splitting the
    // network work in two. The caller stays synchronous, matching the rest of
    // the wrapper.
    block_on(async move {
        let provider = build_provider(rpc_url)?;

        let chain_id = provider
            .get_chain_id()
            .await
            .map_err(|e| TxError::Rpc(redact_urls(&e.to_string())))?;
        let nonce = provider
            .get_transaction_count(from)
            .await
            .map_err(|e| TxError::Rpc(redact_urls(&e.to_string())))?;
        let fees = provider
            .estimate_eip1559_fees()
            .await
            .map_err(|e| TxError::Rpc(redact_urls(&e.to_string())))?;

        // Pre-flight: a wallet that cannot even cover `value` will make
        // `eth_estimateGas` fail with an opaque error. Checking first lets us
        // report the actual shortfall.
        let balance = provider
            .get_balance(from)
            .await
            .map_err(|e| TxError::Rpc(redact_urls(&e.to_string())))?;
        if balance < plan.value {
            return Err(TxError::InsufficientFunds(Some(Shortfall {
                required: plan.value,
                available: balance,
                covers: Covers::PriceOnly,
            })));
        }

        let request = TransactionRequest::default()
            .with_from(from)
            .with_to(plan.to)
            .with_value(plan.value)
            .with_input(plan.input.clone());

        let estimate = provider
            .estimate_gas(request)
            .await
            .map_err(|e| classify(redact_urls(&e.to_string())))?;
        let gas_limit = estimate.saturating_mul(GAS_LIMIT_BUFFER_PCT) / 100;

        let max_cost =
            plan.value + U256::from(gas_limit).saturating_mul(U256::from(fees.max_fee_per_gas));
        if balance < max_cost {
            return Err(TxError::InsufficientFunds(Some(Shortfall {
                required: max_cost,
                available: balance,
                covers: Covers::PriceAndGas,
            })));
        }

        let tx = TxEip1559 {
            chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas: fees.max_fee_per_gas,
            max_priority_fee_per_gas: fees.max_priority_fee_per_gas,
            to: TxKind::Call(plan.to),
            value: plan.value,
            access_list: Default::default(),
            input: plan.input.clone().into(),
        };

        let signature = signer.sign_prehash(tx.signature_hash())?;
        let raw = TxEnvelope::Eip1559(tx.into_signed(signature)).encoded_2718();

        let pending = provider
            .send_raw_transaction(&raw)
            .await
            .map_err(|e| classify(redact_urls(&e.to_string())))?;
        Ok(format!("0x{}", hex::encode(pending.tx_hash().as_slice())))
    })
}

// ── Internals ─────────────────────────────────────────────────────────────────

fn build_provider(rpc_url: &str) -> Result<impl Provider, TxError> {
    let url: url::Url = rpc_url
        .parse()
        .map_err(|e: url::ParseError| TxError::Rpc(redact_urls(&e.to_string())))?;
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
        TxPlan {
            to: Address::ZERO,
            value: U256::ZERO,
            input: vec![],
        }
    }

    #[test]
    fn send_invalid_url_is_rpc_error() {
        let signer = LocalSigner::from_hex(ANVIL_KEY).unwrap();
        let err = send("not-a-url", &signer, &plan()).unwrap_err();
        assert!(matches!(err, TxError::Rpc(_)), "got {err:?}");
    }

    /// The agent door prints `TxError` straight through as `HeadlessError::Rpc`,
    /// so an unreachable node during a headless purchase is where the packed
    /// endpoint reaches an operator. Driven through `send` rather than through
    /// the sanitiser, so what is asserted is what the door would actually
    /// print.
    #[test]
    fn a_failed_send_never_carries_the_packed_endpoints_key() {
        const KEY: &str = "3ac91be5d7204f18ba6e0c9d4f27a615";
        const HOST: &str = "127.0.0.1";
        let signer = LocalSigner::from_hex(ANVIL_KEY).unwrap();

        for url in [
            // Port 1 is reserved and never listening, so every one of these is
            // refused rather than left to time out.
            format!("http://{HOST}:1/v2/{KEY}"),
            format!("http://{HOST}:1/rpc?apiKey={KEY}"),
            format!("http://apikey:{KEY}@{HOST}:1/rpc"),
        ] {
            let rendered = send(&url, &signer, &plan()).unwrap_err().to_string();
            assert!(
                !rendered.contains(KEY),
                "the key reached the door: {rendered}"
            );
            assert!(
                rendered.contains(HOST),
                "the endpoint that failed still has to be nameable: {rendered}"
            );
        }
    }

    /// The classifier only pattern-matched a string, so it must not claim to
    /// know the amounts: a fabricated zero would read as "top up nothing".
    #[test]
    fn classify_maps_node_insufficient_funds_without_amounts() {
        let err = classify("err: insufficient funds for gas * price + value".into());
        assert!(
            matches!(err, TxError::InsufficientFunds(None)),
            "got {err:?}"
        );
    }

    #[test]
    fn classify_is_case_insensitive() {
        let err = classify("Insufficient Funds".into());
        assert!(
            matches!(err, TxError::InsufficientFunds(None)),
            "got {err:?}"
        );
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
        let err = TxError::InsufficientFunds(Some(Shortfall {
            required: U256::from(1_000u64),
            available: U256::from(7u64),
            covers: Covers::PriceAndGas,
        }));
        let rendered = err.to_string();
        assert!(rendered.contains("1000"), "{rendered}");
        assert!(rendered.contains('7'), "{rendered}");
        assert!(rendered.contains("value + gas"), "{rendered}");
    }

    /// The pre-flight check fires before gas can be estimated, so its figure
    /// must not be presented as covering gas.
    #[test]
    fn price_only_shortfall_says_it_excludes_gas() {
        let rendered = TxError::InsufficientFunds(Some(Shortfall {
            required: U256::from(1_000u64),
            available: U256::ZERO,
            covers: Covers::PriceOnly,
        }))
        .to_string();
        assert!(rendered.contains("before gas"), "{rendered}");
        assert!(!rendered.contains("value + gas"), "{rendered}");
    }

    /// With no measured amounts the message says so, rather than rendering a
    /// shortfall of zero.
    #[test]
    fn insufficient_funds_message_without_amounts_states_no_figures() {
        let rendered = TxError::InsufficientFunds(None).to_string();
        assert!(rendered.contains("insufficient funds"), "{rendered}");
        assert!(!rendered.contains('0'), "{rendered}");
    }
}
