//! Per-window identity. The window id seeds namespaced webview labels (Tauri labels are
//! app-global, so two windows sharing a URL would otherwise collide) and per-(window,tab)
//! session stores. Derived from the window title via frozen FNV-1a so it's stable across runs.

use crate::hash::fnv1a_64;

/// Stable, label-safe window id from the window title. `:` is a legal Tauri label char, so
/// `window_id:within` composites are valid labels.
pub fn window_id(title: &str) -> String {
    format!("w{:016x}", fnv1a_64(title.as_bytes()))
}

/// Namespace a within-window label (e.g. `chrome`, `tab-<hash>`) under a window id.
pub fn namespaced(window_id: &str, within: &str) -> String {
    format!("{window_id}:{within}")
}

/// Seed for a per-(window,tab) WebKit data store: the same URL in two windows yields two
/// stores (two logins / profiles).
#[allow(dead_code)]
pub fn session_seed(window_id: &str, url: &str) -> String {
    format!("{window_id}:{url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_id_is_stable_and_label_safe() {
        assert_eq!(window_id("Comms"), window_id("Comms"));
        let id = window_id("Comms");
        assert!(id.starts_with('w'));
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':' || c == '_'));
    }

    #[test]
    fn distinct_titles_give_distinct_ids() {
        assert_ne!(window_id("Comms"), window_id("Keepers"));
    }

    #[test]
    fn same_url_in_two_windows_gets_distinct_session_seeds() {
        let a = session_seed(&window_id("Comms"), "https://mail.google.com/");
        let b = session_seed(&window_id("Keepers"), "https://mail.google.com/");
        assert_ne!(a, b);
    }

    #[test]
    fn namespacing_composites_with_colon() {
        assert_eq!(namespaced("wabc", "chrome"), "wabc:chrome");
    }
}
