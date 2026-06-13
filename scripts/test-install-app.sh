#!/usr/bin/env bash
# Functional test for install-app.sh. Targets a temp dest via CURATOR_APP_DEST
# so it never touches a real /Applications install.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Build a fake "built" app bundle with a marker file.
src="$tmp/build/curator.app"
mkdir -p "$src/Contents"
echo "marker" > "$src/Contents/marker.txt"

dest="$tmp/Applications/curator.app"

# 1. Happy path: copies the bundle to the dest.
CURATOR_APP_DEST="$dest" "$here/install-app.sh" "$src"
[ -f "$dest/Contents/marker.txt" ] || { echo "FAIL: bundle not installed"; exit 1; }

# 2. Replaces an existing dest (stale file must be gone).
echo "stale" > "$dest/Contents/stale.txt"
CURATOR_APP_DEST="$dest" "$here/install-app.sh" "$src"
[ -f "$dest/Contents/marker.txt" ] || { echo "FAIL: bundle missing after replace"; exit 1; }
[ -e "$dest/Contents/stale.txt" ] && { echo "FAIL: stale file survived replace"; exit 1; }

# 3. Missing source bundle: must exit non-zero.
if CURATOR_APP_DEST="$dest" "$here/install-app.sh" "$tmp/nope.app" 2>/dev/null; then
  echo "FAIL: expected non-zero exit for missing source"; exit 1
fi

echo "PASS: install-app.sh"
