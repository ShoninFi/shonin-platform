#!/bin/bash
# The prover's VM: at every boot, run the prover's container until it has had
# nothing to prove for its idle time, then power the VM off. Whatever goes
# wrong on the way (metadata missing, the image not pulled), the VM powers
# off: it costs nothing while off, and the waker starts it again when a seal
# is wanted.
#
# Container-Optimized OS runs this as root (the instance's `startup-script`).
# The instance's metadata names the prover's image (by digest) and the
# executor's status port:
#   shonin-prover-image   us-central1-docker.pkg.dev/<project>/shonin/prover@sha256:<digest>
#   shonin-executor-url   http://<executor's internal name>:19085
#
# The Groth16 step runs in RISC Zero's prover image through this VM's Docker:
# the prover's container gets the Docker socket and a work directory at the
# same path on both sides.

set -uo pipefail
trap 'echo "shonin-prover: powering off"; poweroff' EXIT

metadata() {
  curl -fsS -H 'Metadata-Flavor: Google' \
    "http://metadata.google.internal/computeMetadata/v1/instance/$1"
}

image=$(metadata attributes/shonin-prover-image) || { echo "shonin-prover: no shonin-prover-image in the metadata"; exit 1; }
executor=$(metadata attributes/shonin-executor-url) || { echo "shonin-prover: no shonin-executor-url in the metadata"; exit 1; }
work=/var/lib/shonin/groth16
export DOCKER_CONFIG=/var/lib/shonin/docker
mkdir -p "$work" "$DOCKER_CONFIG"

# Pull from Artifact Registry as the instance's account: its token, on stdin.
# The network can still be settling just after boot, so each step is tried
# for a few minutes before the VM gives up and powers off.
registry=${image%%/*}
signed_in=
for try in 1 2 3 4 5 6 7 8 9 10; do
  if metadata service-accounts/default/token |
    sed -n 's/.*"access_token" *: *"\([^"]*\)".*/\1/p' |
    docker login -u oauth2accesstoken --password-stdin "https://$registry" >/dev/null; then
    signed_in=1
    break
  fi
  sleep 15
done
[ -n "$signed_in" ] || { echo "shonin-prover: could not sign in to $registry"; exit 1; }
pulled=
for try in 1 2 3 4 5; do
  if docker pull "$image"; then pulled=1; break; fi
  sleep 15
done
[ -n "$pulled" ] || { echo "shonin-prover: could not pull the prover's image"; exit 1; }
# RISC Zero's Groth16 prover image, pulled while the first proof's STARK
# stages run.
docker pull risczero/risc0-groth16-prover:v2025-04-03.1 >/dev/null 2>&1 &

started=$(date +%s)
docker run --rm --name shonin-prover --log-driver gcplogs \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v "$work:$work" -e RISC0_WORK_DIR="$work" \
  "$image" --executor-url "$executor" --idle-exit-seconds 600
status=$?
echo "shonin-prover: the prover exited ($status) after $(( $(date +%s) - started )) s"
