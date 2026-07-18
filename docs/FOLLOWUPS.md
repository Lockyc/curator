---
type: reference
description: Conscious, intentionally-deferred follow-ups for curator.
links:
  - rel: part-of
    to: CLAUDE.md
---

# curator — deferred follow-ups

Known, intentionally-deferred work. Each item is a conscious deferral, not an oversight — recorded
here so it isn't lost, and so "what's next in curator?" has one place to land. Remove an item when
it's done.

## A scriptable "open window by title" entry point — build it in shell-core (shared)

Nothing outside the app can open, raise, or reopen a specific window today: it's GUI-only (the
Window submenu of the shared menu spine), and a second `open -a curator --args …` is silently
dropped — there's no `tauri-plugin-single-instance`, `tauri-plugin-deep-link`, or CLI-arg handling
anywhere in the app.

**Intended change — build the entry point in shell-core, not here**, so curator, warden, lector, and
any future sibling inherit one implementation (the same shape as `register_plugins`). Either
single-instance argv (`--window <title>`) or a `<scheme>://window/<title>` deep link, forwarded to
the already-running instance and mapped title → open-or-focus. curator's side is then just the
mapping onto `open_or_focus_window` (`lib.rs`), which already does open-or-focus by `window_id`.

**Concrete driver is on the warden side**, not curator's: warden's `work` window is
`open_on_start = false`, so its `work_startup.sh` needs a command to open it on demand. curator has
no forcing need yet — it's a shared-core capability curator would pick up for free.
