#!/bin/bash
# The waker's test: runs deploy/gcp/services/waker.sh for a round or two
# against a stand-in for Google's metadata server and Compute Engine, the
# API's wake answer, and the machines' status ports, and checks what it
# started and stopped. Needs python3 and curl; touches nothing outside a
# temporary directory.
#   deploy/gcp/services/waker-test.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
scratch=$(mktemp -d)
stand_in_pid=
cleanup() {
  if [ -n "$stand_in_pid" ]; then
    kill "$stand_in_pid" 2>/dev/null || true
    wait "$stand_in_pid" 2>/dev/null || true
  fi
  rm -rf "$scratch"
}
trap cleanup EXIT

# The stand-in: every answer comes from $scratch/world.json, and every start
# or stop is appended to $scratch/acts.
cat >"$scratch/stand_in.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

scratch = sys.argv[1]

def world():
    with open(f"{scratch}/world.json") as f:
        return json.load(f)

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def answer(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        w = world()
        path = self.path.split("?")[0]
        if path.endswith("/service-accounts/default/token"):
            return self.answer(200, {"access_token": "stand-in", "expires_in": 60})
        if path.startswith("/compute/"):
            name = path.rsplit("/", 1)[-1]
            return self.answer(200, {"status": w["states"].get(name, "TERMINATED")})
        if path == "/wake":
            if w.get("wake") is None:
                return self.answer(503, {})
            return self.answer(200, w["wake"])
        machine = path.strip("/").split("/")[0]
        if path.endswith("/attestation") and machine in w["status"]:
            body = w["status"][machine]
            if body is None:
                return self.answer(503, {})
            return self.answer(200, body)
        return self.answer(404, {})

    def do_POST(self):
        if self.path.startswith("/compute/"):
            name, act = self.path.rstrip("/").split("/")[-2:]
            with open(f"{scratch}/acts", "a") as f:
                f.write(f"{name} {act}\n")
            return self.answer(200, {})
        return self.answer(404, {})

server = HTTPServer(("127.0.0.1", 0), Handler)
with open(f"{scratch}/port", "w") as f:
    f.write(str(server.server_port))
server.serve_forever()
PY

echo '{"states": {}, "status": {}, "wake": null}' >"$scratch/world.json"
python3 -I "$scratch/stand_in.py" "$scratch" &
stand_in_pid=$!
for _ in $(seq 1 50); do [ -s "$scratch/port" ] && break; sleep 0.1; done
base="http://127.0.0.1:$(cat "$scratch/port")"

failed=0
world() { printf '%s' "$1" >"$scratch/world.json"; }
# Run the waker for $1 rounds; what it started and stopped is left in acts.
waker() {
  : >"$scratch/acts"
  SHONIN_EXECUTOR_URL="$base/executor" \
    SHONIN_PROVER_INSTANCE=prover \
    SHONIN_BUILDER_URL="$base/builder" SHONIN_BUILDER_INSTANCE=builder \
    SHONIN_REVIEWER_URL="$base/reviewer" SHONIN_REVIEWER_INSTANCE=reviewer \
    SHONIN_WAKE_URL="$base/wake" \
    SHONIN_METADATA_URL="$base/metadata" SHONIN_COMPUTE_URL="$base/compute" \
    SHONIN_WAKER_EVERY=0 SHONIN_WAKER_ROUNDS="$1" \
    SHONIN_WAKER_UNREACHABLE="${UNREACHABLE:-900}" \
    sh "$here/waker.sh" >"$scratch/log" 2>&1
}
expect() {
  local name=$1 wanted=$2 got
  got=$(sort "$scratch/acts" | tr '\n' ',' | sed 's/,$//')
  if [ "$got" = "$wanted" ]; then
    echo "ok: $name"
  else
    echo "FAILED: $name: wanted [$wanted], got [$got]"
    sed 's/^/  /' "$scratch/log"
    failed=1
  fi
}

world '{"states": {"builder": "TERMINATED", "reviewer": "TERMINATED", "prover": "TERMINATED"},
        "status": {}, "wake": {"builder": true, "reviewer": false}}'
waker 2
expect "a wanted machine that is off is started, once within its gap; one not wanted is not" "builder start"

world '{"states": {"builder": "STOPPING", "reviewer": "TERMINATED", "prover": "TERMINATED"},
        "status": {}, "wake": {"builder": true, "reviewer": true}}'
waker 1
expect "a machine still stopping is left until it has stopped" "reviewer start"

world '{"states": {"builder": "TERMINATED", "reviewer": "TERMINATED", "prover": "TERMINATED"},
        "status": {}, "wake": null}'
waker 1
expect "an API that does not answer wants nothing started" ""

world '{"states": {"builder": "RUNNING", "reviewer": "TERMINATED", "prover": "TERMINATED"},
        "status": {"executor": {"seal_wanted": false}, "builder": {"seal_wanted": true}},
        "wake": {"builder": false, "reviewer": false}}'
waker 2
expect "a seal wanted by the builder starts the prover, once within its gap" "prover start"

world '{"states": {"builder": "RUNNING", "reviewer": "RUNNING", "prover": "RUNNING"},
        "status": {"executor": {"seal_wanted": false}, "builder": {"seal_wanted": false},
                   "reviewer": {"seal_wanted": false, "idle_exit": true}},
        "wake": {"builder": false, "reviewer": false}}'
waker 1
expect "a running machine that says it has exited is stopped; one answering is not" "reviewer stop"

world '{"states": {"builder": "RUNNING", "reviewer": "TERMINATED", "prover": "TERMINATED"},
        "status": {"executor": {"seal_wanted": false}}, "wake": {"builder": false, "reviewer": false}}'
waker 3
expect "a running machine silent for less than its while is left running" ""
UNREACHABLE=0 waker 2
expect "a running machine silent for its while is stopped" "builder stop"

[ "$failed" = 0 ] && echo "the waker's test passed"
exit "$failed"
