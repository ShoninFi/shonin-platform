# Fees

A Depositor pays four fees, and all four are fixed when a vault is created. They never change under it, and each vault's page shows its own.

| Fee | How much | Who gets it |
| --- | --- | --- |
| Going in (transaction fee) | 0.1% | Nobody: it stays in the vault |
| Going out (transaction fee) | 0.1% | Nobody: it stays in the vault |
| While you hold (streaming fee) | 0.5% a year | Shonin |
| On new profit (profit fee) | 15% | 10% to the Author, 5% to Shonin |

## The fee nobody gets

The fees going in and out stay in the vault, which nudges up the value of everyone else's shares. So nobody can make money off the people who stayed by jumping in and out at the right moment.

## How fees are paid

Fees are worked out every day and paid once a week, as **new shares** rather than by selling anything, so paying them never forces the agent out of a position at a bad moment. Your number of shares doesn't change; the value behind each one does.

The value you see is always after fees. What's owed so far is already taken out, so nothing drops on the day fees are paid, and you can withdraw at that value any time. If the agent gives back a gain before the week is out, the fee on it goes away too.

## New profit, and the high-water mark

The profit fee is only charged on **new** profit: value per share above the best the vault has reached at a weekly payment. That level is its **high-water mark**, and it means no gain is ever charged for twice. If the vault drops and then climbs back, the climb back is free.

The mark belongs to the vault, not to you, and it doesn't reset when you arrive. Join a vault that's 10% below its mark, and the recovery up to it costs you no profit fee at all. Join one that has just hit a new high, and you pay from the first dollar. Each vault's page shows its mark beside its current value, so you can see where you'd be starting.

## Two worked years

Fees are easier to judge with real numbers, so here's one year up and one year down.

### A good year

Say a vault is at $1.00 a share, exactly at its high-water mark, and the agent makes 20% over the following year before fees.

You put in **$10,000**:

| | Per share | Your 9,990 shares |
| --- | ---: | ---: |
| You buy in, after the $10 fee going in | $1.0000 | $9,990.00 |
| The agent returns 20% | $1.2000 | $11,988.00 |
| Less the streaming fee, 0.5% | $1.1940 | $11,928.06 |
| Less the profit fee, 15% of $0.194 | $1.1649 | $11,637.35 |
| Less the fee going out | | $11,625.71 |

**You put in $10,000 and take out $11,626**, a return of 16.3% on an agent that made 20%. Of the difference, $194 went to the Author, $157 to Shonin, and $22 stayed in the vault as the fees going in and out.

!!! note "Where this simplifies"
    Fees are paid weekly, not once a year. For a year that only goes up, compressing it into one payment changes the streaming fee by a few pence and the profit fee not at all. The example also leaves out the spread you pay on the way out, the small gap between buying and selling prices, which depends on the market rather than on Shonin.

### A losing year

The same $10,000, in an agent that loses 10%.

| | Per share | Your 9,990 shares |
| --- | ---: | ---: |
| You buy in, after the $10 fee going in | $1.0000 | $9,990.00 |
| The agent loses 10% | $0.9000 | $8,991.00 |
| Less the streaming fee, 0.5% | $0.8955 | $8,946.05 |
| Profit fee | - | - |
| Less the fee going out | | $8,937.10 |

You take out **$8,937**, down 10.6% on an agent that lost 10%.

There's no profit fee, because there's no new profit, so the Author earns nothing this year. The streaming fee is charged either way. The high-water mark stays at $1.0000, so anyone who holds through the recovery back up to it pays no profit fee on the way.

In a losing year, then, the Author earns nothing and Shonin takes only the streaming fee. Neither can take more than the schedule the vault was created with.

The deep-dive on [Vaults](how-it-works/vaults.md#fees) has the exact calculation.

[Next: publishing an agent →](authors.md){ .md-button .md-button--primary }
