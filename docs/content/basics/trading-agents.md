# Trading agents

An agent is a **real program**, written in Rust, compiled to a WebAssembly component, running continuously on an [attested machine](attested-machines.md). It has its own memory, its own control flow, and a long-lived async function that the host starts once and never schedules again. Everything it does, it does inside that call.

```rust
// It wakes on what the market and the vault tell it, and decides.
while let Some(woke) = waking.next().await {
    match woke {
        Woke::Venue => step(&mut mind).await?,          // a fill, a funding payment, a price
        Woke::Vault(ClaimPending(_)) => step(&mut mind).await?,  // a depositor is leaving
        Woke::Vault(Halting(_)) => return flatten(&mind).await,  // unwind, don't be unwound
    }
}
```

It can reach out. It can compute. It can call a model it shipped.

| It can | Through |
| --- | --- |
| Watch its own fills, funding and account state, as a live stream | `venues` |
| Hear that a depositor is leaving, or that the vault is stopping | `vault` |
| Read market data the executor fetches once and shares | `data` |
| Hold an open websocket to a host, terminated by the executor | `websocket` |
| Run an ONNX model it packed in its own bundle | `inference` |
| Keep a cache across events, and across a restart | `state` |
| Use a secret sealed to its own bundle hash, which we cannot read | `secrets` |
| Submit orders, cancels and capital moves, and be told what happened to each | `venues` |

That is the whole interface, frozen as `shonin:strategy@0.1.0`: seven interfaces, `venues`, `vault`, `data`, `websocket`, `inference`, `state` and `secrets`. An author writes against it with an SDK; the host implements it inside the measured image. No filesystem, no environment, no sockets of its own. Everything the agent reaches, it reaches through one of those seven. [Agents](../how-it-works/agents.md) has the detail.

!!! abstract "Agent, or strategy?"
    Same thing. These pages and the app say **agent**. The contracts say *strategy* (`StrategyRegistry`, `strategyId`, `shonin:strategy`), so that word appears here only where it names something in them.

So an agent can fetch a funding curve, run a model over it, size a position from the output, place both legs of a hedge, and unwind itself when a depositor leaves. It is software, with the tools software has.

[Next: attested machines →](attested-machines.md){ .md-button .md-button--primary }
