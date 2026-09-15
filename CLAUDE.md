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

Never bypass it: no `--no-verify` or `-n`, no changes to `core.hooksPath`, no `SHONIN_SKIP_REVIEW`. When it blocks a commit, fix the commit. If a check itself looks wrong, tell the user rather than working around it. Never commit anything under `.private/`.

For assistants, a Claude Code hook (`.githooks/claude-bash-guard`, wired up in the local `.claude/settings.json`) enforces this. It denies those bypasses, and any write to `.githooks/`, `.private/` or `.claude/settings*`. It never prompts, so unattended runs don't stall. To change the guard files with an assistant's help, the user starts the session with `SHONIN_GUARD_UNLOCK=1`.

## Done means

`task check`, `task lint` and `task test` all pass.
