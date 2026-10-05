#!/usr/bin/env bash
# Writes a new random keypair for every Solana program into a directory
# (default contracts/solana/target/deploy), in the format Anchor and
# solana-keygen write: a JSON array of the 32-byte secret seed then the
# 32-byte public key.
#
# The program crates compile in their address from these files, so the root
# workspace's crates are built against fresh program ids, as `anchor build`
# gives the Solana job, without needing the Solana toolchain. Only OpenSSL
# is used. Refuses to overwrite an existing keypair.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-"$root/contracts/solana/target/deploy"}
mkdir -p "$out"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for manifest in "$root"/contracts/solana/programs/*/Cargo.toml; do
  # The keypair is named after the program's library, as Anchor names it.
  name=$(awk '/^\[lib\]/{lib=1; next} /^\[/{lib=0} lib && /^name *=/{gsub(/[" ]/, "", $0); sub(/^name=/, "", $0); print; exit}' "$manifest")
  if [ -z "$name" ]; then
    name=$(awk -F'"' '/^name *=/{print $2; exit}' "$manifest" | tr - _)
  fi
  file="$out/$name-keypair.json"
  if [ -e "$file" ]; then
    echo "$file already exists" >&2
    exit 1
  fi
  openssl genpkey -algorithm ed25519 -outform DER -out "$tmp/key.der"
  openssl pkey -inform DER -in "$tmp/key.der" -pubout -outform DER -out "$tmp/pub.der"
  # The raw seed and public key are the last 32 bytes of each DER encoding.
  bytes=$( { tail -c 32 "$tmp/key.der"; tail -c 32 "$tmp/pub.der"; } | od -An -v -tu1 | tr -s ' \n' ' ' | sed 's/^ //; s/ $//; s/ /,/g')
  echo "[$bytes]" > "$file"
  chmod 600 "$file"
done
