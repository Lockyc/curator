//! Webview label identity. A window's Tauri label (assigned by shell-core's `WindowSet`)
//! namespaces its content webview labels, since Tauri labels are app-global and two windows sharing
//! a URL would otherwise collide.

/// Namespace a within-window label (e.g. `tab-<hash>`) under a window label. `:` is a legal Tauri
/// label char, so `window_label:within` composites are valid labels.
pub fn namespaced(window_label: &str, within: &str) -> String {
    format!("{window_label}:{within}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespacing_composites_with_colon() {
        assert_eq!(namespaced("wabc", "chrome"), "wabc:chrome");
    }
}
