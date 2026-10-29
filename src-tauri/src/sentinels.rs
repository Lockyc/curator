//! Curator's own sentinel navigations: the notify and badge shims navigate to dead `.invalid`
//! hosts, which shell-core verifies against the per-webview key and hands to the app hook.

/// Sentinel host the injected Notification override navigates to so the native
/// `on_navigation` handler can fire a real banner. Distinct from the cmd-click escape host.
pub const NOTIFY_SENTINEL_HOST: &str = "curator.notify.invalid";

/// Title + body parsed out of a notify-sentinel navigation.
pub struct NotifyPayload {
    pub title: String,
    pub body: String,
}

/// If `nav_url` is a notify sentinel, return its `t` (title) and `b` (body) query params
/// (each defaulting to empty). Any other URL → `None`.
pub fn notify_sentinel(nav_url: &url::Url) -> Option<NotifyPayload> {
    if nav_url.host_str() != Some(NOTIFY_SENTINEL_HOST) {
        return None;
    }
    let get = |key: &str| {
        nav_url
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    };
    Some(NotifyPayload {
        title: get("t"),
        body: get("b"),
    })
}

/// Sentinel host the injected Badging-API shim navigates to so the native `on_navigation`
/// handler can update a service's unread badge. Distinct from the notify and escape hosts.
pub const BADGE_SENTINEL_HOST: &str = "curator.badge.invalid";

/// An unread signal decoded from a badge sentinel: an explicit count (0 = clear) or a
/// countless dot (`setAppBadge()` called with no argument).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeSignal {
    Count(u32),
    Dot,
}

/// If `nav_url` is a badge sentinel, decode its signal: a `dot` param → `Dot`; otherwise the
/// `n` param parsed as a count. A malformed/missing `n` is treated as `Count(0)` (clear) so
/// the dead sentinel host is always consumed rather than navigated to. Any other host → None.
pub fn badge_sentinel(nav_url: &url::Url) -> Option<BadgeSignal> {
    if nav_url.host_str() != Some(BADGE_SENTINEL_HOST) {
        return None;
    }
    let get = |key: &str| {
        nav_url
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    if get("dot").is_some() {
        return Some(BadgeSignal::Dot);
    }
    let n = get("n").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    Some(BadgeSignal::Count(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn notify_sentinel_extracts_title_and_body() {
        let u = url("https://curator.notify.invalid/?t=New%20message&b=hello%20there");
        let p = notify_sentinel(&u).unwrap();
        assert_eq!(p.title, "New message");
        assert_eq!(p.body, "hello there");
    }

    #[test]
    fn notify_sentinel_defaults_missing_parts_to_empty() {
        let u = url("https://curator.notify.invalid/?t=Title");
        let p = notify_sentinel(&u).unwrap();
        assert_eq!(p.title, "Title");
        assert_eq!(p.body, "");
    }

    #[test]
    fn non_notify_host_is_none() {
        let u = url("https://curator.escape.invalid/?u=https%3A%2F%2Fx.test%2F");
        assert!(notify_sentinel(&u).is_none());
    }

    #[test]
    fn badge_sentinel_reads_count() {
        let u = url("https://curator.badge.invalid/?n=5");
        assert_eq!(badge_sentinel(&u), Some(BadgeSignal::Count(5)));
    }

    #[test]
    fn badge_sentinel_zero_is_count_zero() {
        let u = url("https://curator.badge.invalid/?n=0");
        assert_eq!(badge_sentinel(&u), Some(BadgeSignal::Count(0)));
    }

    #[test]
    fn badge_sentinel_dot_is_dot() {
        let u = url("https://curator.badge.invalid/?dot=1");
        assert_eq!(badge_sentinel(&u), Some(BadgeSignal::Dot));
    }

    #[test]
    fn badge_sentinel_malformed_or_missing_count_clears() {
        // Garbage or absent params still consume the dead sentinel host (treated as clear),
        // so the page never actually navigates to it.
        assert_eq!(
            badge_sentinel(&url("https://curator.badge.invalid/?n=abc")),
            Some(BadgeSignal::Count(0))
        );
        assert_eq!(
            badge_sentinel(&url("https://curator.badge.invalid/")),
            Some(BadgeSignal::Count(0))
        );
    }

    #[test]
    fn keyed_urls_parse_as_shell_core_passes_them() {
        let n = url("https://curator.notify.invalid/?t=Hi&b=there&k=abc123");
        let p = notify_sentinel(&n).unwrap();
        assert_eq!((p.title.as_str(), p.body.as_str()), ("Hi", "there"));
        assert_eq!(
            badge_sentinel(&url("https://curator.badge.invalid/?n=3&k=abc123")),
            Some(BadgeSignal::Count(3))
        );
        assert_eq!(
            badge_sentinel(&url("https://curator.badge.invalid/?dot=1&k=abc123")),
            Some(BadgeSignal::Dot)
        );
    }

    #[test]
    fn badge_sentinel_other_host_is_none() {
        assert_eq!(
            badge_sentinel(&url("https://curator.notify.invalid/?t=x")),
            None
        );
        assert_eq!(badge_sentinel(&url("https://mail.google.com/")), None);
    }
}
