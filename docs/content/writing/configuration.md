# Configuration

An agent is configured in two places:

- **On its vault:** the things you can change while it runs.
- **In `shonin.toml`:** the chain its vault lives on, and the agent's description, claims, drivers, bridges and any extra limits on who can deposit, which are decisions locked in forever.

## Vault configuration

Two settings live on the vault itself. You choose them when you [activate](publishing.md#activate-it) your agent, and you can change them on the live vault whenever you like. The vault's page shows every change.

- **Allowances:** how much computing and gas your agent may use, which is also the most your [tank](../tank.md) can be charged for it. They cover computing, memory, requests to other services, data, live connections, time spent running AI models, and gas in dollars a day.
- **The idle floor:** how much of the vault stays at home as cash, ready for withdrawals: 5% by default, and 0% is allowed for an agent that needs every dollar out trading. Raising it is instant; lowering it takes a day, so Depositors see it coming.

## `shonin.toml`

Every agent carries one configuration file, `shonin.toml`, at the root of its project.

- **It's read by the attested machines, not taken on trust.** The Reviewer reads it beside your code, and the builder reads it to learn which chain your vault lives on, and which other chains its drivers need.
- **It never changes.** It's part of what gets built and recorded, so nobody can change what an agent promised after Depositors chose it. Changing it means publishing a new agent.
- **It's always shown on the agent's page**, even when the source is sealed. Depositors see the same vault chain, description, claims, drivers and bridges the Reviewer read, whether or not they can read the code.

```toml
capabilities = ["network"]

[vault]
chain = "hyperliquid"

[description]
title = "My agent"
text = "What it does, in your own words."
claims = [
    "Something it promises about itself",
]

# one table for each driver it trades through
[drivers.hyperliquid]
[drivers.robinhood]

# and one for each bridge it uses
[bridges.across]
```

This agent's vault lives on Hyperliquid, where it trades. It also trades stock tokens on Robinhood Chain, uses one bridge to move money between the two, and reaches the network for data of its own.

You don't give your agent an id: it gets one automatically when you submit it. Its `title` is the name people see.

### `capabilities`

Optional. What your agent may use beyond its drivers. Today there is one, `network`: reaching any host on the internet, described on [Services](services.md#the-network). An agent that doesn't list it has no network at all, and a name that isn't a capability is refused.

### `[vault]`

**`chain`**: the chain your vault lives on. It's where Depositors put money in and take it out, in that chain's dollar, and where you pay the 1 USDC to publish.

| Chain | Name | Its dollar |
| --- | --- | --- |
| Base | `base` | USDC |
| Hyperliquid | `hyperliquid` | USDC |
| Robinhood Chain | `robinhood` | USDG |
| Solana | `solana` | USDC |

It needn't be where your agent trades: its drivers can trade on other chains, reached through its bridges. But if your agent uses one driver, choose that driver's chain, so the vault and its station sit side by side with no bridge in between.

### `[description]`

- **`title`** and **`text`**: what the agent does, in your own words.
- **`claims`**: what it promises about itself, one plain sentence each, such as a limit it never goes past.

The [AI Review](../basics/ai-review.md) reads your description and your claims, and checks them against what the code actually does. A claim your code doesn't keep counts against your agent's rating for an accurate description.

### `[drivers.<name>]`

One table for each [driver](drivers.md) the agent trades through, holding whatever settings that driver needs. An agent can only ever trade through the drivers it lists here.

### `[bridges.<name>]`

One table for each [bridge](bridges.md) the agent uses to move money between chains. They're shown on the vault's page before anyone deposits. An agent that only trades on its vault's chain has none.

### `[depositors]`

Optional. Your vault already refuses deposits from US persons, sanctioned persons and anyone its [drivers' venues exclude](drivers.md#who-a-venue-excludes). Here you can add more places, by country code:

```toml
[depositors]
exclude = ["GB", "CA"]
```

You can only add to the list, never shorten it. Like the rest of the file, it's fixed once the agent is published, and it's shown on the vault's page.

## Your agent's own settings

Neither place holds the numbers your agent decides with, like which market to trade or how large an order may be. Those belong in your code. The template keeps them together as constants in `src/config.rs`, so they're easy to find and change, and the Reviewer reads them like the rest of your code.

[Next: services →](services.md){ .md-button .md-button--primary }
