//! Unread awareness: parse a service's `<title>` into an unread state, drive the sidebar
//! badge and the aggregate macOS dock badge.

use crate::escape::BadgeSignal;
use crate::AppState;
use curator_config::UnreadMode;
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

/// The unread state to *display* for a tab: its source-derived state, upgraded to `Activity`
/// while a notification-derived dot is pending. The dot is the third and weakest source — it
/// only fills in for `None`, so a Badging count or a title count always wins. It exists for
/// services that report unread through neither (Google Chat: no `setAppBadge`, and a title that
/// merely flashes "X messaged you - Chat" with no count), where a delivered banner is the only
/// signal curator ever gets. It shows in the sidebar alone: [`dock_count`] sums numeric states,
/// so an `Activity` dot contributes nothing to the dock badge.
pub fn displayed(unread: Unread, notified: bool) -> Unread {
    match unread {
        Unread::None if notified => Unread::Activity,
        u => u,
    }
}

/// Narrow a source-derived unread state to what the tab's [`UnreadMode`] admits: `off` badges
/// nothing, `count` drops a countless `Activity` (the marker a service leaves permanently on —
/// Discord's `"• Discord"` title means "some channel is unread", which for a busy account is
/// always true), and `all` passes everything through. A real count is never dropped.
pub fn admitted(mode: UnreadMode, unread: Unread) -> Unread {
    if !mode.allows_any() {
        return Unread::None;
    }
    match unread {
        Unread::Activity if !mode.allows_countless() => Unread::None,
        u => u,
    }
}

/// Whether a Badging signal makes the service authoritative — i.e. silences its title heuristic.
/// A signal the tab's mode discards must not: a service that reports a countless dot *and* a
/// title count would otherwise go authoritative on the dot, have it dropped by `count` mode, and
/// end up badging nothing at all.
pub fn badge_is_authoritative(mode: UnreadMode, signal: BadgeSignal) -> bool {
    match mode {
        UnreadMode::Off => false,
        UnreadMode::Count => matches!(signal, BadgeSignal::Count(_)),
        UnreadMode::All => true,
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
    let shown = {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(window_id) else {
            return;
        };
        // Ignore a late event for a tab that's been unloaded or orphaned (its webview is on
        // its way out) — otherwise a stale unread could re-appear and linger on the dock badge.
        if !rt.tabs.is_created(&label) {
            return;
        }
        // The one place the tab's `unread` mode narrows a service-reported state — both the title
        // and Badging sources land here, so neither can drift from the other.
        let unread = admitted(rt.unread_mode(&label), unread);
        rt.unread.insert(label.clone(), unread);
        displayed(unread, rt.notified.contains(&label))
    };
    emit_badge(app, &state, window_id, label, shown);
}

/// Push one tab's badge state out: the per-window sidebar pill plus the single aggregate dock
/// badge. The one place either is written, so every source (title, Badging, notification dot)
/// renders identically. Must be called with the `windows` lock released.
fn emit_badge(
    app: &tauri::AppHandle,
    state: &AppState,
    window_id: &str,
    label: String,
    shown: Unread,
) {
    // Per-window sidebar update → that window's chrome only. The chrome is the window's main
    // webview, so its label is the window id.
    let _ = app.emit_to(
        window_id,
        "service-badge",
        BadgeEvent {
            label,
            text: badge_text(shown),
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
            rt.notified.remove(label);
        }
    }
    let _ = app.emit_to(
        window_id,
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
            if badge_is_authoritative(rt.unread_mode(label), signal) {
                rt.badge_authoritative.insert(label.to_string());
            }
        }
    }
    apply_unread(app, &window_id, label.to_string(), badge_unread(signal));
}

/// Notify-sentinel handler: raise a notification-derived activity dot on the tab that fired the
/// banner (see [`displayed`] for why this source exists). Skipped for the tab the user is already
/// looking at, for a Badging-authoritative service (its own count beats a dot), and for a tab set
/// to `unread = "off"`. `unread = "count"` deliberately does *not* suppress it: a delivered banner
/// is evidence of a real event, not a marker read off a title.
pub fn on_notification(app: &tauri::AppHandle, window_id: &str, label: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let shown = {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(window_id) else {
            return;
        };
        if !rt.tabs.is_created(label)
            || rt.badge_authoritative.contains(label)
            || rt.tabs.active() == Some(label)
            || !rt.unread_mode(label).allows_any()
        {
            return;
        }
        rt.notified.insert(label.to_string());
        displayed(rt.unread.get(label).copied().unwrap_or(Unread::None), true)
    };
    emit_badge(app, &state, window_id, label.to_string(), shown);
}

/// Clear a tab's notification-derived dot — called when the user selects the tab. Selecting is the
/// *only* thing that clears it: a title count is retracted by the service itself once the message
/// is read, but nothing in the page ever tells curator a banner was seen.
pub fn mark_read(app: &tauri::AppHandle, window_id: &str, label: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let shown = {
        let mut windows = state.windows.lock().unwrap();
        let Some(rt) = windows.get_mut(window_id) else {
            return;
        };
        if !rt.notified.remove(label) {
            return; // no dot pending — leave the title/Badging state alone
        }
        displayed(rt.unread.get(label).copied().unwrap_or(Unread::None), false)
    };
    emit_badge(app, &state, window_id, label.to_string(), shown);
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
    fn notification_dot_only_fills_in_for_none() {
        // The dot is the weakest source: it never overrides a real count or an existing dot.
        assert_eq!(displayed(Unread::None, true), Unread::Activity);
        assert_eq!(displayed(Unread::None, false), Unread::None);
        assert_eq!(displayed(Unread::Count(3), true), Unread::Count(3));
        assert_eq!(displayed(Unread::Activity, true), Unread::Activity);
    }

    #[test]
    fn notification_dot_survives_a_countless_title() {
        // Google Chat flashes "Lachlan Collins messaged you - Chat" ↔ "Chat"; both parse to
        // None, so the dot has to outlive a title update or it would blink out a second later.
        let dot = true;
        assert_eq!(
            displayed(parse_unread("Lachlan Collins messaged you - Chat"), dot),
            Unread::Activity
        );
        assert_eq!(displayed(parse_unread("Chat"), dot), Unread::Activity);
    }

    #[test]
    fn count_mode_drops_a_countless_marker_but_never_a_count() {
        // Discord's "• Discord" (any channel unread) parses to Activity and is permanently on for
        // a busy account; its "(N) Discord" (a mention) is the signal worth surfacing.
        assert_eq!(
            admitted(UnreadMode::Count, parse_unread("• Discord")),
            Unread::None
        );
        assert_eq!(
            admitted(UnreadMode::Count, parse_unread("(4) Discord")),
            Unread::Count(4)
        );
        assert_eq!(
            admitted(UnreadMode::Count, badge_unread(BadgeSignal::Dot)),
            Unread::None
        );
        assert_eq!(
            admitted(UnreadMode::Count, badge_unread(BadgeSignal::Count(2))),
            Unread::Count(2)
        );
    }

    #[test]
    fn all_mode_is_the_pre_existing_behaviour() {
        assert_eq!(
            admitted(UnreadMode::All, parse_unread("• Discord")),
            Unread::Activity
        );
        assert_eq!(
            admitted(UnreadMode::All, parse_unread("(4) Discord")),
            Unread::Count(4)
        );
    }

    #[test]
    fn off_mode_drops_every_state() {
        assert_eq!(admitted(UnreadMode::Off, Unread::Activity), Unread::None);
        assert_eq!(admitted(UnreadMode::Off, Unread::Count(9)), Unread::None);
        assert_eq!(admitted(UnreadMode::Off, Unread::None), Unread::None);
    }

    #[test]
    fn badging_is_authoritative_only_for_signals_the_mode_admits() {
        // Going authoritative on a signal the mode then discards would silence the title too,
        // leaving a service that reports both ways with no badge at all.
        assert!(badge_is_authoritative(UnreadMode::All, BadgeSignal::Dot));
        assert!(!badge_is_authoritative(UnreadMode::Count, BadgeSignal::Dot));
        assert!(badge_is_authoritative(
            UnreadMode::Count,
            BadgeSignal::Count(3)
        ));
        assert!(badge_is_authoritative(
            UnreadMode::Count,
            BadgeSignal::Count(0)
        ));
        assert!(!badge_is_authoritative(
            UnreadMode::Off,
            BadgeSignal::Count(3)
        ));
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
