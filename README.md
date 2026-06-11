# curator

A dedicated, always-findable home for the handful of browser tabs you can't afford to
lose. macOS only.

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

Status: early — design only. See `docs/` (untracked scratch).
