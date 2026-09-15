# Shonin Platform

The Shonin platform: one Cargo workspace (`packages/*`) driven by Task. See `README.md` for the layout and the quickstart.

## Rules

- **This repository is public.** Never mention other repositories, local paths or private projects in anything committed.
- **Config has one home.** Non-secret config lives only in `taskfiles/config.yml`. Cloudflare project names live only as `name` in `cloudflare/<target>/wrangler.jsonc`. Secrets live only in `.env`.
- **Secrets travel by name.** Tasks forward exactly the `.env` variables they need through `requires:` and `env:`. Never pass a secret with `--var`, and never set `CLOUDFLARE_INCLUDE_PROCESS_ENV`. The frontend never receives secrets.
- **Dependencies live in the workspace.** Declare every version in `[workspace.dependencies]` in the root `Cargo.toml`; crates use `{ workspace = true }`. Keep the wasm-bindgen family (`wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `web-sys`) exactly pinned and bumped together.
- **`shared` is dual-target.** It compiles for native and `wasm32-unknown-unknown` with no `#[cfg(target_arch)]` and no web-sys types.

## Commit guard

Every commit runs `.githooks/commit-msg` (enabled by `task setup`). It rejects a commit whose message or staged changes contain:

- a secret (gitleaks);
- any term from the local, gitignored `.private/terms.txt`, or an absolute home path;
- content that a reviewer model (`claude -p`) judges to break the local, gitignored policy in `.private/review.md`.

CI (`.github/workflows/secret-scan.yml`) re-runs the secret scan over the full history on every push to `main` and every pull request. The term and review checks run only locally, because their inputs never leave the machine.

Never bypass it: no `--no-verify` or `-n`, no changes to `core.hooksPath`, no `SHONIN_SKIP_REVIEW`. When it blocks a commit, fix the commit. If a check itself looks wrong, tell the user rather than working around it. Never commit anything under `.private/`.

## Done means

`task check`, `task lint` and `task test` all pass.
