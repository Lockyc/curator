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
    cd src-tauri && CURATOR_CONFIG="{{justfile_directory()}}/examples/config.toml" cargo tauri dev

# Validate a config and print the resolved window/tab tree + warnings (defaults to the demo).
[group("dev")]
validate path="examples/config.toml":
    cd src-tauri && cargo run -- validate "{{justfile_directory()}}/{{path}}"

# Run the Rust unit tests
[group("check")]
test:
    cd src-tauri && cargo test

# Type-check without producing a binary
[group("check")]
check:
    cd src-tauri && cargo check

# Format Rust sources
[group("check")]
fmt:
    cd src-tauri && cargo fmt

# Lint with clippy (warnings as errors)
[group("check")]
clippy:
    cd src-tauri && cargo clippy -- -D warnings

# Full pre-merge gate: format check (non-mutating), clippy, tests, config-format check.
[group("check")]
gate:
    @grep -qE '^\[patch\.' src-tauri/Cargo.toml && { echo "✗ active [patch] in src-tauri/Cargo.toml — run 'just chrome-pin' before committing"; exit 1; } || true
    cd src-tauri && cargo fmt --check
    cd src-tauri && cargo clippy -- -D warnings
    cd src-tauri && cargo test
    cd src-tauri && cargo run -- fmt --check "{{justfile_directory()}}/examples/config.toml"

# ── shared chrome-core dev loop (require the sibling ../chrome-core ghq checkout) ──

# Build curator against local ../chrome-core (uncommitted edits included): activate the [patch], `just run`
[group("chrome")]
chrome-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    [ -d ../chrome-core ] || { echo "✗ ../chrome-core not found — ghq get github.com/Lockyc/chrome-core"; exit 1; }
    tmp=$(mktemp); sed 's/^#PATCH#//' src-tauri/Cargo.toml > "$tmp" && mv "$tmp" src-tauri/Cargo.toml
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
    tmp=$(mktemp); sed -E 's|^\[patch\."https://github.com/Lockyc/chrome-core"\]$|#PATCH#&|; s|^chrome-core = \{ path = "\.\./\.\./chrome-core" \}$|#PATCH#&|' "$dep" > "$tmp" && mv "$tmp" "$dep"
    cd src-tauri && cargo update -p chrome-core
    echo "✓ pinned chrome-core → $rev (patch deactivated). Commit src-tauri/Cargo.toml + src-tauri/Cargo.lock."

# Open chrome-core's visual preview loop (requires ../chrome-core checked out)
[group("chrome")]
chrome-preview:
    @[ -f ../chrome-core/justfile ] && just -f ../chrome-core/justfile preview || echo "✗ ../chrome-core not found — ghq get github.com/Lockyc/chrome-core"

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
    bash scripts/install-app.sh "src-tauri/target/release/bundle/macos/curator.app"
    echo "→ launching"
    open "/Applications/curator.app"
    echo "✓ curator updated in /Applications"
