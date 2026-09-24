# Parameters

Every number the platform runs on, in one place. These are the values a vault is created with today; a vault's own page always shows its own, because a schedule is fixed when a vault is created and never changes under it.

Anything marked **working default** has not been measured yet and is on the [checklist](../checklist.md#numbers-to-measure).

## Fees

| What | Level | Charged on | Paid to |
| --- | --- | --- | --- |
| Streaming fee | 0.5% a year | The vault's value | The platform |
| Author fee | 10% | Gains above the high-water mark | The author |
| Platform fee | 5% | The same gains | The platform |
| Transaction fee | 0.1% | Every deposit and withdrawal | Nobody: it stays in the pool |

Fees crystallise daily, as new shares. The profit fee is only ever taken on a new high-water mark.

## Leaving

| What | Value |
| --- | --- |
| Exit band | 1% either side of the venue's oracle price |
| Deposits valued at | Mark |
| Withdrawals valued at | Exit-side marks: a long at the bid, a short at the ask |
| Freeze after an unserved claim | 7 days |
| Claim states | Pending, waiting, paid |

## The tank

| What | Value |
| --- | --- |
| Auto-fill | 100% of each author fee, until the tank reaches its target |
| Tank target | 30 days of charges at the declared allowances |
| Review fee | A flat fee per review |
| Factory minimum | The least any vault's tank can be created with, read from the factory |
| Least a tank can be funded with | The larger of the factory minimum and the review fee plus a week at the declared allowances, shown on the publish page |

## Published prices

What a day of declared allowances costs, and what the vault's contract will let a usage statement charge.

| Unit | Price | |
| --- | --- | --- |
| A full core | $0.15 an hour | working default |
| Memory | $0.01 a GiB-hour | working default |
| Egress | $0.15 a GB | working default |
| Inference | At the processor rate | working default |

The mapping from the runtime's fuel to processor time is not measured yet, so the fuel rate an author declares is what the tank is billed at and how often the agent yields, and the runtime's **epoch** is what actually bounds an agent.

## Allowances

What an agent declares, what the host holds it to, and what its tank is charged for.

| Allowance | A modest agent |
| --- | --- |
| `fuel_per_sec` | 2,000,000 |
| `memory_mib` | 256 |
| `requests_per_min` | 120 |
| `bytes_per_day` | 50,000,000 |
| `streams` | 4 |
| `inference_ms_per_min` | 0 |

A manifest may declare any allowances it likes; these are a starting point. A heavier shape costs more a day and the publish page prices each one before you choose.

## The record

| What | Value |
| --- | --- |
| Volatility every record is normalised to | 15% annualised |
| How volatility is estimated | An exponentially weighted moving average of daily returns, 60-day half-life |
| Estimated from | Only the days before each one, never the whole series |
| Most a quiet record may be scaled up | 3× |
| Live days before an agent is ranked | 90 |
| Before that | Open for deposits, labelled **new** |

## Attestation

| What | Value |
| --- | --- |
| Timelock on adding an image | 2 days |
| Removing an image | Immediate |
| Blessing lifetime | 72 hours, renewed by attesting again |
| Image kinds | Executor, unwinder, builder, reviewer |
| A vault accepts an image | Allowlisted at its creation, or added since |

## The review

| What | Value |
| --- | --- |
| Score | An integer, 0-100, higher is more reasonable |
| Finding categories | 8, fixed by the reviewer image |
| Severities | Note, concern, serious |
| Who may buy one | Anyone, at any time, for the same flat fee |
| When it runs | Before the build |
| Is it a gate? | Only for code found `malicious`, which is refused and never built |
