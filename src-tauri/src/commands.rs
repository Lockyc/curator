use crate::{config::TabView, webviews, AppState, WindowRuntime};
use serde::Serialize;
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

/// Resolve the invoking webview's window handle plus a closure-friendly window id.
fn calling_window(webview: &Webview) -> Result<(Window, String), String> {
    let wid = calling_window_id(webview);
    let window = webview
        .app_handle()
        .get_window(&wid)
        .ok_or("no such window")?;
    Ok((window, wid))
}

#[tauri::command]
pub fn get_tabs(webview: Webview, state: State<AppState>) -> Vec<TabItem> {
    let wid = calling_window_id(&webview);
    let windows = state.windows.lock().unwrap();
    let Some(rt) = windows.get(&wid) else {
        return Vec::new();
    };
    let active = rt.tabs.active().map(str::to_string);
    rt.cfg
        .tab_views()
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
    let (window, wid) = calling_window(&webview)?;
    let mut windows = state.windows.lock().unwrap();
    let rt = windows.get_mut(&wid).ok_or("no such window")?;
    let views = rt.cfg.tab_views();
    let target = views
        .iter()
        .find(|v| v.label == label)
        .ok_or("unknown tab")?
        .clone();

    if !rt.tabs.is_created(&label) {
        webviews::create_content_webview(&window, &target).map_err(|e| e.to_string())?;
        rt.tabs.mark_created(&label);
    }
    rt.tabs.set_active(&label);

    // Raise the selected tab; always_load tabs stay live behind it, others are hidden.
    webviews::apply_active(&window, Some(&label), &views).map_err(|e| e.to_string())
}

/// Reload every already-created content webview in `wid`'s window back to its canonical URL.
fn reset_window_tabs(app: &AppHandle, wid: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let window = app.get_window(wid).ok_or("no such window")?;
    let windows = state.windows.lock().unwrap();
    let rt = windows.get(wid).ok_or("no such window")?;
    for v in &rt.cfg.tab_views() {
        if rt.tabs.is_created(&v.label) {
            webviews::reload_canonical(&window, &v.label, &v.url).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn reset_all(webview: Webview) -> Result<(), String> {
    let wid = calling_window_id(&webview);
    reset_window_tabs(webview.app_handle(), &wid)
}

/// Refresh a single tab's current page in place (no-op if it hasn't been opened yet).
#[tauri::command]
pub fn reload_tab(label: String, webview: Webview) -> Result<(), String> {
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
    let (window, wid) = calling_window(&webview)?;
    let url = {
        let windows = state.windows.lock().unwrap();
        let rt = windows.get(&wid).ok_or("no such window")?;
        rt.cfg
            .tab_views()
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
    let (window, wid) = calling_window(&webview)?;
    if let Some(wv) = window.get_webview(&label) {
        wv.close().map_err(|e| e.to_string())?;
    }
    {
        let mut windows = state.windows.lock().unwrap();
        if let Some(rt) = windows.get_mut(&wid) {
            rt.tabs.mark_unloaded(&label);
        }
    }
    // Drop the gone webview's unread contribution: clear its sidebar pill and refresh the dock
    // badge. The closed webview can never send a clear, so without this its count is stranded.
    crate::awareness::forget_tab(webview.app_handle(), &wid, &label);
    Ok(())
}

/// Window id to drive from a menu command: the focused window (menu items act on whichever
/// window has key focus).
fn focused_window_id(app: &AppHandle) -> Option<String> {
    app.get_focused_window().map(|w| w.label().to_string())
}

/// Reload the focused window's active tab (Cmd+R / menu). No-op if nothing is active.
pub fn reload_active_tab(app: &AppHandle) {
    let Some(wid) = focused_window_id(app) else {
        return;
    };
    let active = {
        let state = app.state::<AppState>();
        let windows = state.windows.lock().unwrap();
        windows
            .get(&wid)
            .and_then(|rt: &WindowRuntime| rt.tabs.active().map(str::to_string))
    };
    if let (Some(label), Some(window)) = (active, app.get_window(&wid)) {
        if let Some(wv) = window.get_webview(&label) {
            let _ = wv.eval("location.reload()");
        }
    }
}

/// Open the WebKit Web Inspector on the focused window's active tab (menu "Open Developer
/// Tools" / ⌥⌘I). No-op if nothing is active. Compiled in for both dev and release via the
/// `devtools` Cargo feature, so the inspector is available in deployed builds too.
pub fn open_active_devtools(app: &AppHandle) {
    let Some(wid) = focused_window_id(app) else {
        return;
    };
    let active = {
        let state = app.state::<AppState>();
        let windows = state.windows.lock().unwrap();
        windows
            .get(&wid)
            .and_then(|rt: &WindowRuntime| rt.tabs.active().map(str::to_string))
    };
    if let (Some(label), Some(window)) = (active, app.get_window(&wid)) {
        if let Some(wv) = window.get_webview(&label) {
            wv.open_devtools();
        }
    }
}

/// Reset the focused window's tabs (menu "Reset All Tabs"). No-op if no window is focused.
pub fn reset_all_tabs(app: &AppHandle) -> Result<(), String> {
    let Some(wid) = focused_window_id(app) else {
        return Ok(());
    };
    reset_window_tabs(app, &wid)
}
