use crate::{config::Density, config::TabView, webviews, AppState};
use serde::Serialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager, State, Webview, Window};

/// A tab plus its runtime state: whether its content webview is warm, and whether it's the
/// active (visible) tab.
#[derive(Serialize)]
pub struct TabItem {
    #[serde(flatten)]
    view: TabView,
    loaded: bool,
    active: bool,
}

/// The window id of the window owning the invoking webview. A command is invoked from a
/// window's chrome sidebar, whose parent window's label *is* the window id.
fn calling_window_id(webview: &Webview) -> String {
    webview.window().label().to_string()
}

/// True only when the caller is a window's trusted `:chrome` sidebar webview. `withGlobalTauri`
/// injects the IPC bridge into *every* webview (including remote `External` content tabs) and
/// app commands aren't ACL-gated, so without this gate a remote page could invoke the whole
/// command surface — read sibling tabs' URLs via `get_tabs`, force-reload/unload/select tabs,
/// etc. Chrome webviews are labelled `{window_id}:chrome`; content webviews are
/// `{window_id}:tab-<hash>`, so the `:chrome` suffix is unique to the sidebar (the same check
/// `layout_webviews` relies on).
fn is_chrome_caller(webview: &Webview) -> bool {
    label_is_chrome(webview.label())
}

/// Pure predicate behind [`is_chrome_caller`]: a chrome sidebar's label is `{window_id}:chrome`,
/// content labels are `{window_id}:tab-<hash>` (`url_label` always prefixes `tab-`), so the
/// `:chrome` suffix is exclusive to the sidebar.
fn label_is_chrome(label: &str) -> bool {
    label.ends_with(":chrome")
}

/// Reject a command call that didn't originate from the trusted chrome sidebar.
fn require_chrome(webview: &Webview) -> Result<(), String> {
    if is_chrome_caller(webview) {
        Ok(())
    } else {
        Err("forbidden: not a chrome caller".into())
    }
}

/// Resolve the invoking webview's window handle plus a closure-friendly window id.
fn calling_window(webview: &Webview) -> Result<(Window, String), String> {
    let wid = calling_window_id(webview);
    let window = webview
        .app_handle()
        .get_window(&wid)
        .ok_or("no such window")?;
    Ok((window, wid))
}

/// The invoking window's identity for the chrome banner: its title and optional accent colour,
/// plus the default sidebar width so the chrome's double-press reset doesn't hardcode `CHROME_W`.
#[derive(Serialize)]
pub struct WindowIdentity {
    title: String,
    colour: Option<String>,
    default_width: f64,
    /// Whole-app chrome density ("comfortable" | "compact"), from the global config. The chrome
    /// sets it as `data-density` on the root so its CSS variables switch sizing.
    density: Density,
}

/// Return the calling window's title + accent colour so the chrome can paint a per-window
/// identity banner. Colour is `None` when the window config omits it (chrome stays neutral).
/// Carries the global density too — the default width follows it (compact is narrower).
#[tauri::command]
pub fn window_identity(webview: Webview, state: State<AppState>) -> WindowIdentity {
    let wid = calling_window_id(&webview);
    let density = *state.density.lock().unwrap();
    let default_width = match density {
        Density::Compact => webviews::COMPACT_CHROME_W,
        Density::Comfortable => webviews::CHROME_W,
    };
    if !is_chrome_caller(&webview) {
        return WindowIdentity {
            title: String::new(),
            colour: None,
            default_width,
            density,
        };
    }
    let windows = state.windows.lock().unwrap();
    match windows.get(&wid) {
        Some(rt) => WindowIdentity {
            title: rt.cfg.title.clone(),
            colour: rt.cfg.colour.clone(),
            default_width,
            density,
        },
        None => WindowIdentity {
            title: String::new(),
            colour: None,
            default_width,
            density,
        },
    }
}

#[tauri::command]
pub fn get_tabs(webview: Webview, state: State<AppState>) -> Vec<TabItem> {
    if !is_chrome_caller(&webview) {
        return Vec::new();
    }
    let wid = calling_window_id(&webview);
    let windows = state.windows.lock().unwrap();
    let Some(rt) = windows.get(&wid) else {
        return Vec::new();
    };
    let active = rt.tabs.active().map(str::to_string);
    rt.cfg
        .tab_views(rt.global_session.as_deref())
        .into_iter()
        .map(|view| {
            let loaded = rt.tabs.is_created(&view.label);
            let active = active.as_deref() == Some(view.label.as_str());
            TabItem {
                view,
                loaded,
                active,
            }
        })
        .collect()
}

#[tauri::command]
pub fn select_tab(label: String, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    let (window, wid) = calling_window(&webview)?;
    let mut windows = state.windows.lock().unwrap();
    let rt = windows.get_mut(&wid).ok_or("no such window")?;
    let views = rt.cfg.tab_views(rt.global_session.as_deref());
    let target = views
        .iter()
        .find(|v| v.label == label)
        .ok_or("unknown tab")?
        .clone();

    if !rt.tabs.is_created(&label) {
        // Pass the current sidebar width (read under this held lock); create_content_webview must
        // not re-lock `windows` itself — that would self-deadlock the non-reentrant mutex.
        let cw = f64::from_bits(rt.chrome_w.load(Ordering::Relaxed));
        webviews::create_content_webview(&window, &target, cw).map_err(|e| e.to_string())?;
        rt.tabs.mark_created(&label);
    }
    rt.tabs.set_active(&label);

    // Raise the selected tab; load_on_open tabs stay live behind it, others are hidden.
    webviews::apply_active(&window, Some(&label), &views).map_err(|e| e.to_string())
}

/// Set the calling window's sidebar width from a chrome resize-drag (logical px). Stores the
/// desired width, then clamps it Rust-side (range + ≤40% of the window) and re-lays-out the chrome
/// and content webviews. The chrome owns persistence — it stores the desired width it sent (for
/// grow-recovery), not Rust's clamped value — so nothing is returned here.
#[tauri::command]
pub fn set_sidebar_width(
    width: f64,
    webview: Webview,
    state: State<AppState>,
) -> Result<(), String> {
    require_chrome(&webview)?;
    if !width.is_finite() {
        return Err("non-finite width".into());
    }
    let (window, wid) = calling_window(&webview)?;
    let chrome_w = {
        let windows = state.windows.lock().unwrap();
        windows.get(&wid).ok_or("no such window")?.chrome_w.clone()
    };
    chrome_w.store(width.to_bits(), Ordering::Relaxed);
    webviews::relayout_with_width(&window, &chrome_w);
    Ok(())
}

/// Reload every already-created content webview in `wid`'s window back to its canonical URL.
fn reset_window_tabs(app: &AppHandle, wid: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let window = app.get_window(wid).ok_or("no such window")?;
    let windows = state.windows.lock().unwrap();
    let rt = windows.get(wid).ok_or("no such window")?;
    for v in &rt.cfg.tab_views(rt.global_session.as_deref()) {
        if rt.tabs.is_created(&v.label) {
            webviews::reload_canonical(&window, &v.label, &v.url).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn reset_all(webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    let wid = calling_window_id(&webview);
    reset_window_tabs(webview.app_handle(), &wid)
}

/// Refresh a single tab's current page in place (no-op if it hasn't been opened yet).
#[tauri::command]
pub fn reload_tab(label: String, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    let (window, _) = calling_window(&webview)?;
    if let Some(wv) = window.get_webview(&label) {
        wv.eval("location.reload()").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Return a single tab's content webview to its config-defined start URL. Powers both the
/// sidebar home button and the click-active-tab-again gesture. No-op if the tab's webview
/// isn't created yet.
#[tauri::command]
pub fn home_tab(label: String, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    let (window, wid) = calling_window(&webview)?;
    let url = {
        let windows = state.windows.lock().unwrap();
        let rt = windows.get(&wid).ok_or("no such window")?;
        rt.cfg
            .tab_views(rt.global_session.as_deref())
            .into_iter()
            .find(|v| v.label == label)
            .ok_or("unknown tab")?
            .url
    };
    webviews::reload_canonical(&window, &label, &url).map_err(|e| e.to_string())
}

/// Step the tab's content webview back through its in-page history. No-op if the webview
/// isn't created or there's nothing to go back to (WKWebView history isn't exposed here).
#[tauri::command]
pub fn nav_back(label: String, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    let (window, _) = calling_window(&webview)?;
    if let Some(wv) = window.get_webview(&label) {
        wv.eval("history.back()").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Step the tab's content webview forward through its in-page history. No-op if the webview
/// isn't created or there's nothing to go forward to.
#[tauri::command]
pub fn nav_forward(label: String, webview: Webview) -> Result<(), String> {
    require_chrome(&webview)?;
    let (window, _) = calling_window(&webview)?;
    if let Some(wv) = window.get_webview(&label) {
        wv.eval("history.forward()").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Destroy a tab's content webview, freeing its memory. The tab stays in the sidebar and
/// reloads lazily on next selection. No-op if it isn't loaded.
#[tauri::command]
pub fn unload_tab(label: String, webview: Webview, state: State<AppState>) -> Result<(), String> {
    require_chrome(&webview)?;
    let (window, wid) = calling_window(&webview)?;
    if let Some(wv) = window.get_webview(&label) {
        wv.close().map_err(|e| e.to_string())?;
    }
    // Mark unloaded. If this was the active tab, promote an already-live `load_on_open` tab to
    // active (mirroring launch's active-resolution) and relayout after the lock drops — without
    // this the content area would strand a still-shown `load_on_open` webview behind a sidebar
    // that highlights nothing. If it wasn't active, the layout is untouched.
    let relayout = {
        let mut windows = state.windows.lock().unwrap();
        windows.get_mut(&wid).and_then(|rt| {
            let was_active = rt.tabs.active() == Some(label.as_str());
            rt.tabs.mark_unloaded(&label);
            if !was_active {
                return None;
            }
            let views = rt.cfg.tab_views(rt.global_session.as_deref());
            let new_active = views
                .iter()
                .find(|v| v.load_on_open && v.label != label && rt.tabs.is_created(&v.label))
                .map(|v| v.label.clone());
            if let Some(a) = &new_active {
                rt.tabs.set_active(a);
            }
            Some((views, new_active))
        })
    };
    // Drop the gone webview's unread contribution: clear its sidebar pill and refresh the dock
    // badge. The closed webview can never send a clear, so without this its count is stranded.
    crate::awareness::forget_tab(webview.app_handle(), &wid, &label);
    if let Some((views, new_active)) = relayout {
        webviews::apply_active(&window, new_active.as_deref(), &views)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Window id to drive from a menu command: the focused window (menu items act on whichever
/// window has key focus).
fn focused_window_id(app: &AppHandle) -> Option<String> {
    app.get_focused_window().map(|w| w.label().to_string())
}

/// Run `f` against the focused window's active content webview, if there is one. Resolves the
/// focused window, reads its active tab label (under the `windows` lock, dropped before acting),
/// and looks up that tab's webview — a no-op if no window is focused, nothing is active, or the
/// active tab's webview isn't created yet.
fn with_focused_active_webview(app: &AppHandle, f: impl FnOnce(&Webview)) {
    let Some(wid) = focused_window_id(app) else {
        return;
    };
    let active = {
        let state = app.state::<AppState>();
        let windows = state.windows.lock().unwrap();
        windows
            .get(&wid)
            .and_then(|rt| rt.tabs.active().map(str::to_string))
    };
    if let (Some(label), Some(window)) = (active, app.get_window(&wid)) {
        if let Some(wv) = window.get_webview(&label) {
            f(&wv);
        }
    }
}

/// Reload the focused window's active tab (Cmd+R / menu). No-op if nothing is active.
pub fn reload_active_tab(app: &AppHandle) {
    with_focused_active_webview(app, |wv| {
        let _ = wv.eval("location.reload()");
    });
}

/// Open the WebKit Web Inspector on the focused window's active tab (menu "Open Developer
/// Tools" / ⌥⌘I). No-op if nothing is active. Compiled in for both dev and release via the
/// `devtools` Cargo feature, so the inspector is available in deployed builds too.
pub fn open_active_devtools(app: &AppHandle) {
    with_focused_active_webview(app, |wv| wv.open_devtools());
}

/// Reset the focused window's tabs (menu "Reset All Tabs"). No-op if no window is focused.
pub fn reset_all_tabs(app: &AppHandle) -> Result<(), String> {
    let Some(wid) = focused_window_id(app) else {
        return Ok(());
    };
    reset_window_tabs(app, &wid)
}

#[cfg(test)]
mod tests {
    use super::label_is_chrome;

    #[test]
    fn only_chrome_labels_are_callers() {
        // The trusted sidebar.
        assert!(label_is_chrome("w0123456789abcdef:chrome"));
        // Content webviews (url_label always yields `tab-<hash>`) must be rejected — this is the
        // gate keeping remote pages out of the command surface.
        assert!(!label_is_chrome("w0123456789abcdef:tab-00112233445566ff"));
        // A window whose id somehow embedded `:chrome` still can't smuggle a content tab through:
        // the content label ends in the tab hash, not `:chrome`.
        assert!(!label_is_chrome("wdead:chrome:tab-00112233445566ff"));
        assert!(!label_is_chrome("curator-error-view"));
        assert!(!label_is_chrome(""));
    }
}
