<p align="center">
  <img src="src-tauri/icons/icon.png" alt="curator app icon" width="128" height="128">
</p>

<h1 align="center">curator</h1>

A dedicated, always-findable home for the handful of browser tabs you can't afford to
lose. macOS only.

<!-- screenshot: drop a real capture here once a demo config is in place, e.g.
<p align="center"><img src="docs/screenshot.png" alt="curator window" width="720"></p>
-->

Not a general browser: a minimal, single-window app (Tauri v2) that renders a *curated,
declarative* set of "keeper" tabs from a `tabs.toml` config, and refuses to let new-tab
navigation pollute that set — handing every such intent off to the system default handler
(Velja) instead.

## Why

Important tabs (mail, calendar, dashboards) get buried in a sea of browser windows.
Firefox pinned tabs are the closest workaround, but the pinned window itself gets lost and
keeping it clean is constant manual work. curator gives keeper tabs a distinct, stable
home that lives outside the window-pile and never accumulates cruft — curation is
file-driven, everything else is ephemeral.

## Model

- **`tabs.toml` is the source of truth** — keeper tabs, grouped and ordered. No in-app
  pin/unpin; you curate by editing the file (hot-reloaded on save).
- **Keeper tabs are home bases** — wander within a session; each resets to its canonical
  URL on restart.
- **New-tab intents escape** — `target="_blank"`, `window.open`, cmd/middle-click all
  shell out to `open`, routing through Velja instead of opening in curator.
- **Sessions persist** — log into a site once in-app; it stays.
- **Page-first chrome** — the active page fills the window edge-to-edge, painting under an
  overlay title bar; the native title bar (with traffic lights, draggable) is exposed only
  as a strip above the sidebar tab list.

## Setup

1. Copy the sample config into place:

   ```sh
   mkdir -p ~/.config/curator
   cp examples/tabs.toml ~/.config/curator/tabs.toml
   ```

   It lives under `~/.config/` so it slots into a dotfiles workflow — your curated tab set
   becomes versioned, portable config.

2. Run it (requires Rust + Node):

   ```sh
   just dev      # or: npm run tauri dev
   ```

   `just build` produces a `.app` bundle; **`just deploy`** builds and installs/updates it
   in `/Applications` (quitting the running copy and relaunching). `just test` runs the Rust
   tests. The app icon source is `src-tauri/icons/icon.svg` — re-run `npx tauri icon
   src-tauri/icons/icon.svg` after editing it.

3. Edit `~/.config/curator/tabs.toml` and save — the sidebar **hot-reloads**, no restart.
   A malformed file keeps the last-good config running and shows an error banner instead of
   crashing. The **Config** menu has *Edit Config* / *Reveal Config in Finder* so you needn't
   memorise the path, plus *Reset All Tabs* to snap every open tab back to its canonical URL.

## Config options

Each `[[group.tab]]` requires `title` and `url`. Optional per tab:

| Field          | Type            | Default | Meaning                                          |
|----------------|-----------------|---------|--------------------------------------------------|
| `always_load`  | bool            | `false` | Preload the tab and keep it warm from launch.    |
| `reload_every` | positive int    | none    | Auto-refresh the canonical URL every N minutes.  |

Lazy by default: a tab's webview is created on first activation and kept warm for the
session. Each row shows a green dot when its tab is loaded — click it to **unload** (free
that webview's memory); the tab reloads on next click. The ⟳ reloads a tab's current page
(**⌘R** reloads the active tab).

Top-level option:

| Field            | Type                | Default | Meaning                                              |
|------------------|---------------------|---------|------------------------------------------------------|
| `open_on_launch` | bool \| tab title   | `false` | `true` opens the first tab on launch; a string opens the tab with that title; `false`/omit shows the blank screen. |

See `examples/tabs.toml` for a starting point.

## License

[MIT](LICENSE)
