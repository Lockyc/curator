use crate::{config::TabView, webviews, AppState, WindowRuntime};
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
}

/// Return the calling window's title + accent colour so the chrome can paint a per-window
/// identity banner. Colour is `None` when the window config omits it (chrome stays neutral).
#[tauri::command]
pub fn window_identity(webview: Webview, state: State<AppState>) -> WindowIdentity {
    let wid = calling_window_id(&webview);
    let windows = state.windows.lock().unwrap();
    match windows.get(&wid) {
        Some(rt) => WindowIdentity {
            title: rt.cfg.title.clone(),
            colour: rt.cfg.colour.clone(),
            default_width: webviews::CHROME_W,
        },
        None => WindowIdentity {
            title: String::new(),
            colour: None,
            default_width: webviews::CHROME_W,
        },
    }
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

/// Build the ordered TOML field list for a new tab: required `title`/`url` first, then only the
/// advanced fields the user actually set (so the written table stays minimal). Leaf list — must
/// match curator's `Tab` shape; config_core::add_tab is otherwise leaf-agnostic.
fn tab_fields(tab: &crate::config::Tab) -> Vec<(&'static str, config_core::toml_edit::Value)> {
    let mut f: Vec<(&'static str, config_core::toml_edit::Value)> = vec![
        ("title", tab.title.clone().into()),
        ("url", tab.url.clone().into()),
    ];
    if tab.load_on_open {
        f.push(("load_on_open", true.into()));
    }
    if let Some(n) = tab.reload_every {
        f.push(("reload_every", (n as i64).into()));
    }
    if let Some(s) = &tab.session {
        if !s.trim().is_empty() {
            f.push(("session", s.clone().into()));
        }
    }
    f
}

/// Clone `base` with `tab` inserted into its loose tabs (`group = None`) or the named group's
/// tabs. Errors if `group` names a group the window doesn't have (mirrors config_core::add_tab).
fn build_candidate_window(
    base: &crate::config::WindowConfig,
    group: Option<&str>,
    tab: crate::config::Tab,
) -> Result<crate::config::WindowConfig, String> {
    let mut win = base.clone();
    match group {
        None => win.tabs.push(tab),
        Some(g) => {
            let grp = win
                .groups
                .iter_mut()
                .find(|gr| gr.name == g)
                .ok_or_else(|| format!("no group named {g:?}"))?;
            grp.tabs.push(tab);
        }
    }
    Ok(win)
}

/// Add a new tab to the calling window. Validates the candidate config in memory (the same checks
/// as load), and only on success appends it to the config file via `config_core::add_tab`; the
/// file watcher then drives the reload that re-renders the sidebar. Returns an error string the
/// chrome shows inline in the add-tab form. `group = None`/`""` → the loose section.
#[allow(clippy::too_many_arguments)] // Tauri IPC commands can't wrap args in a struct at this layer
#[tauri::command]
pub fn add_tab(
    title: String,
    url: String,
    group: Option<String>,
    load_on_open: bool,
    reload_every: Option<u64>,
    session: Option<String>,
    webview: Webview,
    state: State<AppState>,
) -> Result<(), String> {
    let wid = calling_window_id(&webview);
    let group = group.filter(|g| !g.trim().is_empty());

    // Snapshot the window's config under the lock, then drop it before any file IO.
    let base = {
        let windows = state.windows.lock().unwrap();
        windows.get(&wid).ok_or("no such window")?.cfg.clone()
    };

    let tab = crate::config::Tab {
        title: title.trim().to_string(),
        url: url.trim().to_string(),
        load_on_open,
        reload_every,
        session: session.filter(|s| !s.trim().is_empty()),
    };

    // Validate the candidate (window-wide checks are all per-window, so a one-window Config is
    // sufficient and runs the identical load-time validation).
    let candidate = build_candidate_window(&base, group.as_deref(), tab.clone())?;
    let cfg = crate::config::Config {
        windows: vec![candidate],
        ..Default::default()
    };
    crate::config::validate_config(&cfg).map_err(|e| e.to_string())?;

    // Write — the watcher reload makes the new tab appear.
    let path = crate::config::resolve_config_path();
    config_core::add_tab(&path, &base.title, group.as_deref(), &tab_fields(&tab))
        .map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{parse_and_validate, validate_config, Tab};

    fn comms() -> crate::config::WindowConfig {
        parse_and_validate("[[window]]\ntitle = \"Comms\"\n[[window.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n")
            .unwrap()
            .0
            .windows
            .remove(0)
    }

    #[test]
    fn tab_fields_emits_set_fields_in_canonical_order() {
        let tab = Tab {
            title: "Cal".into(),
            url: "https://cal.test/".into(),
            load_on_open: true,
            reload_every: Some(15),
            session: Some("work".into()),
        };
        let f = tab_fields(&tab);
        let keys: Vec<_> = f.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            ["title", "url", "load_on_open", "reload_every", "session"]
        );
    }

    #[test]
    fn tab_fields_omits_unset_optionals() {
        let tab = Tab {
            title: "Cal".into(),
            url: "https://cal.test/".into(),
            load_on_open: false,
            reload_every: None,
            session: None,
        };
        let keys: Vec<_> = tab_fields(&tab).iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["title", "url"]); // load_on_open=false, no reload, no session → omitted
    }

    #[test]
    fn candidate_with_duplicate_title_is_rejected() {
        let base = comms();
        let dup = Tab {
            title: "Gmail".into(),
            url: "https://x.test/".into(),
            load_on_open: false,
            reload_every: None,
            session: None,
        };
        let win = build_candidate_window(&base, None, dup).unwrap();
        let cfg = crate::config::Config {
            windows: vec![win],
            ..Default::default()
        };
        assert!(validate_config(&cfg).is_err());
    }
}
