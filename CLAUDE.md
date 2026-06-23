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
Each window has a `window_id` (derived from `title` via `identity::window_id`) that namespaces
its webview labels (`{window_id}:{tab_hash}`) so they're collision-free across windows. The
window id is purely a label key — run-ephemeral, nothing persistent tied to it — so renaming a
window's `title` is harmless.

**Sessions (logins) are decoupled from windows.** A tab's WebKit data store is keyed on a
resolved `session` string via the chain `tab.session → window.session → DEFAULT_SESSION`
(`config.rs` builds `TabView::session`; `session::data_store_id` hashes it). Tabs sharing a
session string share a login (even across windows); distinct strings are isolated accounts.
With no `session` set anywhere, all tabs share one app-wide store, so SSO across related
services (e.g. Gmail + Calendar) works. Because sessions key off `session` — not the window or
URL — renaming a window or editing a tab's URL never logs you out.

**Loading is driven solely by per-tab `always_load`** — there are no per-window mode flags.
Every content webview gets the full shim set (escape-click, visibility, notification, badge),
so any *loaded* tab can fire native banners and report unread. `always_load` tabs are created
at launch and kept live (never hidden — `apply_active` in `webviews.rs` shows them behind the
active tab), so they keep syncing and notify in the background. Tabs without `always_load` are
lazy (created on first click) and hidden when inactive (throttled → no background notifications,
by choice — same as unloading). `apply_active` is the single switch primitive: show+raise the
active tab, keep `always_load` tabs shown, hide the rest.

**Dock badge** aggregates the unread count across every window's loaded tabs.

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
