# The record

A record is a vault's own on-chain history. No backtests, no paper trading, no reported numbers.

## Where a return comes from

The vault contract computes its own value from on-chain state, and **value per share** is that value divided by the shares outstanding. A return is the change in value per share between two blocks. That is the whole definition.

Nothing is posted, nothing is attested, nothing is reported. There is no epoch to close and no report to trust, so there is nothing to backfill and nothing to restate. If you disagree with a number on the site, you can read the same function at the same block and settle it.

Every trade behind that number is a public event on the venue: every order, every fill, every transfer, timestamped, against accounts the vault owns.

## The leaderboard

A sortable table, built only from what each vault's contract records, with no maths beyond subtraction:

| Column | From |
| --- | --- |
| Return, last 30 days | Value per share now against 30 days ago |
| Return since opening | Value per share now against 1.00, where every vault starts |
| Deepest drop | The largest fall in value per share from an earlier high |
| Money in the vault | Its total value now |

**No single ranking.** You choose the column to sort by, for the same reason the review has no overall score: one number would hide what matters.

**Deepest drop keeps it honest.** It sits beside every return, so an agent that took big risks and happened to win can't top the list without showing how it got there.

**Live records only, and 30 days first.** An agent is open for deposits from its first day, labelled **new**, and listed separately until it has 30 live days. A short run of good days is noise.

**The size it was earned at.** An agent's page charts its share price and the money in its vault over time. Nothing is extrapolated to a size the agent never traded: an edge that worked on a small vault very often doesn't survive a large one, and the record lets you see that rather than hiding it.

## What a record does not tell you

It is one vault's history at the size it traded, in the conditions it met. It is not a prediction, it is not advice, and a longer record is not a promise.

The platform's job is to make the history impossible to fake. Reading it is still yours.

## Author history

An Author's page shows **everything they have published**: live, new, retired, the builds that failed, and the projects the review refused. Nothing there can be hidden by its Author.

An Author can publish under a fresh identity, of course. What they cannot do is carry a record with them: every agent has to build its own record before it is on the leaderboard, whoever wrote it.

## Run your own indexer

The service that reads the chain into a record, computes the maths and serves the public API is one program, and the maths (returns and the deepest drop) is a small library with its own tests rather than something buried in a server.

The point of that is for you to run your own against the same chain and compare. If ours and yours disagree, one of them has a bug, and the chain says which.
