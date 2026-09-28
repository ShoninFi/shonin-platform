# Glossary

One word for one thing, throughout the platform and these docs.

**Agent**
: The program an Author publishes: a long-lived Rust program, compiled to a WebAssembly component, that watches the market, computes, and decides. It reaches the world through seven interfaces and nothing else. The contracts call it a *strategy*.

**Allowlist key**
: One of Shonin's two keys. It proposes machine images for the allowlist, which become usable after a public timelock, and revokes them instantly. It cannot move funds or stop a vault.

**Allowances**
: The compute an agent declares in its manifest: fuel a second, memory, requests, bandwidth, open streams and inference. The host holds it to them, and its vault's tank is charged for them at the published prices.

**Attested machine**
: A secure enclave running one of the platform's allowlisted images: it reviews, builds, or runs an agent and holds its keys. It signs an attestation of the exact code it runs, and our contracts accept a key only with one.

**Attestation**
: A proof, signed by the hardware, of which code an attested machine is running. Our contracts check it before they accept a key. It says which code holds a key; it is not what stops the key withdrawing.

**Author**
: Whoever publishes an agent. They fund its tank, earn the Author fee, and cannot touch a vault's capital.

**Blessing**
: The registry's record that one key, made in one image, may act for one vault running one bundle, until it expires. Renewed by attesting again.

**Bundle**
: What a build produces: one WebAssembly component plus any models the project ships. Identified by its `bundleHash`, which is what a vault's keys are blessed for.

**Claim**
: A withdrawal the idle balance could not pay. The shares go pending, the contract closes the leaver's fraction of every position inside the exit band, and once the cash is home they are burned at the marks then and paid. Three states: pending, waiting, paid.

**Crystallise**
: Paying the fees a vault owes as new shares. Weekly, as part of the vault's daily `keep` call, which anyone may send. Between payments, what is owed so far is already taken out of value per share.

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

**Guardian key**
: One of Shonin's two keys. It kills a vault, and publishes its reason. It cannot move funds anywhere but back to Depositors, or touch the allowlist.

**High-water mark**
: The highest value per share a vault has reached at a weekly fee payment. Profit fees are only ever charged on gains above it, so no gain is charged for twice.

**Idle balance**
: The part of a vault's value sitting in the contract rather than at a venue. Withdrawals up to it are paid in one transaction.

**Image**
: A measured build of one of the platform's services (an executor, an unwinder, the build service or the Reviewer), which runs on an attested machine. Every image that may hold a key is on a public, timelocked allowlist.

**Kill**
: The guardian key stopping a vault: every position closed, all cash home, every Depositor paid from idle balance. A halt, never a theft.

**Manifest**
: The `shonin.toml` in a project. What the agent is, what it will not do, the venues it trades, the config it takes and the allowances it starts with. Hashed with the bundle; only the allowances can change afterwards.

**Mark**
: The venue's own reference price, used to value a deposit. A withdrawal uses **exit-side marks** instead: a long at the bid, a short at the ask.

**New**
: An agent open for deposits but not yet ranked, because its record is still too short to mean much.

**Normalised return**
: A record rescaled to a common 15% annualised volatility, so agents can be compared at one level of risk. Shown beside the raw return, never instead of it.

**Record**
: A vault's own on-chain history: value per share over time, read from the contract that computes it.

**Retire**
: An Author ending their agent. The vault stops taking deposits for a seven-day wind-down while Depositors leave as usual; then the agent is stopped for good, and its positions are closed and the cash brought home.

**Review**
: An attested model's reading of the source before it is built: a rating in each of six areas, the findings behind them, and a summary in its own words. Only for public source do findings point to a file and a line. Advice, not a gate, except that code found malicious is refused and never built.

**Sealed**
: Source, or a secret, encrypted so that only the attested services can read it. Sealing changes who can read something, never whether it was compiled or reviewed.

**Settle**
: The public function that closes a claim's positions, brings the cash home and pays the leaver. Anyone can send it.

**Shares**
: ERC-4626 shares in a vault. A pro-rata claim on the pool, minted at value per share on deposit and burned on withdrawal.

**Strategy**
: The contracts' word for an agent: `strategyId`, `StrategyRegistry`, the `shonin:strategy` interface. These docs use it only where they name something in the contracts.

**Tank**
: The Author's balance for a vault's running costs, funded by them and refilled from their own fees. Excluded from what shares are worth.

**Transaction fee**
: 0.1% on every deposit and withdrawal, which stays in the pool. Nobody receives it.

**Usage total**
: A running total of what the agent has used, kept by the executor and signed by the vault's blessed key. It stops the agent when the unpaid part reaches the tank, and Shonin collects from it, capped by the allowances the Author declared.

**Value per share**
: A vault's value divided by its shares outstanding. The whole of the record is the change in this number over time.

**Vault**
: One contract holding one pool of capital behind one agent. It computes its own value, mints its own shares, and pays its own withdrawals.

**Venue**
: Where an agent actually trades. Each one enforces, in its own way, that a vault's key can trade but cannot withdraw.
