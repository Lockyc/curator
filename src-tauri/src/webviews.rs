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
    /// orphan lingers visible (show_only only hides labels in the live config) and surfaces
    /// when the covering tab is unloaded.
    pub fn orphans(&self, keep: &HashSet<String>) -> Vec<String> {
        self.created
            .iter()
            .filter(|l| !keep.contains(*l))
            .cloned()
            .collect()
    }
}

use crate::config::TabView;
use crate::escape;
use tauri::{
    webview::{NewWindowResponse, WebviewBuilder},
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalSize, TitleBarStyle, WebviewUrl,
    Window, WindowEvent,
};

pub const CHROME_W: f64 = 240.0;
/// macOS title-bar height. The title bar is an overlay (transparent, floating traffic
/// lights); the content webview paints over it full-height while the chrome sidebar is
/// inset by this much, leaving the native title-bar strip exposed only above the tab list.
pub const TITLEBAR_H: f64 = 28.0;
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

/// Current inner size of the window in logical px.
fn logical_inner(window: &Window) -> (f64, f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let size = window.inner_size().unwrap_or(PhysicalSize::new(1280, 860));
    (size.width as f64 / scale, size.height as f64 / scale)
}

/// Position of a content webview within the window (right of the chrome sidebar).
fn content_position() -> LogicalPosition<f64> {
    LogicalPosition::new(CHROME_W, 0.0)
}

/// Size of a content webview for the given window dimensions.
fn content_size(w: f64, h: f64) -> LogicalSize<f64> {
    LogicalSize::new((w - CHROME_W).max(0.0), h)
}

/// Re-lay-out the chrome (fixed-width sidebar) and every content webview (filling the rest)
/// to the window's current size. Called on every window resize.
fn layout_webviews(window: &Window) {
    let (w, h) = logical_inner(window);
    for wv in window.webviews() {
        let is_chrome = wv.label().ends_with(":chrome");
        let (pos, size) = if is_chrome {
            (
                LogicalPosition::new(0.0, TITLEBAR_H),
                LogicalSize::new(CHROME_W, (h - TITLEBAR_H).max(0.0)),
            )
        } else {
            (content_position(), content_size(w, h))
        };
        let _ = wv.set_position(pos);
        let _ = wv.set_size(size);
    }
}

/// Build a window and its chrome (sidebar) webview, and wire window-resize relayout.
/// `window_id` becomes the window label and namespaces the chrome/placeholder webview labels.
/// Returns the window.
pub fn build_window(
    app: &AppHandle,
    window_id: &str,
    title: &str,
    win_w: f64,
    win_h: f64,
) -> tauri::Result<Window> {
    let window = tauri::window::WindowBuilder::new(app, window_id)
        .title(title)
        .inner_size(win_w, win_h)
        .title_bar_style(TitleBarStyle::Overlay)
        .build()?;

    let chrome_label = crate::identity::namespaced(window_id, "chrome");
    let chrome = WebviewBuilder::new(&chrome_label, WebviewUrl::App("index.html".into()));
    window.add_child(
        chrome,
        LogicalPosition::new(0.0, TITLEBAR_H),
        LogicalSize::new(CHROME_W, (win_h - TITLEBAR_H).max(0.0)),
    )?;

    // Blank-screen placeholder (muted grey app icon), shown in the content area behind every
    // content webview. Content webviews are added later, so they stack on top and cover it
    // when a tab is open; it shows through when nothing is selected.
    let ph_label = crate::identity::namespaced(window_id, "placeholder");
    let placeholder = WebviewBuilder::new(&ph_label, WebviewUrl::App("placeholder.html".into()));
    window.add_child(placeholder, content_position(), content_size(win_w, win_h))?;

    // Resize/reposition all webviews whenever the window resizes or changes DPI. The handler
    // queries webviews() live, so content webviews created later are covered too.
    let win = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => layout_webviews(&win),
        _ => {}
    });

    Ok(window)
}

/// Create a content webview for `view` in the given window. The webview's session store is
/// per-(window,tab); injection + navigation handlers are chosen from the window's flags:
/// plain windows inject only the escape-click shim; live windows add the visibility shim,
/// plus notification/badge shims for the features they opted into.
pub fn create_content_webview(
    window: &Window,
    window_id: &str,
    win_cfg: &crate::config::WindowConfig,
    view: &TabView,
) -> tauri::Result<()> {
    let url: url::Url = view.url.parse().expect("url validated at config load");
    let seed = crate::identity::session_seed(window_id, &view.url);

    let mut init = ESCAPE_CLICK_JS.to_string();
    if win_cfg.is_live() {
        init.push_str("\n;\n");
        init.push_str(VISIBILITY_SHIM_JS);
    }
    if win_cfg.notifications {
        init.push_str("\n;\n");
        init.push_str(NOTIFICATION_JS);
    }
    if win_cfg.unread {
        init.push_str("\n;\n");
        init.push_str(BADGE_JS);
    }

    let nav_app = window.app_handle().clone();
    let nav_label = view.label.clone();
    let unread = win_cfg.unread;
    let notifications = win_cfg.notifications;

    let mut builder = WebviewBuilder::new(&view.label, WebviewUrl::External(url))
        .data_store_identifier(crate::session::data_store_id(&seed))
        .user_agent(DESKTOP_UA)
        .initialization_script(&init)
        .on_new_window(|url, _features| {
            if escape::is_escapable_scheme(&url) {
                escape::escape_to_default_browser(url.as_str());
            }
            NewWindowResponse::Deny
        })
        .on_navigation(move |url| {
            if unread {
                if let Some(sig) = escape::badge_sentinel(url) {
                    crate::awareness::on_badge_signal(&nav_app, &nav_label, sig);
                    return false;
                }
            }
            if notifications {
                if let Some(p) = escape::notify_sentinel(url) {
                    crate::notification::fire(&nav_app, &p.title, &p.body);
                    return false;
                }
            }
            if let Some(target) = escape::sentinel_target(url) {
                escape::escape_to_default_browser(&target);
                return false;
            }
            escape::allow_same_tab_navigation(url.as_str())
        });

    if win_cfg.unread {
        builder = builder.on_document_title_changed(|webview, title| {
            crate::awareness::on_title_changed(&webview, &title);
        });
    }

    let (w, h) = logical_inner(window);
    let webview = window.add_child(builder, content_position(), content_size(w, h))?;
    #[cfg(target_os = "macos")]
    {
        let _ = webview.with_webview(|pw| crate::insecure::ensure_patched(pw.inner()));
    }
    Ok(())
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

/// Show `label`'s content webview and hide all others in `all_labels`.
pub fn show_only(window: &Window, label: &str, all_labels: &[String]) -> tauri::Result<()> {
    for l in all_labels {
        if let Some(wv) = window.get_webview(l) {
            if l == label {
                wv.show()?;
            } else {
                wv.hide()?;
            }
        }
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
