#!/bin/bash
# The testnet-market VM's startup script (Container-Optimized OS), run as
# root on every boot. It starts, on the host's network:
#
#   shonin-testnet-market   the market, health on :19088, open to the VPC,
#                           with its own test network key alone
#
# What it runs is named in the instance's metadata (`task
# cloud:deploy:testnet-market` sets it): shonin-testnet-market-image (by
# digest), shonin-testnet-market-secret (its key's secret) and
# shonin-rpc-secret (the paid endpoints, optional).
#
# The key is read from Secret Manager with the VM's own token and written to
# a file only root can read; it is never echoed, never put on a command
# line, and never logged.
set -euo pipefail
umask 077

METADATA=http://metadata.google.internal/computeMetadata/v1
STATE=/var/lib/shonin

say() { echo "shonin-testnet-market: $*"; }

metadata() {
  curl -sf --retry 5 --retry-delay 2 -H 'Metadata-Flavor: Google' "$METADATA/$1"
}

attribute() { metadata "instance/attributes/$1"; }

# Container-Optimized OS drops every inbound connection but SSH: the health
# route answers the VPC, and the cloud firewall keeps the rest out.
iptables -C INPUT -p tcp -s 10.128.0.0/9 --dport 19088 -j ACCEPT 2>/dev/null ||
  iptables -I INPUT -p tcp -s 10.128.0.0/9 --dport 19088 -j ACCEPT

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
image=$(attribute shonin-testnet-market-image)
key_secret=$(attribute shonin-testnet-market-secret)
rpc_secret=$(attribute shonin-rpc-secret 2>/dev/null || true)

mkdir -p "$STATE/secrets" "$STATE/docker"
chmod 700 "$STATE/secrets" "$STATE/docker"

key=$(secret "$project" "$key_secret")
[ -n "$key" ] || { say "the market's key could not be read"; exit 1; }
printf 'SHONIN_TESTNET_MARKET_KEY=%s\n' "$key" >"$STATE/secrets/market.env"
unset key
# The paid endpoints (SHONIN_RPC_* lines); without them the market reads the
# committed public endpoints.
: >"$STATE/secrets/rpc.env"
if [ -n "$rpc_secret" ]; then
  rpc=$(secret "$project" "$rpc_secret" || true)
  printf '%s\n' "$rpc" | grep -E '^SHONIN_RPC_[A-Z0-9_]+=https?://' >"$STATE/secrets/rpc.env" || true
  unset rpc
  say "$(grep -c . "$STATE/secrets/rpc.env" || true) paid endpoint(s)"
fi
chmod 600 "$STATE/secrets/market.env" "$STATE/secrets/rpc.env"

systemctl is-active --quiet docker || systemctl start docker

# Pulls from Artifact Registry with the VM's token, into a docker config of
# the script's own (the token passes on stdin).
export DOCKER_CONFIG="$STATE/docker"
registry=${image%%/*}
token | docker login -u oauth2accesstoken --password-stdin "https://$registry" >/dev/null
docker pull -q "$image" >/dev/null

# Replaced on every boot, restarted by docker if it stops, its lines sent to
# Cloud Logging (log gcplogs-docker-driver).
docker rm -f shonin-testnet-market >/dev/null 2>&1 || true
docker run -d --name shonin-testnet-market --network host --restart unless-stopped \
  --log-driver gcplogs \
  --env-file "$STATE/secrets/market.env" --env-file "$STATE/secrets/rpc.env" \
  "$image" >/dev/null
say "up"
