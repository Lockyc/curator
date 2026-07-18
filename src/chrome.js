// curator's chrome controller: binds the shared chrome-core ChromeSidebar (the view) to curator's
// Tauri backend. The sidebar rendering, rows, dots, groups, kill-confirm, resize, and error bar all
// live in chrome-core; this file only maps callbacks → commands and events → setters, and keeps the
// curator-only nav pill (browser navigation), which mounts into the component's header slot.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
// In-app updates are a chrome-core capability (check / install / relaunch + the 6h re-check cadence);
// see chrome-core's dividing-line decision. This controller only passes curator's `auto_update` gate
// (autoUpdate in the mount config) and forwards the menu event to sb.checkForUpdateNow(). curator
// keeps its own updater identity — endpoint, pubkey, plugin registration — in tauri.conf.json / Rust.

// ── Nav pill (curator-only) ─────────────────────────────────────────────────
// SVGs: exact geometry so icons align. Handlers act on the active tab (mirrored in `activeLabel`).
const BACK_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 18l-6-6 6-6"/></svg>`;
const FWD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>`;
const RELOAD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.6-6.36"/><path d="M21 3v6h-6"/></svg>`;
const HOME_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/></svg>`;

let activeLabel = null; // controller mirror of the component's active tab (the nav pill acts on it)
let navBtns = [];
// Labels currently popped out into their own detached window (from the DTO). A click on a detached
// row means "raise its window" (raise_popped_window), not "select" — so onSelect consults this.
const detachedLabels = new Set();

function buildNavPill() {
  const pill = document.createElement("div");
  pill.className = "nav-pill";
  const wire = (id, icon, cmd) => {
    const btn = document.createElement("button");
    btn.className = "nav-btn";
    btn.id = id;
    btn.innerHTML = icon;
    btn.disabled = true;
    btn.addEventListener("click", () => {
      if (activeLabel) invoke(cmd, { label: activeLabel }).catch(() => {});
    });
    pill.appendChild(btn);
    return btn;
  };
  navBtns = [
    wire("nav-back", BACK_SVG, "nav_back"),
    wire("nav-forward", FWD_SVG, "nav_forward"),
    wire("nav-reload", RELOAD_SVG, "reload_tab"),
    wire("nav-home", HOME_SVG, "home_tab"),
  ];
  return pill;
}

function setActiveLabel(label) {
  activeLabel = label;
  for (const b of navBtns) b.disabled = !label;
}

// ── DTO mapping ─────────────────────────────────────────────────────────────
// Unread badges arrive via `service-badge`, not `get_tabs`, so persist them here (like the old
// chrome) and fold them into the DTO's `attention` field on every rebuild.
const badges = new Map(); // label → badge text

// Map curator's badge text to chrome-core's attention: a number → a count, any other non-empty
// (a bullet / activity marker) → true, empty → none.
function badgeToAttention(text) {
  if (!text) return null;
  return /^\d+$/.test(text) ? Number(text) : true;
}

async function buildDto() {
  const id = await invoke("window_identity");
  const tabs = await invoke("get_tabs");
  // Drop badges for tabs that no longer exist (removed / URL-hash label moved), so a future tab
  // reusing the label doesn't inherit a stale count with no fresh service-badge (mirrors warden).
  const labels = new Set(tabs.map((t) => t.label));
  [...badges.keys()].forEach((l) => { if (!labels.has(l)) badges.delete(l); });
  // Refresh the detached-label mirror so onSelect can tell a popped-out row (raise its window) from
  // a normal one (select). Rebuilt each DTO so it clears when a tab redocks.
  detachedLabels.clear();
  tabs.forEach((t) => { if (t.detached) detachedLabels.add(t.label); });
  return {
    title: (id && id.title) || "",
    colour: (id && id.colour) ?? null,
    density: (id && id.density) || "comfortable",
    // sidebar_drag (global config, default on): make the non-interactive chrome a window-move drag
    // handle. Absent field defaults on, matching the config default.
    windowDrag: !(id && id.sidebar_drag === false),
    // curator's Rust side owns which tab is active — pass it so chrome-core honours it (no auto-fire).
    active: (tabs.find((t) => t.active) || {}).label ?? null,
    tabs: tabs.map((t) => ({
      id: t.label,
      title: t.title,
      group: t.group ?? null,
      live: t.loaded,
      attention: badgeToAttention(badges.get(t.label)),
      presence: null, // curator has no session-presence concept
      killable: false, // curator has no kill concept
      // Popped out into its own window: chrome-core renders the ⤢ mark and routes a row click to
      // onSelect, which the controller maps to "raise the window". Invisible unless forwarded here.
      detached: !!t.detached,
      warn: false,
    })),
  };
}

// ── Mount + refresh ─────────────────────────────────────────────────────────
let sb = null;

// The empty-state (muted curator mark) shows only when no tab is active — otherwise a content
// webview covers the hole. It's composited BEHIND the content webviews, so this is occluded
// whenever a tab is shown; toggling on `active` keeps it from peeking during transitions.
function paintEmptyState(active) {
  document.getElementById("empty-state").style.display = active ? "none" : "flex";
}

// Report the #content-hole's CSS rect so Rust positions the content webviews to match. This is the
// single source of truth for content placement (warden's model): chrome-core owns the sidebar
// width and clamp, the flex hole follows from CSS, and Rust just applies what's measured here.
function reportRect() {
  const r = document.getElementById("content-hole").getBoundingClientRect();
  invoke("set_hole_rect", { rect: { x: r.x, y: r.y, width: r.width, height: r.height } }).catch(() => {});
}

async function refresh() {
  const dto = await buildDto();
  sb.update(dto);
  setActiveLabel(dto.active);
  paintEmptyState(dto.active);
  reportRect();
}

// Shared by chrome-core's own row-unload control (onUnload below) and the ⌘W menu shortcut's
// "close-tab" event (below) — both mean the same thing (unload the given/active tab to cold), so
// this is the one place that does it rather than two copies.
async function unloadTab(tabId) {
  await invoke("unload_tab", { label: tabId }).catch(() => {});
  // Unloading may make Rust promote a load_on_open tab to active (or clear it) — re-render so
  // the highlight + loaded dots follow the new state (get_tabs carries the new active).
  await refresh();
}

// Shared by chrome-core's per-row ⤢ control (onPopOut) and the ⌘⇧O menu shortcut's "pop-out-tab"
// event — both pop the given/active tab out into its own window. Refresh so the origin's sidebar
// shows the row's ⤢ detached mark and its newly-promoted active tab (get_tabs carries both).
async function popOutTab(tabId) {
  await invoke("pop_out_tab", { label: tabId }).catch(() => {});
  await refresh();
}

async function mountChrome() {
  const id = await invoke("window_identity");
  const title = (id && id.title) || "";
  const defaultWidth = (id && id.default_width) || 240;

  sb = window.ChromeSidebar.mount(
    document.getElementById("sidebar"),
    {
      onSelect(tabId, { wasActive }) {
        // A popped-out row has no local webview to select — a click raises its detached window.
        if (detachedLabels.has(tabId)) {
          invoke("raise_popped_window", { label: tabId }).catch(() => {});
          return;
        }
        setActiveLabel(tabId);
        // Re-clicking the active tab snaps it home (curator's home-on-active); otherwise select it.
        invoke(wasActive ? "home_tab" : "select_tab", { label: tabId }).catch(() => {});
      },
      onUnload: unloadTab,
      // Pop the tab out into its own window (recreated webview; login survives via the session
      // store). Refresh so the row picks up its ⤢ detached mark and the origin's new active tab.
      onPopOut: popOutTab,
      // Dock a popped-out tab back in (the ↩ overlay on a detached row's tile): close its window,
      // whose Destroyed handler runs redock — the same return path as closing it by hand.
      onPopIn(tabId) {
        invoke("pop_in_tab", { label: tabId }).catch(() => {});
      },
      onResize(width) {
        // The chrome is the window's full-size main webview: the sidebar's visible width is CSS
        // (set here); the flex #content-hole follows, and reportRect tells Rust where to put the
        // content webviews. Rust no longer computes or clamps a width — chrome-core is the sole
        // clamp (bounds below), so there's nothing to keep in sync across the JS/Rust boundary.
        setSidebarWidth(width);
        reportRect();
      },
      // onKill: unused — curator sets killable:false, so the component never invokes it.
    },
    {
      header: buildNavPill(),
      appName: "curator",
      storageKey: "curator:sidebar-width:" + title,
      defaultWidth,
      minWidth: MIN_W,
      maxWidth: MAX_W,
      // The chrome is the full-window main webview, so chrome-core's `window.innerWidth` IS the
      // window width and this is the ≤40% cap. chrome-core is now the *sole* clamp — Rust positions
      // the content from the reported hole rect, so there's no second (Rust) cap to keep aligned.
      maxFraction: MAX_FRACTION,
      // chrome-core's self-updater gate: run the launch + 6h checks when curator's config allows.
      autoUpdate: id ? id.auto_update !== false : false,
    }
  );

  await refresh();

  // First-run width: chrome-core restores a saved width itself (firing onResize → CSS + reportRect);
  // if none is saved, apply the density-aware default. Setting the sidebar CSS width reflows the flex
  // #content-hole, so the ResizeObserver below fires reportRect and Rust realigns the content — no
  // explicit width has to be pushed to Rust anymore.
  const saved = parseFloat(localStorage.getItem("curator:sidebar-width:" + title));
  if (!(saved > 0)) {
    setSidebarWidth(defaultWidth);
  }

  // (The launch + 6h update checks are armed by chrome-core from the `autoUpdate` mount config.)
}

// Sidebar width bounds passed to chrome-core (the single clamp). The window-resize handler below
// re-applies the ≤40% cap because a window shrink can push the sidebar past it without a drag.
const MIN_W = 160, MAX_W = 520, MAX_FRACTION = 0.4;

function setSidebarWidth(w) {
  document.getElementById("sidebar").style.width = Math.round(w) + "px";
}

// A window resize can push the sidebar past the ≤40% cap; re-clamp it here, then report the new
// hole so Rust repositions the content (there's no Rust-side resize relayout — JS drives it, as in
// warden). The flex #content-hole also fires the ResizeObserver below, so this is belt-and-braces.
window.addEventListener("resize", () => {
  const el = document.getElementById("sidebar");
  const cur = parseInt(el.style.width, 10) || parseInt(getComputedStyle(el).width, 10);
  if (Number.isFinite(cur)) {
    const upper = Math.min(MAX_W, window.innerWidth * MAX_FRACTION);
    setSidebarWidth(Math.max(MIN_W, Math.min(cur, upper)));
  }
  reportRect();
});

// The content webviews track the hole: re-report whenever it resizes (sidebar drag, window resize).
// ResizeObserver fires once when observation begins, which is what makes the initial report happen.
const holeObserver = new ResizeObserver(() => reportRect());
holeObserver.observe(document.getElementById("content-hole"));

// ── Events ──────────────────────────────────────────────────────────────────
listen("config-reloaded", () => {
  sb.clearError();
  refresh();
});
listen("config-error", (e) => {
  sb.setError("Config error (keeping last good): " + e.payload);
});
listen("service-badge", (e) => {
  const { label, text } = e.payload;
  if (text) badges.set(label, text);
  else badges.delete(label);
  sb.setAttention(label, badgeToAttention(text));
});
// Keyboard tab navigation (Tabs menu): ⌘⇧]/⌘⇧[ cycle (all tabs — curator has no cold tabs to skip),
// ⌘1–9 jump. The component resolves the target and routes it through the normal select path.
listen("nav-tab", (e) => sb.selectByOffset(e.payload, { liveOnly: false }));
listen("jump-tab", (e) => sb.selectByIndex(e.payload));
// The menu spine's ⌘W (Tabs ▸ Close Tab): unloads whichever tab is active in THIS window. lib.rs
// routes it via emit_to_focused_chrome, so only the focused window's chrome receives it.
listen("close-tab", () => {
  if (activeLabel) unloadTab(activeLabel);
});
// The menu spine's ⌘⇧O (Tabs ▸ Pop Out Tab): pop THIS window's active tab out into its own window.
// lib.rs routes it via emit_to_focused_chrome, so only the focused window's chrome receives it.
listen("pop-out-tab", () => {
  if (activeLabel) popOutTab(activeLabel);
});
// A desktop-notification banner was clicked (A2): select+activate the tab that fired it.
// Skip when it's already the active tab — re-selecting it would trip the home-on-active gesture
// (onSelect wasActive → home_tab), navigating away from the very thing the banner was about; the
// window raise already happened Rust-side, so surfacing an already-active tab needs no action.
listen("focus-tab", (e) => {
  const label = e.payload && e.payload.label;
  if (label && label !== activeLabel) sb.select(label);
});
// Menu "Check for Updates…" → chrome-core checks now + announces (up-to-date / error / a banner).
listen("check-update", () => sb && sb.checkForUpdateNow());

mountChrome();
