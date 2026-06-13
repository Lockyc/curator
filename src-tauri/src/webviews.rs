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
}

use crate::config::TabView;
use crate::escape;
use tauri::{
    webview::{NewWindowResponse, WebviewBuilder},
    AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl, Window,
};

const CHROME_W: f64 = 240.0;
/// 16 bytes → one shared persistent session store for all content webviews.
const SESSION_STORE: [u8; 16] = *b"curator-session1";
/// Click-interceptor that reroutes cmd/middle-clicks through the escape sentinel.
const ESCAPE_CLICK_JS: &str = include_str!("../../src/inject/escape-click.js");

/// Build the main window and the chrome (sidebar) webview. Returns the window.
pub fn build_window(app: &AppHandle, win_w: f64, win_h: f64) -> tauri::Result<Window> {
    let window = tauri::window::WindowBuilder::new(app, "main")
        .title("curator")
        .inner_size(win_w, win_h)
        .build()?;

    let chrome = WebviewBuilder::new("chrome", WebviewUrl::App("index.html".into()));
    window.add_child(
        chrome,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(CHROME_W, win_h),
    )?;
    Ok(window)
}

/// Lazily create a content webview for `tab`, positioned in the content area. Idempotent
/// via the caller's TabState. `on_new_window` denies in-app creation and escapes to Velja;
/// `on_navigation` allows same-tab navigation.
pub fn create_content_webview(
    window: &Window,
    tab: &TabView,
    win_w: f64,
    win_h: f64,
) -> tauri::Result<()> {
    let url: url::Url = tab.url.parse().expect("url validated at config load");
    let builder = WebviewBuilder::new(&tab.label, WebviewUrl::External(url))
        .data_store_identifier(SESSION_STORE)
        .initialization_script(ESCAPE_CLICK_JS)
        .on_new_window(|url, _features| {
            escape::escape_to_default_browser(url.as_str());
            NewWindowResponse::Deny
        })
        .on_navigation(|url| {
            // cmd/middle-click sentinel → escape to Velja, cancel the in-app nav.
            if let Some(target) = escape::sentinel_target(url) {
                escape::escape_to_default_browser(&target);
                return false;
            }
            escape::allow_same_tab_navigation(url.as_str())
        });

    window.add_child(
        builder,
        LogicalPosition::new(CHROME_W, 0.0),
        LogicalSize::new(win_w - CHROME_W, win_h),
    )?;
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
