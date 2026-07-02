// curator's chrome controller: binds the shared chrome-core ChromeSidebar (the view) to curator's
// Tauri backend. The sidebar rendering, rows, dots, groups, kill-confirm, resize, and error bar all
// live in chrome-core; this file only maps callbacks → commands and events → setters, and keeps the
// curator-only nav pill (browser navigation), which mounts into the component's header slot.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ── Nav pill (curator-only) ─────────────────────────────────────────────────
// SVGs: exact geometry so icons align. Handlers act on the active tab (mirrored in `activeLabel`).
const BACK_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 18l-6-6 6-6"/></svg>`;
const FWD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>`;
const RELOAD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.6-6.36"/><path d="M21 3v6h-6"/></svg>`;
const HOME_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/></svg>`;

let activeLabel = null; // controller mirror of the component's active tab (the nav pill acts on it)
let navBtns = [];

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

async function refresh() {
  const dto = await buildDto();
  sb.update(dto);
  setActiveLabel(dto.active);
  paintEmptyState(dto.active);
}

async function mountChrome() {
  const id = await invoke("window_identity");
  const title = (id && id.title) || "";
  const defaultWidth = (id && id.default_width) || 240;

  sb = window.ChromeSidebar.mount(
    document.getElementById("sidebar"),
    {
      onSelect(tabId, { wasActive }) {
        setActiveLabel(tabId);
        // Re-clicking the active tab snaps it home (curator's home-on-active); otherwise select it.
        invoke(wasActive ? "home_tab" : "select_tab", { label: tabId }).catch(() => {});
      },
      async onUnload(tabId) {
        await invoke("unload_tab", { label: tabId }).catch(() => {});
        // Unloading may make Rust promote a load_on_open tab to active (or clear it) — re-render so
        // the highlight + loaded dots follow the new state (get_tabs carries the new active).
        await refresh();
      },
      onResize(width) {
        // The chrome is now the window's full-size main webview, so the sidebar's visible width is
        // CSS (set here) and Rust positions the content webviews at the matching x via
        // set_sidebar_width. Both clamp with identical bounds (see the config below ↔ Rust's
        // clamp_chrome_w), so the sidebar edge and the content's left edge always agree.
        setSidebarWidth(width);
        invoke("set_sidebar_width", { width: Math.round(width) }).catch(() => {});
      },
      // onKill: unused — curator sets killable:false, so the component never invokes it.
    },
    {
      header: buildNavPill(),
      storageKey: "curator:sidebar-width:" + title,
      defaultWidth,
      minWidth: MIN_W,
      maxWidth: MAX_W,
      // Matches Rust's clamp_chrome_w (≤40% of the window). Now that the chrome is the full-window
      // main webview, chrome-core's `window.innerWidth` IS the window width, so this JS cap and the
      // Rust cap agree — keeping the sidebar edge aligned with the content webviews' left edge.
      maxFraction: MAX_FRACTION,
    }
  );

  await refresh();

  // First-run width: chrome-core restores a saved width itself (firing onResize → CSS + Rust); if
  // none is saved, apply the density-aware default. Rust launches content at CHROME_W (comfortable);
  // only the narrower compact default needs to be pushed to Rust to realign the content edge.
  const saved = parseFloat(localStorage.getItem("curator:sidebar-width:" + title));
  if (!(saved > 0)) {
    setSidebarWidth(defaultWidth);
    if (id && id.density === "compact") {
      invoke("set_sidebar_width", { width: Math.round(defaultWidth) }).catch(() => {});
    }
  }
}

// Sidebar width bounds — kept identical to Rust's clamp_chrome_w (MIN_CHROME_W / MAX_CHROME_W /
// MAX_CHROME_FRACTION) so the JS-owned sidebar edge and the Rust-positioned content edge agree.
const MIN_W = 160, MAX_W = 520, MAX_FRACTION = 0.4;

function setSidebarWidth(w) {
  document.getElementById("sidebar").style.width = Math.round(w) + "px";
}

// A window resize can push the sidebar past the ≤40% cap; re-clamp it here with the same bounds
// Rust re-clamps the content with, so the two stay edge-aligned without any extra round-trip.
window.addEventListener("resize", () => {
  const el = document.getElementById("sidebar");
  const cur = parseInt(el.style.width, 10) || parseInt(getComputedStyle(el).width, 10);
  if (!Number.isFinite(cur)) return;
  const upper = Math.min(MAX_W, window.innerWidth * MAX_FRACTION);
  setSidebarWidth(Math.max(MIN_W, Math.min(cur, upper)));
});

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
// A desktop-notification banner was clicked (A2): select+activate the tab that fired it.
// Skip when it's already the active tab — re-selecting it would trip the home-on-active gesture
// (onSelect wasActive → home_tab), navigating away from the very thing the banner was about; the
// window raise already happened Rust-side, so surfacing an already-active tab needs no action.
listen("focus-tab", (e) => {
  const label = e.payload && e.payload.label;
  if (label && label !== activeLabel) sb.select(label);
});

mountChrome();
