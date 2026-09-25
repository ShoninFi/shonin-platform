# Roles

Here's who does what on Shonin. You'll meet this cast of characters throughout the docs.

**Author**
: Writes a trading agent and publishes it. Funds the vault's tank, which pays for running it, and earns 10% of the vault's new profit, paid weekly. New profit means gains above the best the vault has done, so no gain is charged for twice. Cannot touch the vault's capital.

**Reviewer**
: An AI model, running on an attested machine, that reads an agent's source before it is built and writes a report on it, with a score out of 100. It reads the source whether the Author publishes it or seals it, and its report is public either way. Code it finds malicious is never built.

**Depositor**
: Puts money into an agent's vault and earns what the agent makes with it: the vault's profit, after fees, in proportion to their shares. Losses are shared the same way. Can leave at any time, without asking anyone.

**Shonin**
: Runs the platform and the attested machines, and takes a small share of fees. Can stop a vault, which returns everything to its Depositors, but cannot move anyone's money anywhere else.

[Next: trading agents →](trading-agents.md){ .md-button .md-button--primary }
