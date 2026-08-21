# Contract setup

This file owns contract operations: the commands, environment variables, addresses, and runbooks for building, deploying, paying, migrating, and auditing rub3's contracts, including the mechanics of the protocol fee. Why the contracts are shaped this way is argued in [architecture.md](../architecture.md); what is built and what is planned is in [implementation.md](../implementation.md); the Solidity test inventory is in [testing.md](../testing.md).

## Prerequisites

Install Foundry:

```bash
curl -L https://foundry.paradigm.xyz | bash
foundryup
```

Verify:

```bash
forge --version   # forge 1.x.x
anvil --version
```

## Formatting

Every Solidity file in this project is formatted by `forge fmt`, and CI gates it: `forge fmt --check` runs beside `cargo fmt --all -- --check` in the `lint` job of `.github/workflows/ci.yml`, and a drift turns that job red.

```bash
cd contracts
forge fmt          # rewrite every file
forge fmt --check  # what CI runs
```

The version matters. The checked-in formatting was produced by forge v1.5.1, and CI checks it with exactly that version, pinned on the toolchain step of the `lint` job in `.github/workflows/ci.yml`, which also records why. `forge fmt` output is a pure function of the forge binary, so a different local forge may legitimately disagree with the committed tree in either direction: a red local `forge fmt --check` on files you have not touched, or a locally clean run that lands a red gate. That is a version mismatch, not drift, so check `forge --version` before committing a reformat you did not set out to make.

`forge fmt` is not idempotent on a tree it has never formatted: one pass over unformatted source can leave output that a second pass changes again. If `forge fmt --check` is still red immediately after `forge fmt`, run `forge fmt` once more. A tree already in the committed shape converges in a single pass.

### The `[fmt]` section is tuned, not stock

`foundry.toml` carries a `[fmt]` section, and each entry there has a comment saying why it is set. The short version:

| Setting | Value | Why |
|---|---|---|
| `line_length` | `100` | The style these files were hand-written in wraps at 100 columns. Stock 120 unwraps multi-line function headers, event parameter lists and struct literals that were split on purpose. |
| `prefer_compact` | `"none"` | One parameter per line once a call, event or error has to wrap. Stock `"all"` repacks them into one dense line, which hides `indexed` markers and turns a one-parameter change into a whole-line diff. |
| `single_line_imports` | `true` | An import stays one line even when a long dependency path pushes it past `line_length`. |
| `wrap_comments` | `false` | Stock default, restated: doc comments here carry Markdown tables and ASCII rules that a re-wrap would destroy. |
| `sort_imports` | `false` | Stock default, restated: imports are grouped by origin, not sorted. |

### Keeping a block the formatter would flatten

`forge fmt` has no notion of column alignment, so a hand-aligned block is flattened wherever the formatter touches it. That is accepted almost everywhere: the aligned assignment and declaration blocks that used to be here read fine one fact per line, and preserving them all would have meant opting most of the tree out of the formatter it just adopted.

One block is exempt, via the formatter's own `// forgefmt: disable-next-item` marker: `_summary` in `script/Deploy.s.sol`. It prints a fixed-width, label-aligned deploy summary, and its source is laid out to mirror that output, one `console.log` per printed line with the label column aligned inside the format strings. Splitting the calls that run past `line_length` across four lines each breaks that correspondence and makes the printed layout unreadable from the source. The marker is the narrow fix; loosening `line_length` for the whole tree to save one function would not be.

Prefer that marker, scoped to one item, over a config change, whenever a single block needs to keep its shape.

One block keeps its shape without a marker and must go on doing so: the `string[30] memory forbidden` list in `test/Rub3Invariants.t.sol`. `prefer_compact = "none"` and `line_length = 100` leave it one signature per line, and that is the layout the wrapper's mirror test parses when it checks `attest::FORBIDDEN_SIGNATURES` against the Solidity list. A `[fmt]` change that repacked the array would turn that Rust test red rather than drift in silence, but the coupling is worth knowing before touching the settings. The sibling `string[10]` in `test/Rub3CodeRegistry.t.sol` has the same shape and no such reader.

## Local testing with Anvil

No `.env` file needed. Forge tests use Foundry's built-in VM - they run against an in-process EVM with no network.

```bash
cd contracts

# Run all tests
forge test

# Verbose output (shows logs and traces)
forge test -vvv

# Run a single test file
forge test --match-path test/Rub3Access.t.sol -vvv
```

### Deploy locally against Anvil

Start Anvil in a separate terminal:

```bash
anvil
```

Anvil pre-funds ten accounts. The first one's private key is always:

```text
0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
```

Deploy `Rub3Access`:

```bash
cd contracts

TOKEN_NAME="My App License" \
TOKEN_SYMBOL=MAL \
IDENTITY_MODEL=0 \
WRAPPER_HASHES=0x9f2c8b1d3e4a5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8 \
PRICE=50000000000000000 \
forge script script/Deploy.s.sol \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --broadcast
```

The deployed address is printed in the script summary.

`WRAPPER_HASHES` seeds the append-only hash set with the launch release's binaries - comma-separate one hash per platform. It is optional: omit it to deploy with an empty set and add hashes later with `addWrapperHash`. `WRAPPER_HASH` (singular) still works as the one-hash shorthand. The zero hash is *not* accepted as a member - it is the `Unknown` sentinel - so passing it deploys an empty set.

## On-chain setup (Base Sepolia)

### 1. Copy and fill `.env`

```bash
cp .env.example .env
```

Edit `.env`:

| Variable | Where to get it |
|---|---|
| `BASE_SEPOLIA_RPC_URL` | [Alchemy](https://www.alchemy.com), [Infura](https://infura.io), or use the public `https://sepolia.base.org` |
| `DEPLOYER_KEY` | Private key of the deploying wallet (hex, no `0x` prefix) |
| `BASESCAN_API_KEY` | [Basescan](https://basescan.org/register) → API keys |

Fund the deployer wallet with Base Sepolia ETH from the [Base Sepolia faucet](https://docs.base.org/tools/network-faucets).

### 2. Dry run (no broadcast)

Simulate deployment without spending gas:

```bash
source .env

TOKEN_NAME="My App License" \
TOKEN_SYMBOL=MAL \
IDENTITY_MODEL=0 \
WRAPPER_HASHES=0x9f2c8b1d3e4a5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8 \
PRICE=50000000000000000 \
forge script script/Deploy.s.sol \
  --rpc-url $BASE_SEPOLIA_RPC_URL
```

### 3. Broadcast and verify

```bash
source .env

TOKEN_NAME="My App License" \
TOKEN_SYMBOL=MAL \
IDENTITY_MODEL=0 \
WRAPPER_HASHES=0x9f2c8b1d3e4a5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8 \
PRICE=50000000000000000 \
forge script script/Deploy.s.sol \
  --rpc-url $BASE_SEPOLIA_RPC_URL \
  --private-key $DEPLOYER_KEY \
  --broadcast --verify --etherscan-api-key $BASESCAN_API_KEY
```

The contract address appears in the output and at `broadcast/Deploy.s.sol/<chain-id>/run-latest.json`.

## Environment variable reference

| Variable | Required | Description |
|---|---|---|
| `TOKEN_NAME` | yes | ERC-721 name (e.g. `"My App License"`) |
| `TOKEN_SYMBOL` | yes | ERC-721 symbol (e.g. `MAL`) |
| `IDENTITY_MODEL` | yes | `0` = wallet is user_id; `1` = TBA is user_id |
| `PRICE` | yes | Purchase price in wei (the ETH rail) |
| `PRICE_TOKEN` | no | ERC-20 accepted alongside ETH, e.g. USDC. Must implement EIP-3009 **including the Circle FiatTokenV2_2-style `receiveWithAuthorization(address,address,uint256,uint256,uint256,bytes32,bytes)` overload that takes an opaque `bytes signature`**. A token implementing only EIP-3009's `(uint8 v, bytes32 r, bytes32 s)` form is *not* supported: it passes the constructor probe, which reads `authorizationState`, and then reverts for every buyer. See [Which payment tokens work](#which-payment-tokens-work). `0x0` (default) = ETH only |
| `PRICE_AMOUNT` | no | Purchase price in `PRICE_TOKEN`'s smallest unit (USDC has 6 decimals, so `5000000` = 5 USDC). Must be `0` when `PRICE_TOKEN` is unset, or the deploy reverts with `TokenPriceInconsistent`. An independent quote, never converted from `PRICE` |
| `WRAPPER_HASHES` | no | Comma-separated `bytes32` SHA-256s of the launch release's wrapper binaries, one per platform. Seeds the append-only hash set; empty is valid |
| `WRAPPER_HASH` | no | Single-hash shorthand for `WRAPPER_HASHES`. Ignored when `WRAPPER_HASHES` is set; a zero hash means "none" |
| `PREDECESSOR` | no | License contract whose holders may migrate onto this one via `claimFromPredecessor`. Frozen at deploy; `0x0` (default) accepts no migrations. With `FACTORY` set it must additionally be canonical - see [A factory deploy may only succeed a canonical predecessor](#a-factory-deploy-may-only-succeed-a-canonical-predecessor) |
| `SUPPLY_CAP` | no | Max mintable tokens; `0` = uncapped (default). Immutable once deployed |
| `COOLDOWN_BLOCKS` | no | Blocks a *seat* must wait between activations (default `1800` ≈ 1 hr on Base; floor `15` ≈ 30 s is enforced on-chain). Per seat, so a token lands at most `SEATS` activations per window |
| `SEATS` | no | Concurrent sessions one token grants (default `1`, the single-session tier-3 licence; ceiling `64` is enforced on-chain). Immutable and per contract: sell a second seat count by deploying a second contract |
| `SESSION_TTL` | no | Seconds a seat stays taken when nobody releases it (default `86400`; range `300` to `7776000` is enforced on-chain). This is what frees a seat when a fleet instance dies without calling `release` |
| `OWNER` | no | Contract owner address; defaults to broadcaster |
| `FACTORY` | no | `Rub3Factory` to deploy through. Set it to the **published canonical address** to stamp the protocol fee and get an `isDeployed` row. It also constrains `PREDECESSOR`, which has to be canonical on this path. Unset or `0x0` (**the default**) deploys directly: fee-free and **unrecorded**, free to name any predecessor, and nothing fails to tell you so. The canonical address for a chain is published in [`deployments.json`](deployments.json), and is unpopulated on every chain until launch. See [The protocol fee](#the-protocol-fee) and [A factory deploy may only succeed a canonical predecessor](#a-factory-deploy-may-only-succeed-a-canonical-predecessor) |

`script/DeployFactory.s.sol` deploys the factory itself and takes three variables of its own:

| Variable | Required | Description |
|---|---|---|
| `FEE_BPS` | yes | Protocol fee in basis points, within `MIN_FEE_BPS`..`MAX_FEE_BPS` (200-300). No default: this decides rub3's take for every contract the factory ever deploys |
| `TREASURY` | yes | Fee recipient. Must be non-zero, and must be able to receive ETH. Immutable on the factory and on everything it deploys, so the custody requirement and the pre-mainnet proof in [Treasury custody, and the pre-mainnet proof](#treasury-custody-and-the-pre-mainnet-proof) apply before any mainnet factory deploy |
| `PREVIOUS_FACTORY` | no | The `Rub3Factory` this one supersedes. Unset (or `0x0`) for the **first** factory only; set it on every later one, or the contracts the old factory recorded stop being acceptable predecessors on the new one. Immutable, so it cannot be added afterwards |

## Paying in USDC (EIP-3009)

A contract deployed with `PRICE_TOKEN` sells on two rails at once. `purchase(address)` takes ETH; `purchaseWithAuthorization(address,(address,uint256,uint256,bytes32,bytes))` takes a stablecoin payment the buyer authorised off-chain, and **anyone may submit it** - the developer, a facilitator, or the buyer. That is what makes it gasless for the buyer, and an agent holding only USDC can obtain a licence without ever owning ETH.

### Both rails require the exact listed price

**The ETH rail takes the listed price to the wei and nothing else.** `purchase` reverts `IncorrectPayment(sent, required)` unless `msg.value` equals `price()`. Over reverts exactly as under does, and **there is no refund path**: the transaction fails rather than settling and paying anything back.

That rule exists for one event: a price that moves between the read and the transaction. An agent reads `price()`, the developer calls `setPrice`, and the agent's transaction lands against terms it never saw. A price *rise* was always rejected. A price *cut* used to go through silently - the buyer paid the stale higher amount, kept nothing back, and the protocol fee was charged on the excess as well.

**Both rails now fail loudly on that event, and they fail on the same event.** The stablecoin rail already did: `value` is not a parameter, it is the listed price read at execution, so a price move leaves the buyer's signed digest no longer matching and the token rejects the authorization. The ETH rail reaches the same outcome through the exact-amount check. Neither rail can settle a payment against a price the buyer did not read, and no buyer can choose to send more than the listed price, so no fee accrues on a buyer's excess. The one remaining way the fee base can exceed the listed amount is a payment token that credits more than it was asked for; the fee is charged on what arrived, which is the correct reading of that.

What a caller does about it is the same on both: re-read the price and resubmit. For an agent that means a failed transaction it can retry, rather than a successful purchase at a price it never agreed to.

A zero price is not a special case: a contract listing at zero accepts a payment of exactly zero, and `purchase{value: anything else}` reverts.

### Which payment tokens work

**The token rail requires a payment token that exposes the Circle FiatTokenV2_2-style `bytes signature` overload of `receiveWithAuthorization`.** A token that implements only the `(uint8 v, bytes32 r, bytes32 s)` form specified by EIP-3009 is **not supported** and cannot be used as `PRICE_TOKEN`, even though it is a conforming EIP-3009 token.

That is a deliberate trade, made to admit smart-contract wallets. The `bytes` form validates through a signature checker: ECDSA recovery for a 65-byte EOA signature, falling through to EIP-1271 `isValidSignature` for a contract signer. Taking it means an ERC-4337 smart account - which is how a growing share of agent wallets hold funds, and agents are who this rail exists for - can buy a licence on the same single entry point an EOA uses. The split `(v, r, s)` form can only ever serve an EOA. Contract code is frozen at deploy, so supporting both later would mean a new deploy behind the successor pattern; narrower token support was judged the better price.

Nothing on-chain can check this for you. The constructor probe reads `authorizationState`, which both forms have, and a staticcall probe for the overload itself cannot tell "no such function" from "bad signature" - both revert. So **verify it before deploying**, and verify it against the right address.

**Resolve the implementation first.** USDC is deployed behind a `FiatTokenProxy`, and `cast interface` reads the ABI the explorer has for the address you give it. Asked about the proxy address - the one you configure as `PRICE_TOKEN` - it returns the proxy's own ABI (`implementation()`, `admin()`, `upgradeTo`, a fallback) and no `receiveWithAuthorization` at all. An empty grep against a proxy address therefore says nothing about the token; it is the same trap that makes a bytecode selector scan useless here.

```bash
# 1. If the token is a proxy, this prints the implementation address.
#    A non-proxy answers with the zero address or an error - then just use
#    <PRICE_TOKEN> itself in step 2.
cast implementation <PRICE_TOKEN> --rpc-url $RPC

# 2. Ask the implementation what it exposes. The `bytes` overload must be
#    listed alongside (or instead of) the (v, r, s) one.
cast interface <IMPLEMENTATION_OR_TOKEN> --chain <CHAIN> | grep receiveWithAuthorization
```

Read the output rather than the exit status: what you need is a line ending in `bytes signature)` (or `bytes)`). A listing that shows only the `(uint8, bytes32, bytes32)` form means the token cannot be used as `PRICE_TOKEN`. An *empty* result means the check did not answer - an unverified contract, the wrong chain, or a proxy address you have not resolved - and is not itself grounds to conclude anything about the token.

A misconfiguration is not silent at runtime either, and it costs nobody a licence: the wrapper pre-flights the `purchaseWithAuthorization` call as an `eth_call` before broadcasting anything, differing from the broadcast one only in the authorization's validity window, and a token that reverts there selects the ETH rail with a printed reason naming the likely cause. No gas is spent and no activation is lost.

Read what a contract offers (this is also exactly how the wrapper decides which rail to use - a zero token, or a revert from a contract deployed before §2.2, both mean "ETH only"):

```bash
cast call <CONTRACT_ADDRESS> "priceToken()(address)"  --rpc-url $RPC
cast call <CONTRACT_ADDRESS> "priceAmount()(uint256)" --rpc-url $RPC
cast call <CONTRACT_ADDRESS> "price()(uint256)"       --rpc-url $RPC   # the ETH rail
```

The two rails are independently quoted and the contract holds no oracle, so
nothing on-chain relates the wei price to the token amount. On the wrapper side
that bound is the operator's: `RUB3_AGENT_MAX_TOKEN_AMOUNT`, an integer in the
payment token's own smallest unit, must be set before a headless build will use
the stablecoin rail at all. Unset, it buys in ETH and says so; a listed
`priceAmount` above it is refused with exit code 22 rather than switched to the
other rail. The ETH rail carries the same bound, `RUB3_AGENT_MAX_ETH_WEI`, an
integer in wei, weighed after `price()` is read and before the transaction is
sent, so a refusal is the same exit code 22 and costs no gas; it differs in
having a default, 0.1 ETH, because wei is a fixed unit, so an operator who
configures nothing still buys on a bounded rail rather than an unbounded one.
Nothing here changes what the *contract* accepts - either rail is always
spendable by anyone who submits a valid transaction.

Change what is offered to *future* buyers (owner only; it reaches nothing already issued, because a licence is bought once and `ownerOf` is the whole entitlement from then on):

```bash
cast send <CONTRACT_ADDRESS> "setTokenPrice(address,uint256)" <USDC> 5000000 \
  --rpc-url $RPC --private-key $DEPLOYER_KEY
cast send <CONTRACT_ADDRESS> "setTokenPrice(address,uint256)" \
  0x0000000000000000000000000000000000000000 0 \
  --rpc-url $RPC --private-key $DEPLOYER_KEY   # stop offering the rail
```

### Buying with an authorization

Three steps: derive the nonce from the contract, sign `ReceiveWithAuthorization` over the *token's* EIP-712 domain, then have anyone submit it.

```bash
# 1. The nonce is derived by the contract, not chosen freely. It commits to the
#    mint recipient, so a submitter cannot redirect the licence to themselves.
SALT=0x$(openssl rand -hex 32)
NONCE=$(cast call <CONTRACT_ADDRESS> "purchaseAuthorizationNonce(address,bytes32)(bytes32)" \
  <BUYER> $SALT --rpc-url $RPC)

# The authorization's expiry is signed, and step 3 has to submit the same
# number, so bind it here rather than recomputing it from the clock later.
VALID_BEFORE=$(( $(date +%s) + 900 ))

# 2. Sign the authorization. `to` is the licence contract, `value` is
#    `priceAmount()` exactly, and the domain is the token's own - read `name()`
#    and `version()` off the token rather than assuming, then cross-check:
#    the typed data below must hash to the token's DOMAIN_SEPARATOR().
cat > auth.json <<EOF
{
  "types": {
    "EIP712Domain": [
      {"name": "name", "type": "string"},
      {"name": "version", "type": "string"},
      {"name": "chainId", "type": "uint256"},
      {"name": "verifyingContract", "type": "address"}
    ],
    "ReceiveWithAuthorization": [
      {"name": "from", "type": "address"},
      {"name": "to", "type": "address"},
      {"name": "value", "type": "uint256"},
      {"name": "validAfter", "type": "uint256"},
      {"name": "validBefore", "type": "uint256"},
      {"name": "nonce", "type": "bytes32"}
    ]
  },
  "primaryType": "ReceiveWithAuthorization",
  "domain": {
    "name": "USDC", "version": "2",
    "chainId": 8453, "verifyingContract": "<USDC>"
  },
  "message": {
    "from": "<BUYER>", "to": "<CONTRACT_ADDRESS>", "value": "5000000",
    "validAfter": "0", "validBefore": "$VALID_BEFORE",
    "nonce": "$NONCE"
  }
}
EOF
# `cast wallet sign` already returns the 0x-prefixed 65-byte r || s || v
# signature the authorization carries. It is passed through whole - the licence
# contract never splits it, and a smart-contract wallet's EIP-1271 signature
# goes in the same field.
SIG=$(cast wallet sign --data --from-file auth.json --private-key $BUYER_KEY)

# 3. Anyone submits it. `recipient` of 0x0 mints to the buyer who signed - never
#    to the submitter. The buyer spends no gas and no ETH. Every value in the
#    tuple must be the one that was signed, which is why $VALID_BEFORE is bound
#    above rather than recomputed here.
cast send <CONTRACT_ADDRESS> \
  "purchaseWithAuthorization(address,(address,uint256,uint256,bytes32,bytes))" \
  0x0000000000000000000000000000000000000000 \
  "(<BUYER>,0,$VALID_BEFORE,$SALT,$SIG)" \
  --rpc-url $RPC --private-key $SUBMITTER_KEY
```

### Why `receiveWithAuthorization`

`signature` is opaque bytes rather than split `(v, r, s)`, which is what lets an EIP-1271 smart-contract wallet buy on the same entry point as an EOA - see [Which payment tokens work](#which-payment-tokens-work) for what that costs.

EIP-3009 defines two ways to spend the same six signed fields. `transferWithAuthorization` may be submitted by anyone *to the token*, which would let an observer move a buyer's USDC into the licence contract without the mint, burning the nonce and leaving the buyer paid-up with nothing. `receiveWithAuthorization` requires `msg.sender == to`, so the licence contract is the only address that can spend the authorization at all, and spending it always mints. Everything else that could be redirected is pinned the same way: the recipient is bound into the derived nonce, replay is the token's single-use nonce, and the licence contract additionally checks that its balance really rose by the price before minting.

### Withdrawing proceeds

`withdraw(address payable)` takes the developer's ETH balance; `withdrawToken(address,address)` takes their balance of an ERC-20:

```bash
cast send <CONTRACT_ADDRESS> "withdrawToken(address,address)" <USDC> <DEVELOPER> \
  --rpc-url $RPC --private-key $DEPLOYER_KEY
```

On a contract deployed directly, "the developer's balance" is everything the contract holds. On a factory deploy it is everything *less* the protocol fee accrued against it, which is swept separately - see [The protocol fee](#the-protocol-fee).

## The protocol fee

rub3's revenue, stamped at deploy time and immutable thereafter (implementation.md §2.3). Two numbers decide it and both are frozen the moment a contract is constructed:

| Getter | What it is |
|---|---|
| `feeBps()` | The protocol's share of every payment, in basis points. `0` on a directly deployed contract |
| `treasury()` | Where that share accrues. `address(0)` iff `feeBps()` is `0` |

Both are `immutable`. There is no setter on the licence contract, none on the factory, and no path of any kind - developer, factory, or rub3 - that moves either afterwards. **A developer's economics can never change after their contract is deployed.** rub3 changes its take only by deploying a *new* `Rub3Factory`, which affects contracts deployed by that factory and nothing that already exists. The two setters that would break the promise, `setFeeBps(uint16)` and `setTreasury(address)`, are on the forbidden-selector list below and absent from the bytecode.

### Deploying through the factory

Which path you get is decided by one environment variable, `FACTORY`:

| `FACTORY` | Path | Fee | `isDeployed` |
|---|---|---|---|
| unset / `0x0` (**the default**) | direct | none | no row anywhere - **unrecorded** |
| the published canonical address | registered | 200-300 bps, immutable | recorded on the canonical factory |
| some other factory address | that factory's | that factory's, to *its* treasury | recorded only on that factory |

Forgetting `FACTORY` is not an error and does not fail: you get a working, fee-free, unrecorded contract and one line of `console.log` saying so.

**Which factory is canonical is answered by [`contracts/deployments.json`](deployments.json)**, committed beside `canonical-bytecode.json` and keyed by chain id, one entry per chain carrying the factory address, the block it was deployed in, its generation in the `previousFactory` chain, and - since the code registry (below) - that registry's address and deploy block. That file is the only place either answer is published, so a tool that needs one reads it rather than asking a human, keyed by the chain it is asking about:

```bash
# from contracts/
CHAIN_ID=8453 # 84532 for Base Sepolia
jq -er --arg id "$CHAIN_ID" ".chains[\$id].factory // error(\"no canonical factory is published for chain \(\$id)\")" deployments.json
```

Its own `fields` object documents every key, and `scripts/check-deployments.sh` (run by CI) rejects a malformed or half-filled entry. The factory record (`factory`, `deploy_block`, `generation`) and the code registry record (`code_registry`, `code_registry_deploy_block`) are each wholly populated or wholly null, and they are checked independently: they are separate deploys with separate lifecycles, and requiring them to move together would either block one launch on the other or invite a placeholder to unblock it.

**Every entry in it is unpopulated today.** Nothing is deployed to a public network: the contracts are not deployed to mainnet or declared ready for use until the registry is ready, and the factory and the registry launch together. Unpopulated is written as `null` in every field, never as a placeholder address, so there is nothing in the file a script could mistake for a deploy - a `null` factory means "this chain has no canonical factory", and the correct response is to stop, not to substitute another address. That read is the one recipe used everywhere in this repo, and the `// error(...)` half is the point of it: on an unpopulated entry it prints nothing at all and reports the error on stderr, so there is no value to paste and no empty string to hand to forge. The walkthrough below is local, so it takes its factory from step 1 and uses that same read as the live-chain alternative. Either way the deploy refuses unless it has a well-formed address, because `vm.envOr` reads anything it cannot parse exactly as it reads unset, as "no factory": an unpopulated entry, a stray `null`, or an unsubstituted placeholder must never be allowed to degrade into a direct, unrecorded deploy, which is the outcome this file exists to prevent.

```bash
cd contracts

# 1. The factory. FEE_BPS decides rub3's take for everything it will ever deploy.
#    A later factory adds PREVIOUS_FACTORY=<the one it supersedes>, without which
#    that factory's deploys stop being acceptable predecessors on the new one.
FEE_BPS=250 \
TREASURY=0xYourTreasury \
forge script script/DeployFactory.s.sol \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --broadcast

# 2. Any number of licence contracts through it. The fee is not an input here:
#    the factory reads it off itself. This walkthrough is local, and anvil has
#    no canonical factory, so paste the Rub3Factory address step 1 printed.
#    Targeting a live chain, take it from the manifest instead:
#
#      CHAIN_ID=8453 # 84532 for Base Sepolia
#      FACTORY_INPUT=$(jq -er --arg id "$CHAIN_ID" ".chains[\$id].factory // error(\"no canonical factory is published for chain \(\$id)\")" deployments.json)
#
#    The grep below is not decoration. forge reads a FACTORY it cannot parse as
#    an address exactly as it reads an unset one, as "no factory", so a stray
#    "null" or an unsubstituted placeholder would deploy directly and unrecorded
#    without failing. Anything that is not 40 hex digits stops here instead, and
#    so does the all-zero address, which forge reads as "no factory" too. To
#    deploy directly on purpose, drop the FACTORY line entirely.
FACTORY_INPUT=0xYourLocalFactoryFromStep1

FACTORY_ADDR=$(printf '%s' "$FACTORY_INPUT" | grep -Ex '0x[0-9a-fA-F]{40}' | grep -Ev '^0x0{40}$')

TOKEN_NAME="My App License" \
TOKEN_SYMBOL=MAL \
IDENTITY_MODEL=0 \
PRICE=50000000000000000 \
FACTORY=${FACTORY_ADDR:?FACTORY_INPUT is not a usable factory address: paste the one step 1 printed, read it from deployments.json for a live chain, or drop this FACTORY line for a deliberate direct deploy} \
forge script script/Deploy.s.sol \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --broadcast
```

Check what was stamped and that the factory recorded it:

```bash
cast call <LICENCE> "feeBps()(uint16)"          --rpc-url $RPC
cast call <LICENCE> "treasury()(address)"       --rpc-url $RPC
cast call <FACTORY> "isDeployed(address)(bool)" <LICENCE> --rpc-url $RPC
```

`Rub3Factory` also enumerates: `deploymentCount()`, `deploymentAt(uint256)`, `deployments()`, so an agent can list the canonical set without replaying logs.

### Treasury custody, and the pre-mainnet proof

`TREASURY` is the single most consequential argument to `DeployFactory.s.sol`, because it is `immutable` on the factory *and* on every licence contract that factory will ever deploy. There is no setter, no admin path, and no migration that reaches a contract already deployed - so if the treasury key is lost, or the treasury is a contract that later becomes unable to receive, **every fee accrued on every contract that factory deployed is unrecoverable, permanently**. The accrue-don't-push design means that failure never reaches a buyer (`test_accrual_rejectingTreasuryCannotBlockPurchases`: purchases still settle and the developer is still paid in full, only rub3's own sweep fails), which is precisely why it would be discovered late.

**The accepted position: the treasury is a Safe multisig on Base with rotatable owners.** A multisig rather than an EOA because the address can never be changed while the signer set behind it can, so a lost or rotated key is an owner change rather than a stranded factory generation; a Safe specifically because it is the battle-tested contract on this chain and it receives both ETH and ERC-20 without conditions. Nothing about this needs a contract change - the contracts already accept any address that can receive - and no multisig is configured today.

**Pre-mainnet launch requirement, to be performed before the mainnet factory deploy and not after:** on Base Sepolia, deploy a factory whose `TREASURY` is the Safe, deploy a licence through it, complete one purchase on each rail, then call `withdrawFees()` and `withdrawTokenFees(<USDC>)` and confirm the Safe's ETH and USDC balances actually moved. Both sweeps are permissionless, so any key can drive them:

```bash
cast send <LICENCE> "withdrawFees()"                    --rpc-url $BASE_SEPOLIA_RPC_URL --private-key $ANY_KEY
cast send <LICENCE> "withdrawTokenFees(address)" <USDC> --rpc-url $BASE_SEPOLIA_RPC_URL --private-key $ANY_KEY
cast balance <SAFE>                                     --rpc-url $BASE_SEPOLIA_RPC_URL
cast call <USDC> "balanceOf(address)(uint256)" <SAFE>   --rpc-url $BASE_SEPOLIA_RPC_URL
```

**And a second pre-mainnet step, this one against Base mainnet itself and immediately before the factory deploy:** confirm that the address about to be passed as `TREASURY` has code, and that the Safe behind it reads back the owner set and threshold intended.

```bash
cast code $TREASURY                           --rpc-url $BASE_RPC_URL   # must be non-empty
cast call $TREASURY "getOwners()(address[])"  --rpc-url $BASE_RPC_URL
cast call $TREASURY "getThreshold()(uint256)" --rpc-url $BASE_RPC_URL
```

That is a separate step because the testnet proof cannot cover it: the Sepolia Safe is a separate deployment, and code at an address on one chain says nothing about code at the same address on another. An identical address is not evidence either - Safe proxies are deployed through the canonical factory with CREATE2, so the same owners, threshold, and salt nonce give the same address on both chains, and a Safe that exists on Sepolia at that address can still be nothing but a counterfactual on mainnet. So passing the proof above establishes that a Safe receives on both rails and nothing about the value actually typed into the mainnet deploy. `Rub3Factory`'s constructor rejects only `address(0)` and deliberately performs no code check - an EOA treasury stays valid - so a mistyped address, or a counterfactual Safe address not yet deployed on mainnet, is accepted silently and permanently. Accrue-don't-push then means nothing surfaces the mistake until the first sweep, which is the late discovery this section exists to prevent.

A Safe that cannot receive on either rail is a mainnet factory that can never collect on that rail, with no way back. Proving the mechanism on testnet and checking the mainnet address itself before the deploy is the whole mitigation.

### How the split works

The fee runs on-chain inside `purchase()`, on **both** payment rails, and it is the same rule on each: `feeBps` of *what arrived* to the treasury, the remainder to the developer.

- **On the amount received.** ETH: `msg.value`, which the rail requires to equal the listed price exactly (see "Both rails require the exact listed price" above). Stablecoin: the measured balance delta, against a `value` that is the listed amount read at execution. Neither rail lets a buyer choose to pay more than the price, so for any buyer-chosen payment the fee base is the listed price and a listing at zero has no revenue to hide in it. A payment token that credits more than it was asked for is the one remaining way the balance delta can exceed the listed amount, and charging what arrived is the correct reading of that.
- **Rounding favours the developer.** Integer division, so a fee below one wei (or one of the token's smallest units) is zero and the whole payment is the developer's.
- **Accrued, not pushed.** The fee is held in the contract and swept separately rather than transferred to the treasury inside the purchase, so nothing on the buyer's path calls out.

Why each of those three is load-bearing rather than incidental is argued in `architecture.md` → "Why the fee split is shaped this way".

The two balances are disjoint and neither side can reach the other's:

| Call | Who | Moves |
|---|---|---|
| `withdraw(address payable)` | contract owner | `address(this).balance - feesAccrued()` |
| `withdrawToken(address,address)` | contract owner | `balanceOf(this) - tokenFeesAccrued(token)` |
| `withdrawFees()` | anyone | `feesAccrued()`, to `treasury()` |
| `withdrawTokenFees(address)` | anyone | `tokenFeesAccrued(token)`, to `treasury()` |

The two fee sweeps are permissionless because their destination is immutable: the caller decides nothing but the timing, and rub3 collecting should not require rub3 to send a transaction on every contract that ever sold a licence. On a contract with no fee they revert `NoFeeConfigured` rather than burning the balance to `address(0)`.

### What the fee covers, and what it does not

So a developer knows exactly where they stand: **the fee is charged on value that arrives through the contract's payment functions** - `purchase` and `purchaseWithAuthorization`. Value that reaches the contract any other way is never accrued against, and `withdraw` / `withdrawToken` release it in full to the developer. Concretely, that means a direct ERC-20 `transfer` to the licence contract, a `selfdestruct` beneficiary, and a coinbase payout: nothing was taken on them, so all of it is the developer's.

This is a decided position on where the fee's scope ends, not an implementation gap: charging on unaccounted balance would take a cut of mistaken transfers and airdrops, which are not revenue. `test_token_unaccruedBalanceSweepsEntirelyToTheDeveloper` pins the behaviour. The argument for treating the fee as economic rather than technical is in `architecture.md` → "Why the fee split is shaped this way"; what the factory row buys today versus what is still planned is in "The accepted position on fee-free deployment" below.

```bash
# Settle both halves of a sale.
cast send <LICENCE> "withdrawFees()"                     --rpc-url $RPC --private-key $ANY_KEY
cast send <LICENCE> "withdraw(address)" <DEVELOPER>      --rpc-url $RPC --private-key $OWNER_KEY
```

A `ProtocolFeeAccrued(address token, uint256 amount, uint256 fee, uint256 developerAmount)` log is emitted on every payment, with `token == address(0)` meaning ETH. Both shares and their sum are readable from that one log.

### A factory deploy may only succeed a canonical predecessor

`claimFromPredecessor` charges nothing, on purpose: migration must never be taxed. Left unconstrained, `PREDECESSOR` would therefore be a way to launder an entire sale through the registry - sell every licence on a fee-free direct deploy, then deploy the successor **through the factory** naming that contract as predecessor, and every holder claims onto a fee-bearing, `isDeployed`-listed contract with the treasury never paid.

So `deployAccess` accepts a predecessor only when it is **canonical**:

- `address(0)` (no migrations accepted), or
- a contract in this factory's `isDeployed`, or
- a contract in the `isDeployed` of a factory reachable through `previousFactory` (below).

Anything else reverts `PredecessorNotCanonical(address)` before the licence is built. The rule is readable up front, so a deploy never has to be attempted to find out:

```bash
cast call <FACTORY> "isCanonicalPredecessor(address)(bool)" <PREDECESSOR> --rpc-url $RPC
```

**The `previousFactory` chain.** rub3 changes its take by deploying a *new* factory, so contracts an earlier factory recorded have to stay migratable onto the new one. Each factory therefore carries an immutable `previousFactory` - `address(0)` on the first one, the superseded factory's address on every later one (`PREVIOUS_FACTORY` on `DeployFactory.s.sol`). `isCanonicalPredecessor` walks it. The pointer is set once at construction and probed there: a `previousFactory` that cannot answer both `isDeployed(address)` and `previousFactory()` reverts `IncompatiblePreviousFactory(address)` at deploy, because it is immutable afterwards and would otherwise break every predecessor-bearing deploy for as long as the factory existed.

**The walk is bounded** at `MAX_PREDECESSOR_FACTORY_HOPS` (8), the number of earlier factories consulted beyond the current one - nine registries in total. It has to be bounded because it sits on the deploy path and the chain's length is decided by whoever deploys the factories. Eight generations is far past any plausible sequence of rate changes; past it, the oldest registries stop being reachable and their contracts migrate onto a directly deployed successor instead.

**The consequence, stated plainly: a pre-factory contract cannot migrate its holders onto a canonical contract through the factory path.** A contract deployed before any factory existed, or deployed directly, is not in any factory's `isDeployed`, so a successor to it cannot be deployed through a factory. That is the cost of closing the route and it is accepted rather than worked around - the alternative is a registry row available at the far end of any fee-free sale. The launch sequencing keeps the cost small: the contracts are not deployed to mainnet or declared ready for use until the registry is ready, so at launch there is no installed base of mainnet holders whose migration onto a canonical successor is being blocked, and a Base Sepolia pilot could never have been a canonical predecessor of a mainnet deploy under any rule, since `isDeployed` is per factory and per chain. It also means every factory deploy at launch names `PREDECESSOR=0x0`, which is the ordinary case and is unaffected by this rule. Migration itself is untouched: deploy the successor **directly** with `PREDECESSOR=<OLD_CONTRACT>`, and holders claim exactly as described in [Migrating holders to a new contract](#migrating-holders-to-a-new-contract). What that successor does not get is a row in `isDeployed`, which is the same thing any direct deploy does not get.

**The check lives on `Rub3Factory` only, not on the deployer helper.** `Rub3AccessDeployer.deploy` is permissionless and records nothing, so a licence it produces carries no `isDeployed` row and none of the standing the laundering route was after - it is equivalent to deploying the open-source template yourself, which is already free and already fine. Constraining it would restrict a path that grants nothing, while the guard's whole subject is the registry row. It belongs where the registry row is granted. `test_predecessor_deployerHelperIsUnconstrainedAndUnrecorded` pins that split, and `test_predecessor_launderingThroughTheFactoryReverts` is the closed route itself.

### The accepted position on fee-free deployment

Stated plainly, because it is a decided trade-off rather than a gap:

**A direct deploy is fee-free, unrecorded, and fully functional. By design.** Deploying the open-source templates yourself passes `FeeTerms(0, address(0))` and pays nothing, ever. Both payment rails, the mint, the claim path, the ownership invariants, and the wrapper behave identically to a factory deploy. Nothing prevents or penalises it, and nothing will: penalising a direct deploy would need exactly the revocation surface §2.4 forbids. The fee is an economic argument, not a technical lock.

**The factory path stamps 200-300 bps and grants an `isDeployed` row.** That is the entire difference.

**What the row buys today:** a durable, immutable, on-chain record that this contract was deployed through a specific factory - a canonical referent that anyone can check, and the eligibility criterion for the registry and marketplace once they are live.

**What is built, and what is not:** the registry (§3.2) is built and tested and deployed nowhere; the marketplace (§4.3) is not built. Until both are live, the row buys the record and the future eligibility, and no distribution, no verification service, and no liquidity. Do not read the fee's rationale as a description of features that exist.

**The fee does not go live ahead of the registry.** The contracts are not deployed to mainnet, and are not declared ready for use, until the registry is ready: the factory and the registry launch together. So there is no window in which a developer pays a live fee for a carrot that does not exist yet.

**Bytecode identity is never evidence of canonical deployment.** Anyone may call a factory's `accessDeployer()` directly, pass the canonical factory's own `FeeTerms`, and obtain a licence contract whose runtime code is byte-identical to a genuine factory deploy - and which the factory never recorded. Anyone may also deploy their own `Rub3Factory` with their own treasury and any `feeBps` in [200, 300]; its contracts read as fee-bearing and are `isDeployed` **on that factory**. So a verifier must check `isDeployed` on a specific, known factory address, and must never conclude anything from a matching fingerprint or from a non-zero `feeBps()`.

```bash
# The only check that means anything. <CANONICAL_FACTORY> is not interchangeable
# with "some factory" - it must be the published canonical address.
cast call <CANONICAL_FACTORY> "isDeployed(address)(bool)" <LICENCE> --rpc-url $RPC
```

**Where `<CANONICAL_FACTORY>` comes from is [`contracts/deployments.json`](deployments.json)**, keyed by chain id - the one committed place that answers "which factory is canonical here", so the check above has a referent a verifier can reach without trusting whoever handed them an address. Every entry in it is `null` today, because nothing is deployed to a public network yet, and until one is populated "deployed through the factory" still has no verifiable referent outside a deployment you performed yourself. A `null` entry means there is no canonical factory on that chain; it never means "use any factory you were given".

### Why the factory deploys through a helper contract

`Rub3Factory` cannot `new` the licence contract itself: a contract's runtime code has to carry the creation code of everything it deploys, and `Rub3Access`'s alone is over 16 KB against a 24,576-byte runtime limit, which would leave the factory almost nothing for itself. A `new` reached only from a *constructor* lands in the creation code, which is discarded after deployment, so the factory builds one `Rub3AccessDeployer` in its own constructor and keeps its address as an immutable. Consequences worth knowing:

- **The factory's own fingerprint does not pin the licence implementation.** Its runtime code does not contain it. An auditor confirms which implementation a factory deploys by fetching the code at `accessDeployer()` and comparing it against the manifest, then comparing a deployed licence against `Rub3Access`.
- **The deployer is callable by anyone, and that is not a hole.** Calling it directly yields a licence contract the factory never recorded, which is exactly what deploying the template directly already gets you. Trust comes from `isDeployed`, never from who created the contract.
- **The factory's initcode is bounded by EIP-3860 (49,152 bytes), because it carries the deployer's creation code - and so the licence's - inside it.** It is around 22 KB today, so there is headroom, and `test_factory_initcodeFitsUnderEip3860` guards the bound: growing the licence contract fails a test rather than producing an undeployable factory.

## Managing the wrapper hash set after deployment

The hash set is append-only. There is no `setWrapperHash` and no removal - shipping a new build adds to the set, it never invalidates what came before, so every binary a user already downloaded stays verifiable forever.

Add a new release's binary hash:

```bash
cast send <CONTRACT_ADDRESS> \
  "addWrapperHash(bytes32)" \
  <NEW_HASH> \
  --rpc-url $BASE_SEPOLIA_RPC_URL \
  --private-key $DEPLOYER_KEY
```

Flag a compromised build. The reason is recorded on-chain and cannot be empty:

```bash
cast send <CONTRACT_ADDRESS> \
  "revokeWrapperHash(bytes32,string)" \
  <BAD_HASH> "build server compromised 2026-08-01; rebuild from tag v1.0.1" \
  --rpc-url $BASE_SEPOLIA_RPC_URL \
  --private-key $DEPLOYER_KEY
```

Revocation is terminal - a revoked hash can never be re-added, which is what makes the set auditable. Correct a mistaken revocation by publishing a fresh build and adding its hash.

**Revoking a binary hash never affects token validity.** `ownerOf`, `honorsContract` and `activate` do not read the hash set. The holder downloads a patched build and their same license works.

Read the set:

```bash
cast call <CONTRACT_ADDRESS> "wrapperHashList()(bytes32[])"        --rpc-url $RPC
cast call <CONTRACT_ADDRESS> "wrapperHashes(bytes32)(uint8)" <H>   --rpc-url $RPC  # 0=Unknown 1=Valid 2=Revoked
cast call <CONTRACT_ADDRESS> "revocationReason(bytes32)(string)" <H> --rpc-url $RPC
```

## Migrating holders to a new contract

For contract bugs, paid major versions, and chain migration. Both sides opt in, and the holder does the moving.

1. Deploy the successor with `PREDECESSOR=<OLD_CONTRACT>` (immutable - a contract deployed without it accepts no claims). Through a factory, the predecessor must also be canonical - see [A factory deploy may only succeed a canonical predecessor](#a-factory-deploy-may-only-succeed-a-canonical-predecessor); a direct deploy may name any predecessor at all. An address that has no code, or that cannot answer `successor()`, reverts at deploy with `IncompatiblePredecessor(address)`: `predecessor` is immutable, so a mistyped one would brick every holder's claim forever with redeployment the only remedy.
2. Point the old contract at it:

   ```bash
   cast send <OLD_CONTRACT> "setSuccessor(address)" <NEW_CONTRACT> \
     --rpc-url $RPC --private-key $DEPLOYER_KEY
   ```

3. Each holder migrates themselves, from their own wallet:

   ```bash
   cast send <NEW_CONTRACT> "claimFromPredecessor(uint256)" <OLD_TOKEN_ID> \
     --rpc-url $RPC --private-key $HOLDER_KEY
   ```

Nobody else can do step 3 - not the old contract's owner, not the new one's. The old token is not burned or moved (there is no way to do either); the holder ends up with both, and the old contract keeps validating its tokens forever. A claim carries no terms across, because a licence has none to carry: it is bought once, and holding it is the whole entitlement on either contract. Claiming is still the holder's decision alone, and nobody's licence changes if they never make it.

Because this is a snapshot-claim rather than burn-to-mint, **migration can duplicate a seat**, and that is accepted rather than fixed. The holder can claim onto v2, sell the v1 token, and both stay honored via `honorsContract`, so the number of concurrently honored seats is not bounded by either contract's `supplyCap`. Burn-to-mint would bound it, but only by making the predecessor expose a burn - the revocation surface that must not exist - so the no-revocation guarantee takes priority and nothing in the contracts bounds, tracks, or invalidates the duplicate. Size a successor's `SUPPLY_CAP` with that in mind, or deploy v2 with no `PREDECESSOR` at all (a paid major version), which accepts no claims.

The trust rule a wrapper is meant to use - "contract X, or X's successor holding a token claimed from X" - is one call. It is a contract capability today: no shipped wrapper calls it (see `architecture.md` -> "Successor pattern"), so for now it is a check you run yourself:

```bash
cast call <NEW_CONTRACT> "honorsContract(address,uint256)(bool)" <OLD_CONTRACT> <NEW_TOKEN_ID> --rpc-url $RPC
```

It spans exactly one hop, by construction: each contract compares the address you pass against its own immutable `predecessor` and looks no further back. After a second migration (v1 -> v2 -> v3), `v3.honorsContract(v1, <V3_TOKEN_ID>)` is false, so a caller still pinned to v1 does not honor the v3 token. Nobody is stranded by that - no token is ever burned, so the holder's v1 token (and their v2 token, if they claimed one) keeps validating forever on its own contract, which is exactly what a v1-pinned wrapper checks.

## Reproducible builds and canonical fingerprints

The canonical fingerprint of a rub3 contract is the `sha256` of the compiler's `deployedBytecode.object`: the runtime code with every immutable slot left zeroed. Because the immutables are zeroed, the fingerprint is a function of the contract's compiled semantics alone, not of the constructor arguments a particular deploy chose - two deploys of the same code with different `supplyCap` share it. That is the number a buyer's agent compares an on-chain contract against, so it has to be reproducible by somebody who is not the deployer.

### Read this first: the fingerprint is not `sha256(eth_getCode(addr))`

**Any comparator checking a live deploy MUST zero the immutable byte ranges of the code it fetched before hashing it.** `eth_getCode(addr)` returns the runtime code with every immutable slot filled in with the value that deploy's constructor supplied, while `deployedBytecode.object` has those same slots zeroed. Hashing what the chain returns therefore never equals a published fingerprint, on any real deploy, no matter how correct the build is. This is a property of Solidity immutables, not a defect in the contracts or in the manifest.

The ranges to zero are published per contract, so nobody has to derive them:

```bash
jq '.contracts.Rub3Access.immutable_ranges' contracts/canonical-bytecode.json
```

Each entry is a `{"start": <byte offset into the runtime code>, "length": <bytes>}` pair, flattened out of solc's `deployedBytecode.immutableReferences` and sorted by offset. Every slot is 32 bytes wide, one EVM word. The comparison a buyer's agent performs is therefore three steps, in this order:

1. `code = eth_getCode(addr)`, stripped of its `0x` prefix and decoded to bytes.
2. For each published range, overwrite `code[start : start + length]` with zero bytes.
3. `sha256(code)` and compare against `deployed_bytecode_sha256`.

Step 2 is not optional and it is not a refinement. Skipping it fails 100% of the time.

Measured on this branch, `Rub3Access` declares seven immutables, all inherited from `Rub3License`: `identityModel`, `tbaImplementation`, `supplyCap`, `predecessor`, `cooldownBlocks`, and the §2.3 fee terms `feeBps` and `treasury`. Because a single immutable is read at several places in the runtime code, the slot count is higher than the variable count: `Rub3Access` carries 18 ranges, 576 bytes. Those numbers move whenever the code that reads an immutable moves, which is also whenever the fingerprint moves, so the manifest records both together and the drift gate compares both.

`Rub3Factory` is fingerprinted too, with four immutables of its own (`feeBps`, `treasury`, `accessDeployer` and `previousFactory`) across 10 ranges. `Rub3Registry`, the §3.2 discovery registry, carries one immutable (`factory`, the canonical factory whose deploys it will list) across 3 ranges, because a single immutable read at three places in the runtime code reserves a slot at each. `Rub3AccessDeployer` and `Rub3CodeRegistry` have none, so their `immutable_ranges` are empty and their runtime code hashes directly - which is what makes the deployer helper the thing to compare a factory's declared `accessDeployer()` against.

Zeroing an immutable range destroys the constructor argument it held, which is the point: the fingerprint answers "is this the code I expect", not "was this deployed with the terms I expect". Read the terms separately from the contract's own getters (`supplyCap()`, `predecessor()`, `cooldownBlocks()`, and the rest), which is where they are authoritative anyway.

The wrapper performs exactly this comparison before it buys: `crates/rub3-wrapper/src/attest.rs` pins these fingerprints and ranges in the binary and refuses to purchase on a miss. The manual form of the same three steps is under "Auditing the invariants before buying" below; [../implementation.md](../implementation.md) §2.6 records what is built.

### The reproducibility contract

To arrive at the same fingerprint from a checkout of this repository at a given commit, a third party must match all of these. They are all pinned in-tree, so "match" means "do not override them".

| Input | Value | Where it is pinned |
|---|---|---|
| `solc_version` | `0.8.28` (recorded as `0.8.28+commit.7893614a`) | `contracts/foundry.toml` |
| `optimizer` | `true` | `contracts/foundry.toml` |
| `optimizer_runs` | `200` | `contracts/foundry.toml` |
| `evm_version` | `cancun` | `contracts/foundry.toml` |
| `bytecode_hash` | `none` | `contracts/foundry.toml` |
| `openzeppelin-contracts` | `b8c7b9e82d2b340cf82f2913c38e3a0bac2f96ae` | `contracts/foundry.lock` |
| `forge-std` | `0844d7e1fc5e60d77b68e469bff60265f236c398` | `contracts/foundry.lock` |
| dependency revisions, cross-check | the same two revisions | the submodule records in git, `git ls-files -s contracts/lib/` (enforced by the gate) |
| import remappings | `@openzeppelin/contracts/=lib/openzeppelin-contracts/contracts/`, `forge-std/=lib/forge-std/src/` | `contracts/remappings.txt` |

`contracts/foundry.lock` is checked in and is a convenient mirror of the submodule gitlinks; the gitlinks are the git-authoritative record, so `git ls-files -s contracts/lib/` is an independent confirmation path that does not require trusting a generated file. Because the lock is tracked rather than regenerated into every fresh clone, the two could in principle drift apart in git, so the gate cross-checks them: it fails, showing both values, if any recorded revision disagrees with its gitlink or is missing a revision entirely. The confirmation path is enforced, not merely asserted. The gate reads the index rather than `HEAD` so that a staged dependency bump can be regenerated into the manifest in the same pull request; after a checkout the two are identical, so CI compares against exactly what is committed.

One limitation is worth stating plainly. `contracts/foundry.lock` and `git ls-files -s contracts/lib/` both pin only the two top-level dependencies; the revisions of OpenZeppelin's own nested submodules (`erc4626-tests`, `halmos-cheatcodes`, its vendored `forge-std`) are pinned by neither, so they are outside the reproducibility contract described here. This is currently inert: no contract under `contracts/src/` imports any of those paths, so no published fingerprint depends on them. It would need closing before anything under `contracts/src/` did.

The manifest records `solc_version` as the full compiler string including its `+commit` suffix, for example `0.8.28+commit.7893614a`, because the compiler build is part of the build identity and exact reproduction is the whole point. `foundry.toml` pins the `0.8.28` half; `forge` resolves it to that exact commit.

The table is the human-readable summary of the inputs that matter in practice. The authoritative and complete record is the `build` block of [`canonical-bytecode.json`](canonical-bytecode.json), which carries the compiler version alongside solc's own settings object as the compiler itself reported it, so a setting this table does not name (`viaIR`, say, or one a future solc adds) is still recorded next to the fingerprints it produced. That block will churn when solc or forge changes what it emits; the churn is the manifest explaining why a fingerprint moved, which is the point of keeping it.

Two keys are dropped from that settings object. `compilationTarget` is per-contract, not a build input. `remappings` is excluded because only part of it is pinned here. `contracts/remappings.txt` fixes the two remappings the rub3 contracts actually import through, `@openzeppelin/contracts/=` and `forge-std/=`, and those are in the table above. On top of them forge appends three of its own, `erc4626-tests/=`, `halmos-cheatcodes/=` and `openzeppelin-contracts/=`, derived from how deep the submodules happen to be initialised; the artifact's `remappings` array is all five together. Those three describe the environment rather than the build contract, so recording the array would let a checkout difference fail the gate while the fingerprints themselves are byte-identical. Nothing is lost: a remapping that actually changes compiled output moves the fingerprint, which is what the gate exists to catch.

Beyond those recorded inputs nothing else moves the fingerprint: not the `forge` version (it fetches and drives the pinned `solc` rather than compiling anything itself), not the checkout path, not comments in the source.

The `forge` version earns one caveat, because it is the only entry on that list that can still turn the blocking gate red. forge assembles the standard-json input it hands to solc, so a forge release that starts passing an extra setting, or stops passing one, changes the `solc_settings` block the manifest records even though every fingerprint is byte-identical. The gate diffs the whole manifest, so that reads as drift. `.github/workflows/ci.yml` therefore pins `foundry-rs/foundry-toolchain` to a fixed forge version for the `bytecode-fingerprints` job, so an unrelated pull request cannot go red because a new forge shipped that morning. It is one of the two gates in that file that pin; the other is the `forge fmt --check` step of the `lint` job, and the two pin the same version and move together. Bumping the pin is a deliberate act, and `.github/workflows/ci.yml` owns the rule and the procedure for both gates: see its `WHICH FOUNDRY JOBS PIN forge, AND WHY` and `HOW TO BUMP THE PIN` comments.

The checkout path and the comments in the source are the reason `bytecode_hash = "none"` is set. With solc's default (`ipfs`) the compiler appends a CBOR metadata trailer that hashes the metadata JSON, and that JSON covers comment text and source file paths. Measured on these contracts:

| Perturbation | Default `ipfs` | With `bytecode_hash = "none"` |
|---|---|---|
| Add one comment line to `Rub3License.sol` | fingerprint moves | unchanged |
| Rename the source directory `src/` to `contracts_src/` | fingerprint moves | unchanged |
| `optimizer_runs` 200 to 999 | fingerprint moves | fingerprint moves |

The third row is correct behaviour: `optimizer_runs` changes the emitted code, so it is a real input, which is why it is in the table above.

### Reproducing it

```bash
cd contracts && forge build
python3 -c "import json,hashlib; a=json.load(open('out/Rub3Access.sol/Rub3Access.json'));
print(hashlib.sha256(bytes.fromhex(a['deployedBytecode']['object'][2:])).hexdigest())"
```

or, for every deployable contract at once, from the repo root:

```bash
scripts/canonical-bytecode-hashes.sh print
```

`print` applies the same guards as `check`: it reads the source and artifact directories from the resolved foundry config, so it cannot report on a build it did not perform, and it refuses to emit a fingerprint compiled under anything but `bytecode_hash = "none"`. A number it prints is therefore one you can compare against the manifest, not merely whatever the local environment happened to produce.

### The expected values, and the drift gate

The current fingerprints live in [`canonical-bytecode.json`](canonical-bytecode.json), alongside the build inputs they were produced under. Those inputs are read back out of the emitted artifacts' own solc `metadata` blocks rather than out of `foundry.toml` text, so they describe the build that actually produced the hashes: a `[profile.*]` selection or a `FOUNDRY_*` environment override cannot record one set of inputs next to hashes compiled under another. The `bytecode_hash = "none"` guard is driven off that same artifact metadata for the same reason. Because the manifest publishes a single build block covering every fingerprint, the gate reads the compiler version and settings from every discovered contract's artifact and fails, naming both contracts and the field, if any two disagree; one set of build inputs has to hold for the whole of `contracts/src/`. It is JSON because it is consumed by machines as much as by people: the CI gate diffs against it, and the wrapper compiles the same table into the binary (`attest::CANONICAL`, held in step by a unit test that reads this file at compile time), so a `serde`-shaped file beats a prose table or a bare checksum list. Each contract entry carries its `immutable_ranges` alongside its hash for the same reason: a consumer needs both to compare anything against a live deploy, and the gate diffs the whole manifest, so a change in the immutable layout is drift like any other rather than something the check silently ignores. The AST-node keys solc groups those ranges under are dropped, because they are compiler internals with no meaning outside one artifact and a masker needs only the offsets.

CI runs `scripts/canonical-bytecode-hashes.sh check` as a **blocking** job (`.github/workflows/ci.yml` -> `bytecode-fingerprints`). It rebuilds from scratch and fails if any fingerprint, or any pinned build input, differs from the manifest. When a contract change is intended, regenerate and commit the manifest in the same pull request:

```bash
scripts/canonical-bytecode-hashes.sh update
```

Splitting that into a separate commit or pull request defeats the gate, which exists so that a fingerprint can never move without a reviewer seeing it move. A moved fingerprint is also a wrapper change: `attest::CANONICAL` pins a copy of this manifest and gains a **new** row per moved contract, never an overwritten one - see [../AGENTS.md](../AGENTS.md) for the sequence.

New contracts under `contracts/src/` are picked up automatically, at any depth and including a second contract declared inside an existing file. Discovery never reads Solidity: it walks the artifacts `forge build --force` just wrote and keeps every one whose `.metadata.settings.compilationTarget` names a file under `contracts/src/`, which is also where the manifest's `source` field comes from. That set is the build's own account of what it compiled, so a declaration written in an unusual style cannot go unfingerprinted, a contract in `test/` or `script/` cannot leak in, and a contract deleted in the same commit cannot linger (the `--force` build clears the artifact directory first). Abstract bases such as `Rub3License` and interfaces such as `IRub3Predecessor` appear there too, but compile to an empty `deployedBytecode` object and are dropped on that basis rather than by looking for the `abstract` keyword.

Libraries are excluded as well, and deliberately so: the manifest covers the deployable contracts an agent verifies - the licence contracts, since §2.3 `Rub3Factory` and its deployer helper, and since §2.9 `Rub3CodeRegistry`, whose row is what lets a wrapper check the registry before believing it - and a library is not one. It also could not be published honestly here. A library compiles to real runtime code whose leading 20 bytes are a zeroed self-address placeholder that the deployer patches with the library's own address, and that placeholder is not an immutable, so it would appear in no `immutable_ranges` list and the three-step comparison above would fail every time with nothing in the manifest to explain it. An empty `deployedBytecode` object does not catch this case, so the gate reads each artifact's AST and drops anything whose `contractKind` is `library`. That is what `ast = true` in [`foundry.toml`](foundry.toml) is for. It selects extra output rather than changing a compilation input: it is absent from solc's `.metadata.settings`, and enabling it moved no fingerprint, measured rather than assumed. If the AST is ever missing the gate stops rather than guessing, since guessing would mean publishing a library.

The manifest keys contracts by name, so a name declared in two different files under `contracts/src/` fails the gate, naming both files, rather than being silently collapsed to whichever one sorted last. Give every contract under `contracts/src/` a unique name; the migration path is a new deploy of a differently named contract behind the successor pointer, not a second `Rub3Access` in a `v2/` directory.

## The code registry

`Rub3CodeRegistry` is the append-only record of which masked code hashes are genuine rub3 releases (`../implementation.md` §2.9). It exists because a fingerprint table compiled into a wrapper binary can only answer "is this the code *that build* was packed against". A contract deployed from a later template release is absent from that table, and so is a modified copy, so a binary alone has to refuse both. The registry is what tells them apart.

**Nothing is deployed.** `code_registry` is `null` for every chain in [`deployments.json`](deployments.json), and every wrapper therefore refuses on a table miss exactly as it did before the registry existed. `null` means "this chain has no code registry, so there is nothing to ask"; it never means "use one you were given".

### What it does and does not say

- **A record says the code is a genuine rub3 release. It says nothing about a deployment.** Which address runs that code, who deployed it, what the immutables behind the mask were set to, and how the owner will behave are all outside it. "Was this deployed through the canonical factory" is `Rub3Factory.isDeployed` on a specific factory address, and the two questions must not be run together.
- **`Deprecated` means "not recommended for new purchases". It never means "stop honouring".** The record stays whole, offsets included, and a held token is untouched - the registry has no status that could invalidate one, and nothing on any launch path reads it. An agent meeting a deprecated hash warns and buys.
- **Nothing can be removed, overwritten, or moved backwards.** `publish` reverts on a hash that already has a record, deprecated ones included. There is no proxy, no removal, and no un-deprecate. A compromise of the owner key can therefore only *add*, and every addition is a permanent public `Published` event. `test/Rub3CodeRegistry.t.sol` asserts the removal and rewrite surfaces are absent from the deployed runtime bytecode, the way the licence contracts' forbidden selectors are - with its own 10-name list, because the shared 30-name list is about tokens and says nothing about a registry.

### Publishing a release

One transaction per fingerprint, from the owner key. The numbers come out of `canonical-bytecode.json`, which is the point: the registry republishes what the manifest already fixed rather than deciding anything.

```bash
# from contracts/
NAME=Rub3Access
MCH=$(jq -er ".contracts.$NAME.deployed_bytecode_sha256" canonical-bytecode.json)
OFFSETS=$(jq -r "[.contracts.$NAME.immutable_ranges[] | \"(\(.start),\(.length))\"] | join(\",\")" canonical-bytecode.json)
SOLC=$(jq -er '.build.solc_version' canonical-bytecode.json)
COMMIT=0x$(git rev-parse HEAD | sed 's/$/000000000000000000000000/')   # 20-byte sha1, right-padded to bytes32

# role: 0 licence, 1 factory, 2 deployer helper, 3 code registry, 4 discovery registry
cast send <CODE_REGISTRY> \
  "publish(bytes32,uint8,string,string,bytes32,string,(uint32,uint32)[])" \
  "0x$MCH" 0 "$NAME" "<release label>" "$COMMIT" "$SOLC" "[$OFFSETS]" \
  --rpc-url $RPC --private-key $OWNER_KEY
```

Deprecating carries its reason, the way `revokeWrapperHash` does, because a permanent public act should say why:

```bash
cast send <CODE_REGISTRY> "deprecate(bytes32,string)" \
  "0x$MCH" "superseded by <the newer release>" --rpc-url $RPC --private-key $OWNER_KEY
```

### Reading it, and the offsets bootstrap

Computing a masked code hash needs the immutable ranges, and finding the record needs the hash. The registry breaks that circle by publishing the *distinct* tables its releases use - four across today's canonical set, one each for `Rub3Access`, `Rub3Factory` and `Rub3Registry` plus the empty one `Rub3AccessDeployer` and the code registry share - so a verifier fetches the short candidate list once, hashes under each, and looks each result up.

**On a purchase path, read a bounded window of the newest tables.** How many tables exist is the owner key's to choose, and the append-only bound on that key covers what it can publish, not how long a buyer waits for it. `latestOffsetTables(count)` returns at most `count` tables newest-first, clamped, so a verifier asks for the number of candidates it is willing to try and never pays to transfer or decode more; each surviving candidate then costs its own `record` round trip, so hold the same bound over the loop as well - a node need not honour what it was asked for. The wrapper reads `latestOffsetTables(attest::MAX_CANDIDATE_OFFSET_TABLES)` and caps the lookups at the same number.

**Read the newest end, not the first.** This registry is consulted only when the verifier's own pinned table missed, and a miss is by definition about code newer than that build. A budget spent on the oldest layouts would make every release published under a layout past the budget unreadable to every fielded binary, while the first releases stayed readable forever - blinding fielded binaries to the new releases the registry exists to vouch for. None of this is correctness: a table never read is a release refused as unknown, which is what a verifier with no registry already does, so what is at stake is reachability and latency.

`offsetTableWindow(start, count)` walks the set from an arbitrary point in first-use order, for an indexer backfilling, and `offsetTables()` returns everything for a watcher with no deadline.

```bash
cast call <CODE_REGISTRY> "latestOffsetTables(uint256)((uint32,uint32)[][])" 16 --rpc-url $RPC
cast call <CODE_REGISTRY> "offsetTableCount()(uint256)" --rpc-url $RPC
cast call <CODE_REGISTRY> "offsetTableWindow(uint256,uint256)((uint32,uint32)[][])" 0 16 --rpc-url $RPC
cast call <CODE_REGISTRY> "offsetTables()((uint32,uint32)[][])" --rpc-url $RPC   # the whole set
cast call <CODE_REGISTRY> "record(bytes32)((uint8,uint8,string,string,bytes32,string,uint64,(uint32,uint32)[]))" \
  "0x$MCH" --rpc-url $RPC
```

A verifier following the registry's tables must check the ranges against the code it fetched before masking with any of them: each range one 32-byte word, sorted, disjoint, inside the code, and **preceded by a `PUSH32` opcode**. A masked byte is a byte the comparison never looks at, and the `PUSH32` check is what bounds that blind spot: in code a compiler emitted, the byte after a `PUSH32` is its immediate operand, and jump-destination analysis excludes bytes inside push immediates, so the masked bytes cannot execute. `publish` enforces the width, the ordering and the EIP-170 bound on chain; only the fetched code can settle the last one, so only the verifier can. The wrapper does exactly this in `crates/rub3-wrapper/src/attest.rs`.

Be precise about what that last check does *not* settle, because the guarantee is weaker here than it is for a fingerprint table shipped inside a binary. A one-byte lookback does not prove the `PUSH32` byte is an instruction rather than data inside an earlier push's immediate, so code and table shaped together could mask bytes that do execute. Producing such a pair needs the registry's owner key, which has the shorter route of publishing an empty offsets table and letting the hostile code be hashed whole, so this check is what keeps a careless or drifted table honest; the owner key itself is bounded by append-only publication, by every addition being a permanent public `Published` event, and by the registry's own code having to match its published fingerprint first.

### Believing it at all

A verifier must fetch the registry's own runtime code and compare it against `Rub3CodeRegistry` in `canonical-bytecode.json` before believing a word it says. Otherwise the trust rests on whoever put an address in front of you, which is what a published fingerprint exists to avoid. The wrapper carries one registry address per chain and the registry's own masked hash, both frozen into the binary at pack time, and refuses to consult an address whose code does not match.

**It rests on an honest RPC, like everything else here.** A single endpoint that lies returns canonical code for a hostile contract, and lies about the registry's own code in the same breath - the second authority neither dilutes that risk nor compounds it, because one dishonest view of chain state defeats both reads at once. The claim supported is "an honest view of chain state implies canonical code", and no stronger one. A read quorum would close it and is not built.

## The discovery registry

`Rub3Registry` is the discovery surface of `../implementation.md` §3.2: which applications exist, which of them are listable, and in what order a buyer should be shown them.

**It is not `Rub3CodeRegistry`**, and the two are never interchangeable. They share four letters and nothing else:

| Contract | Question it answers | Keyed by | Read by |
|---|---|---|---|
| `Rub3CodeRegistry` | is this bytecode a genuine rub3 release? | masked code hash | a wrapper on the purchase path, when its pinned table missed |
| `Rub3Registry` | which apps exist, and which are listable? | licence contract address | an agent shopping, before it has an address to verify |

Neither is evidence for the other's question. Canonical *code* says nothing about which address runs it, and a listing says nothing about whether the code at that address is genuine. An agent that wants both asks both, in that order: find a candidate here, then verify it against the code registry and the canonical fingerprint before spending.

**Nothing is deployed.** The registry and the factory launch together (`../implementation.md` §2.3), and every entry in [`deployments.json`](deployments.json) is `null`, so there is no discovery registry to read on any public chain yet.

### Discovery, never validity

This is the invariant the contract is built around, and it is the one to check first when reading it. **Delisting removes the badge and the listing. It cannot invalidate a token, end a session, or change what a licence contract charges.**

The proof is an absence rather than a promise. No licence contract in this project reads the registry, holds its address, or has any function that could be made to; `ownerOf`, `honorsContract` and `activate` run on state that lives in the licence contract, and every external call the registry makes is a `view`. `test/Rub3Registry.t.sol` asserts it behaviourally rather than leaving it as a claim: `test_delisting_cannotTouchAHeldTokenOrALiveSession` pulls every discovery lever at once - the owner delists, the registry suspends, and the payment token stops being recognised - and then measures the held token, its validation, its live session, a fresh activation, a fresh purchase and a transfer, all of which survive. `test_registryWrites_leaveTheLicenseContractUntouched` makes the same claim from the other side, snapshotting nine pieces of licence state across every registry write.

That is what bounds a compromise of the registry's owner key: it can hide listings, restore them, and reorder them. It cannot take away anything anyone paid for. There is no state here whose worst case is worse than "the discovery surface is wrong until it is fixed".

`Rub3Registry` is deliberately **not** one of the targets in the forbidden-selector audit under [Auditing the invariants before buying](#auditing-the-invariants-before-buying). That list is about tokens - burns, seizures, pauses, forced migration - and asserting it against a contract that holds no tokens would be a weak claim dressed as a strong one. The registry's invariant is a different one and is tested where it means something.

### What may be listed

`register(address,string,string)` gates on two things, both read live:

1. **A canonical factory deployed the contract.** `isCanonicalDeploy(address)` checks `factory.isDeployed`, then walks `previousFactory` for up to `MAX_FACTORY_GENERATION_HOPS` (8) further generations, so an older generation's deploys stay listable when rub3 ships a new factory. A directly deployed licence is perfectly good software and is simply not listable; that is the trade the fee-free path makes, see [The accepted position on fee-free deployment](#the-accepted-position-on-fee-free-deployment).
2. **The caller owns that licence contract**, by `Rub3License.owner()` at the moment of the call. Authority over a listing therefore follows the licence contract's ownership with nothing to update here: transfer the contract and the new owner controls the listing.

Which factory a registry trusts is fixed at its deploy, from [`deployments.json`](deployments.json) keyed by chain id - the same committed answer everything else on this page reads, carrying the deploy block an indexer starts at and the generation in the `previousFactory` chain. It is immutable, because a registry that could be repointed at another factory could list contracts no rub3 factory ever deployed, which is the only thing a listing here asserts.

The walk is deliberately a second implementation rather than a call to `Rub3Factory.isCanonicalPredecessor`, which performs the same steps for a different question. Binding discovery to the deploy path's rule would mean a future factory tightening its predecessor rule silently delisted applications, which is a validity decision reaching into discovery by the back door.

### The entry is an agent card

`card(address)` returns one machine-readable record: the contract address and its current owner, both price rails, the identity model and its ERC-6551 implementation, the wrapper hash set with each hash's status, the content URI, and the frozen `feeBps` / `treasury`. `cards(start,count)` returns a globally ranked page of them and `cardWindow(start,count)` a bounded one; which of those an agent's spend policy should call is [the next section](#reads-whose-cost-the-caller-controls).

The wrapper hash set is the one field a card does not carry whole. `Rub3License.addWrapperHash` is append-only and uncapped, so without a bound a licence owner would decide what reading their own card cost - and with it what any page of cards containing them cost, which is a reach into unrelated listings' discoverability. A card carries the newest `MAX_CARD_WRAPPER_HASHES` (32) hashes and reports `wrapperHashCount`, the number the licence contract really holds, beside them, plus a `wrapperHashesTruncated` flag saying the same thing without arithmetic. Nothing is capped on the licence contract itself: the full set stays readable there through `wrapperHashCount()` and `wrapperHashAt(index)`. The newest end is kept for the reason `latestOffsetTables` spends its budget there - a buyer checking the build it just downloaded is asking about the most recently published hash.

**Everything on a card except the two presentation fields is read off the licence contract at call time.** Only `appName` and `contentURI` are stored here, because they are the two facts the chain does not carry yet - §3.1 puts `contentURI` on the licence contract, and this field is what a listing quotes until it does. A card can therefore never describe terms the contract no longer offers.

`appName` is required, because a listing nobody can name is not a listing. `contentURI` is not: an empty string means "nothing published yet", which is the honest state while §3.1 is unbuilt, and a mandatory field a developer has no value for is filled with a placeholder that reads like a URI. That is the position [`deployments.json`](deployments.json) already takes on unpublished addresses.

**Both are length-bounded, and the limits are the two numbers a publisher has to plan around:**

| Field | Limit | Constant |
|---|---|---|
| `appName` | 128 bytes | `MAX_APP_NAME_BYTES` |
| `contentURI` | 512 bytes | `MAX_CONTENT_URI_BYTES` |
| `suspend` reason | 512 bytes | `MAX_SUSPENSION_REASON_BYTES` |

Bytes, not characters: that is what the chain charges for, so a multi-byte name fits fewer glyphs. Two limits rather than one because a name and a locator are not the same kind of value - a CIDv1 base32 `ipfs://` URI already runs to about 66 bytes before any path is added. Over-length input reverts `TextTooLong(field, length, limit)`, which names what to shorten and to what, so a rejected registration takes one more attempt rather than a bisection.

The bound exists for the same reason the hash cap above does, and it is checked at the point the text *enters* the contract rather than while a card is assembled. `card` copies both strings, and registration is permissionless for anyone holding a factory deploy, so an unbounded `appName` would have let one listing's owner decide what reading a shared page of cards cost everybody in it - exactly the reach `MAX_CARD_WRAPPER_HASHES` was added to close. Bounding at entry also closes the class rather than the two known instances: a text field added later is bounded by being written through the same helper, which is the only way it gets written at all. `contentURI` stays optional throughout - it passes a length-only sibling of the required-text check, so an empty value is still accepted and still means "nothing published yet".

```bash
cast call <REGISTRY> "isCanonicalDeploy(address)(bool)" <LICENCE> --rpc-url $RPC
cast call <REGISTRY> "rankedListings()(address[])" --rpc-url $RPC
cast call <REGISTRY> "rankedListingWindow(uint256,uint256)(address[])" 0 25 --rpc-url $RPC
cast call <REGISTRY> "isRecognisedRail(address)(bool)" <LICENCE> --rpc-url $RPC

# One listing, whole.
cast call <REGISTRY> \
  "card(address)((address,address,string,string,uint8,bool,bool,uint256,address,uint256,bool,uint8,address,uint16,address,(bytes32,uint8)[],uint256,bool,uint64))" \
  <LICENCE> --rpc-url $RPC
```

Listing an application is one transaction from the licence contract's owner:

```bash
cast send <REGISTRY> "register(address,string,string)" \
  <LICENCE> "My App" "ipfs://<cid>" --rpc-url $RPC --private-key $DEVELOPER_KEY

# Presentation fields only; everything else is changed on the licence contract.
cast send <REGISTRY> "updateListing(address,string,string)" \
  <LICENCE> "My App" "ipfs://<newer cid>" --rpc-url $RPC --private-key $DEVELOPER_KEY

# Withdraw and restore your own listing. Discovery only, both ways.
cast send <REGISTRY> "delist(address)" <LICENCE> --rpc-url $RPC --private-key $DEVELOPER_KEY
cast send <REGISTRY> "relist(address)" <LICENCE> --rpc-url $RPC --private-key $DEVELOPER_KEY
```

### Reads whose cost the caller controls

Registration is permissionless for anyone holding a factory deploy, and nothing is ever removed - `delist` and `suspend` change an entry's flags and leave it in `registered()`. The set therefore only grows, at a rate strangers decide, and the contract cannot be redeployed to fix that later. So every read that would scan all of it has a counterpart whose cost is the caller's:

| Whole set | Bounded | What the bounded one gives up |
|---|---|---|
| `registered()` | `registeredWindow(start,count)` | nothing |
| `rankedListings()`, `rankedListingWindow(start,count)` | `rankedRegistrationWindow(start,count)` | a globally correct order |
| `cards(start,count)` | `cardWindow(start,count)` | the same |

**`rankedListingWindow` and `cards` are in the left column deliberately.** They take a `start` and a `count` and look bounded, but those index into the global ranking, which has to be computed over every registered entry before a page of it can be cut: they bound the response, not the work, and cost what `rankedListings()` costs however small a page is asked for. That is a legitimate read - a globally correct page is worth paying for - and it is written on both functions rather than left to be discovered from a gas limit.

The bounded reads take their cursor over registration order instead, so they scan at most `count` entries and make at most one `priceToken()` read per listed entry among them. **The price is that a bounded page is ranked within its window and not globally**: an entry quoting an unrecognised rail in an early window still comes back before a recognised entry from a later one, because no window can know what the others hold without reading them. Paging through does not reconstruct `rankedListings()`. A caller that needs the global order either pays for it, or collects the windows and ranks them off-chain, where `isRecognisedRail` is the same input the contract uses.

A bounded page is also shorter than `count` whenever its window holds delisted, suspended or never-listed entries, so advance the cursor by `count` rather than by the page length. All of them clamp rather than revert at either end, exactly like `Rub3CodeRegistry.offsetTableWindow`, so one call is enough and no caller needs `registeredCount()` first.

```bash
# A ranked page whose cost does not follow how large the registry has grown.
cast call <REGISTRY> "rankedRegistrationWindow(uint256,uint256)(address[])" 0 25 --rpc-url $RPC
cast call <REGISTRY> "registeredWindow(uint256,uint256)(address[])" 0 25 --rpc-url $RPC
```


### The recognised-token list, and why the rank is read live

The protocol fee accrues in whatever asset a contract lists as its `priceToken`, and the licence contracts deliberately hold no policy about which assets count (see [../architecture.md](../architecture.md#why-the-fee-split-is-shaped-this-way)). That judgement lives in the registry instead: entries quoting a recognised token rank above entries that do not, in a stable partition that keeps registration order inside each group.

**The native rail is always recognised and cannot be un-recognised.** An ETH-only contract quotes no token at all (`priceToken == address(0)`) and its fee accrues in ETH, so the only entries that rank below are those quoting a token rail in an asset the registry does not recognise. `setTokenRecognised` rejects `address(0)` in both directions: allowing it would put the entire ETH-only population one owner transaction away from the bottom of the list.

The list is registry-maintained rather than baked into a licence contract precisely so it can move as tokens do - deprecated, migrated, or newly worth accruing a fee in - without touching anything already deployed.

**The rank reads `priceToken` live, on every call, and this is the part that would be wrong if it were done the obvious way.** `setTokenPrice(address,uint256)` stays owner-callable on a licence contract for its whole life, so a contract registered while priced in a recognised token can switch the block afterwards. A rank frozen at registration would go on advertising that contract on a quote it no longer honours, and no event the registry emits would say so. `test_rank_followsAPostRegistrationTokenPriceChange` is the test for exactly that sequence: two entries swap quotes after registration and the order swaps with them, so a snapshot implementation passes every other test in the file and fails that one.

An off-chain indexer that would rather not re-read everything has the equivalent: re-validate an entry whenever the licence contract emits `TokenPriceUpdated`. What it must not do is read the quote once at registration and keep it.

Demotion is discovery, bound by the same invariant as delisting: an entry that drops to the bottom has lost placement and nothing else.

```bash
# Curation, from the registry owner's key.
cast send <REGISTRY> "setTokenRecognised(address,bool)" <TOKEN> true \
  --rpc-url $RPC --private-key $OWNER_KEY
cast call <REGISTRY> "recognisedTokens()(address[])" --rpc-url $RPC
cast call <REGISTRY> "isRecognisedToken(address)(bool)" <TOKEN> --rpc-url $RPC

# Withholding and restoring the badge. A suspension carries its reason, the way
# revokeWrapperHash and deprecate do, and the listing's own owner cannot undo it.
cast send <REGISTRY> "suspend(address,string)" <LICENCE> "<why>" \
  --rpc-url $RPC --private-key $OWNER_KEY
cast send <REGISTRY> "reinstate(address)" <LICENCE> --rpc-url $RPC --private-key $OWNER_KEY
```

Ownership is `Ownable2Step` and cannot be renounced: abandoning it would freeze the recognised-token list at whatever it happened to say, permanently and with no recovery, on a chain where the assets it names can be deprecated or migrated.

## Auditing the invariants before buying

An agent can verify the ownership guarantees against the deployed bytecode rather than trusting the source. `test/Rub3Invariants.t.sol` runs exactly this audit; the full property-by-property breakdown, including which properties are convention rather than bytecode, is in [../architecture.md](../architecture.md#ownership-invariants-all-license-contracts).

There are two checks here and they are not equals. **The fingerprint comparison is the one that decides anything**; the selector scan below it is a diagnostic that makes a refusal legible. Run them in that order.

### The check that decides: compare the masked code hash

Fetch the runtime code, zero the immutable ranges published for the contract, hash, and compare against the canonical fingerprint - the three steps spelled out under "Read this first" above. A match says the deployed code is the template built from this repository, byte for byte in every position that can execute. It is indifferent to how the contract is named, how large it grows, and what a modified copy chose to call its extra function.

```bash
cast code <CONTRACT_ADDRESS> --rpc-url $RPC > /tmp/code.hex
python3 - Rub3Access /tmp/code.hex <<'EOF'
import hashlib, json, sys
name, path = sys.argv[1], sys.argv[2]
code = bytearray.fromhex(open(path).read().strip().removeprefix("0x"))
record = json.load(open("canonical-bytecode.json"))["contracts"][name]
for r in record["immutable_ranges"]:
    code[r["start"] : r["start"] + r["length"]] = bytes(r["length"])
match = hashlib.sha256(code).hexdigest() == record["deployed_bytecode_sha256"]
print("MATCH" if match else "MISMATCH")
EOF
```

The wrapper does this itself before it spends anything: `crates/rub3-wrapper/src/attest.rs` pins the same fingerprints and ranges in the binary, both purchase paths refuse on a miss before anything is signed (`headless::purchase` with exit code 23, the activation window with a refusal screen in place of the purchase screen), and a unit test fails if the pinned table and this manifest drift apart. See [../implementation.md](../implementation.md) §2.6 and §2.8.

What a match does **not** say is worth as much as what it does. Zeroing the immutable ranges destroys the constructor arguments they held, so a match is silent about `identityModel`, `tbaImplementation`, `supplyCap`, `cooldownBlocks`, `predecessor` and the fee terms; read those from the contract's own getters and check them against your own policy. It says nothing about how a canonical contract's owner will use the powers the invariants deliberately preserve (`setPrice`, `setSuccessor`, `revokeWrapperHash`, `withdraw`). And it rests entirely on the RPC endpoint answering `eth_getCode` honestly: the claim it supports is "an honest view of chain state implies canonical code", and no stronger one. A mismatch is likewise not an accusation - a contract deployed from a later release of these templates than the comparator knows about looks exactly the same way. That is what the code registry above answers: a hash absent from your table but published there is a newer release, and a hash absent from both is the one to refuse. Until a registry is deployed there is nothing to ask, and a miss is simply a miss.

The check cannot be defeated by swapping code at the address after it is read. `evm_version = "cancun"`, and under EIP-6780 `SELFDESTRUCT` only deletes an account created in the same transaction, so there is no window between the read and the purchase and no metamorphic-contract attack. Pre-Cancun this would not have held.

### The diagnostic: scan for known revocation selectors

Fetch the runtime code and confirm the revocation selectors are absent. This is nearly free and it is worth running, but read the paragraph after it before drawing any conclusion from silence.

```bash
CODE=$(cast code <CONTRACT_ADDRESS> --rpc-url $RPC)
for SIG in "burn(uint256)" "burn(address,uint256)" "burnFrom(address,uint256)" \
           "adminTransfer(address,address,uint256)" \
           "forceTransfer(address,address,uint256)" "seize(uint256)" "clawback(uint256)" \
           "pause()" "unpause()" "paused()" "setPaused(bool)" \
           "revoke(uint256)" "revokeToken(uint256)" "invalidate(uint256)" \
           "upgradeTo(address)" "upgradeToAndCall(address,bytes)" "initialize()" \
           "setWrapperHash(bytes32)" "removeWrapperHash(bytes32)" \
           "unrevokeWrapperHash(bytes32)" \
           "forceMigrate(uint256,address)" "setPredecessor(address)" \
           "setPreviousFactory(address)" \
           "setFeeBps(uint16)" "setTreasury(address)" \
           "setSeatsPerToken(uint256)" "setMaxConcurrentSessions(uint256,uint256)" \
           "setSessionTtl(uint256)" \
           "revokeSeat(uint256,uint256)" "clearSeats(uint256)"; do
  SEL=$(cast sig "$SIG" | sed 's/^0x//')
  case "$CODE" in *"$SEL"*) echo "PRESENT: $SIG";; esac
done
```

Silence means exactly one thing: none of those 30 known revocation selectors appears in the deployed runtime bytecode. **It is not evidence that no revocation surface exists**, and it should never be reported as though it were. The list is a blacklist of *names*, and a modified copy of these templates can expose the same power under a name nobody guessed - `seizeToken(uint256)`, or something as dull as `reconcileLedger(uint256,address)` - and pass this scan in silence. The scan also weakens with every legitimate function the contracts gain, because one more plausible-looking owner function is far less conspicuous among fifteen than among six.

Its real job is the failure message. When the fingerprint comparison rejects a contract, this scan is what turns "unrecognised code" into "the contract exposes `seize(uint256)`" - a finding a human can act on. That is the whole of its value, and the wrapper keeps it for exactly that reason and labels it a diagnostic in the code.

Sanity-check the method itself against a selector that *is* there - `cast sig "activate(uint256)"` should be found. The same list is written out in five places, so if you change it, sweep them all: the `string[N]` array in `test/Rub3Invariants.t.sol`, the loop above, the bytecode table in [../architecture.md](../architecture.md), `attest::FORBIDDEN_SIGNATURES` in the wrapper, and the counts stated in [../implementation.md](../implementation.md) §2.4.

## Planned contract evolution

The contracts above are the current, working set - including the §2.4 ownership invariants (append-only hash set, successor pattern), the §2.2 stablecoin rail, the §2.3 factory and protocol fee, and the §2.9 code registry, all of which have landed. The agent-first plan (see [../implementation.md](../implementation.md)) adds the following - all as **new deploys**, never in-place upgrades:

- **`contentURI`** (§3.1) - content-addressed binary location on-chain, making the contract a complete distribution record
- **`Rub3Metered`** (§4.1) - per-launch / per-session micropayment billing
- **`Rub3Registry`** (§3.2) - discovery and verification, never validity; entries double as ERC-8004-style agent cards. Distinct from the `Rub3CodeRegistry` above, which records which *code* is a genuine rub3 release and lists no products

Invariants for every license contract, present and future: no burn, no admin transfer, no pause on validation reads, no proxies. Evolution changes what is offered going forward, never what was granted.
