# deotp

Wallet-native desktop software. NFT-gated access for native applications, without a browser.

## The Paradigm

Web3 replaced username/password with wallet connect for web apps. deotp does the same for native desktop applications.

The NFT is not a license key in the DRM sense. It is an access credential in the web3 sense — owned by a wallet, verifiable on-chain, transferrable, composable. The wrapper is the runtime that enforces this on the user's machine, independent of any browser or web context.

There is no offline mode. There is no machine binding. The wallet is the identity layer. Connecting it is how you prove who you are, the same way it works in every other web3 context — except here the gated resource is a native binary, not a webpage.

## How It Works

1. **Developer** packages their Rust or Tauri app inside the deotp wrapper using the CLI
2. **Developer** deploys a license contract (one-time purchase or subscription) on Base
3. **User** purchases access — mints an NFT via the in-wrapper purchase UI or any standard frontend
4. **User** launches the wrapped app → wrapper checks for a valid cached session
5. If no session (or session expired): wrapper opens wallet connection, verifies on-chain ownership, requests a session signature
6. **Wrapper** caches the session locally for the configured TTL
7. **Every launch within TTL**: wrapper verifies session signature locally, launches immediately
8. **Session expiry**: wallet prompt again — ownership re-verified on-chain

## What deotp Builds

- **deotp-wrapper** — Rust binary that manages wallet sessions and gates the embedded application
- **deotp-sdk** — Rust crate apps link against for heartbeat and session access
- **deotp-cli** — Packaging and deployment tool for developers
- **tauri-plugin-deotp** — First-class Tauri integration

## Design Principles

- **Wallet is identity.** No machine fingerprinting, no license files, no key servers. The wallet signature is the credential.
- **Desktop ≠ browser.** Native UX — system tray, OS notifications, no browser dependency. The embedded webview is only for wallet connection UI, not the app itself.
- **Always-online by design.** Session TTL enforces periodic on-chain re-verification. This is a feature, not a limitation — it means NFT transfers take effect on next session renewal, subscriptions expire naturally, and ownership is always current.
- **Multi-device by default.** One wallet works on any number of machines. Each device maintains its own session cache. No coordination, no device slots, no gas cost per device.
- **Transfer = re-activation.** When an NFT is sold or transferred, the old owner's sessions expire at their next TTL. The new owner activates fresh. No revocation infrastructure needed.
- **No backend.** The chain is the source of truth. The wallet is the key. deotp has no servers, no databases, no auth service.

## Two Access Models

**One-time purchase** (`DeotpAccess`) — standard ERC-721, no expiry. Pay once, own forever. NFT is transferrable — selling it transfers access to the buyer. Developer sets price and optional supply cap.

**Subscription** (`DeotpSubscription`) — ERC-721 with `expiresAt`. Monthly (or configurable) renewal via `renew(tokenId)`. Session TTL is set to match the billing period. Expired subscription = failed session renewal = no launch.

Both are enforced identically at the wrapper level. The contract type determines whether the on-chain check is `ownerOf()` or `isValid()`.

## Key Decisions

- **Chain: Base.** Coinbase on-ramp, ENS support, EVM compatibility, `alloy` Rust crate (~30 deps). Chain abstracted behind config.
- **SIWE-style sessions.** Wrapper requests a signed statement from the wallet: `H(app_id || tokenId || nonce || expires_at)`. This is the session token — no backend, no JWT, no cookie. Cached locally, verified cryptographically on each launch.
- **ENS trust layer.** Developer registers ENS pointing to their contract. Wrapper resolves at session creation and rejects mismatches. Trust chain: ENS → contract → binary hash → running wrapper.
- **Webview for wallet UI only.** `wry` embeds a minimal webview for WalletConnect. The wrapped app never touches the webview — it is only used for the session creation flow.

## What This Is Not

- Not DRM. Binary encryption is not a goal. The wrapper enforces access, not cryptographic lockdown.
- Not a backend auth system. There is no server validating requests.
- Not browser-based. The app runs natively. The wallet connection happens natively.
- Not machine-locked. The same wallet activates on any device.

## Related Projects

- **Valist** — Decentralized software distribution with NFT license keys. Handles distribution, not runtime enforcement.
- **Unlock Protocol** — Subscription NFT contracts. Smart contract patterns are relevant; they require a backend for enforcement.
- **SIWE** — Sign-In With Ethereum. The session primitive deotp adapts for desktop.
- **Privy / Magic** — Custodial wallet auth. Opposite philosophy — they manage keys server-side.

No existing project delivers wallet-native session management for native desktop binaries without a backend. That is the gap.

See [architecture.md](architecture.md) and [implementation.md](implementation.md) for technical details.
