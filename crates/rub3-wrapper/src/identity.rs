//! Identity model + ERC-6551 TBA derivation.
//!
//! See `architecture.md` §"Identity Models" and §"TBA Address Derivation".
//!
//! Two identity models:
//!   - access  (0): `user_id = wallet` — NFT gates access, wallet is identity.
//!   - account (1): `user_id = TBA`    — NFT is an account, TBA is identity.
//!
//! TBA derivation is pure CREATE2 over the canonical ERC-6551 registry at
//! `0x000000006551c19487814612e58FE06813775758`. No RPC is required — given
//! `(implementation, chainId, contract, tokenId)` the account address is
//! deterministic regardless of whether the TBA has been deployed.

use alloy::primitives::{address, keccak256, Address, B256, U256};

// ── IdentityModel enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityModel {
    /// user_id = wallet address of current holder.
    Access,
    /// user_id = deterministic TBA address derived from the token.
    Account,
}

impl IdentityModel {
    /// Maps the on-chain `uint8` to the enum. Unknown values return `None` —
    /// the contract constructor rejects anything > 1, so this only fires for
    /// wire-format corruption.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(IdentityModel::Access),
            1 => Some(IdentityModel::Account),
            _ => None,
        }
    }

    /// Wire string used in the `Session.identity` field.
    pub fn as_str(&self) -> &'static str {
        match self {
            IdentityModel::Access  => "access",
            IdentityModel::Account => "account",
        }
    }
}

// ── ERC-6551 constants ────────────────────────────────────────────────────────

/// Canonical ERC-6551 registry deployed at the same address on every chain.
pub const ERC6551_REGISTRY: Address = address!("000000006551c19487814612e58FE06813775758");

/// Salt passed to the registry. rub3 always uses 0 so a given
/// `(implementation, chainId, contract, tokenId)` yields exactly one TBA.
pub const ERC6551_SALT: B256 = B256::ZERO;

// Reference account-proxy init code as fixed in the ERC-6551 registry.
// The implementation address is spliced in between the two halves.
//   0x3d60ad80600a3d3981f3  — constructor that returns the runtime code
//   363d3d373d3d3d363d73    — runtime prefix up to the DELEGATECALL target
//   <20 bytes implementation>
//   5af43d82803e903d91602b57fd5bf3 — runtime suffix
const INIT_CODE_PREFIX: &[u8] = &[
    0x3d, 0x60, 0xad, 0x80, 0x60, 0x0a, 0x3d, 0x39, 0x81, 0xf3,
    0x36, 0x3d, 0x3d, 0x37, 0x3d, 0x3d, 0x3d, 0x36, 0x3d, 0x73,
];
const INIT_CODE_SUFFIX: &[u8] = &[
    0x5a, 0xf4, 0x3d, 0x82, 0x80, 0x3e, 0x90, 0x3d, 0x91,
    0x60, 0x2b, 0x57, 0xfd, 0x5b, 0xf3,
];

// ── TBA derivation ────────────────────────────────────────────────────────────

/// Derives the ERC-6551 token-bound account address for a token.
///
/// Pure — computed locally via CREATE2 with the canonical registry, `salt = 0`,
/// and the standard account-proxy init code. Does not depend on whether the
/// account contract has been deployed.
///
/// # Arguments
/// * `implementation` — the ERC-6551 account implementation (set per contract
///   at deploy time as `tbaImplementation`).
/// * `chain_id` — EVM chain id the token lives on.
/// * `contract` — the NFT contract address.
/// * `token_id` — the NFT token id.
pub fn derive_tba(
    implementation: Address,
    chain_id:       u64,
    contract:       Address,
    token_id:       u64,
) -> Address {
    // keccak256(abi.encode(salt, chainId, tokenContract, tokenId))
    // Each element is a 32-byte ABI word; addresses are left-padded.
    let mut salt_preimage = [0u8; 128];
    salt_preimage[0..32].copy_from_slice(ERC6551_SALT.as_slice());
    salt_preimage[32..64].copy_from_slice(&U256::from(chain_id).to_be_bytes::<32>());
    salt_preimage[76..96].copy_from_slice(contract.as_slice());
    salt_preimage[96..128].copy_from_slice(&U256::from(token_id).to_be_bytes::<32>());
    let final_salt = keccak256(salt_preimage);

    // code = INIT_PREFIX ++ implementation (20 bytes) ++ INIT_SUFFIX
    let mut code = Vec::with_capacity(INIT_CODE_PREFIX.len() + 20 + INIT_CODE_SUFFIX.len());
    code.extend_from_slice(INIT_CODE_PREFIX);
    code.extend_from_slice(implementation.as_slice());
    code.extend_from_slice(INIT_CODE_SUFFIX);
    let code_hash = keccak256(&code);

    // CREATE2: keccak256(0xff ++ registry ++ salt ++ codeHash)[12..]
    let mut create2 = [0u8; 85];
    create2[0] = 0xff;
    create2[1..21].copy_from_slice(ERC6551_REGISTRY.as_slice());
    create2[21..53].copy_from_slice(final_salt.as_slice());
    create2[53..85].copy_from_slice(code_hash.as_slice());
    Address::from_slice(&keccak256(create2)[12..])
}

// ── user_id resolution ────────────────────────────────────────────────────────

/// Returns the wire-format `user_id` for a session.
///
/// Access model: the wallet address. Account model: the TBA address. Both are
/// lower-cased 0x-prefixed hex so callers can compare them byte-for-byte.
pub fn resolve_user_id(model: IdentityModel, wallet: Address, tba: Option<Address>) -> String {
    match model {
        IdentityModel::Access  => format_addr(wallet),
        IdentityModel::Account => {
            format_addr(tba.expect("resolve_user_id: account model requires tba"))
        }
    }
}

/// Canonical lowercase 0x-prefixed hex address.
pub fn format_addr(addr: Address) -> String {
    format!("0x{}", hex::encode(addr.as_slice()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const IMPL:     Address = address!("1111111111111111111111111111111111111111");
    const CONTRACT: Address = address!("2222222222222222222222222222222222222222");

    // ── IdentityModel ────────────────────────────────────────────────────────

    #[test]
    fn from_u8_valid() {
        assert_eq!(IdentityModel::from_u8(0), Some(IdentityModel::Access));
        assert_eq!(IdentityModel::from_u8(1), Some(IdentityModel::Account));
    }

    #[test]
    fn from_u8_rejects_out_of_range() {
        assert_eq!(IdentityModel::from_u8(2),   None);
        assert_eq!(IdentityModel::from_u8(255), None);
    }

    #[test]
    fn as_str_matches_wire_format() {
        assert_eq!(IdentityModel::Access.as_str(),  "access");
        assert_eq!(IdentityModel::Account.as_str(), "account");
    }

    // ── TBA determinism ──────────────────────────────────────────────────────

    #[test]
    fn derive_tba_is_deterministic() {
        let a = derive_tba(IMPL, 8453, CONTRACT, 42);
        let b = derive_tba(IMPL, 8453, CONTRACT, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_tba_differs_by_token_id() {
        let a = derive_tba(IMPL, 8453, CONTRACT, 1);
        let b = derive_tba(IMPL, 8453, CONTRACT, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn derive_tba_differs_by_contract() {
        let other: Address = address!("3333333333333333333333333333333333333333");
        let a = derive_tba(IMPL, 8453, CONTRACT, 42);
        let b = derive_tba(IMPL, 8453, other,    42);
        assert_ne!(a, b);
    }

    #[test]
    fn derive_tba_differs_by_chain_id() {
        let a = derive_tba(IMPL, 1,    CONTRACT, 42);
        let b = derive_tba(IMPL, 8453, CONTRACT, 42);
        assert_ne!(a, b);
    }

    #[test]
    fn derive_tba_differs_by_implementation() {
        let other: Address = address!("4444444444444444444444444444444444444444");
        let a = derive_tba(IMPL,  8453, CONTRACT, 42);
        let b = derive_tba(other, 8453, CONTRACT, 42);
        assert_ne!(a, b);
    }

    // ── user_id resolution ───────────────────────────────────────────────────

    #[test]
    fn resolve_user_id_access_returns_wallet() {
        let wallet: Address = address!("00000000000000000000000000000000000000aa");
        let id = resolve_user_id(IdentityModel::Access, wallet, None);
        assert_eq!(id, "0x00000000000000000000000000000000000000aa");
    }

    #[test]
    fn resolve_user_id_account_returns_tba() {
        let wallet: Address = address!("00000000000000000000000000000000000000aa");
        let tba:    Address = address!("00000000000000000000000000000000000000bb");
        let id = resolve_user_id(IdentityModel::Account, wallet, Some(tba));
        assert_eq!(id, "0x00000000000000000000000000000000000000bb");
    }

    #[test]
    #[should_panic(expected = "account model requires tba")]
    fn resolve_user_id_account_panics_without_tba() {
        let wallet: Address = address!("00000000000000000000000000000000000000aa");
        let _ = resolve_user_id(IdentityModel::Account, wallet, None);
    }
}
