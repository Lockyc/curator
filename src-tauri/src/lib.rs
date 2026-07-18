mod awareness;
mod commands;
mod escape;
#[cfg(target_os = "macos")]
mod insecure;
mod notification;
mod session;
mod watcher;
mod webviews;
#[cfg(target_os = "macos")]
mod zorder;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, Theme};

/// Window theme to apply for a given `dark_mode` setting. `None` = follow the system.
fn theme_for(dark_mode: bool) -> Option<Theme> {
    dark_mode.then_some(Theme::Dark)
}

/// Per-window runtime state: its config, lazy/active tab bookkeeping, and the awareness state
/// (per-tab unread + which tabs have gone Badging-authoritative) feeding the dock badge.
pub struct WindowRuntime {
    pub cfg: curator_config::WindowConfig,
    /// App-wide session base (`Config.session`) captured for this window, so commands and reopen
    /// can re-resolve the session chain without the whole `Config`. Kept in sync on hot-reload.
    pub global_session: Option<String>,
    pub tabs: webviews::TabState,
    pub unread: HashMap<String, awareness::Unread>,
    pub badge_authoritative: HashSet<String>,
    /// Set true to stop this window's `reload_every` timer threads — on window close, removal,
    /// or when its tab set changes (a fresh generation is spawned with a new flag).
    pub reload_cancel: Arc<AtomicBool>,
    /// The content-hole rect (logical px) the chrome last reported via `set_hole_rect`, seeded from
    /// [`webviews::initial_hole`]. Lazily-created / hot-reload-added tabs are placed here so they
    /// land in the current hole. Guarded by the `windows` mutex like every other field — the chrome
    /// owns the sidebar width and clamp, so Rust just tracks the reported geometry (no Rust clamp).
    pub hole: webviews::HoleRect,
}

/// Spawn one background thread per `reload_every` tab that reloads it on schedule until
/// `cancel` is set. Sleeps in 1s chunks so a cancelled timer exits promptly rather than after
/// a full interval, and never reloads after cancellation — so closed/removed windows don't
/// leak threads that keep poking dead webviews.
fn spawn_reload_timers(
    window: &tauri::Window,
    views: &[curator_config::TabView],
    cancel: Arc<AtomicBool>,
) {
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
    /// Current app-wide chrome `density`, kept live across hot-reload so `window_identity`
    /// (re-called by the chrome on `config-reloaded`) returns the new mode.
    pub density: Mutex<curator_config::Density>,
    /// Current app-wide `sidebar_drag`, kept live across hot-reload so `window_identity`
    /// (re-called by the chrome on `config-reloaded`) returns the new value. Drives the
    /// component's `windowDrag` flag (default true).
    pub sidebar_drag: AtomicBool,
    /// Current app-wide `auto_update`, kept live across hot-reload so `window_identity` returns the
    /// new value. Gates the chrome's launch-time update check (the manual menu check ignores it).
    pub auto_update: AtomicBool,
    /// The most recently applied config, kept in `AppState` (not a watcher-thread-local variable)
    /// so [`reload_windows`] can diff against it from *any* caller — the config-file watcher and
    /// the home surface's "Create a starter config" command both go through the same function now.
    /// A watcher-thread-local `cfg` would go stale the moment a reload happened by some other path
    /// (the watcher's next diff would then see its own already-applied windows as freshly "added").
    /// Updated at the end of every [`reload_windows`] call; seeded at setup with the launch config.
    last_cfg: Mutex<curator_config::Config>,
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
    win_cfg: &curator_config::WindowConfig,
) -> tauri::Result<(String, WindowRuntime)> {
    let wid = curator_config::identity::window_id(&win_cfg.title);
    let window = webviews::build_window(
        handle,
        &wid,
        &win_cfg.title,
        win_cfg.width as f64,
        win_cfg.height as f64,
    )?;
    window.set_theme(theme_for(dark_mode))?;
    // Best-guess hole until the chrome mounts and reports its `#content-hole` via `set_hole_rect`;
    // launch-time `load_on_open` tabs are placed here for the frame or two before that first report.
    let hole = webviews::initial_hole(win_cfg.width as f64, win_cfg.height as f64);

    let views = win_cfg.tab_views(global_session);
    let mut tabs = webviews::TabState::default();

    // Eager-create the `load_on_open` tabs: they load at launch and stay live (never hidden), so
    // they keep syncing and can notify in the background. Everything else is lazy.
    for v in views.iter().filter(|v| v.load_on_open) {
        webviews::create_content_webview(&window, v, hole)?;
        tabs.mark_created(&v.label);
    }

    // Active tab: whatever `startup_label` resolves to — by default the first load_on_open tab (so
    // a window of background services opens showing one of them), else the blank placeholder.
    let active = win_cfg.startup_label(global_session);
    if let Some(label) = &active {
        if let Some(v) = views.iter().find(|v| &v.label == label) {
            if !tabs.is_created(label) {
                webviews::create_content_webview(&window, v, hole)?;
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
            hole,
        },
    ))
}

/// Print config-load warnings to stderr — shared by the initial load and the hot-reload path so
/// the format stays in one place.
pub(crate) fn log_config_warnings(warnings: &[curator_config::Warning]) {
    for w in warnings {
        eprintln!("config warning [{}]: {}", w.window, w.message);
    }
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
        // The chrome is the window's main webview, so its label is the window id.
        let _ = app.emit_to(id.as_str(), event, payload.clone());
    }
}

/// Emit an event to just the focused window's chrome sidebar. Used by the keyboard tab-nav
/// menu items (⌘1–9 jump, ⌘⇧[ / ⌘⇧] cycle), which act on whichever window has key focus. The
/// chrome is the window's main webview, so its label *is* the window id (== the window label).
fn emit_to_focused_chrome<S: serde::Serialize + Clone>(
    app: &tauri::AppHandle,
    event: &str,
    payload: S,
) {
    if let Some(win) = app.get_focused_window() {
        let _ = app.emit_to(win.label(), event, payload);
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

/// Handle a user-initiated close (native red button or ⌘W) of a real window. Always lets the
/// close proceed (returns `false`); closing the **last** window quits curator
/// (last-window-quit — matching warden), rather than lingering as a menu-bar-only app with no
/// visible UI. Runs `cleanup_closed_window` first either way. (The fallback error window isn't
/// built via `build_window`, so it never reaches here.)
pub(crate) fn on_real_window_close(app: &tauri::AppHandle, window_id: &str) -> bool {
    cleanup_closed_window(app, window_id);
    if app.windows().len() <= 1 {
        app.exit(0);
    }
    false
}

/// Apply a new config to the running app: close windows that disappeared (dropping their runtime),
/// open windows that appeared, and reconcile tabs for windows that stayed. Emits
/// `config-reloaded` to each kept/added window's chrome.
///
/// Diffs against `state.last_cfg` (the previously applied config) rather than a caller-supplied
/// one, which is what makes this the single path both the config-file watcher AND the home
/// surface's "Create a starter config" command go through — the latter's "old" is always the
/// zero-window default (`shell_home_create_config` only runs when no config existed at all, so
/// `last_cfg` is still whatever `run()`'s setup seeded it with: `Config::default()`), so the diff
/// naturally treats every window in the fresh config as newly added. Updates `last_cfg` to
/// `new_cfg` before returning, so a later reload's diff is always against what's actually live.
pub(crate) fn reload_windows(app: &tauri::AppHandle, new_cfg: &curator_config::Config) {
    let state = app.state::<AppState>();
    let old_cfg = state.last_cfg.lock().unwrap().clone();
    let diff = watcher::diff_windows(&old_cfg, new_cfg);
    // Keep the live dark_mode current so a later Window-menu reopen themes to match.
    state.dark_mode.store(new_cfg.dark_mode, Ordering::Relaxed);
    // Keep the live density current so each kept window's chrome, re-calling window_identity on
    // `config-reloaded` below, picks up a density change without a relaunch.
    *state.density.lock().unwrap() = new_cfg.density;
    // Same for sidebar_drag — the chrome re-reads it via window_identity and re-applies windowDrag.
    state
        .sidebar_drag
        .store(new_cfg.sidebar_drag, Ordering::Relaxed);
    // Same for auto_update — a reload can flip whether launch checks run (menu check is unaffected).
    state
        .auto_update
        .store(new_cfg.auto_update, Ordering::Relaxed);

    // Closed windows: drop the window and its runtime, and stop its reload timers. Use
    // `destroy()` (not `close()`) so this programmatic removal bypasses `on_real_window_close` —
    // a reload that drops the last window must reconcile to the new config (error window or
    // replacement), not trip last-window-quit and exit the app mid-reconcile.
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
            .find(|w| &curator_config::identity::window_id(&w.title) == id)
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
            .find(|w| &curator_config::identity::window_id(&w.title) == id)
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

    // The home surface keeps the app from ever being stranded invisible: it closes once a real
    // window exists, or appears if this reload left the config defining no windows at all (a valid
    // but window-less config). Shared with warden and lector — curator's own error window could
    // only ever state an error, never offer to create a config or list windows.
    let entries = window_entries(app, &state);
    let cfg_path = curator_config::resolve_config_path();
    reconcile_home(app, &entries, &cfg_path, true, None);

    // Rebuild the menu so the Window submenu's per-window reopen items match the new config
    // (added windows gain an item; removed ones lose theirs).
    if let Ok(menu) = build_app_menu(app, &cfg_path, &entries) {
        let _ = app.set_menu(menu);
    }

    // Refresh the aggregate dock badge to reflect windows that came or went (a removed window's
    // unread is dropped with its runtime; without this its count would linger until the next
    // badge event).
    let total = awareness::dock_count(&state.all_unread());
    if let Some(win) = app.windows().values().next() {
        let _ = win.set_badge_count(total);
    }

    // Surface the reload on every window that's still open.
    emit_to_all_chrome(app, "config-reloaded", ());

    *state.last_cfg.lock().unwrap() = new_cfg.clone();
}

/// Project the window registry into the menu spine's / home surface's shared `WindowEntry` shape.
/// `open` is read live off the running app (not cached) — a window can close (its `WindowRuntime`
/// stays registered, so it can be reopened from the Window menu / home surface) while the app keeps
/// running: closing the *last* one is the one case that can't leave a closed-but-registered entry
/// behind, since that trips last-window-quit instead. Sorted by title for a stable, deterministic
/// menu/home order — the registry is a `HashMap`, so iteration order on its own isn't one.
fn window_entries(app: &tauri::AppHandle, state: &AppState) -> Vec<shell_core::menu::WindowEntry> {
    let mut entries: Vec<_> = state
        .windows
        .lock()
        .unwrap()
        .iter()
        .map(|(id, rt)| shell_core::menu::WindowEntry {
            id: id.clone(),
            title: rt.cfg.title.clone(),
            open: app.get_window(id).is_some(),
            colour: rt.cfg.colour.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.title.cmp(&b.title));
    entries
}

/// Show or close the shared home surface to match `entries`. `has_windows` is `!entries.is_empty()`
/// — the registry only ever holds an entry for a window the current config still defines (a window
/// dropped from the config is removed from it entirely, see `reload_windows`'s `diff.removed` loop),
/// so a non-empty registry means the config defines at least one window, which is the question that
/// actually matters here (whether that window happens to be open right now is not: curator's
/// last-window-quit means "config defines windows, but none are open" isn't a state the running app
/// can be caught in — the app would have already exited). Shared by setup, every hot-reload
/// (successful or failed), and a menu/home-driven window reopen.
fn reconcile_home(
    app: &tauri::AppHandle,
    entries: &[shell_core::menu::WindowEntry],
    config_path: &std::path::Path,
    config_exists: bool,
    load_error: Option<&str>,
) {
    let has_windows = !entries.is_empty();
    match shell_core::home::home_state(
        has_windows,
        config_exists,
        &config_path.display().to_string(),
        load_error,
        entries,
    ) {
        Some(s) => {
            let _ = shell_core::home::show_home(app, &s, "curator");
        }
        None => shell_core::home::close_home(app),
    }
}

/// The menu spine's Window submenu selector, and the home surface's per-window button
/// (`shell_home_open_window`): focus `window_id` if it's already open, otherwise rebuild it from
/// its retained `WindowRuntime.cfg`. Reconciles the home surface afterwards so it closes now that a
/// real window exists again (a no-op in the overwhelmingly common case where it wasn't showing).
pub(crate) fn open_or_focus_window(app: &tauri::AppHandle, window_id: &str) {
    if let Some(win) = app.get_window(window_id) {
        let _ = win.set_focus();
    } else {
        let state = app.state::<AppState>();
        let snapshot = {
            let windows = state.windows.lock().unwrap();
            windows
                .get(window_id)
                .map(|rt| (rt.cfg.clone(), rt.global_session.clone()))
        };
        if let Some((cfg, global_session)) = snapshot {
            let dark_mode = state.dark_mode.load(Ordering::Relaxed);
            if let Ok((new_wid, rt)) = open_window(app, dark_mode, global_session.as_deref(), &cfg)
            {
                state.windows.lock().unwrap().insert(new_wid, rt);
            }
        }
    }
    let state = app.state::<AppState>();
    let entries = window_entries(app, &state);
    let path = curator_config::resolve_config_path();
    reconcile_home(app, &entries, &path, path.exists(), None);
}

/// Reconcile one kept window's content webviews to its new config: eager-create newly-added
/// load_on_open tabs (others stay lazy), close orphaned webviews and drop their
/// unread/authoritative state, recompute the dock badge, then re-apply the active layout.
fn reconcile_window_tabs(
    state: &AppState,
    window: &tauri::Window,
    window_id: &str,
    global_session: Option<&str>,
    win_cfg: &curator_config::WindowConfig,
) {
    let views = win_cfg.tab_views(global_session);
    let keep: HashSet<String> = views.iter().map(|v| v.label.clone()).collect();

    // Decide everything under the lock, but perform the webview ops AFTER releasing it.
    // reconcile runs on the watcher thread; Tauri marshals webview ops (add_child / close) to
    // the main thread, which may itself be waiting on this same lock (e.g. in on_title_changed)
    // — holding the lock across them would deadlock. So we compute the orphan/create lists,
    // active tab, dock total, and a fresh reload-timer generation under the lock, then act.
    let (orphans, to_create, active, dock_total, reload_cancel, hole) = {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(window_id) else {
            return;
        };
        rt.cfg = win_cfg.clone();
        rt.global_session = global_session.map(str::to_string);
        // Read the current hole rect under the lock to pass to create below (re-locking inside
        // create_content_webview would self-deadlock the non-reentrant mutex).
        let hole = rt.hole;

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
        let mut to_create: Vec<curator_config::TabView> = Vec::new();
        for v in &views {
            if v.load_on_open && !rt.tabs.is_created(&v.label) {
                rt.tabs.mark_created(&v.label);
                to_create.push(v.clone());
            }
        }

        // Resolve the active tab: keep the current one if it survived (mark_unloaded already
        // cleared it if it was orphaned), else fall back to startup_label (by default the first
        // load_on_open tab). Ensure it's created.
        let active = rt
            .tabs
            .active()
            .map(str::to_string)
            .or_else(|| win_cfg.startup_label(global_session));
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
        (orphans, to_create, active, dock_total, reload_cancel, hole)
    };

    // Webview side-effects, lock released.
    for label in &orphans {
        if let Some(wv) = window.get_webview(label) {
            let _ = wv.close();
        }
    }
    for v in &to_create {
        let _ = webviews::create_content_webview(window, v, hole);
    }
    spawn_reload_timers(window, &views, reload_cancel);
    if let Some(win) = window.app_handle().get_window(window_id) {
        let _ = win.set_badge_count(dock_total);
    }
    // Re-apply the layout so the content area isn't left on a closed orphan.
    let _ = webviews::apply_active(window, active.as_deref(), &views);
}

/// Build the full app menu: the shared spine (App/Config/Window submenus + the Close Tab item)
/// interleaved with curator's own Edit and Tabs submenus. Edit is the standard macOS Edit menu —
/// its clipboard accelerators are what make ⌘C/⌘V/⌘X/⌘A work in content webviews; it is not
/// app-agnostic (nothing to hand to the spine), so it stays curator's own and must not be dropped.
/// Tabs carries curator's keyboard tab-nav + reload/reset/devtools, plus the spine's Close Tab
/// (⌘W) — curator's own `close_window` menu-id/accelerator are gone: ⌘W used to close the whole
/// window, which was the bug; the spine's Window submenu now owns Close Window at ⌘⇧W instead.
/// Rebuilt on every hot-reload so the Window submenu's per-window reopen items track the config.
fn build_app_menu<R: tauri::Runtime, M: Manager<R>>(
    manager: &M,
    config_path: &std::path::Path,
    window_entries: &[shell_core::menu::WindowEntry],
) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};

    let spine = shell_core::menu::build_spine(
        manager,
        shell_core::menu::SpineConfig {
            app_name: "curator",
            config_path,
            windows: window_entries,
        },
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_GIT_SHA"),
        env!("BUILD_DATE"),
    )?;

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

    let tabs_menu = SubmenuBuilder::new(manager, "Tabs")
        .item(&spine.close_tab)
        .separator()
        .items(&[&tab_next, &tab_prev])
        .separator()
        .items(&tab_jump_refs)
        .separator()
        .items(&[&reload_tab, &reset, &devtools])
        .build()?;

    MenuBuilder::new(manager)
        .items(&[
            &spine.submenus[0], // App
            &edit_menu,
            &tabs_menu,
            &spine.submenus[1], // Config
            &spine.submenus[2], // Window
        ])
        .build()
}

/// `curator validate [path]`: load + validate a config and print its resolved window/tab tree
/// (with the cascaded session per tab) plus any non-fatal warnings. Exit 0 on success, 1 on a
/// load/parse/validation error. Mirrors `warden validate`.
pub fn validate_cli(path: Option<std::path::PathBuf>) -> i32 {
    let path = path.unwrap_or_else(curator_config::resolve_config_path);
    match curator_config::load_config(&path) {
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

/// Filename for the window-state plugin's saved bounds, scoped per config file. The plugin keys
/// window state by Tauri label *within one file*; two different configs can reuse a window title,
/// so scope the filename by a stable hash of the (canonicalized) config path to keep their bounds
/// separate. Uses `fnv1a_64` — deliberately NOT `std`'s `DefaultHasher`, whose output isn't
/// guaranteed stable across Rust releases: a `rust-toolchain.toml` bump could silently change the
/// filename and reset every window to default bounds. Moving/renaming the config orphans its saved
/// bounds (acceptable; the path is otherwise stable). Mirrors warden.
fn window_state_filename() -> String {
    let path = curator_config::resolve_config_path();
    let canonical = std::fs::canonicalize(&path).unwrap_or(path);
    let hash = fnv1a_64(canonical.as_os_str().as_encoded_bytes());
    format!(".window-state-{hash:016x}.json")
}

/// FNV-1a 64-bit hash. Small, deterministic, and — crucially — **stable across Rust toolchains**
/// (unlike `std::hash::DefaultHasher`), so the value drives a persistent on-disk filename without
/// risk of a toolchain bump changing it. Non-cryptographic; collision resistance is irrelevant
/// here (the input is a single trusted config path).
///
/// Duplicated in warden rather than lifted into shell-core: `window_state_filename` stays per-app
/// (each app hashes its own config path) and shell-core's dividing-line decision explicitly rules
/// the hash out of that crate.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

pub fn run() {
    // Register the shell-core plugins (window-state + updater + process) — the set every sibling app
    // installs identically. Window-state persists each window's size/position/maximized keyed by
    // Tauri label within a per-config state file (window_state_filename) so two configs sharing a
    // window title don't share bounds; both save and restore (the plugin's window_created hook, on
    // the main loop) are automatic — build_window must NOT restore by hand (deadlocks; see the
    // footgun there). The shared home surface (shell-home, replacing curator's own error window) is
    // excluded from state restore.
    shell_core::register_plugins(
        tauri::Builder::default(),
        window_state_filename(),
        &[shell_core::home::HOME_LABEL],
    )
    .setup(move |app| {
        // Prime native banner notifications (authorization + presentation/click delegate) and
        // capture the app handle the click delegate uses to surface a tab. The banner path is a
        // no-op in dev / off the packaged app; the badge/sentinel path is independent of this.
        notification::init(app.handle().clone());

        let path = curator_config::resolve_config_path();
        let (cfg, load_err) = match curator_config::load_config(&path) {
            Ok((c, warnings)) => {
                log_config_warnings(&warnings);
                (c, None)
            }
            Err(e) => {
                eprintln!("config error: {e}");
                (curator_config::Config::default(), Some(e.to_string()))
            }
        };
        #[cfg(target_os = "macos")]
        insecure::set_allowlist(cfg.allow_insecure.clone());

        let handle = app.handle().clone();
        let mut runtimes: HashMap<String, WindowRuntime> = HashMap::new();
        for win_cfg in &cfg.windows {
            let (wid, rt) = open_window(&handle, cfg.dark_mode, cfg.session.as_deref(), win_cfg)?;
            runtimes.insert(wid, rt);
        }
        app.manage(AppState {
            windows: Mutex::new(runtimes),
            dark_mode: AtomicBool::new(cfg.dark_mode),
            density: Mutex::new(cfg.density),
            sidebar_drag: AtomicBool::new(cfg.sidebar_drag),
            auto_update: AtomicBool::new(cfg.auto_update),
            last_cfg: Mutex::new(cfg.clone()),
        });

        // The home surface keeps the app from ever being stranded invisible: it appears when the
        // config failed to load or defines no `[[window]]` blocks, and closes once a real window
        // exists. Shared with warden and lector — curator's own error window could only ever state
        // an error, never offer to create a config or list windows.
        let entries = {
            let state = app.state::<AppState>();
            window_entries(&handle, &state)
        };
        reconcile_home(&handle, &entries, &path, path.exists(), load_err.as_deref());

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
            // The exact bytes of our own most recent format-on-save write, so we can swallow
            // the watch event it triggers and reload exactly once per user save (see below).
            let mut self_write: Option<String> = None;
            for res in rx {
                let Ok(event) = res else { continue };
                if !event.paths.iter().any(|p| p == &watch_path) {
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(&watch_path) else {
                    continue;
                };
                // If this event is the echo of our own format write, consume it and skip — the
                // user save that prompted the rewrite already reloaded. `take()` clears the
                // marker either way, so at worst a missed echo costs one redundant no-op reload.
                if self_write.take().as_deref() == Some(src.as_str()) {
                    continue;
                }
                match watcher::reconcile(&src) {
                    Ok((new_cfg, warnings)) => {
                        log_config_warnings(&warnings);
                        // Format-on-save: rewrite in house style on a clean reload. The write
                        // is diff-guarded, so an already-formatted file is a no-op. When it does
                        // rewrite, remember the formatted bytes as `self_write` so the watch
                        // event the write triggers is swallowed above — one reload per user
                        // save, not two. Formatting only touches whitespace, so `new_cfg`
                        // (parsed pre-format) already matches the formatted file's config.
                        if new_cfg.format_on_save {
                            let formatted = curator_config::format_str(&src);
                            if formatted != src {
                                match curator_config::format_file(&watch_path) {
                                    Ok(_) => self_write = Some(formatted),
                                    Err(e) => eprintln!("config format error: {e}"),
                                }
                            }
                        }
                        reload_windows(&app_handle, &new_cfg);
                    }
                    Err(msg) => {
                        // Surface in each window's sidebar, and reconcile the shared home surface
                        // (Broken state) in case every window happens to be closed.
                        emit_to_all_chrome(&app_handle, "config-error", msg.clone());
                        let state = app_handle.state::<AppState>();
                        let entries = window_entries(&app_handle, &state);
                        reconcile_home(&app_handle, &entries, &watch_path, true, Some(&msg));
                    }
                }
            }
        });

        // We replace Tauri's default menu, so we re-add the standard macOS menus (the Edit
        // submenu owns the clipboard accelerators ⌘C/⌘V/⌘X/⌘A/⌘Z that content webviews
        // need). Built here and rebuilt on hot-reload so the Window submenu tracks windows.
        let menu = build_app_menu(app, &path, &entries)?;
        app.set_menu(menu)?;

        let cfg_path = path.clone();
        app.on_menu_event(move |app, event| {
            let id = event.id().as_ref();
            // The spine's file-acting ids (Edit Config, Reveal Config) need no window — let it
            // consume them first.
            if shell_core::menu::handle_spine_event(id, &cfg_path) {
                return;
            }
            match id {
                "reload_active" => {
                    commands::reload_active_tab(app);
                }
                "reset_all" => {
                    let _ = commands::reset_all_tabs(app);
                }
                "open_devtools" => {
                    commands::open_active_devtools(app);
                }
                // chrome-core owns self-update; forward to its checkForUpdateNow().
                shell_core::menu::ids::CHECK_UPDATES => {
                    emit_to_focused_chrome(app, "check-update", ())
                }
                // ⌘W unloads the ACTIVE TAB — it does not close the window. The chrome owns which
                // tab is active and the dot repaint, so it drives unload_tab off this event
                // (warden's model, now the family standard — curator's ⌘W used to close the whole
                // window, which was the bug this fixes).
                shell_core::menu::ids::CLOSE_TAB => emit_to_focused_chrome(app, "close-tab", ()),
                shell_core::menu::ids::CLOSE_WINDOW => {
                    // Close the focused window via `close()` so it flows through the same
                    // `CloseRequested` path as the native red button: that handler wipes the
                    // closed window's unread/timers (the runtime stays registered so its cfg
                    // survives for reopen via the Window menu) and quits curator if this was the
                    // last window.
                    if let Some(win) = app.get_focused_window() {
                        let _ = win.close();
                    }
                }
                "tab_next" => emit_to_focused_chrome(app, "nav-tab", 1i32),
                "tab_prev" => emit_to_focused_chrome(app, "nav-tab", -1i32),
                id if id.starts_with("tab_jump:") => {
                    if let Ok(n) = id["tab_jump:".len()..].parse::<usize>() {
                        emit_to_focused_chrome(app, "jump-tab", n);
                    }
                }
                id => {
                    if let Some(wid) = shell_core::menu::selected_window(id) {
                        open_or_focus_window(app, wid);
                    }
                }
            }
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
        commands::set_hole_rect,
        commands::shell_home_create_config,
        commands::shell_home_edit_config,
        commands::shell_home_open_window,
    ])
    .run(tauri::generate_context!())
    .expect("error while running curator");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_64_matches_known_vectors() {
        // Canonical FNV-1a/64 test vectors — pins the algorithm so the window-state filename can
        // never drift with the toolchain (the whole point of not using DefaultHasher).
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn window_state_filename_shape_is_stable() {
        // Same config path → same filename, every run (no per-run seed).
        assert_eq!(window_state_filename(), window_state_filename());
        assert!(window_state_filename().starts_with(".window-state-"));
        assert!(window_state_filename().ends_with(".json"));
    }
}
