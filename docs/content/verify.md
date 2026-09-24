# Verify it yourself

Everything the platform claims is a mechanism, and every mechanism has a way to check it without asking us. This page is that list.

## The five checks

| The claim | How to check it | What you need |
| --- | --- | --- |
| **The vault's keys belong to attested executors running the committed agent** | Read the blessing on the attestation registry: each key's image digest, the bundle hash it may run, the vault it may act for, and when it expires | A chain RPC |
| **The agent's bytecode came from its published source** | Rebuild the source with the published builder image and compare the bundle hash | The source and the builder image |
| **These are all the vault's trades** | Read the vault's accounts on each venue. Every order, fill and transfer is already a public, timestamped record | Chain RPCs and venue APIs |
| **The returns are real** | Read value per share from the vault contract over time. It computes its own value from on-chain state | A chain RPC |
| **The image running today is one the allowlist permits** | Check the image digest against the registry's allowlist, and the timelock on the proposal that added it | A chain RPC and the executor's source |

## The panel on every vault page

Every agent's page answers the question a depositor should ask — **which code is trading my money right now?** — and everything in it is on-chain:

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

A verifier checks the blessing transactions; that each key's record names *this* vault and *this* bundle; that the image digest is allowlisted, at the vault's creation or since; that the venue's own records show only those keys approved; and that the vault's trades came from those keys.

## Rebuilding a bundle

The builder image and the recipe are published. Take the source archive from the store, run the same image against it, and compare the bundle hash with what the registry holds:

```bash
# the source the registry says this bundle came from
curl -s https://api.shonin.fi/api/store/<sourceHash>/source.tar -o source.tar

# rebuild with the published builder image, then compare
#   bundleHash on the registry  ==  sha256 of what you just built
```

The recipe is pinned and published: the toolchain, the SDK the build service carries, and the dependency graph the author locked.

A rebuild that matches is the strongest form of this check, and it needs the recipe to be **hermetic** — a pinned image, fixed paths, a fixed timestamp, no network. That is not finished yet, so a rebuild today may differ in ways that have nothing to do with the source. It is [on the checklist](checklist.md#build-and-review), and until it is done the attested signature is what the mapping from source to bundle rests on.

## What the chain cannot prove

**That nothing happened outside it.** For withdrawals it does not need to: a vault's keys can only move funds between the vault's own accounts, so no key can send value anywhere else. Trades are different. A trade at a bad price moves value to whoever took the other side, and the chain shows the trade without saying who that was or why. Today the only on-chain bound on an order's price is the exit band on a claim; a bound on every order is [on the checklist](checklist.md#guarantees-first).

**That the source is honest.** Reproducible builds prove the bytecode matches the source. Whether the code does something sensible is what the review reads, what an audit examines, and what you can read yourself.

**That a record predicts anything.** It is history, at the size it was earned, in the conditions it met.

## Exit without us

The check that matters most is the one you can run on the worst day. It holds for a vault whose exit class is **direct**, which the vault's page shows:

1. Stop trusting every service we run.
2. Call `redeem` on the vault contract directly.
3. If the idle balance covers it, you are paid in that transaction.
4. If not, a claim is booked — and `settle` is a public function that **you** can send.

No service of ours is in that path. That is the point of it.

A vault whose exit class is **served** trades on at least one venue the contract cannot unwind by itself, so a claim there needs an operator to close that part. The vault's page says how many operators can serve it.
