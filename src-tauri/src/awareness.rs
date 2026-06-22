//! Unread awareness: parse a service's `<title>` into an unread state, drive the sidebar
//! badge and the aggregate macOS dock badge.

use crate::escape::BadgeSignal;

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
