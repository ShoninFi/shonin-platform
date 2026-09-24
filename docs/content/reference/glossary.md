# Glossary

One word for one thing, throughout the platform and these docs.

**Agent**
: The program an author publishes: a long-lived Rust program, compiled to a WebAssembly component, that watches the market, computes, and decides. It reaches the world through seven interfaces and nothing else. The contracts call it a *strategy*.

**Allowances**
: The compute an agent declares in its manifest: fuel a second, memory, requests, bandwidth, open streams and inference. The host holds it to them, and its vault's tank is charged for them at the published prices.

**Attestation**
: A proof of which code is running inside an enclave. It says which image holds a key; it is not what stops the key withdrawing.

**Author**
: Whoever publishes an agent. They fund its tank, earn the author fee, and cannot touch a vault's capital.

**Blessing**
: The registry's record that one key, made in one image, may act for one vault running one bundle, until it expires. Renewed by attesting again.

**Bundle**
: What a build produces: one WebAssembly component plus any models the project ships. Identified by its `bundleHash`, which is what a vault's keys are blessed for.

**Claim**
: A withdrawal the idle balance could not pay. The shares go pending, the contract closes the leaver's fraction of every position inside the exit band, and once the cash is home they are burned at the marks then and paid. Three states: pending, waiting, paid.

**Crystallise**
: Turning the fees a vault has accrued into new shares. Daily, and anyone may call it.

**Custody level**
: How a venue enforces that a key cannot take funds out: venue-enforced, chain-enforced, or something weaker, shown per venue on every vault page.

**Depositor**
: Whoever puts capital into a vault. They hold shares, can leave at any time, and never need permission to do either.

**Exit band**
: How far either side of the venue's oracle price a claim may close, 1% by default. It bounds what the pool pays for someone else's exit.

**Exit class**
: Whether a vault can be exited by its contract alone (**direct**) or needs an operator to unwind a venue the contract cannot reach (**served**). Shown on every vault page.

**Executor**
: The attested service that runs an agent and holds the vault's trading keys. It can only trade the vault's own accounts.

**Freeze**
: What anyone may do to a vault whose claim has gone unserved for the freeze period: no new deposits, and only unwinding remains possible.

**Fuel**
: The runtime's unit of work. An agent declares a rate, the rate is what its tank is billed at, and it decides how often the agent yields.

**High-water mark**
: The highest value per share a vault has crystallised at. Profit fees are only ever taken above it.

**Idle balance**
: The part of a vault's value sitting in the contract rather than at a venue. Withdrawals up to it are paid in one transaction.

**Image**
: A measured build of one of the platform's services — an executor, an unwinder, the build service or the reviewer. Every image that may hold a key is on a public, timelocked allowlist.

**Kill**
: The admin key stopping a vault: every position closed, all cash home, every depositor paid from idle balance. A halt, never a theft.

**Manifest**
: The `shonin.toml` in a project. What the agent is, what it will not do, the venues it trades, the config it takes and the allowances it asks for. Hashed with the bundle.

**Mark**
: The venue's own reference price, used to value a deposit. A withdrawal uses **exit-side marks** instead: a long at the bid, a short at the ask.

**New**
: An agent open for deposits but not yet ranked, because it has fewer than ninety live days.

**Normalised return**
: A record rescaled to a common 15% annualised volatility, so agents can be compared at one level of risk. Shown beside the raw return, never instead of it.

**Record**
: A vault's own on-chain history: value per share over time, read from the contract that computes it.

**Review**
: An attested model's score out of 100 with findings, each in a fixed category with a severity, a file and a line. A score, not a gate.

**Sealed**
: Source, or a secret, encrypted so that only the attested services can read it. Sealing changes who can read something, never whether it was compiled or reviewed.

**Settle**
: The public function that closes a claim's positions, brings the cash home and pays the leaver. Anyone can send it.

**Shares**
: ERC-4626 shares in a vault. A pro-rata claim on the pool, minted at value per share on deposit and burned on withdrawal.

**Strategy**
: The contracts' word for an agent: `strategyId`, `StrategyRegistry`, the `shonin:strategy` interface. These docs use it only where they name something in the contracts.

**Supersede**
: An author's declaration that a new version replaces an older one. A claim about lineage, not a mechanism: the old vault runs exactly as it always has.

**Tank**
: The author's balance for a vault's running costs, funded by them and refilled from their own fees. Excluded from what shares are worth.

**Transaction fee**
: 0.1% on every deposit and withdrawal, which stays in the pool. Nobody receives it.

**Usage statement**
: What a day of running cost, signed by the vault's blessed key and charged to its tank, capped by the allowances the author declared.

**Value per share**
: A vault's value divided by its shares outstanding. The whole of the record is the change in this number over time.

**Vault**
: One contract holding one pool of capital behind one agent version. It computes its own value, mints its own shares, and pays its own withdrawals.

**Venue**
: Where an agent actually trades. Each one enforces, in its own way, that a vault's key can trade but cannot withdraw.

**Version**
: An agent version never changes. A new version is a new bundle, a new vault and a declared lineage — never an edit of something a depositor already chose.
