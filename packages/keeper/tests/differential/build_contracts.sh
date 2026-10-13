#!/usr/bin/env bash
# Compile the keeper's differential stand-in contracts and write their
# creation code to code.json beside this script, for the tests to deploy.
#
# Run from anywhere: ./packages/keeper/tests/differential/build_contracts.sh
# It needs solc 0.8.30 or later: SOLC names the compiler; without it, the
# copy svm (Foundry's compiler manager) keeps is used if there is one, and
# `solc` on the PATH otherwise.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"

if [[ -z "${SOLC:-}" ]]; then
    for candidate in \
        "$HOME/Library/Application Support/svm/0.8.30/solc-0.8.30" \
        "$HOME/.svm/0.8.30/solc-0.8.30" \
        solc; do
        if command -v "$candidate" >/dev/null 2>&1; then
            SOLC="$candidate"
            break
        fi
    done
fi
version="$("$SOLC" --version | sed -n 's/^Version: \([0-9.]*\).*/\1/p')"
case "$version" in
    0.8.3[0-9]* | 0.8.[4-9][0-9]* | 0.9.*) ;;
    *)
        echo "solc $version is too old: the interfaces need 0.8.30; set SOLC" >&2
        exit 1
        ;;
esac

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

"$SOLC" \
    --base-path "$repo" \
    --allow-paths "$repo/contracts/evm/src,$here/contracts" \
    --evm-version cancun \
    --optimize --optimize-runs 200 --via-ir \
    --metadata-hash none \
    --combined-json bin \
    "$here"/contracts/*.sol >"$out/combined.json"

# Keep the deployable contracts only, by name, as 0x-prefixed creation code.
python3 -I - "$out/combined.json" "$here/code.json" <<'EOF'
import json
import sys

combined = json.load(open(sys.argv[1]))
wanted = [
    "StandInVault",
    "StandInStation",
    "StandInRegistry",
    "StandInDriver",
    "StandInExchange",
    "StandInBoard",
    "StandInFactory",
    "StandInCloser",
]
code = {}
for path, contract in combined["contracts"].items():
    name = path.rsplit(":", 1)[1]
    if name in wanted:
        code[name] = "0x" + contract["bin"]
missing = [name for name in wanted if not code.get(name) or code[name] == "0x"]
if missing:
    sys.exit(f"not compiled: {missing}")
with open(sys.argv[2], "w") as out:
    json.dump(dict(sorted(code.items())), out, indent=2)
    out.write("\n")
EOF
echo "wrote $here/code.json"
