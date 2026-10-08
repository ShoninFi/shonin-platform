#!/bin/bash
# The services VM's startup script (Container-Optimized OS), run as root on
# every boot. It starts, on the host's network:
#
#   shonin-api      the API (the indexer inside), on :19081, open to the VPC,
#                   and its internal port :19082, for the builder and the
#                   reviewer alone (the cloud's firewall), never tunneled
#   shonin-keeper   the keeper, health on :19087, with the keeper's keys alone
#                   (its EVM key, and its Solana devnet key)
#   shonin-tunnel   cloudflared, carrying the API out to its public name
#   shonin-waker    a small loop that starts the prover's VM when a machine
#                   wants a seal, and the builder's and the reviewer's when
#                   the API says they are wanted
#
# What it runs is named in the instance's metadata (`task cloud:deploy:services`
# sets it): shonin-api-image and shonin-keeper-image (by digest),
# shonin-executor-url, shonin-prover-instance, shonin-builder-url,
# shonin-builder-instance, shonin-reviewer-url, shonin-reviewer-instance,
# shonin-keeper-secret, shonin-keeper-solana-secret, shonin-tunnel-secret,
# and the waker's script as shonin-waker.
#
# The secrets are read from Secret Manager with the VM's own token and
# written to files only root can read; they are never echoed, never put on a
# command line, and never logged. The keeper's Solana key is the one a VM
# made before it existed may lack: without it the keeper keeps the EVM chains
# and its health says Solana devnet is not kept.
set -euo pipefail
umask 077

METADATA=http://metadata.google.internal/computeMetadata/v1
STATE=/var/lib/shonin

say() { echo "shonin-services: $*"; }

metadata() {
  curl -sf --retry 5 --retry-delay 2 -H 'Metadata-Flavor: Google' "$METADATA/$1"
}

# Container-Optimized OS drops every inbound connection but SSH. The API and
# the keeper's health answer the VPC (the executor posts its reports to the
# API), and the API's internal port the builder and the reviewer; the cloud
# firewall keeps the rest of the world out, and the internal port from all
# but those two machines.
for port in 19081 19082 19087; do
  iptables -C INPUT -p tcp -s 10.128.0.0/9 --dport "$port" -j ACCEPT 2>/dev/null ||
    iptables -I INPUT -p tcp -s 10.128.0.0/9 --dport "$port" -j ACCEPT
done

attribute() { metadata "instance/attributes/$1"; }

# The VM's service account's access token. Printed to a pipe only.
token() {
  metadata instance/service-accounts/default/token |
    sed -n 's/.*"access_token" *: *"\([^"]*\)".*/\1/p'
}

# A secret's latest version, decoded, on stdout (captured, never shown). The
# token goes to curl as a header read from stdin, not on its command line.
secret() {
  local project=$1 name=$2
  printf 'Authorization: Bearer %s\n' "$(token)" |
    curl -sf --retry 5 --retry-delay 2 -H @- \
      "https://secretmanager.googleapis.com/v1/projects/$project/secrets/$name/versions/latest:access" |
    sed -n 's/.*"data" *: *"\([^"]*\)".*/\1/p' |
    base64 -d
}

project=$(metadata project/project-id)
api_image=$(attribute shonin-api-image)
keeper_image=$(attribute shonin-keeper-image)
executor_url=$(attribute shonin-executor-url)
prover_instance=$(attribute shonin-prover-instance)
# The machines that run on demand: absent from an older deploy's metadata.
builder_url=$(attribute shonin-builder-url 2>/dev/null || true)
builder_instance=$(attribute shonin-builder-instance 2>/dev/null || true)
reviewer_url=$(attribute shonin-reviewer-url 2>/dev/null || true)
reviewer_instance=$(attribute shonin-reviewer-instance 2>/dev/null || true)
keeper_secret=$(attribute shonin-keeper-secret)
keeper_solana_secret=$(attribute shonin-keeper-solana-secret 2>/dev/null || true)
tunnel_secret=$(attribute shonin-tunnel-secret)
cloudflared_image=$(attribute shonin-cloudflared-image 2>/dev/null || echo cloudflare/cloudflared:latest)
waker_image=$(attribute shonin-waker-image 2>/dev/null || echo curlimages/curl:latest)

mkdir -p "$STATE/secrets" "$STATE/store" "$STATE/api-state" "$STATE/docker"
chmod 700 "$STATE/secrets" "$STATE/docker"
# The API's store, kept across restarts, for the images' own user (nonroot,
# 65532 in the distroless images); and its own state beside it: the
# builder's and the reviewer's sealed state, their wake plans, the build
# queue as last given. Readable by that user alone.
chown 65532:65532 "$STATE/store" "$STATE/api-state"
chmod 755 "$STATE" "$STATE/store"
chmod 700 "$STATE/api-state"

# The secrets: each into a variable, checked, then into an env file only root
# can read (docker reads it as root and hands it to the container alone).
keeper_key=$(secret "$project" "$keeper_secret")
[ -n "$keeper_key" ] || { say "the keeper's key could not be read"; exit 1; }
printf 'SHONIN_TESTNET_KEEPER_KEY=%s\n' "$keeper_key" >"$STATE/secrets/keeper.env"
unset keeper_key
if [ -n "$keeper_solana_secret" ]; then
  keeper_solana_key=$(secret "$project" "$keeper_solana_secret" || true)
  if [ -n "$keeper_solana_key" ]; then
    printf 'SHONIN_TESTNET_KEEPER_SOLANA_KEY=%s\n' "$keeper_solana_key" >>"$STATE/secrets/keeper.env"
  else
    say "the keeper's Solana key could not be read: Solana devnet is not kept (task cloud:secrets makes it)"
  fi
  unset keeper_solana_key
fi
tunnel_token=$(secret "$project" "$tunnel_secret")
[ -n "$tunnel_token" ] || { say "the tunnel's token could not be read"; exit 1; }
printf 'TUNNEL_TOKEN=%s\n' "$tunnel_token" >"$STATE/secrets/tunnel.env"
unset tunnel_token
chmod 600 "$STATE/secrets/keeper.env" "$STATE/secrets/tunnel.env"

attribute shonin-waker >"$STATE/waker.sh"
chmod 644 "$STATE/waker.sh"

systemctl is-active --quiet docker || systemctl start docker

# Pulls from Artifact Registry with the VM's token, into a docker config of
# the script's own (the token passes on stdin).
export DOCKER_CONFIG="$STATE/docker"
registry=${api_image%%/*}
token | docker login -u oauth2accesstoken --password-stdin "https://$registry" >/dev/null
for image in "$api_image" "$keeper_image" "$cloudflared_image" "$waker_image"; do
  docker pull -q "$image" >/dev/null
done

# Each container replaced on every boot, restarted by docker if it stops, its
# lines sent to Cloud Logging (log gcplogs-docker-driver).
run() {
  local name=$1
  shift
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" --network host --restart unless-stopped \
    --log-driver gcplogs "$@" >/dev/null
  say "$name started"
}

run shonin-api -v "$STATE/store:/var/lib/shonin/store" -v "$STATE/api-state:/var/lib/shonin/api-state" "$api_image"
run shonin-keeper --env-file "$STATE/secrets/keeper.env" "$keeper_image"
run shonin-tunnel --env-file "$STATE/secrets/tunnel.env" "$cloudflared_image" tunnel --no-autoupdate run
run shonin-waker -v "$STATE/waker.sh:/waker.sh:ro" \
  -e "SHONIN_EXECUTOR_URL=$executor_url" \
  -e "SHONIN_PROVER_INSTANCE=$prover_instance" \
  -e "SHONIN_BUILDER_URL=$builder_url" \
  -e "SHONIN_BUILDER_INSTANCE=$builder_instance" \
  -e "SHONIN_REVIEWER_URL=$reviewer_url" \
  -e "SHONIN_REVIEWER_INSTANCE=$reviewer_instance" \
  -e "SHONIN_WAKE_URL=http://localhost:19082/internal/wake" \
  --entrypoint sh "$waker_image" /waker.sh
say "up"
