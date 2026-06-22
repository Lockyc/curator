mod awareness;
mod commands;
mod config;
mod escape;
mod hash;
mod identity;
#[cfg(target_os = "macos")]
mod insecure;
mod notification;
mod session;
mod watcher;
mod webviews;
#[cfg(target_os = "macos")]
mod zorder;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tauri::{Emitter, Manager, Theme};

/// Window theme to apply for a given `dark_mode` setting. `None` = follow the system.
fn theme_for(dark_mode: bool) -> Option<Theme> {
    dark_mode.then_some(Theme::Dark)
}

/// Per-window runtime state: its config, lazy/active tab bookkeeping, and the awareness state
/// (per-tab unread + which tabs have gone Badging-authoritative) feeding the dock badge.
pub struct WindowRuntime {
    pub cfg: config::WindowConfig,
    pub tabs: webviews::TabState,
    pub unread: HashMap<String, awareness::Unread>,
    pub badge_authoritative: HashSet<String>,
}

/// The whole app's window registry, keyed by window id (== window label == chrome prefix).
pub struct AppState {
    pub windows: Mutex<HashMap<String, WindowRuntime>>,
}

impl AppState {
    /// Every window's unread states flattened — input to the single aggregate dock badge.
    pub fn all_unread(&self) -> Vec<awareness::Unread> {
        self.windows
            .lock()
            .unwrap()
            .values()
            .flat_map(|w| w.unread.values().copied())
            .collect()
    }
}

/// Build one window from its config, populate its content webviews per its lifecycle model
/// (live: eager-load all + raise first, never hide; plain: lazy-load, hide all, open the
/// startup tab via show_only), and start its per-tab reload timers. Returns the window id and
/// its fresh `WindowRuntime` for the caller to register. Shared by setup and hot-reload.
fn open_window(
    handle: &tauri::AppHandle,
    dark_mode: bool,
    win_cfg: &config::WindowConfig,
) -> tauri::Result<(String, WindowRuntime)> {
    let wid = identity::window_id(&win_cfg.title);
    let window = webviews::build_window(
        handle,
        &wid,
        &win_cfg.title,
        win_cfg.width as f64,
        win_cfg.height as f64,
    )?;
    window.set_theme(theme_for(dark_mode))?;

    let views = win_cfg.tab_views();
    let mut tabs = webviews::TabState::default();

    if win_cfg.is_live() {
        // Live window: eager-load every tab, raise the first, never hide.
        for v in &views {
            webviews::create_content_webview(&window, &wid, win_cfg, v)?;
            tabs.mark_created(&v.label);
        }
        if let Some(first) = views.first() {
            tabs.set_active(&first.label);
            webviews::raise(&window, &first.label)?;
        }
    } else {
        // Plain window: lazy-load; eager-create only always_load tabs, hide all, then open
        // the startup tab if configured.
        for v in views.iter().filter(|v| v.always_load) {
            webviews::create_content_webview(&window, &wid, win_cfg, v)?;
            tabs.mark_created(&v.label);
        }
        let all_labels: Vec<String> = views.iter().map(|v| v.label.clone()).collect();
        for l in &all_labels {
            if let Some(wv) = window.get_webview(l) {
                wv.hide()?;
            }
        }
        if let Some(label) = win_cfg.startup_label() {
            if let Some(v) = views.iter().find(|v| v.label == label) {
                if !tabs.is_created(&label) {
                    webviews::create_content_webview(&window, &wid, win_cfg, v)?;
                    tabs.mark_created(&label);
                }
                tabs.set_active(&label);
                webviews::show_only(&window, &label, &all_labels)?;
            }
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

    Ok((
        wid,
        WindowRuntime {
            cfg: win_cfg.clone(),
            tabs,
            unread: HashMap::new(),
            badge_authoritative: HashSet::new(),
        },
    ))
}

/// Emit an event to every open window's chrome sidebar. Used for `config-error` (which all
/// windows surface) and per-window `config-reloaded` fan-out.
fn emit_to_all_chrome<S: serde::Serialize + Clone>(
    app: &tauri::AppHandle,
    event: &str,
    payload: S,
) {
    let state = app.state::<AppState>();
    let ids: Vec<String> = state.windows.lock().unwrap().keys().cloned().collect();
    for id in ids {
        let _ = app.emit_to(identity::namespaced(&id, "chrome"), event, payload.clone());
    }
}

/// Apply a successful config reload: close windows that disappeared (dropping their runtime),
/// open windows that appeared, and reconcile tabs for windows that stayed. Emits
/// `config-reloaded` to each kept/added window's chrome.
fn reload_windows(app: &tauri::AppHandle, old_cfg: &config::Config, new_cfg: &config::Config) {
    let diff = watcher::diff_windows(old_cfg, new_cfg);
    let state = app.state::<AppState>();

    // Closed windows: drop the window and its runtime.
    for id in &diff.removed {
        if let Some(win) = app.get_window(id) {
            let _ = win.close();
        }
        state.windows.lock().unwrap().remove(id);
    }

    // New windows: build and register.
    for id in &diff.added {
        if let Some(win_cfg) = new_cfg
            .windows
            .iter()
            .find(|w| &identity::window_id(&w.title) == id)
        {
            if let Ok((wid, rt)) = open_window(app, new_cfg.dark_mode, win_cfg) {
                state.windows.lock().unwrap().insert(wid, rt);
            }
        }
    }

    // Kept windows: reconcile tabs against the new config.
    for id in &diff.kept {
        let Some(win_cfg) = new_cfg
            .windows
            .iter()
            .find(|w| &identity::window_id(&w.title) == id)
        else {
            continue;
        };
        let Some(window) = app.get_window(id) else {
            continue;
        };
        let _ = window.set_theme(theme_for(new_cfg.dark_mode));
        reconcile_window_tabs(&state, &window, id, win_cfg);
    }

    // Surface the reload on every window that's still open.
    emit_to_all_chrome(app, "config-reloaded", ());
}

/// Reconcile one kept window's content webviews to its new config: create newly-added tabs
/// (live windows eager-load them; plain windows leave them lazy), close orphaned webviews and
/// drop their unread/authoritative state, recompute the dock badge, then re-show the active
/// tab. Mirrors the single-window watcher's teardown, per window.
fn reconcile_window_tabs(
    state: &AppState,
    window: &tauri::Window,
    window_id: &str,
    win_cfg: &config::WindowConfig,
) {
    let views = win_cfg.tab_views();
    let keep: HashSet<String> = views.iter().map(|v| v.label.clone()).collect();
    let all_labels: Vec<String> = views.iter().map(|v| v.label.clone()).collect();

    // Mutate this window's runtime under a single lock; collect what we need for the
    // side-effecting webview ops afterwards (lock dropped first to avoid re-entrancy).
    let (active, dock_total) = {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(window_id) else {
            return;
        };
        rt.cfg = win_cfg.clone();

        // Close orphans (removed tabs, or tabs whose URL — hence label — changed) and forget
        // all their state.
        for label in rt.tabs.orphans(&keep) {
            if let Some(wv) = window.get_webview(&label) {
                let _ = wv.close();
            }
            rt.tabs.mark_unloaded(&label);
            rt.unread.remove(&label);
            rt.badge_authoritative.remove(&label);
        }

        // Live windows eager-load every (including newly-added) tab so they sync immediately.
        if win_cfg.is_live() {
            for v in &views {
                if !rt.tabs.is_created(&v.label) {
                    let _ = webviews::create_content_webview(window, window_id, win_cfg, v);
                    rt.tabs.mark_created(&v.label);
                }
            }
        }

        let active = rt.tabs.active().map(str::to_string);
        // Recompute the single dock badge now that this window's unread set may have shrunk.
        let dock_total = awareness::dock_count(
            &windows
                .values()
                .flat_map(|w| w.unread.values().copied())
                .collect::<Vec<_>>(),
        );
        (active, dock_total)
    };

    if let Some(win) = window.app_handle().get_window(window_id) {
        let _ = win.set_badge_count(dock_total);
    }

    // Re-show/raise the active tab so the content area isn't left on a closed orphan.
    if let Some(active) = active {
        if win_cfg.is_live() {
            let _ = webviews::raise(window, &active);
        } else {
            let _ = webviews::show_only(window, &active, &all_labels);
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            let path = config::resolve_config_path();
            let mut cfg = config::load_config(&path).unwrap_or_else(|e| {
                eprintln!("config error, starting empty: {e}");
                config::Config::default()
            });
            #[cfg(target_os = "macos")]
            insecure::set_allowlist(cfg.allow_insecure.clone());

            let handle = app.handle().clone();
            let mut runtimes: HashMap<String, WindowRuntime> = HashMap::new();
            for win_cfg in &cfg.windows {
                let (wid, rt) = open_window(&handle, cfg.dark_mode, win_cfg)?;
                runtimes.insert(wid, rt);
            }
            app.manage(AppState {
                windows: Mutex::new(runtimes),
            });

            // Extract what we need from cfg before the watcher thread takes ownership of it.
            let dark_mode = cfg.dark_mode;
            let window_titles: Vec<(String, String)> = cfg
                .windows
                .iter()
                .map(|w| (identity::window_id(&w.title), w.title.clone()))
                .collect();

            // Watch the config file and hot-reload on change, keeping the last-good config
            // (and surfacing an error banner on each open window) if the new contents don't
            // parse/validate.
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
                    match watcher::reconcile(&cfg, &src) {
                        Ok(new_cfg) => {
                            reload_windows(&app_handle, &cfg, &new_cfg);
                            cfg = new_cfg;
                        }
                        Err(msg) => emit_to_all_chrome(&app_handle, "config-error", msg),
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
            // Window menu — minimize / zoom / full screen; Close Window (⌘W) with a >1 guard
            // so the last window can never be closed (prevents stranding the app); and one item
            // per configured window so any closed window can be reopened from the menu.
            let close_window = MenuItemBuilder::with_id("close_window", "Close Window")
                .accelerator("CmdOrCtrl+W")
                .build(app)?;
            let mut window_menu = SubmenuBuilder::new(app, "Window")
                .minimize()
                .maximize()
                .fullscreen()
                .separator()
                .item(&close_window)
                .separator();
            for (wid, title) in &window_titles {
                let item =
                    MenuItemBuilder::with_id(format!("open_window:{wid}"), title).build(app)?;
                window_menu = window_menu.item(&item);
            }
            let window_menu = window_menu.build()?;
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
                "close_window" => {
                    // Never close the last window — that would strand the app with no reopen
                    // path. We keep the WindowRuntime in the registry so its cfg survives for
                    // reopen via the per-window menu items below.
                    let windows = app.windows();
                    if windows.len() > 1 {
                        if let Some(win) =
                            windows.values().find(|w| w.is_focused().unwrap_or(false))
                        {
                            let _ = win.close();
                        }
                    }
                }
                id if id.starts_with("open_window:") => {
                    let wid = &id["open_window:".len()..];
                    if let Some(win) = app.get_window(wid) {
                        // Window is already open — just focus it.
                        let _ = win.set_focus();
                    } else {
                        // Window was closed; reopen it from the retained cfg in the registry.
                        // Clone the cfg and drop the lock before calling open_window to avoid
                        // holding the registry lock across webview construction.
                        let state = app.state::<AppState>();
                        let win_cfg = {
                            let windows = state.windows.lock().unwrap();
                            windows.get(wid).map(|rt| rt.cfg.clone())
                        };
                        if let Some(cfg) = win_cfg {
                            if let Ok((new_wid, rt)) = open_window(app, dark_mode, &cfg) {
                                state.windows.lock().unwrap().insert(new_wid, rt);
                            }
                        }
                    }
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
