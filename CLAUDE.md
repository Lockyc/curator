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

`curator validate [path]` (arg-dispatched in `main.rs` before Tauri starts → `validate_cli` in
`lib.rs`) loads + validates a config and prints the resolved window/tab tree (with each tab's
cascaded session) plus any warnings. Exit 0 ok / 1 load error / 2 unknown command.

## Config schema

A window's tabs may be **loose** (`[[window.tab]]`) or grouped (`[[window.group]]` →
`[[window.group.tab]]`); a window can mix both or use only loose tabs (groups are no longer
required). `tab_views` flattens to one ordered list — loose tabs first as a headerless section
(`TabView.group = None`), then each group in file order (`Some(name)`) — and the chrome renders a
section header only for `Some`. Per-tab fields: `title`, `url` (both required, non-empty),
`load_on_open` (bool, default false), `reload_every` (minutes, must be > 0 if set), `session`.

Validation (`parse_and_validate`, last-good-on-failure) **errors** on: empty window title, dup
window title, zero window dimension, invalid colour, empty group name, dup group name within a
window, dup tab title window-wide (across loose + grouped), empty tab title/url, unparseable url,
zero `reload_every`. It also returns a **warnings** channel (`Vec<Warning>`, non-fatal) — first
producer: a URL repeated within a window (the URL-hash labels still disambiguate, so it loads but
warns). `parse_and_validate`/`load_config` return `(Config, Vec<Warning>)`; warnings are
`eprintln`'d on load/hot-reload and printed by `curator validate`. Tab identity stays the
URL-hash label (`url_label`), not the title — titles are display labels (hence the new title
uniqueness, which removes the silent `open_on_launch` first-match ambiguity).

## Architecture

**Multi-window.** curator opens one `NSWindow` per `[[window]]` block in `config.toml`.
Each window has a `window_id` (derived from `title` via `identity::window_id`) that namespaces
its webview labels (`{window_id}:{tab_hash}`) so they're collision-free across windows. The
window id is purely a label key — run-ephemeral, nothing persistent tied to it — so renaming a
window's `title` is harmless.

**Sessions (logins) are decoupled from windows.** A tab's WebKit data store is keyed on a
resolved `session` string via the full cascade
`tab.session → window.session → top-level Config.session → DEFAULT_SESSION`
(`config.rs` builds `TabView::session` in `tab_views(global_session)`; `session::data_store_id`
hashes it). An explicit `session = ""` at any level is treated as unset and falls through to the
next. Tabs sharing a session string share a login (even across windows); distinct strings are
isolated accounts. With no `session` set anywhere, all tabs share one app-wide store, so SSO
across related services (e.g. Gmail + Calendar) works. Because sessions key off `session` — not
the window or URL — renaming a window or editing a tab's URL never logs you out. (The top-level
`Config.session` is captured per window in `WindowRuntime.global_session` so commands and
menu-reopen re-resolve the chain without the whole `Config`.)

**Sentinels are key-gated.** The notification/badge/escape shims signal the Rust side by
navigating to dead sentinel hosts (`curator.*.invalid`) that `on_navigation` intercepts — no
command/IPC is exposed to remote pages. Each content webview gets a random per-load key,
substituted into its shims at injection (a function-local literal, never on `window`) and
required on every sentinel URL (`&k=`); `on_navigation` rejects any sentinel without it, so a
page can't forge a banner/badge/browser-escape by hitting the host directly. Any new
sentinel-emitting shim must carry the `__CURATOR_KEY__` placeholder.

**Loading is driven solely by per-tab `load_on_open`** — there are no per-window mode flags.
Every content webview gets the full shim set (escape-click, visibility, notification, badge),
so any *loaded* tab can fire native banners and report unread. It also gets a `tauri-guard`
shim, injected first: `withGlobalTauri` leaks the notification plugin's guest init into remote
pages, which eagerly probes `plugin:notification|is_permission_granted` over IPC — denied by ACL
for content webviews — and the uncaught rejection trips a page's own error handler (Forgejo's
crashes on it). The guard swallows exactly that rejection (capture phase, scoped to the command).
`load_on_open` tabs are created
at launch and kept live (never hidden — `apply_active` in `webviews.rs` shows them behind the
active tab), so they keep syncing and notify in the background. Tabs without `load_on_open` are
lazy (created on first click) and hidden when inactive (throttled → no background notifications,
by choice — same as unloading). `apply_active` is the single switch primitive: show+raise the
active tab, keep `load_on_open` tabs shown, hide the rest.

**Dock badge** aggregates the unread count across every window's loaded tabs.

**Window menu** — a real **Window** submenu lets the user close a non-last window (⌘W)
and reopen any closed window from the list. All configured windows open at launch; closed
windows can be reopened from the Window menu. Both ⌘W and the native red button flow through
`on_real_window_close`, which refuses to close the last window (no stranding) and, on close,
wipes the window's unread/timers while keeping its `WindowRuntime` registered so its cfg
survives for reopen. Config-reload window removal uses `destroy()` to bypass that guard.

**App menu.** `lib.rs` fully replaces Tauri's default menu, so standard macOS menus must
be re-added by hand. The **Edit** submenu is load-bearing: its predefined items own the
clipboard accelerators (⌘C/⌘V/⌘X/⌘A/⌘Z), so dropping it silently breaks paste in content
webviews. Keep Edit (and Window/Hide) when touching the menu.

The **Tabs** submenu also carries keyboard tab navigation: **⌘1–9** jump to a tab position and
**⌘⇧]** / **⌘⇧[** cycle next/previous. The handlers `emit_to_focused_chrome` a `nav-tab` /
`jump-tab` event; the focused window's chrome resolves the target row and routes it through the
normal `select()` path (so a lazy tab still creates on demand). The submenu's "Open Developer
Tools" (⌥⌘I) opens the WebKit inspector on the focused window's active content tab. It works in
release builds because `tauri`'s `devtools` feature is enabled in `Cargo.toml` — that's
deliberate (this is an operator console, not a sandboxed consumer app), not a debug leftover;
don't strip the feature.

**Resizable sidebar.** The sidebar is a fixed-width child webview (default `CHROME_W`) with the
content webviews Rust-positioned beside it, so width can't be pure CSS. Each window's width lives
in a `WindowRuntime.chrome_w` (`Arc<AtomicU64>`, f64 bits) shared with its resize closure; a
right-edge drag in the chrome invokes `set_sidebar_width`, which clamps Rust-side (`clamp_chrome_w`:
160–520px and ≤40% of the window) and `relayout_with_width`s the chrome + content. The window
re-clamps on resize; the chosen width persists in `localStorage` per window title. The active-tab
highlight tints with the window's accent colour (`--active-bg`), falling back to neutral blue.

## Non-goals / parked: browser extensions (Bitwarden etc.)

curator does **not** support browser/Safari web extensions, and there is no password-manager
integration. This was evaluated for Bitwarden (2026-06-23) and parked — do not build it without
the owner reopening it.

- The capability now exists in WebKit: macOS 15.4+ ships `WKWebExtension` /
  `WKWebExtensionContext` / `WKWebExtensionController`, which let a third-party WebKit app load
  Safari web extensions (incl. unpacked from disk). Bitwarden ships a Safari web extension, so
  hosting the *real* extension is theoretically possible.
- **The blocker is the toolchain, not WebKit.** `webExtensionController` must be set on the
  `WKWebViewConfiguration` *before* the `WKWebView` is created (config is immutable after), and
  neither wry (`0.55.1`) nor Tauri exposes a hook to set it at that point. Tauri #13200 (the
  generic "customize WKWebViewConfiguration" request) is open with no PR. You can reach the live
  webview post-creation via `objc2`, but that's too late for this property. The only path is a
  **wry fork** that attaches the controller to its internally-built config (or landing #13200
  with that property).
- A CLI-backed autofill shim (`bw serve` + an inject script) was considered as a fallback and
  **rejected** — the owner will not ship credential autofill via that hack, and will not proceed
  on extensions at all until real extension support is leverageable, nor build the wry fork now.
  Don't propose the CLI route again as the answer.

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

## Installer & the public repo

`install.sh` and the `/curator:install` command install curator by `git clone`-ing
`~/.curator` from `https://github.com/Lockyc/curator` and building from source; the
README's `curl` one-liner fetches `install.sh` over `raw.githubusercontent.com`. The repo
is public, so the raw fetch and the unauthenticated `git clone` both work.

Because the repo is public, **every tracked file must be self-contained** — no references
to machine-local paths, scripts, or personal tooling. Clones, the build-from-source path,
and any CI only have what's in the tree.

## Deferred work

- **Format-on-save (port from warden):** the sibling app warden formats its TOML config in
  place on a clean hot-reload (opt-in via a `format_on_save` key, default false) and via a
  `warden fmt [--check]` CLI. It embeds the `taplo` crate as a library with a fixed house
  style (`indent_tables`, `indent_entries`, `align_entries`, `align_comments`,
  `reorder_keys=false`) and rewrites the file diff-guarded (only if bytes change) + atomically
  (temp file + rename), so the writer can't loop its own watcher; it only ever rewrites on a
  clean parse. curator has the same watcher + config shape, and now also a CLI dispatch in
  `main.rs` (`curator validate`), so a `curator fmt [--check]` subcommand could mirror warden's
  alongside a reload-path formatter. **Not implemented yet.**
