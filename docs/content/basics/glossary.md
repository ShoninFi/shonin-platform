# Glossary

The words you'll meet throughout these docs: first who does what, then the handful of terms everything else is built from. The [technical glossary](../reference/glossary.md) has the rest.

## Roles

**[Author](../authors.md)**
: Writes a trading agent and publishes it. Funds the vault's tank, which pays for running it, and earns 10% of the vault's new profit, paid weekly. New profit means gains above the highest price its shares have reached, so no gain is charged for twice. Cannot touch the vault's capital.

**[Depositor](../depositors.md)**
: Puts money into an agent's vault and earns what the agent makes with it: the vault's profit, after fees, in proportion to their shares. Losses are shared the same way. Can leave at any time, without asking anyone.

**[Reviewer](ai-review.md)**
: An AI model, running on an attested machine, that reads an agent's source before it is built and writes a report on it. Code it finds malicious is never built. The report is there to help you choose an agent, not to keep your money safe: that is the job of the attested machine and the trade-only keys.

**Shonin**
: Runs the platform and the attested machines, and takes a small share of fees. Can stop a vault, which returns everything to its Depositors, but cannot move anyone's money anywhere else.

## Key terms

**[Agent](trading-agents.md)**
: A program an Author writes to trade the money in its vault. It runs on an attested machine, inside a sandbox.

**[AI Review](ai-review.md)**
: The report card the Reviewer writes on an agent's code before it's built, rating it in six areas.

**[Attested machine](attested-machines.md)**
: A computer that can prove exactly what code it's running, so nobody can quietly change it or read what's inside.

**Booked withdrawal**
: A withdrawal bigger than the cash the vault has at home. The contract closes your share of its positions and pays you once that cash is back.

**High-water mark**
: The highest a vault's share price has been on a payday. The profit fee is only charged on gains above it.

**Share**
: Your slice of a vault. You get shares when you put money in, and their price rises and falls with the vault.

**[Tank](../tank.md)**
: The Author's account for running their agent. It pays for computing and gas, never from Depositors' money.

**[Trade-only key](trade-only-keys.md)**
: The key that trades for a vault. A venue lets it place and cancel orders, and nothing else.

**[Vault](../under-the-hood/vaults.md)**
: The smart contract holding the money behind one agent. It keeps its own accounts, pays its own withdrawals, and works out its own share price.

**Venue**
: Where an agent actually trades, like an exchange.

[Next: trading agents →](trading-agents.md){ .md-button .md-button--primary }
