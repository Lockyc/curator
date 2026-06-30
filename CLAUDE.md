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

`curator fmt [--check] [path]` (same dispatch → `fmt_cli`) reformats the config in the shared
house style (`config_core::format_file`, atomic + diff-guarded): without `--check` it rewrites in
place and prints what it did; with `--check` it writes nothing and exits 1 if the file would
change (for pre-commit/CI). Exit 0 ok / 1 read or TOML error.

## Config schema

A window's tabs may be **loose** (`[[window.tab]]`) or grouped (`[[window.group]]` →
`[[window.group.tab]]`); a window can mix both or use only loose tabs (groups are no longer
required). `tab_views` flattens to one ordered list — loose tabs first as a headerless section
(`TabView.group = None`), then each group in file order (`Some(name)`) — and the chrome renders a
section header only for `Some`. Per-tab fields: `title`, `url` (both required, non-empty),
`load_on_open` (bool, default false), `reload_every` (minutes, must be > 0 if set), `session`.
App-global keys: `dark_mode`, `allow_insecure`, `session`, and `format_on_save` (bool, default
false — reformat the file in house style on a clean hot-reload; see Deferred work → done below).
Accent-colour validation delegates to `config_core::Colour::parse` (shared with warden).

Validation (`parse_and_validate`, last-good-on-failure) **errors** on: empty window title, dup
window title, zero window dimension, invalid colour, empty group name, dup group name within a
window, dup tab title window-wide (across loose + grouped), empty tab title/url, unparseable url,
zero `reload_every`, and any **unrecognized key** on `Config`/`WindowConfig`/`Group`/`Tab` (all
`#[serde(deny_unknown_fields)]`) — so a removed/renamed key such as the old `always_load` (now
`load_on_open`) or a plain typo fails loudly rather than being silently ignored. It also returns a
**warnings** channel (`Vec<Warning>`, non-fatal) — first
producer: a URL repeated within a window (the URL-hash labels still disambiguate, so it loads but
warns). `parse_and_validate`/`load_config` return `(Config, Vec<Warning>)`; warnings are
`eprintln`'d on load/hot-reload and printed by `curator validate`. Tab identity stays the
URL-hash label (`url_label`), not the title — titles are display labels (hence the new title
uniqueness, which removes the silent `open_on_launch` first-match ambiguity).

Tabs can be added to an existing window via the `add_tab` Tauri command (see Architecture below)
without hand-editing the file. The command appends via `config_core::add_tab(path, window_title,
group, fields)`, where `group: Option<&str>` is the seam for placing a new tab into an existing
group or the loose section. **Creating a new group from the UI is not yet built** — the group
picker in the `+ Add tab` form offers only existing groups plus the loose section; new groups must
be added by editing `config.toml` directly.

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

**Native banners go through `UNUserNotificationCenter`, not `tauri-plugin-notification`**
(`notification.rs`, objc2). The plugin's desktop backend (`notify-rust` → `mac-notification-sys`)
posts via the deprecated `NSUserNotification` API, which is a **silent no-op on macOS 26** —
`show()` returns `Ok`, nothing is delivered, and the app never registers in Notification Center.
`notification::init` (called once in the Tauri setup hook) requests authorization and installs a
`UNUserNotificationCenterDelegate` whose `willPresentNotification` returns `.banner`, so banners
show even while curator is the frontmost app (the hidden-tab-in-focused-window case). It's
gated on `!tauri::is_dev()` — `currentNotificationCenter` throws on a nil bundle id, so native
banners only fire from the packaged `curator.app` (dev still badges). Don't reintroduce the
plugin for banners. Because the plugin is gone, `withGlobalTauri` no longer injects a notification
guest that probes `plugin:notification|is_permission_granted` over IPC, so content webviews need
no `tauri-guard` shim anymore (it was removed with the plugin).

**Loading is driven solely by per-tab `load_on_open`** — there are no per-window mode flags.
Every content webview gets the full shim set (escape-click, visibility, notification, badge),
so any *loaded* tab can fire native banners and report unread. `load_on_open` tabs are created
at launch and kept live (never hidden — `apply_active` in `webviews.rs` shows them behind the
active tab), so they keep syncing and notify in the background. Tabs without `load_on_open` are
lazy (created on first click) and hidden when inactive (throttled → no background notifications,
by choice — same as unloading). `apply_active` is the single switch primitive: show+raise the
active tab, keep `load_on_open` tabs shown, hide the rest.

**Dock badge** aggregates the unread count across every window's loaded tabs.

**Window menu** — a real **Window** submenu lets the user close a window (⌘W) and reopen any
closed window from the list. All configured windows open at launch; closed windows can be
reopened from the Window menu while the app is still running. Both ⌘W and the native red button
flow through `on_real_window_close`, which wipes the closed window's unread/timers while keeping
its `WindowRuntime` registered so its cfg survives for reopen — and **quits curator when the last
window closes** (last-window-quit, matching warden), rather than lingering as a menu-bar-only app.
Config-reload window removal uses `destroy()` (not the user-close path), so a reload that drops a
window doesn't trip last-window-quit mid-reconcile.

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

**Adding tabs from the UI.** The sidebar's always-visible **`+ Add tab`** button expands an
inline form (title, URL, optional group picker, and an Advanced section for `load_on_open`,
`reload_every`, `session`). On submit, the chrome invokes the `add_tab` Tauri command
(`commands.rs`). The command: (1) snapshots the window's current config under the `windows` lock
and then drops the lock; (2) builds a candidate `WindowConfig` with the new tab appended to the
target group (or loose section if `group` is `None`/empty); (3) wraps it in a single-window
`Config` and calls `config::validate_config` — the same full validation as load-time; (4) only on
success writes the new tab to the config file via `config_core::add_tab`. The existing file-watcher
reload then re-renders the sidebar — there is no separate render path. Any validation error is
returned as a string and shown inline in the form. New-group creation from the UI is not supported;
adding a tab to a group that doesn't yet exist returns an error.

**Resizable sidebar.** The sidebar is a fixed-width child webview (default `CHROME_W`) with the
content webviews Rust-positioned beside it, so width can't be pure CSS. Each window's width lives
in a `WindowRuntime.chrome_w` (`Arc<AtomicU64>`, f64 bits) shared with its resize closure; a
right-edge drag in the chrome invokes `set_sidebar_width`, which clamps Rust-side (`clamp_chrome_w`:
160–520px and ≤40% of the window) and `relayout_with_width`s the chrome + content. The window
re-clamps on resize; the chosen width persists in `localStorage` per window title. The active-tab
highlight tints with the window's accent colour (`--active-bg`), falling back to neutral blue.

**Footgun — the `AppState.windows` mutex is the only lock, and commands must stay synchronous.**
Several `#[tauri::command]`s (`select_tab`, `reset_window_tabs`, …) hold the `windows` lock across
webview ops (`add_child`/show/hide/raise/navigate). That's deadlock-free *only* because sync Tauri
commands run on the main thread, so those ops execute inline and the watcher thread (which always
drops the lock before marshaling a webview op to main) can't deadlock against them. **Do not make
any command that holds `windows` `async`** — it would then run off-main, hold the lock, and block
on a main-thread-marshaled webview op while a `windows`-locking main-thread callback waits, which
deadlocks. If a command must become async, drop the `windows` guard before any webview op. (This
is the class the "sidebar-width re-lock" fix closed by passing `chrome_w` by value instead of
re-locking inside `create_content_webview`.)

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

`just build` **signs with Developer ID and notarizes + staples automatically** when the
Apple signing/notary env vars are present in the build environment — `APPLE_SIGNING_IDENTITY`
pointing at a Developer ID Application cert, plus `APPLE_ID`/`APPLE_PASSWORD`/`APPLE_TEAM_ID`
(or `APPLE_API_KEY`/`APPLE_API_ISSUER`/`APPLE_API_KEY_PATH`); Tauri runs the signing and
`notarytool` submission (with hardened runtime) during the bundle step. This is env-driven, not
pinned in `tauri.conf.json`, so a build environment without those vars silently ships an
ad-hoc/unsigned bundle that other Macs block under Gatekeeper — set them before cutting a release
meant for distribution. Verify a release artifact with `spctl -a -vvv <app>` (expect `source=Notarized
Developer ID`) and `xcrun stapler validate <app>`.

## Installer & the public repo

`install.sh` and the `/curator:install` command install curator by `git clone`-ing
`~/.curator` from `https://github.com/Lockyc/curator` and building from source; the
README's `curl` one-liner fetches `install.sh` over `raw.githubusercontent.com`. The repo
is public, so the raw fetch and the unauthenticated `git clone` both work.

Because the repo is public, **every tracked file must be self-contained** — no references
to machine-local paths, scripts, or personal tooling. Clones, the build-from-source path,
and any CI only have what's in the tree.

## Shared config primitives: `config-core`

curator and warden share the same `window → group → tab` config shape and house style, so the
domain-free primitives live in the **`config-core`** crate
(`https://github.com/Lockyc/config-core`) — a git dependency (cargo fetches it at build, so the
build-from-source install needs no extra setup). curator uses two pieces:

- **`fmt`** — the house-style formatter behind `curator fmt` and format-on-save. `format_file`
  is atomic, diff-guarded, and symlink/mode-preserving, and a no-op on an already-formatted file,
  so the reload-path formatter (run on a clean hot-reload when the new config sets
  `format_on_save = true`) can't loop its own watcher.
- **`colour`** — `Colour::parse` backs `is_hex_colour` (per-window accent validation).

Both curator and warden consume `config-core` (warden is where `fmt`/`colour` originated, since
retrofitted onto the shared crate — there's one implementation, not a copy per app). The crate is
deliberately leaf-free: each app keeps its own model, validation, and session/shell cascade. Only
add to `config-core` what is genuinely identical and leaf-agnostic in *both* apps — don't grow it
into a generic config framework.
