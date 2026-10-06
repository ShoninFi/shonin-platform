#!/bin/sh
# The waker: starts the prover's VM when the executor wants a seal and the
# prover is off. Runs on the services VM (a container on the host's network,
# started by startup.sh), with the VM's own service account, which may start
# and read that one instance and nothing else.
#
# Every SHONIN_WAKER_EVERY seconds (30) it asks the executor's status port
# (GET /attestation) whether a seal is wanted; when one is and the prover's
# instance is TERMINATED (stopped, or reclaimed as Spot), it starts it. It
# starts it at most once every SHONIN_WAKER_GAP seconds (900), so a prover
# that fails and powers off is not started over and over; the prover powers
# itself off once idle. Nothing it reads or holds is secret but the VM's
# short-lived token, which goes to curl on stdin and is never printed.
set -u

executor=${SHONIN_EXECUTOR_URL:?the executor status port URL}
instance=${SHONIN_PROVER_INSTANCE:?projects/<project>/zones/<zone>/instances/<name>}
every=${SHONIN_WAKER_EVERY:-30}
gap=${SHONIN_WAKER_GAP:-900}
metadata=http://metadata.google.internal/computeMetadata/v1
compute=https://compute.googleapis.com/compute/v1/$instance

say() { echo "waker: $*"; }

token() {
  curl -sf --max-time 10 -H 'Metadata-Flavor: Google' \
    "$metadata/instance/service-accounts/default/token" |
    sed -n 's/.*"access_token" *: *"\([^"]*\)".*/\1/p'
}

# Whether the executor says a seal is wanted. An executor that doesn't
# answer wants nothing.
wanted() {
  curl -sf --max-time 10 "$executor/attestation" |
    tr -d ' \t\r\n' | grep -q '"seal_wanted":true'
}

# The prover instance's status (RUNNING, TERMINATED, ...), or nothing.
status() {
  printf 'Authorization: Bearer %s\n' "$(token)" |
    curl -sf --max-time 10 -H @- "$compute?fields=status" |
    tr -d ' \t\r\n' | sed -n 's/.*"status":"\([A-Z_]*\)".*/\1/p'
}

start() {
  printf 'Authorization: Bearer %s\n' "$(token)" |
    curl -sf --max-time 30 -X POST -H @- -H 'Content-Length: 0' "$compute/start" >/dev/null
}

say "watching $executor/attestation for the prover $instance"
last_start=0
while :; do
  if wanted; then
    state=$(status)
    now=$(date +%s)
    case "$state" in
      TERMINATED)
        if [ $((now - last_start)) -ge "$gap" ]; then
          if start; then
            last_start=$now
            say "a seal is wanted and the prover was off: started it"
          else
            say "a seal is wanted and the prover is off, but it could not be started"
          fi
        fi
        ;;
      "")
        say "a seal is wanted, and the prover's state could not be read"
        ;;
    esac
  fi
  sleep "$every"
done
