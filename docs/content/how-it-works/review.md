# The review

An attested model reads the source, before anything is compiled, and answers one question: **is this code reasonable?**

It is a score, with one exception: code it finds **malicious** is refused and never built. Anything else goes ahead whatever it scores. The review protects nobody on its own, and it never rates expected profit. An agent can be careful and lose money, and reckless and make it.

## Why it comes before the build

Compiling Rust runs the author's own code: build scripts and procedural macros execute on the machine doing the build. Reading the source first means code written to attack the builder is refused before any of it runs.

## What it looks at

Whether the risks are bounded by something in the code itself. Whether the code handles the ways the world goes wrong. Whether it does what it appears to do.

Every finding falls into exactly one of eight categories. The first refuses the build; the other seven only lower the score.

| Category | What it means |
| --- | --- |
| `malicious` | Code written to do harm: to attack the machine that builds or runs it, to reach for keys or secrets it was not given, or to move depositors' money out by trading it away on purpose. **Refuses the build** |
| `unbounded-risk` | Position sizing that can grow without a ceiling, leverage the code never bounds, or nothing said about what happens after a losing streak |
| `missing-failure-handling` | Rejected orders, venue errors, a host that is down, or a restart that begins with an empty cache |
| `model-dependence` | A model's output sizing a position directly, with no bound around it |
| `external-dependence` | Behaviour that hangs on something outside the bundle |
| `known-failure-pattern` | A shape that has gone wrong before, in code that holds other people's money |
| `hidden-behaviour` | Code paths that behave very differently from the rest, such as logic that trades differently after a date or a signal |
| `unaudited-dependencies` | Crates with little use elsewhere, pulled into code that holds depositors' money |

Each finding carries a severity (**note**, **concern** or **serious**) and points at a file and a line. The report also carries a summary in the reviewer's own words, beside the author's, and a plain list of **what the review could not tell**.

## The score

An integer from 0 to 100, higher being more reasonable. Ordinary, plainly-written code with stated limits scores well, and the score comes down for what was actually found, weighted by severity. An agent with no findings at all should score in the nineties rather than at 100: no reader sees everything.

Every agent page shows the score beside what it cannot tell you, because a number on its own would be read as a promise.

## What is pinned

The reviewer runs on an attested machine, and its image pins:

- the model,
- the prompt,
- the output schema,
- the eight categories and the three severities.

Changing any of them is an image change, which goes through the same public, timelocked allowlist as an executor image. The prompt and the schema are published, so an author can run the same review themselves against any model they like before publishing. It just is not signed, and it is not binding.

## What is recorded

A **refusal** is recorded in the transaction that closes the vault: the source hash, the reviewer's image and the hash of its report, which is public. There is no bundle, because nothing was built.

A review that goes on to the build is submitted to the review registry with the reviewer's blessed key once the bundle is registered. The registry checks that the sender is blessed as a reviewer on an allowed image, and that the bundle and source hashes match the version registered:

```solidity
submitReview(strategyId, version, Review(
    bytes32 bundleHash,        // the bundle reviewed
    bytes32 sourceHash,        // the source the reviewer read
    bytes32 reviewerImage,     // the image it ran in
    bytes32 modelIdHash,       // the pinned model
    bytes32 promptHash,        // the prompt and the output schema together
    bytes32 reportHash,        // the full JSON report
    uint8   score,             // 0-100
    uint16  findingCount,
    uint64  reviewedAt,
    address boughtBy           // who paid for this one
))
```

Every review is kept, newest first. A review is of one source and the one bundle built from it: a good score can never follow code nobody scored.

## Buying another one

**Anyone can buy a fresh review** of any agent, at any time, for the same flat fee: the author, a depositor, a competitor, a sceptic. The newest is shown first and the older ones are kept, so an agent cannot quietly shop for a better score.

A fresh review that finds a live agent **malicious** cannot refuse a build that already happened. Instead it [pauses deposits](vaults.md#deposits-paused) into the vault until the guardian key either kills it or lifts the pause, with its reason published.

Nothing is re-reviewed automatically when the reviewer image changes. A review says which image produced it, and you can buy one under the current image whenever you want.

## Prompt injection

An agent's comments, identifiers and strings are text the author controls and a model reads. Three things bound that:

- **The prompt comes first and is pinned in the image**, and the author's code follows it as clearly delimited data, named as data and given no authority over the review.
- **The output is schema-constrained.** The model can only return a score and findings from the fixed categories, each with a severity, a file and a line.
- **Addressing the reviewer is itself a finding.** Text that tries to instruct the reviewer is reported as hidden behaviour, quoted where it appears, and scores the agent down.

## Sealed source

Sealed source is reviewed exactly like published source. The reviewer reads it on its attested machine; the store refuses to serve it to anyone else. What a depositor relies on is the attested build plus the attested review. Published source is extra, and it is what makes an independent rebuild possible.

## A new version

Where an author declares that a version supersedes another, the review of the new version stands on its own: a new bundle from new source gets a new reading, and the old review stays attached to the old vault, which is still running the code it was scored on.
