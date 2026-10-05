#!/usr/bin/env bash
# Decides which CI jobs a change needs, and writes `evm`, `solana` and `rust`
# (each true or false) to $GITHUB_OUTPUT, or to stdout outside Actions.
#
# On a pull request the checkout is GitHub's merge commit, whose first parent
# is the base branch, so `HEAD^1..HEAD` is exactly what the pull request
# changes. Anything else (a push to main, a schedule, a manual run) and any
# doubt runs every job.
#
# What depends on what:
# - the EVM contracts depend only on themselves and their library submodules;
# - the Solana programs are their own Cargo workspace on the pinned toolchain;
# - the root workspace's crates compile the Solana programs' crates as path
#   dependencies (packages/chain), generate their EVM bindings from
#   contracts/evm/src/interfaces (packages/chain/build.rs), and include files
#   from config/.
set -euo pipefail

evm=false solana=false rust=false
all() { evm=true solana=true rust=true; }

if [ "${GITHUB_EVENT_NAME:-}" != "pull_request" ]; then
  all
elif ! git rev-parse --verify --quiet HEAD^2 >/dev/null; then
  echo "the checkout is not a pull request's merge commit: running everything"
  all
else
  while IFS= read -r path; do
    case "$path" in
      .github/workflows/ci.yml | .github/scripts/* | Taskfile.yml | taskfiles/*)
        all ;;
    esac
    case "$path" in
      contracts/evm/* | .gitmodules)
        evm=true ;;
    esac
    case "$path" in
      contracts/solana/* | rust-toolchain.toml)
        solana=true ;;
    esac
    case "$path" in
      packages/* | Cargo.toml | Cargo.lock | rust-toolchain.toml | config/* \
        | contracts/solana/* | contracts/evm/src/interfaces/* \
        | .cargo/* | clippy.toml | rustfmt.toml | .rustfmt.toml)
        rust=true ;;
    esac
  done < <(git diff --name-only --no-renames HEAD^1 HEAD)
fi

{
  echo "evm=$evm"
  echo "solana=$solana"
  echo "rust=$rust"
} | tee -a "${GITHUB_OUTPUT:-/dev/null}"
