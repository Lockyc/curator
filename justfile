# curator — task runner

# Recipes run in `sh`, which doesn't inherit cargo from an interactive fish/zsh setup.
# Guarantee rustup's bin dir is on PATH so the tauri CLI can find `cargo`.
export PATH := env_var('HOME') + "/.cargo/bin:" + env_var('PATH')

# List available recipes
default:
    @just --list

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
    cd src-tauri && cargo fmt --check
    cd src-tauri && cargo clippy -- -D warnings
    cd src-tauri && cargo test
    cd src-tauri && cargo run -- fmt --check "{{justfile_directory()}}/examples/config.toml"

# Build the release .app bundle (needs the Tauri CLI: `cargo install tauri-cli --version ^2`)
[group("dist")]
build:
    cd src-tauri && cargo tauri build

# Build a release .app and install/replace it in /Applications, then relaunch
[group("dist")]
deploy: build
    #!/usr/bin/env bash
    set -euo pipefail
    bash scripts/install-app.sh "src-tauri/target/release/bundle/macos/curator.app"
    echo "→ launching"
    open "/Applications/curator.app"
    echo "✓ curator updated in /Applications"
