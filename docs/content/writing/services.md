# Services

An agent's sandbox gives it a fixed set of services, provided by the attested machine it runs on, and nothing else. Together they're the WebAssembly Interface Type (WIT) every agent is built against, frozen as `shonin:agent`. The WIT itself ships with the SDK: [`wit/agent.wit`](https://github.com/ShoninFi/shonin-platform/blob/main/packages/strategy-sdk/wit/agent.wit), with the standard WASI interfaces it uses under `wit/deps/`.

| Service | What it gives |
| --- | --- |
| `drivers` | Submit orders and hear what happened to each; a stream of events on the vault's own accounts at the venue; a read of those accounts' state |
| `transfers` | Move money between the vault, its stations and their accounts, and over the [bridges](bridges.md) your agent names |
| `vault` | A stream of three events: `claim-pending` when a Depositor is leaving, `buffer-low` when cash at home falls to the idle floor, and `halting` when the vault is stopping |
| `data` | Shonin's shared venue streams: Hyperliquid's prices, books and funding, Lighter's books and Chainlink's token prices, each value stamped with when it arrived |
| `websocket` and `wasi:http` | Connections and requests to any host on the internet, outbound only, if your agent declares `network` |
| `wasi:keyvalue` | Your agent's state: 64 MiB of keys and values, empty every time it starts |
| `secrets` | Values you sealed to your agent, such as an API key, readable only inside the attested machine |
| `inference` | AI models you shipped with your agent, run by the attested machine under a time budget |

An order is opaque bytes that its driver understands, decoded inside the attested machine. You build them with a typed crate from the SDK, so the decision is yours and the signing is ours.

## What came of an order

Each order you submit comes back with one of three answers:

- **Accepted:** the venue took it, and the answer carries its receipt, such as the fill.
- **Rejected:** nothing happened. The venue or the contracts said no and gave a reason, or the order couldn't be sent at all.
- **Unknown:** the order was sent and no answer came back, so it may still land. Read your account before you act as if it didn't, because sending it again could do it twice.

## The vault's events

- **`claim-pending`:** a Depositor is leaving, and the vault is closing their share of your positions itself. It names the claim and the fraction of every position being closed for it. You don't have to do anything; it tells you so your agent can make sense of positions that shrank.
- **`buffer-low`:** the vault's cash at home has fallen to the idle floor, so it won't let any more money out to the stations. Bring some home with `transfers` if you want withdrawals to keep being paid at once.
- **`halting`:** the vault is stopping, because it was killed, it was frozen, or a retirement's week is up. Your agent has 10 minutes to wind down in its own way before the vault closes whatever is left. It isn't sent when the tank runs dry: the agent simply stops.

## Moving money

`transfers` is how your agent moves money around its own vault: out to a station, between a station's accounts, home again, or across a bridge. Every destination is one of the vault's own, checked by the contracts, and the vault never lets money out if that would leave less than its idle floor at home.

Some moves take more than one step: from the vault to a venue, the money goes through the station on the way. So a move tells you how far it got:

- **Done:** all of it arrived.
- **Refused:** none of it moved, and you're told why.
- **Unknown:** it was sent and no answer came back, so it may still land. Check the balances before you move the same money again.
- **Partial:** it stopped on the way, and says where the money is now (always one of the vault's own accounts) and whether the next step may still land.

`balances` says what each of the vault's accounts holds now, and leaves out any account whose read failed. `read-balances` reads the same accounts and says which of them it couldn't read, so a total that misses one is never taken for the vault's value.

## Shared venue streams

Venues limit how much one address may ask of them, and every agent on a machine shares one. So the attested machine holds a single connection to each venue, subscribes once to every feed any agent wants, and hands each value to every agent that asked for it:

- **Hyperliquid:** every market's mid price, any market's order book, and a perpetual's funding rate with its mark and oracle prices.
- **Lighter:** order books.
- **Chainlink on Robinhood Chain:** each stock token's price feed.

Every value carries the time it arrived, and the venue's own time where it gives one. Nothing is promised about how fresh a value is beyond those stamps, and an agent that reads too slowly is handed the newest values rather than every one. Subscribe with the SDK's typed feeds, and read the values with the same crate.

The streams are Shonin's own and part of the attested machine. Anything else your agent wants, it fetches itself over the network.

## The network

An agent that declares `network` in its `shonin.toml` can reach any host on the internet, through `wasi:http` for requests and the `websocket` service for live connections:

```toml
capabilities = ["network"]
```

- **Outbound only.** Nothing connects in to your agent, and it can't reach the machine it runs on or that machine's network.
- **TLS ends in the attested machine.** Your agent never holds a socket or a certificate; the machine checks each host's certificate and hands your agent what the host sent.
- **Nothing is reconnected for you.** When a connection drops, your agent decides whether to open another.
- **Held to your allowances.** Open connections and subscriptions together, requests and messages a minute, and bytes a day all come from your vault's allowances, and each message or body can be at most 4 MiB. Past any of them the call is refused and your agent decides what to do.

You declare the capability, not the hosts, so a new data source never means a new agent. What your agent reads isn't recorded; the AI Review reads how your code uses it, and Depositors see that your agent reaches the network.

## State

Your agent's state is a key-value store of its own, through the standard `wasi:keyvalue` interfaces (the SDK wraps them for the common case). It holds 64 MiB, counted as the bytes of every key and value, and a write that would go past that is refused.

It's **empty every time your agent starts**: after a fault, a change of allowances, a move to another machine, anything. That's on purpose. Positions, balances and open orders are on the venue anyway, so an agent that rebuilds from there on every start is always ready for the restart it didn't see coming. Use state for what's cheaper to keep than to work out again.

## Secrets

Seal your secrets, such as an API key for a paid data service, in the web app before you [activate](publishing.md#activate-it) your agent. They're encrypted in your browser to your agent's build, so only the attested machine running it can read them.

Once the vault is live, only venue credentials can be changed, such as a venue's API key that has to be renewed, and every change shows on the vault's page. Everything else stays as it was when Depositors chose your agent.

[Next: drivers →](drivers.md){ .md-button .md-button--primary }
