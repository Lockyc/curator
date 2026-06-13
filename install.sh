#!/usr/bin/env bash
# install.sh — build curator from source and install it to /Applications.
# Usage:  bash install.sh
#    or:  curl -fsSL https://raw.githubusercontent.com/Lockyc/curator/main/install.sh | bash
#
# Manages a persistent source clone at ~/.curator and (re)builds from it. For
# guided setup with prerequisite installation, use /curator:install in Claude Code.
set -euo pipefail

REPO_URL="https://github.com/Lockyc/curator"
INSTALL_DIR="$HOME/.curator"

# 1. Hard prerequisites. The /curator:install command offers to install these;
#    the bare script only refuses with a hint.
missing=0
for c in git cargo npm; do
  if ! command -v "$c" >/dev/null 2>&1; then
    echo "curator: '$c' is required but not found on PATH" >&2
    missing=1
  fi
done
if [ "$missing" -ne 0 ]; then
  echo "curator: install Rust (https://rustup.rs), Node (brew install node), and" >&2
  echo "         Xcode Command Line Tools (xcode-select --install), then re-run." >&2
  exit 1
fi

# 2. Clone or update the source checkout.
if [ ! -e "$INSTALL_DIR" ]; then
  echo "→ cloning curator into $INSTALL_DIR"
  git clone "$REPO_URL" "$INSTALL_DIR"
elif [ -d "$INSTALL_DIR/.git" ]; then
  echo "→ updating curator clone in $INSTALL_DIR"
  git -C "$INSTALL_DIR" pull --ff-only
else
  echo "curator: $INSTALL_DIR exists but is not a git clone — move it aside and re-run." >&2
  exit 1
fi

# 3. Build the release bundle.
cd "$INSTALL_DIR"
echo "→ installing npm deps"
npm install
echo "→ building release bundle (this takes a while)"
npm run tauri build

# 4. Install the built app into /Applications.
bash scripts/install-app.sh "src-tauri/target/release/bundle/macos/curator.app"

# 5. Seed the user config from the example (never overwrite an existing one).
mkdir -p "$HOME/.config/curator"
if [ ! -f "$HOME/.config/curator/tabs.toml" ]; then
  cp examples/tabs.toml "$HOME/.config/curator/tabs.toml"
  echo "→ seeded ~/.config/curator/tabs.toml from the example"
else
  echo "→ ~/.config/curator/tabs.toml already exists — left untouched"
fi

echo ""
echo "✓ curator installed to /Applications/curator.app"
echo "  Edit ~/.config/curator/tabs.toml to curate your tabs, then launch curator."
echo "  Update any time by re-running this installer (it git-pulls + rebuilds)."
