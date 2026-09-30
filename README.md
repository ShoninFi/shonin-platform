# Shonin Platform

Shonin is an open platform for verifiable trading strategies with pooled capital. Authors publish strategies as WASM components, depositors pool capital in vaults, and attested executors run the strategies, able to move funds only between a vault's own accounts.

This repository is the platform: the contracts, the frontends, the attested machines and the services around them, and the tooling that runs them. Everything off-chain is Rust, in one Cargo workspace; the EVM contracts are Solidity, built with Foundry, and the Solana programs are built with Anchor.

## Prerequisites

- [Rust](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml` and installs itself on first use, with the `wasm32-unknown-unknown` target for the app and `wasm32-wasip2` for strategies.
- [Task](https://taskfile.dev) and [Trunk](https://trunkrs.dev).
- [Foundry](https://getfoundry.sh) for the EVM contracts and the local EVM chains (`forge`, `anvil`).
- The [Solana CLI](https://solana.com/docs/intro/installation) for the local validator (`solana-test-validator`), and [Anchor](https://www.anchor-lang.com) to build the Solana programs.
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

Three anvils standing in for the EVM vault chains (base, hyperliquid, robinhood) and a Solana validator; the devnet, which deploys the contracts to every EVM chain, then moves their clocks and plays their markets; the four machines; the API, the indexer and the keeper; the app; and the marketing site. Every machine and service serves all three EVM chains at once, and retries until each chain answers and its deployment exists, so they all start at once. `task services:seed` publishes to base, or to `VAULT_CHAIN=hyperliquid` and so on. `task dev:services` is the same without the frontends.

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
| 19110–19112 | The Solana validator: RPC, websocket, faucet |

The base port is `SHONIN_START_PORT_DEV` (19080); every other port is a fixed offset from it, listed in `taskfiles/config.yml`.

**The ops dashboard** at `http://localhost:19088/` shows every chain of the network (key, RPC, the chain id or genesis it reports, its head and its clock), every machine and service and whether its `/health` answers, each chain's contract addresses and roles, and, once a Google Cloud project is set, what is running there. It refreshes itself, and it is local only.

**Everything a run writes** lives under `.shonin/<network>` (gitignored; `.shonin/local` for `task dev`): one deployment file per chain in `deployments/`, the content-addressed store, the build directories and the Solana ledger. It is safe to delete; the next run rebuilds it.

**The local chains' accounts** come from anvil's published mnemonic. Each role (admin, builder, reviewer, executor, keeper, author, ...) is one of them by index, listed in `.env.example`, so no key is written down anywhere. The app's **development wallet** is one of them: it signs nothing in the browser, because the chain already holds the key. A block is mined every second and advances an hour of chain time, so a vault reaches the ninety live days it needs to be ranked in about thirty-six minutes.

Worth knowing separately:

```bash
task chains:deploy                   # deploy the contracts to every local EVM chain
task services:seed                   # publish the reference agents through the real flows
task services:seed AGENT=funding-basis   # publish one agent, as it stands
task contracts:solana:build          # anchor build
task contracts:solana:deploy         # deploy the programs to the local validator
```

### The testnets

The same stand-in stack deploys to the public testnets (Base Sepolia, Hyperliquid testnet, Robinhood Chain testnet, Solana devnet). There the roles are accounts derived from mnemonics you hold in `.env`, `SHONIN_TESTNET_EVM_MNEMONIC` and `SHONIN_TESTNET_SOLANA_MNEMONIC`, in the same order as the local ones, and the image timelock is zero.

```bash
task chains:accounts NETWORK=testnet          # every role's address and gas: fund these from the faucets
task chains:deploy NETWORK=testnet            # deploy to every EVM testnet
task contracts:solana:deploy NETWORK=testnet  # deploy the programs to Solana devnet
NETWORK=testnet task dev:services             # run everything here, against them
```

The testnets keep their own time, so nothing drives their clocks or their stand-in venues' prices. **Mainnet refuses**: these tasks deploy the stand-in stack (a mintable dollar, a stand-in venue, the development attestation verifier), which is for local chains and testnets only.

### Chains and networks

`config/chains.toml` lists every chain the platform knows, keyed `kind:id` (`evm:8453`, `solana:devnet`), with its label, RPC, websocket and the dollar a vault there settles in, and maps each network to the chain behind each vault chain name. Every service takes `--network`, `--chains` and `--deployments-dir`, and serves every EVM vault chain of the network at once, each with its own client and deployment; `--vault-chain` (or `VAULT_CHAIN=` on a task) narrows it to one. The snapshot and the app's config carry every chain, and each strategy says which chain its vault is on. Where one id and version is published on two chains, its slug is qualified with the chain (`funding-basis-v1-hyperliquid`). No service speaks to Solana yet.

- **To point a chain at another RPC**, set `SHONIN_RPC_<KIND>_<ID>` (or `SHONIN_WS_...`) in `.env`, for example `SHONIN_RPC_EVM_8453`. Paid RPC URLs carry keys, so they go there and never in the file.
- **To add a chain**, add a `[chains."kind:id"]` entry and name it in a network. A new local chain also needs its process in `taskfiles/chains.yml` and its port in `taskfiles/config.yml`.
- **Deployments:** mainnet addresses are committed under `config/deployments/mainnet/`, and a Solana program's mainnet id in `mainnet-id` beside its `Cargo.toml`. Testnet addresses never are: they stay under `.shonin/testnet/deployments/`, as a local run's stay under `.shonin/local/deployments/`. A program's local or devnet address is its keypair's, set when it builds.

### Where configuration lives

| What | Where |
| --- | --- |
| Paths, ports, URLs, the network, Google Cloud projects and region | `taskfiles/config.yml` |
| Every chain, its endpoints, and what each network means | `config/chains.toml` |
| Cloudflare Worker names (`shonin-app`, `shonin-landing`, ...) | `name` in `cloudflare/<target>/wrangler.jsonc` |
| Secrets (testnet mnemonics included), RPC overrides and per-developer settings | `.env` (gitignored); the template is `.env.example` |
| Contract addresses | `config/deployments/mainnet/` (committed); `.shonin/<local or testnet>/deployments/` (never committed) |
| Rust toolchain; dependency versions | `rust-toolchain.toml`; `[workspace.dependencies]` in `Cargo.toml` |

## Layout

```text
config/           chains.toml, and the committed mainnet deployments
contracts/
  evm/            the EVM contracts (Foundry); src/interfaces is what the bindings read
  solana/         the Solana programs (Anchor): a skeleton for now
packages/
  shared/         types shared by every crate (native and wasm), and the record's maths
  chain/          contract bindings, chain config and deployments (native and wasm)
  frontend/
    app/          the app: Rust -> wasm via Trunk
    marketing/    the marketing site: static pages generated by a native binary
    ui/           the design tokens both read, written out as tokens.css
  machines/       the attested machines, each its own image
    reviewer/     reads a strategy's source and scores it before anything is built
    builder/      compiles submitted source into a signed bundle
    executor/     runs strategies and trades for their vaults
    unwinder/     finishes the withdrawals the contracts can't finish alone
  api/            the app's server: runs the indexer in-process and serves /api
  indexer/        the reference indexer: reads the chain into the record
  keeper/         crystallises fees, settles claims and freezes, so nobody has to
  devnet/         deploys and drives the local chains, and serves the ops dashboard
  strategy-sdk/   Rust bindings for the shonin:strategy WIT world
deploy/gcp/       a Dockerfile per machine and service, and how they launch
cloudflare/       one wrangler.jsonc per Worker: app, landing, marketing, docs
docs/             the public documentation site (docs.shonin.fi)
taskfiles/        config.yml, and the tasks: chains, services, contracts, frontend, docs, cloud
```

## Documentation

The public documentation lives in `docs/` and is built with [Zensical](https://zensical.org). It describes how the platform works and how to check it. The [build checklist](docs/content/checklist.md) is the working guideline for what is still to do.

```bash
task docs:dev      # serve it with live reload on http://localhost:19089
task docs:build    # build it into docs/site
task docs:deploy   # build and deploy it to Cloudflare
```

## Deploying

**Cloudflare.** Each frontend is its own Worker serving static assets, with no Worker script. The deploy tasks need `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_DEPLOY_WORKERS_TOKEN` (an account API token) in `.env`; `.env.example` lists the token's permissions. A Worker, and its custom domain declared under `routes`, is created on its first deploy.

| Worker | Where | Task |
| --- | --- | --- |
| `shonin-app` | app.shonin.fi | `task frontend:app:deploy` |
| `shonin-landing` | shonin.fi: the app, until the marketing site takes it | `task frontend:landing:deploy` |
| `shonin-marketing` | its workers.dev address only, for now | `task frontend:marketing:deploy` |
| `shonin-docs` | docs.shonin.fi | `task docs:deploy` |

`task deploy` runs the first two. The deployed app reads a chain and an API, so it shows live data only once both are deployed somewhere public.

**Google Cloud.** The machines run on Confidential Space VMs and the services on Cloud Run, from the images in `deploy/gcp/`: `task cloud:build`, `cloud:push`, `cloud:deploy:machine`, `cloud:deploy:service`, `cloud:status` and `cloud:logs`, each with `NETWORK=testnet` or `mainnet`. [deploy/gcp/README.md](deploy/gcp/README.md) has the details.

## Commit guard

`task setup` points git at `.githooks/`, so every commit is checked for secrets, for private terms and paths, and by a reviewer model against a publication policy. The term list and policy are local and never committed (`.private/`, gitignored); commits are blocked until both exist. See `CLAUDE.md` for the rules. In CI, `.github/workflows/secret-scan.yml` runs gitleaks over the full history on every push to `main` and every pull request.

## License

The platform is source-available under the [Business Source License 1.1](LICENSE). You may read, modify and build it, and make non-production use of it. Production use is permitted for authoring strategies for Shonin and for independently verifying the platform (rebuilding executor images, checking attestations, replaying decision logs); anything else needs a commercial license. Each version converts to the Apache License 2.0 four years after its first public release.

The strategy SDK (`packages/strategy-sdk`) is licensed under the [Apache License 2.0](packages/strategy-sdk/LICENSE), because its code is compiled into authors' own strategies.
