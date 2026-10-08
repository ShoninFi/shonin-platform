#!/bin/sh
# The waker: starts the machines that run on demand when they are wanted.
# Runs on the services VM (a container on the host's network, started by
# startup.sh), with the VM's own service account, which may start, stop and
# read those instances and nothing else.
#
# Every SHONIN_WAKER_EVERY seconds (30):
#
# - the prover: when the executor, the builder or the reviewer says on its
#   status port (GET /attestation) that a seal is wanted, and the prover's
#   instance is TERMINATED (stopped, or reclaimed as Spot), it is started, at
#   most once every SHONIN_WAKER_GAP seconds (900), so a prover that fails and
#   powers off is not started over and over; it powers itself off once idle.
#   A machine that doesn't answer wants nothing.
# - the builder and the reviewer: the API beside it says which are wanted
#   (GET SHONIN_WAKE_URL, the API's internal port, on this machine alone),
#   and a wanted one that is TERMINATED is started, at most once every
#   SHONIN_WAKER_MACHINE_GAP seconds (300). One that is still stopping is
#   left until it has stopped. Each exits once idle and its VM powers off by
#   itself; should one not, the waker stops it: one RUNNING whose status says
#   it has exited ("idle_exit": true), or that has not answered for
#   SHONIN_WAKER_UNREACHABLE seconds (900) while RUNNING.
#
# Nothing it reads or holds is secret but the VM's short-lived token, which
# goes to curl on stdin and is never printed. SHONIN_COMPUTE_URL and
# SHONIN_METADATA_URL name Google's endpoints, and SHONIN_WAKER_ROUNDS stops
# it after that many rounds (0, the default, never), for its test.
set -u

executor=${SHONIN_EXECUTOR_URL:?the executor status port URL}
prover=${SHONIN_PROVER_INSTANCE:?projects/<project>/zones/<zone>/instances/<name>}
builder_url=${SHONIN_BUILDER_URL:-}
builder_instance=${SHONIN_BUILDER_INSTANCE:-}
reviewer_url=${SHONIN_REVIEWER_URL:-}
reviewer_instance=${SHONIN_REVIEWER_INSTANCE:-}
wake_url=${SHONIN_WAKE_URL:-http://localhost:19082/internal/wake}
every=${SHONIN_WAKER_EVERY:-30}
gap=${SHONIN_WAKER_GAP:-900}
machine_gap=${SHONIN_WAKER_MACHINE_GAP:-300}
unreachable=${SHONIN_WAKER_UNREACHABLE:-900}
rounds=${SHONIN_WAKER_ROUNDS:-0}
metadata=${SHONIN_METADATA_URL:-http://metadata.google.internal/computeMetadata/v1}
compute=${SHONIN_COMPUTE_URL:-https://compute.googleapis.com/compute/v1}

say() { echo "waker: $*"; }

token() {
  curl -sf --max-time 10 -H 'Metadata-Flavor: Google' \
    "$metadata/instance/service-accounts/default/token" |
    sed -n 's/.*"access_token" *: *"\([^"]*\)".*/\1/p'
}

# A machine's status port's answer, on one line, or nothing if it doesn't
# answer.
attestation() {
  [ -n "$1" ] || return 1
  curl -sf --max-time 5 "$1/attestation" | tr -d ' \t\r\n'
}

# An instance's status (RUNNING, TERMINATED, ...), or nothing.
status() {
  printf 'Authorization: Bearer %s\n' "$(token)" |
    curl -sf --max-time 10 -H @- "$compute/$1?fields=status" |
    tr -d ' \t\r\n' | sed -n 's/.*"status":"\([A-Z_]*\)".*/\1/p'
}

# Start or stop an instance.
act() {
  printf 'Authorization: Bearer %s\n' "$(token)" |
    curl -sf --max-time 30 -X POST -H @- -H 'Content-Length: 0' "$compute/$2/$1" >/dev/null
}

say "watching $executor/attestation for the prover $prover"
[ -n "$builder_instance" ] && say "and $wake_url for the builder $builder_instance"
[ -n "$reviewer_instance" ] && say "and $wake_url for the reviewer $reviewer_instance"

prover_started=0
builder_started=0
reviewer_started=0
builder_silent=0
reviewer_silent=0

# One machine that runs on demand: started when wanted and off, stopped when
# it exited (or went silent) and did not power off.
tend() {
  name=$1 url=$2 instance=$3 answer=$4 wanted=$5
  [ -n "$instance" ] || return 0
  now=$(date +%s)
  eval "started=\$${name}_started silent=\$${name}_silent"
  state=$(status "$instance")
  case "$state" in
    TERMINATED | STOPPED)
      eval "${name}_silent=0"
      if [ "$wanted" = yes ] && [ $((now - started)) -ge "$machine_gap" ]; then
        if act start "$instance"; then
          eval "${name}_started=$now"
          say "the $name is wanted and was off: started it"
        else
          say "the $name is wanted and is off, but it could not be started"
        fi
      fi
      ;;
    RUNNING)
      if [ -n "$answer" ] && ! printf '%s' "$answer" | grep -q '"idle_exit":true'; then
        eval "${name}_silent=0"
        return 0
      fi
      if printf '%s' "$answer" | grep -q '"idle_exit":true'; then
        why="it has exited"
      else
        [ "$silent" -gt 0 ] || { eval "${name}_silent=$now"; return 0; }
        [ $((now - silent)) -ge "$unreachable" ] || return 0
        why="it has not answered for $((now - silent)) s"
      fi
      if act stop "$instance"; then
        eval "${name}_silent=0"
        say "the $name is running and $why: stopped it"
      else
        say "the $name is running and $why, but it could not be stopped"
      fi
      ;;
    "")
      [ "$wanted" = yes ] && say "the $name is wanted, and its state could not be read"
      ;;
  esac
}

round=0
while :; do
  executor_answer=$(attestation "$executor")
  builder_answer=$(attestation "$builder_url")
  reviewer_answer=$(attestation "$reviewer_url")

  # The prover, for whichever machine wants a seal.
  if printf '%s\n%s\n%s\n' "$executor_answer" "$builder_answer" "$reviewer_answer" |
    grep -q '"seal_wanted":true'; then
    state=$(status "$prover")
    now=$(date +%s)
    case "$state" in
      TERMINATED)
        if [ $((now - prover_started)) -ge "$gap" ]; then
          if act start "$prover"; then
            prover_started=$now
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

  # The builder and the reviewer, as the API says. An API that doesn't
  # answer wants nothing started.
  wake=$(curl -sf --max-time 10 "$wake_url" | tr -d ' \t\r\n')
  for name in builder reviewer; do
    if printf '%s' "$wake" | grep -q "\"$name\":true"; then wanted=yes; else wanted=no; fi
    case "$name" in
      builder) tend builder "$builder_url" "$builder_instance" "$builder_answer" "$wanted" ;;
      reviewer) tend reviewer "$reviewer_url" "$reviewer_instance" "$reviewer_answer" "$wanted" ;;
    esac
  done

  round=$((round + 1))
  [ "$rounds" -gt 0 ] && [ "$round" -ge "$rounds" ] && break
  sleep "$every"
done
