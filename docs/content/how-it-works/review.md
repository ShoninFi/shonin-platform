# The review

An attested model reads the source, before anything is compiled, and rates it in six areas. The plain-words version is [AI Review](../basics/ai-review.md); this page has the mechanism.

The review is advice, with one exception: code it finds **malicious** is refused and never built. Anything else goes ahead however it is rated. The review protects nobody on its own, and it never rates expected profit. An agent can be careful and lose money, and reckless and make it.

## Why it comes before the build

Compiling Rust runs the Author's own code: build scripts and procedural macros execute on the machine doing the build. Reading the source first means code written to attack the builder is refused before any of it runs.

## The six areas

The Reviewer judges what the code actually does, then checks that against what its Author says it does. Every finding belongs to exactly one area.

| Area | What it looks for |
| --- | --- |
| Security | Code written to do harm: attacking the machine that builds or runs it, reaching for keys or secrets it was not given, trading Depositors' money away on purpose, or logic that behaves differently after a date, an address or a signal. Text addressed to the Reviewer, and crates with little use elsewhere, are findings here too. A finding that the code is **malicious refuses the build** |
| Financial risk | Position size or leverage with no ceiling the code enforces, no response to a losing streak, sizing shapes that have failed before (adding to losers, martingale doubling, selling volatility with no cap on the loss), and a model's output sizing a position with no bound around it |
| Reliability | Rejected orders, venue errors and failed reads that leave the agent believing it holds what it does not, and a restart that begins with an empty cache |
| Reproducibility | A build script or procedural macro that reads the clock, the network, the environment or a random number, so that building twice could produce different code |
| Outside influence | Behaviour that hangs on something outside the bundle: hosts, feeds and signals the agent follows, especially any its Author controls |
| Accurate description | The Author's summary and the limits their manifest claims, compared with what the code does. A limit that is claimed and never enforced is a finding here |

## Ratings

Each area is rated one of four ways: **looks fine**, **some concerns**, **serious concerns**, or **couldn't tell**, for an area the source alone cannot settle, such as how a shipped model's weights will behave. There is no overall score.

A rating is not the model's opinion of a number. It is worked out from a fixed set of narrow questions about the code, each answered by the model, with the thresholds published, so a reader can see which answer moved it.

Behind the ratings are the **findings**, each with a severity (**note**, **concern** or **serious**). The report also carries a summary of the code in the Reviewer's own words, beside the Author's, and a plain list of **what the review could not tell**.

When the source is public, each finding also points to a file and a line. When it is sealed, the published report leaves out anything that would reveal the code: no file names, no line numbers and no quotes.

## What is pinned

The Reviewer runs on an attested machine, and its image pins:

- the model,
- the prompt and the output schema,
- the questions and their thresholds,
- the six areas, the four ratings and the three severities.

Changing any of them is an image change, which goes through the same public, timelocked allowlist as an executor image. The prompt, the schema and the questions are published, so an Author can run the same review themselves against any model they like before publishing. It just is not signed, and it is not binding.

## What is recorded

Each agent is reviewed **once**, before it is built, and that review stays with it for the life of its vault. A later Reviewer image does not re-review agents that are already running.

A **refusal** is recorded in the transaction that returns the deposit: the source hash, the Reviewer's image and the hash of its report, which is public. There is no bundle, because nothing was built.

A review that goes on to the build is submitted to the review registry with the Reviewer's blessed key once the bundle is registered. The registry checks that the sender is blessed as a Reviewer on an allowed image, and that the bundle and source hashes match the agent registered:

```solidity
submitReview(strategyId, Review(
    bytes32  bundleHash,       // the bundle reviewed
    bytes32  sourceHash,       // the source the reviewer read
    bytes32  reviewerImage,    // the image it ran in
    bytes32  modelIdHash,      // the pinned model
    bytes32  promptHash,       // the prompt, the schema and the questions together
    bytes32  reportHash,       // the report as published
    uint8[6] ratings,          // one per area: fine, some concerns, serious, couldn't tell
    uint16   findingCount,
    uint64   reviewedAt
))
```

A review is of one source and the one bundle built from it, so a good rating can never follow code nobody read.

## Prompt injection

An agent's comments, identifiers and strings are text the Author controls and a model reads. Three things bound that:

- **The prompt comes first and is pinned in the image**, and the Author's code follows it as clearly delimited data, named as data and given no authority over the review.
- **The output is schema-constrained.** The model can only return ratings, a summary, and findings in the fixed areas, each with a severity and, for public source, a file and a line.
- **Addressing the Reviewer is itself a finding.** Text that tries to instruct the Reviewer is reported under security. For public source the report quotes it where it appears.

## Sealed source

Sealed source is reviewed exactly like published source. The Reviewer reads it on its attested machine; the store refuses to serve it to anyone else, and the published report gives the ratings, a summary and findings by area, but never points into the code. What a Depositor relies on is the attested build plus the attested review. Published source is extra, and it is what makes an independent rebuild possible.
