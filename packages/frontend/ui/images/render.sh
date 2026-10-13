#!/usr/bin/env bash
# Rasterise the share images and the PNG icons from their SVG sources, with
# headless Chrome (it reads the self-hosted fonts and the logo files the
# sources point at), then shrink them with optimise.py (python3). Run it
# after changing a source, then commit the PNGs:
#
#   packages/frontend/ui/images/render.sh
#
# CHROME=<path> picks the browser; by default the first Chrome or Chromium
# found on the PATH, or Google Chrome's usual macOS location.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
chrome="${CHROME:-}"
if [ -z "$chrome" ]; then
  for candidate in google-chrome chromium chromium-browser \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"; do
    if command -v "$candidate" >/dev/null 2>&1 || [ -x "$candidate" ]; then
      chrome="$candidate"
      break
    fi
  done
fi
if [ -z "$chrome" ]; then
  echo "render.sh: no Chrome found; set CHROME=<path>" >&2
  exit 1
fi

profile="$(mktemp -d)"
source="$here/.render-$$.svg"
trap 'rm -rf "$profile" "$source"' EXIT

# render <source.svg> <out.png> <width> <height> [optimise.py options]
#
# The source is drawn at exactly <width> by <height> (a size of 100% is
# replaced by it), in a window at least 800 pixels square, since headless
# Chrome won't make a smaller one, and the PNG is cropped to it. The copy
# sits beside the source so its relative font and logo paths still resolve.
render() {
  local src="$1" out="$2" width="$3" height="$4"
  shift 4
  sed -e "s/width=\"100%\" height=\"100%\"/width=\"$width\" height=\"$height\"/" \
    "$here/$src" >"$source"
  "$chrome" --headless=new --disable-gpu --hide-scrollbars \
    --user-data-dir="$profile" --allow-file-access-from-files \
    --force-device-scale-factor=1 --default-background-color=00000000 \
    --virtual-time-budget=3000 \
    --window-size="$((width > 800 ? width : 800)),$((height > 800 ? height : 800))" \
    --screenshot="$here/$out" "file://$source" >/dev/null 2>&1
  python3 -I "$here/optimise.py" --crop "${width}x${height}" "$@" "$here/$out"
}

mkdir -p "$here/share" "$here/icons"
# The share images to 256 colours (their gradients stay smooth, at a third
# of the size); the icons losslessly.
render share-site.svg share/shonin.png 1200 630 --palette
render share-app.svg share/shonin-app.png 1200 630 --palette
render icon-32.svg icons/shonin-32.png 32 32
render icon.svg icons/shonin-180.png 180 180
render icon.svg icons/shonin-512.png 512 512
