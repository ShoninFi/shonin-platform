# Vault and Driver List

## Vault chains

Every vault lives on one chain, named by `chain` in the [`[vault]`](configuration.md#vault) table. It's where Depositors put money in and take it out, and where the $1 to publish is paid.

| Chain | Name in `shonin.toml` |
| --- | --- |
| Base | `base` |
| Hyperliquid | `hyperliquid` |
| Robinhood Chain | `robinhood` |
| Solana | `solana` |

If your agent uses one driver, choose that driver's chain. The vault and its station are then side by side, with no bridge in between.

## Drivers

An agent trades on an exchange through a **driver**: the part of Shonin that knows how to work with that one exchange. It approves the vault's trade-only keys there, works out what the vault holds, and closes positions when someone withdraws. An agent can only use the drivers it names in [`[drivers.<name>]`](configuration.md#driversname) tables.

| Driver | Name in `shonin.toml` | What it trades | Chain |
| --- | --- | --- | --- |
| Hyperliquid | `hyperliquid` | Perpetuals and spot | Hyperliquid |
| Robinhood Chain stock tokens | `robinhood-stocks` | Tokenised stocks, through approved exchanges on the chain | Robinhood Chain |
| Arcus | `arcus` | Stock perpetuals | Robinhood Chain |
| An on-chain game | To be announced | Matches played for stakes | Solana |

### Each driver's settings

Each driver has its own table in `shonin.toml`, named after it, for whatever that driver needs to know. None of the drivers above needs any settings yet, so an empty table is enough:

```toml
[drivers.hyperliquid]
```

As drivers need settings of their own, such as which markets or accounts to use, they'll be listed here, driver by driver.

[Next: publishing →](publishing.md){ .md-button .md-button--primary }
