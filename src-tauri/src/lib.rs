mod config;
mod escape;
mod webviews;

use std::sync::Mutex;
use tauri::Manager;

pub struct AppState {
    pub config: Mutex<config::Config>,
    pub tabs: Mutex<webviews::TabState>,
    pub win_size: (f64, f64),
}

pub fn run() {
    let win_size = (1280.0, 860.0);
    tauri::Builder::default()
        .setup(move |app| {
            let path = config::default_config_path();
            let cfg = config::load_config(&path).unwrap_or_else(|e| {
                eprintln!("config error, starting empty: {e}");
                config::Config { groups: vec![] }
            });
            let handle = app.handle().clone();
            let window = webviews::build_window(&handle, win_size.0, win_size.1)?;

            let views = cfg.tab_views();
            let mut tab_state = webviews::TabState::default();
            // Eagerly create always_load tabs; hide them until selected.
            for v in views.iter().filter(|v| v.always_load) {
                webviews::create_content_webview(&window, v, win_size.0, win_size.1)?;
                tab_state.mark_created(&v.label);
            }
            let all_labels: Vec<String> = views.iter().map(|v| v.label.clone()).collect();
            for l in &all_labels {
                if let Some(wv) = window.get_webview(l) {
                    wv.hide()?;
                }
            }

            app.manage(AppState {
                config: Mutex::new(cfg),
                tabs: Mutex::new(tab_state),
                win_size,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running curator");
}
