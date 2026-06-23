// Injected first into every content webview (main frame, before page load). curator registers
// the Tauri notification plugin (Rust-side, to raise native banners) and runs withGlobalTauri,
// so Tauri injects that plugin's guest init into *every* webview — including remote content
// pages, which by design have no IPC/notification capability. That guest init eagerly probes
// `plugin:notification|is_permission_granted` over IPC with no `.catch`; the ACL denies it for a
// content webview, producing an unhandled promise rejection. A page whose own global handler
// assumes every rejection has a string `.message` (Forgejo's does) then throws while reporting
// it and paints a page-breaking error banner — and the race can abort the page's own init.
//
// curator delivers banners through the sentinel-navigation shims and exposes no IPC to pages, so
// this probe is unwanted here. Swallow exactly that rejection: capture phase so we run before the
// page's own handler, stopImmediatePropagation so it never reaches it, and scoped to the
// notification-plugin command so genuine page rejections still propagate. The probe is fired by
// Tauri's plugin init before our shims run and rejects asynchronously, so registering the
// listener here (synchronously, at document-start) is always in place before it fires.
(function () {
  window.addEventListener(
    "unhandledrejection",
    function (e) {
      var r = e.reason;
      var msg = String((r && r.message) || r || "");
      if (msg.indexOf("plugin:notification") !== -1) {
        e.preventDefault();
        e.stopImmediatePropagation();
      }
    },
    true
  );
})();
