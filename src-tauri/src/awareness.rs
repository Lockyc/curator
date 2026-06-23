//! Unread awareness: parse a service's `<title>` into an unread state, drive the sidebar
//! badge and the aggregate macOS dock badge.

use crate::escape::BadgeSignal;
use crate::{identity, AppState};
use serde::Serialize;
use tauri::{Emitter, Manager};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unread {
    None,
    Activity,
    Count(u32),
}

/// Parse an unread state from a window/tab title. Generic v1 heuristic: a `(N)` or `[N]` group
/// → a count (0 → None); else a leading activity bullet → Activity; else None. Element uses
/// `[N]` (e.g. "Element [3] | Castle"); other apps tend to use `(N)`. Per-service parser
/// overrides are a later refinement.
pub fn parse_unread(title: &str) -> Unread {
    if let Some(n) = bracket_count(title, '(', ')').or_else(|| bracket_count(title, '[', ']')) {
        return if n > 0 {
            Unread::Count(n)
        } else {
            Unread::None
        };
    }
    let t = title.trim_start();
    if t.starts_with('•') || t.starts_with('●') || t.starts_with('🔴') {
        return Unread::Activity;
    }
    Unread::None
}

/// Map a Badging-API signal to an unread state: a zero count clears (`None`), a positive
/// count is exact, a countless dot is unknown-count activity.
pub fn badge_unread(signal: BadgeSignal) -> Unread {
    match signal {
        BadgeSignal::Count(0) => Unread::None,
        BadgeSignal::Count(n) => Unread::Count(n),
        BadgeSignal::Dot => Unread::Activity,
    }
}

/// A title-derived update is honoured only for a service that has never sent an authoritative
/// Badging-API signal. Once an app reports its own count, its title is ignored (it may carry
/// a stale or differently-formatted count).
pub fn title_update_allowed(
    authoritative: &std::collections::HashSet<String>,
    label: &str,
) -> bool {
    !authoritative.contains(label)
}

fn bracket_count(s: &str, open_ch: char, close_ch: char) -> Option<u32> {
    // Parses the *first* `open…close` group only. A title like "(draft) (3)" would read
    // "draft" and miss the count — acceptable for the v1 generic heuristic (per-service
    // parsers later).
    let open = s.find(open_ch)?;
    let close = s[open..].find(close_ch)? + open;
    s[open + 1..close].trim().parse::<u32>().ok()
}

/// Text for the sidebar pill: empty (hidden), a bullet for unknown-count activity, or the
/// number.
pub fn badge_text(u: Unread) -> String {
    match u {
        Unread::None => String::new(),
        Unread::Activity => "•".to_string(),
        Unread::Count(n) => n.to_string(),
    }
}

/// Aggregate dock-badge number: sum of numeric counts; `None` (clear the badge) when zero.
pub fn dock_count(states: &[Unread]) -> Option<i64> {
    let total: i64 = states
        .iter()
        .filter_map(|u| match u {
            Unread::Count(n) => Some(*n as i64),
            _ => None,
        })
        .sum();
    (total > 0).then_some(total)
}

#[derive(Clone, Serialize)]
struct BadgeEvent {
    label: String,
    text: String,
}

/// Update `label`'s unread state within its window, push the per-window `service-badge`, and
/// refresh the single aggregate dock badge from every window's counts.
fn apply_unread(app: &tauri::AppHandle, window_id: &str, label: String, unread: Unread) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(window_id) else {
            return;
        };
        // Ignore a late event for a tab that's been unloaded or orphaned (its webview is on
        // its way out) — otherwise a stale unread could re-appear and linger on the dock badge.
        if !rt.tabs.is_created(&label) {
            return;
        }
        rt.unread.insert(label.clone(), unread);
    }
    // Per-window sidebar update → that window's chrome only.
    let chrome = identity::namespaced(window_id, "chrome");
    let _ = app.emit_to(
        chrome,
        "service-badge",
        BadgeEvent {
            label,
            text: badge_text(unread),
        },
    );
    // Single dock badge across all windows.
    let total = dock_count(&state.all_unread());
    if let Some(win) = app.get_window(window_id) {
        let _ = win.set_badge_count(total);
    }
}

/// Forget a tab's unread state entirely — used when its content webview is destroyed by an
/// explicit unload. Unlike [`apply_unread`] (which inserts a state and requires the tab to still
/// be created), this *removes* the entry, clears the sidebar pill, drops its dock contribution,
/// and lets its title drive the badge again if it's ever reloaded. Without this, an unloaded
/// tab's count would stay stranded on the dock badge (the gone webview can never send a clear).
pub fn forget_tab(app: &tauri::AppHandle, window_id: &str, label: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    {
        let mut windows = state.windows.lock().unwrap();
        if let Some(rt) = windows.get_mut(window_id) {
            rt.unread.remove(label);
            rt.badge_authoritative.remove(label);
        }
    }
    let chrome = identity::namespaced(window_id, "chrome");
    let _ = app.emit_to(
        chrome,
        "service-badge",
        BadgeEvent {
            label: label.to_string(),
            text: String::new(),
        },
    );
    let total = dock_count(&state.all_unread());
    if let Some(win) = app.get_window(window_id) {
        let _ = win.set_badge_count(total);
    }
}

/// Title-change handler: drive the badge from the title heuristic unless the service has gone
/// Badging-authoritative for its window.
pub fn on_title_changed(webview: &tauri::Webview, title: &str) {
    let app = webview.app_handle();
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let label = webview.label().to_string();
    let window_id = webview.window().label().to_string();
    {
        let windows = state.windows.lock().unwrap();
        if let Some(rt) = windows.get(&window_id) {
            if !title_update_allowed(&rt.badge_authoritative, &label) {
                return;
            }
        }
    }
    apply_unread(app, &window_id, label, parse_unread(title));
}

/// Badging-API sentinel handler: mark the service authoritative for its window and apply.
pub fn on_badge_signal(app: &tauri::AppHandle, label: &str, signal: BadgeSignal) {
    // The label is window-namespaced (`<wid>:tab-…`); recover the window id from its prefix.
    let window_id = label
        .split_once(':')
        .map(|(w, _)| w.to_string())
        .unwrap_or_default();
    if let Some(state) = app.try_state::<AppState>() {
        let mut windows = state.windows.lock().unwrap();
        if let Some(rt) = windows.get_mut(&window_id) {
            rt.badge_authoritative.insert(label.to_string());
        }
    }
    apply_unread(app, &window_id, label.to_string(), badge_unread(signal));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_unread_for_plain_title() {
        assert_eq!(parse_unread("Element"), Unread::None);
        assert_eq!(parse_unread(""), Unread::None);
    }

    #[test]
    fn paren_count_anywhere() {
        assert_eq!(parse_unread("(3) Element"), Unread::Count(3));
        assert_eq!(parse_unread("Element (5)"), Unread::Count(5));
    }

    #[test]
    fn bracket_count_anywhere() {
        // Element's real title format: "Element [N] | RoomName".
        assert_eq!(parse_unread("Element [1] | Castle"), Unread::Count(1));
        assert_eq!(parse_unread("Element [12] | Room"), Unread::Count(12));
        assert_eq!(parse_unread("Element"), Unread::None);
    }

    #[test]
    fn zero_paren_count_is_none() {
        assert_eq!(parse_unread("(0) Element"), Unread::None);
    }

    #[test]
    fn unrelated_parens_are_not_counts() {
        assert_eq!(parse_unread("Doc (draft)"), Unread::None);
    }

    #[test]
    fn leading_bullet_is_activity() {
        assert_eq!(parse_unread("• Slack"), Unread::Activity);
        assert_eq!(parse_unread("●WhatsApp"), Unread::Activity);
    }

    #[test]
    fn badge_text_renders_each_state() {
        assert_eq!(badge_text(Unread::None), "");
        assert_eq!(badge_text(Unread::Activity), "•");
        assert_eq!(badge_text(Unread::Count(7)), "7");
    }

    #[test]
    fn dock_count_sums_numeric_only() {
        assert_eq!(
            dock_count(&[
                Unread::Count(3),
                Unread::Activity,
                Unread::None,
                Unread::Count(2)
            ]),
            Some(5)
        );
        assert_eq!(dock_count(&[Unread::Activity, Unread::None]), None);
        assert_eq!(dock_count(&[]), None);
    }

    use crate::escape::BadgeSignal;

    #[test]
    fn badge_unread_maps_signal_to_state() {
        assert_eq!(badge_unread(BadgeSignal::Count(0)), Unread::None);
        assert_eq!(badge_unread(BadgeSignal::Count(4)), Unread::Count(4));
        assert_eq!(badge_unread(BadgeSignal::Dot), Unread::Activity);
    }

    #[test]
    fn title_updates_blocked_only_for_authoritative_labels() {
        use std::collections::HashSet;
        let mut auth = HashSet::new();
        assert!(title_update_allowed(&auth, "svc-a")); // none authoritative yet
        auth.insert("svc-a".to_string());
        assert!(!title_update_allowed(&auth, "svc-a")); // now Badging owns svc-a
        assert!(title_update_allowed(&auth, "svc-b")); // svc-b still title-driven
    }
}
