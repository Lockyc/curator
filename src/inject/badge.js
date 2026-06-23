// Injected into every service webview (with escape-click + visibility + notification).
// WebKit exposes the Badging API only to home-screen PWAs, so an embedded webview has no
// working navigator.setAppBadge — chat apps feature-detect it as absent and never report a
// count. This defines it (only if absent) to fire a curator badge-sentinel navigation,
// which the native on_navigation handler turns into a per-service unread badge (and cancels
// the nav). No Tauri command/IPC is exposed to the page.
(function () {
  var SENTINEL = "https://curator.badge.invalid/?";
  // Per-webview secret, substituted in by Rust at injection (never exposed on window) so a page
  // can't forge an unread badge by navigating to the sentinel host directly.
  var KEY = "__CURATOR_KEY__";

  function fire(qs) {
    try {
      window.location.href = SENTINEL + qs + "&k=" + KEY;
    } catch (e) {}
  }

  function setAppBadge(count) {
    if (arguments.length === 0 || count === undefined || count === null) {
      fire("dot=1");
    } else {
      var n = Math.floor(Number(count));
      if (!(n >= 0)) n = 0; // NaN / negative → clear
      fire("n=" + n);
    }
    return Promise.resolve();
  }

  function clearAppBadge() {
    fire("n=0");
    return Promise.resolve();
  }

  try {
    if (!("setAppBadge" in navigator)) {
      navigator.setAppBadge = setAppBadge;
      navigator.clearAppBadge = clearAppBadge;
    }
  } catch (e) {}
})();
