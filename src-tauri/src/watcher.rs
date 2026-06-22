use crate::config::{parse_and_validate, WindowConfig};

/// Given the current (last-good) first-window config and new file contents, return either the
/// new first-window config or an error message, without ever discarding the last-good on failure.
pub fn reconcile(_current: &WindowConfig, new_src: &str) -> Result<WindowConfig, String> {
    match parse_and_validate(new_src) {
        Ok(cfg) => cfg
            .windows
            .into_iter()
            .next()
            .ok_or_else(|| "no windows in new config".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_win_cfg() -> WindowConfig {
        parse_and_validate(
            "[[window]]\ntitle = \"W\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n"
        )
        .unwrap()
        .windows
        .into_iter()
        .next()
        .unwrap()
    }

    #[test]
    fn valid_new_source_replaces() {
        let cur = make_win_cfg();
        let src = "[[window]]\ntitle = \"W2\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n";
        let res = reconcile(&cur, src).unwrap();
        assert_eq!(res.groups.len(), 1);
    }

    #[test]
    fn invalid_new_source_yields_error_message() {
        let cur = make_win_cfg();
        let err = reconcile(&cur, "= = bad").unwrap_err();
        assert!(err.contains("invalid TOML"));
    }
}
