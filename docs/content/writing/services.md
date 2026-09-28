# Services

An agent's sandbox gives it a fixed set of services, provided by the attested machine it runs on, and nothing else. Together they're the WebAssembly Interface Type (WIT) every agent is built against, frozen as `shonin:agent`:

| Service | What it gives |
| --- | --- |
| `venues` | Submit orders and hear what happened to each; a stream of the vault's own account events; a read of the vault's account state |
| `vault` | A stream of three events: `claim-pending` when a Depositor is leaving, `buffer-low` when cash at home is running low, and `halting` when the vault is stopping |
| `data` | Shared market data the attested machine fetches once and hands to every agent that wants it |
| `websocket` | Live connections to other services, which the attested machine holds for it |
| `state` | A scratch cache for the agent's own use. It can be wiped at any time, including on every restart, so an agent must never rely on it |
| `secrets` | Values you sealed to your agent, such as an API key, readable only inside the attested machine |
| `inference` | AI models you shipped with your agent, run by the attested machine under a time budget |

An order is opaque bytes that the venue's own adapter understands, decoded inside the attested machine. You build them with a typed crate from the SDK, so the decision is yours and the signing is ours.

[Next: example: a rebalancer →](rebalancer.md){ .md-button .md-button--primary }
