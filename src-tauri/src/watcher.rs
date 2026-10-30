use curator_config::{parse_and_validate, Config, Warning};

/// Parse `src`; on success return the new whole config plus any non-fatal warnings, on failure
/// return a message. The caller keeps its last-good `Config` on error (the last-good-on-failure
/// contract).
pub fn reconcile(new_src: &str) -> Result<(Config, Vec<Warning>), String> {
    parse_and_validate(new_src).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_new_source_replaces() {
        let src = "[[window]]\ntitle = \"W2\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n";
        let (res, _warnings) = reconcile(src).unwrap();
        assert_eq!(res.windows.len(), 1);
        assert_eq!(res.windows[0].title, "W2");
    }

    #[test]
    fn invalid_new_source_yields_error_message() {
        let err = reconcile("= = bad").unwrap_err();
        assert!(err.contains("invalid TOML"));
    }
}
