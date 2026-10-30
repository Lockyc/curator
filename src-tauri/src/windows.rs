//! curator's side of shell-core's shared window layer: the [`WindowHost`] that builds and tears
//! down a window's content and writes the window form's changes to the config, and the config →
//! [`WindowDecl`] mapping.

use curator_config::edit::{self, At};
use shell_core::windows::plan::{Slot, WindowDecl};
use shell_core::windows::{WindowHost, WindowWrite};
use tauri::{AppHandle, WebviewWindow};

/// What shell-core needs to know about one `[[window]]`.
pub(crate) fn decl(w: &curator_config::WindowConfig) -> WindowDecl {
    WindowDecl {
        title: w.title.clone(),
        colour: w.colour.clone(),
        width: w.width as f64,
        height: w.height as f64,
        open_on_start: w.open_on_start,
    }
}

/// Every window's [`decl`], config order.
pub(crate) fn decls(cfg: &curator_config::Config) -> Vec<WindowDecl> {
    cfg.windows.iter().map(decl).collect()
}

/// curator's per-window content, built and torn down by the managed
/// [`WindowSet`](shell_core::windows::WindowSet).
pub(crate) struct CuratorHost;

impl WindowHost for CuratorHost {
    fn build(&self, app: &AppHandle, slot: &Slot) -> tauri::Result<WebviewWindow> {
        crate::open_window(app, slot)
    }

    fn closed(&self, app: &AppHandle, label: &str) {
        crate::window_closed(app, label);
    }

    fn retitled(&self, app: &AppHandle, label: &str, decl: &WindowDecl) {
        crate::window_retitled(app, label, decl);
    }

    fn write(&self, _app: &AppHandle, write: &WindowWrite) -> Result<(), String> {
        let path = curator_config::resolve_config_path();
        match write {
            WindowWrite::Add { title, colour } => edit::add_window(&path, title, colour.as_deref()),
            WindowWrite::Set {
                title,
                new_title,
                colour,
            } => edit::edit_window(
                &path,
                title,
                new_title.as_deref(),
                colour.as_ref().map(Option::as_deref),
            ),
            WindowWrite::Remove { title } => edit::remove(
                &path,
                At {
                    window: title,
                    group: None,
                    tab: None,
                },
            ),
        }
        .map_err(|e| e.to_string())
    }

    fn edit_failed(&self, app: &AppHandle, message: &str) {
        eprintln!("window edit failed: {message}");
        crate::emit_to_all_chrome(app, "config-error", message.to_string());
    }

    fn changed(&self, app: &AppHandle) {
        crate::refresh_window_menu(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(src: &str) -> curator_config::Config {
        curator_config::parse_and_validate(src).unwrap().0
    }

    #[test]
    fn windows_map_to_decls_in_config_order() {
        let c = cfg("[[window]]\ntitle = \"Work\"\nwidth = 800\nheight = 600\ncolour = \"#abc\"\nopen_on_start = false\nsession = \"s\"\n[[window]]\ntitle = \"Side\"\n");
        assert_eq!(
            decls(&c),
            vec![
                WindowDecl {
                    title: "Work".into(),
                    colour: Some("#abc".into()),
                    width: 800.0,
                    height: 600.0,
                    open_on_start: false,
                },
                WindowDecl {
                    title: "Side".into(),
                    colour: None,
                    width: 1500.0,
                    height: 1000.0,
                    open_on_start: true,
                },
            ]
        );
    }
}
