# curator — agent notes

macOS-only Tauri v2 app (Rust + a static web frontend in `src/`, driven via npm).

Built as the operator-side console for a self-hosted homelab.

Dev: `just dev`. Build a release `.app`: `just build`. Install/replace it in
`/Applications` and relaunch: `just deploy`. Tests: `just test` (or `cd src-tauri &&
cargo test`). There is no CI — the release gate is running `just fmt`, `just clippy`,
and `just test` locally and confirming all are green before tagging a release.

The launch config path is `$CURATOR_CONFIG` if set, else `~/.config/curator/config.toml`
(`config::resolve_config_path`). `just dev` sets `CURATOR_CONFIG` to the repo's
`examples/config.toml` so dev runs never touch a real user config.

## Architecture

**Multi-window.** curator opens one `NSWindow` per `[[window]]` block in `config.toml`.
Each window has:

- Its own WebKit profile, giving per-window session isolation: the same service in two
  windows gets two independent logins (two sets of cookies/storage).
- A `window_id` (derived from `title` via `identity::window_id`) that namespaces webview
  labels (`{window_id}:{tab_hash}`) and the per-window session directory, keeping them
  stable and collision-free across windows.

**Live vs plain.** A window that sets `notifications = true` or `unread = true` is "live":
it eager-loads all its tabs from launch, never hides, and has the notify/badge shims
injected. A plain window keeps the lazy/hide model (webview created on first click, hidden
when the window isn't active). `WindowConfig::is_live()` in `config.rs` captures this.

**Dock badge** aggregates the unread count across all windows that have `unread = true`.

**Window menu** — a real **Window** submenu lets the user close a non-last window (⌘W)
and reopen any closed window from the list. All configured windows open at launch; closed
windows can be reopened from the Window menu.

**App menu.** `lib.rs` fully replaces Tauri's default menu, so standard macOS menus must
be re-added by hand. The **Edit** submenu is load-bearing: its predefined items own the
clipboard accelerators (⌘C/⌘V/⌘X/⌘A/⌘Z), so dropping it silently breaks paste in content
webviews. Keep Edit (and Window/Hide) when touching the menu.

## Releases

Every release gets a matching GitHub release — don't just push `main`. To cut one:

1. Bump the version in **both** `src-tauri/Cargo.toml` and `package.json` (keep them in
   sync) and commit it.
2. Confirm the release gate is green: `just fmt`, `just clippy`, `just test`.
3. Tag the release commit `v<version>` (matching the bumped version) and publish:
   `gh release create v<version> --target main --title v<version> --notes "<changelog>"`,
   where the notes summarise what shipped since the previous release.
4. There is no CI to build artifacts, so attach the locally built app: `just build`,
   then zip the `.app` and `gh release upload v<version> <app>.zip` so the release
   carries an installable binary (the `install.sh` path still builds from source).

This is part of cutting a release, not a follow-up; do it without being asked.

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
