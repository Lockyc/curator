// Injected into every content webview (main frame, before page load). WKWebView routes
// target="_blank" / window.open through the native on_new_window handler, but cmd-click and
// middle-click arrive only as ordinary main-frame navigations. This catches those two
// gestures and reroutes them through a sentinel URL that the native on_navigation handler
// recognises and escapes to Velja — so no command is ever exposed to remote pages. It also
// maps the mouse side-buttons to history back/forward, which WKWebView ignores by default.
(function () {
  var SENTINEL = "https://curator.escape.invalid/?u=";

  function anchorFrom(target) {
    var el = target;
    while (el && el.tagName !== "A") el = el.parentElement;
    return el && el.href ? el : null;
  }

  function isHttp(href) {
    return /^https?:\/\//i.test(href);
  }

  function escapeTo(href) {
    window.location.href = SENTINEL + encodeURIComponent(href);
  }

  // cmd-click ("open elsewhere" muscle memory) → escape.
  document.addEventListener(
    "click",
    function (e) {
      if (!e.metaKey) return;
      var a = anchorFrom(e.target);
      if (!a || !isHttp(a.href)) return;
      e.preventDefault();
      e.stopPropagation();
      escapeTo(a.href);
    },
    true
  );

  // middle-click → escape. mousedown(button===1) is the earliest hook and the best chance
  // to suppress WKWebView's own navigation before it starts.
  document.addEventListener(
    "mousedown",
    function (e) {
      if (e.button !== 1) return;
      var a = anchorFrom(e.target);
      if (!a || !isHttp(a.href)) return;
      e.preventDefault();
      e.stopPropagation();
      escapeTo(a.href);
    },
    true
  );

  // Belt-and-suspenders: cancel any default middle-click action that still slips through.
  document.addEventListener(
    "auxclick",
    function (e) {
      if (e.button !== 1) return;
      var a = anchorFrom(e.target);
      if (!a || !isHttp(a.href)) return;
      e.preventDefault();
      e.stopPropagation();
    },
    true
  );

  // Mouse side-buttons → history navigation. WKWebView delivers these as ordinary mouse
  // events (button 3 = back, button 4 = forward) but, unlike Safari, never acts on them, so
  // we drive the page's own history. mouseup is the reliable hook for the side buttons.
  document.addEventListener(
    "mouseup",
    function (e) {
      if (e.button === 3) {
        e.preventDefault();
        e.stopPropagation();
        history.back();
      } else if (e.button === 4) {
        e.preventDefault();
        e.stopPropagation();
        history.forward();
      }
    },
    true
  );
})();
