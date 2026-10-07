#!/bin/bash
# One image, in a Cloud Build step (deploy/gcp/cloudbuild.yaml): built with
# the shared BuildKit builder for linux/amd64 and pushed as NAME:TAG. An
# image not named in NAMES is passed over. The settings come from the
# build's substitutions, as `task cloud:build` passes them to a local build.
#
# The compiled dependencies are an image of their own (the Dockerfile's
# `deps` stage), named for everything they are made from: the Dockerfile,
# the toolchain, Cargo.lock, every Cargo.toml, the Solana crates the chain
# crate names by path, and the network's feature. Pushed once, it is where
# every later build with the same inputs starts, so a change to the
# platform's own code compiles only the platform's crates.
set -euo pipefail
name=$1
case " $NAMES " in
  *" $name "*) ;;
  *) echo "$name: not asked for"; exit 0 ;;
esac

# The staged source carries the public deployment files and, for the
# prover, the guest as directories of their own.
contexts=(--build-context "deployments=_deployments")
if [ "$name" = prover ]; then
  [ -s _guest/shonin-attestation-guest.bin ] || { echo "no guest ELF staged in _guest/"; exit 1; }
  contexts+=(--build-context "guest=_guest")
fi
args=(
  --builder shonin --platform linux/amd64 --provenance=false --sbom=false
  --build-arg "NETWORK_FEATURE=$NETWORK_FEATURE"
  --build-arg "NETWORK=$NETWORK"
  --build-arg "WITHOUT_SOLANA=$WITHOUT_SOLANA"
  --build-arg "STORE_URL=$STORE_URL"
  --build-arg "API_URL=$API_URL"
  --build-arg "EXECUTOR_URL=$EXECUTOR_URL"
  "${contexts[@]}"
  -f "deploy/gcp/Dockerfile.$name"
)

started=$(date +%s)
key=$(
  {
    echo "$NETWORK_FEATURE"
    cat "deploy/gcp/Dockerfile.$name" rust-toolchain.toml Cargo.lock
    find . -name Cargo.toml -not -path '*/target/*' -not -path './_*' | LC_ALL=C sort | xargs sha256sum
    find contracts/solana -type f -not -path '*/target/*' | LC_ALL=C sort | xargs sha256sum
  } | sha256sum | cut -c1-24
)
deps="$REGISTRY/deps-$name:$key"
if docker buildx imagetools inspect "$deps" >/dev/null 2>&1; then
  echo "$name: the dependencies are built already ($deps)"
else
  echo "$name: building the dependencies ($deps)"
  docker buildx build "${args[@]}" --target deps \
    --output "type=image,name=$deps,push=true,oci-mediatypes=false" .
  echo "$name: the dependencies built and pushed in $(( $(date +%s) - started )) s"
fi

docker buildx build "${args[@]}" --build-arg "DEPS=$deps" \
  --output "type=image,name=$REGISTRY/$name:$TAG,push=true,oci-mediatypes=false" .
echo "$name:$TAG built and pushed in $(( $(date +%s) - started )) s"
