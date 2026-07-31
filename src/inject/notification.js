// Injected into every service webview (with escape-click + visibility). Embedded WKWebView has
// no working web Notification API, so both ways a page raises a desktop notification —
// `new Notification(...)` and `ServiceWorkerRegistration.showNotification(...)` — are silent
// no-ops. This overrides both to fire a curator notify-sentinel navigation, which the native
// on_navigation handler turns into a real macOS banner (and cancels the nav). No Tauri
// command/IPC is exposed to the page.
(function () {
  var SENTINEL = "https://curator.notify.invalid/?";
  // Per-webview secret, substituted in by Rust at injection (never exposed on window) so a page
  // can't forge a native banner by navigating to the sentinel host directly.
  var KEY = "__CURATOR_KEY__";

  function fire(title, body) {
    try {
      window.location.href =
        SENTINEL +
        "t=" +
        encodeURIComponent(title || "") +
        "&b=" +
        encodeURIComponent(body || "") +
        "&k=" +
        KEY;
    } catch (e) {}
  }

  function FakeNotification(title, opts) {
    opts = opts || {};
    fire(title, opts.body);
    // Return a stub so callers that use the instance API — `n.close()`,
    // `n.addEventListener(...)`, `n.onclick = …` (Element, Slack, Discord all do) — don't
    // throw. Clicking the native banner surfaces the originating tab (handled natively in
    // notification.rs), but the page's own `onclick`/event handlers on this stub are not invoked.
    return {
      title: title,
      body: opts.body || "",
      onclick: null,
      onshow: null,
      onclose: null,
      onerror: null,
      close: function () {},
      addEventListener: function () {},
      removeEventListener: function () {},
      dispatchEvent: function () {
        return false;
      },
    };
  }
  FakeNotification.permission = "granted";
  FakeNotification.requestPermission = function (cb) {
    if (typeof cb === "function") cb("granted");
    return Promise.resolve("granted");
  };

  // Second entry point, and the only one some apps use: ServiceWorkerRegistration
  // .showNotification(). Google Chat calls it straight from the page (not from a push handler),
  // so overriding the constructor above never sees it — and WKWebView ships no Push API and
  // displays no service-worker notification, so the native call is a silent no-op. Route it to
  // the same sentinel. Replaced outright rather than wrapped: calling through would only add a
  // rejected promise nothing can act on.
  try {
    var swProto = window.ServiceWorkerRegistration && window.ServiceWorkerRegistration.prototype;
    if (swProto) {
      swProto.showNotification = function (title, opts) {
        opts = opts || {};
        fire(title, opts.body);
        return Promise.resolve();
      };
    }
  } catch (e) {}

  try {
    Object.defineProperty(window, "Notification", {
      configurable: true,
      writable: true,
      value: FakeNotification,
    });
  } catch (e) {
    try {
      window.Notification = FakeNotification;
    } catch (e2) {}
  }
})();
