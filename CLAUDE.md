# curator — agent notes

macOS-only Tauri v2 app (Rust + a static web frontend in `src/`). Build is pure cargo / `cargo tauri`.

Built as the operator-side console for a self-hosted homelab.

Dev: `just run`. Build a release `.app`: `just build`. Install/replace it in
`/Applications` and relaunch: `just deploy`. Tests: `just test` (or `cd src-tauri &&
cargo test`). There is no CI — run `just gate` locally (format check, clippy, tests) and
confirm it is green before tagging a release.

The launch config path is `$CURATOR_CONFIG` if set, else `~/.config/curator/config.toml`
(`config::resolve_config_path`). `just run` sets `CURATOR_CONFIG` to the repo's
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
App-global keys: `dark_mode`, `allow_insecure`, `session`, `format_on_save` (bool, default
false — reformat the file in house style on a clean hot-reload),
`density` (`comfortable` default / `compact`), `sidebar_drag` (bool, default true — the sidebar
chrome is a window-move drag handle → the component's `windowDrag` flag; `false` turns it off), and
`auto_update` (bool, default true — check for a new release on launch; `false` suppresses the
automatic check, the **Check for Updates…** menu item still works — see *In-app updates*).
These are kept live in `AppState` across hot-reload (like `dark_mode`; `sidebar_drag`/`auto_update`
are `AtomicBool`, `density` a `Mutex`); `window_identity` returns them (plus a density-aware default
sidebar width — compact is narrower) and the controller passes them in the DTO, so **chrome-core**
applies `windowDrag` and sets
`data-density` on `<html>` and swaps its `--cc-*` sizing tokens (`--cc-row-font`/`--cc-tile-size`/
`--cc-dot-size`/… in chrome-core's `assets/sidebar.css`). `compact` is a proportional ~0.85× scale
of the comfortable set. **Both apps consume chrome-core**, so the tokens live once and stay aligned
by construction. Accent-colour validation delegates to `config_core::Colour::parse` (shared with warden).

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

## Architecture

**Multi-window.** curator opens one `NSWindow` per `[[window]]` block in `config.toml`.
Each window has a `window_id` (derived from `title` via `identity::window_id`) that is the Tauri
window label *and* the label of its **main webview** (the chrome sidebar — see *Resizable
sidebar*). It also namespaces the content webviews' labels (`{window_id}:{tab_hash}`) so they're
collision-free across windows, and distinct from the chrome's bare `window_id` label (the basis of
the `require_chrome` gate). The window id is purely a label key — run-ephemeral, nothing persistent
tied to it — so renaming a window's `title` is harmless.

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
navigating to dead sentinel hosts (`curator.*.invalid`) that `on_navigation` intercepts — the
sentinel path itself exposes no command/IPC. Each content webview gets a random per-load key,
substituted into its shims at injection (a function-local literal, never on `window`) and
required on every sentinel URL (`&k=`); `on_navigation` rejects any sentinel without it, so a
page can't forge a banner/badge/browser-escape by hitting the host directly. Any new
sentinel-emitting shim must carry the `__CURATOR_KEY__` placeholder.

**Chrome CSP.** `tauri.conf.json`'s `app.security.csp` locks down the chrome (App-URL) webview:
`default-src 'self'`, `script-src`/`style-src 'self' 'unsafe-inline'`, `img-src 'self' data:`,
`connect-src 'self' ipc: http://ipc.localhost` (the Tauri IPC channel), `frame-src 'none'`,
`object-src 'none'`. It applies to local app pages only, so remote content tabs (`External` URLs)
are unaffected — this hardens the sidebar, not the tabs. Editing the chrome to pull an external
script/style/font, open an iframe, or `fetch` a non-IPC origin will silently fail the CSP; widen
the directive here rather than working around it.

**Commands are chrome-gated — `withGlobalTauri` does inject the IPC bridge into content
webviews.** `tauri.conf.json` sets `withGlobalTauri: true`, so `window.__TAURI__` (and the
underlying invoke channel) reaches *every* webview, remote content tabs included — and
app-defined `#[tauri::command]`s are **not** ACL/capability-gated (the `core:event` capability in
`capabilities/default.json` is a separate concern — see below; the commands work regardless).
The only thing stopping a remote page from invoking the whole command surface (reading sibling
tabs' URLs via `get_tabs`, driving `select_tab`/`unload_tab`/`reset_all`/`set_hole_rect`) is
the `require_chrome`/`is_chrome_caller` guard at the top of every command in `commands.rs`. The
chrome is its window's **main** webview (hole-punch — see *Resizable sidebar*), so its label *is*
the window label; content webviews are `{window_id}:tab-<hash>`, so the guard is `label ==
webview.window().label()` (`label_is_chrome`; the same check `layout_webviews` uses to skip the
chrome). **Every new `#[tauri::command]` that takes a `Webview` must call `require_chrome` first**
(or, for the non-`Result` ones, early-return a safe empty value) — dropping the guard silently
re-exposes the command to remote pages.

Separately, the chrome sidebar *does* need real capability permissions: `core:event` (JS `listen()`)
and `core:window:allow-start-dragging` (the sidebar window-move drag — see *Hole-punch layout*). The
`capabilities/default.json` grant applies to **local** app-URL webviews only (no `remote` block =
`local: true` default), and Tauri denies capability permissions to a webview by its live origin —
so the chrome (an App URL) gets these while content tabs (`External`/remote URLs) never do,
regardless of label. That local/remote origin scoping — not a label glob — is what keeps these
permissions off remote pages now that the chrome's label is the bare window id.

**Native banners go through `UNUserNotificationCenter`, not `tauri-plugin-notification`**
(`notification.rs`, objc2). The plugin's desktop backend (`notify-rust` → `mac-notification-sys`)
posts via the deprecated `NSUserNotification` API, which is a **silent no-op on macOS 26** —
`show()` returns `Ok`, nothing is delivered, and the app never registers in Notification Center.
`notification::init` (called once in the Tauri setup hook) requests authorization and installs a
`UNUserNotificationCenterDelegate` whose `willPresentNotification` returns `.banner | .sound`, so
a banner (with sound) shows even while curator is the frontmost app (the hidden-tab-in-focused-window
case). It's
gated on `!tauri::is_dev()` — `currentNotificationCenter` throws on a nil bundle id, so native
banners only fire from the packaged `curator.app` (dev still badges). Don't reintroduce the
plugin for banners. Because the plugin is gone, `withGlobalTauri` no longer injects a notification
guest that probes `plugin:notification|is_permission_granted` over IPC, so content webviews need
no `tauri-guard` shim anymore (it was removed with the plugin).

Banners are **click-routable**: `fire` takes the originating `(window_id, tab label)` (threaded
from the `on_navigation` notify-sentinel handler in `webviews.rs`, which knows both) and stamps
them into the request's `userInfo`; the same delegate's `didReceiveNotificationResponse` reads
them back on a tap (the *default* action only — dismiss is ignored), raises that window
(`set_focus`, which also activates curator from the background), and emits `focus-tab` to that
window's chrome (the chrome is the window's main webview, so its label *is* the `window_id`) so the
sidebar selects the tab. `init` captures the `AppHandle` the delegate needs. The event targets the
precise chrome webview label, so it reaches only the originating window (no per-window leak — unlike
warden, whose `emit_to` leaks to siblings and so carries a label to filter). This surfaces curator's *own* tab; it does **not** invoke the
web page's `Notification.onclick` (the injected stub's JS handlers stay inert — see
`src/inject/notification.js`).

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

**Hole-punch layout + resizable sidebar.** The chrome is the window's **main** webview
(`build_window` uses `WebviewWindowBuilder`, `hidden_title`, full-window under `TitleBarStyle::Overlay`),
*not* an `add_child` child — because `data-tauri-drag-region` moves the window only from a window's
main webview (a child webview's drag is inert). **Two things are both required for the drag; each is
a silent no-op alone**: (1) the chrome must be the main webview, as here; and (2)
`capabilities/default.json` must grant `core:window:allow-start-dragging`.
The drag region invokes `plugin:window|start_dragging`, and **plugin** commands *are* ACL-gated
(unlike curator's own app commands, which aren't — see the gating section) — without that permission
the invoke is denied and nothing moves, with no error surfaced. Because the full-window webview now
covers the native title-bar strip, this drag path is also the *only* way to move the window (there
is no native-titlebar fallback), so the permission is load-bearing, not a nicety.
This mirrors **warden's hole-punch**: `index.html` is a flex row of a fixed-width `#sidebar` (the
chrome-core mount) and a `#content-hole`; the Rust-positioned content webviews are `add_child`
siblings that composite **above** the main chrome webview over the hole (guaranteed by add-order +
`zorder::raise_to_front` on the active tab). When no content webview covers the hole, the opaque
`#empty-state` (muted curator mark, in `index.html`) shows; `chrome.js` toggles it on `active`. No
transparency needed (content is opaque; unlike warden's native surfaces). The traffic-light inset is
CSS (`#sidebar { padding-top: 28px }`), owned by the frontend — Rust does not offset the chrome.

The sidebar's **visible width is JS-owned CSS** (`#sidebar` inline width, set by `chrome.js`'s
`onResize`); the content webviews' geometry is **whatever the chrome reports**, not something Rust
recomputes from a width. `chrome.js` measures `#content-hole`'s `getBoundingClientRect` and sends it
via the **`set_hole_rect`** command — on mount, on a resize-drag, and on window resize, all driven by
a `ResizeObserver` on `#content-hole` (plus the `resize` handler). Rust stores the rect on
`WindowRuntime.hole` and positions every content webview to it (`webviews::layout_webviews`), so a
lazily-created or hot-reload-added tab lands in the current hole (read under the `windows` lock and
passed into `create_content_webview` by value). This is exactly **warden's `set_hole_rect` model** —
the sole difference is curator needs **no Y-flip** (Tauri's `LogicalPosition` is top-left; warden
flips for its bottom-left native `NSView`). **chrome-core is the *only* sidebar-width clamp**
(`MIN_W`/`MAX_W`/`MAX_FRACTION` in `chrome.js` → the component's `minWidth`/`maxWidth`/`maxFraction`,
160–520px, ≤40% of the window); because Rust just applies the reported hole there is **no Rust-side
clamp to keep identical**. The old `clamp_chrome_w`/`relayout_with_width`/`set_sidebar_width` command
and the `WindowRuntime.chrome_w` atomic were **removed** when curator converged onto warden's
report-the-rect model — don't reintroduce a Rust-side width or clamp (that was the whole
JS↔Rust-duplication this convergence deleted). The window-shrink re-clamp lives in `chrome.js`'s
`resize` handler (a shrink can push the sidebar past the 40% cap without a drag), which then
re-reports the hole; there is **no Rust-side resize relayout** (`build_window` wires only the
user-close handler now — JS drives resize, matching warden, which also drops restore-on-window-grow).
chrome-core owns the drag handle + `localStorage` persistence
(`storageKey: "curator:sidebar-width:<title>"`). The active-tab highlight tints with the window's
accent colour (`--active-bg`), falling back to neutral blue.

**Window size + position persist across launches** via `tauri-plugin-window-state` (SIZE | POSITION
| MAXIMIZED). Restore is handled entirely by the plugin's `window_created` hook, which runs on the
main thread inside the running event loop — where its `set_size`/`set_position` resolve inline and
its monitor-intersection check keeps a stale off-screen position from stranding the window. So
`build_window` must **not** call `restore_state` itself. That looks correct (windows are built at
runtime, not from `tauri.conf.json`, so restore them by hand) but is a footgun: once a saved entry
matches the label, `restore_state` applies geometry via calls that marshal to the main event loop and
block when invoked off it. In the setup hook the loop hasn't started (launch freeze); on the
hot-reload watcher thread it holds the plugin mutex while the `window_created` hook waits on it
(reload deadlock). It stayed invisible while no window title hashed to a persisted label (restore
short-circuited before the marshal); the first matching title — e.g. renaming a window onto an old
entry — froze the app. State is keyed by Tauri label (== `window_id`,
derived from the title, stable across launches) *within a per-config state file*
(`window_state_filename` hashes the config path) so two configs that reuse a window title don't
share bounds. The config `width`/`height` is only the first-run default — saved bounds override it
once present. The transient error window is `skip_initial_state`-excluded. Renaming a window's
`title` changes its id/label, so it normally restores fresh default bounds — unless the new title
happens to hash to a label already in the state file, which is how the rename-onto-old-entry case
above arises. Sidebar width is separate (per-title `localStorage`, above).

**Footgun — the `AppState.windows` mutex is the only lock, and commands must stay synchronous.**
Several `#[tauri::command]`s (`select_tab`, `reset_window_tabs`, …) hold the `windows` lock across
webview ops (`add_child`/show/hide/raise/navigate). That's deadlock-free *only* because sync Tauri
commands run on the main thread, so those ops execute inline and the watcher thread (which always
drops the lock before marshaling a webview op to main) can't deadlock against them. **Do not make
any command that holds `windows` `async`** — it would then run off-main, hold the lock, and block
on a main-thread-marshaled webview op while a `windows`-locking main-thread callback waits, which
deadlocks. If a command must become async, drop the `windows` guard before any webview op. (The
create-tab path avoids this class by reading `WindowRuntime.hole` under the held `windows` lock and
passing it into `create_content_webview` by value, rather than re-locking `windows` inside.)

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

1. Bump the version in `src-tauri/Cargo.toml` (the single source of version truth — `package.json`
   no longer exists) and commit it.
2. Confirm the release gate is green: `just gate`.
3. Tag the release commit `v<version>` (matching the bumped version) and publish:
   `gh release create v<version> --target main --title v<version> --notes "<changelog>"`,
   where the notes summarise what shipped since the previous release.
4. There is no CI to build artifacts, so attach the locally built app: `just build`,
   then zip the `.app` and `gh release upload v<version> <app>.zip` so the release
   carries an installable binary (the `install.sh` path still builds from source).
5. Attach the **updater artifacts** so existing installs auto-update (see *In-app updates*):
   `just release-artifacts <version>` (builds, then writes `latest.json`), then
   `gh release upload v<version> src-tauri/target/release/bundle/macos/curator.app.tar.gz
   src-tauri/target/release/bundle/macos/curator.app.tar.gz.sig latest.json`. The updater fetches
   `latest.json` from the `releases/latest/download/` alias, so the newest release is found with no
   per-release URL edit. This needs the updater signing env set (below); without it no `.sig` is
   produced and `release-artifacts` errors rather than shipping an unsignable release.

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

## In-app updates

curator updates itself via **`tauri-plugin-updater`** (+ `tauri-plugin-process` for the relaunch).
**The self-updater is an app-agnostic capability that belongs in chrome-core** (see chrome-core's
CLAUDE.md dividing-line decision — an updater is an *app* feature, not a terminal/browser one), so the
check + cadence + install flow are being consolidated there for both apps to inherit; today they still
live in this controller (`src/chrome.js`), described below as the current state.
The chrome checks on launch (gated on the `auto_update` config key) and via the **curator ▸ Check
for Updates…** menu item (always, ignoring `auto_update`); on a hit it shows chrome-core's update
bar and, on the user's confirm, downloads + installs + relaunches. It is **confirm-to-install** —
nothing installs silently. The update bar's **×** dismisses it for the session (`chrome.js`'s
`updateDismissed` flag suppresses auto re-surfacing; the menu check clears it and re-surfaces).

- **Endpoint (zero-server):** `plugins.updater.endpoints` in `tauri.conf.json` points at
  `https://github.com/Lockyc/curator/releases/latest/download/latest.json`. GitHub's
  `releases/latest/download/<asset>` alias always resolves to the newest release's asset, so there
  is no server and no per-release URL to edit.
- **Signing key (separate from Apple):** the updater verifies its **own** minisign signature over
  the `.app.tar.gz` — a different trust anchor from the Apple Developer ID cert. The private key
  lives on the build machine at `~/.tauri/curator-updater.key` (never committed); its **public key**
  is committed in `tauri.conf.json` (`plugins.updater.pubkey`). Cutting an updatable release needs
  `TAURI_SIGNING_PRIVATE_KEY` (path or contents) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` set in the
  build env alongside the Apple creds. **`createUpdaterArtifacts` is enabled release-only via the
  `just release-artifacts` `--config` override, NOT in the committed `tauri.conf.json`** — footgun:
  baking it into the config makes *every* `cargo tauri build` demand the signing key, which breaks
  `install.sh` / `just build` / `just deploy` (a keyless from-source build errors with "A public key
  has been found, but no private key"). So the emit of the signed `curator.app.tar.gz` + `.sig`
  happens only under the release recipe; without the key there, `gen-latest-json.sh` errors and the
  release simply isn't auto-updatable (fail-safe). The `pubkey` stays in the committed config (it's
  runtime-only and doesn't trigger signing on its own — only `createUpdaterArtifacts` does).
- **Where the code lives (migrating):** the update **bar UI** is already in **chrome-core**
  (`setUpdate`/`clearUpdate` + the `onUpdate` callback) so both apps share it. The actual
  `check()`/`downloadAndInstall()`/`relaunch()` + re-check cadence **currently** live in
  `src/chrome.js` (`checkForUpdate` / `installUpdate`, reached via the `window.__TAURI__.updater`/
  `.process` globals under `withGlobalTauri`) — but per chrome-core's dividing-line decision they are
  being consolidated **into chrome-core**, which feature-detects the shared Tauri runtime
  (`window.__TAURI__?.updater`) so its isolated `preview.html` still no-ops. What stays here is
  curator's updater *identity* — endpoint, pubkey, the Rust plugin registration, the `auto_update`
  gate. The menu item emits `check-update` to the focused chrome — the same `emit_to_focused_chrome`
  pattern as `nav-tab`/`jump-tab`.
- **Capability:** the chrome is granted `updater:default` + `process:allow-restart` in
  `capabilities/default.json` — local-origin only (no `remote` block), so remote content tabs never
  receive them, exactly like `core:event`.
- **Arch:** these machines are Apple Silicon, so `latest.json` carries only a `darwin-aarch64`
  platform entry (see `scripts/gen-latest-json.sh`). Add a `darwin-x86_64` entry (a second build) or
  switch to a universal binary only if an Intel user ever needs updates.
- **First-release bootstrap:** the release that first ships the updater must be installed the old way
  once (`install.sh` / download the `.zip`); every release after that updates in-app. Call this out
  in that release's notes.

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

## Shared sidebar chrome: `chrome-core`

The sidebar chrome — banner, grouped tab rows (tile, title, dot slots), kill-confirm overlay,
density tokens, resize-drag, error bar — is the shared **`chrome-core`** component
(`https://github.com/Lockyc/chrome-core`), consumed by both curator and warden so a look/behaviour
change is made once. It's a *build-dependency* pinned by `rev` (like config-core); `src-tauri/build.rs`
writes `SIDEBAR_CSS`/`SIDEBAR_JS` into `src/chrome-core.{css,js}` (git-ignored) before Tauri embeds
`src/`. curator's `src/chrome.js` is now a **thin controller** over chrome-core's `ChromeSidebar`
view: it maps the component's callbacks to curator's commands (`onSelect`→`select_tab`/`home_tab`,
`onUnload`→`unload_tab`, `onResize`→sidebar CSS width + `reportRect`→`set_hole_rect`) and events to its setters
(`service-badge`→`setAttention`, `config-error`→`setError`, `nav-tab`/`jump-tab`→nav). The nav pill
(browser-only) mounts into the component's `header` slot; curator passes `active` in the DTO (its
Rust side owns selection). What stays per-app is the content-area topology (curator z-orders content
webviews) + the controller — not the sidebar. See warden's CLAUDE.md and chrome-core's own for the
full interface. Edit the chrome in chrome-core (`assets/sidebar.{css,js}`), never the generated
`src/chrome-core.*`.

**Chrome dev loop — the `chrome-*` just recipes** (they assume the sibling `../chrome-core` ghq checkout).
For active chrome work, **`just chrome-dev`** activates a normally-commented `[patch]` in `src-tauri/Cargo.toml`
so curator builds against your local `../chrome-core` (uncommitted edits included); iterate, then
**`just chrome-pin`** re-pins the rev to `../chrome-core`'s pushed HEAD and re-comments the patch.
**Never commit an active patch** — it breaks fresh clones/CI; **`just gate` refuses to pass while it's
active** (the safety net).

**Visual feedback loop — iterate the sidebar without building curator.** chrome-core ships a checked-in
**`preview.html`** that mounts `ChromeSidebar` in isolation against a representative DTO (loose tabs, a
plain group, a project-tree with folders + leaves, across the dot states). **`just chrome-preview`** opens
it (or `just preview` / `just shot` inside chrome-core) so you *see* a CSS/JS change without the app
round-trip. URL params: **`?density=compact`** previews the compact scale, and **`?header=1`** mounts a
stand-in in the banner's `header` slot (curator fills it with the nav pill; the preview uses a neutral
placeholder) plus a live readout of `#cc-banner`'s height — proving the banner is one fixed height with or
without the slot (chrome-core's `--cc-banner-min`). This is the fast loop for chrome work; the pinned-rev
round-trip through curator is only for shipping. See chrome-core's CLAUDE.md for the full loop + the
banner-height invariant.
