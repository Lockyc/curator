# curator — task runner

# List available recipes
default:
    @just --list

# Run the app in dev mode (hot-reload)
dev:
    npm run tauri dev

# Build the release .app bundle
build:
    npm run tauri build

# Run the Rust unit tests
test:
    cd src-tauri && cargo test

# Type-check without producing a binary
check:
    cd src-tauri && cargo check

# Format Rust sources
fmt:
    cd src-tauri && cargo fmt

# Lint with clippy (warnings as errors)
clippy:
    cd src-tauri && cargo clippy -- -D warnings
