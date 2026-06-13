use crate::{config::TabView, webviews, AppState};
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn get_tabs(state: State<AppState>) -> Vec<TabView> {
    state.config.lock().unwrap().tab_views()
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
    let (w, h) = state.win_size;

    {
        let mut tabs = state.tabs.lock().unwrap();
        if !tabs.is_created(&label) {
            webviews::create_content_webview(&main, &target, w, h).map_err(|e| e.to_string())?;
            tabs.mark_created(&label);
        }
        tabs.set_active(&label);
    }

    let all: Vec<String> = views.iter().map(|v| v.label.clone()).collect();
    webviews::show_only(&main, &label, &all).map_err(|e| e.to_string())?;
    Ok(())
}
