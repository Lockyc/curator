const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

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
  for (const id of ["nav-back", "nav-forward", "nav-home"]) {
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
  wire("nav-home", HOME_SVG, "home_tab");
}

// SVG icons — geometry is exact, so they align perfectly across rows (unlike text glyphs).
const RELOAD_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.6-6.36"/><path d="M21 3v6h-6"/></svg>`;
const DOT_SVG = `<svg class="dot" viewBox="0 0 24 24" width="16" height="16"><circle cx="12" cy="12" r="5.5" fill="#3fb950"/></svg>`;
const CROSS_SVG = `<svg class="cross" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="#f85149" stroke-width="2.6" stroke-linecap="round"><path d="M7 7l10 10M17 7L7 17"/></svg>`;
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
    const h = document.createElement("div");
    h.className = "group";
    h.textContent = group;
    sidebar.appendChild(h);
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

      const actions = document.createElement("span");
      actions.className = "actions";

      // Loaded indicator that doubles as an unload control: a green dot when the tab's
      // webview is warm, turning into an ✕ on hover to destroy it.
      const unload = document.createElement("button");
      unload.className = "unload";
      unload.innerHTML = DOT_SVG + CROSS_SVG;
      setLoaded(unload, t.loaded);
      unload.addEventListener("click", (e) => {
        e.stopPropagation();
        if (!unload.classList.contains("loaded")) return;
        invoke("unload_tab", { label: t.label });
        setLoaded(unload, false);
        row.classList.remove("active");
        updateNav();
      });
      actions.appendChild(unload);

      const reload = document.createElement("button");
      reload.className = "reload";
      reload.innerHTML = RELOAD_SVG;
      reload.title = "Reload " + t.title;
      reload.addEventListener("click", (e) => {
        e.stopPropagation();
        invoke("reload_tab", { label: t.label });
      });
      actions.appendChild(reload);

      row.appendChild(actions);
      sidebar.appendChild(row);
    }
  }
  updateNav();
}

async function select(label, el) {
  await invoke("select_tab", { label });
  for (const b of document.querySelectorAll(".tab")) b.classList.remove("active");
  el.classList.add("active");
  const u = el.querySelector(".unload"); // now warm
  if (u) setLoaded(u, true);
  updateNav();
}

listen("config-reloaded", () => {
  document.getElementById("error-banner").hidden = true;
  render();
});
listen("config-error", (e) => {
  const b = document.getElementById("error-banner");
  b.textContent = "Config error (keeping last good): " + e.payload;
  b.hidden = false;
});

initNav();
render();
