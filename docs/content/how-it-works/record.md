# The record

A record is a vault's own on-chain history. No backtests, no paper trading, no reported numbers.

## Where a return comes from

The vault contract computes its own value from on-chain state, and **value per share** is that value divided by the shares outstanding. A return is the change in value per share between two blocks. That is the whole definition.

Nothing is posted, nothing is attested, nothing is reported. There is no epoch to close and no report to trust, so there is nothing to backfill and nothing to restate. If you disagree with a number on the site, you can read the same function at the same block and settle it.

Every trade behind that number is a public event on the venue: every order, every fill, every transfer, timestamped, against accounts the vault owns.

## What the leaderboard shows

**Live records only.** A vault that never ran has nothing to show.

**Normalised for risk.** Returns are rescaled to a common 15% annualised volatility, so a list of agents is not simply a list of who used the most leverage. The volatility used for the rescaling is an exponentially weighted moving average of daily returns with a sixty-day half-life, estimated only from the days *before* each one, never from the whole series, so no part of a record is normalised by information from its own future. A very quiet record is scaled up by at most three times, because multiplying a tiny sample by a large number invents confidence that is not there.

**Ninety days before ranking.** An agent is open for deposits from its first day, labelled **new**, and is not ranked until it has ninety live days. A short run of good days is noise, and ranking it would be selling noise.

**The size it was earned at.** Every record shows the capital behind it, now, on average and at most. Nothing is extrapolated to a size the agent never traded. An edge that worked on a small vault very often does not survive a large one, and the record should let you see that rather than hide it.

**Raw beside normalised.** The raw return is what the vault actually did. Both are shown, always.

## What a record does not tell you

It is one vault's history at the size it traded, in the conditions it met. It is not a prediction, it is not risk-adjusted advice, and a longer record is not a promise.

The platform's job is to make the history impossible to fake. Reading it is still yours.

## Author history

An Author's page shows **everything they have published**: live, new, superseded, retired, the builds that failed, and the projects the review refused. Nothing there can be hidden by its Author.

An Author can publish under a fresh identity, of course. What they cannot do is carry a record with them: every agent needs its own ninety live days before it is ranked, whoever wrote it.

## Run your own indexer

The service that reads the chain into a record, computes the maths and serves the public API is one program, and the maths (normalisation, drawdown, volatility, correlation) is a library with its own tests rather than something buried in a server.

The point of that is for you to run your own against the same chain and compare. If ours and yours disagree, one of them has a bug, and the chain says which.

## Benchmarks and correlation

A portfolio page shows how your positions have moved with each other and with a few benchmarks, because two things that move together are more of one risk, not less. Benchmarks are named for what they are: a venue's own series, not an invented index.
