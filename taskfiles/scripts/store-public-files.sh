#!/bin/sh
# The files of a store that may go to its public copy, one path per line,
# relative to the store: only content-addressed files of strategies whose
# source is published.
#
#   store-public-files.sh <store directory>
#
# A bundle goes (component.wasm, bundle.tar, hashes.json) only where its
# build record (hashes.json, filed under that bundle's own hash) names a
# source whose source.tar is in the store; with it go that source.tar and the
# manifest the record names. A review report goes only if it says it is not
# of sealed source. Nothing else ever goes: no sealed file however it is
# spelt, no queue, payments, archives or pending reviews, nothing outside a
# sha256-<hex> directory. It fails (exit 1) if anything it was about
# to list is one of those.
set -eu

store=${1:?usage: store-public-files.sh <store directory>}
cd "$store"

# A sha256:<64 lowercase hex> hash's directory name, or nothing.
directory() {
  digits=${1#sha256:}
  case "$1" in
    sha256:*) ;;
    *) return 0 ;;
  esac
  [ "${#digits}" -eq 64 ] || return 0
  case "$digits" in
    *[!0-9a-f]*) return 0 ;;
  esac
  echo "sha256-$digits"
}

listed=$(
  for record in sha256-*/hashes.json; do
    [ -f "$record" ] || continue
    here=${record%/hashes.json}
    bundle=$(directory "$(jq -r '.bundle // empty' "$record")")
    source=$(directory "$(jq -r '.source // empty' "$record")")
    manifest=$(directory "$(jq -r '.manifest // empty' "$record")")
    # A record filed under another bundle's hash vouches for nothing here.
    [ "$bundle" = "$here" ] || continue
    [ -n "$source" ] && [ -f "$source/source.tar" ] || continue
    for name in component.wasm bundle.tar hashes.json; do
      [ -f "$here/$name" ] && echo "$here/$name"
    done
    echo "$source/source.tar"
    [ -n "$manifest" ] && [ -f "$manifest/manifest.json" ] && echo "$manifest/manifest.json"
  done
  for report in sha256-*/report.json; do
    [ -f "$report" ] || continue
    [ "$(jq -r '.sealed' "$report")" = "false" ] && echo "$report"
  done
  true
)

# The last word: every path one of the public names in a hash's directory,
# and none of the names that must never leave.
echo "$listed" | sort -u | while IFS= read -r path; do
  [ -n "$path" ] || continue
  case "$path" in
    *[Ss][Ee][Aa][Ll][Ee][Dd]* | *queue.json | *payments.json | *archives* | *pending-reviews.json)
      echo "refused: $path" >&2
      exit 1
      ;;
    sha256-*/component.wasm | sha256-*/bundle.tar | sha256-*/hashes.json | sha256-*/manifest.json | sha256-*/report.json | sha256-*/source.tar)
      echo "$path"
      ;;
    *)
      echo "refused: $path" >&2
      exit 1
      ;;
  esac
done
