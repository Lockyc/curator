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

## Link-cursor flicker over content tabs — a macOS 26.5.2 regression, not curator's

Over a loaded content tab the cursor flickers while the pointer moves and reverts to the arrow the
moment it stops, so the pointing hand over a link is unreliable. Reproduces on any site, on links
and on plain page areas alike.

**Cause — the OS, not this repo.** curator's hole-punch stacks two WKWebViews: the chrome is the
window's full-window MAIN webview (it must be — `data-tauri-drag-region` only moves the window from
the main webview) and each content tab is an `add_child` sibling composited above it over the hole.
AppKit's mouse tracking is **geometric, not occlusion-aware**, so the chrome keeps claiming the hole
region even while a tab is painted on top of it, and *both* webviews compute a cursor for the same
pixel. WebKit pushes its cursor asynchronously over IPC while AppKit re-asserts synchronously per
mouse event, so the two race. Confirmed directly by styling `#content-hole` (a chrome-side div, under
every tab) with a distinctive cursor and watching it render **over** a live page.

That stacking is as old as the hole-punch, so it does not by itself explain a *recent* regression —
what changed is the arbitration between the two. Bisected by running old tags on the current OS:

| Build | Result |
| --- | --- |
| v0.10.0 | flickers |
| v0.9.0 | flickers |
| v0.7.2 (3 Jul, predates the OS update) | flickers |

Code written and used before the update misbehaves on the current OS, so only the OS changed:
**macOS Tahoe 26.5.2 (25F84), installed 9 July 2026**; first noticed ~23 July. No newer macOS is
available, so there is no update to take. **lector is almost certainly affected too** (identical
WKWebView-over-WKWebView stacking); warden most likely is not, since its hole content is native
surfaces rather than webviews.

**Ruled out — do not re-investigate these:**

| Suspect | How it was cleared |
| --- | --- |
| The `progress_bar` 30 Hz timer | Disabled `progress_bar::install` outright — still flickers |
| tao's full-bounds arrow cursor rect | `disableCursorRects` verified taking effect (`true -> false`) — no change |
| Hit-testing / occlusion | `hitTest:` override returning nil over covered points — no change, so **WebKit does not derive its cursor from hit-testing** |
| wry / tauri injected handlers | wry's WKWebView subclass overrides only `acceptsFirstMouse:`; tauri's bundle has no mousemove cursor handling |
| Anything shipped in 0.10.0 | v0.9.0 and v0.7.2 flicker identically |

**The `NSCursor`-interception workaround was built, tried, and removed — don't rebuild it.** The one
intervention that measurably moved the needle was swizzling `-[NSCursor set]`: filtering
AppKit-originated arrow-sets took AppKit's share of the fight to zero, leaving the covered chrome's
own WebKit-originated arrows. Attributing *those* needs a tag, so the full attempt stamped a sentinel
cursor on `#content-hole` (nothing else uses it, so a sentinel set is by construction "the covered
chrome"), dropped it while a tab was under the pointer, and swapped in the arrow when the hole was
bare. It gated green with the CSS↔Rust pairing enforced by a test — and **it still did not fix the
flicker in practice.** Reverted (`0ec0bb1`, then its revert) because a process-wide cursor swizzle
plus a sentinel-cursor convention is a bad trade even when it works, and a flaky one is no trade at
all. Treat cursor-set interception as **tried and failed**, not as the untried option.

That exhausts the layers curator can reach: cursor rects, hit-testing, and the cursor call itself.
Any real fix now lives in WebKit/AppKit — so the standing action is to **report it to Apple**
(Feedback Assistant; the `#content-hole` sentinel-cursor trick is a crisp reproduction: style the
covered webview's hole and watch its cursor render over a live page) and re-test on each macOS
update. Shelved deliberately until then.

**Footgun if anyone revisits this anyway:** do **not** isa-swizzle the webview instance
(`object_setClass`). tauri/wry already KVO-swizzles the webview, so its real class is an
`NSKVONotifying_…` subclass, and re-pointing the isa underneath KVO aborts at launch with
`Assertion failed: (imp != NULL) … NSDynamicProperties.m`. Add the override to the wry webview class
instead (`class_addMethod`) and short-circuit for instances that aren't the chrome.
