#!/bin/bash
# One command for a testnet deploy: `task deploy:testnet SCOPE=services|contracts|all`.
#
#   services   the API's and the keeper's images, rebuilt in Cloud Build and
#              the services VM pointed at them; then the smoke check.
#   contracts  a contract generation on every EVM testnet (the chains in
#              parallel, Hyperliquid's creations batched), the checker and
#              the project proposed, every image rebuilt with the new
#              deployment baked in, the executor's and the builder's digests
#              proposed, the executor and the services moved onto them (the
#              prover started first, so the seal waits on no cold boot), the
#              firewall and the waker's grants asserted; then the smoke
#              check, and the builder moved onto its image once the
#              executor's root is live (run again after the root is made, if
#              it was not yet). The reviewer follows with `task
#              cloud:reviewer` once the root is live: its model key is sealed
#              to that root.
#   all        the same as contracts.
#
# Safe to run again: every stage looks at what is there before it acts (the
# deploy reads each chain before each step; an image already pushed under
# this tag is not built again; a VM already on the pushed digest is left as
# it is), so a deploy that stopped is carried on by running it again. Each
# stage says how long it took.
#
# Environment (the task passes these): SCOPE, GENERATION (contracts),
# PROJECT, ZONE, REGISTRY, DEPLOYMENTS_DIR, API_URL (public), EXECUTOR_URL
# (internal), STATUS_URLS (the builder's and the reviewer's status ports,
# internal, for the prover), BUILDER (cloud or local), ALLOW_NO_VAULT (1 for
# a smoke check before any vault is priced; a fresh generation sets it
# itself).
set -euo pipefail

: "${SCOPE:?SCOPE=services, contracts or all}"
: "${PROJECT:?}" "${ZONE:?}" "${REGISTRY:?}" "${DEPLOYMENTS_DIR:?}" "${API_URL:?}" "${EXECUTOR_URL:?}"
BUILDER=${BUILDER:-cloud}
STATUS_URLS=${STATUS_URLS:-}
case "$SCOPE" in services | contracts | all) ;; *) echo "SCOPE=services, contracts or all, not $SCOPE"; exit 1 ;; esac

started=$(date +%s)
timings=()
stage() {
  local name=$1
  shift
  local began
  began=$(date +%s)
  echo
  echo "=== $name"
  "$@"
  local took=$(($(date +%s) - began))
  timings+=("$(printf '%-34s %5d s' "$name" "$took")")
  echo "=== $name: done in $took s"
}
report() {
  echo
  echo "Stages:"
  if [ "${#timings[@]}" -gt 0 ]; then
    for line in "${timings[@]}"; do echo "  $line"; done
  fi
  echo "  $(printf '%-34s %5d s' total $(($(date +%s) - started)))"
}
trap report EXIT

gcloud_vm() { gcloud compute instances "$@" --project="$PROJECT" --zone="$ZONE"; }
digest_of() {
  gcloud artifacts docker images describe "$REGISTRY/$1:$TAG" --project="$PROJECT" \
    --format='value(image_summary.digest)' 2>/dev/null || true
}
metadata_of() {
  gcloud_vm describe "$1" --format=json 2>/dev/null |
    jq -r --arg key "$2" '.metadata.items[]? | select(.key == $key) | .value' || true
}

# The tag the images go under: the commit, the public deployment files
# baked into them, and any change not yet committed, so a tag never names
# two different images.
image_tag() {
  local commit deployments dirty
  commit=$(git rev-parse --short=12 HEAD)
  deployments=$(cat "$DEPLOYMENTS_DIR"/evm-[0-9]*.json "$DEPLOYMENTS_DIR"/attestation-evm-[0-9]*.json 2>/dev/null |
    shasum -a 256 | cut -c1-8)
  dirty=$( { git diff HEAD; git ls-files -o --exclude-standard | sort | xargs -I{} shasum -a 256 {} 2>/dev/null; } |
    shasum -a 256 | cut -c1-8)
  if git diff --quiet HEAD && [ -z "$(git ls-files -o --exclude-standard)" ]; then
    echo "$commit-$deployments"
  else
    echo "$commit-$deployments-$dirty"
  fi
}

contracts() {
  task chains:fund NETWORK=testnet
  task chains:deploy NETWORK=testnet ${GENERATION:+GENERATION=$GENERATION}
  task chains:checker NETWORK=testnet
  task chains:project NETWORK=testnet PROJECT="$PROJECT"
}

images() {
  task cloud:images NETWORK=testnet NAMES="$1" BUILDER="$BUILDER" TAG="$TAG" SKIP_PUSHED=1
}

propose() {
  local name digest
  for name in "$@"; do
    digest=$(digest_of "$name")
    [ -n "$digest" ] || { echo "no $name image under $TAG"; exit 1; }
    task chains:image NETWORK=testnet DIGEST="$digest" KIND="$name"
  done
}

# The prover's VM (created if the zone has none yet), pointed at the new
# prover image and started now, with a longer idle time for the next 45
# minutes, so it is up (its Groth16 image already on its disk) when the new
# executor asks for a seal; after that it powers off after the usual 600 s.
prover_warm() {
  local digest current until
  digest=$(digest_of prover)
  [ -n "$digest" ] || { echo "no prover image under $TAG"; exit 1; }
  if ! gcloud_vm describe shonin-prover-testnet >/dev/null 2>&1; then
    task cloud:deploy:prover NETWORK=testnet TAG="$TAG"
  fi
  # The startup script too: a prover made before a change to it would
  # otherwise keep running the old one.
  gcloud_vm add-metadata shonin-prover-testnet \
    --metadata-from-file="startup-script=$(dirname "$0")/prover/startup.sh"
  until=$(($(date +%s) + 45 * 60))
  current=$(metadata_of shonin-prover-testnet shonin-prover-image)
  # The status ports it proves for, beside the executor's (a prover made
  # before them has none).
  if [ "$current" != "$REGISTRY/prover@$digest" ]; then
    gcloud_vm add-metadata shonin-prover-testnet \
      --metadata="^~^shonin-prover-image=$REGISTRY/prover@$digest~shonin-status-urls=$STATUS_URLS~shonin-prover-idle-seconds=1800~shonin-prover-idle-until=$until"
  else
    gcloud_vm add-metadata shonin-prover-testnet \
      --metadata="^~^shonin-status-urls=$STATUS_URLS~shonin-prover-idle-seconds=1800~shonin-prover-idle-until=$until"
  fi
  case "$(gcloud_vm describe shonin-prover-testnet --format='value(status)')" in
    TERMINATED | STOPPED) gcloud_vm start shonin-prover-testnet --async ;;
    *) echo "the prover's VM is already up" ;;
  esac
}

# The executor's VM on the pushed digest: left as it is if it already runs
# it; otherwise replaced (Confidential Space takes its image at creation).
executor() {
  local digest current
  digest=$(digest_of executor)
  current=$(metadata_of shonin-executor-testnet tee-image-reference)
  if [ "$current" = "$REGISTRY/executor@$digest" ]; then
    echo "the executor already runs executor@$digest"
    return
  fi
  if [ -n "$current" ]; then
    echo "replacing the executor (it runs ${current##*@})"
    gcloud_vm delete shonin-executor-testnet --quiet
  fi
  task cloud:deploy:machine NAME=executor NETWORK=testnet TAG="$TAG"
}

# The firewall's rules and the waker's grants on the machines that run on
# demand: asserted, each idempotent.
grants() {
  task cloud:firewall NETWORK=testnet
  task cloud:iam NETWORK=testnet
}

# The builder's VM on the pushed digest, once the executor's root is live
# (its key is derived from the root): left as it is if it already runs it;
# otherwise drained and replaced (Confidential Space takes its image at
# creation). Its first boot gets its key blessed on every chain, then it
# exits when idle and the waker starts it when there is work.
builder() {
  local digest current
  if ! curl -fsS -m 10 "$API_URL/api/rootkey" | jq -e '.serving == true' >/dev/null 2>&1; then
    echo "the root key is not live yet: once it is (task chains:root-key), run this deploy again, or task cloud:deploy:machine NAME=builder NETWORK=testnet TAG=$TAG"
    return
  fi
  digest=$(digest_of builder)
  [ -n "$digest" ] || { echo "no builder image under $TAG"; exit 1; }
  current=$(metadata_of shonin-builder-testnet tee-image-reference)
  if [ "$current" = "$REGISTRY/builder@$digest" ]; then
    echo "the builder already runs builder@$digest"
    return
  fi
  if [ -n "$current" ]; then
    echo "replacing the builder (it runs ${current##*@}) once it holds nothing unfinished"
    task cloud:drain NAME=builder NETWORK=testnet
    gcloud_vm delete shonin-builder-testnet --quiet
  fi
  task cloud:deploy:machine NAME=builder NETWORK=testnet TAG="$TAG"
}

services() {
  local api keeper
  if ! gcloud_vm describe shonin-services-testnet >/dev/null 2>&1; then
    task cloud:deploy:services NETWORK=testnet TAG="$TAG"
    return
  fi
  api=$(digest_of api)
  keeper=$(digest_of keeper)
  if [ "$(metadata_of shonin-services-testnet shonin-api-image)" = "$REGISTRY/api@$api" ] &&
    [ "$(metadata_of shonin-services-testnet shonin-keeper-image)" = "$REGISTRY/keeper@$keeper" ]; then
    echo "the services VM already runs api@$api and keeper@$keeper"
    return
  fi
  task cloud:redeploy:services NETWORK=testnet TAG="$TAG"
}

# The public API answering again after the services VM's restart.
api_up() {
  for _ in $(seq 1 60); do
    if curl -fsS -m 10 "$API_URL/health" >/dev/null 2>&1; then
      echo "the API answers"
      return
    fi
    sleep 5
  done
  echo "the API did not answer within 5 minutes"
  exit 1
}

smoke() {
  local allow=""
  if [ -n "${ALLOW_NO_VAULT:-}" ]; then allow=1; fi
  # A freshly started API reads each chain for the first time within a
  # minute or two, and up to about ten when a chain's vaults' histories have
  # to be sampled through its one endpoint that answers past blocks: give it
  # fifteen before calling the check failed.
  local tries=0
  until task chains:smoke NETWORK=testnet API="$API_URL" ALLOW_NO_VAULT="$allow"; do
    tries=$((tries + 1))
    if [ "$tries" -ge 30 ]; then
      echo "the smoke check still fails after fifteen minutes"
      exit 1
    fi
    echo "trying the smoke check again in 30 s (the API may not have read every chain yet)"
    sleep 30
  done
  # The keeper and the executor, from inside the VPC: neither is public.
  gcloud compute ssh shonin-services-testnet --project="$PROJECT" --zone="$ZONE" --tunnel-through-iap \
    --command "set -e
      printf 'keeper:   '; curl -fsS -m 10 http://localhost:19087/health; echo
      printf 'executor: '; curl -fsS -m 10 $EXECUTOR_URL/health; echo"
}

TAG=$(image_tag)
echo "deploying SCOPE=$SCOPE to the testnets; images under $TAG"
case "$SCOPE" in
  services)
    stage "images: api, keeper" images "api keeper"
    stage "services VM" services
    stage "API up" api_up
    stage "smoke check" smoke
    ;;
  contracts | all)
    stage "contracts (every chain at once)" contracts
    # The deployment changed: so do the images that carry it.
    TAG=$(image_tag)
    ALLOW_NO_VAULT=${ALLOW_NO_VAULT:-1}
    stage "images: api, keeper, executor, prover, builder" images "api keeper executor prover builder"
    stage "executor and builder digests proposed" propose executor builder
    stage "prover started, warm" prover_warm
    stage "executor VM" executor
    stage "firewall and grants" grants
    stage "services VM" services
    stage "API up" api_up
    stage "smoke check" smoke
    stage "builder VM" builder
    echo
    echo "Next: the root key (task chains:root-key NETWORK=testnet) if it is not live, then this deploy again for the builder, and task cloud:reviewer NETWORK=testnet for the reviewer. To publish on the new generation: the seed (PUBLISH_ONLY=1) per chain, through the API."
    echo "Or, with the build service on this machine: task chains:allow-dev-checker NETWORK=testnet, the seed (PUBLISH_ONLY=1) per chain, task store:sync NETWORK=testnet, task chains:retire-dev-checker NETWORK=testnet"
    ;;
esac
