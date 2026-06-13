const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

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
      const el = document.createElement("button");
      el.className = "tab";
      el.textContent = t.title;
      el.dataset.label = t.label;
      el.addEventListener("click", () => select(t.label, el));
      sidebar.appendChild(el);
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
