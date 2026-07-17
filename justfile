# curator — task runner

# Recipes run in `sh`, which doesn't inherit cargo from an interactive fish/zsh setup.
# Guarantee rustup's bin dir is on PATH so the tauri CLI can find `cargo`.
export PATH := env_var('HOME') + "/.cargo/bin:" + env_var('PATH')

# `default` pipes `just --list` through a small stock-perl filter that clips long recipe
# docs to your terminal width (…) instead of wrapping. Self-contained — no external files;
# falls back to plain `just --list` where perl is absent. Edit the recipes below, not this.
# List available recipes
default:
    @if command -v perl >/dev/null 2>&1; then just --color always --list | perl -CS -Mutf8 -lpe 'BEGIN{($w)=`stty size 2>/dev/null </dev/tty`=~/ (\d+)/; $w||=100; $col=(-t STDOUT && !exists $ENV{NO_COLOR})} s/\e\[[0-9;]*m//g unless $col; (my $v=$_)=~s/\e\[[0-9;]*m//g; if(length($v)>$w){my($o,$n)=("",0); while(length && $n<$w-1){ if($col && s/^(\e\[[0-9;]*m)//){$o.=$1}else{s/^(.)//;$o.=$1;$n++} } $_=$o."…".($col?"\e[0m":"")}'; else just --list; fi

# Run the app against the repo's demo config (never touches your real ~/.config/curator config)
[group("dev")]
run:
    CURATOR_CONFIG="{{justfile_directory()}}/examples/config.toml" cargo run -p curator

# Validate a config and print the resolved window/tab tree + warnings (defaults to the demo).
[group("dev")]
validate path="examples/config.toml":
    cargo run -p curator -- validate "{{justfile_directory()}}/{{path}}"

# Run the workspace tests
[group("check")]
test:
    cargo test --workspace

# Type-check the workspace without producing binaries
[group("check")]
check:
    cargo check --workspace

# Format all sources
[group("check")]
fmt:
    cargo fmt --all

# Lint with clippy (warnings as errors)
[group("check")]
clippy:
    cargo clippy --workspace -- -D warnings

# The active-[patch] guard also runs via .githooks/pre-commit (opt-in per clone via core.hooksPath);
# fmt/clippy/tests have no hook/CI yet, so run this manually before committing/merging.
# Full pre-merge gate: fmt-check, clippy, tests, config fmt-check, active-[patch] guard
[group("check")]
gate:
    @grep -qE '^\[patch\.' Cargo.toml && { echo "✗ active [patch] in Cargo.toml — run 'just chrome-pin' / 'just config-pin' / 'just shell-pin' before committing"; exit 1; } || true
    cargo fmt --all --check
    cargo clippy --workspace -- -D warnings
    cargo test --workspace
    cargo run -p curator -- fmt --check "{{justfile_directory()}}/examples/config.toml"

# ── shared chrome-core dev loop (require the sibling ../chrome-core ghq checkout) ──

# Build curator against local ../chrome-core (uncommitted edits included): activate the [patch], `just run`
[group("chrome")]
chrome-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    [ -d ../chrome-core ] || { echo "✗ ../chrome-core not found — ghq get github.com/Lockyc/chrome-core"; exit 1; }
    tmp=$(mktemp); sed 's/^#PATCH:chrome#//' Cargo.toml > "$tmp" && mv "$tmp" Cargo.toml
    echo "✓ chrome-core → local ../chrome-core (patch active). Iterate, then: just run"
    echo "  ⚠ NEVER commit an active patch — run 'just chrome-pin' first ('just gate' will block it)."

# Re-pin chrome-core to ../chrome-core's pushed HEAD + deactivate the patch (run after pushing chrome-core)
[group("chrome")]
chrome-pin:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cc=../chrome-core
    [ -d "$cc" ] || { echo "✗ $cc not found"; exit 1; }
    [ -z "$(git -C "$cc" status --porcelain)" ] || { echo "✗ chrome-core has uncommitted changes — commit + push it first"; exit 1; }
    git -C "$cc" fetch -q origin
    rev=$(git -C "$cc" rev-parse HEAD)
    git -C "$cc" branch -r --contains "$rev" | grep -q origin/ || { echo "✗ chrome-core HEAD ($rev) isn't pushed — push it first"; exit 1; }
    dep=src-tauri/Cargo.toml
    tmp=$(mktemp); sed -E 's|(chrome-core = \{ git = "https://github.com/Lockyc/chrome-core", rev = ")[0-9a-f]+|\1'"$rev"'|' "$dep" > "$tmp" && mv "$tmp" "$dep"
    grep -qF "rev = \"$rev\"" "$dep" || { echo "✗ chrome-pin: failed to write rev into $dep — dep line shape changed, re-pin by hand"; exit 1; }
    tmp=$(mktemp); sed -E 's|^\[patch\."https://github.com/Lockyc/chrome-core"\]$|#PATCH:chrome#&|; s|^chrome-core = \{ path = "\.\./chrome-core" \}$|#PATCH:chrome#&|' Cargo.toml > "$tmp" && mv "$tmp" Cargo.toml
    ! grep -qE '^\[patch\."https://github.com/Lockyc/chrome-core"\]$' Cargo.toml || { echo "✗ chrome-pin: chrome-core [patch] still active after re-comment — check Cargo.toml"; exit 1; }
    cargo update -p chrome-core
    echo "✓ pinned chrome-core → $rev (patch deactivated). Commit src-tauri/Cargo.toml + Cargo.lock."

# Open chrome-core's visual preview loop (requires ../chrome-core checked out)
[group("chrome")]
chrome-preview:
    @[ -f ../chrome-core/justfile ] && just -f ../chrome-core/justfile preview || echo "✗ ../chrome-core not found — ghq get github.com/Lockyc/chrome-core"

# ── shared config-core dev loop (mirrors chrome-*; config-core is git-pinned in curator-config) ──

# Build curator against local ../config-core (uncommitted edits included): activate the [patch], `just run`
[group("config")]
config-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    [ -d ../config-core ] || { echo "✗ ../config-core not found — ghq get github.com/Lockyc/config-core"; exit 1; }
    tmp=$(mktemp); sed 's/^#PATCH:config#//' Cargo.toml > "$tmp" && mv "$tmp" Cargo.toml
    echo "✓ config-core → local ../config-core (patch active). Iterate, then: just run"
    echo "  ⚠ NEVER commit an active patch — run 'just config-pin' first ('just gate' will block it)."

# Re-pin config-core to ../config-core's pushed HEAD + deactivate the patch (run after pushing config-core)
[group("config")]
config-pin:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cc=../config-core
    [ -d "$cc" ] || { echo "✗ $cc not found"; exit 1; }
    [ -z "$(git -C "$cc" status --porcelain)" ] || { echo "✗ config-core has uncommitted changes — commit + push it first"; exit 1; }
    git -C "$cc" fetch -q origin
    rev=$(git -C "$cc" rev-parse HEAD)
    git -C "$cc" branch -r --contains "$rev" | grep -q origin/ || { echo "✗ config-core HEAD ($rev) isn't pushed — push it first"; exit 1; }
    dep=crates/curator-config/Cargo.toml
    tmp=$(mktemp); sed -E 's|(config-core = \{ git = "https://github.com/Lockyc/config-core", rev = ")[0-9a-f]+|\1'"$rev"'|' "$dep" > "$tmp" && mv "$tmp" "$dep"
    grep -qF "rev = \"$rev\"" "$dep" || { echo "✗ config-pin: failed to write rev into $dep — dep line shape changed, re-pin by hand"; exit 1; }
    tmp=$(mktemp); sed -E 's|^\[patch\."https://github.com/Lockyc/config-core"\]$|#PATCH:config#&|; s|^config-core = \{ path = "\.\./config-core" \}$|#PATCH:config#&|' Cargo.toml > "$tmp" && mv "$tmp" Cargo.toml
    ! grep -qE '^\[patch\."https://github.com/Lockyc/config-core"\]$' Cargo.toml || { echo "✗ config-pin: config-core [patch] still active after re-comment — check Cargo.toml"; exit 1; }
    cargo update -p config-core
    echo "✓ pinned config-core → $rev (patch deactivated). Commit crates/curator-config/Cargo.toml + Cargo.lock."

# ── shared shell-core dev loop (mirrors chrome-*; shell-core is git-pinned in src-tauri) ──

# Build curator against local ../shell-core (uncommitted edits included): activate the [patch], `just run`
[group("shell")]
shell-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    [ -d ../shell-core ] || { echo "✗ ../shell-core not found — ghq get github.com/Lockyc/shell-core"; exit 1; }
    tmp=$(mktemp); sed 's/^#PATCH:shell#//' Cargo.toml > "$tmp" && mv "$tmp" Cargo.toml
    echo "✓ shell-core → local ../shell-core (patch active). Iterate, then: just run"
    echo "  ⚠ NEVER commit an active patch — run 'just shell-pin' first ('just gate' will block it)."

# Re-pin shell-core to ../shell-core's pushed HEAD + deactivate the patch (run after pushing shell-core)
[group("shell")]
shell-pin:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    cc=../shell-core
    [ -d "$cc" ] || { echo "✗ $cc not found"; exit 1; }
    [ -z "$(git -C "$cc" status --porcelain)" ] || { echo "✗ shell-core has uncommitted changes — commit + push it first"; exit 1; }
    git -C "$cc" fetch -q origin
    rev=$(git -C "$cc" rev-parse HEAD)
    git -C "$cc" branch -r --contains "$rev" | grep -q origin/ || { echo "✗ shell-core HEAD ($rev) isn't pushed — push it first"; exit 1; }
    dep=src-tauri/Cargo.toml
    tmp=$(mktemp); sed -E 's|(shell-core = \{ git = "https://github.com/Lockyc/shell-core", rev = ")[0-9a-f]+|\1'"$rev"'|g' "$dep" > "$tmp" && mv "$tmp" "$dep"
    grep -qF "rev = \"$rev\"" "$dep" || { echo "✗ shell-pin: failed to write rev into $dep — dep line shape changed, re-pin by hand"; exit 1; }
    tmp=$(mktemp); sed -E 's|^\[patch\."https://github.com/Lockyc/shell-core"\]$|#PATCH:shell#&|; s|^shell-core = \{ path = "\.\./shell-core" \}$|#PATCH:shell#&|' Cargo.toml > "$tmp" && mv "$tmp" Cargo.toml
    ! grep -qE '^\[patch\."https://github.com/Lockyc/shell-core"\]$' Cargo.toml || { echo "✗ shell-pin: shell-core [patch] still active after re-comment — check Cargo.toml"; exit 1; }
    cargo update -p shell-core
    echo "✓ pinned shell-core → $rev (patch deactivated). Commit src-tauri/Cargo.toml + Cargo.lock."

# Re-render docs/social-preview.png (GitHub's repo social preview) from its .svg source of truth.
# docs/social-preview.svg is the source — edit that, never the .png, then run this.
# Needs librsvg (`brew install librsvg` / `apt-get install librsvg2-bin`); ImageMagick is NOT a
# substitute here — it has no rsvg delegate and fails on this file's text elements.
[group("dist")]
social-preview:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    command -v rsvg-convert >/dev/null 2>&1 || { echo "✗ rsvg-convert not found — brew install librsvg"; exit 1; }
    rsvg-convert -w 1280 -h 640 docs/social-preview.svg -o docs/social-preview.png
    echo "✓ docs/social-preview.png ← docs/social-preview.svg (1280×640, GitHub's social-preview size)"

# Build the release .app bundle (needs the Tauri CLI: `cargo install tauri-cli --version ^2`)
[group("dist")]
build:
    cd src-tauri && cargo tauri build

# Build a NOTARIZED curator.app + updater artifacts and attach them to its GitHub release
# (version from src-tauri/Cargo.toml). Run AFTER the release is tagged/pushed and
# `gh release create v<version>` published the notes (see CLAUDE.md › Releases). One command:
# build → notarize → zip → upload the app + the signed updater tarball/.sig/latest.json. Refuses
# to run without the Apple signing/notary env AND the updater key. Mirrors warden's `just release`.
[group("dist")]
release:
    bash scripts/release.sh

# Build a release .app and install/replace it in /Applications, then relaunch
[group("dist")]
deploy: build
    #!/usr/bin/env bash
    set -euo pipefail
    bash scripts/install-app.sh "target/release/bundle/macos/curator.app"
    echo "→ launching"
    open "/Applications/curator.app"
    echo "✓ curator updated in /Applications"
