# Drivers

A driver is what lets your agent use one venue. The vault and its [stations](../under-the-hood/stations.md) know nothing about any venue: everything venue-specific lives in its driver, plugged into the station on that venue's chain. So a new venue is a new driver, and nothing else changes.

You choose drivers by name in `shonin.toml`, one `[drivers.<name>]` table each. Depositors never need to think about them: a vault's page just shows the venues it trades on.

## The drivers

| Driver | Name in `shonin.toml` | What it trades | Chain |
| --- | --- | --- | --- |
| Hyperliquid | `hyperliquid` | Perpetuals and spot | Hyperliquid |
| Robinhood Chain stock tokens | `robinhood-stocks` | Tokenised stocks, through approved exchanges on the chain | Robinhood Chain |
| Lighter | `lighter` | Stock and crypto perpetuals | Robinhood Chain |
| Orca | `orca` | Liquidity in concentrated-liquidity pools, and swaps | Solana |

Each driver has its own table, named after it, for whatever that driver needs to know. None of the drivers above needs any settings yet, so an empty table is enough:

```toml
[drivers.hyperliquid]
```

As drivers need settings of their own, such as which markets or accounts to use, they'll be listed here, driver by driver.

## Two halves

A driver has two halves that work on the same account: one inside the attested machine, which the agent's orders go through, and one on the chain, which controls the account itself.

```mermaid
flowchart TB
    subgraph EXE["Inside the attested machine"]
        direction TB
        AG["<b>Agent</b><br/>decides what to trade"] -->|"an order"| DE["<b>Driver: executor half</b><br/>checks the order, signs it<br/>with the trade-only key"]
    end
    subgraph CH["On the venue's chain"]
        direction TB
        ST["<b>Station</b><br/>holds the money"] --- DC["<b>Driver: on-chain half</b><br/>approves the key, values the account,<br/>closes positions for withdrawals"]
    end
    ACC["<b>The vault's account at the venue</b><br/>owned by the station"]
    DE -->|"places the order"| ACC
    DC -->|"controls"| ACC

    classDef machine fill:#0d948818,stroke:#0d9488,color:inherit
    classDef contract fill:#4338ca18,stroke:#4338ca,color:inherit
    classDef driver fill:#d9770618,stroke:#d97706,color:inherit
    class AG machine
    class ST,ACC contract
    class DE,DC driver
```

| Half | What it does |
| --- | --- |
| **On the chain** | Prepares and checks the calls the station makes at the venue: approving the vault's trade-only keys once they're [blessed](../under-the-hood/attestation.md#blessing-a-key) for the vault, and withdrawing them when they're not; checking that each trade the station carries out is only a trade; working out what the station holds there; and closing positions when someone withdraws, a vault is killed or an agent is retired |
| **In the executor** | Turns the agent's orders into the venue's own, and signs them with the vault's trade-only key, only ever for the vault's own accounts. Streams the account's fills and balances back to the agent, and the venue's public market data to every agent that wants it |

An agent never talks to a venue directly. It builds its orders with the driver's typed library from the SDK, hands them over, and the driver checks and signs them inside the attested machine.

**A driver never holds money.** The station owns the vault's account at the venue, and makes every call there itself: the driver only prepares each call and checks it. Whatever a driver says, the station still keeps its own rules, so money only ever goes to the vault's own addresses: it only calls the targets the driver declared, and never the vault, the station itself, its factory or the registry, whatever the driver lists. A driver with a bug could prepare a bad call, but it can't move money by itself, and it can never reach the vault's cash.

**One driver serves everyone.** Each venue has one driver on its chain, shared by every station there. A fixed version reaches running vaults the same way a fix to a machine image does, after the same public wait, so nothing needs redeploying.

## The promises every driver keeps

Venues work very differently, but a driver only counts if it keeps the same promises for every one of them:

- **Keys can trade, never withdraw.** The vault's keys at the venue can only place and cancel orders.
- **Money only goes to the vault's own accounts,** whatever the key asks for.
- **Holdings are worked out, not reported:** from the venue's on-chain state, or, for a venue that keeps its accounts off-chain, an attested read that the vault's page labels as such.
- **Positions can always be closed** for a withdrawal, by the contracts alone or by an unwinder, and the vault's page says which.
- **The venue's exclusions are carried through.** Each driver carries the list of people its venue doesn't accept, and every vault using it refuses their deposits.

### How money stays in

Something has to stop the vault's keys from sending money anywhere but the vault's own accounts. There are two ways that can happen, and each vault's page says which applies to each venue:

| | Who stops a withdrawal | For example |
| --- | --- | --- |
| **The venue enforces it** | The venue itself: the key it's given can trade, but only the station, as the account's owner, can move money out | Hyperliquid, where the station owns the trading account and the executor's key is a trade-only agent of it; Lighter, where a withdrawal can only ever pay the station that owns the account |
| **The station enforces it** | The station's own account, whose rules only allow calls to that venue's contracts, with the station itself as the only receiver | Stock tokens on Robinhood Chain, bought and sold only through approved exchanges; liquidity positions on Orca |

A venue where only the key's own honesty would stop a withdrawal can't hold a vault's money at all, so it can't have a driver.

### Who a venue excludes

Venues don't accept everyone: most exclude US persons, and some exclude residents of other places too. A vault trading at a venue has to respect that, so each driver carries its venue's list, taken from the venue's own terms.

A vault's list is every one of its drivers' lists put together, plus US persons and sanctioned persons, which every vault excludes. So an agent that uses several drivers takes on the strictest of them. You can add places to your own vault's list in [`shonin.toml`](configuration.md#depositors), but you can't remove any.

The vault enforces it when someone deposits: their wallet signs a statement that they aren't on the list and aren't acting for someone who is, and the contract won't take the deposit without it. [Depositors](../depositors.md#who-can-deposit) explains what they see.

## Adding a driver

A new driver never touches a running vault. Existing agents keep the drivers they chose, and once a new driver is live, new agents can name it.

- **Anyone can ask for one,** on our Discord.
- **Anyone can build one.** The driver interface and a test harness are published, so a venue, or a developer who needs one, can write it.
- **Every driver is approved the slow, public way,** like a machine image. Its executor half is added to our executor and unwinder images, whoever wrote it, and its on-chain half is added to the [registries](../under-the-hood/registries.md#the-attestation-registry). Both wait out the same public wait before they count.
- **Revoking a driver stops trading through it at once,** but closing positions through it keeps working, so Depositors can always get out.

[Next: bridges →](bridges.md){ .md-button .md-button--primary }
