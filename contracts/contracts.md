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
| `PRICE` | yes | Purchase price in wei |
| `WRAPPER_HASHES` | no | Comma-separated `bytes32` SHA-256s of the launch release's wrapper binaries, one per platform. Seeds the append-only hash set; empty is valid |
| `WRAPPER_HASH` | no | Single-hash shorthand for `WRAPPER_HASHES`. Ignored when `WRAPPER_HASHES` is set; a zero hash means "none" |
| `PREDECESSOR` | no | License contract whose holders may migrate onto this one via `claimFromPredecessor`. Frozen at deploy; `0x0` (default) accepts no migrations |
| `SUPPLY_CAP` | no | Max mintable tokens; `0` = uncapped (default). Immutable once deployed |
| `COOLDOWN_BLOCKS` | no | Blocks between activations per token (default `1800` ≈ 1 hr on Base; floor `15` ≈ 30 s is enforced on-chain) |
| `OWNER` | no | Contract owner address; defaults to broadcaster |
| `PERIOD` | subscription only | Subscription length in seconds |

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

1. Deploy the successor with `PREDECESSOR=<OLD_CONTRACT>` (immutable - a contract deployed without it accepts no claims).
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

Nobody else can do step 3 - not the old contract's owner, not the new one's. The old token is not burned or moved (there is no way to do either); the holder ends up with both, and the old contract keeps validating its tokens forever. Subscriptions carry their remaining time and their snapshotted `renewPrice` across.

Because this is a snapshot-claim rather than burn-to-mint, **migration can duplicate a seat**, and that is accepted rather than fixed. The holder can claim onto v2, sell the v1 token, and both stay honored via `honorsContract`, so the number of concurrently honored seats is not bounded by either contract's `supplyCap`. Burn-to-mint would bound it, but only by making the predecessor expose a burn - the revocation surface that must not exist - so the no-revocation guarantee takes priority and nothing in the contracts bounds, tracks, or invalidates the duplicate. Size a successor's `SUPPLY_CAP` with that in mind, or deploy v2 with no `PREDECESSOR` at all (a paid major version), which accepts no claims.

The wrapper's trust rule - "contract X, or X's successor holding a token claimed from X" - is one call:

```bash
cast call <NEW_CONTRACT> "honorsContract(address,uint256)(bool)" <OLD_CONTRACT> <NEW_TOKEN_ID> --rpc-url $RPC
```

## Auditing the invariants before buying

An agent can verify the ownership guarantees against the deployed bytecode rather than trusting the source. `test/Rub3Invariants.t.sol` runs exactly this audit; the full property-by-property breakdown, including which properties are convention rather than bytecode, is in [../architecture.md](../architecture.md#ownership-invariants-all-license-contracts).

The check: fetch the runtime code and confirm the revocation selectors are absent.

```bash
CODE=$(cast code <CONTRACT_ADDRESS> --rpc-url $RPC)
for SIG in "burn(uint256)" "adminTransfer(address,address,uint256)" \
           "forceTransfer(address,address,uint256)" "seize(uint256)" \
           "pause()" "setPaused(bool)" "revoke(uint256)" "invalidate(uint256)" \
           "setExpiresAt(uint256,uint256)" "setRenewPrice(uint256,uint256)" \
           "upgradeTo(address)" "upgradeToAndCall(address,bytes)" "initialize()" \
           "setWrapperHash(bytes32)" "removeWrapperHash(bytes32)" \
           "unrevokeWrapperHash(bytes32)" \
           "forceMigrate(uint256,address)" "setPredecessor(address)"; do
  SEL=$(cast sig "$SIG" | sed 's/^0x//')
  case "$CODE" in *"$SEL"*) echo "PRESENT: $SIG";; esac
done
```

Silence means exactly one thing: none of those 18 known revocation selectors appears in the deployed runtime bytecode. It is not proof that no revocation surface exists. The list is a blacklist of names, and a modified copy of these templates can expose the same power under a name nobody guessed - `seizeToken(uint256)`, say - and pass this scan in silence. Full assurance needs a name-independent check: compare the deployed runtime bytecode against the canonical template built from this repo at the same deploy configuration. That comparison is not set up yet.

Sanity-check the method itself against a selector that *is* there - `cast sig "activate(uint256)"` should be found.

## Planned contract evolution

The contracts above are the current, working set - including the §2.4 ownership invariants (append-only hash set, successor pattern, per-token renewal snapshot), which have landed. The agent-first plan (see [../implementation.md](../implementation.md)) adds the following - all as **new deploys**, never in-place upgrades:

- **`Rub3Factory`** (§2.3) — canonical deployment path; stamps an immutable 2–3% protocol fee split into `purchase()`/`renew()`; registry and marketplace list factory deploys only
- **USDC purchases** (§2.2) — `purchaseWithAuthorization` via EIP-3009 `transferWithAuthorization`; gasless for the buyer
- **`contentURI`** (§3.1) — content-addressed binary location on-chain, making the contract a complete distribution record
- **Concurrent seats** (§3.4) — `maxConcurrentSessions[tokenId] = K` generalizing `activeSessionId` for agent fleets
- **`Rub3Metered`** (§4.1) — per-launch / per-session micropayment billing
- **`Rub3Registry`** (§3.2) — discovery and verification, never validity; entries double as ERC-8004-style agent cards

Invariants for every license contract, present and future: no burn, no admin transfer, no pause on validation reads, no proxies. Evolution changes what is offered going forward, never what was granted.
