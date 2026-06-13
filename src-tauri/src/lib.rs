mod commands;
mod config;
mod escape;
mod watcher;
mod webviews;

use std::sync::Mutex;
use tauri::{Emitter, Manager};

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

            // Periodic reload timers for tabs with `reload_every` (minutes). Only acts on
            // already-created webviews, so a never-opened lazy tab is harmlessly skipped.
            for v in views.iter().filter(|v| v.reload_every.is_some()) {
                let mins = v.reload_every.unwrap();
                let label = v.label.clone();
                let url = v.url.clone();
                let win = window.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(mins * 60));
                    let _ = webviews::reload_canonical(&win, &label, &url);
                });
            }

            app.manage(AppState {
                config: Mutex::new(cfg),
                tabs: Mutex::new(tab_state),
                win_size,
            });

            // Watch the config file and hot-reload on change, keeping the last-good config
            // (and surfacing an error banner) if the new contents don't parse/validate.
            let watch_path = path.clone();
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                use notify::{RecursiveMode, Watcher};
                let (tx, rx) = std::sync::mpsc::channel();
                let Ok(mut watcher) = notify::recommended_watcher(tx) else {
                    return;
                };
                // Watch the parent dir, not the file: editors that atomic-save (write temp +
                // rename) replace the inode, which silently breaks a single-file watch.
                let dir = watch_path.parent().unwrap_or(&watch_path);
                if Watcher::watch(&mut watcher, dir, RecursiveMode::NonRecursive).is_err() {
                    return;
                }
                for res in rx {
                    let Ok(event) = res else { continue };
                    if !event.paths.iter().any(|p| p == &watch_path) {
                        continue;
                    }
                    let Ok(src) = std::fs::read_to_string(&watch_path) else {
                        continue;
                    };
                    let state = app_handle.state::<AppState>();
                    let current = state.config.lock().unwrap().clone();
                    match watcher::reconcile(&current, &src) {
                        Ok(cfg) => {
                            *state.config.lock().unwrap() = cfg;
                            let _ = app_handle.emit("config-reloaded", ());
                        }
                        Err(msg) => {
                            let _ = app_handle.emit("config-error", msg);
                        }
                    }
                }
            });

            // App menu: edit/reveal the config file, and reset all tabs to canonical URLs.
            use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
            let edit_cfg = MenuItemBuilder::with_id("edit_config", "Edit Config").build(app)?;
            let reveal_cfg =
                MenuItemBuilder::with_id("reveal_config", "Reveal Config in Finder").build(app)?;
            let reset = MenuItemBuilder::with_id("reset_all", "Reset All Tabs").build(app)?;
            let app_menu = SubmenuBuilder::new(app, "curator")
                .item(&PredefinedMenuItem::quit(app, None)?)
                .build()?;
            let config_menu = SubmenuBuilder::new(app, "Config")
                .items(&[&edit_cfg, &reveal_cfg, &reset])
                .build()?;
            let menu = MenuBuilder::new(app)
                .items(&[&app_menu, &config_menu])
                .build()?;
            app.set_menu(menu)?;

            let cfg_path = path.clone();
            app.on_menu_event(move |app, event| match event.id().as_ref() {
                "edit_config" => {
                    let _ = std::process::Command::new("open").arg(&cfg_path).spawn();
                }
                "reveal_config" => {
                    let _ = std::process::Command::new("open")
                        .arg("-R")
                        .arg(&cfg_path)
                        .spawn();
                }
                "reset_all" => {
                    let _ = commands::reset_all_tabs(app);
                }
                _ => {}
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_tabs,
            commands::select_tab,
            commands::reset_all
        ])
        .run(tauri::generate_context!())
        .expect("error while running curator");
}
