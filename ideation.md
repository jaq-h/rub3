# rub3

Wallet-native software licensing for the machine economy. NFT-gated access for locally executed software - CLI tools, MCP servers, desktop apps - without a browser, a backend, or anyone's permission.

This file owns positioning: the vision, the business model, and what rub3 is and is not. It makes the case; it does not specify the system. Design rationale is in [architecture.md](architecture.md), the roadmap and current status in [implementation.md](implementation.md), contract operations in [contracts/contracts.md](contracts/contracts.md), and orientation in [README.md](README.md).

## The Paradigm

rub3 lets machines buy, verify, run, and resell software without asking anyone's permission.

Web3 replaced username/password with wallet connect for web apps. rub3 does the same for locally executed software - and takes the next step: the customer doing the connecting is increasingly not a person but an agent. Agents cannot pass KYC, hold a credit card, or click through a Stripe checkout. They can hold a private key. **An agent is a wallet** - which makes wallet-native licensing the only licensing model an autonomous customer can actually use.

The NFT is not a license key in the DRM sense. It is an access credential in the web3 sense - owned by a wallet, verifiable on-chain, transferrable, composable. And because it is transferrable, it is a **liquid capital asset**: an agent buys a license for a workload, uses it, and resells it when the job ends. No traditional licensing system can do this - licenses everywhere else are contractually non-transferable.

The wrapper is the runtime that enforces this on the machine where the software runs, independent of any browser, web context, or vendor server. There is no machine binding. The wallet is the identity layer - except here the gated resource is a locally executed binary, not a webpage.

## The Agent Loop

The unit of adoption is a single closed loop a machine can complete end to end, with no human in it:

```text
discover → pay → fetch → verify → run → (resell)
```

1. **Discover** - find the app in the rub3 registry: contract address, price, content URI, binary hashes, all on-chain
2. **Pay** - purchase the license NFT in USDC (EIP-3009 signed authorization; no ETH required, no account, no API key)
3. **Fetch** - download the binary from content-addressed storage (URI recorded on-chain)
4. **Verify** - check the binary hash against the contract's on-chain hash set
5. **Run** - headless activation: signer in, session out; the wrapper verifies ownership and launches
6. **Resell** - transfer the NFT on the secondary market when the workload ends

Humans complete the same loop through the wrapper's native activation window. The webview is the fallback floor; headless is the front door.

## How It Works

1. **Developer** packages their binary inside the rub3 wrapper using the CLI
2. **Developer** deploys a license contract through the rub3 factory on Base
3. **Buyer** - human or agent - purchases access: mints the NFT via headless purchase, the in-wrapper purchase UI, or any standard frontend
4. **Buyer** launches the wrapped app → wrapper checks for a valid cached session
5. If no session (or session expired): headless builds verify ownership and sign programmatically; interactive builds open the wallet-connection window
6. **Wrapper** caches the session locally for the configured TTL
7. **Every launch within TTL**: wrapper verifies the session signature locally, launches immediately
8. **Session expiry**: ownership re-verified on-chain, session re-signed

## What rub3 Builds

**Open-source rails (free forever):**

- **rub3-wrapper** - Rust runtime that manages wallet sessions and gates the embedded application; headless and interactive modes
- **rub3-sdk** - Rust crate apps link against for heartbeat and session access
- **rub3-cli** - `pack` and `deploy` today, with `fetch` and `register` planned alongside distribution and the registry - the agent-facing interface to everything
- **License contracts** - the one-time purchase licence, with metered planned; audited templates stamped by the factory
- **tauri-plugin-rub3** - first-class Tauri integration (human-surface phase)

**Owned network layers (where revenue lives):**

- **Rub3Factory** - deploys license contracts with an immutable protocol fee split
- **rub3 registry** - on-chain discovery and verification; lists factory deploys only
- **License marketplace** - the venue where agents resell licenses (built when secondary volume appears)
- **Facilitator** - gasless USDC purchase relay for buyers holding only stablecoins

## Business Model

The intent: open-source the rails; own the factory, registry, and marketplace; take 2–3% of a payment flow that only the wrapper can meter, priced low enough that no agent bothers to route around it. The factory, its on-chain fee split and the registry exist today; the marketplace and metered billing below are not built, and the contracts are deployed nowhere: they do not reach mainnet or get declared ready for use until the factory and the registry launch together.

- **Protocol fee** - 2–3% on `purchase()`, split on-chain by factory-deployed contracts. Immutable per contract: a developer's fee can never be raised after deploy. rub3 changes fees by shipping a new factory version, affecting only future deploys.
- **Metered billing** - the wrapper gates every launch, which makes it the only viable choke point for charging per-use on locally executed software. x402 meters API calls because the server is a choke point; nothing has ever been able to meter local execution. `Rub3Metered` turns the wrapper into a payment terminal: per-launch or per-session micropayments in USDC - same take rate, much higher-frequency flow.
- **Marketplace fee** - 1–2% on secondary license trades, plus an ERC-2981-style royalty split with the developer, honoured venue-side on a sale the marketplace settles rather than levied by the token itself: the licence contracts carry no royalty hook, and why is in [implementation.md](implementation.md) §2.3 → "Deliberately not done".
- **Hosted conveniences** - dashboards, purchase webhooks, binary pinning, release attestations. Optional SaaS for humans, entirely off the enforcement path.
- **Never charged:** the wrapper, SDK, CLI, contract templates, self-serve deploys. **No token, ever.** Fees settle in stablecoins.

Why this is expected to work better with agent customers than human ones: agents follow the canonical quickstart literally (defaults dominate), won't burn engineering time to dodge 2.5% on a $10 license, and their spend policies allowlist verified factory contracts - making the fee-carrying path also the trusted path.

## Ownership Invariants

**The token is the invariant; everything else is versioned.** The architecture evolves freely as long as evolution only ever changes what is *offered* going forward, never what was *granted*.

- **No revocation, structurally.** License contracts contain no burn, no admin transfer, no pause on validation reads. Not "we promise not to" - the bytecode cannot.
- **No proxies, no upgradeable contracts.** License terms are frozen at purchase time because contract code is frozen at deploy time.
- **Migrate offerings, never obligations.** Price changes affect future buyers only, and a licence carries no ongoing terms a later change could reach. Contract migrations use an opt-in successor pattern - the old contract validates its tokens forever.
- **Registry is discovery, never validity.** Delisting removes the badge and the listing; it cannot invalidate a session or a token.
- **Vendor death does not brick the software.** No backend means the always-online dependency is on the chain, not the developer. A wrapped app keeps activating even if its developer vanishes - a strictly stronger ownership guarantee than any traditional license.

Developers can deprecate *offerings* - stop selling, stop updating, sunset a version. They cannot deprecate *entitlements*. That distinction is what makes owning a license mean something, and it is what lets licenses trade as assets: markets can price abandonment risk (depreciation); they cannot price arbitrary confiscation.

## Three Billing Models × Two Identity Models

Orthogonal decisions the contract issuer makes at deploy time.

### Billing model

**One-time purchase** (`Rub3Access`) - pay once, own forever. NFT is transferrable, resellable. The only model rub3 ships: a time-bounded `Rub3Subscription` was built alongside it and then removed before any deploy, so an agent shopping a listing never has to work out whether the access it is buying expires.

**Metered** (`Rub3Metered`, planned) - pay per launch or per session-hour in USDC. The right billing shape for an agent that needs a tool once. It bounds what a *launch* costs and never makes an issued token expire, which is why it is not the model that was removed.

### Identity model

**Access** (`identity = "access"`) - wallet is the user identity. The NFT is a gate. Each holder is a distinct user. Transfer to a new wallet creates a fresh account in the application.

**Account** (`identity = "account"`) - the NFT is the user. Identity is the token's ERC-6551 Token Bound Account (TBA) address - deterministic, permanent, independent of who holds the NFT. Transfer sells the account: buyer inherits the history, preferences, and any on-chain assets attached to the TBA.

The wrapper reads the identity model from the contract at session creation. The SDK's `user_id` field reflects this - application code keys all persistent data on `user_id` and never needs to know which model is in use.

### The combinations

| | Access model | Account model |
|---|---|---|
| **One-time** | Standard software license. Wallet = account. | Software with persistent user data. NFT = account. Transferring sells the account. |
| **Metered** | Pay-per-use tooling. Wallet = payer. | Pay-per-use against a persistent account. |

## Key Decisions

- **Chain: Base.** Where x402/USDC machine-payment volume lives; Coinbase on-ramp for humans; ENS support; `alloy` Rust crate. Chain abstracted behind config.
- **Money: USDC first.** Purchases via EIP-3009 `receiveWithAuthorization` - gasless for the buyer, and signable by any x402 client that speaks EIP-3009. ETH pricing remains supported. Why the receive variant and not `transferWithAuthorization` is argued in [architecture.md](architecture.md) → "Money".
- **Headless first.** All crypto (signing, calldata encoding, receipt polling) is native Rust - the agent path needs no webview at all. `headless` builds exclude `wry`/`tao` entirely: smaller binary, no GUI dependencies, container-friendly.
- **Seats, not devices.** Agent fleets clone VMs and scale horizontally as a legitimate pattern. Concurrency is licensed as K on-chain seats per token, not bound to hardware.
- **SIWE-style sessions.** Wrapper requests a signed statement from the wallet: `H(app_id || tokenId || user_id || nonce || expires_at)`. This is the session token - no backend, no JWT, no cookie. Cached locally, verified cryptographically on each launch.
- **Token selection.** A wallet may own multiple tokens from the same contract. The wrapper presents a selection UI after wallet connection (interactive) or takes a `--token-id` flag (headless). Each token maintains its own independent session cache.
- **ENS as signal, not dependency.** Registry and ENS inform purchase-time trust. At launch, a conflicting resolution hard-fails (attack signature); a missing one warns and proceeds. The embedded contract address is the root of trust after purchase.
- **Confirmation has a floor.** Interactive on-chain steps degrade to a manual "paste your tx hash" path; richer modes (WalletConnect, RPC auto-detect) layer on top. Headless mode needs no confirmation UI - the signer broadcasts directly.

## Beachhead: Wallet-Gated MCP Servers

Developers shipping paid MCP servers have no licensing primitive today - Stripe plus API keys needs a backend and cannot onboard an agent customer at all. A rub3-wrapped MCP server is the first target market: the agent buys the license NFT in USDC, the wrapper verifies on-chain, the server runs locally, and the license resells when no longer needed. Fast-growing, natively agent-adjacent, and nobody serves it.

## What This Is Not

- Not DRM. Binary encryption is not a goal. The wrapper enforces access, not cryptographic lockdown.
- Not a backend auth system. There is no server validating requests.
- Not browser-based. The app runs natively. The wallet connection happens natively - or not at all (headless).
- Not machine-locked. The same wallet activates on any device; fleets license seats, not hardware.
- Not a token project. Fees settle in stablecoins; there is no rub3 token and no plan for one.

## Related Projects

- **x402** - pay-per-call for HTTP services. Complementary, not competing: x402 rents software by the call; rub3 sells the tool. An agent paying per-call 500 times a day should buy the license - and can resell it after.
- **ERC-8004** - on-chain agent identity/reputation/validation registries. The rub3 registry aligns with this shape so wrapped apps get discoverable agent cards.
- **Valist** - decentralized software distribution with NFT license keys. Distribution without runtime enforcement.
- **Unlock Protocol** - membership NFT contracts. Relevant contract patterns; requires a backend for enforcement.
- **SIWE** - Sign-In With Ethereum. The session primitive rub3 adapts for local software.
- **Privy / Magic** - custodial wallet auth. Opposite philosophy - they manage keys server-side.

No existing project closes the full machine loop - buy, verify, run, resell - for locally executed software with no backend. That is the gap.

## Current Status

Phase 1 (Proof of Concept) is largely built along the original human-interactive path: wrapper runtime with process supervision, license proof + session crypto, native activation window, on-chain queries, tier-3 cooldown activation, purchase UI, identity models with TBA derivation, and the `Rub3Access` contract, with the ownership invariants above now enforced in bytecode (see [implementation.md](implementation.md) §2.4).

The plan has since been reoriented agent-first (see [implementation.md](implementation.md)): headless activation, USDC/EIP-3009 purchases and the fee-stamping factory have landed, and the next work is the CLI - ahead of WalletConnect and other human-surface polish. Tier-4 device binding and binary encryption are deferred.

See [architecture.md](architecture.md) and [implementation.md](implementation.md) for technical details.
