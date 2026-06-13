# curator — agent notes

macOS-only Tauri v2 app (Rust + a static web frontend in `src/`, driven via npm).

Dev: `just dev`. Build a release `.app`: `just build`. Install/replace it in
`/Applications` and relaunch: `just deploy`. Tests: `just test` (or `cd src-tauri &&
cargo test`). There is no CI — the release gate is running `just fmt`, `just clippy`,
and `just test` locally and confirming all are green before tagging a release.

## Installer & the public-repo requirement

`install.sh` and the `/curator:install` command install curator by `git clone`-ing
`~/.curator` from `https://github.com/Lockyc/curator` and building from source; the
README's `curl` one-liner fetches `install.sh` from that repo over
`raw.githubusercontent.com`. **These public URLs — the raw fetch and the unauthenticated
`git clone` — only work once the GitHub repo is public.** This is known and intentional.

**Do not prompt the owner about making the repo public.** They are fully aware of the
requirement and will flip the repo when they are ready. Do not raise it as a TODO,
caveat, blocker, or question; do not re-explain the public-repo dependency back to them.
Document it for other agents (as here) and move on.
