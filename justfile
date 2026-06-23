# curator — task runner

# Recipes run in `sh`, which doesn't inherit cargo from an interactive fish/zsh setup.
# Guarantee rustup's bin dir is on PATH so the tauri CLI can find `cargo`.
export PATH := env_var('HOME') + "/.cargo/bin:" + env_var('PATH')

# List available recipes
default:
    @just --list

# Run the app in dev mode (hot-reload), loading the repo's demo config so iterating never touches your real config.
[group("dev")]
dev:
    CURATOR_CONFIG="{{justfile_directory()}}/examples/config.toml" npm run tauri dev

# Build the release .app bundle
[group("dev")]
build:
    npm run tauri build

# Build a release .app and install/replace it in /Applications, then relaunch
[group("dev")]
deploy: build
    #!/usr/bin/env bash
    set -euo pipefail
    bash scripts/install-app.sh "src-tauri/target/release/bundle/macos/curator.app"
    echo "→ launching"
    open "/Applications/curator.app"
    echo "✓ curator updated in /Applications"

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
