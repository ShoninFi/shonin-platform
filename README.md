# Shonin Platform

Shonin is an open platform for verifiable trading strategies with pooled capital. Authors publish strategies as WASM components, depositors pool capital in vaults, and attested executors run the strategies, able to move funds only between a vault's own accounts.

This repository is the platform: the contracts, the frontends, the attested machines and the services around them, and the tooling that runs them. Everything off-chain is Rust, in one Cargo workspace; the EVM contracts are Solidity, built with Foundry, and the Solana programs are built with Anchor.

## Prerequisites

- [Rust](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml` and installs itself on first use, with the `wasm32-unknown-unknown` target for the app and `wasm32-wasip2` for strategies.
- [Task](https://taskfile.dev) and [Trunk](https://trunkrs.dev).
- [Foundry](https://getfoundry.sh) for the EVM contracts and the local EVM chains (`forge`, `anvil`).
- The [Solana CLI](https://solana.com/docs/intro/installation) to deploy the Solana programs, [Anchor](https://www.anchor-lang.com) to build them, and [Surfpool](https://github.com/solana-foundation/surfpool) 1.2.0 or later for the local Solana cluster (earlier versions have no Ed25519 program offline). Set `SURFPOOL=/path/to/surfpool` if the one on your path is older.
- [Node.js](https://nodejs.org), only to install wrangler.
- Optional: [Docker](https://www.docker.com) and the [gcloud CLI](https://cloud.google.com/sdk) for the Google Cloud images and tasks.
- For the commit guard: [gitleaks](https://github.com/gitleaks/gitleaks), [jq](https://jqlang.org) and the [Claude Code](https://claude.com/claude-code) CLI. The reviewer uses the same CLI.

## Quickstart

```bash
task setup    # pinned Rust toolchain, wrangler, submodules, the commit guard
task dev      # the whole platform, locally; the dashboard is http://localhost:19088/
task down     # stop everything `task dev` started
task check    # contracts and programs, then every crate for each target it runs on
task lint     # rustfmt --check + clippy -D warnings + forge fmt --check
task test     # contract tests + native tests
task build    # release builds of the app and the marketing site
task deploy   # deploy the app to Cloudflare (app.shonin.fi and shonin.fi)
```

Run `task` on its own to list every task.

## Local development

### Two choices, made separately

1. **Where the services run:** here (`task dev`, `task dev:services`) or on Google Cloud (`task cloud:*`).
2. **Which chains they talk to:** `NETWORK=local` (the default), `testnet` or `mainnet`, set with `SHONIN_NETWORK` in `.env` or `NETWORK=` on a task's command line.

They are independent: running the services here against the testnets needs only `NETWORK=testnet`. The cloud never runs against local chains, and it never uses the gcloud default project: every cloud task takes its project from the network (`GCP_PROJECT_TESTNET`, `GCP_PROJECT_MAINNET`) and refuses to run while it is empty.

### What `task dev` runs

Three anvils standing in for the EVM vault chains (base, hyperliquid, robinhood) and a Solana cluster (Surfpool, offline); the devnet, which deploys the contracts to every EVM chain and the programs to the Solana cluster, then moves their clocks and plays their markets; the four machines; the API, the indexer and the keeper; the app; and the marketing site. Every machine and service serves all three EVM chains at once (and the executor, the indexer, the API and the keeper the Solana cluster too), and retries until each chain answers and its deployment exists, so they all start at once. `task services:seed` publishes to base, or to `VAULT_CHAIN=hyperliquid` and so on; `task services:seed VAULT_CHAIN=solana` seeds the Solana cluster (see below). `task dev:services` is the same without the frontends.

| Port | What |
| --- | --- |
| 19080 | The app, with live reload; `/api` is proxied to the API |
| 19081 | The API the app reads, running the indexer in-process |
| 19082 | The marketing site |
| 19084–19086 | The builder, the executor, the reviewer |
| 19087 | The keeper |
| 19088 | The devnet, and the **ops dashboard** |
| 19089 | The docs, under `task docs:dev` |
| 19090 | The unwinder |
| 19091 | The reference indexer on its own |
| 19100–19102 | The anvils: `evm:31337` base, `evm:31338` hyperliquid, `evm:31339` robinhood |
| 19110–19111 | The Solana cluster (Surfpool): RPC, websocket |

The base port is `SHONIN_START_PORT_DEV` (19080); every other port is a fixed offset from it, listed in `taskfiles/config.yml`.

**The ops dashboard** at `http://localhost:19088/` shows every chain of the network (key, RPC, the chain id or genesis it reports, its head and its clock), every machine and service and whether its `/health` answers, each chain's contract addresses and roles, and, once a Google Cloud project is set, what is running there. It refreshes itself, and it is local only.

**Everything a run writes** lives under `.shonin/<network>` (gitignored; `.shonin/local` for `task dev`): one deployment file per chain in `deployments/` (`solana-localnet.json` for the Solana cluster), the content-addressed store, the build directories and the Solana cluster's logs. It is safe to delete; the next run rebuilds it.

**The local chains' accounts** come from anvil's published mnemonic. Each role (admin, builder, reviewer, executor, keeper, author, ...) is one of them by index, listed in `.env.example`, so no key is written down anywhere. The app's **development wallet** is one of them: it signs nothing in the browser, because the chain already holds the key. A block is mined every second and advances an hour of chain time, so a vault reaches the thirty live days it needs for the leaderboard in about twelve minutes.

Worth knowing separately:

```bash
task chains:deploy                   # deploy the contracts to every local EVM chain
task services:seed                   # publish the reference agents: upload, pay $1, review, build, activate
task services:seed AGENT=funding-basis   # publish one agent, as it stands
task contracts:solana:build          # anchor build: needed once before the devnet can deploy the programs
task contracts:solana:deploy         # build, then deploy and set up the programs on the local cluster
task services:seed VAULT_CHAIN=solana   # the seed on Solana (below)
task services:seed VAULT_CHAIN=solana AGENT=funding-basis   # one agent through the API and the services, as on an EVM chain
```

**The local Solana cluster** is Surfpool, started offline and fresh by `task chains:solana`. The devnet deploys the programs from `contracts/solana/target/deploy` through the loader (the development checker and the stand-in driver final, as every checker and driver must be), sets each up with the deployer key, creates the dollar's mint, lists the stand-in venue's markets, proposes the images, the project and the stand-in driver, funds every role, and writes `deployments/solana-localnet.json`; then it moves the cluster's clock an hour a second and posts the venue's prices, as on the anvils. The roles are keys of the same published mnemonic, along Solana's own derivation path. A program's id is compiled into the services from the same keypairs, so build the programs (`task contracts:solana:build`) before the services. Every machine and service serves the Solana cluster: the build service and the Reviewer each with a Solana key of their own (the role key locally, blessed by the development checker; in the attested image a key derived for the image, a leaf of its attestation, blessed through the proof checker and stocked with lamports by the keeper), and nothing on the cluster is taken until it is blessed and stocked. The full seed on Solana plays every role's part itself, with each role's development key, so it needs none of them running (with `SHONIN_SEED_KEEPER_RUNS` set it leaves the keeper's calls to a running keeper): it publishes a strategy (paid, registered with the build service's signature and the Reviewer's go-ahead, reviewed), activates vaults, deposits and has them priced, puts money to work at the stand-in venue through the station's guard, books and pays a claim, retires one vault and kills another, which is wound up and closed with its depositor paid, checking each on the cluster.

### The testnets

The same stand-in stack deploys to the public testnets (Base Sepolia, Hyperliquid testnet, Robinhood Chain testnet, Solana devnet). There the roles are accounts derived from mnemonics you hold in `.env`, `SHONIN_TESTNET_EVM_MNEMONIC` and `SHONIN_TESTNET_SOLANA_MNEMONIC`, in the same order as the local ones, and the image timelock is zero.

```bash
task chains:accounts NETWORK=testnet          # every role's address and gas: fund these from the faucets
task chains:deploy NETWORK=testnet            # deploy to every EVM testnet
task contracts:solana:deploy NETWORK=testnet  # deploy the programs to Solana devnet
task chains:solana-setup NETWORK=testnet      # set them up: roles funded from the deployer, the dollar, markets, project
NETWORK=testnet task dev:services             # run everything here, against them
task chains:market NETWORK=testnet            # post the stand-in venues' prices and funding, Solana's too
task services:seed NETWORK=testnet VAULT_CHAIN=solana AGENT=funding-basis PUBLISH_ONLY=1 API_URL=<api>
```

On Solana the seed publishes `funding-basis/solana` (an agent's own Solana copy, where it has one) through the API and the cloud build service and reviewer, paying from the author's dollar account; then `STRATEGY=<id>` activates the vault, deposits, waits for the agent's work at the stand-in venue, and books a claim the keeper pays, reading each back from the cluster. `task services:demo NETWORK=testnet VAULT_CHAIN=solana` leaves a fresh vault running with auto-fill, as on an EVM testnet.

The testnets keep their own time, so nothing drives their clocks, and only the market operator (`task chains:market`, on this machine) moves their stand-in venues' prices. **Mainnet refuses**: these tasks deploy the stand-in stack (a mintable dollar, a stand-in venue, the development attestation verifier), which is for local chains and testnets only.

### Chains and networks

`config/chains.toml` lists every chain the platform knows, keyed `kind:id` (`evm:8453`, `solana:devnet`), with its label, RPC, websocket and the dollar a vault there settles in, and maps each network to the chain behind each vault chain name. Every service takes `--network`, `--chains` and `--deployments-dir`, and serves every vault chain of the network it speaks to at once, each with its own client and deployment; `--vault-chain` (or `VAULT_CHAIN=` on a task) narrows it to one. Everything that acts on one chain names it: each per-chain function takes the chain, and so does each API route that acts on one (`/api/{chain}/config`, `/api/{chain}/portfolio/{address}`, `/api/{chain}/uploads`, `/api/{chain}/health`), while `/api/snapshot` and the build queue span every chain and each strategy says which chain its vault is on. Where one id and version is published on two chains, its slug is qualified with the chain (`funding-basis-v1-hyperliquid`). The executor, the indexer, the API and the keeper serve the network's Solana cluster alongside the EVM chains, through the client in `packages/chain` (the `solana` module, behind the feature of that name, which `args` includes), and serve it alone with `--vault-chain solana`. A Solana program's id is compiled in, so a service serving a Solana cluster refuses to start on a build for another network: build it with `--features devnet` for testnet, `--features mainnet` for mainnet, and with neither for local.

- **To point a chain at another RPC**, set `SHONIN_RPC_<KIND>_<ID>` (or `SHONIN_WS_...`) in `.env`, for example `SHONIN_RPC_EVM_8453`. Paid RPC URLs carry keys, so they go there and never in the file. Log queries can go to endpoints of their own (`SHONIN_LOGS_RPC_<KIND>_<ID>`) and be read in windows of at most `SHONIN_LOG_RANGE_<KIND>_<ID>` blocks; `config/chains.toml`'s header says how.
- **To add a chain**, add a `[chains."kind:id"]` entry and name it in a network. A new local chain also needs its process in `taskfiles/chains.yml` and its port in `taskfiles/config.yml`.
- **Deployments:** mainnet addresses are committed under `config/deployments/mainnet/`, and a Solana program's mainnet id in `mainnet-id` beside its `Cargo.toml`. Testnet deployments never are (the programs' devnet ids aside, below): they stay under `.shonin/testnet/deployments/`, as a local run's stay under `.shonin/local/deployments/`. A program's local address is its keypair's, set when it builds; its devnet id is committed in `devnet-id` beside its `Cargo.toml`, the address of a key of the team's mnemonic that `task contracts:solana:deploy NETWORK=testnet` derives, and a build for devnet uses it with `--features devnet`.

### Where configuration lives

| What | Where |
| --- | --- |
| Paths, ports, URLs, the network, Google Cloud projects and region | `taskfiles/config.yml` |
| Every chain, its endpoints, and what each network means | `config/chains.toml` |
| The drivers and bridges a `shonin.toml` may name, and the networks each is offered on | `config/drivers.toml` |
| Cloudflare Worker names (`shonin-app`, `shonin-landing`, ...) | `name` in `cloudflare/<target>/wrangler.jsonc` |
| Secrets (testnet mnemonics included), RPC overrides and per-developer settings | `.env` (gitignored); the template is `.env.example` |
| Contract addresses | `config/deployments/mainnet/` (committed); `.shonin/<local or testnet>/deployments/` (never committed) |
| Rust toolchain; dependency versions | `rust-toolchain.toml`; `[workspace.dependencies]` in `Cargo.toml` |

## Layout

```text
config/           chains.toml, drivers.toml, and the committed mainnet deployments
contracts/
  evm/            the EVM contracts (Foundry); src/interfaces is what the bindings read
  solana/         the Solana programs (Anchor), and their in-process tests
packages/
  shared/         types shared by every crate (native and wasm), and the record's maths
  chain/          contract bindings, chain config and deployments (native and wasm)
  frontend/
    app/          the app: Rust -> wasm via Trunk
    marketing/    the marketing site: static pages generated by a native binary
    ui/           the design tokens both read, written out as tokens.css
  machines/       the attested machines, each its own image
    reviewer/     reads an agent's source and rates it in six areas before anything is built
    builder/      compiles submitted source into a signed bundle
    executor/     runs strategies and trades for their vaults
    unwinder/     finishes the withdrawals the contracts can't finish alone
  api/            the app's server: runs the indexer in-process and serves /api
  indexer/        the reference indexer: reads the chain into the record
  keeper/         the vaults' clockwork: valuations, settling, claims and the daily bill
  devnet/         deploys and drives the local chains, and serves the ops dashboard
  strategy-sdk/   Rust bindings for the shonin:agent WIT world (wit/agent.wit), on crates.io as shonin-strategy-sdk
deploy/gcp/       a Dockerfile per machine and service, and how they launch
cloudflare/       one wrangler.jsonc per Worker: app, landing, marketing, docs
docs/             the public documentation site (docs.shonin.fi)
taskfiles/        config.yml, and the tasks: chains, services, contracts, frontend, docs, cloud
```

## Documentation

The public documentation lives in `docs/` and is built with [Zensical](https://zensical.org). It describes how the platform works, as designed, and how to check it.

```bash
task docs:dev      # serve it with live reload on http://localhost:19089
task docs:build    # build it into docs/site
task docs:deploy   # build and deploy it to Cloudflare
```

## Deploying

**Cloudflare.** Each frontend is its own Worker serving static assets, with no Worker script. The deploy tasks need `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_DEPLOY_WORKERS_TOKEN` (an account API token) in `.env`; `.env.example` lists the token's permissions. A Worker, and its custom domain declared under `routes`, is created on its first deploy.

| Worker | Where | Task |
| --- | --- | --- |
| `shonin-app` | app.shonin.fi: the app | `task frontend:app:deploy` |
| `shonin-landing` | shonin.fi: the marketing site, static pages with no data, API or wallet code | `task frontend:landing:deploy` |
| `shonin-docs` | docs.shonin.fi | `task docs:deploy` |

`task deploy` runs the first two. The deployed app reads a chain and an API, so it shows live data only once both are deployed somewhere public.

**Google Cloud.** The machines run on Confidential Space VMs and the services on Cloud Run, from the images in `deploy/gcp/`: `task cloud:build`, `cloud:push`, `cloud:deploy:machine`, `cloud:deploy:service`, `cloud:status` and `cloud:logs`, each with `NETWORK=testnet` or `mainnet`. [deploy/gcp/README.md](deploy/gcp/README.md) has the details.

## Commit guard

`task setup` points git at `.githooks/`, so every commit is checked for secrets, for private terms and paths, and by a reviewer model against a publication policy. The term list and policy are local and never committed (`.private/`, gitignored); commits are blocked until both exist. See `CLAUDE.md` for the rules. In CI, `.github/workflows/secret-scan.yml` runs gitleaks over the full history on every push to `main` and every pull request.

## License

The platform is source-available under the [Business Source License 1.1](LICENSE). You may read, modify and build it, and make non-production use of it. Production use is permitted for authoring strategies for Shonin and for independently verifying the platform (rebuilding executor images, checking attestations, replaying decision logs); anything else needs a commercial license. Each version converts to the Apache License 2.0 four years after its first public release.

The strategy SDK (`packages/strategy-sdk`) is licensed under the [Apache License 2.0](packages/strategy-sdk/LICENSE), because its code is compiled into authors' own strategies.
