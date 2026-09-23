# Shonin

**Smart contracts are public and auditable. Trading bots should be too.**

Shonin is an open platform for trading strategies you can check. Authors publish a strategy as source. We compile it, a model reviews it, and attested executors run it. Depositors pool capital in a vault behind the strategies they trust, and every trade, every fee and every unit of value is on a chain where nobody can go back and change it.

Nothing here asks you to believe a screenshot.

## The one promise

**Every lever anyone has ends in a halt, never a theft.**

Anyone can switch a strategy off. Nobody — not the author, not an executor, not Shonin — can take money out of a vault. That is not a policy we have written down; it is what the contracts and the venues will let the keys do, and you can read both.

## What that rests on

| The claim | What makes it true |
| --- | --- |
| Only the committed code trades this vault | A vault's keys exist only inside attested executors, and the attestation names the exact bundle they may run |
| Money only moves between the vault's own accounts | The venue and the vault contract enforce it; there is no route out |
| You can leave whenever you want | A withdrawal is paid from idle balance at once, or booked as a claim that anyone can settle |
| The record is real | The vault contract computes its own value from on-chain state; there is nothing to backfill |
| The code was read | An attested reviewer scores the source with a pinned model and prompt, and signs the result |

Each of these is something you can check for yourself, without asking us. [Verify it yourself](verify.md) says how.

## Who you might be

<div class="grid cards" markdown>

- **A depositor.** Start with [Vaults](how-it-works/vaults.md) — what a share is worth, what it costs, and how you get out. Then [The record](how-it-works/record.md), which is what you are actually choosing between.

- **An author.** Start with [Strategies](how-it-works/strategies.md) — what you write and what it may do. Then [Publishing](how-it-works/publishing.md), which is the whole path from a Cargo project to an open vault.

- **A sceptic.** Start with [Verify it yourself](verify.md), then [Attested execution](how-it-works/execution.md). Everything the platform claims is a mechanism, and every mechanism has a way to check it.

- **A builder.** [Run it locally](run-locally.md) brings up the whole platform on your own machine in one command, and the [Build checklist](checklist.md) is what is left to do.

</div>

## Where it is today

The platform runs end to end on a local development chain: the contracts, the build service, the reviewer, an executor, the keeper, the indexer and the web app, with the site showing only what that chain produced. Nothing is deployed to a public chain yet, and the [Build checklist](checklist.md) is the honest list of what stands between here and there.

These docs describe how it works, not how we hope it will work. Where something is designed but not yet built, it says so.
