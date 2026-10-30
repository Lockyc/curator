//! curator's side of shell-core's shared window layer: the [`WindowHost`] that builds and tears
//! down a window's content, the config → [`WindowDecl`] mapping, and the New / Edit / Delete Window
//! flows (shell-core's form decides, config-core's edits write, the reload applies).

use curator_config::edit::{self, At};
use curator_config::toml_edit::Value;
use shell_core::windows::form::{open_form, FormMode, FormResult};
use shell_core::windows::plan::{Slot, WindowDecl};
use shell_core::windows::{WindowHost, WindowSet};
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::{AppHandle, Manager, WebviewWindow, Wry};

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

/// curator's per-window content, built and torn down by the managed [`WindowSet`].
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
}

/// The `set_field` writes an Edit Window result needs, colour first so it is addressed while the
/// window still carries `old_title`. `None` removes the key.
pub(crate) fn edits(
    old_title: &str,
    old_colour: Option<&str>,
    new_title: &str,
    new_colour: Option<&str>,
) -> Vec<(&'static str, Option<String>)> {
    let mut out = Vec::new();
    if old_colour != new_colour {
        out.push(("colour", new_colour.map(str::to_string)));
    }
    if old_title.trim() != new_title.trim() {
        out.push(("title", Some(new_title.trim().to_string())));
    }
    out
}

/// An item of the banner's context menu, addressing a window by its label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BannerAction {
    Edit(String),
    Delete(String),
}

const EDIT_PREFIX: &str = "curator:edit_window:";
const DELETE_PREFIX: &str = "curator:delete_window:";

impl BannerAction {
    pub(crate) fn id(&self) -> String {
        match self {
            BannerAction::Edit(l) => format!("{EDIT_PREFIX}{l}"),
            BannerAction::Delete(l) => format!("{DELETE_PREFIX}{l}"),
        }
    }

    pub(crate) fn parse(id: &str) -> Option<Self> {
        id.strip_prefix(EDIT_PREFIX)
            .map(|l| BannerAction::Edit(l.to_string()))
            .or_else(|| {
                id.strip_prefix(DELETE_PREFIX)
                    .map(|l| BannerAction::Delete(l.to_string()))
            })
    }

    pub(crate) fn run(self, app: &AppHandle) {
        match self {
            BannerAction::Edit(l) => edit_window(app, &l),
            BannerAction::Delete(l) => delete_window(app, &l),
        }
    }
}

/// The banner's context menu for the window labelled `label`.
pub(crate) fn banner_menu(app: &AppHandle, label: &str) -> tauri::Result<Menu<Wry>> {
    let edit = MenuItemBuilder::with_id(BannerAction::Edit(label.into()).id(), "Edit Window…")
        .build(app)?;
    let delete =
        MenuItemBuilder::with_id(BannerAction::Delete(label.into()).id(), "Delete Window…")
            .build(app)?;
    MenuBuilder::new(app).items(&[&edit, &delete]).build()
}

/// `label`'s current title and colour.
fn current(app: &AppHandle, label: &str) -> Option<(String, Option<String>)> {
    app.state::<WindowSet>()
        .entries()
        .into_iter()
        .find(|e| e.id == label)
        .map(|e| (e.title, e.colour))
}

/// Every window title except `label`'s: the titles the form refuses.
fn taken(app: &AppHandle, except: Option<&str>) -> Vec<String> {
    app.state::<WindowSet>()
        .entries()
        .into_iter()
        .filter(|e| Some(e.id.as_str()) != except)
        .map(|e| e.title)
        .collect()
}

/// Surface a failed config write in every window's sidebar.
fn report(app: &AppHandle, result: Result<(), impl std::fmt::Display>) -> bool {
    match result {
        Ok(()) => true,
        Err(e) => {
            eprintln!("window edit failed: {e}");
            crate::emit_to_all_chrome(app, "config-error", e.to_string());
            false
        }
    }
}

/// Window ▸ New Window…: on Create, append the `[[window]]`; the reload opens it.
pub(crate) fn new_window(app: &AppHandle) {
    let app2 = app.clone();
    open_form(app, FormMode::New, taken(app, None), move |r| {
        if let FormResult::Create { title, colour } = r {
            let mut fields = vec![("title", Value::from(title))];
            fields.extend(colour.map(|c| ("colour", Value::from(c))));
            let path = curator_config::resolve_config_path();
            report(&app2, edit::add_window(&path, &fields));
        }
    });
}

/// Banner ▸ Edit Window…: on Update, write the changed keys, then retitle the window in place.
fn edit_window(app: &AppHandle, label: &str) {
    let Some((title, colour)) = current(app, label) else {
        return;
    };
    let (app2, label) = (app.clone(), label.to_string());
    let mode = FormMode::Edit { title, colour };
    open_form(app, mode, taken(app, Some(&label)), move |r| {
        if let FormResult::Update { title, colour } = r {
            apply_edit(&app2, &label, &title, colour.as_deref());
        }
    });
}

fn apply_edit(app: &AppHandle, label: &str, new_title: &str, new_colour: Option<&str>) {
    let Some((old_title, old_colour)) = current(app, label) else {
        return;
    };
    let set = app.state::<WindowSet>();
    if set.label_for_title(new_title).is_some_and(|l| l != label) {
        let taken = shell_core::windows::RenameError::Taken(new_title.trim().into());
        report(app, Err(taken));
        return;
    }
    let path = curator_config::resolve_config_path();
    let at = At {
        window: &old_title,
        group: None,
        tab: None,
    };
    for (key, value) in edits(&old_title, old_colour.as_deref(), new_title, new_colour) {
        if !report(app, edit::set_field(&path, at, key, value.map(Value::from))) {
            return;
        }
    }
    if old_title.trim() != new_title.trim() {
        report(app, set.rename(app, label, new_title));
    }
    crate::refresh_window_menu(app);
}

/// Banner ▸ Delete Window…: on confirm, remove the `[[window]]`, then close and forget the window.
fn delete_window(app: &AppHandle, label: &str) {
    let Some((title, _)) = current(app, label) else {
        return;
    };
    let (app2, label) = (app.clone(), label.to_string());
    open_form(
        app,
        FormMode::ConfirmDelete { title },
        Vec::new(),
        move |r| {
            if r != FormResult::Delete {
                return;
            }
            let Some((title, _)) = current(&app2, &label) else {
                return;
            };
            let at = At {
                window: &title,
                group: None,
                tab: None,
            };
            let path = curator_config::resolve_config_path();
            if report(&app2, edit::remove(&path, at)) {
                app2.state::<WindowSet>().delete(&app2, &label);
                crate::refresh_window_menu(&app2);
            }
        },
    );
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
    fn an_edit_writes_only_what_changed_colour_first() {
        // Unchanged: nothing to write.
        assert!(edits("Work", Some("#abc"), " Work ", Some("#abc")).is_empty());
        // Colour cleared, title kept.
        assert_eq!(
            edits("Work", Some("#abc"), "Work", None),
            [("colour", None)]
        );
        // Both changed: colour first, so it is addressed while the window still has its old title.
        assert_eq!(
            edits("Work", None, "Job", Some("#f00")),
            [
                ("colour", Some("#f00".to_string())),
                ("title", Some("Job".to_string()))
            ]
        );
    }

    #[test]
    fn banner_menu_ids_round_trip_their_window_label() {
        for action in [
            BannerAction::Edit("a:b".into()),
            BannerAction::Delete("Work-2".into()),
        ] {
            assert_eq!(BannerAction::parse(&action.id()), Some(action));
        }
        assert_eq!(BannerAction::parse("shell:new_window"), None);
    }
}
