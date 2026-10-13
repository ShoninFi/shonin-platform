# shonin-strategy-sdk

The Rust SDK for writing [Shonin](https://shonin.fi) agents: trading programs compiled to a WebAssembly component, run in an attested machine, trading for a vault through keys that can't withdraw its money.

An agent is one long-lived async program. Everything it can reach is an import of the `shonin:agent` world, whose WIT is in [`wit/agent.wit`](https://github.com/ShoninFi/shonin-platform/blob/main/packages/strategy-sdk/wit/agent.wit): drivers to submit orders to venues, transfers between the vault's own accounts, the vault's events, Shonin's shared market data, outbound network access, key-value state, sealed secrets and inference. The SDK carries the generated bindings, each driver's typed payloads and helpers for common jobs.

## Start here

- The template: [ShoninFi/shonin-agent-template](https://github.com/ShoninFi/shonin-agent-template), a working agent with tests.
- The guide: [docs.shonin.fi/writing](https://docs.shonin.fi/writing/), from getting started to publishing.

## Using it

Depend on it from crates.io, as on any crate:

```toml
[dependencies]
shonin-strategy-sdk = "0.1.1"
```

The build service compiles every agent against its own copy of this crate, so the version an agent declares is the interface it is written against. It takes a `Cargo.lock` naming the SDK from crates.io at 0.1.1 or an earlier version compatible with it, and refuses one naming a newer version, or naming the SDK as a path package (patched to a copy of it), when the project is uploaded.

## License

Apache-2.0. The SDK is compiled into authors' agents; the rest of the platform is under its own license.
