mod awareness;
mod commands;
mod notification;
#[cfg(target_os = "macos")]
mod pointer;
mod sentinels;
mod watcher;
mod webviews;
mod windows;
#[cfg(target_os = "macos")]
mod zorder;

use curator_config::WindowTabs;
use shell_core::menu::WindowEntry;
use shell_core::windows::plan::{entry_for_title, Slot, WindowDecl, WindowOp};
use shell_core::windows::WindowSet;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{Emitter, Manager, Theme};

/// Window theme to apply for a given `dark_mode` setting. `None` = follow the system.
fn theme_for(dark_mode: bool) -> Option<Theme> {
    dark_mode.then_some(Theme::Dark)
}

/// An open window's runtime state: its config, lazy/active tab bookkeeping, and the awareness state
/// (per-tab unread + which tabs have gone Badging-authoritative) feeding the dock badge. Exists only
/// while the window is open; [`WindowSet`] keeps closed windows listed.
pub struct WindowRuntime {
    /// The window's Tauri label (from [`WindowSet`]), which namespaces its content webview labels.
    pub label: String,
    pub cfg: curator_config::WindowConfig,
    /// App-wide session base (`Config.session`) captured for this window, so commands and reopen
    /// can re-resolve the session chain without the whole `Config`. Kept in sync on hot-reload.
    pub global_session: Option<String>,
    pub tabs: webviews::TabState,
    pub unread: HashMap<String, awareness::Unread>,
    pub badge_authoritative: HashSet<String>,
    /// Tabs carrying a notification-derived activity dot ([`awareness::displayed`]) — the unread
    /// source for a service that reports through neither the Badging API nor a title count.
    /// Cleared by selecting the tab ([`awareness::mark_read`]); nothing in the page ever clears it.
    pub notified: HashSet<String>,
    /// [`ReloadCancel::cancel`] stops this window's `reload_every` timer threads — on window close,
    /// removal, or when its tab set changes (a fresh generation is spawned with a new signal).
    pub reload_cancel: Arc<ReloadCancel>,
    /// The content-hole rect (logical px) the chrome last reported via `set_hole_rect`, seeded from
    /// [`webviews::initial_hole`]. Lazily-created / hot-reload-added tabs are placed here so they
    /// land in the current hole. Guarded by the `windows` mutex like every other field — the chrome
    /// owns the sidebar width and clamp, so Rust just tracks the reported geometry (no Rust clamp).
    pub hole: webviews::HoleRect,
}

impl WindowRuntime {
    /// This window's tabs, labelled under its window label.
    pub fn views(&self) -> Vec<curator_config::TabView> {
        self.cfg
            .tab_views(&self.label, self.global_session.as_deref())
    }

    /// The tab's configured [`UnreadMode`](curator_config::UnreadMode). Resolved from `cfg` on
    /// demand rather than cached alongside `unread`/`notified`: a cache would be a second copy of
    /// config state to rebuild on every hot-reload, and this runs only on a badge/title/notify
    /// event. An unknown label (a tab dropped by a reload mid-flight) falls back to the default.
    pub fn unread_mode(&self, label: &str) -> curator_config::UnreadMode {
        self.views()
            .into_iter()
            .find(|v| v.label == label)
            .map(|v| v.unread)
            .unwrap_or_default()
    }
}

/// A tab popped out into its own detached window (`commands::pop_out_tab`). Kept in
/// [`AppState::detached`], **separate from `windows`** and from the [`WindowSet`], so hot-reload
/// reconcile never sees it. Geometry persistence, by contrast, *does* cover these windows — the
/// label is stable per tab, so a popped-out tab reopens at the size it was left at.
/// Holds the origin bookkeeping needed to return the tab: which window it came from and which tab.
/// A curator tab is a webview that is *recreated* on redock from the origin's current config (login
/// survives via the session-keyed data store), so, unlike warden, there is no live native surface
/// to hold here.
///
/// The origin is known by its **title**, patched on an in-place rename ([`retitle_origins`]):
/// a window renamed in place and then closed comes back under a label made from its new title, and
/// another window may take the label it left. `origin_wid` and `tab_label` follow the window it
/// resolves to ([`CuratorDetached::rehome`]).
pub struct CuratorDetached {
    pub origin_wid: String,
    pub origin_title: String,
    pub tab_label: String,
}

impl CuratorDetached {
    /// Point the bookkeeping at the origin's label `wid`: the tab's label is namespaced under it.
    pub(crate) fn rehome(&mut self, wid: &str) {
        if self.origin_wid != wid {
            self.tab_label = shell_core::label::rehome(&self.tab_label, wid);
            self.origin_wid = wid.to_string();
        }
    }
}

/// After a window closed, against the set's `entries`: drop the bookkeeping of every tab whose
/// origin left the config, returning their detached-window labels to destroy, and rehome the rest
/// onto the label their origin resolves to now, so none keeps a label the closed window freed.
pub(crate) fn settle_detached(
    detached: &mut HashMap<String, CuratorDetached>,
    entries: &[WindowEntry],
) -> Vec<String> {
    let mut orphans = Vec::new();
    detached.retain(|label, d| match entry_for_title(entries, &d.origin_title) {
        Some(home) => {
            d.rehome(&home.id);
            true
        }
        None => {
            orphans.push(label.clone());
            false
        }
    });
    orphans
}

/// The tabs popped out of the window `wid` titled `title`, which is being built: matched by title
/// and rehomed onto `wid`, since a reopened origin may hold another label than the one it left.
pub(crate) fn popped_out_of(
    detached: &mut HashMap<String, CuratorDetached>,
    wid: &str,
    title: &str,
) -> HashSet<String> {
    detached
        .values_mut()
        .filter(|d| d.origin_title.trim() == title.trim())
        .map(|d| {
            d.rehome(wid);
            d.tab_label.clone()
        })
        .collect()
}

/// Carry a window's in-place rename from `old` to `new` into the popped-out tabs' origin titles.
pub(crate) fn retitle_origins(
    detached: &mut HashMap<String, CuratorDetached>,
    old: &str,
    new: &str,
) {
    for d in detached
        .values_mut()
        .filter(|d| d.origin_title.trim() == old.trim())
    {
        d.origin_title = new.trim().to_string();
    }
}

/// Resolve the popped-out tab `detached_label`'s origin by title and rehome its bookkeeping there:
/// the origin's label and whether it is open. `None` when the tab isn't popped out or its origin
/// left the config.
pub(crate) fn resolve_origin(
    app: &tauri::AppHandle,
    detached_label: &str,
) -> Option<(String, bool)> {
    let entries = app.state::<WindowSet>().entries();
    let state = app.state::<AppState>();
    let mut detached = state.detached.lock().unwrap();
    let d = detached.get_mut(detached_label)?;
    let home = entry_for_title(&entries, &d.origin_title)?;
    d.rehome(&home.id);
    Some((home.id.clone(), home.open))
}

/// The opaque token identifying a popped-out tab's detached window
/// (→ [`shell_core::detach::detached_label`]). A curator content-webview label is already
/// `{origin_wid}:tab-<hash>` — globally unique across windows and Tauri-label-safe — so it *is* the
/// token; the origin window is tracked on [`CuratorDetached`], never parsed back out of the label.
pub(crate) fn detach_window_token(tab_label: &str) -> String {
    tab_label.to_string()
}

/// The cancel signal shared with a window's `reload_every` timer threads: a flag they can wait *on*
/// rather than poll.
///
/// **Footgun — don't go back to polling the flag on a short sleep.** The obvious shape (`sleep(1s)`
/// in a loop re-checking an `AtomicBool`) wakes each reloading tab's thread 3,600 times an hour to
/// read a bool that is almost never set, and chaining that many `sleep`s lets the interval drift off
/// the configured minute. A condvar waits out the *whole* interval in one blocked wait, costs zero
/// wakeups, and is woken the instant [`ReloadCancel::cancel`] fires — so the prompt-exit property
/// that motivated the 1s chunks is kept, not traded away.
pub struct ReloadCancel {
    cancelled: Mutex<bool>,
    woken: Condvar,
}

impl ReloadCancel {
    /// A live (not yet cancelled) signal.
    fn new() -> Self {
        Self {
            cancelled: Mutex::new(false),
            woken: Condvar::new(),
        }
    }

    /// Cancel every timer waiting on this signal, waking them immediately.
    fn cancel(&self) {
        *self.cancelled.lock().unwrap() = true;
        self.woken.notify_all();
    }

    /// Block for up to `dur`. Returns `true` if cancelled (the caller must exit without reloading),
    /// `false` if the full interval elapsed. `wait_timeout_while` re-checks across spurious wakeups
    /// and tracks the remaining deadline itself.
    fn wait_cancelled(&self, dur: std::time::Duration) -> bool {
        let guard = self.cancelled.lock().unwrap();
        let (cancelled, _) = self
            .woken
            .wait_timeout_while(guard, dur, |c| !*c)
            .expect("reload-cancel mutex poisoned");
        *cancelled
    }
}

/// Spawn one background thread per `reload_every` tab that reloads it on schedule until `cancel`
/// fires. Each thread spends the interval in a single blocked wait and never reloads after
/// cancellation — so closed/removed windows don't leak threads that keep poking dead webviews.
fn spawn_reload_timers(
    window: &tauri::Window,
    views: &[curator_config::TabView],
    cancel: Arc<ReloadCancel>,
) {
    for v in views.iter().filter(|v| v.reload_every.is_some()) {
        let interval = std::time::Duration::from_secs(v.reload_every.unwrap() * 60);
        let label = v.label.clone();
        let url = v.url.clone();
        let win = window.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            while !cancel.wait_cancelled(interval) {
                let _ = webviews::reload_canonical(&win, &label, &url);
            }
        });
    }
}

/// The open windows' runtimes, keyed by window label (== chrome webview label == content-label
/// prefix), plus app-wide live settings.
pub struct AppState {
    pub windows: Mutex<HashMap<String, WindowRuntime>>,
    /// Tabs currently popped out into their own detached window, keyed by the detached window's
    /// Tauri label ([`shell_core::detach::detached_label`]). **Separate from `windows`** so
    /// reconcile never touches these ephemeral windows; their count is mirrored into the
    /// [`WindowSet`] ([`sync_detached`]) — a detached window is a real surface on screen.
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
    /// Current app-wide `tab_digit_keys`, kept live across hot-reload so the menu rebuild
    /// (`refresh_window_menu`) uses the new mode — a flip applies without a relaunch.
    pub tab_digit_keys: Mutex<curator_config::TabDigitKeys>,
    /// The most recently applied config: what [`open_window`] builds a window's tabs from, whenever
    /// the [`WindowSet`] opens one (launch, reload, reopen). Seeded at setup, replaced by every
    /// [`reload_windows`] before it applies.
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

/// [`WindowHost::build`](shell_core::windows::WindowHost::build): build `slot`'s window from the
/// last applied config — eager-create its `load_on_open` tabs (loaded at launch and kept live),
/// lazily defer the rest, lay them out around the active tab (`apply_active`), start per-tab
/// reload timers — and register its runtime.
pub(crate) fn open_window(
    app: &tauri::AppHandle,
    slot: &Slot,
) -> tauri::Result<tauri::WebviewWindow> {
    let state = app.state::<AppState>();
    let wid = slot.label.as_str();
    let (win_cfg, global_session) = {
        let cfg = state.last_cfg.lock().unwrap();
        let w = cfg
            .windows
            .iter()
            .find(|w| w.title.trim() == slot.decl.title.trim())
            .cloned()
            .ok_or(tauri::Error::WindowNotFound)?;
        (w, cfg.session.clone())
    };
    let global_session = global_session.as_deref();
    // Tabs of this window currently popped out (a reopen while one is out): their webviews live on
    // the detached window, so they stay popped-out placeholders until they redock.
    let detached_tabs = popped_out_of(&mut state.detached.lock().unwrap(), wid, &slot.decl.title);
    let webview_window = webviews::build_window(
        app,
        wid,
        &slot.decl.title,
        slot.decl.width,
        slot.decl.height,
    )?;
    let window = webview_window.as_ref().window();
    window.set_theme(theme_for(state.dark_mode.load(Ordering::Relaxed)))?;
    // Best-guess hole until the chrome mounts and reports its `#content-hole` via `set_hole_rect`;
    // launch-time `load_on_open` tabs are placed here for the frame or two before that first report.
    let hole = webviews::initial_hole(slot.decl.width, slot.decl.height);

    let views = win_cfg.tab_views(wid, global_session);
    let mut tabs = webviews::TabState::default();

    // Eager-create the `load_on_open` tabs: they load at launch and stay live (never hidden), so
    // they keep syncing and can notify in the background. Everything else is lazy. A popped-out tab
    // is NOT recreated here (it would collide on the app-global label).
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
    let active = win_cfg.startup_label(wid, global_session);
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
    let reload_cancel = Arc::new(ReloadCancel::new());
    spawn_reload_timers(&window, &views, reload_cancel.clone());

    state.windows.lock().unwrap().insert(
        wid.to_string(),
        WindowRuntime {
            label: wid.to_string(),
            cfg: win_cfg,
            global_session: global_session.map(str::to_string),
            tabs,
            unread: HashMap::new(),
            badge_authoritative: HashSet::new(),
            notified: HashSet::new(),
            reload_cancel,
            hole,
        },
    );
    Ok(webview_window)
}

/// [`WindowHost::closed`](shell_core::windows::WindowHost::closed): drop `label`'s runtime — its
/// unread, its reload timers — and recompute the dock badge, and settle the popped-out tabs
/// ([`settle_detached`]): one whose window left the config closes with it. The [`WindowSet`]
/// already ended its pages in the window's `CloseRequested`.
pub(crate) fn window_closed(app: &tauri::AppHandle, label: &str) {
    let state = app.state::<AppState>();
    if let Some(rt) = state.windows.lock().unwrap().remove(label) {
        rt.reload_cancel.cancel();
    }
    let entries = app.state::<WindowSet>().entries();
    let orphans = settle_detached(&mut state.detached.lock().unwrap(), &entries);
    if !orphans.is_empty() {
        for detached in orphans {
            if let Some(w) = app.get_window(&detached) {
                let _ = w.destroy();
            }
        }
        sync_detached(app);
    }
    awareness::refresh_dock_badge(app, &state, Some(label));
    // During ⌘Q every window is being destroyed and the menu dies with the app.
    if !app.state::<WindowSet>().is_quitting() {
        refresh_window_menu(app);
    }
}

/// [`WindowHost::retitled`](shell_core::windows::WindowHost::retitled): the window titled `old`,
/// under `label`, changed title or colour in place; carry it into `last_cfg` (so [`open_window`]
/// finds a renamed window before the reload echo) and into the origin titles of the tabs popped
/// out of it, and repaint its banner.
pub(crate) fn window_retitled(app: &tauri::AppHandle, label: &str, old: &str, decl: &WindowDecl) {
    let state = app.state::<AppState>();
    retitle_origins(&mut state.detached.lock().unwrap(), old, &decl.title);
    curator_config::retitle_window(
        &mut state.last_cfg.lock().unwrap().windows,
        old,
        &decl.title,
        decl.colour.clone(),
    );
    if let Some(rt) = state.windows.lock().unwrap().get_mut(label) {
        rt.cfg.title = decl.title.clone();
        rt.cfg.colour = decl.colour.clone();
    }
    let _ = app.emit_to(label, "config-reloaded", ());
}

/// Mirror the popped-out window count into the [`WindowSet`], which keeps the home surface away
/// while one is up.
pub(crate) fn sync_detached(app: &tauri::AppHandle) {
    let count = app.state::<AppState>().detached.lock().unwrap().len();
    app.state::<WindowSet>().set_detached_count(app, count);
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
pub(crate) fn emit_to_all_chrome<S: serde::Serialize + Clone>(
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

/// The `nav-tab` payload: step `dir` from `from`, a popped-out tab, or else from the highlight.
#[derive(Clone, serde::Serialize)]
struct NavTab {
    dir: i32,
    from: Option<String>,
}

/// Runs a spine command in the window `shell_core::menu::route` chose. The chrome is the window's
/// main webview, so the target label *is* the chrome's.
fn on_spine_action(app: &tauri::AppHandle, routed: shell_core::menu::Routed) {
    use shell_core::menu::{SpineAction, TabNavAction};
    let target = routed.target.as_str();
    if routed.from_detached.is_some() {
        match app.get_window(target) {
            Some(w) => {
                let _ = w.set_focus();
            }
            // The pop-out's origin was closed: bring it back. Its chrome mounts fresh, with nothing
            // listening yet for this command, so the command ends there.
            None => {
                open_or_focus_window(app, target);
                return;
            }
        }
    }
    let from = routed.from_detached.as_deref().and_then(|l| {
        let state = app.state::<AppState>();
        let detached = state.detached.lock().unwrap();
        detached.get(l).map(|d| d.tab_label.clone())
    });
    let emit = |event: &str| {
        let _ = app.emit_to(target, event, ());
    };
    match routed.action {
        SpineAction::Nav(TabNavAction::Next) => {
            let _ = app.emit_to(target, "nav-tab", NavTab { dir: 1, from });
        }
        SpineAction::Nav(TabNavAction::Prev) => {
            let _ = app.emit_to(target, "nav-tab", NavTab { dir: -1, from });
        }
        SpineAction::Nav(TabNavAction::Jump(n)) => {
            let _ = app.emit_to(target, "jump-tab", n);
        }
        // ⌘W unloads the ACTIVE TAB — it does not close the window. The chrome owns which tab is
        // active and the dot repaint, so it drives unload_tab off this event.
        SpineAction::CloseTab => emit("close-tab"),
        // `close()` flows through the same `CloseRequested` path as the red button (the
        // `WindowSet`'s); a pop-out's close docks its tab.
        SpineAction::DockPoppedOut | SpineAction::CloseWindow => {
            if let Some(w) = app.get_window(target) {
                let _ = w.close();
            }
        }
        SpineAction::PopOutTab => emit("pop-out-tab"),
        // ⌘⇧F: the content webview may hold first responder, so make the chrome the key view
        // before it focuses its field.
        SpineAction::FindInSidebar => {
            if let Some(chrome) = app.get_webview(target) {
                let _ = chrome.set_focus();
            }
            emit("find-in-sidebar");
        }
        // chrome-core owns self-update; forward to its checkForUpdateNow().
        SpineAction::CheckUpdates => emit("check-update"),
    }
}

/// Apply a new config to the running app: the [`WindowSet`] opens, closes and retitles windows,
/// then each kept open window reconciles its tabs. Emits `config-reloaded` to every open chrome.
/// The single path for both the config-file watcher and the home surface's "Create a starter
/// config" command.
pub(crate) fn reload_windows(app: &tauri::AppHandle, new_cfg: &curator_config::Config) {
    let state = app.state::<AppState>();
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
    // Same for tab_digit_keys — the menu rebuild below reads this, so a flip applies live.
    *state.tab_digit_keys.lock().unwrap() = new_cfg.tab_digit_keys;

    // `open_window` builds from `last_cfg`, so it must be the new config before the set opens any.
    *state.last_cfg.lock().unwrap() = new_cfg.clone();

    let set = app.state::<WindowSet>();
    let ops = set.apply(app, windows::decls(new_cfg));
    let opened: HashSet<String> = ops
        .iter()
        .filter_map(|op| match op {
            WindowOp::Open(slot) => Some(slot.label.clone()),
            _ => None,
        })
        .collect();

    // Kept open windows: reconcile tabs against the new config.
    for entry in set.entries() {
        if !entry.open || opened.contains(&entry.id) {
            continue;
        }
        let Some(win_cfg) = new_cfg
            .windows
            .iter()
            .find(|w| w.title.trim() == entry.title.trim())
        else {
            continue;
        };
        let Some(window) = app.get_window(&entry.id) else {
            continue;
        };
        let _ = window.set_theme(theme_for(new_cfg.dark_mode));
        reconcile_window_tabs(
            &state,
            &window,
            &entry.id,
            new_cfg.session.as_deref(),
            win_cfg,
        );
    }

    // The Window submenu tracks windows that came or went.
    refresh_window_menu(app);

    // Refresh the aggregate dock badge to reflect windows that came or went (a removed window's
    // unread is dropped with its runtime; without this its count would linger until the next
    // badge event).
    awareness::refresh_dock_badge(app, &state, None);

    // Surface the reload on every window that's still open.
    emit_to_all_chrome(app, "config-reloaded", ());
}

/// Rebuild the app menu so the Window submenu matches the [`WindowSet`]. The tick /
/// `"{title}  (closed)"` label is baked in at build time, so every path that opens, closes,
/// renames or deletes a window calls this.
pub(crate) fn refresh_window_menu(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let entries = app.state::<WindowSet>().entries();
    let mode = *state.tab_digit_keys.lock().unwrap();
    if let Ok(menu) = build_app_menu(app, mode, &entries) {
        let _ = app.set_menu(menu);
    }
}

/// The menu spine's Window submenu selector, and the home surface's per-window button
/// (`shell_home_open_window`): focus `label`'s window, or reopen it.
pub(crate) fn open_or_focus_window(app: &tauri::AppHandle, label: &str) {
    app.state::<WindowSet>().open_or_focus(app, label);
    refresh_window_menu(app);
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
    if app.state::<WindowSet>().is_quitting() {
        return;
    }
    let state = app.state::<AppState>();

    // Find the origin by title and point the bookkeeping at its current label, then peek the
    // origin + tab without removing (see the doc comment's ordering rationale).
    let home = resolve_origin(app, detached_label);
    let Some((origin_wid, tab_label)) = state
        .detached
        .lock()
        .unwrap()
        .get(detached_label)
        .map(|d| (d.origin_wid.clone(), d.tab_label.clone()))
    else {
        return; // already redocked (double-close) — nothing to do
    };

    match home {
        // Reopen the origin if the user closed it while the tab was popped out (case: another
        // window or the detached window itself kept the app alive past last-window-quit).
        Some((_, false)) => open_or_focus_window(app, &origin_wid),
        Some(_) => {}
        // The origin left the config: the tab has no home, and the label it left may be another
        // window's now.
        None => {
            state.detached.lock().unwrap().remove(detached_label);
            sync_detached(app);
            return;
        }
    }

    // Now take the bookkeeping and recreate the tab on the origin.
    if state
        .detached
        .lock()
        .unwrap()
        .remove(detached_label)
        .is_none()
    {
        return; // raced with a double-close
    }
    sync_detached(app);
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
        let views = rt.views();
        // Recreate from the *current* config's view, not the one captured at pop-out: a session
        // (or other per-tab) edit made while the tab was out applies on return.
        let current = views.iter().find(|v| v.label == tab_label).cloned();
        rt.tabs.clear_detached(&tab_label);
        match current {
            None => None, // tab removed from config while detached — it simply ends
            Some(view) => {
                rt.tabs.mark_created(&tab_label);
                rt.tabs.set_active(&tab_label);
                Some((
                    view,
                    rt.hole,
                    views,
                    rt.tabs.active().map(str::to_string),
                    rt.cfg.colour.clone(),
                ))
            }
        }
    };
    match plan {
        Some((view, hole, views, active, colour)) => {
            let _ = webviews::create_content_webview(&window, &view, hole, colour.as_deref());
            let _ = webviews::apply_active(&window, active.as_deref(), &views);
            // It returns as the active tab, so a dot it collected while out is now seen.
            awareness::mark_read(app, &origin_wid, &tab_label);
        }
        // The popped-out webview reported unread into the origin's runtime; with the tab gone,
        // nothing will ever clear it.
        None => awareness::forget_tab(app, &origin_wid, &tab_label),
    }

    // Re-render the origin chrome so the returned row loses its ⤢ detached mark and reflects the new
    // active tab. `config-reloaded` drives the chrome's refresh() (a get_tabs re-fetch); emit_to
    // targets only that window's chrome (curator's per-window emit scoping). If the origin was just
    // reopened its fresh mount already refreshes, so a missed emit self-corrects.
    let _ = app.emit_to(origin_wid.as_str(), "config-reloaded", ());
}

/// Labels present in both `old` and `new` whose resolved login `session` differs. A tab's label
/// hashes its URL alone, so a session edit keeps the label — but a live webview's data store is
/// fixed at creation, so such a tab must be rebuilt to pick the new session up.
fn session_changed(
    old: &[curator_config::TabView],
    new: &[curator_config::TabView],
) -> HashSet<String> {
    new.iter()
        .filter(|n| {
            old.iter()
                .any(|o| o.label == n.label && o.session != n.session)
        })
        .map(|n| n.label.clone())
        .collect()
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
    let views = win_cfg.tab_views(window_id, global_session);

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
        let rebuild = session_changed(&rt.views(), &views);
        let keep: HashSet<String> = views
            .iter()
            .map(|v| v.label.clone())
            .filter(|l| !rebuild.contains(l))
            .collect();
        let prior_active = rt.tabs.active().map(str::to_string);
        rt.cfg = win_cfg.clone();
        rt.global_session = global_session.map(str::to_string);
        // Read the current hole rect under the lock to pass to create below (re-locking inside
        // create_content_webview would self-deadlock the non-reentrant mutex).
        let hole = rt.hole;

        // Orphans: created tabs no longer in the config (removed, or URL/label changed) or whose
        // session changed. Forget all their state; the webviews are closed after the lock is
        // dropped, and a session-changed tab is recreated below like any uncreated one.
        let orphans = rt.tabs.orphans(&keep);
        for label in &orphans {
            rt.tabs.mark_unloaded(label);
            rt.unread.remove(label);
            rt.badge_authoritative.remove(label);
            rt.notified.remove(label);
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
        // cleared it if it was orphaned) or is only being rebuilt for a session change, else fall
        // back to startup_label (by default the first load_on_open tab). Ensure it's created. A
        // detached tab can't be active (its content is on the detached window), so it's never
        // created or promoted here.
        let active = rt
            .tabs
            .active()
            .map(str::to_string)
            .or_else(|| prior_active.filter(|a| rebuild.contains(a)))
            .or_else(|| win_cfg.startup_label(window_id, global_session))
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
        rt.reload_cancel.cancel();
        let reload_cancel = Arc::new(ReloadCancel::new());
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
            shell_core::web::content::close_content_webview(&wv);
        }
    }
    for v in &to_create {
        let _ = webviews::create_content_webview(window, v, hole, colour.as_deref());
    }
    spawn_reload_timers(window, &views, reload_cancel);
    awareness::set_dock_badge(window.app_handle(), dock_total, None);
    // Re-apply the layout so the content area isn't left on a closed orphan.
    let _ = webviews::apply_active(window, active.as_deref(), &views);
    // A tab made active here is one the user is now looking at: clear its dot.
    if let Some(label) = active.as_deref() {
        awareness::mark_read(window.app_handle(), window_id, label);
    }
}

/// Build the full app menu: shell-core's App, Edit, Tab, Config and Window submenus, with
/// curator's Reload Tab / Reset All Tabs / Open Developer Tools as the Tab submenu's one app group.
/// Rebuilt on every hot-reload (a `tab_digit_keys` flip, the Window submenu tracking the config)
/// and whenever a window opens or closes.
fn build_app_menu<R: tauri::Runtime, M: Manager<R>>(
    manager: &M,
    mode: curator_config::TabDigitKeys,
    window_entries: &[shell_core::menu::WindowEntry],
) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};

    let spine = shell_core::menu::build_spine(
        manager,
        shell_core::menu::SpineConfig {
            app_name: "curator",
            windows: window_entries,
        },
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_GIT_SHA"),
        env!("BUILD_DATE"),
    )?;

    let reload_tab = MenuItemBuilder::with_id("reload_active", "Reload Tab")
        .accelerator("CmdOrCtrl+R")
        .build(manager)?;
    let reset = MenuItemBuilder::with_id("reset_all", "Reset All Tabs").build(manager)?;
    let devtools = MenuItemBuilder::with_id("open_devtools", "Open Developer Tools")
        .accelerator("CmdOrCtrl+Alt+I")
        .build(manager)?;
    let tabs_menu = shell_core::menu::build_tab_menu(
        manager,
        &spine,
        mode.is_cycle(),
        &[&[&reload_tab, &reset, &devtools]],
    )?;

    MenuBuilder::new(manager)
        .items(&[
            &spine.submenus[0], // App
            &spine.edit,
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
                for v in w.tab_views(&w.title, cfg.session.as_deref()) {
                    let group = v
                        .group
                        .as_deref()
                        .map(|g| format!(" group={g:?}"))
                        .unwrap_or_default();
                    // Only shown when narrowed — printing `unread=all` on every line would bury
                    // the one tab that isn't, which is the whole reason to look.
                    let unread = match v.unread {
                        curator_config::UnreadMode::All => String::new(),
                        m => format!(" unread={m:?}").to_lowercase(),
                    };
                    println!(
                        "    tab {:?} url={} load_on_open={}{} session={:?}{}",
                        v.title, v.url, v.load_on_open, unread, v.session, group
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
    // Register the shell-core plugins (geometry + updater + process) — the set every sibling app
    // installs identically. Geometry persists each window's size/position, in AppKit points, keyed by
    // Tauri label within a per-config store file (scoped by shell-core's `geometry_filename` from the
    // config path below) so two configs sharing a window title don't share bounds. `maximized` is
    // not persisted — on macOS the green button zooms to a fullscreen space, and geometry recorded
    // while fullscreen is exactly what the plugin refuses to save. Both save (on move/resize and at
    // exit) and restore (the plugin's `on_window_ready` hook, on the main loop) are automatic —
    // build_window must NOT restore by hand (deadlocks; see the footgun there). Only the shared home
    // surface is excluded, structurally inside shell-core rather than via the skip_labels argument;
    // popped-out tab windows persist, keyed by their `shell-detach:` label.
    let config_path = curator_config::resolve_config_path();
    shell_core::register_plugins(tauri::Builder::default(), Some(&config_path), &[])
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
            shell_core::mouse_nav::install(move || {
                commands::focused_active_webview(&mouse_nav_handle)
            });

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
            shell_core::web::insecure::set_allowlist(cfg.allow_insecure.clone());

            let handle = app.handle().clone();
            app.manage(AppState {
                windows: Mutex::new(HashMap::new()),
                detached: Mutex::new(HashMap::new()),
                dark_mode: AtomicBool::new(cfg.dark_mode),
                density: Mutex::new(cfg.density),
                sidebar_drag: AtomicBool::new(cfg.sidebar_drag),
                auto_update: AtomicBool::new(cfg.auto_update),
                tab_digit_keys: Mutex::new(cfg.tab_digit_keys),
                last_cfg: Mutex::new(cfg.clone()),
            });

            // The window set opens the `open_on_start` windows, lists the rest, and shows the home
            // surface whenever no window is open (a failed load, no `[[window]]`, all closed). The
            // user closing the last window quits instead (matching warden), rather than lingering
            // as a menu-bar-only app.
            app.manage(
                WindowSet::new("curator", path.clone(), windows::CuratorHost)
                    .quit_on_last_close(true),
            );
            let set = app.state::<WindowSet>();
            match load_err {
                None => set.materialize(&handle, windows::decls(&cfg)),
                Some(e) => set.set_load_error(&handle, Some(e)),
            }
            let entries = set.entries();

            // Watch the config file and hot-reload on change, keeping the last-good config
            // (and surfacing an error banner on each open window) if the new contents don't
            // parse/validate.
            // The shared shell-core watcher owns the parent-dir watch, the file-name match (macOS
            // FSEvents-robust, so a symlinked config dir's events are seen), and the echo-swallow (via the `Option<String>` the
            // closure returns when it format-writes). curator supplies just the parse + apply.
            let app_handle = app.handle().clone();
            let fmt_path = path.clone();
            shell_core::watch::watch_config(path.clone(), move |src| {
                match watcher::reconcile(src) {
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
                        // Surface in each window's sidebar, and on the home surface (Broken state)
                        // in case every window happens to be closed.
                        emit_to_all_chrome(&app_handle, "config-error", msg.clone());
                        app_handle
                            .state::<WindowSet>()
                            .set_load_error(&app_handle, Some(msg));
                        None
                    }
                }
            });

            // We replace Tauri's default menu, so the spine re-adds the standard macOS ones (Edit
            // owns the clipboard accelerators content webviews need). Rebuilt on hot-reload so the
            // Window submenu tracks windows.
            let menu = build_app_menu(app, cfg.tab_digit_keys, &entries)?;
            app.set_menu(menu)?;

            let cfg_path = path.clone();
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref();
                // The spine's file-acting ids (Edit Config, Reveal Config) need no window — let it
                // consume them first.
                if shell_core::menu::handle_spine_event(id, &cfg_path) {
                    return;
                }
                // Tab ▸ Cycle Tabs: save the flip; the watcher's reload rebuilds the menu. muda
                // already flipped the tick, so a failed write rebuilds to undo it.
                if id == shell_core::menu::ids::TAB_DIGIT_CYCLE {
                    if let Err(e) = commands::toggle_tab_digit_keys(&cfg_path) {
                        // Every sidebar, and the home surface when no window is open.
                        emit_to_all_chrome(app, "config-error", e.clone());
                        app.state::<WindowSet>().set_load_error(app, Some(e));
                        refresh_window_menu(app);
                    }
                    return;
                }
                match id {
                    "reload_active" => {
                        commands::reload_active_tab(app);
                        return;
                    }
                    "reset_all" => {
                        let _ = commands::reset_all_tabs(app);
                        return;
                    }
                    "open_devtools" => {
                        commands::open_active_devtools(app);
                        return;
                    }
                    _ => {}
                }
                if app.state::<WindowSet>().handle_menu(app, id) {
                    return;
                }
                if let Some(label) = shell_core::menu::selected_window(id) {
                    open_or_focus_window(app, label);
                    return;
                }
                let Some(focused) = app.get_focused_window() else {
                    return;
                };
                let origin_of = |l: &str| resolve_origin(app, l).map(|(wid, _)| wid);
                if let Some(routed) = shell_core::menu::route(id, focused.label(), origin_of) {
                    on_spine_action(app, routed);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_tabs,
            commands::window_identity,
            commands::select_tab,
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
            commands::window_menu,
        ])
        .build(tauri::generate_context!())
        .expect("error while building curator")
        .run(|app, event| {
            // ExitRequested fires before every window's Destroyed during ⌘Q; mark quitting so a
            // detached window's teardown doesn't reopen its origin mid-quit (see `redock`) and the
            // closing windows don't raise the home surface.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(set) = app.try_state::<WindowSet>() {
                    set.mark_quitting();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::menu::WindowEntry;

    fn origin_home(title: &str, entries: &[WindowEntry]) -> Option<(String, bool)> {
        entry_for_title(entries, title).map(|e| (e.id.clone(), e.open))
    }

    fn entry(id: &str, title: &str, open: bool) -> WindowEntry {
        WindowEntry {
            id: id.into(),
            title: title.into(),
            open,
            colour: None,
        }
    }

    /// A tab popped out of the window first labelled "work", since renamed in place to "job".
    fn out_of_renamed() -> HashMap<String, CuratorDetached> {
        HashMap::from([(
            "shell-detach:work:tab-1".to_string(),
            CuratorDetached {
                origin_wid: "work".into(),
                origin_title: "job".into(),
                tab_label: "work:tab-1".into(),
            },
        )])
    }

    #[test]
    fn a_renamed_and_closed_origin_is_found_by_its_title() {
        // The closed slot is relabelled from its new title: the tab returns to "job", under it.
        let mut out = out_of_renamed();
        let d = out.values_mut().next().unwrap();
        assert_eq!(
            origin_home(&d.origin_title, &[entry("job", "job", false)]),
            Some(("job".to_string(), false))
        );
        d.rehome("job");
        assert_eq!(
            (d.origin_wid.as_str(), d.tab_label.as_str()),
            ("job", "job:tab-1")
        );
        // Still open under its old label: that label.
        assert_eq!(
            origin_home(" job ", &[entry("work", "job", true)]),
            Some(("work".to_string(), true))
        );
    }

    #[test]
    fn a_closed_window_settles_the_pop_outs_by_origin_title() {
        // "job" (renamed from "work", tab popped out) closed: its slot is now labelled "job", so
        // the pop-out's bookkeeping moves off the freed "work" label a New Window may take.
        let mut out = out_of_renamed();
        out.insert(
            "shell-detach:gone:tab-2".into(),
            CuratorDetached {
                origin_wid: "gone".into(),
                origin_title: "gone".into(),
                tab_label: "gone:tab-2".into(),
            },
        );
        let orphans = settle_detached(&mut out, &[entry("job", "job", false)]);
        assert_eq!(
            orphans,
            ["shell-detach:gone:tab-2"],
            "its window left the config"
        );
        assert_eq!(out.len(), 1);
        let d = &out["shell-detach:work:tab-1"];
        assert_eq!(
            (d.origin_wid.as_str(), d.tab_label.as_str()),
            ("job", "job:tab-1")
        );
    }

    #[test]
    fn an_origin_reopened_first_keeps_its_tab_popped_out() {
        let mut out = out_of_renamed();
        assert_eq!(
            popped_out_of(&mut out, "job", "job"),
            HashSet::from(["job:tab-1".to_string()])
        );
        let d = out.values().next().unwrap();
        assert_eq!(d.origin_wid, "job", "the bookkeeping follows the window");
    }

    #[test]
    fn a_new_window_on_the_freed_label_is_not_the_origin() {
        let mut out = out_of_renamed();
        assert!(popped_out_of(&mut out, "work", "work").is_empty());
        assert_eq!(out.values().next().unwrap().origin_wid, "work");
        assert_eq!(origin_home("job", &[entry("work", "work", false)]), None);
    }

    #[test]
    fn a_rename_carries_into_the_popped_out_origin_titles() {
        let mut out = out_of_renamed();
        retitle_origins(&mut out, " job ", " gig ");
        retitle_origins(&mut out, "other", "x");
        assert_eq!(out.values().next().unwrap().origin_title, "gig");
    }

    #[test]
    fn rename_patches_last_cfg_title() {
        // What `window_retitled` does to `last_cfg` after the set renamed "Work" to "Job": the
        // window `open_window` looks up by its new title is found before the reload echo.
        let src = "[[window]]\ntitle = \"Work\"\n[[window.tab]]\ntitle = \"A\"\nurl = \"https://a.test/\"\n[[window]]\ntitle = \"Side\"\n";
        let mut cfg = curator_config::parse_and_validate(src).unwrap().0;
        curator_config::retitle_window(&mut cfg.windows, "Work", "Job", Some("#f00".into()));
        let w = cfg
            .windows
            .iter()
            .find(|w| w.title.trim() == "Job")
            .unwrap();
        assert_eq!(w.colour.as_deref(), Some("#f00"));
        assert!(cfg.windows.iter().all(|w| w.title != "Work"));
    }

    #[test]
    fn detach_token_round_trips_through_the_detached_label() {
        // A content-webview label is `{origin_wid}:tab-<hash>` — the token IS that label, and it
        // must survive the shell-core detached-label wrapping so a detached window is recognised
        // (is_detached_label) and its token recoverable (detach_token). Deterministic, too.
        let tab_label = "Comms:tab-00112233445566ff";
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

    fn view(label: &str, session: &str) -> curator_config::TabView {
        curator_config::TabView {
            label: label.to_string(),
            group: None,
            title: label.to_string(),
            url: format!("https://{label}.example"),
            load_on_open: false,
            reload_every: None,
            unread: Default::default(),
            session: session.to_string(),
        }
    }

    #[test]
    fn session_changed_flags_only_kept_labels_with_a_new_session() {
        let old = vec![
            view("a", "default"),
            view("b", "default"),
            view("gone", "x"),
        ];
        let new = vec![view("a", "work"), view("b", "default"), view("added", "y")];
        let changed = session_changed(&old, &new);
        assert_eq!(changed, HashSet::from(["a".to_string()]));
    }
}
