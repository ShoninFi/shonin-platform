# Verification helpers

How to run each check on [Verify it yourself](../verify.md). Everything here reads public state: a chain RPC, the venue's own records, and the content-addressed store, which is keyed by the hashes the chain holds.

The examples use `cast` from [Foundry](https://getfoundry.sh). Any client that can call a contract works the same way.

## The checks at a glance

| The claim | How to check it | What you need |
| --- | --- | --- |
| **The vault's keys belong to attested machines running the committed agent** | Read each key's blessing on the attestation registry: its image digest, the bundle hash it may run, the vault it may act for, and when it expires | A chain RPC |
| **The agent's bytecode came from its published source** | Rebuild the source with the published builder image and compare the bundle hash | The source and the builder image |
| **The image running today is one the allowlist permits** | Check the image digest against the registry's allowlist, and the timelock on the proposal that added it | A chain RPC and the image's source |
| **The returns are real** | Read the vault's share price over time. The contract computes it from on-chain state | A chain RPC |
| **These are all the vault's trades** | Read the vault's accounts on each venue. Every order, fill and transfer is a public, timestamped record | Chain RPCs and venue APIs |

## Keys and images

```bash
# what a key may do: the image that made it, the bundle it may run,
# the one vault it may act for, and when its blessing expires
cast call $REGISTRY "keyInfo(address)" $KEY --rpc-url $RPC

# the bundle a vault's agent version must be
cast call $STRATEGIES "bundleHashOf(bytes32,uint32)(bytes32)" $STRATEGY_ID $VERSION --rpc-url $RPC

# an image on the allowlist, and when it became usable
cast call $REGISTRY "imageOf(bytes32)" $IMAGE_DIGEST --rpc-url $RPC
```

A key checks out when its `vault` is this vault, its `strategyHash` equals the bundle hash above, its image is allowlisted (at the vault's creation or since), and its blessing has not expired. The venue's own records should show only those keys approved for the vault's accounts, and the vault's trades should come from them.

## Reading the vault panel

Every vault page carries a panel like this one, and every line of it is on-chain:

```text
Which code is trading this vault?                     Vault 0x7a3…
Audit status        vault contracts: … · executor image: … · zk guest: …

Agent               funding-basis · v3
  bundleHash        0x51af…        source: published ✓ rebuilds to 0x51af…
  built by          builder 0xae77…  from source 0x44d0…   immutable for this vault's life
Executor image      0x3b9e…        allowlisted 2026-09-28 ✓ (this vault created 2026-09-20)
Primary executor    0xE7…  blessed until 2026-10-05 14:00 UTC   proof: tx 0x9c1…
Venue approvals     agent 0xE7… (trade-only)   venue-enforced
Value now           1,204,331 USDC · computed by the vault contract, not reported
```

To check it: read the blessing transactions it links; confirm each key's record names *this* vault and *this* bundle; confirm the image digest is allowlisted, at the vault's creation or since; and confirm the venue's records show only those keys approved.

## Rebuilding a bundle

The builder image and the recipe are published. Take the source archive from the store, run the same image against it, and compare the bundle hash with what the registry holds:

```bash
# the source the registry says this bundle came from
curl -s https://api.shonin.fi/api/store/<sourceHash>/source.tar -o source.tar

# rebuild with the published builder image, then compare
#   bundleHash on the registry  ==  sha256 of what you just built
```

The recipe pins the toolchain, the SDK the build service carries, and the dependency graph the Author locked.

A rebuild that matches is the strongest form of this check, and it needs the recipe to be **hermetic** (a pinned image, fixed paths, a fixed timestamp, no network), so that an outsider's rebuild differs from ours only if the source does. Where a rebuild cannot be made, the attested signature is what the mapping from source to bundle rests on.

## Returns

```bash
# value per share, as the vault's contract computes it; read it at any block
cast call $VAULT "sharePrice()(uint256)" --rpc-url $RPC --block $BLOCK
```

A return is the change in share price between two blocks. Read it at the same blocks as any number on the site to check that number.

## Leaving without us

These are the calls a Depositor makes to leave with no service of ours involved. They hold for a vault whose exit class is **direct**, which its page shows.

```bash
# withdraw: paid now from idle balance, or a claim is booked
cast send $VAULT "redeem(uint256,address)" $SHARES $YOU --rpc-url $RPC

# settle a booked claim: anyone may send this, including you
cast send $VAULT "settle(uint256)" $CLAIM_ID --rpc-url $RPC

# read a claim's state: pending, waiting, or paid
cast call $VAULT "claimAt(uint256)" $CLAIM_ID --rpc-url $RPC
```

If a claim goes unserved for the freeze period, anyone may call `freeze()`: no new deposits, and only unwinding left.

A vault whose exit class is **served** trades on at least one venue the contract cannot unwind by itself, so a claim there needs an operator to close that part. Its page says how many operators can serve it.
