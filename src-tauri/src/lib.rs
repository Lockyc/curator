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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, Theme};

/// Window theme to apply for a given `dark_mode` setting. `None` = follow the system.
fn theme_for(dark_mode: bool) -> Option<Theme> {
    dark_mode.then_some(Theme::Dark)
}

/// Per-window runtime state: its config, lazy/active tab bookkeeping, and the awareness state
/// (per-tab unread + which tabs have gone Badging-authoritative) feeding the dock badge.
pub struct WindowRuntime {
    pub cfg: config::WindowConfig,
    /// App-wide session base (`Config.session`) captured for this window, so commands and reopen
    /// can re-resolve the session chain without the whole `Config`. Kept in sync on hot-reload.
    pub global_session: Option<String>,
    pub tabs: webviews::TabState,
    pub unread: HashMap<String, awareness::Unread>,
    pub badge_authoritative: HashSet<String>,
    /// Set true to stop this window's `reload_every` timer threads — on window close, removal,
    /// or when its tab set changes (a fresh generation is spawned with a new flag).
    pub reload_cancel: Arc<AtomicBool>,
    /// Current sidebar width in logical px (f64 bits), shared with the window's resize closure.
    /// Driven by the `set_sidebar_width` drag command and re-clamped on window resize.
    pub chrome_w: Arc<AtomicU64>,
}

/// Spawn one background thread per `reload_every` tab that reloads it on schedule until
/// `cancel` is set. Sleeps in 1s chunks so a cancelled timer exits promptly rather than after
/// a full interval, and never reloads after cancellation — so closed/removed windows don't
/// leak threads that keep poking dead webviews.
fn spawn_reload_timers(window: &tauri::Window, views: &[config::TabView], cancel: Arc<AtomicBool>) {
    for v in views.iter().filter(|v| v.reload_every.is_some()) {
        let interval = std::time::Duration::from_secs(v.reload_every.unwrap() * 60);
        let label = v.label.clone();
        let url = v.url.clone();
        let win = window.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let tick = std::time::Duration::from_secs(1);
            loop {
                let mut waited = std::time::Duration::ZERO;
                while waited < interval {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let chunk = tick.min(interval - waited);
                    std::thread::sleep(chunk);
                    waited += chunk;
                }
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let _ = webviews::reload_canonical(&win, &label, &url);
            }
        });
    }
}

/// The whole app's window registry, keyed by window id (== window label == chrome prefix).
pub struct AppState {
    pub windows: Mutex<HashMap<String, WindowRuntime>>,
    /// Current app-wide `dark_mode`, kept live across hot-reload so Window-menu reopen themes a
    /// window to match every other open window (not the stale launch-time value).
    pub dark_mode: AtomicBool,
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

/// Build one window from its config: eager-create its `load_on_open` tabs (loaded at launch and
/// kept live), lazily defer the rest, lay them out around the active tab (`apply_active`), and
/// start per-tab reload timers. Returns the window id and its fresh `WindowRuntime` for the
/// caller to register. Shared by setup and hot-reload.
fn open_window(
    handle: &tauri::AppHandle,
    dark_mode: bool,
    global_session: Option<&str>,
    win_cfg: &config::WindowConfig,
) -> tauri::Result<(String, WindowRuntime)> {
    let wid = identity::window_id(&win_cfg.title);
    let (window, chrome_w) = webviews::build_window(
        handle,
        &wid,
        &win_cfg.title,
        win_cfg.width as f64,
        win_cfg.height as f64,
    )?;
    window.set_theme(theme_for(dark_mode))?;
    // Sidebar width is the default at launch (resize comes later); read it from the shared cell
    // so new tabs are placed consistently with `apply_active`.
    let cw = f64::from_bits(chrome_w.load(Ordering::Relaxed));

    let views = win_cfg.tab_views(global_session);
    let mut tabs = webviews::TabState::default();

    // Eager-create the `load_on_open` tabs: they load at launch and stay live (never hidden), so
    // they keep syncing and can notify in the background. Everything else is lazy.
    for v in views.iter().filter(|v| v.load_on_open) {
        webviews::create_content_webview(&window, v, cw)?;
        tabs.mark_created(&v.label);
    }

    // Active tab: whatever `open_on_launch` resolves to, else the first load_on_open tab (so a
    // window of background services opens showing one of them rather than the blank placeholder).
    let active = win_cfg.startup_label(global_session).or_else(|| {
        views
            .iter()
            .find(|v| v.load_on_open)
            .map(|v| v.label.clone())
    });
    if let Some(label) = &active {
        if let Some(v) = views.iter().find(|v| &v.label == label) {
            if !tabs.is_created(label) {
                webviews::create_content_webview(&window, v, cw)?;
                tabs.mark_created(label);
            }
            tabs.set_active(label);
        }
    }
    webviews::apply_active(&window, active.as_deref(), &views)?;

    // Periodic reload timers for tabs with `reload_every` (minutes). Only acts on
    // already-created webviews, so a never-opened lazy tab is harmlessly skipped. The cancel
    // flag lets us stop them when the window goes away.
    let reload_cancel = Arc::new(AtomicBool::new(false));
    spawn_reload_timers(&window, &views, reload_cancel.clone());

    Ok((
        wid,
        WindowRuntime {
            cfg: win_cfg.clone(),
            global_session: global_session.map(str::to_string),
            tabs,
            unread: HashMap::new(),
            badge_authoritative: HashSet::new(),
            reload_cancel,
            chrome_w,
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

/// Emit an event to just the focused window's chrome sidebar. Used by the keyboard tab-nav
/// menu items (⌘1–9 jump, ⌘⇧[ / ⌘⇧] cycle), which act on whichever window has key focus — the
/// window label *is* the window id, so its chrome is `{id}:chrome`.
fn emit_to_focused_chrome<S: serde::Serialize + Clone>(
    app: &tauri::AppHandle,
    event: &str,
    payload: S,
) {
    if let Some(win) = app.get_focused_window() {
        let _ = app.emit_to(identity::namespaced(win.label(), "chrome"), event, payload);
    }
}

/// Clear a closed window's awareness contribution and stop its reload timers, then recompute and
/// apply the aggregate dock badge. The `WindowRuntime` stays in the registry so its cfg survives
/// for reopen from the Window menu — only the live state (unread, badging-authoritative set,
/// timers) is wiped. Shared by the user-close path; the dock badge is applied to some *other*
/// open window since this one is on its way out.
fn cleanup_closed_window(app: &tauri::AppHandle, window_id: &str) {
    let state = app.state::<AppState>();
    {
        let mut windows = state.windows.lock().unwrap();
        if let Some(rt) = windows.get_mut(window_id) {
            rt.unread.clear();
            rt.badge_authoritative.clear();
            rt.reload_cancel.store(true, Ordering::Relaxed);
        }
    }
    let total = awareness::dock_count(&state.all_unread());
    if let Some(remaining) = app.windows().values().find(|w| w.label() != window_id) {
        let _ = remaining.set_badge_count(total);
    }
}

/// Handle a user-initiated close (native red button or ⌘W) of a real window. Returns `true` if
/// the close should be *prevented*: we never close the last open window, which would strand the
/// app with no visible UI and only the menu bar left. Otherwise it runs `cleanup_closed_window`
/// and returns `false` to let the close proceed. (The fallback error window isn't built via
/// `build_window`, so it never reaches here.)
pub(crate) fn on_real_window_close(app: &tauri::AppHandle, window_id: &str) -> bool {
    if app.windows().len() <= 1 {
        return true;
    }
    cleanup_closed_window(app, window_id);
    false
}

/// Apply a successful config reload: close windows that disappeared (dropping their runtime),
/// open windows that appeared, and reconcile tabs for windows that stayed. Emits
/// `config-reloaded` to each kept/added window's chrome.
fn reload_windows(app: &tauri::AppHandle, old_cfg: &config::Config, new_cfg: &config::Config) {
    let diff = watcher::diff_windows(old_cfg, new_cfg);
    let state = app.state::<AppState>();
    // Keep the live dark_mode current so a later Window-menu reopen themes to match.
    state.dark_mode.store(new_cfg.dark_mode, Ordering::Relaxed);

    // Closed windows: drop the window and its runtime, and stop its reload timers. Use
    // `destroy()` (not `close()`) so this programmatic removal bypasses the user-close guard in
    // `on_real_window_close` — that guard must only block the user closing their last window.
    for id in &diff.removed {
        if let Some(win) = app.get_window(id) {
            let _ = win.destroy();
        }
        if let Some(rt) = state.windows.lock().unwrap().remove(id) {
            rt.reload_cancel.store(true, Ordering::Relaxed);
        }
    }

    // New windows: build and register.
    for id in &diff.added {
        if let Some(win_cfg) = new_cfg
            .windows
            .iter()
            .find(|w| &identity::window_id(&w.title) == id)
        {
            if let Ok((wid, rt)) =
                open_window(app, new_cfg.dark_mode, new_cfg.session.as_deref(), win_cfg)
            {
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
            // Window is closed but its runtime is retained for reopen — refresh its stored cfg
            // so reopening it from the Window menu uses the latest config, not a stale snapshot.
            if let Some(rt) = state.windows.lock().unwrap().get_mut(id) {
                rt.cfg = win_cfg.clone();
                rt.global_session = new_cfg.session.clone();
            }
            continue;
        };
        let _ = window.set_theme(theme_for(new_cfg.dark_mode));
        reconcile_window_tabs(&state, &window, id, new_cfg.session.as_deref(), win_cfg);
    }

    // Keep the fallback error window in sync with whether any real window exists: close it once
    // windows return, or show it if this reload left none (a valid but window-less config) so the
    // app is never stranded invisible.
    let has_windows = !state.windows.lock().unwrap().is_empty();
    match (has_windows, app.get_window(webviews::WINDOW_ERROR)) {
        (true, Some(err_win)) => {
            let _ = err_win.close();
        }
        (false, None) => {
            let _ = webviews::build_error_window(app, "Your config defines no [[window]] blocks.");
        }
        _ => {}
    }

    // Rebuild the menu so the Window submenu's per-window reopen items match the new config
    // (added windows gain an item; removed ones lose theirs).
    let titles: Vec<(String, String)> = new_cfg
        .windows
        .iter()
        .map(|w| (identity::window_id(&w.title), w.title.clone()))
        .collect();
    if let Ok(menu) = build_app_menu(app, &titles) {
        let _ = app.set_menu(menu);
    }

    // Refresh the aggregate dock badge to reflect windows that came or went (a removed window's
    // unread is dropped with its runtime; without this its count would linger until the next
    // badge event). There's always at least one window here (the error window, if no real ones).
    let total = awareness::dock_count(&state.all_unread());
    if let Some(win) = app.windows().values().next() {
        let _ = win.set_badge_count(total);
    }

    // Surface the reload on every window that's still open.
    emit_to_all_chrome(app, "config-reloaded", ());
}

/// Reconcile one kept window's content webviews to its new config: eager-create newly-added
/// load_on_open tabs (others stay lazy), close orphaned webviews and drop their
/// unread/authoritative state, recompute the dock badge, then re-apply the active layout.
fn reconcile_window_tabs(
    state: &AppState,
    window: &tauri::Window,
    window_id: &str,
    global_session: Option<&str>,
    win_cfg: &config::WindowConfig,
) {
    let views = win_cfg.tab_views(global_session);
    let keep: HashSet<String> = views.iter().map(|v| v.label.clone()).collect();

    // Decide everything under the lock, but perform the webview ops AFTER releasing it.
    // reconcile runs on the watcher thread; Tauri marshals webview ops (add_child / close) to
    // the main thread, which may itself be waiting on this same lock (e.g. in on_title_changed)
    // — holding the lock across them would deadlock. So we compute the orphan/create lists,
    // active tab, dock total, and a fresh reload-timer generation under the lock, then act.
    let (orphans, to_create, active, dock_total, reload_cancel, cw) = {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(window_id) else {
            return;
        };
        rt.cfg = win_cfg.clone();
        rt.global_session = global_session.map(str::to_string);
        // Read the sidebar width under the lock to pass to create below (re-locking inside
        // create_content_webview would self-deadlock the non-reentrant mutex).
        let cw = f64::from_bits(rt.chrome_w.load(Ordering::Relaxed));

        // Orphans: created tabs no longer in the config (removed, or URL/label changed). Forget
        // all their state; the webviews are closed after the lock is dropped.
        let orphans = rt.tabs.orphans(&keep);
        for label in &orphans {
            rt.tabs.mark_unloaded(label);
            rt.unread.remove(label);
            rt.badge_authoritative.remove(label);
        }

        // Eager-create newly-added load_on_open tabs so they're live immediately; others stay
        // lazy. Mark created here; build after the lock is dropped.
        let mut to_create: Vec<config::TabView> = Vec::new();
        for v in &views {
            if v.load_on_open && !rt.tabs.is_created(&v.label) {
                rt.tabs.mark_created(&v.label);
                to_create.push(v.clone());
            }
        }

        // Resolve the active tab: keep the current one if it survived (mark_unloaded already
        // cleared it if it was orphaned), else fall back to open_on_launch or the first
        // load_on_open tab. Ensure it's created.
        let active = rt
            .tabs
            .active()
            .map(str::to_string)
            .or_else(|| win_cfg.startup_label(global_session))
            .or_else(|| {
                views
                    .iter()
                    .find(|v| v.load_on_open)
                    .map(|v| v.label.clone())
            });
        if let Some(a) = &active {
            if !rt.tabs.is_created(a) {
                rt.tabs.mark_created(a);
                if let Some(v) = views.iter().find(|v| &v.label == a) {
                    to_create.push(v.clone());
                }
            }
            rt.tabs.set_active(a);
        }

        // Stop the old reload timers and start a fresh generation for the new tab set (this is
        // also what gives newly-added tabs their reload timers).
        rt.reload_cancel.store(true, Ordering::Relaxed);
        let reload_cancel = Arc::new(AtomicBool::new(false));
        rt.reload_cancel = reload_cancel.clone();

        // Recompute the single dock badge now that this window's unread set may have shrunk.
        let dock_total = awareness::dock_count(
            &windows
                .values()
                .flat_map(|w| w.unread.values().copied())
                .collect::<Vec<_>>(),
        );
        (orphans, to_create, active, dock_total, reload_cancel, cw)
    };

    // Webview side-effects, lock released.
    for label in &orphans {
        if let Some(wv) = window.get_webview(label) {
            let _ = wv.close();
        }
    }
    for v in &to_create {
        let _ = webviews::create_content_webview(window, v, cw);
    }
    spawn_reload_timers(window, &views, reload_cancel);
    if let Some(win) = window.app_handle().get_window(window_id) {
        let _ = win.set_badge_count(dock_total);
    }
    // Re-apply the layout so the content area isn't left on a closed orphan.
    let _ = webviews::apply_active(window, active.as_deref(), &views);
}

/// Build the full app menu. Re-added because we replace Tauri's default menu; the Edit submenu
/// is load-bearing (clipboard accelerators for content webviews). The Window submenu carries
/// one reopen item per configured window, so it's rebuilt on hot-reload as windows change.
fn build_app_menu<R: tauri::Runtime, M: Manager<R>>(
    manager: &M,
    window_titles: &[(String, String)],
) -> tauri::Result<tauri::menu::Menu<R>> {
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
        .build(manager)?;
    let reset = MenuItemBuilder::with_id("reset_all", "Reset All Tabs").build(manager)?;
    let devtools = MenuItemBuilder::with_id("open_devtools", "Open Developer Tools")
        .accelerator("CmdOrCtrl+Alt+I")
        .build(manager)?;
    // Keyboard tab navigation: ⌘⇧]/⌘⇧[ cycle to the next/previous tab, ⌘1–9 jump to a position.
    // The handlers emit to the focused window's chrome, which resolves the target row and selects
    // it through the normal click path (so lazy tabs still create on demand).
    let tab_next = MenuItemBuilder::with_id("tab_next", "Next Tab")
        .accelerator("CmdOrCtrl+Shift+BracketRight")
        .build(manager)?;
    let tab_prev = MenuItemBuilder::with_id("tab_prev", "Previous Tab")
        .accelerator("CmdOrCtrl+Shift+BracketLeft")
        .build(manager)?;
    let mut tab_jumps = Vec::new();
    for n in 1..=9 {
        tab_jumps.push(
            MenuItemBuilder::with_id(format!("tab_jump:{n}"), format!("Tab {n}"))
                .accelerator(format!("CmdOrCtrl+{n}"))
                .build(manager)?,
        );
    }
    let tab_jump_refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = tab_jumps
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<R>)
        .collect();
    let edit_cfg = MenuItemBuilder::with_id("edit_config", "Edit Config").build(manager)?;
    let reveal_cfg =
        MenuItemBuilder::with_id("reveal_config", "Reveal Config in Finder").build(manager)?;
    let app_menu = SubmenuBuilder::new(manager, "curator")
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
    // Standard Edit menu — makes clipboard shortcuts work in content webviews. Don't drop it.
    let edit_menu = SubmenuBuilder::new(manager, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let tabs_menu = SubmenuBuilder::new(manager, "Tabs")
        .items(&[&tab_next, &tab_prev])
        .separator()
        .items(&tab_jump_refs)
        .separator()
        .items(&[&reload_tab, &reset, &devtools])
        .build()?;
    let config_menu = SubmenuBuilder::new(manager, "Config")
        .items(&[&edit_cfg, &reveal_cfg])
        .build()?;
    // Window menu — minimize / zoom / full screen; Close Window (⌘W) with a >1 guard so the
    // last window can never be closed (prevents stranding the app); and one item per configured
    // window so any closed window can be reopened.
    let close_window = MenuItemBuilder::with_id("close_window", "Close Window")
        .accelerator("CmdOrCtrl+W")
        .build(manager)?;
    let mut window_menu = SubmenuBuilder::new(manager, "Window")
        .minimize()
        .maximize()
        .fullscreen()
        .separator()
        .item(&close_window)
        .separator();
    for (wid, title) in window_titles {
        let item = MenuItemBuilder::with_id(format!("open_window:{wid}"), title).build(manager)?;
        window_menu = window_menu.item(&item);
    }
    let window_menu = window_menu.build()?;
    MenuBuilder::new(manager)
        .items(&[
            &app_menu,
            &edit_menu,
            &tabs_menu,
            &config_menu,
            &window_menu,
        ])
        .build()
}

/// `curator validate [path]`: load + validate a config and print its resolved window/tab tree
/// (with the cascaded session per tab) plus any non-fatal warnings. Exit 0 on success, 1 on a
/// load/parse/validation error. Mirrors `warden validate`.
pub fn validate_cli(path: Option<std::path::PathBuf>) -> i32 {
    let path = path.unwrap_or_else(config::resolve_config_path);
    match config::load_config(&path) {
        Ok((cfg, warnings)) => {
            println!("ok: {} ({} window(s))", path.display(), cfg.windows.len());
            for w in &cfg.windows {
                println!("  window {:?}", w.title);
                for v in w.tab_views(cfg.session.as_deref()) {
                    let group = v
                        .group
                        .as_deref()
                        .map(|g| format!(" group={g:?}"))
                        .unwrap_or_default();
                    println!(
                        "    tab {:?} url={} load_on_open={} session={:?}{}",
                        v.title, v.url, v.load_on_open, v.session, group
                    );
                }
            }
            for warn in &warnings {
                eprintln!("warning [{}]: {}", warn.window, warn.message);
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// `curator fmt [--check] [path]`: reformat the config file in curator's house style (shared with
/// warden via `config_core`). Without `--check`, rewrites in place (atomic, diff-guarded — a
/// no-op when already formatted) and prints what it did. With `--check`, writes nothing and exits
/// 1 if the file would be reformatted (for pre-commit/CI). Exit 0 ok / 1 on read or TOML error.
pub fn fmt_cli(check: bool, path: Option<std::path::PathBuf>) -> i32 {
    let path = path.unwrap_or_else(config::resolve_config_path);
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", path.display());
            return 1;
        }
    };
    // Only format well-formed TOML: taplo error-recovers on malformed input and would otherwise
    // silently return it unchanged, masking the breakage.
    if let Err(e) = toml::from_str::<toml::Value>(&src) {
        eprintln!("error: {} is not valid TOML: {e}", path.display());
        return 1;
    }
    if check {
        if config_core::format_str(&src) != src {
            eprintln!("would reformat: {}", path.display());
            return 1;
        }
        println!("ok: {} already formatted", path.display());
        return 0;
    }
    match config_core::format_file(&path) {
        Ok(true) => {
            println!("formatted: {}", path.display());
            0
        }
        Ok(false) => {
            println!("ok: {} already formatted", path.display());
            0
        }
        Err(e) => {
            eprintln!("error: cannot format {}: {e}", path.display());
            1
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            let path = config::resolve_config_path();
            let (mut cfg, load_err) = match config::load_config(&path) {
                Ok((c, warnings)) => {
                    for w in &warnings {
                        eprintln!("config warning [{}]: {}", w.window, w.message);
                    }
                    (c, None)
                }
                Err(e) => {
                    eprintln!("config error: {e}");
                    (config::Config::default(), Some(e.to_string()))
                }
            };
            #[cfg(target_os = "macos")]
            insecure::set_allowlist(cfg.allow_insecure.clone());

            let handle = app.handle().clone();
            let mut runtimes: HashMap<String, WindowRuntime> = HashMap::new();
            for win_cfg in &cfg.windows {
                let (wid, rt) =
                    open_window(&handle, cfg.dark_mode, cfg.session.as_deref(), win_cfg)?;
                runtimes.insert(wid, rt);
            }
            app.manage(AppState {
                windows: Mutex::new(runtimes),
                dark_mode: AtomicBool::new(cfg.dark_mode),
            });

            // No windows opened — either the config failed to parse or it defines no
            // `[[window]]` blocks. Show a visible error window instead of launching invisibly;
            // editing + saving the config hot-reloads the real windows (and closes this one).
            if cfg.windows.is_empty() {
                let msg = load_err
                    .unwrap_or_else(|| "Your config defines no [[window]] blocks.".to_string());
                webviews::build_error_window(&handle, &msg)?;
            }

            // Extract what we need from cfg before the watcher thread takes ownership of it.
            // (dark_mode lives in AppState now so reopen reads the live value, not this snapshot.)
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
                    match watcher::reconcile(&src) {
                        Ok((new_cfg, warnings)) => {
                            for w in &warnings {
                                eprintln!("config warning [{}]: {}", w.window, w.message);
                            }
                            // Format-on-save: rewrite in house style on a clean reload. The write
                            // is diff-guarded, so an already-formatted file is a no-op; if it does
                            // rewrite, the resulting watch event reconciles to the same config and
                            // formats to a no-op — no loop. Formatting only touches whitespace, so
                            // `new_cfg` (parsed pre-format) matches the formatted file's config.
                            if new_cfg.format_on_save {
                                if let Err(e) = config_core::format_file(&watch_path) {
                                    eprintln!("config format error: {e}");
                                }
                            }
                            reload_windows(&app_handle, &cfg, &new_cfg);
                            cfg = new_cfg;
                        }
                        Err(msg) => {
                            // Surface in each window's sidebar, and refresh the standalone error
                            // window if we're in the window-less error state.
                            webviews::refresh_error_window(&app_handle, &msg);
                            emit_to_all_chrome(&app_handle, "config-error", msg);
                        }
                    }
                }
            });

            // We replace Tauri's default menu, so we re-add the standard macOS menus (the Edit
            // submenu owns the clipboard accelerators ⌘C/⌘V/⌘X/⌘A/⌘Z that content webviews
            // need). Built here and rebuilt on hot-reload so the Window submenu tracks windows.
            let menu = build_app_menu(app, &window_titles)?;
            app.set_menu(menu)?;

            let cfg_path = path.clone();
            app.on_menu_event(move |app, event| match event.id().as_ref() {
                "reload_active" => {
                    commands::reload_active_tab(app);
                }
                "reset_all" => {
                    let _ = commands::reset_all_tabs(app);
                }
                "open_devtools" => {
                    commands::open_active_devtools(app);
                }
                "tab_next" => emit_to_focused_chrome(app, "nav-tab", 1i32),
                "tab_prev" => emit_to_focused_chrome(app, "nav-tab", -1i32),
                id if id.starts_with("tab_jump:") => {
                    if let Ok(n) = id["tab_jump:".len()..].parse::<usize>() {
                        emit_to_focused_chrome(app, "jump-tab", n);
                    }
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
                    // Close the focused window via `close()` so it flows through the same
                    // `CloseRequested` path as the native red button: that handler keeps the
                    // last window open and wipes the closed window's unread/timers (the runtime
                    // stays registered so its cfg survives for reopen via the menu items below).
                    if let Some(win) = app.get_focused_window() {
                        let _ = win.close();
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
                        let snapshot = {
                            let windows = state.windows.lock().unwrap();
                            windows
                                .get(wid)
                                .map(|rt| (rt.cfg.clone(), rt.global_session.clone()))
                        };
                        if let Some((cfg, global_session)) = snapshot {
                            // Read the live dark_mode (not the launch-time capture) so a reopened
                            // window matches the theme of every other window after a config flip.
                            let dark_mode = state.dark_mode.load(Ordering::Relaxed);
                            if let Ok((new_wid, rt)) =
                                open_window(app, dark_mode, global_session.as_deref(), &cfg)
                            {
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
            commands::window_identity,
            commands::select_tab,
            commands::reset_all,
            commands::reload_tab,
            commands::unload_tab,
            commands::home_tab,
            commands::nav_back,
            commands::nav_forward,
            commands::set_sidebar_width
        ])
        .run(tauri::generate_context!())
        .expect("error while running curator");
}
