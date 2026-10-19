#!/bin/bash
# Stage what a Cloud Build of the images reads, as one gzipped tarball:
#   stage.sh <out.tgz> <deployments dir> [<guest dir>] [<sealed dir>]
# (either directory may be given as "" for none)
# - the repository's files git tracks or would track (so nothing it ignores:
#   no .env, no .shonin, no target directory), less what no image build
#   reads, and refusing outright anything that looks like a secret or
#   build output;
# - `_deployments/`: the network's public deployment files alone (each
#   chain's addresses, its checkers' and its mirror feeds'), never a deploy's
#   progress records;
# - `_guest/`: the prover's reproducibly built guest, when given;
# - `_sealed/`: the reviewer's model key as sealed to its purpose
#   (`sealed.json`, the ciphertext alone), when given. Nothing else is taken
#   from that directory, so the key it was sealed from never stages.
# Run from the repository root.
set -euo pipefail
out=$1
deployments=$2
guest=${3:-}
sealed=${4:-}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

git ls-files -co --exclude-standard -z |
  tr '\0' '\n' |
  grep -v -E '^(docs/|cloudflare/|contracts/evm/lib/|packages/frontend/app/tests/)|(^|/)\.env\.example$' |
  while IFS= read -r path; do [ -f "$path" ] && printf '%s\n' "$path"; done >"$stage/files"
if grep -E '(^|/)\.env($|\.)|(^|/)\.shonin/|(^|/)target/|(^|/)\.private/|(^|/)claude-api-key$|\.(key|pem)$' "$stage/files"; then
  echo "refused: the files above must never leave this machine"
  exit 1
fi

mkdir -p "$stage/_deployments"
found=0
for file in "$deployments"/*.json; do
  case "$(basename "$file")" in
    *.partial.json | *.run-*) continue ;;
  esac
  if basename "$file" | grep -qE '^(evm-[0-9]+|attestation-evm-[0-9]+|mirror-feeds-evm-[0-9]+|solana-[a-z-]+)\.json$'; then
    cp "$file" "$stage/_deployments/"
    found=$((found + 1))
  fi
done
[ "$found" -gt 0 ] || { echo "no deployment files in $deployments"; exit 1; }

extra=(-C "$stage" _deployments)
if [ -n "$guest" ]; then
  mkdir -p "$stage/_guest"
  cp "$guest/shonin-attestation-guest.bin" "$stage/_guest/"
  extra+=(_guest)
fi

if [ -n "$sealed" ]; then
  [ -s "$sealed/sealed.json" ] || { echo "no sealed.json in $sealed"; exit 1; }
  # A sealed message, not a key: refused if it holds anything that looks
  # like one in the clear (nothing is printed).
  if grep -q -E 'sk-ant-|BEGIN [A-Z ]*PRIVATE KEY' "$sealed/sealed.json"; then
    echo "refused: $sealed/sealed.json holds something that looks like a key in the clear"
    exit 1
  fi
  mkdir -p "$stage/_sealed"
  cp "$sealed/sealed.json" "$stage/_sealed/sealed.json"
  extra+=(_sealed)
fi

# No macOS metadata (its "._" files would reach the build as sources).
export COPYFILE_DISABLE=1
tar -cf "$stage/source.tar" -T "$stage/files"
tar -rf "$stage/source.tar" "${extra[@]}"
gzip -c "$stage/source.tar" >"$out"
echo "staged $(wc -l <"$stage/files" | tr -d ' ') files, $found deployment files$([ -n "$guest" ] && echo ', the guest')$([ -n "$sealed" ] && echo ', the sealed model key'): $(du -h "$out" | cut -f1)"
