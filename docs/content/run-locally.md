# Run it locally

The whole platform runs on your own machine: a development chain, the contracts on it, a stand-in venue, the build service, the Reviewer, an executor, the keeper, the indexer and the web app. The site shows only what that chain produced.

Nothing needs a key, an account or a cloud project.

## Prerequisites

- [Rust](https://rustup.rs): the toolchain is pinned and installs itself on first use, with `wasm32-unknown-unknown` for the web app and `wasm32-wasip2` for agents
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

`task services:seed` publishes several agents the way an Author would: one sealed, one that deliberately fails to build, and one that is then killed, so every state the site has to handle actually exists on your chain.

## What is listening

| Port | What |
| --- | --- |
| 19080 | The web app, with live reload; `/api` is proxied to the API |
| 19081 | The indexer and the public HTTP API |
| 19083 | The chain |
| 19084 | The build service |
| 19085 | The executor |
| 19086 | The Reviewer |
| 19087 | The coordinator (the keeper) |
| 19088 | The devnet: the chain's clock and its market |

Everything a run writes lives under `.shonin/local`, which is safe to delete; the next run rebuilds it.

## The clock runs fast

A block is mined every second and advances an hour of chain time, so a week of fees passes in under three minutes and a vault builds up the record it needs to be ranked in well under an hour. Weekly fee payments, claims, freezes and blessing renewals all become things you can watch rather than read about.

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

From the web app's **Publish** page, with `task dev` running: pay the deposit, choose your agent's folder, and watch the queue until its vault opens. The development wallet signs as the chain's Author account, so it needs no key of yours.

The same two steps work from a script: one transaction to pay the deposit, then one signed request to [`POST /api/submissions`](reference/api.md#post-apisubmissions).

## The API

Every page reads the same public API the site does:

```bash
curl -s http://localhost:19081/api/snapshot | jq '.strategies[0]'
curl -s http://localhost:19081/api/config   | jq '.terms'
```

See [The API](reference/api.md).
