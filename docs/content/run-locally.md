# Run it locally

The whole platform runs on your own machine: a development chain, the contracts on it, a stand-in venue, the build service, the reviewer, an executor, the keeper, the indexer and the web app. The site shows only what that chain produced.

Nothing needs a key, an account or a cloud project.

!!! warning "The repository is not public yet"
    Everything below works, and it is what we run. You cannot clone it today, because the source has not been published. That is [on the checklist](checklist.md#the-site), and until it is done this page describes something you have to take our word for — which is the one thing this project is supposed to avoid asking of anyone.

## Prerequisites

- [Rust](https://rustup.rs) — the toolchain is pinned and installs itself on first use, with `wasm32-unknown-unknown` for the web app and `wasm32-wasip2` for agents
- [Foundry](https://getfoundry.sh) for the contracts and the chain (`forge`, `anvil`, `cast`)
- [Task](https://taskfile.dev)
- [Trunk](https://trunkrs.dev) for the frontend
- [Node.js](https://nodejs.org), only to install wrangler

## One command

```bash
task setup    # pinned toolchain, contract submodules, wrangler
task dev      # chain, contracts, every service, and the web app
```

Then open **http://localhost:19080**.

```bash
task services:seed   # publish the reference agents through the real flows
task down            # stop everything task dev started
```

`task services:seed` publishes several versions the way an author would: one superseded by a newer one, one sealed, one that deliberately fails to build, and one that is then killed — so every state the site has to handle actually exists on your chain.

## What is listening

| Port | What |
| --- | --- |
| 19080 | The web app, with live reload; `/api` is proxied to the API |
| 19081 | The indexer and the public HTTP API |
| 19083 | The chain |
| 19084 | The build service |
| 19085 | The executor |
| 19086 | The reviewer |
| 19087 | The coordinator (the keeper) |
| 19088 | The devnet: the chain's clock and its market |

Everything a run writes lives under `.shonin/local`, which is safe to delete; the next run rebuilds it.

## The clock runs fast

A block is mined every second and advances an hour of chain time, so a day of fees passes in twenty-four seconds and a vault reaches the ninety live days it needs to be ranked in about thirty-six minutes. Crystallisation, claims, freezes and blessing renewals all become things you can watch rather than read about.

## The development wallet

The chain hands out ten unlocked accounts from a mnemonic everybody knows, which is exactly what a development chain wants and exactly why nothing of value goes near one.

The web app offers one of them as a **development wallet**. It signs nothing in your browser, because the chain already holds the key. The browser-wallet path is the real one and runs the same code against the same contracts.

## What stands in

| Real | Standing in |
| --- | --- |
| The contracts, their accounting and their access control | The venue is a contract with markets, marks, a funding index and trade-only agents |
| Share maths, fees, the high-water mark, the tank | The asset is a local dollar token anyone may mint |
| Claims, the exit band, freezes, kills | Attestation is a development verifier that accepts a signed journal |
| The build service, its queue and its pinned recipe | The image digest stands for the service rather than measuring an enclave |
| The review, run against a real model with the pinned prompt and schema | |
| The executor running a real WebAssembly component under fuel, allowances and signed health reports | |
| The record, read from each vault's value per share | |

Deliberately absent: a bridge, a second chain, remote legs, an enclave, model inference, and any network for an agent to reach. Those are on the [checklist](checklist.md), and a local chain has no way to exercise them honestly.

## Publishing an agent to it

```bash
# from the root of this repository, with task dev running
cargo run -q -p shonin-cli -- publish path/to/your-agent            # packs, creates the vault, funds the tank, submits
cargo run -q -p shonin-cli -- publish path/to/your-agent --sealed   # the same, with the source kept out of the store
```

It publishes as the chain's author account, so it needs no key of yours.

or from the web app's **Publish** page: drop the packed archive, fund the tank, and watch the queue.

## The API

Every page reads the same public API the site does:

```bash
curl -s http://localhost:19081/api/snapshot | jq '.strategies[0]'
curl -s http://localhost:19081/api/config   | jq '.terms'
```

See [The API](reference/api.md).
