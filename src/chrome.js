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
      row.addEventListener("click", () => select(t.label, row));

      const fav = document.createElement("span");
      fav.className = "favicon";
      fav.textContent = tileInitial(t.title);
      fav.style.background = tileColor(t.url || t.title);
      row.appendChild(fav);

      const title = document.createElement("span");
      title.className = "tab-title";
      title.textContent = t.title;
      row.appendChild(title);

      const reload = document.createElement("button");
      reload.className = "reload";
      reload.textContent = "⟳";
      reload.title = "Reload " + t.title;
      reload.addEventListener("click", (e) => {
        e.stopPropagation();
        reload.classList.remove("spin");
        void reload.offsetWidth; // restart the animation if clicked repeatedly
        reload.classList.add("spin");
        invoke("reload_tab", { label: t.label });
      });
      row.appendChild(reload);

      sidebar.appendChild(row);
    }
  }
}

async function select(label, el) {
  await invoke("select_tab", { label });
  for (const b of document.querySelectorAll(".tab")) b.classList.remove("active");
  el.classList.add("active");
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

render();
