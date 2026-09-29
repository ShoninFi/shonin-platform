# Example: a rebalancer

The template's agent holds a target exposure to one market, say half the vault's value long ETH, and trades back towards it when the market moves it too far away. It's deliberately small, so you can read all of it, but it's real: it keeps its limits in code, copes with rejected orders, and steps aside when Depositors leave.

## Its configuration file

```toml
[vault]
chain = "hyperliquid"

[description]
title = "Rebalancer"
text = """
Holds a target exposure to one market and trades back towards it when \
the market moves it too far away.
"""
claims = [
    "Exposure never above the vault's value",
    "No single order above 5% of the vault",
    "Trades one market, and nothing else",
]

[drivers.hyperliquid]
```

[Configuration](configuration.md) explains each part.

## Its settings

The rebalancer's settings are constants in `src/config.rs`:

```rust
/// The one market it trades.
pub const MARKET: &str = "ETH-PERP";
/// How much of the vault's value to hold there, long: 0.5 is half.
pub const TARGET_EXPOSURE: &str = "0.5";
/// The most it will ever hold, whatever the target says.
pub const MAX_EXPOSURE: &str = "1.0";
/// How far exposure may drift from the target before it trades.
pub const REBALANCE_BAND: &str = "0.05";
/// The largest single order, as a share of the vault's value.
pub const MAX_ORDER_FRACTION: &str = "0.05";
```

The numbers are written as text and read as exact decimals, so two machines can never round them differently.

## How it runs

Its one long-lived `run`, from `src/lib.rs`. It uses two of the [services](services.md): `drivers`, to read the vault's account and send orders, and `vault`, to hear when a Depositor is leaving or the vault is stopping:

```rust
impl Strategy for Rebalancer {
    async fn run() -> Result<(), String> {
        // Whatever a previous run left behind is still on the exchange,
        // so look before doing anything.
        step().await?;

        let account = drivers::account_events(DRIVER).into_stream().map(|_| Woke::Account);
        let vault_events = vault::events().into_stream().map(Woke::Vault);
        let mut waking = futures::stream::select(account, vault_events);

        while let Some(woke) = waking.next().await {
            match woke {
                // A fill or a price move: look again.
                Woke::Account => step().await?,
                // A Depositor is leaving, or cash at home is running low:
                // look again, and keep more at home.
                Woke::Vault(VaultEvent::ClaimPending(_) | VaultEvent::BufferLow(_)) => {
                    step().await?
                }
                // The vault is stopping: close out rather than be closed out.
                Woke::Vault(VaultEvent::Halting(_)) => {
                    close_out().await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
```

There's no callback, no tick handler and no scheduler. The attested machine starts `run` once, and everything the agent does, it does inside that call. If it ever crashes, it's started again, so an agent always rebuilds its picture from what the exchange says it holds, because that's where the truth is.

## Its claims, kept in code

Each of the rebalancer's three claims is a promise in `claims` and something its code makes true:

- **"Exposure never above the vault's value"**: `MAX_EXPOSURE` is `1.0`, the target is held below it whatever `TARGET_EXPOSURE` says, and a buy never goes past the target.
- **"No single order above 5% of the vault"**: every order is capped at `MAX_ORDER_FRACTION` of the vault's value, so a big gap closes over several steps.
- **"Trades one market, and nothing else"**: it only ever reads and orders `MARKET`, and does nothing if the exchange doesn't list it.

That's the whole pattern. There's no platform risk policy, so a limit only exists if your code enforces it, and the [AI Review](../basics/ai-review.md) checks each promise against the code.

[Next: how the machines work →](../under-the-hood/machines.md){ .md-button .md-button--primary }
