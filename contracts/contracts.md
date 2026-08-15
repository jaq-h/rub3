# Contract setup

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

## Local testing with Anvil

No `.env` file needed. Forge tests use Foundry's built-in VM — they run against an in-process EVM with no network.

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

```
0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
```

Deploy `Rub3Access`:

```bash
cd contracts

CONTRACT_TYPE=access \
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

Deploy `Rub3Subscription` (30-day period):

```bash
cd contracts

CONTRACT_TYPE=subscription \
TOKEN_NAME="My App Sub" \
TOKEN_SYMBOL=MAS \
IDENTITY_MODEL=0 \
WRAPPER_HASHES=0x9f2c8b1d3e4a5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8 \
PRICE=10000000000000000 \
PERIOD=2592000 \
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

CONTRACT_TYPE=access \
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

CONTRACT_TYPE=access \
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
| `CONTRACT_TYPE` | yes | `access` or `subscription` |
| `TOKEN_NAME` | yes | ERC-721 name (e.g. `"My App License"`) |
| `TOKEN_SYMBOL` | yes | ERC-721 symbol (e.g. `MAL`) |
| `IDENTITY_MODEL` | yes | `0` = wallet is user_id; `1` = TBA is user_id |
| `PRICE` | yes | Purchase price in wei (the ETH rail) |
| `PRICE_TOKEN` | no | ERC-20 accepted alongside ETH, e.g. USDC. Must implement EIP-3009 **including the Circle FiatTokenV2_2-style `receiveWithAuthorization(address,address,uint256,uint256,uint256,bytes32,bytes)` overload that takes an opaque `bytes signature`**. A token implementing only EIP-3009's `(uint8 v, bytes32 r, bytes32 s)` form is *not* supported: it passes the constructor probe, which reads `authorizationState`, and then reverts for every buyer. See [Which payment tokens work](#which-payment-tokens-work). `0x0` (default) = ETH only |
| `PRICE_AMOUNT` | no | Purchase price in `PRICE_TOKEN`'s smallest unit (USDC has 6 decimals, so `5000000` = 5 USDC). Must be `0` when `PRICE_TOKEN` is unset, or the deploy reverts with `TokenPriceInconsistent`. An independent quote, never converted from `PRICE` |
| `WRAPPER_HASHES` | no | Comma-separated `bytes32` SHA-256s of the launch release's wrapper binaries, one per platform. Seeds the append-only hash set; empty is valid |
| `WRAPPER_HASH` | no | Single-hash shorthand for `WRAPPER_HASHES`. Ignored when `WRAPPER_HASHES` is set; a zero hash means "none" |
| `PREDECESSOR` | no | License contract whose holders may migrate onto this one via `claimFromPredecessor`. Frozen at deploy; `0x0` (default) accepts no migrations |
| `SUPPLY_CAP` | no | Max mintable tokens; `0` = uncapped (default). Immutable once deployed |
| `COOLDOWN_BLOCKS` | no | Blocks between activations per token (default `1800` ≈ 1 hr on Base; floor `15` ≈ 30 s is enforced on-chain) |
| `OWNER` | no | Contract owner address; defaults to broadcaster |
| `PERIOD` | subscription only | Subscription length in seconds |

## Paying in USDC (EIP-3009)

A contract deployed with `PRICE_TOKEN` sells on two rails at once. `purchase(address)` keeps taking ETH exactly as before; `purchaseWithAuthorization(address,(address,uint256,uint256,bytes32,bytes))` takes a stablecoin payment the buyer authorised off-chain, and **anyone may submit it** - the developer, a facilitator, or the buyer. That is what makes it gasless for the buyer, and an agent holding only USDC can obtain a licence without ever owning ETH.

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

A misconfiguration is not silent at runtime either, and it costs nobody a licence: the wrapper pre-flights the exact `purchaseWithAuthorization` call as an `eth_call` before broadcasting anything, and a token that reverts there selects the ETH rail with a printed reason naming the likely cause. No gas is spent and no activation is lost.

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
other rail. Nothing here changes what the *contract* accepts - either rail is
always spendable by anyone who submits a valid transaction.

Change what is offered to *future* buyers (owner only; it reaches nothing already issued, and a subscription snapshots both rails per token at mint):

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

Renewing a subscription is the same shape against `renewAuthorizationNonce(uint256,bytes32)` and `renewWithAuthorization(uint256,(...))`, charging that token's frozen `renewPriceAmount` of its frozen `renewPriceToken` - never the current listing.

### Why `receiveWithAuthorization`

`signature` is opaque bytes rather than split `(v, r, s)`, which is what lets an EIP-1271 smart-contract wallet buy on the same entry point as an EOA - see [Which payment tokens work](#which-payment-tokens-work) for what that costs.

EIP-3009 defines two ways to spend the same six signed fields. `transferWithAuthorization` may be submitted by anyone *to the token*, which would let an observer move a buyer's USDC into the licence contract without the mint, burning the nonce and leaving the buyer paid-up with nothing. `receiveWithAuthorization` requires `msg.sender == to`, so the licence contract is the only address that can spend the authorization at all, and spending it always mints. Everything else that could be redirected is pinned the same way: the recipient (and, for renewals, the token id) is bound into the derived nonce, replay is the token's single-use nonce, and the licence contract additionally checks that its balance really rose by the price before minting.

### Withdrawing proceeds

`withdraw(address payable)` takes the ETH balance; `withdrawToken(address,address)` takes the whole balance of an ERC-20:

```bash
cast send <CONTRACT_ADDRESS> "withdrawToken(address,address)" <USDC> <TREASURY> \
  --rpc-url $RPC --private-key $DEPLOYER_KEY
```

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

**Revoking a binary hash never affects token validity.** `ownerOf`, `isValid`, and `activate` do not read the hash set. The holder downloads a patched build and their same license works.

Read the set:

```bash
cast call <CONTRACT_ADDRESS> "wrapperHashList()(bytes32[])"        --rpc-url $RPC
cast call <CONTRACT_ADDRESS> "wrapperHashes(bytes32)(uint8)" <H>   --rpc-url $RPC  # 0=Unknown 1=Valid 2=Revoked
cast call <CONTRACT_ADDRESS> "revocationReason(bytes32)(string)" <H> --rpc-url $RPC
```

## Migrating holders to a new contract

For contract bugs, paid major versions, and chain migration. Both sides opt in, and the holder does the moving.

1. Deploy the successor with `PREDECESSOR=<OLD_CONTRACT>` (immutable - a contract deployed without it accepts no claims). The successor must be the same model as the predecessor: `CONTRACT_TYPE=access` with a subscription predecessor, or `CONTRACT_TYPE=subscription` with an access one, reverts at deploy with `IncompatiblePredecessor(address)`. Cross-model succession is impossible by construction, not a judgement call - both constructors probe `period()` as the discriminator, the subscription requiring the predecessor to answer it and the access license requiring it to fail.
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

   Subscription holders should read the successor's terms first, because the claim is the moment they accept them:

   ```bash
   cast call <NEW_CONTRACT> "period()(uint256)" --rpc-url $RPC
   cast call <NEW_CONTRACT> "price()(uint256)"  --rpc-url $RPC
   ```

Nobody else can do step 3 - not the old contract's owner, not the new one's. The old token is not burned or moved (there is no way to do either); the holder ends up with both, and the old contract keeps validating its tokens forever. Subscriptions carry their remaining time and their snapshotted `renewPrice` across, but **not** `period`, which is immutable per contract: the successor's own `period` decides what the carried price buys from then on, so a successor with a shorter period raises the effective rate without the price moving. That takes nothing already granted - the old token keeps validating at its original terms forever - which is why step 3 is the holder's decision and why they should read the successor's `period` and `price` first.

Because this is a snapshot-claim rather than burn-to-mint, **migration can duplicate a seat**, and that is accepted rather than fixed. The holder can claim onto v2, sell the v1 token, and both stay honored via `honorsContract`, so the number of concurrently honored seats is not bounded by either contract's `supplyCap`. Burn-to-mint would bound it, but only by making the predecessor expose a burn - the revocation surface that must not exist - so the no-revocation guarantee takes priority and nothing in the contracts bounds, tracks, or invalidates the duplicate. Size a successor's `SUPPLY_CAP` with that in mind, or deploy v2 with no `PREDECESSOR` at all (a paid major version), which accepts no claims.

The wrapper's trust rule - "contract X, or X's successor holding a token claimed from X" - is one call:

```bash
cast call <NEW_CONTRACT> "honorsContract(address,uint256)(bool)" <OLD_CONTRACT> <NEW_TOKEN_ID> --rpc-url $RPC
```

It spans exactly one hop, by construction: each contract compares the address you pass against its own immutable `predecessor` and looks no further back. After a second migration (v1 -> v2 -> v3), `v3.honorsContract(v1, <V3_TOKEN_ID>)` is false, so a wrapper still pinned to v1 does not honor the v3 token. Nobody is stranded by that - no token is ever burned, so the holder's v1 token (and their v2 token, if they claimed one) keeps validating forever on its own contract, which is exactly what a v1-pinned wrapper checks.

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

Measured on this branch, `Rub3Access` declares five immutables (`identityModel`, `tbaImplementation`, `supplyCap`, `predecessor`, `cooldownBlocks`) inherited from `Rub3License`, and `Rub3Subscription` declares those five plus its own `period`, six in total. Because a single immutable is read at several places in the runtime code, the slot count is higher than the variable count: `Rub3Access` carries 13 ranges (416 bytes) and `Rub3Subscription` 17 (544 bytes). Those numbers move whenever the code that reads an immutable moves, which is also whenever the fingerprint moves, so the manifest records both together and the drift gate compares both.

Zeroing an immutable range destroys the constructor argument it held, which is the point: the fingerprint answers "is this the code I expect", not "was this deployed with the terms I expect". Read the terms separately from the contract's own getters (`supplyCap()`, `period()`, `predecessor()`, and the rest), which is where they are authoritative anyway.

Nothing in this repository performs that comparison today. This section, and the ranges in the manifest, exist so that the follow-on work implementing it has an unambiguous contract to implement against.

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

The `forge` version earns one caveat, because it is the only entry on that list that can still turn the blocking gate red. forge assembles the standard-json input it hands to solc, so a forge release that starts passing an extra setting, or stops passing one, changes the `solc_settings` block the manifest records even though every fingerprint is byte-identical. The gate diffs the whole manifest, so that reads as drift. `.github/workflows/ci.yml` therefore pins `foundry-rs/foundry-toolchain` to a fixed forge version for the `bytecode-fingerprints` job alone, so an unrelated pull request cannot go red because a new forge shipped that morning. Bumping that pin is a deliberate act: raise it and commit the regenerated manifest in the same pull request.

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

The current fingerprints live in [`canonical-bytecode.json`](canonical-bytecode.json), alongside the build inputs they were produced under. Those inputs are read back out of the emitted artifacts' own solc `metadata` blocks rather than out of `foundry.toml` text, so they describe the build that actually produced the hashes: a `[profile.*]` selection or a `FOUNDRY_*` environment override cannot record one set of inputs next to hashes compiled under another. The `bytecode_hash = "none"` guard is driven off that same artifact metadata for the same reason. Because the manifest publishes a single build block covering every fingerprint, the gate reads the compiler version and settings from every discovered contract's artifact and fails, naming both contracts and the field, if any two disagree; one set of build inputs has to hold for the whole of `contracts/src/`. It is JSON because it is consumed by machines as much as by people: the CI gate diffs against it, and the wrapper will later compile the same table into the binary, so a `serde`-shaped file beats a prose table or a bare checksum list. Each contract entry carries its `immutable_ranges` alongside its hash for the same reason: a consumer needs both to compare anything against a live deploy, and the gate diffs the whole manifest, so a change in the immutable layout is drift like any other rather than something the check silently ignores. The AST-node keys solc groups those ranges under are dropped, because they are compiler internals with no meaning outside one artifact and a masker needs only the offsets.

CI runs `scripts/canonical-bytecode-hashes.sh check` as a **blocking** job (`.github/workflows/ci.yml` -> `bytecode-fingerprints`). It rebuilds from scratch and fails if any fingerprint, or any pinned build input, differs from the manifest. When a contract change is intended, regenerate and commit the manifest in the same pull request:

```bash
scripts/canonical-bytecode-hashes.sh update
```

Splitting that into a separate commit or pull request defeats the gate, which exists so that a fingerprint can never move without a reviewer seeing it move.

New contracts under `contracts/src/` are picked up automatically, at any depth and including a second contract declared inside an existing file. Discovery never reads Solidity: it walks the artifacts `forge build --force` just wrote and keeps every one whose `.metadata.settings.compilationTarget` names a file under `contracts/src/`, which is also where the manifest's `source` field comes from. That set is the build's own account of what it compiled, so a declaration written in an unusual style cannot go unfingerprinted, a contract in `test/` or `script/` cannot leak in, and a contract deleted in the same commit cannot linger (the `--force` build clears the artifact directory first). Abstract bases such as `Rub3License` and interfaces such as `IRub3Predecessor` appear there too, but compile to an empty `deployedBytecode` object and are dropped on that basis rather than by looking for the `abstract` keyword.

Libraries are excluded as well, and deliberately so: the manifest covers the deployable license contracts an agent verifies, and a library is not one. It also could not be published honestly here. A library compiles to real runtime code whose leading 20 bytes are a zeroed self-address placeholder that the deployer patches with the library's own address, and that placeholder is not an immutable, so it would appear in no `immutable_ranges` list and the three-step comparison above would fail every time with nothing in the manifest to explain it. An empty `deployedBytecode` object does not catch this case, so the gate reads each artifact's AST and drops anything whose `contractKind` is `library`. That is what `ast = true` in [`foundry.toml`](foundry.toml) is for. It selects extra output rather than changing a compilation input: it is absent from solc's `.metadata.settings`, and enabling it left both fingerprints byte-identical, measured rather than assumed. If the AST is ever missing the gate stops rather than guessing, since guessing would mean publishing a library.

The manifest keys contracts by name, so a name declared in two different files under `contracts/src/` fails the gate, naming both files, rather than being silently collapsed to whichever one sorted last. Give every contract under `contracts/src/` a unique name; the migration path is a new deploy of a differently named contract behind the successor pointer, not a second `Rub3Access` in a `v2/` directory.

## Auditing the invariants before buying

An agent can verify the ownership guarantees against the deployed bytecode rather than trusting the source. `test/Rub3Invariants.t.sol` runs exactly this audit; the full property-by-property breakdown, including which properties are convention rather than bytecode, is in [../architecture.md](../architecture.md#ownership-invariants-all-license-contracts).

The check: fetch the runtime code and confirm the revocation selectors are absent.

```bash
CODE=$(cast code <CONTRACT_ADDRESS> --rpc-url $RPC)
for SIG in "burn(uint256)" "burn(address,uint256)" "burnFrom(address,uint256)" \
           "adminTransfer(address,address,uint256)" \
           "forceTransfer(address,address,uint256)" "seize(uint256)" "clawback(uint256)" \
           "pause()" "unpause()" "paused()" "setPaused(bool)" \
           "revoke(uint256)" "revokeToken(uint256)" "invalidate(uint256)" \
           "setExpiresAt(uint256,uint256)" "setRenewPrice(uint256,uint256)" \
           "setRenewPriceToken(uint256,address)" "setRenewPriceAmount(uint256,uint256)" \
           "setPeriod(uint256)" \
           "upgradeTo(address)" "upgradeToAndCall(address,bytes)" "initialize()" \
           "setWrapperHash(bytes32)" "removeWrapperHash(bytes32)" \
           "unrevokeWrapperHash(bytes32)" \
           "forceMigrate(uint256,address)" "setPredecessor(address)"; do
  SEL=$(cast sig "$SIG" | sed 's/^0x//')
  case "$CODE" in *"$SEL"*) echo "PRESENT: $SIG";; esac
done
```

Silence means exactly one thing: none of those 27 known revocation selectors appears in the deployed runtime bytecode. It is not proof that no revocation surface exists. The list is a blacklist of names, and a modified copy of these templates can expose the same power under a name nobody guessed - `seizeToken(uint256)`, say - and pass this scan in silence. Full assurance needs a name-independent check: compare the deployed runtime bytecode against the canonical fingerprint of the template built from this repo, after zeroing the immutable ranges published for it. Those fingerprints and ranges are now pinned - see "Reproducible builds and canonical fingerprints" above - but nothing in this repository performs the comparison yet.

Sanity-check the method itself against a selector that *is* there - `cast sig "activate(uint256)"` should be found.

## Planned contract evolution

The contracts above are the current, working set - including the §2.4 ownership invariants (append-only hash set, successor pattern, per-token renewal snapshot) and the §2.2 stablecoin rail, both of which have landed. The agent-first plan (see [../implementation.md](../implementation.md)) adds the following - all as **new deploys**, never in-place upgrades:

- **`Rub3Factory`** (§2.3) — canonical deployment path; stamps an immutable 2–3% protocol fee split into `purchase()`/`renew()`; registry and marketplace list factory deploys only
- **`contentURI`** (§3.1) — content-addressed binary location on-chain, making the contract a complete distribution record
- **Concurrent seats** (§3.4) — `maxConcurrentSessions[tokenId] = K` generalizing `activeSessionId` for agent fleets
- **`Rub3Metered`** (§4.1) — per-launch / per-session micropayment billing
- **`Rub3Registry`** (§3.2) — discovery and verification, never validity; entries double as ERC-8004-style agent cards

Invariants for every license contract, present and future: no burn, no admin transfer, no pause on validation reads, no proxies. Evolution changes what is offered going forward, never what was granted.
