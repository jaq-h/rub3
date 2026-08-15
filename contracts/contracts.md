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

### The reproducibility contract

To arrive at the same fingerprint from a checkout of this repository at a given commit, a third party must match all of these. They are all pinned in-tree, so "match" means "do not override them".

| Input | Value | Where it is pinned |
|---|---|---|
| `solc_version` | `0.8.28` | `contracts/foundry.toml` |
| `optimizer` | `true` | `contracts/foundry.toml` |
| `optimizer_runs` | `200` | `contracts/foundry.toml` |
| `evm_version` | `cancun` | `contracts/foundry.toml` |
| `bytecode_hash` | `none` | `contracts/foundry.toml` |
| `openzeppelin-contracts` | `b8c7b9e82d2b340cf82f2913c38e3a0bac2f96ae` | `contracts/foundry.lock` |
| `forge-std` | `0844d7e1fc5e60d77b68e469bff60265f236c398` | `contracts/foundry.lock` |

Nothing else matters: not the `forge` version (it fetches the pinned `solc`), not the checkout path, not comments in the source.

Those last two are the reason `bytecode_hash = "none"` is set. With solc's default (`ipfs`) the compiler appends a CBOR metadata trailer that hashes the metadata JSON, and that JSON covers comment text and source file paths. Measured on these contracts:

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

### The expected values, and the drift gate

The current fingerprints live in [`canonical-bytecode.json`](canonical-bytecode.json), alongside the build inputs they were produced under. It is JSON because it is consumed by machines as much as by people: the CI gate diffs against it, and the wrapper will later compile the same table into the binary, so a `serde`-shaped file beats a prose table or a bare checksum list.

CI runs `scripts/canonical-bytecode-hashes.sh check` as a **blocking** job (`.github/workflows/ci.yml` -> `bytecode-fingerprints`). It rebuilds from scratch and fails if any fingerprint, or any pinned build input, differs from the manifest. When a contract change is intended, regenerate and commit the manifest in the same pull request:

```bash
scripts/canonical-bytecode-hashes.sh update
```

Splitting that into a separate commit or pull request defeats the gate, which exists so that a fingerprint can never move without a reviewer seeing it move.

New contracts under `contracts/src/` are picked up automatically; abstract bases such as `Rub3License` have no `deployedBytecode` of their own and are excluded by construction.

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
           "setPeriod(uint256)" \
           "upgradeTo(address)" "upgradeToAndCall(address,bytes)" "initialize()" \
           "setWrapperHash(bytes32)" "removeWrapperHash(bytes32)" \
           "unrevokeWrapperHash(bytes32)" \
           "forceMigrate(uint256,address)" "setPredecessor(address)"; do
  SEL=$(cast sig "$SIG" | sed 's/^0x//')
  case "$CODE" in *"$SEL"*) echo "PRESENT: $SIG";; esac
done
```

Silence means exactly one thing: none of those 25 known revocation selectors appears in the deployed runtime bytecode. It is not proof that no revocation surface exists. The list is a blacklist of names, and a modified copy of these templates can expose the same power under a name nobody guessed - `seizeToken(uint256)`, say - and pass this scan in silence. Full assurance needs a name-independent check: compare the deployed runtime bytecode against the canonical template built from this repo at the same deploy configuration. That comparison is not set up yet.

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
