# Putting money in

You pick an agent, put money into its vault, and hold shares in a pool. The agent trades; your shares are worth whatever the pool is worth divided by the shares outstanding. You leave when you want to.

An agent is a real program — it watches the market, runs its own computations, and decides. [What an agent actually is](index.md#what-an-agent-actually-is) covers what it can reach, and why a program that capable still cannot take your money.

This page is what that costs and what it feels like. The mechanism underneath is in the [technical deep-dive](how-it-works/index.md).

## Choosing one

Every agent's page shows the same four things, and they are worth reading in this order.

**The record.** Value per share over time, read from the vault's own contract. Beside it, the same record rescaled to a common level of risk, because an unadjusted leaderboard mostly measures who used the most leverage.

**The size it was earned at.** An edge that worked on a small vault often does not survive a large one. The page shows the capital behind the record — now, on average, and at most — so you can see whether the number you are looking at was earned at the size you would be joining.

**The review.** A score out of 100 with findings, each pointing at a file and a line, and a plain list of what the review could not tell. It is not a gate and it never stopped a vault opening. Read the findings rather than the number.

**Which code is trading it.** The bundle hash, whether the source is published, which executor image holds the keys, and when its attestation expires. All of it on-chain.

An agent with fewer than ninety live days is labelled **new** and is not ranked. It can still take your money. That is deliberate: a record has to start somewhere, and hiding young agents would just mean nobody could ever have one.

## What a deposit costs

One fee on the way in, one on the way out, and two while you are there.

| | | |
| --- | --- | --- |
| Entering | 0.1% | Stays in the pool |
| Leaving | 0.1% | Stays in the pool |
| While you hold | 0.5% a year | To the platform |
| On profit above the vault's high-water mark | 15% | 10% to the author, 5% to the platform |

The transaction fee is the odd one. Nobody receives it. It stays in the pool, which lifts the value of every share that did not move, so somebody timing a deposit around a price update cannot take anything from the people who stayed.

Fees crystallise daily as **new shares** rather than as a sale, so paying them never forces the agent out of a position at a bad moment. Your share count does not change. The value behind each share does.

## Two worked years

The fees are easier to judge against real numbers than percentages, so here are both directions.

### A good year

Say a vault is at $1.00 a share, sitting exactly on its high-water mark, and the agent makes 20% over the following year before fees.

You put in **$10,000**.

```mermaid
flowchart LR
    IN(["$10,000"]) ==> SH["9,990 shares<br/>$1.0000"]
    SH ==> UP["after +20%<br/>$1.2000"]
    UP ==> ST["$1.1940"]
    ST ==> PF["$1.1649"]
    PF ==> OUT(["$11,626"])

    UP -.-> F1["streaming<br/>$60"]
    ST -.-> F2["profit fee<br/>$291"]
    SH -.-> F0["entering<br/>$10"]
    PF -.-> F3["leaving<br/>$12"]

    classDef ends fill:#4338ca20,stroke:#4338ca,color:inherit,font-weight:bold
    classDef share fill:#0d948818,stroke:#0d9488,color:inherit
    classDef fee fill:#f59e0b18,stroke:#f59e0b,color:inherit,stroke-dasharray:4 3
    class IN,OUT ends
    class SH,UP,ST,PF share
    class F0,F1,F2,F3 fee
```

In numbers:

| | Per share | Your 9,990 shares |
| --- | ---: | ---: |
| You buy in at | $1.0000 | $9,990.00 |
| The agent returns 20% | $1.2000 | $11,988.00 |
| Less the streaming fee, 0.5% | $1.1940 | $11,928.06 |
| Less the profit fee, 15% of $0.194 | $1.1649 | $11,637.35 |
| Less the transaction fee on the way out | | $11,625.71 |

**You put in $10,000 and take out $11,626**, a return of 16.3% on an agent that made 20%. Of the difference, $194 went to the author, $157 to the platform, and $22 stayed in the pool as the two transaction fees.

!!! note "Where this simplifies"
    Crystallisation is daily, not annual. Compressing a year into one call changes the streaming fee by a few pence. The example also assumes the exit-side marks equal the mid price, so it leaves out the spread you pay on the way out, which depends on the market rather than on the platform.

### A losing year

The same $10,000, in an agent that loses 10%.

| | Per share | Your 9,990 shares |
| --- | ---: | ---: |
| You buy in at | $1.0000 | $9,990.00 |
| The agent loses 10% | $0.9000 | $8,991.00 |
| Less the streaming fee, 0.5% | $0.8955 | $8,946.05 |
| Profit fee | — | — |
| Less the transaction fee on the way out | | $8,937.10 |

You take out **$8,937**, down 10.6% on an agent that lost 10%.

No profit fee is charged, because there is no gain above the high-water mark — the author earns nothing this year. The streaming fee is charged anyway, at 0.5% of value, because it pays for a platform that ran whether or not the agent worked. The mark stays at $1.0000, so whoever holds shares through the recovery back to it pays no profit fee on the way.

The asymmetry is deliberate and it is worth being clear about: the author has upside and no downside beyond earning nothing, and the platform takes a small fee in a bad year. What neither of them can do is take more than that.

## The high-water mark is the vault's, not yours

This one surprises people, and it can work for or against you.

The profit fee is charged on value per share above the highest the vault has ever crystallised at. That mark belongs to the vault. It does not reset when you arrive.

Deposit into a vault that is 10% below its own high-water mark, and the recovery back up to that mark costs you no profit fee at all. Deposit into one that has just made a new high, and you pay from the first dollar.

Neither is a trick. It falls out of charging fees on the pool rather than per depositor, and the vault's page shows its high-water mark beside its value per share so you can see where you are standing.

## Getting out

Withdrawing has two shapes, and which one you get depends on how much cash the agent is holding idle.

**Paid immediately.** If the vault's idle balance covers your withdrawal, your shares are burned and you are paid in the same transaction. Agents keep a working buffer precisely so that most withdrawals end here.

**Booked as a claim.** If it does not, your shares are marked pending and the contract books a claim. It closes your fraction of every position, inside a band around the venue's own oracle price, and pays you once the cash is home at the value then.

The settle function is public. Anyone can call it — you, another depositor, a passer-by, or the keeper we run. That is the part that matters: your exit does not depend on us being alive, solvent, or willing.

A claim that goes unserved for seven days lets anyone freeze the vault, which stops new deposits and leaves only unwinding.

## What you are still exposed to

Being honest about this is the point of the whole project, so here it is plainly.

**The agent can lose money.** Nothing here prevents that. A review is not a prediction and a record is not a promise. The platform makes the losses visible and honest; it does not stop them.

**The market can be bad when you leave.** A claim closes your positions at the market, inside a band. In a fast market the close may not fill, and the claim waits.

**Our code can be wrong.** The contracts and the images are ours. They are open, they will be audited, and every vault page shows the audit status of each one. Until those audits exist, that page says "unaudited", which is not the same as "fine".

**A venue can fail.** Custody is only as good as the venue enforcing it, which is why every vault shows its custody level per venue.

What you are *not* exposed to is somebody transferring your deposit somewhere else. There is no path for it.
