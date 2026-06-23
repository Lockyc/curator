/// A same-tab/main-frame navigation: allow it (return true = "home base, wander freely").
pub fn allow_same_tab_navigation(_url: &str) -> bool {
    true
}

/// Build the argv for handing a URL to the macOS default handler (the user's default browser).
pub fn open_command(url: &str) -> (&'static str, Vec<String>) {
    ("open", vec![url.to_string()])
}

/// Hand a URL to the macOS default handler (the user's default browser). Side-effecting; not unit-tested.
pub fn escape_to_default_browser(url: &str) {
    let (cmd, args) = open_command(url);
    let _ = std::process::Command::new(cmd).args(args).spawn();
}

/// Schemes curator will hand to the macOS opener for an in-app `window.open` / new-window
/// intent. http/https are normal links; mailto/tel are the everyday "open in my mail/phone
/// app" hand-offs. Everything else (file://, custom app schemes, smb://, …) is refused so a
/// page can't drive `open` into launching an arbitrary handler.
pub fn is_escapable_scheme(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "mailto" | "tel")
}

/// Naive registrable domain: the last two dot-labels of a host (`accounts.google.com` →
/// `google.com`). Not public-suffix-aware (treats `foo.co.uk` as `co.uk`), which is fine for
/// the everyday provider domains this is used to recognise.
fn registrable(host: &str) -> String {
    let mut labels: Vec<&str> = host.rsplitn(3, '.').collect();
    labels.truncate(2);
    labels.reverse();
    labels.join(".")
}

/// Whether a new-window `target` belongs to the same site as the tab's `home_url` — i.e. it's
/// the app's own flow (a sign-in popup goes to the provider's domain) rather than an external
/// link. Same-site new windows are kept in-app so they complete in the tab's own login session;
/// cross-site ones escape to the default browser. http(s) only.
pub fn same_site(home_url: &str, target: &url::Url) -> bool {
    if !matches!(target.scheme(), "http" | "https") {
        return false;
    }
    let (Some(t_host), Ok(home)) = (target.host_str(), url::Url::parse(home_url)) else {
        return false;
    };
    match home.host_str() {
        Some(h_host) => registrable(h_host) == registrable(t_host),
        None => false,
    }
}

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

/// Sentinel host the injected click-interceptor navigates to so the native
/// `on_navigation` handler can escape cmd/middle-clicks (which WKWebView does not route
/// through `on_new_window`). Must be a host no real keeper site will ever use.
pub const SENTINEL_HOST: &str = "curator.escape.invalid";

/// If `nav_url` is the escape sentinel, return the real URL to hand off (its decoded `u`
/// query param, restricted to http/https). Any other navigation — or a sentinel with a
/// missing/garbage target — yields `None` (don't escape junk; let it navigate normally).
pub fn sentinel_target(nav_url: &url::Url) -> Option<String> {
    if nav_url.host_str() != Some(SENTINEL_HOST) {
        return None;
    }
    let target = nav_url
        .query_pairs()
        .find(|(k, _)| k == "u")
        .map(|(_, v)| v.into_owned())?;
    match url::Url::parse(&target).ok()?.scheme() {
        "http" | "https" => Some(target),
        _ => None,
    }
}

/// True if `url`'s host is one of curator's internal sentinel hosts (notify / badge / escape).
pub fn is_sentinel_host(url: &url::Url) -> bool {
    matches!(
        url.host_str(),
        Some(NOTIFY_SENTINEL_HOST | BADGE_SENTINEL_HOST | SENTINEL_HOST)
    )
}

/// Whether a sentinel `url` carries this webview's secret `expected` key (the `k` param). Our
/// injected shims bake the key in as a function-local literal — never on `window` — so a page
/// can't read it and therefore can't forge a sentinel navigation by hitting the host directly.
pub fn sentinel_key_ok(url: &url::Url, expected: &str) -> bool {
    !expected.is_empty() && url.query_pairs().any(|(k, v)| k == "k" && v == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_tab_navigation_is_always_allowed() {
        assert!(allow_same_tab_navigation(
            "https://mail.google.com/mail/u/0/#inbox"
        ));
        assert!(allow_same_tab_navigation("https://example.com/whatever"));
    }

    #[test]
    fn open_command_shells_to_macos_open() {
        let (cmd, args) = open_command("https://example.com/");
        assert_eq!(cmd, "open");
        assert_eq!(args, vec!["https://example.com/".to_string()]);
    }

    fn url(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn sentinel_extracts_encoded_target() {
        let u = url("https://curator.escape.invalid/?u=https%3A%2F%2Fexample.org%2Fa%3Fb%3Dc");
        assert_eq!(
            sentinel_target(&u),
            Some("https://example.org/a?b=c".to_string())
        );
    }

    #[test]
    fn non_sentinel_url_is_none() {
        let u = url("https://mail.google.com/mail/u/0/#inbox");
        assert_eq!(sentinel_target(&u), None);
    }

    #[test]
    fn sentinel_without_u_is_none() {
        let u = url("https://curator.escape.invalid/");
        assert_eq!(sentinel_target(&u), None);
    }

    #[test]
    fn sentinel_with_non_http_target_is_none() {
        let u = url("https://curator.escape.invalid/?u=file%3A%2F%2F%2Fetc%2Fpasswd");
        assert_eq!(sentinel_target(&u), None);
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
    fn same_site_keeps_provider_auth_in_app() {
        // Google Chat opening its accounts domain is same registrable domain → in-app.
        assert!(same_site(
            "https://chat.google.com/",
            &url("https://accounts.google.com/o/oauth2/auth?foo=bar")
        ));
        // Exact-host popup is same-site too.
        assert!(same_site(
            "https://app.element.io/",
            &url("https://app.element.io/#/login")
        ));
    }

    #[test]
    fn same_site_escapes_external_links() {
        // A genuinely external link is cross-site → not kept in-app.
        assert!(!same_site(
            "https://chat.google.com/",
            &url("https://example.com/article")
        ));
        // Non-web schemes are never "same site".
        assert!(!same_site(
            "https://chat.google.com/",
            &url("mailto:a@b.test")
        ));
    }

    #[test]
    fn escapable_schemes_are_web_and_contact() {
        for s in [
            "https://x.test/",
            "http://x.test/",
            "mailto:a@b.test",
            "tel:+15551234",
        ] {
            assert!(is_escapable_scheme(&url(s)), "{s} should escape");
        }
    }

    #[test]
    fn non_escapable_schemes_are_refused() {
        for s in [
            "file:///etc/passwd",
            "smb://server/share",
            "ftp://x.test/f",
            "customapp://do-something",
        ] {
            assert!(!is_escapable_scheme(&url(s)), "{s} should be refused");
        }
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
    fn sentinel_hosts_are_recognised() {
        assert!(is_sentinel_host(&url(
            "https://curator.notify.invalid/?t=x"
        )));
        assert!(is_sentinel_host(&url("https://curator.badge.invalid/?n=1")));
        assert!(is_sentinel_host(&url(
            "https://curator.escape.invalid/?u=x"
        )));
        assert!(!is_sentinel_host(&url("https://mail.google.com/")));
    }

    #[test]
    fn sentinel_key_must_match() {
        let u = url("https://curator.notify.invalid/?t=x&k=abc123");
        assert!(sentinel_key_ok(&u, "abc123"));
        assert!(!sentinel_key_ok(&u, "wrong"));
        // A missing key never matches (a forged sentinel with no key is rejected).
        assert!(!sentinel_key_ok(
            &url("https://curator.notify.invalid/?t=x"),
            "abc123"
        ));
        // An empty expected key never matches, even against a keyless URL.
        assert!(!sentinel_key_ok(&u, ""));
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
