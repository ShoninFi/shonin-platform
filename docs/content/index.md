# Shonin

**Smart contracts are public and auditable. Trading agents should be too.**

If a program is trading your money, you want to know three things: what it does, what it can do with your money, and whether you can get out. On Shonin, each answer is something you can check rather than something you are told.

An author publishes a trading **agent** as source code. We compile it, a model reads it and scores it, and attested machines run it with keys that can trade and cannot withdraw. You put money into a vault behind the agents you like. Every trade, every fee and every unit of value lands on a chain, where it stays.

## What an agent actually is

An agent is a **real program**, written in Rust, compiled to a WebAssembly component, running continuously on a machine we attest. It has its own memory, its own control flow, and a long-lived async function that the host starts once and never schedules again. Everything it does, it does inside that call.

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

That is the whole interface, frozen as `shonin:strategy@0.1.0`: seven interfaces, `venues`, `vault`, `data`, `websocket`, `inference`, `state` and `secrets`. An author writes against it with an SDK; the host implements it inside the measured image. No filesystem, no environment, no sockets of its own. Everything the agent reaches, it reaches through one of those seven. [Agents](how-it-works/agents.md) has the detail.

!!! abstract "Agent, or strategy?"
    Same thing. These pages and the app say **agent**. The contracts say *strategy* — `StrategyRegistry`, `strategyId`, `shonin:strategy` — so that word appears here only where it names something in them.

So an agent can fetch a funding curve, run a model over it, size a position from the output, place both legs of a hedge, and unwind itself when a depositor leaves — because it is software, with the tools software has.

!!! info "What runs today"
    The venue stream, the vault stream, market data, the state cache, sealed secrets and the full order path all work on the development chain. Outward websockets and model inference are in the frozen interface but not yet implemented by the executor: a call to either returns an error rather than pretending. Both are on the [checklist](checklist.md#execution).

## What it cannot do

You have just been told that a program with a network connection and a model of its own is going to trade your money. The obvious question is what stops it walking off with the lot.

The agent never holds a key and never signs anything. It submits a decision; the host signs it. And the key the host signs with is registered at the venue as a **trade-only agent key**: it can place orders, and it cannot move money out. Moving money at all goes through the vault's contract, which will only move it between accounts the vault itself owns.

So nobody — the agent, its author, an executor, us, or somebody who has stolen all our keys — can **withdraw** your deposit or send it to an address of their own. That is enforced by the venue and by the contract, not by a sandbox we promise to maintain or a review that caught everything.

That is the trust bar: *halt, never withdraw*. Anyone can switch an agent off. No key can take the money out.

```mermaid
flowchart LR
    K(["the key the host<br/>signs the agent's<br/>decisions with"])

    subgraph CAN["can"]
        direction TB
        T["place an order"]
        C["cancel an order"]
    end

    subgraph CANNOT["cannot — refused by the venue and by the contract"]
        direction TB
        W["withdraw"]
        X["send to an outside address"]
        A["approve somebody else"]
    end

    K ==> CAN
    K -.-> CANNOT

    classDef key fill:#4338ca20,stroke:#4338ca,color:inherit,font-weight:bold
    classDef yes fill:#0d948825,stroke:#0d9488,color:inherit
    classDef no fill:#dc262612,stroke:#dc2626,color:inherit,stroke-dasharray:5 4
    class K key
    class T,C yes
    class W,X,A no
    style CAN fill:none,stroke:#0d9488,stroke-width:1px
    style CANNOT fill:none,stroke:#dc2626,stroke-width:1px,stroke-dasharray:5 4
```

!!! warning "What a trade-only key can still do"
    It can lose money, and a key that can trade can lose it on purpose. An agent could buy from an account its author controls above the market and sell to it below, in a thin market, and move value out one bad trade at a time without ever withdrawing anything.

    Nothing on the chain bounds the price an agent trades at today, except when a claim is being settled. What stands against it is the published source, the review, the record, and your ability to leave. A bound on every order's price is [on the checklist](checklist.md#guarantees-first).

## What that buys you

You can leave whenever you want. A withdrawal is paid out of the vault's idle balance in the same transaction. If there is not enough sitting idle, the contract books a claim, closes your share of every position, and pays you when the cash is home. The function that drives that is public: if every service we run stopped tonight, you could send it yourself. That holds for a vault whose exit class is **direct**, where the contract can reach every venue it trades on. A vault that uses a venue the contract cannot reach is **served**, needs an operator to unwind that part, and says so on its page.

You can check the returns rather than believe them. The vault contract computes its own value from on-chain state, so a record is value per share over time, read from the chain. There is nothing to backfill and nothing to restate.

You can read the code. Source is published by default, and the build is designed so that anyone can compile it and compare hashes. An author can seal their source instead, and then what you are relying on is the attested build and the attested review, which the agent's page says plainly.

## Where to go next

<div class="grid cards" markdown>

- **:material-cash-multiple: I want to put money in**

    ---

    How to read an agent's record, what the fees actually take out of a year, and how leaving works.

    [Putting money in](depositors.md)

- **:material-code-braces: I want to publish an agent**

    ---

    What you write, what it can reach, what it costs to publish, and what you earn if it works.

    [Publishing an agent](authors.md)

- **:material-magnify: I do not believe you**

    ---

    Every claim on this site, and how to check it with a chain node and the published source.

    [Verify it yourself](verify.md)

- **:material-console: I want to read the code**

    ---

    The whole platform runs on your own machine with one command.

    [Run it locally](run-locally.md)

</div>

## Where the project is today

Everything above runs, end to end, on a local development chain: the contracts, the build service, the reviewer, an executor, the keeper, the indexer and the web app.

Nothing is deployed to a public chain yet. There is no live vault and no money in anything. The [build checklist](checklist.md) is the honest list of what stands between here and there, and it is long.

The web app is at [shonin.fi](https://shonin.fi/). It sits behind a login while the project is pre-release, so a link to it will ask you to sign in rather than let you in; what it shows is a development chain either way.

The source is not public yet either. That is on the checklist and it is near the top of it, because a page that tells you to check things for yourself is worth very little until you can.

These docs describe what is built. Where something is designed but not finished, the page says so rather than using the present tense and hoping.
