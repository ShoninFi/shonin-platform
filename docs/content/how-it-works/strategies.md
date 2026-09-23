# Strategies

A strategy is an ordinary Rust project that compiles to one **WebAssembly component**. It runs as a single long-lived program, sources its own data, and hands the host its decisions. It never holds a key, never signs anything, and can only ever trade its vault's own accounts.

## One long-lived program

There is no callback, no tick handler and no scheduler. The host starts `run` once and never schedules it again: everything the strategy does, it does inside that call.

```rust
impl Strategy for FundingBasis {
    async fn run(config: Vec<u8>) -> Result<(), String> {
        let config = Config::from_bytes(&config)?;
        let mut mind = Mind::new(&config);

        // Whatever a previous instance left behind is still on the venue,
        // so look before doing anything.
        step(&mut mind).await?;

        let account = venues::account_events(VENUE).into_stream().map(|_| Woke::Venue);
        let vault_events = vault::events().into_stream().map(Woke::Vault);
        let mut waking = futures::stream::select(account, vault_events);

        while let Some(woke) = waking.next().await {
            match woke {
                // A fill, a funding payment, a price: look again.
                Woke::Venue => step(&mut mind).await?,
                Woke::Vault(VaultEvent::ClaimPending(_)) => step(&mut mind).await?,
                // Unwind rather than be unwound.
                Woke::Vault(VaultEvent::Halting(_)) => {
                    flatten(&mind).await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
```

If it faults, the host tears it down and starts it again. A strategy is written to rebuild its picture from venue state, because that is where the truth is.

## What it may reach

The interface is frozen as `shonin:strategy@0.1.0`. A strategy sees these and nothing else — no filesystem, no environment, no sockets of its own.

| Interface | What it gives | What it does not |
| --- | --- | --- |
| `venues` | `submit` actions and be told what happened to each, a stream of the vault's own account events, and a read of the vault's account state | Reach any account but the vault's |
| `vault` | A stream of two events: `claim-pending` when a depositor is leaving, and `halting` when the vault is stopping | Move capital anywhere outside the vault |
| `data` | Cached, shared reads the executor fetches once and hands to every strategy that wants them | Be a private feed |
| `websocket` | A socket the executor terminates, so the strategy never holds a certificate or a key | Hold credentials |
| `state` | A per-strategy cache that may survive a restart | Be relied on; it is promised to nobody |
| `secrets` | Values the author sealed to the bundle, opened only inside the measured image | Be read by us, or by another bundle |
| `inference` | Models the author shipped in the bundle, run on the host under a time budget | Reach a model the bundle does not contain |

A **submission** is opaque bytes that the venue's own adapter understands, decoded inside the measured image. A strategy builds them with a typed crate, so the decision is the strategy's and the signing is the host's.

## Limits are code

**There is no platform risk policy.** We do not impose a leverage cap, a drawdown stop or a position limit, because a limit we imposed would be a promise about someone else's code that we could not keep.

A limit is something the author writes:

```toml
[config]
max_gross_leverage = "2"
max_order_fraction = "0.10"
idle_buffer        = "0.05"
drawdown_stop      = "0.10"
minimum_notional   = "100"
```

...and something the author's code enforces, and something the reviewer reads and the depositor can read too. A strategy that says it caps its leverage and does not is a finding, and the score says so.

## The manifest

Every project carries a `shonin.toml`: what the strategy is, what it will not do, the venues and accounts it trades, the config it takes, and the compute it asks for.

```toml
[strategy]
id = "funding-basis"
version = 1
name = "Funding basis"
summary = "Holds the spot asset, shorts the perpetual against it, and collects the funding between them."
limits = ["Gross leverage never exceeds 2×", "..."]

[venues.local]
accounts = ["venue"]

[runtime.allowances]
fuel_per_sec = 2_000_000
memory_mib = 256
requests_per_min = 120
bytes_per_day = 50_000_000
streams = 4
inference_ms_per_min = 0
```

The manifest is hashed with the bundle, so what it declared is fixed for the vault's life and shown on the vault's page. The **allowances** are what the host holds the strategy to and what the vault's tank is charged for, at the platform's published prices.

## Versions

A strategy version never changes. A new version is a new bundle, a new vault and a declared lineage:

- Depositors in the old vault are not moved, not asked and not affected. It runs exactly as it always has.
- The old vault's page shows that a newer version exists, and links to it.
- Moving is a withdrawal there and a deposit here, which you do when you want to.
- The new vault's high-water mark starts fresh.

## The SDK

Authors depend on `shonin-strategy-sdk`, which carries the generated bindings, typed helpers for each interface, decimal arithmetic that two machines cannot round differently, and the payload types each venue's adapter understands. Helper code that a strategy should not have to rewrite — hedging two legs, working an order over time, quoting from fair value — ships as ordinary crates, pinned by `Cargo.lock` like any dependency, with no privileges of their own.

The SDK is Apache-2.0.
