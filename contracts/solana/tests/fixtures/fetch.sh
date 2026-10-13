#!/bin/sh
# Fetch the third-party programs the Solana tests load into the directory
# given (.shonin/fixtures from the repository root, where the tests look),
# each dumped from Solana mainnet and checked against the hash committed in
# programs.txt beside this script. One already there with the pinned hash is
# kept. Run by `task contracts:solana:fixtures` and by CI.
set -eu

out=${1:?usage: fetch.sh <fixtures directory>}
list="$(dirname "$0")/programs.txt"
mkdir -p "$out"
grep -v '^#' "$list" | while read -r file id sha; do
  [ -n "$file" ] || continue
  path="$out/$file"
  if [ -f "$path" ] && [ "$(shasum -a 256 "$path" | cut -d' ' -f1)" = "$sha" ]; then
    echo "$file is $id, as pinned"
    continue
  fi
  solana program dump -u m "$id" "$path.new" </dev/null >/dev/null
  got=$(shasum -a 256 "$path.new" | cut -d' ' -f1)
  if [ "$got" != "$sha" ]; then
    rm -f "$path.new"
    echo "$id on mainnet is $got, not the pinned $sha: it was upgraded. Run the tests against the new binary before pinning its hash."
    exit 1
  fi
  mv "$path.new" "$path"
  echo "$file is $id, fetched and checked"
done
