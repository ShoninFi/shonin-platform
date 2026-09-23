# Shonin Platform

Shonin is an open platform for verifiable trading strategies with pooled capital. Authors publish strategies as WASM components, depositors pool capital in vaults, and attested executors run the strategies, able to move funds only between a vault's own accounts.

This repository is the platform: the contracts, the web app, the services behind it, and the tooling around them. Everything off-chain is Rust, in one Cargo workspace; the contracts are Solidity, built with Foundry.

## Prerequisites

- [Rust](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml` and installs itself on first use, with the `wasm32-unknown-unknown` target for the web app and `wasm32-wasip2` for strategies.
- [Foundry](https://getfoundry.sh) for the contracts and the local chain (`forge`, `anvil`, `cast`).
- [Task](https://taskfile.dev)
- [Trunk](https://trunkrs.dev) for the frontend
- [Node.js](https://nodejs.org), only to install wrangler
- For the commit guard: [gitleaks](https://github.com/gitleaks/gitleaks), [jq](https://jqlang.org) and the [Claude Code](https://claude.com/claude-code) CLI. The reviewer uses the same CLI.

## Commit guard

`task setup` points git at `.githooks/`, so every commit is checked for secrets, for private terms and paths, and by a reviewer model against a publication policy. The term list and policy are local and never committed (`.private/`, gitignored); commits are blocked until both exist. See `CLAUDE.md` for the rules.

In CI, `.github/workflows/secret-scan.yml` runs gitleaks over the full history on every push to `main` and every pull request.

## Quickstart

```bash
task setup    # pinned Rust toolchain + wrangler
task dev      # the whole platform, locally: http://localhost:19080
task down     # stop everything `task dev` started
task check    # contracts, then every crate for each target it runs on
task lint     # rustfmt --check + clippy -D warnings + forge fmt --check
task test     # contract tests + native tests
task deploy   # release build + deploy to Cloudflare
```

Run `task` on its own to list every task.

## Documentation

The public documentation lives in `docs/` and is built with [Zensical](https://zensical.org). It describes how the platform works, how to check it, and what is left to build.

```bash
task docs:dev      # serve it with live reload on http://localhost:19089
task docs:build    # build it into docs/site
task docs:deploy   # build and deploy it to Cloudflare
```

The [build checklist](docs/content/checklist.md) is the working guideline for what is still to do.

## Running the platform locally

`task dev` starts a development chain, deploys the contracts to it, and runs every service against them, so the web app shows real data: real vaults, real deposits, a real build queue, a real record read from the chain.

| Port | What |
| --- | --- |
| 19080 | The web app, with live reload; `/api` is proxied to the API |
| 19081 | The indexer and the public HTTP API |
| 19083 | The chain (anvil) |
| 19084 | The build service |
| 19085 | The executor |
| 19086 | The reviewer |
| 19087 | The coordinator |
| 19088 | The devnet: the chain's clock and its market |

Everything the run writes lives under `.shonin/local` (gitignored): the contracts' addresses, the content-addressed store and the build directories. It is safe to delete; the next run rebuilds it.

The chain is anvil, with the ten unlocked accounts it hands out from a mnemonic everyone knows. Roles are those accounts by index, listed in `.env.example`, so no key is written down anywhere. The web app's **development wallet** is one of them: it signs nothing in the browser, because the chain already holds the key, which is what makes it a development chain. The browser-wallet path is the real one and works the same way against the same contracts.

A chain block is mined every second and advances an hour of chain time, so a vault reaches the ninety live days it needs to be ranked in about thirty-six minutes.

Two tasks are worth knowing separately:

```bash
task services:deploy   # deploy the contracts and write .shonin/local/deployments.json
task services:seed     # publish the reference strategies through the real flows
```

## Layout

```text
contracts/
  src/interfaces/ the contracts' interfaces, which the bindings also read
  src/            the contracts themselves
  test/           their tests
packages/
  shared/         types shared by every crate (native and wasm), and the record's maths
  chain/          contract bindings and deployment addresses (native and wasm)
  frontend/       the web app: Rust -> wasm via Trunk, served as Workers static assets
  backend/        the API: a Cloudflare Worker (workers-rs)
  indexer/        reads the chain into the record, and serves the public API
  executor/       the attested executor: feeds, runner and signer
  builder/        the build service: compiles submitted source into a signed bundle
  reviewer/       the attested reviewer: scores whether a strategy's code is reasonable
  coordinator/    the untrusted service that places strategies and keeps the clockwork turning
  devnet/         deploys and drives the local development chain
  strategy-sdk/   Rust bindings for the shonin:strategy WIT world
  cli/            the `shonin` CLI for strategy authors
docs/
  zensical.toml   the public documentation site (docs.shonin.fi)
  content/        its pages: how it works, how to verify it, and the build checklist
cloudflare/
  landing/        wrangler.jsonc for the shonin-landing Worker (the frontend)
  docs/           wrangler.jsonc for the shonin-docs Worker (the documentation)
taskfiles/
  config.yml      non-secret configuration: paths, ports, URLs, project names
  contracts.yml   building and testing the contracts
  services.yml    the local chain and every service that reads it
  frontend.yml    frontend dev, build and deploy
  docs.yml        documentation dev, build and deploy
```

## Where configuration lives

| What | Where |
| --- | --- |
| Paths, ports, URLs | `taskfiles/config.yml` |
| Cloudflare Worker names (`shonin-landing`, `shonin-docs`, ...) | `name` in `cloudflare/<target>/wrangler.jsonc` |
| Secrets and per-developer settings | `.env` (gitignored); the template is `.env.example` |
| Contract addresses, after a local deploy | `.shonin/local/deployments.json` (gitignored) |
| Rust toolchain | `rust-toolchain.toml` |
| Dependency versions | `[workspace.dependencies]` in the root `Cargo.toml` |

## Deploying

Each deploy target is its own Cloudflare Worker, named `shonin-<target>`. The frontend deploys as `shonin-landing`: static assets with no Worker script. `task deploy` needs `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_DEPLOY_WORKERS_TOKEN` (an account API token) in `.env`; `.env.example` lists the token's permissions. The first deploy creates the Worker. Its custom domain (`shonin.fi`) is declared under `routes` in the same `wrangler.jsonc`, and it has no `workers.dev` address, so the site has one canonical origin.

The deployed site is the web app alone. It reads a chain and an API, so a deployment that shows live data needs both, and there is no public chain to point it at yet.

## License

The platform is source-available under the [Business Source License 1.1](LICENSE). You may read, modify and build it, and make non-production use of it. Production use is permitted for authoring strategies for Shonin and for independently verifying the platform (rebuilding executor images, checking attestations, replaying decision logs); anything else needs a commercial license. Each version converts to the Apache License 2.0 four years after its first public release.

The strategy SDK (`packages/strategy-sdk`) is licensed under the [Apache License 2.0](packages/strategy-sdk/LICENSE), because its code is compiled into authors' own strategies.
