# Registries

The registries are the contracts every vault shares. They record which machine images are approved, which keys are blessed, which agents exist, how each was built and what its review said. They're deployed on every chain we support, so a vault or station can check them on its own chain.

## The attestation registry

The attestation registry decides which machines may hold a key at all. It records:

- **Approved images,** each as one kind: Reviewer, builder, executor or unwinder. A new image only counts after a public wait, and a revoked one stops at once ([the allowlist](attestation.md#the-allowlist)).
- **Approved operators,** whose accounts may run those images, added the same slow, public way.
- **Approved drivers,** the on-chain half of each, added the same slow, public way and revoked at once ([drivers](../writing/drivers.md#adding-a-driver)).
- **Blessed keys:** for each, the image it came from, its kind, and for a vault's key the one vault and exact build it may act for, and when its blessing expires ([blessing a key](attestation.md#blessing-a-key)).
- **The attestation checker:** the fingerprint of the one proof program it accepts, and the Google root certificate that program trusts ([proving an attestation](attestation.md#proving-an-attestation)). These change only through the same public wait as an image.

## The strategy registry

Every agent is recorded here, under the id it was given when its Author paid the $1:

- its Author;
- the fingerprints of its source, its build and its `shonin.toml`;
- the chains it needs, which its vault and stations are created from;
- whether it's live, winding down or closed.

A build is only recorded with a signature from a blessed builder, over exactly these details, so it can't be recorded for any other agent, Author or chain.

## The review registry

Each agent's one review is recorded here: the source it read, the build made from it, the Reviewer's image and model, the fingerprint of the published report, and its six ratings. It's only accepted from a blessed Reviewer, and only if its source and build match the agent's in the strategy registry. A refusal is recorded here too, against the source it refused.

## Publishing and the factories

- **The publishing contract,** on every chain a vault can live on, takes the $1 to publish and gives each agent its id. The job only starts on a payment from the wallet that uploaded the code.
- **The vault factory** creates a vault, and its station on the same chain, when the Author activates, from the agent's record in the strategy registry. Nobody chooses where a vault or its stations live except that record.
- **The station factories,** one on every chain, create a vault's stations on other chains, only for a key blessed as that vault's executor.

## Governance

One published **admin key** governs the registries at launch, to be replaced by a multisig once there are people to hold one.

| It can | It can't |
| --- | --- |
| Propose a new image, operator, driver or attestation checker, which only counts after a public two-day wait | Bless a key, or skip the wait |
| Revoke an image, operator, driver or checker, at once | Move money anywhere but back to Depositors |
| [Kill a vault](vaults.md#a-kill) | Change a vault's agent, or anything a vault has recorded |

The wait itself can be raised at once, but lowering it takes as long as the current wait, so it can never be cut quietly. Every vault's page shows it.

**Every chain's registry is kept in step, with Solana's as the source of truth.** Every change goes to Solana first and to the other chains after, so no chain is ever ahead of it, and a wait counts from when the change reached Solana, so copying it elsewhere can never shorten it. A revocation is sent to every chain straight away. Anyone can run a published checker that compares every chain's list and flags any difference.

## What the chain verifies

| Fact | Checked by the chain | Rests on |
| --- | --- | --- |
| Which images, operators and checker are approved, and since when | ✓ | |
| Which keys are blessed, for what, and until when | ✓ | Google's attestation, checked by the proof program |
| Every agent's source, build and chains | ✓ | The builder's attested compile |
| Every review and its ratings | ✓ | The Reviewer's attested reading |
| Every change the admin key made, and when | ✓ | |

[Next: glossary →](../reference/glossary.md){ .md-button .md-button--primary }
