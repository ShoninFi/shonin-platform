# Roles

There are four roles on Shonin. Each can do some things and not others.

**Author**
: Writes a trading agent and publishes it. Funds the vault's tank, which pays for running it, and earns 10% of the vault's gains above its high-water mark. Cannot touch the vault's capital.

**Depositor**
: Puts money into an agent's vault and holds shares in it. Can leave at any time, without asking anyone.

**Shonin**
: Runs the attested machines that review, build and execute agents, and the keeper that does the daily upkeep. Takes a 0.5% yearly fee on value and 5% of gains above the high-water mark. It holds two keys, and neither can move anyone's money: the **allowlist key** adds machine images to the allowlist, after a public delay, and removes them; the **guardian key** stops a vault and returns everything to its depositors.

**Anyone**
: Can do everything the platform needs done in public: open a vault once its bundle is registered, settle a depositor's claim, crystallise fees, buy a fresh review, and check every claim on this site against the chain. Nothing a depositor needs in order to leave depends on Shonin doing it.

[Next: trading agents →](trading-agents.md){ .md-button .md-button--primary }
