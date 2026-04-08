# deotp

Decentralized One-Time Purchase — NFT-based software licensing with a native application wrapper.

## Core Concept

A developer deploys a standard NFT contract (ERC-721 + payable mint) for their application. Users purchase a license by minting an NFT. A native Rust wrapper enforces the license locally — the user connects their wallet once to activate, and the wrapper verifies offline on every subsequent launch. No backend servers, no key services, no intermediaries.

## How It Works

1. **Developer** packages their Rust or Tauri app inside the deotp wrapper using the CLI
2. **Developer** deploys a standard ERC-721 contract with a `purchase()` function (price, supply cap, etc.)
3. **User** buys the NFT (standard on-chain transaction)
4. **User** launches the wrapped app → prompted to connect wallet
5. **Wrapper** checks chain: does this wallet own an NFT from this app's contract? (one-time, online)
6. **Wallet** signs a machine-bound activation message: `H(app_id || tokenId || machine_id)`
7. **Wrapper** stores `{signature, wallet_addr, tokenId}` locally as the license proof
8. **Every subsequent launch**: wrapper re-derives message, verifies signature offline — no network needed

## What Already Exists (Off the Shelf)

- NFT minting contracts (OpenZeppelin ERC-721 templates)
- Wallet connection (ethers.js, wagmi, WalletConnect)
- On-chain ownership verification (`ownerOf(tokenId)`)

## What deotp Builds (The Product)

The wrapper is the entire product. Without it, an NFT receipt has no enforcement on the user's machine.

- **deotp-wrapper** — Rust binary that hosts and gates the embedded application
- **deotp-sdk** — Rust crate that apps link against for heartbeat/lifecycle integration
- **deotp-cli** — Packaging tool that bundles an app into a wrapped distributable
- **tauri-plugin-deotp** — Tauri plugin for web-app integration

## Design Principles

- The wrapper does not need internet after initial activation
- The wrapper can host embedded Rust binaries and Tauri web applications
- The embedded app cannot run if the wrapper process is killed (heartbeat IPC)
- The wrapper takes minimal CPU/memory overhead
- The wrapper does not interfere with the app's system file access but can pause compute and kill the app
- Binary encryption is explicitly a non-goal — the wrapper enforces license checks, not cryptographic DRM. This matches how most commercial software works (the effort to crack exceeds the cost to buy).

## Open Questions

- **Which chain?** Solana (low fees, fast finality) vs L2 like Base/Arbitrum (EVM compatibility, low cost). Ethereum mainnet is too expensive for small purchases.
- **Machine ID stability** — how to derive a machine fingerprint that's stable across reboots but unique per machine, cross-platform
- **NFT transfer = license transfer?** If the NFT is sold, the old activation signature is still valid on the old machine. Options: expiring activations that require periodic re-check, or accept that transfers need re-activation.
- **Wallet connection from native Rust** — no browser available. Options: small webview for wallet flow, WalletConnect QR code, or direct keystore access.
- **Multi-machine licenses** — user wants the app on desktop and laptop. One NFT = one machine? Or allow N activations per token?

## Related Projects

- **Valist** (valist.io) — Decentralized software distribution with NFT license keys. Handles distribution but not local enforcement.
- **Keygen** (keygen.sh) — Mature offline license verification with Rust SDK. Not blockchain-based. Closest prior art for the wrapper side.
- **Unlock Protocol** — ERC-721 membership keys with time-limited access. Smart contract patterns are relevant.
- **CryptLex** — Commercial RSA-signed offline license files. Machine-locking approach is relevant.

No existing project combines on-chain NFT purchase with offline native wrapper enforcement. That's the gap.

See [architecture.md](architecture.md) and [implementation.md](implementation.md) for technical details.
