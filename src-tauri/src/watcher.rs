use crate::config::{parse_and_validate, Config};

/// Given the current (last-good) config and new file contents, return either the new config
/// or an error message, without ever discarding the last-good on failure.
pub fn reconcile(current: &Config, new_src: &str) -> Result<Config, String> {
    match parse_and_validate(new_src) {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            let _ = current; // last-good retained by caller; we only surface the error
            Err(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_new_source_replaces() {
        let cur = Config { groups: vec![] };
        let src =
            "[[group]]\nname = \"G\"\n[[group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n";
        let res = reconcile(&cur, src).unwrap();
        assert_eq!(res.groups.len(), 1);
    }

    #[test]
    fn invalid_new_source_yields_error_message() {
        let cur = Config { groups: vec![] };
        let err = reconcile(&cur, "= = bad").unwrap_err();
        assert!(err.contains("invalid TOML"));
    }
}
