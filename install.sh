#!/usr/bin/env bash
# install.sh — build curator from source and install it to /Applications.
# Usage:  bash install.sh
#    or:  curl -fsSL https://raw.githubusercontent.com/Lockyc/curator/main/install.sh | bash
#
# The curl URL and the git clone below require the GitHub repo to be public.
#
# Two modes, auto-detected:
#   • IN_REPO     — run from a curator checkout: builds from the current working
#                   tree (so local changes are picked up). No clone/pull.
#   • NOT_IN_REPO — otherwise: manages a persistent source clone at ~/.curator
#                   (clone if absent, git pull if present) and builds from it.
#
# Never relaunches the app (the caller decides) and never depends on `just`. For
# guided setup with prerequisite installation, use /curator:install in Claude Code.
set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
  echo "curator is a macOS-only app; install.sh only runs on macOS." >&2
  exit 1
fi

REPO_URL="https://github.com/Lockyc/curator"
INSTALL_DIR="$HOME/.curator"

# 1. Hard prerequisites. /curator:install offers to install these; the bare
#    script only refuses with a hint (except the Tauri CLI, which it backstops).
missing=0
for c in git cargo; do
  if ! command -v "$c" >/dev/null 2>&1; then
    echo "curator: '$c' is required but not found on PATH" >&2
    missing=1
  fi
done
if [ "$missing" -ne 0 ]; then
  echo "curator: install Rust (https://rustup.rs) and Xcode Command Line Tools" >&2
  echo "         (xcode-select --install), then re-run." >&2
  exit 1
fi

# 2. Resolve the source dir (IN_REPO vs clone at ~/.curator).
if [ -f install.sh ] && [ -f src-tauri/tauri.conf.json ]; then
  SRC="$(pwd)"
  echo "→ building from the current curator checkout: $SRC"
else
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
  SRC="$INSTALL_DIR"
fi

# 3. Tauri CLI backstop — a source build needs `cargo tauri`; ship it as a cargo global.
if ! command -v cargo-tauri >/dev/null 2>&1; then
  echo "→ installing the Tauri CLI (cargo install tauri-cli — this takes a while)"
  cargo install tauri-cli --version '^2' --locked
fi

# 4. Build the release bundle.
cd "$SRC"
echo "→ building release bundle (this takes a few minutes)"
( cd src-tauri && cargo tauri build )

# 5. Install the built app into /Applications.
bash scripts/install-app.sh "src-tauri/target/release/bundle/macos/curator.app"

# 6. Seed the user config from the example (never overwrite an existing one).
mkdir -p "$HOME/.config/curator"
if [ ! -f "$HOME/.config/curator/config.toml" ]; then
  cp examples/config.toml "$HOME/.config/curator/config.toml"
  echo "→ seeded ~/.config/curator/config.toml from the example"
else
  echo "→ ~/.config/curator/config.toml already exists — left untouched"
fi

echo ""
echo "✓ curator installed to /Applications/curator.app"
echo "  Edit ~/.config/curator/config.toml to curate your tabs, then launch curator."
echo "  Update any time by re-running this installer (it git-pulls + rebuilds)."
