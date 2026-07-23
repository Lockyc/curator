mod awareness;
mod chrome_hit;
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

/// A tab popped out into its own detached window (`commands::pop_out_tab`). Kept in
/// [`AppState::detached`], **separate from `windows`**, so hot-reload reconcile never sees it (the
/// detached-label prefix, [`shell_core::detach::is_detached_label`], keeps window-state persistence
/// off it too). Holds the origin bookkeeping needed to return the tab: which window it came from,
/// which tab, and the resolved [`curator_config::TabView`] to recreate its webview from — a curator
/// tab is a webview that is *recreated* on redock (login survives via the session-keyed data store),
/// so, unlike warden, there is no live native surface to hold here.
pub struct CuratorDetached {
    pub origin_wid: String,
    pub tab_label: String,
    pub view: curator_config::TabView,
}

/// Set once on `RunEvent::ExitRequested` (see [`run`]), which fires before every window's
/// `Destroyed` during ⌘Q. Checked at the top of [`redock`] so a detached window's teardown doesn't
/// reopen its (already-closing) origin window mid-quit. Never reset — curator doesn't prevent exit.
static IS_QUITTING: AtomicBool = AtomicBool::new(false);

/// Mark the app as quitting. Called once, from `RunEvent::ExitRequested`.
pub(crate) fn mark_quitting() {
    IS_QUITTING.store(true, Ordering::SeqCst);
}

/// Whether the app is quitting — see [`IS_QUITTING`].
pub(crate) fn is_quitting() -> bool {
    IS_QUITTING.load(Ordering::SeqCst)
}

/// The opaque token identifying a popped-out tab's detached window
/// (→ [`shell_core::detach::detached_label`]). A curator content-webview label is already
/// `{origin_wid}:tab-<hash>` — globally unique across windows and Tauri-label-safe — so it *is* the
/// token; the origin window is tracked on [`CuratorDetached`], never parsed back out of the label.
pub(crate) fn detach_window_token(tab_label: &str) -> String {
    tab_label.to_string()
}

/// The detached window's banner height (matches shell-core's `detach.html` `#banner`, 2.25rem ≈
/// 36px). Only used to size the recreated webview's BIRTH rect so it doesn't flash full-height for
/// one frame before `detach.html`'s own `set_hole_rect` lands and reports the exact hole.
pub(crate) const DETACH_BANNER_H: f64 = 36.0;

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
    /// Tabs currently popped out into their own detached window, keyed by the detached window's
    /// Tauri label ([`shell_core::detach::detached_label`]). **Separate from `windows`** so
    /// reconcile/window-state never touch these ephemeral windows, and so the home-surface check can
    /// still count them (`reconcile_home`) — a detached window is a real surface on screen.
    pub detached: Mutex<HashMap<String, CuratorDetached>>,
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
    detached_tabs: &HashSet<String>,
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
    // they keep syncing and can notify in the background. Everything else is lazy. A tab that is
    // currently popped out into its own detached window (only possible on a reopen while a tab of
    // this window is detached — `open_or_focus_window` passes those in) is NOT recreated here: its
    // webview lives on the detached window (recreating it would collide on the app-global label),
    // so it stays a popped-out placeholder until it redocks.
    for v in views.iter().filter(|v| v.load_on_open) {
        if detached_tabs.contains(&v.label) {
            tabs.mark_detached(&v.label);
            continue;
        }
        webviews::create_content_webview(&window, v, hole, win_cfg.colour.as_deref())?;
        tabs.mark_created(&v.label);
    }

    // Active tab: whatever `startup_label` resolves to — by default the first load_on_open tab (so
    // a window of background services opens showing one of them), else the blank placeholder. A
    // detached tab can't be the active one (its content isn't on this window), so it's left as a
    // placeholder and the window opens on the empty state until the user picks another tab.
    let active = win_cfg.startup_label(global_session);
    if let Some(label) = &active {
        if detached_tabs.contains(label) {
            tabs.mark_detached(label);
        } else if let Some(v) = views.iter().find(|v| &v.label == label) {
            if !tabs.is_created(label) {
                webviews::create_content_webview(&window, v, hole, win_cfg.colour.as_deref())?;
                tabs.mark_created(label);
            }
            tabs.set_active(label);
        }
    }
    webviews::apply_active(&window, tabs.active(), &views)?;

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

/// Build a `WindowRuntime` for a window configured `open_on_start = false`: registered in the
/// registry (so the Window menu / home surface list it and can reopen it) but with **no live window
/// built**. Its tab/awareness state starts empty and its reload flag is pre-cancelled — there are no
/// timers, no webviews, nothing on screen. This is exactly the shape a runtime has after a window is
/// *closed* (see [`cleanup_closed_window`]), which is the right starting state for a never-opened
/// one: [`open_or_focus_window`] reads only `cfg`/`global_session` and builds a fresh runtime via
/// [`open_window`] when the user opens it, replacing this dormant entry.
fn dormant_runtime(
    win_cfg: &curator_config::WindowConfig,
    global_session: Option<&str>,
) -> WindowRuntime {
    WindowRuntime {
        cfg: win_cfg.clone(),
        global_session: global_session.map(str::to_string),
        tabs: webviews::TabState::default(),
        unread: HashMap::new(),
        badge_authoritative: HashSet::new(),
        reload_cancel: Arc::new(AtomicBool::new(true)),
        hole: webviews::initial_hole(win_cfg.width as f64, win_cfg.height as f64),
    }
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
            // A freshly config-added window has no popped-out tabs (its origin id is new), so pass
            // an empty detached set.
            if let Ok((wid, rt)) = open_window(
                app,
                new_cfg.dark_mode,
                new_cfg.session.as_deref(),
                win_cfg,
                &HashSet::new(),
            ) {
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

/// Show or close the shared home surface to match `entries`. `has_windows` is whether any window is
/// actually **open** (or a detached surface is up) — not whether the config merely *defines*
/// windows. That distinction is load-bearing: an `open_on_start = false` window is registered but
/// dormant, so at launch the app can have configured windows yet nothing on screen, and the home
/// surface must then appear (listing every configured window, dormant ones included, for the user to
/// open) rather than leave the app stranded invisible. (This used to test the registry
/// `!entries.is_empty()`, which was safe only while every configured window opened at launch and
/// last-window-quit guaranteed "configured but none open" couldn't occur — `open_on_start` breaks
/// that guarantee.) The `entries` list still carries *all* configured windows with their live `open`
/// flag, so the home surface can list the dormant ones as reopenable. Shared by setup, every
/// hot-reload (successful or failed), and a menu/home-driven window reopen.
fn reconcile_home(
    app: &tauri::AppHandle,
    entries: &[shell_core::menu::WindowEntry],
    config_path: &std::path::Path,
    config_exists: bool,
    load_error: Option<&str>,
) {
    // A detached (popped-out) window is a real surface on screen: while one exists the app is not
    // "windowless", so the home surface must not appear over it. In practice a detached window can
    // only exist when the config defines at least one window (you pop a tab out *of* a window), so
    // `entries` is already non-empty then — this fold is a belt-and-braces guard that keeps the
    // invariant explicit rather than resting on that coincidence.
    let has_detached = !app.state::<AppState>().detached.lock().unwrap().is_empty();
    // Gate on a window actually being OPEN, not merely configured — `open` is read live per entry
    // (see `window_entries`), so a registry full of dormant `open_on_start = false` windows counts
    // as "no windows" and the home surface shows to offer them.
    let has_windows = entries.iter().any(|e| e.open) || has_detached;
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
            // Tabs of THIS window that are currently popped out — open_window must not recreate
            // their webviews on the reopened origin (they live on the detached window; the label is
            // app-global and would collide). They stay popped-out placeholders until they redock.
            let detached_tabs: HashSet<String> = state
                .detached
                .lock()
                .unwrap()
                .values()
                .filter(|d| d.origin_wid == window_id)
                .map(|d| d.tab_label.clone())
                .collect();
            if let Ok((new_wid, rt)) = open_window(
                app,
                dark_mode,
                global_session.as_deref(),
                &cfg,
                &detached_tabs,
            ) {
                state.windows.lock().unwrap().insert(new_wid, rt);
            }
        }
    }
    let state = app.state::<AppState>();
    let entries = window_entries(app, &state);
    let path = curator_config::resolve_config_path();
    reconcile_home(app, &entries, &path, path.exists(), None);
}

/// Return a popped-out tab to its origin window when its detached window closes — the `on_close`
/// wired via [`shell_core::detach::wire_return`]. Runs on the main thread (Tauri delivers the
/// window `Destroyed` event there). A curator tab is a webview that is *recreated* on the origin
/// (login survives via the session-keyed data store), so — unlike warden — there is no live surface
/// to move back; the detached window's webview simply died with it, and this builds a fresh one.
///
/// Order matters: the origin+tab are read (not removed) first, so that if the origin window was
/// closed while the tab was out, [`open_or_focus_window`] reopens it while the tab is STILL in
/// `detached` — `open_window` then skips recreating it (no app-global label collision) and marks it
/// a placeholder. Only then is the bookkeeping removed and the webview recreated on the origin.
pub(crate) fn redock(app: &tauri::AppHandle, detached_label: &str) {
    // ⌘Q teardown: `RunEvent::ExitRequested` fires before every window's `Destroyed`. Don't reopen
    // an origin or recreate a webview mid-quit — everything is being torn down.
    if is_quitting() {
        return;
    }
    let state = app.state::<AppState>();

    // Peek the origin + tab without removing (see the doc comment's ordering rationale).
    let Some((origin_wid, tab_label)) = state
        .detached
        .lock()
        .unwrap()
        .get(detached_label)
        .map(|d| (d.origin_wid.clone(), d.tab_label.clone()))
    else {
        return; // already redocked (double-close) — nothing to do
    };

    // Reopen the origin if the user closed it while the tab was popped out (case: another window or
    // the detached window itself kept the app alive past last-window-quit).
    if app.get_window(&origin_wid).is_none() {
        open_or_focus_window(app, &origin_wid);
    }

    // Now take the bookkeeping and recreate the tab on the origin.
    let Some(det) = state.detached.lock().unwrap().remove(detached_label) else {
        return; // raced with a double-close
    };
    let Some(window) = app.get_window(&origin_wid) else {
        return; // origin gone from config entirely — the tab has no home; nothing to free
    };

    // Under the lock: clear the placeholder mark and, unless the tab was removed from the config
    // while it was popped out, mark it created+active and read the plan to recreate it.
    let plan = {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(&origin_wid) else {
            return;
        };
        let views = rt.cfg.tab_views(rt.global_session.as_deref());
        let still_in_config = views.iter().any(|v| v.label == tab_label);
        rt.tabs.clear_detached(&tab_label);
        if !still_in_config {
            None // tab removed from config while detached — it simply ends
        } else {
            rt.tabs.mark_created(&tab_label);
            rt.tabs.set_active(&tab_label);
            Some((
                rt.hole,
                views,
                rt.tabs.active().map(str::to_string),
                rt.cfg.colour.clone(),
            ))
        }
    };
    if let Some((hole, views, active, colour)) = plan {
        let _ = webviews::create_content_webview(&window, &det.view, hole, colour.as_deref());
        let _ = webviews::apply_active(&window, active.as_deref(), &views);
    }

    // Re-render the origin chrome so the returned row loses its ⤢ detached mark and reflects the new
    // active tab. `config-reloaded` drives the chrome's refresh() (a get_tabs re-fetch); emit_to
    // targets only that window's chrome (curator's per-window emit scoping). If the origin was just
    // reopened its fresh mount already refreshes, so a missed emit self-corrects.
    let _ = app.emit_to(origin_wid.as_str(), "config-reloaded", ());
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
    let (orphans, to_create, active, dock_total, reload_cancel, hole, colour) = {
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
            // A popped-out tab is skipped: its webview lives on the detached window, so recreating
            // it here would collide on the app-global label. It redocks via `redock`, not reconcile.
            if v.load_on_open && !rt.tabs.is_created(&v.label) && !rt.tabs.is_detached(&v.label) {
                rt.tabs.mark_created(&v.label);
                to_create.push(v.clone());
            }
        }

        // Resolve the active tab: keep the current one if it survived (mark_unloaded already
        // cleared it if it was orphaned), else fall back to startup_label (by default the first
        // load_on_open tab). Ensure it's created. A detached tab can't be active (its content is on
        // the detached window), so it's never created or promoted here.
        let active = rt
            .tabs
            .active()
            .map(str::to_string)
            .or_else(|| win_cfg.startup_label(global_session))
            .filter(|a| !rt.tabs.is_detached(a));
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
        (
            orphans,
            to_create,
            active,
            dock_total,
            reload_cancel,
            hole,
            win_cfg.colour.clone(),
        )
    };

    // Webview side-effects, lock released.
    for label in &orphans {
        if let Some(wv) = window.get_webview(label) {
            let _ = wv.close();
        }
    }
    for v in &to_create {
        let _ = webviews::create_content_webview(window, v, hole, colour.as_deref());
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
        .item(&spine.pop_out_tab)
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
                let launch = if w.open_on_start {
                    ""
                } else {
                    " (dormant — open_on_start=false; opened from the Window menu)"
                };
                println!("  window {:?}{}", w.title, launch);
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

pub fn run() {
    // Register the shell-core plugins (window-state + updater + process) — the set every sibling app
    // installs identically. Window-state persists each window's size/position/maximized keyed by
    // Tauri label within a per-config state file (scoped by shell-core's `state_filename` from the
    // config path below) so two configs sharing a window title don't share bounds; both save and
    // restore (the plugin's window_created hook, on the main loop) are automatic — build_window must
    // NOT restore by hand (deadlocks; see the footgun there). The shared home surface (shell-home,
    // replacing curator's own error window) is excluded from state restore.
    let config_path = curator_config::resolve_config_path();
    shell_core::register_plugins(
        tauri::Builder::default(),
        Some(&config_path),
        &[shell_core::home::HOME_LABEL],
    )
    .setup(move |app| {
        // Prime native banner notifications (authorization + presentation/click delegate) and
        // capture the app handle the click delegate uses to surface a tab. The banner path is a
        // no-op in dev / off the packaged app; the badge/sentinel path is independent of this.
        notification::init(app.handle().clone());

        // Native mouse side-button (back/forward) navigation — the shared shell-core NSEvent monitor
        // (WKWebView never delivers these to the DOM, so it can't be done in the page). curator
        // supplies only the focused-active-webview resolver; shell-core owns the monitor + the
        // native goBack/goForward.
        let mouse_nav_handle = app.handle().clone();
        shell_core::mouse_nav::install(move || commands::focused_active_webview(&mouse_nav_handle));

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
            // `open_on_start = false` windows are configured-but-dormant: registered so the Window
            // menu / home surface can list and reopen them, but not built at launch. Everything else
            // materializes now. This is the only site that consults `open_on_start` — hot-reload
            // reconcile deliberately ignores it (launch-only gate; see the field doc and warden).
            if win_cfg.open_on_start {
                let (wid, rt) = open_window(
                    &handle,
                    cfg.dark_mode,
                    cfg.session.as_deref(),
                    win_cfg,
                    &HashSet::new(),
                )?;
                runtimes.insert(wid, rt);
            } else {
                let wid = curator_config::identity::window_id(&win_cfg.title);
                runtimes.insert(wid, dormant_runtime(win_cfg, cfg.session.as_deref()));
            }
        }
        app.manage(AppState {
            windows: Mutex::new(runtimes),
            detached: Mutex::new(HashMap::new()),
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
        // The shared shell-core watcher owns the parent-dir watch, the file-name match (macOS
        // FSEvents-robust — this is the fix for the old exact-path bug that silently missed every
        // event under a symlinked config dir), and the echo-swallow (via the `Option<String>` the
        // closure returns when it format-writes). curator supplies just the parse + apply.
        let app_handle = app.handle().clone();
        let fmt_path = path.clone();
        shell_core::watch::watch_config(path.clone(), move |src| match watcher::reconcile(src) {
            Ok((new_cfg, warnings)) => {
                log_config_warnings(&warnings);
                // Format-on-save: rewrite in house style on a clean reload. The write is
                // diff-guarded, so an already-formatted file is a no-op; when it does rewrite,
                // return the formatted bytes so the watcher swallows the echo (one reload per user
                // save, not two). Formatting only touches whitespace, so `new_cfg` (parsed
                // pre-format) already matches the formatted file's config.
                let self_write = if new_cfg.format_on_save {
                    let formatted = curator_config::format_str(src);
                    if formatted != src {
                        match curator_config::format_file(&fmt_path) {
                            Ok(_) => Some(formatted),
                            Err(e) => {
                                eprintln!("config format error: {e}");
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                reload_windows(&app_handle, &new_cfg);
                self_write
            }
            Err(msg) => {
                // Surface in each window's sidebar, and reconcile the shared home surface
                // (Broken state) in case every window happens to be closed.
                emit_to_all_chrome(&app_handle, "config-error", msg.clone());
                let state = app_handle.state::<AppState>();
                let entries = window_entries(&app_handle, &state);
                reconcile_home(&app_handle, &entries, &fmt_path, true, Some(&msg));
                None
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
                // ⌘⇧O pops the focused window's active tab out into its own window. The chrome owns
                // which tab is active, so it drives pop_out_tab off this event (routed to only the
                // focused window's chrome, curator's per-window emit pattern).
                shell_core::menu::ids::POP_OUT_TAB => {
                    emit_to_focused_chrome(app, "pop-out-tab", ())
                }
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
        commands::pop_out_tab,
        commands::raise_popped_window,
        commands::pop_in_tab,
        commands::shell_home_create_config,
        commands::shell_home_edit_config,
        commands::shell_home_open_window,
    ])
    .build(tauri::generate_context!())
    .expect("error while building curator")
    .run(|_app, event| {
        // ExitRequested fires before every window's Destroyed during ⌘Q; mark quitting so a
        // detached window's teardown doesn't reopen its origin mid-quit (see `redock`).
        if let tauri::RunEvent::ExitRequested { .. } = event {
            mark_quitting();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_token_round_trips_through_the_detached_label() {
        // A content-webview label is `{origin_wid}:tab-<hash>` — the token IS that label, and it
        // must survive the shell-core detached-label wrapping so a detached window is recognised
        // (is_detached_label) and its token recoverable (detach_token). Deterministic, too.
        let tab_label = "w0123456789abcdef:tab-00112233445566ff";
        let token = detach_window_token(tab_label);
        assert_eq!(token, detach_window_token(tab_label)); // stable
        let label = shell_core::detach::detached_label(&token);
        assert!(shell_core::detach::is_detached_label(&label));
        assert_eq!(
            shell_core::detach::detach_token(&label),
            Some(token.as_str())
        );
        // A real (config-defined) window label is never mistaken for a detached one.
        assert!(!shell_core::detach::is_detached_label(tab_label));
    }
}
