//! Fire native macOS banner notifications, Rust-side. Called from the `on_navigation`
//! notify-sentinel handler — so notifications never require granting a remote service
//! webview any command/IPC capability.

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Show a native banner. Best-effort: a failure (e.g. the user denied the macOS
/// notification permission) is swallowed.
pub fn fire(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}
