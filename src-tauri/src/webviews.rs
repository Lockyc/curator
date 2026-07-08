use std::collections::HashSet;

/// Pure bookkeeping for which content webviews have been created (lazy-load tracking)
/// and which is active. Webview side-effects live in the Tauri-aware code below.
#[derive(Default)]
pub struct TabState {
    created: HashSet<String>,
    active: Option<String>,
}

impl TabState {
    pub fn is_created(&self, label: &str) -> bool {
        self.created.contains(label)
    }
    pub fn mark_created(&mut self, label: &str) {
        self.created.insert(label.to_string());
    }
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }
    pub fn set_active(&mut self, label: &str) {
        self.active = Some(label.to_string());
    }
    pub fn mark_unloaded(&mut self, label: &str) {
        self.created.remove(label);
        if self.active.as_deref() == Some(label) {
            self.active = None;
        }
    }
    /// Created-webview labels absent from `keep` (the new config's labels) — orphaned by a
    /// reload that changed a tab's URL (its hash-derived label moves) or removed the tab.
    /// Pure: the caller closes each webview and calls `mark_unloaded`. Without this, an
    /// orphan lingers (`apply_active` only lays out tabs in the live config) and surfaces
    /// when the covering tab is unloaded.
    pub fn orphans(&self, keep: &HashSet<String>) -> Vec<String> {
        self.created
            .iter()
            .filter(|l| !keep.contains(*l))
            .cloned()
            .collect()
    }
}

use crate::escape;
use curator_config::TabView;
use tauri::{
    webview::{NewWindowResponse, WebviewBuilder},
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalSize, TitleBarStyle, WebviewUrl,
    Window, WindowEvent,
};

/// Window label for the fallback error window (config failed to load / no windows defined).
/// Not a real window id, so it never collides with a `w<hex>` window.
pub const WINDOW_ERROR: &str = "curator-error";
/// Webview label inside the error window (so we can refresh its message on a later failure).
const ERROR_VIEW: &str = "curator-error-view";

/// Default sidebar width, delivered to the chrome via `window_identity` as its reset/first-run
/// default. It is *only* a default: the chrome (chrome-core) owns the sidebar width and its clamp,
/// and Rust learns the resulting content geometry from the chrome's reported [`HoleRect`] — Rust no
/// longer computes or clamps a sidebar width itself. Also the initial best-guess hole ([`initial_hole`])
/// until the first `set_hole_rect` arrives.
pub const CHROME_W: f64 = 240.0;
/// Default sidebar width under `density = "compact"` — narrower to match the condensed type.
/// Same role as [`CHROME_W`]: the chrome's reset/first-run default for the compact mode.
pub const COMPACT_CHROME_W: f64 = 200.0;
const DESKTOP_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Click-interceptor that reroutes cmd/middle-clicks through the escape sentinel.
const ESCAPE_CLICK_JS: &str = include_str!("../../src/inject/escape-click.js");
/// Drives WebKit's `visibilitychange`/`focus` so live services keep syncing while hidden.
const VISIBILITY_SHIM_JS: &str = include_str!("../../src/inject/visibility.js");
/// Reroutes web `Notification` calls through the notify sentinel for a native banner.
const NOTIFICATION_JS: &str = include_str!("../../src/inject/notification.js");
/// Reroutes the Badging API through the badge sentinel for unread pills + dock badge.
const BADGE_JS: &str = include_str!("../../src/inject/badge.js");

/// A per-webview anti-forgery key, baked into that webview's injected shims as a function-local
/// literal (never exposed on `window`, so page scripts can't read it) and required on every
/// sentinel navigation. Drawn straight from the OS CSPRNG (`getrandom`) — 128 bits a page can't
/// guess to forge a banner/badge/browser-escape by navigating to a sentinel host directly. (Using
/// the CSPRNG directly, rather than hashing under `RandomState`, keeps the unforgeability property
/// from resting on any assumption about SipHash-as-PRF.)
fn random_nonce() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS CSPRNG (getrandom) must succeed on macOS");
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Current inner size of the window in logical px.
fn logical_inner(window: &Window) -> (f64, f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let size = window.inner_size().unwrap_or(PhysicalSize::new(1280, 860));
    (size.width as f64 / scale, size.height as f64 / scale)
}

/// The content hole's rect in logical px (top-left origin), exactly as the chrome measures its
/// `#content-hole` element via `getBoundingClientRect` and reports it through `set_hole_rect`.
///
/// This is the single source of truth for content-webview placement. The chrome (chrome-core) owns
/// the sidebar width and clamps it; the flex `#content-hole` follows from CSS; the chrome reports
/// the resulting rect here. Rust just tracks and applies it — it no longer recomputes the geometry
/// from a sidebar width or clamps anything (which is what removed the old Rust↔JS clamp duplication).
/// Tauri's `LogicalPosition`/`LogicalSize` are also top-left, so — unlike warden's bottom-left
/// native `NSView` surface — no coordinate flip is needed; the reported rect is used as-is.
#[derive(Debug, Clone, Copy)]
pub struct HoleRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The best-guess hole before the chrome's first `set_hole_rect`: full height, offset by the
/// default sidebar width. The first report corrects it — this only has to place launch-time
/// `load_on_open` tabs sensibly for the frame or two before the chrome mounts and measures.
pub fn initial_hole(win_w: f64, win_h: f64) -> HoleRect {
    HoleRect {
        x: CHROME_W,
        y: 0.0,
        width: (win_w - CHROME_W).max(0.0),
        height: win_h,
    }
}

/// Position every content webview to fill the reported hole. The chrome is the window's main
/// webview (auto-sized to the window as its content view; its label IS the window label), so it's
/// skipped — only the `add_child` content webviews are placed. All loaded tabs stack in the same
/// hole; `apply_active` raises the active one, `load_on_open` tabs sit live behind it.
pub fn layout_webviews(window: &Window, hole: HoleRect) {
    for wv in window.webviews() {
        if wv.label() == window.label() {
            continue;
        }
        let _ = wv.set_position(LogicalPosition::new(hole.x, hole.y));
        let _ = wv.set_size(LogicalSize::new(hole.width.max(0.0), hole.height.max(0.0)));
    }
}

/// Build a window and its chrome (sidebar) webview, and wire the user-close handler.
/// `window_id` becomes the window label and namespaces the content webview labels.
/// Returns the window; the caller seeds its runtime hole rect from [`initial_hole`].
pub fn build_window(
    app: &AppHandle,
    window_id: &str,
    title: &str,
    win_w: f64,
    win_h: f64,
) -> tauri::Result<Window> {
    // The sidebar chrome is the window's MAIN webview (hole-punch, warden-style): built as the
    // window's content view, so `data-tauri-drag-region` in it moves the window natively — a child
    // (`add_child`) webview cannot (that's why the whole-window drag was inert before). It spans the
    // whole window (index.html renders the sidebar in a left column and leaves a "hole" on the
    // right); the Rust-positioned content webviews are `add_child` siblings, added later so they
    // composite ABOVE this webview over the hole. `hidden_title` drops the OS title (the in-app
    // banner names the window); the traffic lights float over the sidebar's padding-top inset.
    //
    // The main webview's label IS the window label (window_id) — Tauri ties them. Content webviews
    // are `{window_id}:tab-<hash>`, so `label == window.label()` uniquely identifies the chrome
    // (see `is_chrome_label` in commands.rs and the skip in `layout_webviews`). `core:event` stays
    // off remote content because capabilities apply to local app URLs only (content is `External`).
    let webview_window =
        tauri::WebviewWindowBuilder::new(app, window_id, WebviewUrl::App("index.html".into()))
            .title(title)
            .inner_size(win_w, win_h)
            .hidden_title(true)
            .title_bar_style(TitleBarStyle::Overlay)
            .build()?;
    let window = webview_window.as_ref().window();

    // Saved bounds (size/position/maximized) are restored by tauri-plugin-window-state's own
    // `window_created` hook, which runs on the main thread *inside* the event loop — where its
    // `set_size`/`set_position` (and the monitor-intersection check that keeps a stale off-screen
    // position from stranding the window) resolve inline. Every window is covered except WINDOW_ERROR
    // (`skip_initial_state` in lib.rs, which must not restore its throwaway bounds).
    //
    // FOOTGUN: do NOT call `window.restore_state(...)` here. It looks right — windows are built at
    // runtime, so restore them by hand — but `restore_state` reads/sets geometry via calls that
    // marshal to the main event loop, and `build_window` always runs *off* it (the setup hook runs
    // before the loop starts; hot-reload runs on the watcher thread). Off the loop that marshal
    // blocks: a self-hang at launch, or a mutex-holding deadlock against the auto-hook on reload. It
    // stayed invisible while no window title hashed to a persisted state entry (restore
    // short-circuited before the marshal); the first matching title — e.g. renaming a window onto an
    // old entry — froze the app.

    // Route a user close (native red button or ⌘W) through the shared close logic so it can't
    // strand the app and doesn't leak the window's unread/timers (see lib.rs). Content-webview
    // repositioning is NOT wired here: the chrome's `#content-hole` is a flex child, so a window
    // resize reflows it in the webview, the chrome's ResizeObserver fires `reportRect`, and the
    // resulting `set_hole_rect` repositions the content — the same JS-reported path warden uses.
    // (No Rust-side resize relayout means no Rust-side sidebar-width clamp to keep in sync.)
    let close_app = app.clone();
    let close_wid = window_id.to_string();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. }
            if crate::on_real_window_close(&close_app, &close_wid) =>
        {
            api.prevent_close();
        }
        _ => {}
    });

    Ok(window)
}

/// Create a content webview for `view` in the given window. Its login store is keyed on the
/// resolved `view.session` (tabs sharing a session string share a login; the default is one
/// shared app-wide store). Every tab gets the full shim set, so any loaded tab can fire native
/// banners and report unread — whether it notifies in the background is purely a function of
/// whether it's kept live, which is what `load_on_open` controls (see `apply_active`).
///
/// `hole` is the window's current content-hole rect (the value stored on the runtime, seeded from
/// [`initial_hole`] and updated by every `set_hole_rect`), so a newly-created tab lands exactly
/// where the chrome measured the hole. It's passed in (not read from `AppState` here) because
/// callers already hold the `windows` lock when they create a tab — re-locking it inside would
/// self-deadlock the non-reentrant mutex.
pub fn create_content_webview(
    window: &Window,
    view: &TabView,
    hole: HoleRect,
) -> tauri::Result<()> {
    let url: url::Url = view.url.parse().expect("url validated at config load");

    // Per-webview secret keying the sentinel handlers; substituted into the shims that emit
    // sentinel navigations so only our own injected code can trigger them.
    let nonce = random_nonce();
    let escape_js = ESCAPE_CLICK_JS.replace("__CURATOR_KEY__", &nonce);
    let notification_js = NOTIFICATION_JS.replace("__CURATOR_KEY__", &nonce);
    let badge_js = BADGE_JS.replace("__CURATOR_KEY__", &nonce);
    let init = format!("{escape_js}\n;\n{VISIBILITY_SHIM_JS}\n;\n{notification_js}\n;\n{badge_js}");

    let nav_app = window.app_handle().clone();
    let nav_label = view.label.clone();
    // The owning window's id (== label), so a notification can route a banner click back to this
    // window's chrome to surface the tab that fired it (see notification::fire / did_receive).
    let nav_window_id = window.label().to_string();
    let nav_nonce = nonce;
    // Captured separately for the new-window handler (the above are moved into on_navigation).
    let open_app = window.app_handle().clone();
    let open_label = view.label.clone();
    let home_url = view.url.clone();

    let builder = WebviewBuilder::new(&view.label, WebviewUrl::External(url))
        // Let WKWebView deliver native OS file drops to the page's own HTML5 drop targets
        // (attach-to-compose, upload boxes, …). Tauri's default drag-drop handler consumes the
        // drop (emits a `tauri://drag-drop` event and returns `true`), which stops WKWebView from
        // ever seeing it — curator listens for no such event, so disabling it is pure gain. The
        // drop lands on the active tab only: `apply_active` raises it to the front of the
        // superview, occluding the live-but-background `load_on_open` tabs across the content rect.
        .disable_drag_drop_handler()
        .data_store_identifier(crate::session::data_store_id(&view.session))
        .user_agent(DESKTOP_UA)
        .initialization_script(&init)
        .on_new_window(move |url, _features| {
            // Keep the app's own popups/auth flows (same site as the tab) in-app by navigating
            // the tab itself, so sign-in completes in the tab's own login session. Genuinely
            // external links (a different site) still escape to the default browser.
            if escape::same_site(&home_url, &url) {
                if let Some(wv) = open_app.get_webview(&open_label) {
                    let _ = wv.navigate(url);
                }
                return NewWindowResponse::Deny;
            }
            if escape::is_escapable_scheme(&url) {
                escape::escape_to_default_browser(url.as_str());
            }
            NewWindowResponse::Deny
        })
        .on_navigation(move |url| {
            // Sentinel hosts drive native banners / unread badges / browser-escape. Honour them
            // only when the navigation carries this webview's secret key — otherwise a page
            // could forge one by navigating to the host directly. Forged or unrecognised
            // sentinels are swallowed (never navigated to the dead host).
            if escape::is_sentinel_host(url) {
                if !escape::sentinel_key_ok(url, &nav_nonce) {
                    return false;
                }
                if let Some(sig) = escape::badge_sentinel(url) {
                    crate::awareness::on_badge_signal(&nav_app, &nav_label, sig);
                } else if let Some(p) = escape::notify_sentinel(url) {
                    crate::notification::fire(&p.title, &p.body, &nav_window_id, &nav_label);
                } else if let Some(target) = escape::sentinel_target(url) {
                    escape::escape_to_default_browser(&target);
                }
                return false;
            }
            escape::allow_same_tab_navigation(url.as_str())
        })
        .on_document_title_changed(|webview, title| {
            crate::awareness::on_title_changed(&webview, &title);
        });

    let webview = window.add_child(
        builder,
        LogicalPosition::new(hole.x, hole.y),
        LogicalSize::new(hole.width.max(0.0), hole.height.max(0.0)),
    )?;
    #[cfg(target_os = "macos")]
    {
        let _ = webview.with_webview(|pw| crate::insecure::ensure_patched(pw.inner()));
    }
    Ok(())
}

/// Escape a string for embedding inside a double-quoted JS string literal.
fn js_string_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Build a standalone error window shown when the config can't be loaded or defines no windows.
/// A single webview fills it and displays `message`; the app menu (incl. Config ▸ Edit Config)
/// still works, so the user can fix the config — after which hot-reload opens the real windows
/// and closes this one. Labelled `WINDOW_ERROR` so the reload path can find and close it.
pub fn build_error_window(app: &AppHandle, message: &str) -> tauri::Result<Window> {
    let window = tauri::window::WindowBuilder::new(app, WINDOW_ERROR)
        .title("curator")
        .inner_size(720.0, 420.0)
        .title_bar_style(TitleBarStyle::Overlay)
        .build()?;

    let view = WebviewBuilder::new(ERROR_VIEW, WebviewUrl::App("error.html".into()))
        .initialization_script(format!(
            "window.__CURATOR_ERROR__ = \"{}\";",
            js_string_escape(message)
        ));
    let (w, h) = logical_inner(&window);
    window.add_child(view, LogicalPosition::new(0.0, 0.0), LogicalSize::new(w, h))?;
    Ok(window)
}

/// Update the open error window's message in place (a later config save that still fails). Eval
/// is a Rust→webview push, so it needs no capability. No-op if the error window isn't open.
pub fn refresh_error_window(app: &AppHandle, message: &str) {
    if let Some(view) = app
        .get_window(WINDOW_ERROR)
        .and_then(|w| w.get_webview(ERROR_VIEW))
    {
        let _ = view.eval(format!(
            "var m=document.getElementById('msg'); if (m) m.textContent = \"{}\";",
            js_string_escape(message)
        ));
    }
}

/// Navigate a content webview back to its canonical URL (reset / periodic reload).
/// No-op if the webview hasn't been created yet (a never-opened lazy tab is skipped).
pub fn reload_canonical(window: &Window, label: &str, canonical_url: &str) -> tauri::Result<()> {
    if let Some(wv) = window.get_webview(label) {
        let url: url::Url = canonical_url.parse().expect("url validated at config load");
        wv.navigate(url)?;
    }
    Ok(())
}

/// Raise `label` to the front without hiding others (hiding throttles their sync). Live
/// windows switch tabs with this. No-op if the webview doesn't exist.
pub fn raise(window: &Window, label: &str) -> tauri::Result<()> {
    if let Some(_wv) = window.get_webview(label) {
        #[cfg(target_os = "macos")]
        {
            let _ = _wv.with_webview(|pw| crate::zorder::raise_to_front(pw.inner()));
        }
    }
    Ok(())
}

/// Lay out a window's created webviews around the `active` tab: the active tab is shown and
/// raised to the front; `load_on_open` tabs stay shown (live behind it, so they keep syncing and
/// can notify in the background); every other created tab is hidden (and thus throttled). This
/// is the single switching primitive — `load_on_open` alone decides what stays live.
pub fn apply_active(window: &Window, active: Option<&str>, views: &[TabView]) -> tauri::Result<()> {
    for v in views {
        if let Some(wv) = window.get_webview(&v.label) {
            if v.load_on_open || Some(v.label.as_str()) == active {
                wv.show()?;
            } else {
                wv.hide()?;
            }
        }
    }
    if let Some(label) = active {
        raise(window, label)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_created_and_active() {
        let mut s = TabState::default();
        assert!(!s.is_created("tab-0"));
        s.mark_created("tab-0");
        assert!(s.is_created("tab-0"));
        assert_eq!(s.active(), None);
        s.set_active("tab-0");
        assert_eq!(s.active(), Some("tab-0"));
    }

    #[test]
    fn unloading_clears_created_and_active() {
        let mut s = TabState::default();
        s.mark_created("tab-0");
        s.set_active("tab-0");
        s.mark_unloaded("tab-0");
        assert!(!s.is_created("tab-0"));
        assert_eq!(s.active(), None);
    }

    #[test]
    fn unloading_a_background_tab_keeps_active() {
        let mut s = TabState::default();
        s.mark_created("tab-0");
        s.mark_created("tab-1");
        s.set_active("tab-1");
        s.mark_unloaded("tab-0");
        assert!(!s.is_created("tab-0"));
        assert_eq!(s.active(), Some("tab-1"));
    }

    #[test]
    fn orphans_are_created_labels_missing_from_new_config() {
        // A tab's URL was edited: its label moved from nextdns-old to nextdns-new while a
        // webview is still live under the old label. Another tab is unchanged.
        let mut s = TabState::default();
        s.mark_created("nextdns-old");
        s.mark_created("grafana");
        s.set_active("nextdns-old");

        let keep: HashSet<String> = ["nextdns-new", "grafana"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let orphans = s.orphans(&keep);
        assert_eq!(orphans, vec!["nextdns-old".to_string()]);

        // Reload teardown the watcher performs for each orphan.
        for l in &orphans {
            s.mark_unloaded(l);
        }
        assert!(!s.is_created("nextdns-old")); // orphan closed
        assert_eq!(s.active(), None); // was active → cleared, so content falls back to blank
        assert!(s.is_created("grafana")); // surviving tab untouched
        assert!(s.orphans(&keep).is_empty()); // nothing left to prune
    }
}
