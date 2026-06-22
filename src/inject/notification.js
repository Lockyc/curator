// Injected into every service webview (with escape-click + visibility). Embedded WKWebView
// has no working web Notification API, so chat apps' new Notification(...) calls are no-ops.
// This overrides it to fire a curator notify-sentinel navigation, which the native
// on_navigation handler turns into a real macOS banner (and cancels the nav). No Tauri
// command/IPC is exposed to the page.
(function () {
  var SENTINEL = "https://curator.notify.invalid/?";

  function fire(title, body) {
    try {
      window.location.href =
        SENTINEL + "t=" + encodeURIComponent(title || "") + "&b=" + encodeURIComponent(body || "");
    } catch (e) {}
  }

  function FakeNotification(title, opts) {
    opts = opts || {};
    fire(title, opts.body);
    // Return a stub so callers that use the instance API — `n.close()`,
    // `n.addEventListener(...)`, `n.onclick = …` (Element, Slack, Discord all do) — don't
    // throw. We don't route clicks back to JS yet (v1), so the handlers are inert.
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
