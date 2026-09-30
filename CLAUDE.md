# Shonin Platform

The Shonin platform: EVM contracts (`contracts/evm`, Foundry) and Solana programs (`contracts/solana`, Anchor), one Cargo workspace (`packages/*`: the frontends under `frontend/`, the attested machines under `machines/`, and the api, indexer, keeper and devnet), the chain config (`config/`), the images (`deploy/gcp`) and the public documentation site (`docs/`), driven by Task. See `README.md` for the layout, the quickstart and the local development conventions.

The vocabulary follows the docs. The **machines** are the four attested images: reviewer, builder, executor, unwinder. The **services** are everything untrusted: the api (the app's server), the indexer (the reference record), the keeper (the daily clockwork) and, locally, the devnet.

## Rules

- **This repository is public.** Never mention other repositories, local paths or private projects in anything committed.
- **Config has one home.** Non-secret config lives only in `taskfiles/config.yml`, and the chains only in `config/chains.toml`. Google Cloud project ids live only as `GCP_PROJECT_TESTNET` / `GCP_PROJECT_MAINNET` in `config.yml`. Cloudflare project names live only as `name` in `cloudflare/<target>/wrangler.jsonc`. Secrets, keyed RPC URLs included, live only in `.env`.
- **Two choices, always explicit.** Where the services run (locally, or on Google Cloud) and which chains they talk to (`NETWORK=local|testnet|mainnet`) are chosen separately. Never rely on the gcloud default project or account: every `gcloud` call passes `--project` (and `--region`/`--zone`) from `config.yml`, and a cloud task refuses to run without them. The cloud never runs against `local`.
- **Secrets travel by name.** Tasks forward exactly the `.env` variables they need through `requires:` and `env:`. Never pass a secret with `--var`, and never set `CLOUDFLARE_INCLUDE_PROCESS_ENV`. The frontend never receives secrets.
- **Dependencies live in the workspace.** Declare every version in `[workspace.dependencies]` in the root `Cargo.toml`; crates use `{ workspace = true }`. Keep the wasm-bindgen family (`wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `web-sys`) exactly pinned and bumped together.
- **`shared` and `chain` are dual-target.** They compile for native and `wasm32-unknown-unknown` with no `#[cfg(target_arch)]` and no web-sys types, so the web app and the services read the same types and the same contract bindings.
- **The contracts' interfaces are the source of truth for both sides.** `contracts/evm/src/interfaces/*.sol` is what the contracts implement and what `packages/chain` parses with `sol!`. Change the interface once and both follow; never hand-write a second copy of a signature.
- **No key is written down.** The local chains' accounts come from their own published mnemonic, taken by index for each role.
- **Nothing local is committed.** A local run writes only under `.shonin/local`, which is gitignored and safe to delete. Only public-chain deployments are committed, under `config/deployments/<network>/`.
- **`docs/` is published.** It describes the platform as designed: mechanisms a reader can check. It never carries strategy, reasoning about pricing, comparisons with anyone, or plans beyond the checklist. `docs/content/checklist.md` is the only record of what is built; pages do not carry "not built yet" notes, and the site's footer says so once. Keep the checklist current.

## Commit guard

Every commit runs `.githooks/commit-msg` (enabled by `task setup`). It rejects a commit whose message or staged changes contain:

- a secret (gitleaks);
- any term from the local, gitignored `.private/terms.txt`, or an absolute home path;
- content that a reviewer model (`claude -p`) judges to break the local, gitignored policy in `.private/review.md`.

CI (`.github/workflows/secret-scan.yml`) re-runs the secret scan over the full history on every push to `main` and every pull request. The term and review checks run only locally, because their inputs never leave the machine.

Never bypass it: no `--no-verify` or `-n`, no changes to `core.hooksPath`, no `SHONIN_SKIP_REVIEW`. When it blocks a commit, fix the commit. If a check itself looks wrong, tell the user rather than working around it. Never commit anything under `.private/`.

For assistants, a Claude Code hook (`.githooks/claude-bash-guard`, wired up in the local `.claude/settings.json`) enforces this. It denies those bypasses, and any write to `.githooks/`, `.private/` or `.claude/settings*`. It never prompts, so unattended runs don't stall. To change the guard files with an assistant's help, the user starts the session with `SHONIN_GUARD_UNLOCK=1`.

## Done means

`task check`, `task lint` and `task test` all pass, and `task docs:build` reports no issues when the documentation changed.
