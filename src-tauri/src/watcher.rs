use crate::config::{parse_and_validate, Config};
use crate::identity::window_id;

/// Parse `src`; on success return the new whole config, on failure return a message. The
/// caller keeps its last-good `Config` on error (the last-good-on-failure contract).
pub fn reconcile(new_src: &str) -> Result<Config, String> {
    parse_and_validate(new_src).map_err(|e| e.to_string())
}

/// Window-id sets that appeared, disappeared, or persisted across a config reload.
pub struct WindowDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub kept: Vec<String>,
}

/// Compare window id sets between two configs (ids derived from window titles).
pub fn diff_windows(old: &Config, new: &Config) -> WindowDiff {
    let old_ids: std::collections::HashSet<String> =
        old.windows.iter().map(|w| window_id(&w.title)).collect();
    let new_ids: std::collections::HashSet<String> =
        new.windows.iter().map(|w| window_id(&w.title)).collect();
    WindowDiff {
        added: new_ids.difference(&old_ids).cloned().collect(),
        removed: old_ids.difference(&new_ids).cloned().collect(),
        kept: old_ids.intersection(&new_ids).cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_and_validate;

    fn cfg(titles: &[&str]) -> crate::config::Config {
        let mut s = String::new();
        for t in titles {
            s.push_str(&format!("[[window]]\ntitle = \"{t}\"\n[[window.group]]\nname=\"G\"\n[[window.group.tab]]\ntitle=\"T\"\nurl=\"https://x.test/\"\n"));
        }
        parse_and_validate(&s).unwrap()
    }

    #[test]
    fn diff_detects_added_removed_kept() {
        let old = cfg(&["A", "B"]);
        let new = cfg(&["B", "C"]);
        let d = diff_windows(&old, &new);
        let id = crate::identity::window_id;
        assert_eq!(d.removed, vec![id("A")]);
        assert!(d.added.contains(&id("C")));
        assert!(d.kept.contains(&id("B")));
    }

    #[test]
    fn valid_new_source_replaces() {
        let src = "[[window]]\ntitle = \"W2\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n";
        let res = reconcile(src).unwrap();
        assert_eq!(res.windows.len(), 1);
        assert_eq!(res.windows[0].title, "W2");
    }

    #[test]
    fn invalid_new_source_yields_error_message() {
        let err = reconcile("= = bad").unwrap_err();
        assert!(err.contains("invalid TOML"));
    }
}
