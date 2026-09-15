# Shonin Platform

Shonin is an open platform for verifiable trading strategies with pooled capital. Authors publish strategies as WASM components, depositors pool capital in vaults, and attested executors run the strategies, able to move funds only between a vault's own accounts.

This repository is the platform: the web app, its backend, the executor and the tooling around them. Everything is Rust, in one Cargo workspace.

## Prerequisites

- [Rust](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml` and installs itself on first use.
- [Task](https://taskfile.dev)
- [Trunk](https://trunkrs.dev) for the frontend
- [Node.js](https://nodejs.org), only to install wrangler
- For the commit guard: [gitleaks](https://github.com/gitleaks/gitleaks), [jq](https://jqlang.org) and the [Claude Code](https://claude.com/claude-code) CLI

## Commit guard

`task setup` points git at `.githooks/`, so every commit is checked for secrets, for private terms and paths, and by a reviewer model against a publication policy. The term list and policy are local and never committed (`.private/`, gitignored); commits are blocked until both exist. See `CLAUDE.md` for the rules.

## Quickstart

```bash
task setup    # pinned Rust toolchain + wrangler
task dev      # frontend with live reload on http://localhost:19080
task check    # cargo check, native and wasm
task lint     # rustfmt --check + clippy -D warnings
task test     # native tests
task deploy   # release build + deploy to Cloudflare
```

Run `task` on its own to list every task.

## Layout

```text
packages/
  shared/         types shared by every crate (native and wasm)
  frontend/       the web app: Rust -> wasm via Trunk, served as Workers static assets
  backend/        the API: a Cloudflare Worker (workers-rs)
  executor/       the attested executor: feeds, runner and signer
  coordinator/    the untrusted service that deploys and monitors executors
  simulator/      deterministic replay: backtests, stress tests, capacity estimates
  strategy-sdk/   Rust bindings for the shonin:strategy WIT world
  cli/            the `shonin` CLI for strategy authors
cloudflare/
  landing/        wrangler.jsonc for the shonin-landing Worker (the frontend)
taskfiles/
  config.yml      non-secret configuration: paths, ports, URLs, project names
  frontend.yml    frontend dev, build and deploy
```

## Where configuration lives

| What | Where |
| --- | --- |
| Paths, ports, URLs | `taskfiles/config.yml` |
| Cloudflare Worker names (`shonin-landing`, ...) | `name` in `cloudflare/<target>/wrangler.jsonc` |
| Secrets and per-developer settings | `.env` (gitignored); the template is `.env.example` |
| Rust toolchain | `rust-toolchain.toml` |
| Dependency versions | `[workspace.dependencies]` in the root `Cargo.toml` |

## Deploying

Each deploy target is its own Cloudflare Worker, named `shonin-<target>`. The frontend deploys as `shonin-landing`: static assets with no Worker script. `task deploy` needs `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_DEPLOY_WORKERS_TOKEN` (an account API token) in `.env`; `.env.example` lists the token's permissions. The first deploy creates the Worker. Its custom domain (`shonin.fi`) is declared under `routes` in the same `wrangler.jsonc`, and it has no `workers.dev` address, so the site has one canonical origin.

## License

The platform is source-available under the [Business Source License 1.1](LICENSE). You may read, modify and build it, and make non-production use of it. Production use is permitted for authoring strategies for Shonin and for independently verifying the platform (rebuilding executor images, checking attestations, replaying decision logs); anything else needs a commercial license. Each version converts to the Apache License 2.0 four years after its first public release.

The strategy SDK (`packages/strategy-sdk`) is licensed under the [Apache License 2.0](packages/strategy-sdk/LICENSE), because its code is compiled into authors' own strategies.
