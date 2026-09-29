# Services

An agent's sandbox gives it a fixed set of services, provided by the attested machine it runs on, and nothing else. Together they're the WebAssembly Interface Type (WIT) every agent is built against, frozen as `shonin:agent`:

| Service | What it gives |
| --- | --- |
| `drivers` | Submit orders and hear what happened to each; a stream of events on the vault's own accounts at the venue; a read of those accounts' state |
| `transfers` | Move money between the vault, its stations and their accounts, and over the [bridges](bridges.md) your agent names |
| `vault` | A stream of three events: `claim-pending` when a Depositor is leaving, `buffer-low` when cash at home falls to the idle floor, and `halting` when the vault is stopping |
| `data` | Shared market data the attested machine fetches once and hands to every agent that wants it |
| `websocket` | Live connections to other services, which the attested machine holds for it |
| `state` | A scratch cache for the agent's own use. It can be wiped at any time, including on every restart, so an agent must never rely on it |
| `secrets` | Values you sealed to your agent, such as an API key, readable only inside the attested machine |
| `inference` | AI models you shipped with your agent, run by the attested machine under a time budget |

An order is opaque bytes that its driver understands, decoded inside the attested machine. You build them with a typed crate from the SDK, so the decision is yours and the signing is ours.

## The vault's events

- **`claim-pending`:** a Depositor is leaving, and the vault is closing their share of your positions itself. You don't have to do anything; it tells you so your agent can make sense of positions that shrank.
- **`buffer-low`:** the vault's cash at home has fallen to the idle floor, so it won't let any more money out to the stations. Bring some home with `transfers` if you want withdrawals to keep being paid at once.
- **`halting`:** the vault is stopping, because it was killed, it was frozen, or a retirement's week is up. Your agent has 10 minutes to wind down in its own way before the vault closes whatever is left. It isn't sent when the tank runs dry: the agent simply stops.

## Moving money

`transfers` is how your agent moves money around its own vault: out to a station, between a station's accounts, home again, or across a bridge. Every destination is one of the vault's own, checked by the contracts, and the vault never lets money out if that would leave less than its idle floor at home.

## Secrets

Seal your secrets, such as an API key for a paid data service, in the web app before you [activate](publishing.md#activate-it) your agent. They're encrypted in your browser to your agent's build, so only the attested machine running it can read them.

Once the vault is live, only venue credentials can be changed, such as a venue's API key that has to be renewed, and every change shows on the vault's page. Everything else stays as it was when Depositors chose your agent.

[Next: drivers →](drivers.md){ .md-button .md-button--primary }
