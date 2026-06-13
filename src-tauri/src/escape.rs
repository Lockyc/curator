/// A same-tab/main-frame navigation: allow it (return true = "home base, wander freely").
pub fn allow_same_tab_navigation(_url: &str) -> bool {
    true
}

/// Build the argv for handing a URL to the macOS default handler (→ Velja).
pub fn open_command(url: &str) -> (&'static str, Vec<String>) {
    ("open", vec![url.to_string()])
}

/// Hand a URL to the macOS default handler (→ Velja). Side-effecting; not unit-tested.
pub fn escape_to_default_browser(url: &str) {
    let (cmd, args) = open_command(url);
    let _ = std::process::Command::new(cmd).args(args).spawn();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_tab_navigation_is_always_allowed() {
        assert!(allow_same_tab_navigation("https://mail.google.com/mail/u/0/#inbox"));
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
}
