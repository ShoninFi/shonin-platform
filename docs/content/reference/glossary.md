# Glossary

The words you'll meet throughout these docs: first who does what, then the handful of terms everything else is built from, and last the terms you'll only meet once you look under the hood.

## Roles

**[Author](../authors.md)**
: Writes a trading agent and publishes it. Funds the vault's tank, which pays for running it, and earns 10% of the vault's new profit, paid weekly. New profit means gains above the highest price its shares have reached, so no gain is charged for twice. Cannot touch the vault's capital.

**[Depositor](../depositors.md)**
: Puts money into an agent's vault and earns what the agent makes with it: the vault's profit, after fees, in proportion to their shares. Losses are shared the same way. Can leave at any time, without asking anyone.

**[Reviewer](../basics/ai-review.md)**
: An AI model, running on an attested machine, that reads an agent's source before it is built and writes a report on it. Code it finds malicious is never built. The report is there to help you choose an agent, not to keep your money safe: that is the job of the attested machine and the trade-only keys.

**Shonin**
: Runs the platform and the attested machines, and takes a small share of fees. Can stop a vault, which returns everything to its Depositors, but cannot move anyone's money anywhere else.

## Key terms

**[Agent](../basics/trading-agents.md)**
: A program an Author writes to trade the money in its vault. It runs on an attested machine, inside a sandbox.

**[AI Review](../basics/ai-review.md)**
: The report card the Reviewer writes on an agent's code before it's built, rating it in six areas.

**[Allowances](../writing/configuration.md#vault-configuration)**
: How much computing and gas an agent may use, set by its Author and changeable on the live vault. They're also the most its tank can be charged.

**[Attested machine](../basics/attested-machines.md)**
: A computer that can prove exactly what code it's running, so nobody can quietly change it or read what's inside.

**Booked withdrawal**
: A withdrawal bigger than the cash the vault has at home. The contract closes your share of its positions and pays you once that cash is back.

**[High-water mark](../fees.md#the-profit-fee)**
: The highest a vault's share price has been on a payday. The profit fee is only charged on gains above it.

**[Idle floor](../writing/configuration.md#vault-configuration)**
: How much of a vault its Author keeps at home as cash, ready for withdrawals.

**New**
: An agent open for deposits, but with fewer than 30 days of history, so it isn't on the leaderboard yet.

**Record**
: A vault's history of its share price, read straight from its own contract.

**[Retire](../writing/publishing.md#retiring-it)**
: An Author ending their agent. The vault stops taking deposits and the agent keeps running for seven days; then it's stopped for good, and its positions are closed and the cash brought home to the vault, where anyone still in can withdraw it.

**[Sealed](../authors.md#keeping-your-code-private)**
: Source code, or a secret, encrypted so only the attested machines can read it. Sealing changes who can read something, never whether it was reviewed and built.

**Share**
: Your slice of a vault. You get shares when you put money in, and their price rises and falls with the vault.

**[`shonin.toml`](../writing/configuration.md#shonintoml)**
: An agent's configuration file: its description, the claims it makes about itself, the venues it trades and the bridges it uses. Fixed once published.

**[Tank](../tank.md)**
: The Author's account for running their agent. It pays for computing and gas, never from Depositors' money.

**[Trade-only key](../basics/trade-only-keys.md)**
: The key that trades for a vault. A venue lets it place and cancel orders, and nothing else.

**[Vault](../under-the-hood/vaults.md)**
: The smart contract holding the money behind one agent. It keeps its own accounts, pays its own withdrawals, and works out its own share price.

**[Venue](venues.md)**
: Where an agent actually trades, like an exchange.

## Under the hood

**[Admin key](../under-the-hood/vaults.md#a-kill)**
: Shonin's one key with special powers. It proposes machine images, which only count after a public wait, revokes them at once, and can kill a vault. It can't move money anywhere but back to Depositors.

**[Attestation](../under-the-hood/machines.md#what-attestation-proves-and-what-it-doesnt)**
: A statement, signed by the processor, of exactly which code an attested machine is running. Our contracts check one before they accept any key it made.

**[Blessing](../under-the-hood/machines.md#blessing-a-key)**
: The on-chain record that one key, made on one image, may act for one vault running one bundle, until it expires. Renewed by attesting again.

**Bundle**
: What a build produces: the agent's compiled code, plus any AI models it ships. Its hash is what a vault's keys are blessed to run.

**Claim**
: The contract's name for a booked withdrawal. It's *pending*, then *waiting* if the market was too far from its price to close, then *paid*.

**[Exit band](../under-the-hood/vaults.md#the-exit-band)**
: How far from the venue's own price a booked withdrawal may close positions, 1% by default. It limits what one person leaving can cost everyone else.

**Freeze**
: What anyone can do to a vault whose booked withdrawal has gone unpaid for seven days: no new deposits, and only winding down left.

**Idle balance**
: The part of a vault's money sitting in its contract rather than at a venue. Withdrawals up to it are paid at once.

**[Image](../under-the-hood/machines.md#the-allowlist)**
: One of the approved programs an attested machine may run: the Reviewer, the builder, the executor or the unwinder. The list is public, and a new image only counts after a public wait.

**[Kill](../under-the-hood/vaults.md#a-kill)**
: Shonin stopping a vault: every position closed, all the cash brought home, and every Depositor able to withdraw at once. A halt, never a theft.

**Mark**
: A venue's own reference price, used to value a deposit. A withdrawal is valued at **exit-side marks** instead: what a position would actually sell for.

**Settle**
: The public function that closes a booked withdrawal's positions, brings the cash home and pays the leaver. Anyone can send it.

**Strategy**
: The contracts' word for an agent, as in `strategyId` and `StrategyRegistry`. These docs only use it when naming something in the contracts.

**Usage total**
: The running total of what an agent has used, kept and signed by its attested machine. The agent stops when the unpaid part reaches the tank, and Shonin collects from it, never past the allowances.

**Value per share**
: A vault's value divided by its shares. It's the share price, and the whole of a vault's record is how it changes.
