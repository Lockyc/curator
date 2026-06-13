use crate::{config::TabView, webviews, AppState};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// A tab plus its runtime state: whether its content webview is warm, and whether it's the
/// active (visible) tab.
#[derive(Serialize)]
pub struct TabItem {
    #[serde(flatten)]
    view: TabView,
    loaded: bool,
    active: bool,
}

#[tauri::command]
pub fn get_tabs(state: State<AppState>) -> Vec<TabItem> {
    let cfg = state.config.lock().unwrap();
    let tabs = state.tabs.lock().unwrap();
    let active = tabs.active().map(str::to_string);
    cfg.tab_views()
        .into_iter()
        .map(|view| {
            let loaded = tabs.is_created(&view.label);
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
pub fn select_tab(label: String, app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let main = app.get_window("main").ok_or("no main window")?;
    let views = state.config.lock().unwrap().tab_views();
    let target = views
        .iter()
        .find(|v| v.label == label)
        .ok_or("unknown tab")?
        .clone();

    {
        let mut tabs = state.tabs.lock().unwrap();
        if !tabs.is_created(&label) {
            webviews::create_content_webview(&main, &target).map_err(|e| e.to_string())?;
            tabs.mark_created(&label);
        }
        tabs.set_active(&label);
    }

    let all: Vec<String> = views.iter().map(|v| v.label.clone()).collect();
    webviews::show_only(&main, &label, &all).map_err(|e| e.to_string())?;
    Ok(())
}

/// Reload every already-created content webview back to its canonical URL. Shared by the
/// `reset_all` command and the "Reset All Tabs" menu item.
pub fn reset_all_tabs(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let main = app.get_window("main").ok_or("no main window")?;
    let views = state.config.lock().unwrap().tab_views();
    let tabs = state.tabs.lock().unwrap();
    for v in &views {
        if tabs.is_created(&v.label) {
            webviews::reload_canonical(&main, &v.label, &v.url).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn reset_all(app: AppHandle) -> Result<(), String> {
    reset_all_tabs(&app)
}

/// Refresh a single tab's current page in place (no-op if it hasn't been opened yet).
#[tauri::command]
pub fn reload_tab(label: String, app: AppHandle) -> Result<(), String> {
    let main = app.get_window("main").ok_or("no main window")?;
    if let Some(wv) = main.get_webview(&label) {
        wv.eval("location.reload()").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Destroy a tab's content webview, freeing its memory. The tab stays in the sidebar and
/// reloads lazily on next selection. No-op if it isn't loaded.
#[tauri::command]
pub fn unload_tab(label: String, app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let main = app.get_window("main").ok_or("no main window")?;
    if let Some(wv) = main.get_webview(&label) {
        wv.close().map_err(|e| e.to_string())?;
    }
    state.tabs.lock().unwrap().mark_unloaded(&label);
    Ok(())
}

/// Refresh the currently-active tab's page (Cmd+R / menu). No-op if nothing is active.
pub fn reload_active_tab(app: &AppHandle) {
    let active = app
        .state::<AppState>()
        .tabs
        .lock()
        .unwrap()
        .active()
        .map(str::to_string);
    if let (Some(label), Some(main)) = (active, app.get_window("main")) {
        if let Some(wv) = main.get_webview(&label) {
            let _ = wv.eval("location.reload()");
        }
    }
}
