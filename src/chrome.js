const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Opaque dark base the active-tab tint composites over — matches #1e1e1e in chrome.css.
const TINT_BASE = [30, 30, 30];

function hexToRgb(hex) {
  const h = hex.replace("#", "");
  const f = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  return [parseInt(f.slice(0, 2), 16), parseInt(f.slice(2, 4), 16), parseInt(f.slice(4, 6), 16)];
}

// Blend an accent colour over the dark base at `ratio`, returning an opaque rgb(...).
function tintOverBase(hex, ratio) {
  const c = hexToRgb(hex);
  const ch = (i) => Math.round(TINT_BASE[i] * (1 - ratio) + c[i] * ratio);
  return `rgb(${ch(0)},${ch(1)},${ch(2)})`;
}

// Per-window sidebar width persistence. The width itself is owned by Rust (the sidebar is a
// fixed-width webview), so we persist the user's chosen *desired* width in localStorage keyed by
// window title and restore it by pushing it back to Rust on load. We persist the desired width
// (what the user dragged to), not the clamped width Rust applies for the current window size, so
// growing the window later restores the intent up to the new cap.
let widthKey = null;
// Rust owns the default sidebar width (CHROME_W); window_identity delivers it as `default_width`
// so the double-press reset doesn't duplicate the literal here. Null until the first identity load.
let defaultSidebarW = null;
// The width restore pushes a width to Rust (a full relayout); it's a one-time-at-load action, so
// guard it from re-running on every config reload.
let widthRestored = false;
function saveSidebarWidth(w) {
  if (widthKey && w > 0) localStorage.setItem(widthKey, String(Math.round(w)));
}
function restoreSidebarWidth(title) {
  if (!title) return;
  widthKey = "curator:sidebar-width:" + title;
  const saved = parseFloat(localStorage.getItem(widthKey));
  // Saved width wins; otherwise apply the density-aware default (window_identity returns a
  // narrower default under compact) so first-run compact corrects from Rust's launch-time
  // CHROME_W down to its default. Skip the *comfortable* default: it equals CHROME_W, the width
  // Rust already built, so applying it would be a redundant resize/relayout. (data-density is set
  // just above in applyIdentity, before this runs.)
  const usingDefault = !(saved > 0);
  const comfortable = document.documentElement.getAttribute("data-density") !== "compact";
  const target = saved > 0 ? saved : defaultSidebarW;
  if (target > 0 && !(usingDefault && comfortable)) {
    invoke("set_sidebar_width", { width: Math.round(target) }).catch(() => {});
  }
}

// Wire the right-edge resize grip: a drag pushes the target width (= pointer x within the sidebar
// webview, whose left edge is the window's left edge) to Rust, coalesced to one call per frame;
// a quick double-press resets to Rust's default width (`defaultSidebarW`, delivered by
// window_identity so the literal isn't duplicated here). Rust clamps for the current window size
// when it lays out but keeps the desired width, so we persist the desired (sent) value here for
// grow-recovery.
function initResize() {
  const handle = document.getElementById("resize-handle");
  let dragging = false;
  let pendingX = null;
  let raf = 0;
  let lastDown = 0;
  const setWidth = (w) => {
    // Persist the desired width we sent (not Rust's clamped echo) so window-grow recovers intent.
    saveSidebarWidth(w);
    invoke("set_sidebar_width", { width: Math.round(w) }).catch(() => {});
  };
  const flush = () => {
    raf = 0;
    if (pendingX != null) {
      setWidth(pendingX);
      pendingX = null;
    }
  };
  handle.addEventListener("pointerdown", (e) => {
    // preventDefault below suppresses the synthetic dblclick event, so detect a quick second
    // press here to handle the "double-click to reset" affordance ourselves.
    if (e.timeStamp - lastDown < 300) {
      lastDown = 0;
      if (defaultSidebarW != null) setWidth(defaultSidebarW);
      return;
    }
    lastDown = e.timeStamp;
    dragging = true;
    handle.classList.add("dragging");
    handle.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  handle.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    pendingX = e.clientX;
    if (!raf) raf = requestAnimationFrame(flush);
  });
  const end = (e) => {
    if (!dragging) return;
    dragging = false;
    handle.classList.remove("dragging");
    try {
      handle.releasePointerCapture(e.pointerId);
    } catch {}
  };
  handle.addEventListener("pointerup", end);
  handle.addEventListener("pointercancel", end);
}

// Paint the per-window identity from the window's accent colour: the whole title bar (nav pill
// + name) takes a tint of the colour, with the name shown after the pill. No colour configured →
// name hidden, title bar neutral (the pill stays).
async function applyIdentity() {
  const id = await invoke("window_identity");
  // Whole-app density token applied to <html> so the CSS sizing variables switch. Re-runs on
  // `config-reloaded`, so a live density change restyles the chrome without a relaunch.
  document.documentElement.setAttribute("data-density", (id && id.density) || "comfortable");
  if (id) defaultSidebarW = id.default_width;
  // The width restore is a one-time relayout at load, not a per-reload action.
  if (!widthRestored) {
    restoreSidebarWidth(id && id.title);
    widthRestored = true;
  }
  const banner = document.getElementById("identity");
  const titlebar = document.getElementById("titlebar");
  if (!id || !id.colour) {
    banner.hidden = true;
    document.body.style.removeProperty("--active-bg");
    // Clear any accent left from a previous colour, so removing `colour` on hot-reload
    // fully reverts the title bar to neutral instead of stranding the old colour until restart.
    titlebar.style.removeProperty("background");
    return;
  }
  banner.textContent = id.title;
  banner.hidden = false;
  // The whole title bar carries a tint of the window's accent colour (lighter than the full
  // colour so the dark nav pill + white name read cleanly); the active-tab tint is stronger.
  titlebar.style.background = tintOverBase(id.colour, 0.18);
  // Tint the active-tab highlight with the same accent (a stronger blend than the bar) so
  // the selected row reads as part of this window's identity.
  document.body.style.setProperty("--active-bg", tintOverBase(id.colour, 0.28));
}

// Per-tab unread badge text, pushed from Rust via `service-badge`. Cached so a re-render
// (config reload) keeps badges, and patched live without a full re-render.
const badges = new Map();

function applyBadge(label, text) {
  const sidebar = document.getElementById("sidebar");
  const row = sidebar.querySelector(`.tab[data-label="${CSS.escape(label)}"]`);
  if (!row) return;
  let pill = row.querySelector(".badge");
  if (!text) {
    if (pill) pill.remove();
    return;
  }
  if (!pill) {
    pill = document.createElement("span");
    pill.className = "badge";
    // Insert before the actions span so the badge sits between the title and the
    // loaded/unload control.
    const actions = row.querySelector(".actions");
    row.insertBefore(pill, actions);
  }
  pill.textContent = text;
}

// Mock favicon: a colored letter-tile derived from the tab. Works for internal IPs /
// homelab hosts with no network fetch.
function tileInitial(s) {
  const m = (s || "").match(/[a-z0-9]/i);
  return m ? m[0].toUpperCase() : "•";
}
function tileColor(seed) {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  return `hsl(${h % 360}, 48%, 47%)`;
}

function setLoaded(el, loaded) {
  el.classList.toggle("loaded", loaded);
  el.title = loaded ? "Unload tab (frees memory; reloads on next click)" : "";
}

// The label of the currently-selected tab, read straight from the DOM (the active row is
// the single source of truth for selection in the chrome).
function activeLabel() {
  const el = document.querySelector(".tab.active");
  return el ? el.dataset.label : null;
}

// Nav buttons act on the active tab, so they're inert when nothing is selected.
function updateNav() {
  const has = !!activeLabel();
  for (const id of ["nav-back", "nav-forward", "nav-reload", "nav-home"]) {
    document.getElementById(id).disabled = !has;
  }
}

// Wire the nav pill once at load: paint icons and route each button to the active tab.
function initNav() {
  const wire = (id, icon, cmd) => {
    const btn = document.getElementById(id);
    btn.innerHTML = icon;
    btn.addEventListener("click", () => {
      const label = activeLabel();
      if (label) invoke(cmd, { label });
    });
  };
  wire("nav-back", BACK_SVG, "nav_back");
  wire("nav-forward", FWD_SVG, "nav_forward");
  wire("nav-reload", RELOAD_SVG, "reload_tab");
  wire("nav-home", HOME_SVG, "home_tab");
}

// SVG icons — geometry is exact, so they align perfectly across rows (unlike text glyphs).
const RELOAD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.6-6.36"/><path d="M21 3v6h-6"/></svg>`;
const DOT_SVG = `<svg class="dot" viewBox="0 0 24 24" width="20" height="20"><circle cx="12" cy="12" r="5.5" fill="#3fb950"/></svg>`;
const CROSS_SVG = `<svg class="cross" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#f85149" stroke-width="2.6" stroke-linecap="round"><path d="M7 7l10 10M17 7L7 17"/></svg>`;
const BACK_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 18l-6-6 6-6"/></svg>`;
const FWD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>`;
const HOME_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/></svg>`;

async function render() {
  const tabs = await invoke("get_tabs");
  const sidebar = document.getElementById("sidebar");
  sidebar.innerHTML = "";
  const byGroup = new Map();
  for (const t of tabs) {
    if (!byGroup.has(t.group)) byGroup.set(t.group, []);
    byGroup.get(t.group).push(t);
  }
  for (const [group, items] of byGroup) {
    // Loose tabs (group `null`) render as a leading headerless section; only real groups
    // get a section header.
    if (group) {
      const h = document.createElement("div");
      h.className = "group";
      h.textContent = group;
      sidebar.appendChild(h);
    }
    for (const t of items) {
      const row = document.createElement("div");
      row.className = "tab";
      row.dataset.label = t.label;
      if (t.active) row.classList.add("active");
      row.addEventListener("click", () => {
        if (row.classList.contains("active")) {
          invoke("home_tab", { label: t.label });
        } else {
          select(t.label, row);
        }
      });

      const fav = document.createElement("span");
      fav.className = "favicon";
      fav.textContent = tileInitial(t.title);
      fav.style.background = tileColor(t.url || t.title);
      row.appendChild(fav);

      const title = document.createElement("span");
      title.className = "tab-title";
      title.textContent = t.title;
      row.appendChild(title);

      const cached = badges.get(t.label);
      if (cached) {
        const pill = document.createElement("span");
        pill.className = "badge";
        pill.textContent = cached;
        row.appendChild(pill);
      }

      const actions = document.createElement("span");
      actions.className = "actions";

      // Loaded indicator that doubles as an unload control: a green dot when the tab's
      // webview is warm, turning into an ✕ on hover to destroy it.
      const unload = document.createElement("button");
      unload.className = "unload";
      unload.innerHTML = DOT_SVG + CROSS_SVG;
      setLoaded(unload, t.loaded);
      unload.addEventListener("click", async (e) => {
        e.stopPropagation();
        if (!unload.classList.contains("loaded")) return;
        await invoke("unload_tab", { label: t.label });
        // Unloading the active tab makes Rust promote an load_on_open tab to active (or clear
        // it); re-render so the active highlight and loaded dots follow the new state.
        await render();
      });
      actions.appendChild(unload);

      row.appendChild(actions);
      sidebar.appendChild(row);
    }
  }
  updateNav();
}

let selectGen = 0;
async function select(label, el) {
  // Move the highlight optimistically (before the await) so held-key cycling (⌘]/⌘[) reads the
  // new selection immediately instead of recomputing the same index until the IPC resolves.
  const gen = ++selectGen;
  const prev = [...document.querySelectorAll(".tab.active")];
  if (!el.classList.contains("active")) {
    for (const b of prev) b.classList.remove("active");
    el.classList.add("active");
  }
  try {
    await invoke("select_tab", { label });
  } catch {
    // Backend select failed — roll the highlight back, but only if no newer select() has since
    // moved it; otherwise we'd re-add this call's stale `prev` rows and paint two tabs active.
    if (gen === selectGen) {
      el.classList.remove("active");
      for (const b of prev) b.classList.add("active");
    }
    return;
  }
  const u = el.querySelector(".unload"); // now warm
  if (u) setLoaded(u, true);
  updateNav();
}

listen("config-reloaded", () => {
  document.getElementById("error-banner").hidden = true;
  applyIdentity();
  render();
});
listen("config-error", (e) => {
  const b = document.getElementById("error-banner");
  b.textContent = "Config error (keeping last good): " + e.payload;
  b.hidden = false;
});
listen("service-badge", (e) => {
  const { label, text } = e.payload;
  if (text) badges.set(label, text);
  else badges.delete(label);
  applyBadge(label, text);
});

// Keyboard tab navigation, driven by the Tabs menu (⌘⇧]/⌘⇧[ cycle, ⌘1–9 jump). Resolve the
// target row from the rendered order and route through select() so a lazy tab still creates.
function selectRow(row) {
  if (row && !row.classList.contains("active")) select(row.dataset.label, row);
}
listen("nav-tab", (e) => {
  const rows = [...document.querySelectorAll(".tab")];
  if (!rows.length) return;
  const dir = e.payload;
  let i = rows.findIndex((r) => r.classList.contains("active"));
  if (i < 0) i = dir > 0 ? -1 : 0;
  selectRow(rows[(i + dir + rows.length) % rows.length]);
});
listen("jump-tab", (e) => {
  selectRow([...document.querySelectorAll(".tab")][e.payload - 1]);
});

// A desktop-notification banner was clicked: the native layer already raised this window (it's
// emitted only to the originating window's chrome), so select the tab that fired it. Best-effort —
// a stale label (tab since unloaded/removed, or its URL edited so the label moved) has no row, so
// leave the active tab as-is and just surface the window.
listen("focus-tab", (e) => {
  const label = e.payload && e.payload.label;
  if (!label) return;
  selectRow(document.querySelector(`.tab[data-label="${CSS.escape(label)}"]`));
});

initNav();
initResize();
applyIdentity();
render();
