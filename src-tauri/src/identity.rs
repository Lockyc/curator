//! Window label identity. The window id namespaces a window's webview labels (Tauri labels are
//! app-global, so two windows sharing a URL would otherwise collide). It's a purely mechanical,
//! run-ephemeral label key — nothing persistent (logins live in `session`-keyed data stores) is
//! tied to it, so renaming a window is harmless. Derived from the title via frozen FNV-1a.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_id_is_stable_and_label_safe() {
        assert_eq!(window_id("Comms"), window_id("Comms"));
        let id = window_id("Comms");
        assert_eq!(id.len(), 17);
        assert_eq!(&id[..1], "w");
        assert!(id[1..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn distinct_titles_give_distinct_ids() {
        assert_ne!(window_id("Comms"), window_id("Keepers"));
    }

    #[test]
    fn namespacing_composites_with_colon() {
        assert_eq!(namespaced("wabc", "chrome"), "wabc:chrome");
    }
}
