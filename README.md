<p align="center">
  <img src="src-tauri/icons/icon.png" alt="curator app icon" width="128" height="128">
</p>

<h1 align="center">curator</h1>

A dedicated, always-findable home for the browser tabs you can't afford to lose. macOS only.

<p align="center"><img src="docs/screenshot.png" alt="curator window showing grouped keeper tabs in the sidebar" width="840"></p>

Not a general browser: a minimal app (Tauri v2) that renders a *curated, declarative* set
of "keeper" tabs from a `config.toml` config, and refuses to let new-tab navigation
pollute that set — handing every such intent off to your macOS default browser instead.

## Why

Important tabs (mail, calendar, dashboards) get buried in a sea of browser windows.
Firefox pinned tabs are the closest workaround, but the pinned window itself gets lost and
keeping it clean is constant manual work. curator gives keeper tabs a distinct, stable
home that lives outside the window-pile and never accumulates cruft — curation is
file-driven, everything else is ephemeral.

## Model

- **`config.toml` is the source of truth** — each `[[window]]` block opens one window,
  containing `[[window.group]]` and `[[window.group.tab]]` entries. No in-app pin/unpin;
  you curate by editing the file (hot-reloaded on save).
- **Multiple windows** — each `[[window]]` opens its own window with its own tab list. All
  open at launch; ⌘W closes any non-last window and the **Window** menu reopens it.
- **Keeper tabs are home bases** — wander within a session, then snap any tab back to its
  canonical URL with the sidebar's ⌂ home button (or by re-clicking the active tab); every
  tab also resets on restart.
- **New-tab intents escape** — `target="_blank"`, `window.open`, cmd/middle-click all
  shell out to `open`, routing to your macOS default browser instead of opening in curator.
- **Sessions persist, and are shareable** — log into a site once in-app and it stays. By
  default every tab shares one login store, so signing into a provider covers its related
  services (Gmail, Calendar, …). Set a tab's (or a window's) `session = "name"` to give it a
  separate account; reuse the same name elsewhere to share that login. Logins follow the
  `session` name, so renaming a window or editing a URL never signs you out.
- **Page-first chrome** — the active page fills the window edge-to-edge, painting under an
  overlay title bar; the native title bar (with traffic lights, draggable) is exposed only
  as a strip above the sidebar tab list.
- **Per-window opt-in features** — `notifications = true` lets web `Notification` calls
  pop native banners; `unread = true` tracks per-tab unread counts, rolling them up to the
  dock badge. Both default off; a window with either enabled is always eager-loaded.
- **Dock badge aggregates across windows** — the badge total is the sum of all windows
  that have `unread = true`.
- **Window menu** — close a non-last window (⌘W); reopen any closed window from the
  Window menu.

## Quick install

In **Claude Code**, run `/curator:install` — it checks prerequisites (offering to install
any that are missing), builds curator from source into `~/.curator`, installs `curator.app`
to `/Applications`, and seeds your config.

Or install from a terminal (once this repo is public):

```sh
curl -fsSL https://raw.githubusercontent.com/Lockyc/curator/main/install.sh | bash
```

Re-running either path updates curator (`git pull` + rebuild). The steps below describe the
manual / contributor flow.

## Setup

1. Copy the sample config into place:

   ```sh
   mkdir -p ~/.config/curator
   cp examples/config.toml ~/.config/curator/config.toml
   ```

   It lives under `~/.config/` so it slots into a dotfiles workflow — your curated tab set
   becomes versioned, portable config.

2. Run it (requires Rust + Node):

   ```sh
   just dev      # or: npm run tauri dev
   ```

   `just dev` loads the repo's `examples/config.toml` (via the `CURATOR_CONFIG` env var) so
   iterating never touches your real `~/.config/curator/config.toml`. Point `CURATOR_CONFIG`
   at any file to test another config.

   `just build` produces a `.app` bundle; **`just deploy`** builds and installs/updates it
   in `/Applications` (quitting the running copy and relaunching). `just test` runs the Rust
   tests. The app icon source is `src-tauri/icons/icon.svg` — re-run `npx tauri icon
   src-tauri/icons/icon.svg` after editing it.

3. Edit `~/.config/curator/config.toml` and save — the sidebar **hot-reloads**, no restart.
   A malformed file keeps the last-good config running and shows an error banner instead of
   crashing. The **Config** menu has *Edit Config* / *Reveal Config in Finder* so you needn't
   memorise the path; the **Tabs** menu has *Reload Tab* (⌘R) and *Reset All Tabs* to snap
   every open tab back to its canonical URL.

## Config

curator opens one window per `[[window]]` block. Each window inlines its groups and tabs:

```toml
# App-global options
# dark_mode     = true            # force dark appearance; omit = follow system
# allow_insecure = ["192.168.1.1"] # accept self-signed TLS for these hosts

[[window]]
title         = "Keepers"          # required; must be unique across windows
# width       = 1500               # optional; default 1500 × 1000
# height      = 1000
# open_on_launch = "Grafana"       # true/false/"Tab Title"
# notifications = false            # allow web Notification API to pop native banners
# unread        = false            # track unread counts → dock badge

  [[window.group]]
  name = "Dashboards"

    [[window.group.tab]]
    title        = "Grafana"
    url          = "https://play.grafana.org/"
    always_load  = true    # preload + keep warm from launch
    reload_every = 5       # auto-refresh every 5 minutes

[[window]]
title         = "Comms"
notifications = true
unread        = true

  [[window.group]]
  name = "Chat"

    [[window.group.tab]]
    title       = "Mattermost"
    url         = "https://community.mattermost.com/"
    always_load = true
```

### Per-window options

| Field             | Type                     | Default       | Meaning                                                                    |
|-------------------|--------------------------|---------------|----------------------------------------------------------------------------|
| `title`           | string                   | **required**  | Window title; must be unique across all windows.                           |
| `width`/`height`  | int                      | `1500`/`1000` | Initial window size in logical pixels. Applied at launch (restart to change). |
| `open_on_launch`  | bool \| tab title string | `false`       | `true` opens the first tab; a string opens the named tab; `false` = blank screen. |
| `notifications`   | bool                     | `false`       | Allow the web `Notification` API to pop native banners.                    |
| `unread`          | bool                     | `false`       | Track per-tab unread counts; roll them up to the dock badge.               |

### Per-tab options

Each `[[window.group.tab]]` requires `title` and `url`. Optional:

| Field          | Type         | Default | Meaning                                         |
|----------------|--------------|---------|--------------------------------------------------|
| `always_load`  | bool         | `false` | Preload the tab and keep it warm from launch.    |
| `reload_every` | positive int | none    | Auto-refresh the canonical URL every N minutes.  |

### App-global options

| Field            | Type          | Default | Meaning                                                                                       |
|------------------|---------------|---------|-----------------------------------------------------------------------------------------------|
| `dark_mode`      | bool          | `false` | Force dark appearance so sites honouring `prefers-color-scheme` render dark.                  |
| `allow_insecure` | list of hosts | `[]`    | Accept self-signed/invalid TLS certs for these hosts. Applied at launch (restart to change).  |

Tabs are lazy by default: a webview is created on first activation and kept warm for the
session. Each row shows a green dot when its tab is loaded — click it to **unload** (free
that webview's memory); the tab reloads on next click. A navigation pill at the top of the
sidebar drives the active tab: **◀ back** and **▶ forward** through in-page history, and
**⌂ home** to snap back to its canonical URL.

See `examples/config.toml` for a two-window starting-point example.

## License

[MIT](LICENSE)
