# Fees

Two fees pay the Author and Shonin. The profit fee is a share of the profit an agent actually makes, and nothing when it makes none. The holding fee is a yearly fee on what the vault holds, for the platform every vault runs on. Running the agent itself is the Author's cost, paid from [the tank](tank.md).

| Fee | How much | Who gets it |
| --- | --- | --- |
| Profit fee | 15% of new profit | 10% to the Author, 5% to Shonin |
| Holding fee | 1% a year | Shonin |
| Deposit and withdrawal fee | 0.1% each way | The vault: it raises every share's value |

The profit and holding fees are paid every Monday, by creating new shares for the Author and Shonin rather than taking cash out of the vault.

## The profit fee

It's only charged on **new** profit. Each vault remembers the most a share has been worth on a payday, its **high-water mark**, and the fee only applies to gains above that. So if a vault dips and then recovers, nobody pays again on the way back up. And if an agent stops making money, the profit fee stops too: the Author earns nothing until it's back above its high-water mark.

Every vault's page shows its mark next to today's share value. If you join while a vault is below its mark, its recovery up to the mark is fee-free for you.

## The holding fee

A yearly fee on what the vault holds, whether it's up or down. It pays for the platform: the vault contracts, the reviews and the checks behind every vault, on every chain it trades.

## The deposit and withdrawal fee

A tenth of a percent, going in and coming out. Neither the Author nor Shonin gets it: it stays in the vault and nudges up the value of every share in it. That way, people coming and going cover the cost of their own moves, and everyone in the vault benefits.

## An example

Depositors put $100 into a vault whose shares start at $1.00. To keep it simple, the share prices shown are before fees.

| Payday | That week | Share price | High-water mark | Profit fee |
| --- | --- | --- | --- | --- |
| 1 | Depositors put in $100, and the agent makes 7% | $1.07 | $1.07 | $1.05 = 15% of the $7.00 profit |
| 2 | Depositors take out $90, and the agent makes 7% on the $17 left | $1.14 | $1.14 | $0.18 = 15% of the $1.19 profit |
| 3 | The agent loses 10% | $1.03 | $1.14 | nothing |
| 4 | The agent makes 10% | $1.13 | $1.14 | nothing: it's still climbing back to $1.14 |
| 5 | The agent makes 5% | $1.19 | $1.19 | $0.11 = 15% of the $0.72 profit above $1.14 |

When most of the money came out in week 2, the agent kept working and the fee kept coming, just on fewer dollars. And after the loss in week 3, there was no fee until the vault had made it all back.

[Next: money flow →](flow.md){ .md-button .md-button--primary }
