# Bridges

A bridge moves money between chains: from the vault out to a station on another chain, and back again. Only an agent that trades on more than one chain needs one. Bridges are other people's services: Shonin doesn't run them or put up money for them.

## The bridges

| Bridge | Name in `shonin.toml` | Moves | How it works |
| --- | --- | --- | --- |
| Circle's CCTP | `cctp` | USDC, between Base, Hyperliquid and Solana | Circle destroys the USDC on one chain and creates the same amount on the other, so there's no pool of money in the middle to lose |
| Across | `across` | Into and out of USDG on Robinhood Chain | A relayer pays out on the other side straight away, and is repaid from Across's own pools |

A bridge is only listed once every route it offers here has been confirmed to work.

You name the bridges your agent uses in `shonin.toml`, one table each:

```toml
[bridges.cctp]
```

Its vault's page shows them before anyone deposits.

## What keeps money safe

- **It only ever goes to the vault's own address.** When a station sends money into a bridge, the station itself checks, on the chain, that it will arrive at the vault's own address on the other side. Nothing else is accepted, whatever the agent asks.
- **Only bridges on this list can be used,** and only the ones your agent names.
- **Your agent moves it,** through the `transfers` [service](services.md). The vault never lets money out if that would leave less than its idle floor at home.

## What a bridge risks

Money is exposed to a bridge while it crosses. If a bridge fails, or is hacked, money in it can be lost, and that's a risk you choose on your Depositors' behalf by naming it.

**Money in transit** still counts in the vault's value for up to an hour. If it hasn't arrived by then, it counts as zero until it does, and the vault's page flags it, so nobody deposits or withdraws at a value that includes money that may not come.

[Next: publishing →](publishing.md){ .md-button .md-button--primary }
