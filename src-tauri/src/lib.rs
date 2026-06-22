mod commands;
mod config;
mod escape;
mod hash;
mod identity;
#[cfg(target_os = "macos")]
mod insecure;
mod session;
mod watcher;
mod webviews;

use std::sync::Mutex;
use tauri::{Emitter, Manager, Theme};

/// Window theme to apply for a given `dark_mode` setting. `None` = follow the system.
fn theme_for(dark_mode: bool) -> Option<Theme> {
    dark_mode.then_some(Theme::Dark)
}

pub struct AppState {
    pub config: Mutex<config::WindowConfig>,
    pub tabs: Mutex<webviews::TabState>,
}

pub fn run() {
    tauri::Builder::default()
        .setup(move |app| {
            let path = config::resolve_config_path();
            let cfg = config::load_config(&path).unwrap_or_else(|e| {
                eprintln!("config error, starting empty: {e}");
                config::Config::default()
            });
            let Some(win_cfg) = cfg.windows.first().cloned() else {
                return Ok(());
            };
            #[cfg(target_os = "macos")]
            insecure::set_allowlist(cfg.allow_insecure.clone());
            let handle = app.handle().clone();
            let window =
                webviews::build_window(&handle, win_cfg.width as f64, win_cfg.height as f64)?;
            window.set_theme(theme_for(cfg.dark_mode))?;

            let views = win_cfg.tab_views();
            let mut tab_state = webviews::TabState::default();
            // Eagerly create always_load tabs; hide them until selected.
            for v in views.iter().filter(|v| v.always_load) {
                webviews::create_content_webview(&window, v)?;
                tab_state.mark_created(&v.label);
            }
            let all_labels: Vec<String> = views.iter().map(|v| v.label.clone()).collect();
            for l in &all_labels {
                if let Some(wv) = window.get_webview(l) {
                    wv.hide()?;
                }
            }

            // Open a tab on launch if configured (`open_on_launch`), so we don't land on the
            // blank placeholder screen.
            if let Some(label) = win_cfg.startup_label() {
                if let Some(v) = views.iter().find(|v| v.label == label) {
                    if !tab_state.is_created(&label) {
                        webviews::create_content_webview(&window, v)?;
                        tab_state.mark_created(&label);
                    }
                    tab_state.set_active(&label);
                    webviews::show_only(&window, &label, &all_labels)?;
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
                config: Mutex::new(win_cfg),
                tabs: Mutex::new(tab_state),
            });

            // Watch the config file and hot-reload on change, keeping the last-good config
            // (and surfacing an error banner) if the new contents don't parse/validate.
            let watch_path = path.clone();
            let app_handle = app.handle().clone();
            let dark_mode = cfg.dark_mode;
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
                        Ok(win_cfg) => {
                            let keep: std::collections::HashSet<String> =
                                win_cfg.tab_views().into_iter().map(|v| v.label).collect();
                            if let Some(win) = app_handle.get_window("main") {
                                let _ = win.set_theme(theme_for(dark_mode));
                                // Close webviews orphaned by this reload (a tab whose URL
                                // changed gets a new label; a removed tab drops out entirely).
                                // Left alone they linger visible and surface on unload.
                                let mut tabs = state.tabs.lock().unwrap();
                                for label in tabs.orphans(&keep) {
                                    if let Some(wv) = win.get_webview(&label) {
                                        let _ = wv.close();
                                    }
                                    tabs.mark_unloaded(&label);
                                }
                            }
                            *state.config.lock().unwrap() = win_cfg;
                            let _ = app_handle.emit("config-reloaded", ());
                        }
                        Err(msg) => {
                            let _ = app_handle.emit("config-error", msg);
                        }
                    }
                }
            });

            // We replace Tauri's default menu, so we must re-add the standard macOS menus it
            // would otherwise provide. The Edit menu in particular owns the clipboard
            // accelerators (⌘C/⌘V/⌘X/⌘A/⌘Z) — without it, webview text fields can't paste.
            use tauri::menu::{AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
            let about_meta = AboutMetadataBuilder::new()
                .name(Some("curator"))
                .version(Some(env!("CARGO_PKG_VERSION")))
                .short_version(Some(env!("CURATOR_GIT_SHA")))
                .comments(Some(format!(
                    "commit {} · built {}",
                    env!("CURATOR_GIT_SHA"),
                    env!("CURATOR_BUILD_DATE"),
                )))
                .build();
            let reload_tab = MenuItemBuilder::with_id("reload_active", "Reload Tab")
                .accelerator("CmdOrCtrl+R")
                .build(app)?;
            let reset = MenuItemBuilder::with_id("reset_all", "Reset All Tabs").build(app)?;
            let edit_cfg = MenuItemBuilder::with_id("edit_config", "Edit Config").build(app)?;
            let reveal_cfg =
                MenuItemBuilder::with_id("reveal_config", "Reveal Config in Finder").build(app)?;
            let app_menu = SubmenuBuilder::new(app, "curator")
                .about(Some(about_meta))
                .separator()
                .services()
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .quit()
                .build()?;
            // Standard Edit menu — this is what makes clipboard shortcuts work in content
            // webviews (logging into sites, typing anywhere). Don't drop it.
            let edit_menu = SubmenuBuilder::new(app, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;
            let tabs_menu = SubmenuBuilder::new(app, "Tabs")
                .items(&[&reload_tab, &reset])
                .build()?;
            let config_menu = SubmenuBuilder::new(app, "Config")
                .items(&[&edit_cfg, &reveal_cfg])
                .build()?;
            // Window menu — minimize / zoom / full screen. No Close Window (⌘W): curator is
            // single-window with no reopen path, so closing the only window strands the app.
            let window_menu = SubmenuBuilder::new(app, "Window")
                .minimize()
                .maximize()
                .fullscreen()
                .build()?;
            let menu = MenuBuilder::new(app)
                .items(&[
                    &app_menu,
                    &edit_menu,
                    &tabs_menu,
                    &config_menu,
                    &window_menu,
                ])
                .build()?;
            app.set_menu(menu)?;

            let cfg_path = path.clone();
            app.on_menu_event(move |app, event| match event.id().as_ref() {
                "reload_active" => {
                    commands::reload_active_tab(app);
                }
                "reset_all" => {
                    let _ = commands::reset_all_tabs(app);
                }
                "edit_config" => {
                    let _ = std::process::Command::new("open").arg(&cfg_path).spawn();
                }
                "reveal_config" => {
                    let _ = std::process::Command::new("open")
                        .arg("-R")
                        .arg(&cfg_path)
                        .spawn();
                }
                _ => {}
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_tabs,
            commands::select_tab,
            commands::reset_all,
            commands::reload_tab,
            commands::unload_tab,
            commands::home_tab,
            commands::nav_back,
            commands::nav_forward
        ])
        .run(tauri::generate_context!())
        .expect("error while running curator");
}
