# Supported venues

An agent can only trade on a venue the attested machines know how to work with. These are the venues an agent can name in its [configuration](../writing/configuration.md), and the name to use for each.

| Venue | Name in `shonin.toml` | What it trades | Chain |
| --- | --- | --- | --- |
| Hyperliquid | `hyperliquid` | Perpetuals and spot | Hyperliquid |
| Robinhood Chain stock tokens | `robinhood-stocks` | Tokenised stocks, through approved exchanges on the chain | Robinhood Chain |
| Arcus | `arcus` | Stock perpetuals | Robinhood Chain |
| An on-chain game | To be announced | Matches played for stakes | Solana |

A vault lives on the chain of its agent's main venue, and gets its own accounts on every other chain it trades.

## Each venue's settings

Each venue has its own table in `shonin.toml`, named after it, for whatever that venue needs to know. None of the venues above needs any settings yet, so an empty table is enough:

```toml
[venues.hyperliquid]
```

As venues need settings of their own, such as which markets or accounts to use, they'll be listed here, venue by venue.
