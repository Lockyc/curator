//! curator's side of shell-core's shared window layer: the [`WindowHost`] that builds and tears
//! down a window's content and writes the window form's changes to the config, and the config →
//! [`WindowDecl`] mapping.

use curator_config::edit::{self, At};
use curator_config::toml_edit::Value;
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
        write_window(&curator_config::resolve_config_path(), write).map_err(|e| e.to_string())
    }

    fn edit_failed(&self, app: &AppHandle, message: &str) {
        eprintln!("window edit failed: {message}");
        crate::emit_to_all_chrome(app, "config-error", message.to_string());
    }

    fn changed(&self, app: &AppHandle) {
        crate::refresh_window_menu(app);
    }
}

/// Apply `write` to the config at `path` through config-core's edits.
fn write_window(path: &std::path::Path, write: &WindowWrite) -> Result<(), edit::EditError> {
    let at = |window| At {
        window,
        group: None,
        tab: None,
    };
    match write {
        WindowWrite::Add { title, colour } => {
            let mut fields = vec![("title", Value::from(title.as_str()))];
            fields.extend(colour.as_deref().map(|c| ("colour", Value::from(c))));
            edit::add_window(path, &fields)
        }
        WindowWrite::Set {
            title,
            new_title,
            colour,
        } => {
            if let Some(colour) = colour {
                let value = colour.as_deref().map(Value::from);
                edit::set_field(path, at(title), "colour", value)?;
            }
            if let Some(new_title) = new_title {
                let value = Some(Value::from(new_title.as_str()));
                edit::set_field(path, at(title), "title", value)?;
            }
            Ok(())
        }
        WindowWrite::Remove { title } => edit::remove(path, at(title)),
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

    #[test]
    fn window_writes_land_in_the_config() {
        let dir = std::env::temp_dir().join(format!("curator-winwrite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[[window]]\ntitle = \"Work\"\ncolour = \"#abc\"\n").unwrap();
        let read = || cfg(&std::fs::read_to_string(&path).unwrap());

        let add = WindowWrite::Add {
            title: "Side".into(),
            colour: Some("#f00".into()),
        };
        write_window(&path, &add).unwrap();
        assert_eq!(read().windows[1].colour.as_deref(), Some("#f00"));

        let set = WindowWrite::Set {
            title: "Work".into(),
            new_title: Some("Job".into()),
            colour: Some(None),
        };
        write_window(&path, &set).unwrap();
        let c = read();
        assert_eq!(
            (c.windows[0].title.as_str(), c.windows[0].colour.as_deref()),
            ("Job", None)
        );

        let remove = WindowWrite::Remove {
            title: "Side".into(),
        };
        write_window(&path, &remove).unwrap();
        assert_eq!(read().windows.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
