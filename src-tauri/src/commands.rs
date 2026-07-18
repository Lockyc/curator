use crate::{webviews, AppState};
use curator_config::{Density, TabView};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager, State, Webview, Window};

/// curator's starter config. Tracked, and `include_str!`'d so a missing/renamed template is a
/// build error rather than a runtime surprise.
const DEFAULT_CONFIG: &str = include_str!("../../src/default-config.toml");

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

/// True only when the caller is a window's trusted chrome sidebar webview. `withGlobalTauri`
/// injects the IPC bridge into *every* webview (including remote `External` content tabs) and
/// app commands aren't ACL-gated, so without this gate a remote page could invoke the whole
/// command surface — read sibling tabs' URLs via `get_tabs`, force-reload/unload/select tabs,
/// etc. The chrome is the window's MAIN webview (hole-punch), so its label IS the window label;
/// content webviews are `{window_id}:tab-<hash>`, always distinct (the same check
/// `layout_webviews` relies on).
fn is_chrome_caller(webview: &Webview) -> bool {
    label_is_chrome(webview.label(), webview.window().label())
}

/// Pure predicate behind [`is_chrome_caller`]: the chrome sidebar is a window's main webview, so
/// its label equals the window label (== window id); content webviews are `{window_id}:tab-<hash>`
/// (`url_label` always prefixes `tab-`), so they never equal the bare window label.
fn label_is_chrome(label: &str, window_label: &str) -> bool {
    label == window_label
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
    /// Whole-app `sidebar_drag`, from the global config → the chrome's `windowDrag` flag (makes the
    /// non-interactive sidebar chrome a window-move drag region). Default true.
    sidebar_drag: bool,
    /// Whole-app `auto_update`, from the global config. The chrome gates its launch-time update
    /// check on this (the manual Check-for-Updates menu path ignores it). Default true.
    auto_update: bool,
}

/// Return the calling window's title + accent colour so the chrome can paint a per-window
/// identity banner. Colour is `None` when the window config omits it (chrome stays neutral).
/// Carries the global density too — the default width follows it (compact is narrower).
#[tauri::command]
pub fn window_identity(webview: Webview, state: State<AppState>) -> WindowIdentity {
    let wid = calling_window_id(&webview);
    let density = *state.density.lock().unwrap();
    let sidebar_drag = state.sidebar_drag.load(Ordering::Relaxed);
    let auto_update = state.auto_update.load(Ordering::Relaxed);
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
            sidebar_drag,
            auto_update,
        };
    }
    let windows = state.windows.lock().unwrap();
    match windows.get(&wid) {
        Some(rt) => WindowIdentity {
            title: rt.cfg.title.clone(),
            colour: rt.cfg.colour.clone(),
            default_width,
            density,
            sidebar_drag,
            auto_update,
        },
        None => WindowIdentity {
            title: String::new(),
            colour: None,
            default_width,
            density,
            sidebar_drag,
            auto_update,
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
        // Pass the current hole rect (read under this held lock); create_content_webview must
        // not re-lock `windows` itself — that would self-deadlock the non-reentrant mutex.
        let hole = rt.hole;
        webviews::create_content_webview(&window, &target, hole).map_err(|e| e.to_string())?;
        rt.tabs.mark_created(&label);
    }
    rt.tabs.set_active(&label);

    // Raise the selected tab; load_on_open tabs stay live behind it, others are hidden.
    webviews::apply_active(&window, Some(&label), &views).map_err(|e| e.to_string())
}

/// A content-hole rect reported by the chrome (logical px, top-left), deserialized from the
/// `set_hole_rect` command's `{ rect: {x, y, width, height} }` argument.
#[derive(Deserialize)]
pub struct RectArg {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Position the calling window's content webviews from the chrome's reported `#content-hole` rect.
/// The chrome (chrome-core) owns the sidebar width and its clamp; the flex hole follows from CSS
/// and the chrome reports the measured rect here on mount, on a resize-drag, and on window resize
/// (via a ResizeObserver). Rust stores it on the runtime — so lazily-created tabs land in the
/// current hole — and repositions the existing content webviews. This is warden's `set_hole_rect`
/// model: there is no Rust-side sidebar-width computation or clamp to keep in sync with the JS.
#[tauri::command]
pub fn set_hole_rect(
    rect: RectArg,
    webview: Webview,
    state: State<AppState>,
) -> Result<(), String> {
    require_chrome(&webview)?;
    if ![rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err("non-finite hole rect".into());
    }
    let (window, wid) = calling_window(&webview)?;
    let hole = webviews::HoleRect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    };
    // Store under the lock, then reposition after releasing it (webview ops marshal to the main
    // thread; `set_hole_rect` is a sync command so they run inline, but keeping them off the lock
    // matches the window-mutex discipline in the rest of this file).
    {
        let mut windows = state.windows.lock().unwrap();
        windows.get_mut(&wid).ok_or("no such window")?.hole = hole;
    }
    webviews::layout_webviews(&window, hole);
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

/// The label to promote to active after `unloaded` is closed, given the window's tabs in render
/// order and which of them are created (curator's loaded-tab signal — the same `is_created` that
/// drives the sidebar live dot). Delegates the index policy to shell-core so warden/curator/lector
/// agree: nearest created neighbour, background only if none. `created` is parallel to `views`.
pub(crate) fn fallback_active(
    views: &[TabView],
    unloaded: &str,
    created: &[bool],
) -> Option<String> {
    let idx = views.iter().position(|v| v.label == unloaded)?;
    shell_core::pick_live_neighbour(idx, created).map(|n| views[n].label.clone())
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
    // Mark unloaded. If this was the active tab, promote the nearest created neighbour to active
    // (`fallback_active`, shell-core's `pick_live_neighbour` policy) and relayout after the lock
    // drops — without this the content area would strand a still-shown loaded webview behind a
    // sidebar that highlights nothing. If it wasn't active, the layout is untouched.
    let relayout = {
        let mut windows = state.windows.lock().unwrap();
        windows.get_mut(&wid).and_then(|rt| {
            let was_active = rt.tabs.active() == Some(label.as_str());
            rt.tabs.mark_unloaded(&label);
            if !was_active {
                return None;
            }
            let views = rt.cfg.tab_views(rt.global_session.as_deref());
            let created: Vec<bool> = views.iter().map(|v| rt.tabs.is_created(&v.label)).collect();
            let new_active = fallback_active(&views, &label, &created);
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

/// The home surface's "Create a starter config" button. This is where config-core is called (via
/// `curator_config`'s re-export — this crate never pins config-core directly, the same one-source
/// rule as its other re-exported house helpers) — shell-core owns the surface but never touches
/// config-core (the cores stay independent).
///
/// Routes through the exact same [`crate::reload_windows`] the config-file watcher uses (not a
/// bespoke "build windows from scratch" path): `reload_windows` diffs against `AppState.last_cfg`,
/// which is still `Config::default()` here (this button only shows when no config existed at all),
/// so the diff naturally treats every window the fresh config defines as newly added.
#[tauri::command]
pub fn shell_home_create_config(app: AppHandle) -> Result<(), String> {
    let path = curator_config::resolve_config_path();
    match curator_config::write_default_config(&path, DEFAULT_CONFIG) {
        // A config already existed — say so rather than report a success that didn't happen.
        Ok(false) => Err(format!(
            "{} already exists — left untouched",
            path.display()
        )),
        Ok(true) => match curator_config::load_config(&path) {
            Ok((cfg, warnings)) => {
                crate::log_config_warnings(&warnings);
                crate::reload_windows(&app, &cfg);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    }
}

/// The home surface's "Edit Config" button (shown for a `Broken` config). Reuses the spine's own
/// Edit Config action rather than a second `open` spawn — same one-source-of-truth reason the menu
/// handler consumes `handle_spine_event` first.
#[tauri::command]
pub fn shell_home_edit_config() {
    let path = curator_config::resolve_config_path();
    shell_core::menu::handle_spine_event(shell_core::menu::ids::EDIT_CONFIG, &path);
}

/// The home surface's per-window button (shown for the `Windows` list state): open, or focus if
/// already open. Same path the menu spine's Window submenu uses for the same id.
#[tauri::command]
pub fn shell_home_open_window(id: String, app: AppHandle) {
    crate::open_or_focus_window(&app, &id);
}

#[cfg(test)]
mod tests {
    use super::label_is_chrome;

    #[test]
    fn only_chrome_labels_are_callers() {
        let wid = "w0123456789abcdef";
        // The trusted sidebar is the window's main webview: its label equals the window label.
        assert!(label_is_chrome(wid, wid));
        // Content webviews (url_label always yields `{wid}:tab-<hash>`) must be rejected — this is
        // the gate keeping remote pages out of the command surface.
        assert!(!label_is_chrome(
            "w0123456789abcdef:tab-00112233445566ff",
            wid
        ));
        // A content webview from another window doesn't equal *this* window's label either.
        assert!(!label_is_chrome("wdead:tab-00112233445566ff", wid));
        assert!(!label_is_chrome("curator-error-view", "curator-error"));
        assert!(!label_is_chrome("", wid));
    }
}
