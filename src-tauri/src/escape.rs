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
}
