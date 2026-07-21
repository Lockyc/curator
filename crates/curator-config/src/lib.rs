//! curator-config: parse, validate, and resolve curator's TOML config (windows + browser keeper-tabs).
//!
//! The house-style TOML formatter + colour parsing are shared with warden via the config-core crate,
//! re-exported here so the app (`src-tauri`) uses `curator_config::{Colour, format_file, format_str}`.
pub use config_core::{
    fmt_cli, format_file, format_str, write_default_config, Colour, ColourError, Density, Group,
    SeedError, Warning,
};

// Pure label-identity helpers (FNV-1a hash + window-label namespacing), used when resolving each
// tab's webview label below. No Tauri/macOS deps, so the crate stays platform-neutral.
pub mod hash;
pub mod identity;

use serde::{Deserialize, Serialize};
use std::path::Path;

// `Density`, the `Group<Tab>` container, `Warning`, and the `default_*` serde helpers are the
// leaf-free primitives shared with lector (and, for Density/Warning, warden) — they live in
// config-core and are re-exported above. curator layers its own leaf `Tab` on top.

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Force dark appearance app-wide (applied per window at setup). Omit/false = follow system.
    #[serde(default)]
    pub dark_mode: bool,
    /// Reformat the config file in place (house style) on a clean hot-reload. Default false.
    /// The rewrite is diff-guarded, so an already-formatted file is a no-op and the writer can't
    /// loop its own watcher. Also available on demand via `curator fmt`.
    #[serde(default)]
    pub format_on_save: bool,
    /// Hosts whose self-signed/invalid TLS curator accepts. Process-wide (WebKit-global).
    #[serde(default)]
    pub allow_insecure: Vec<String>,
    /// App-wide default login store — the bottom of the session chain
    /// (`tab.session → window.session → this → DEFAULT_SESSION`). Set it to make every tab that
    /// doesn't override share one store. Omit → the built-in `DEFAULT_SESSION`. An explicit
    /// `session = ""` is treated as unset and falls through to `DEFAULT_SESSION`.
    #[serde(default)]
    pub session: Option<String>,
    /// Chrome sizing mode (whole-app). Default comfortable; `compact` proportionally condenses
    /// the chrome. See [`Density`].
    #[serde(default)]
    pub density: Density,
    /// Whether the sidebar chrome acts as a window-move drag handle (whole-app). Default true;
    /// `false` turns it off. The chrome maps this to the component's `windowDrag` flag.
    #[serde(default = "config_core::default_true")]
    pub sidebar_drag: bool,
    /// Whether curator checks for a new release on launch (whole-app). Default true; `false`
    /// suppresses the automatic launch check. The manual **Check for Updates…** menu item stays
    /// available regardless. The chrome gates its launch check on this.
    #[serde(default = "config_core::default_true")]
    pub auto_update: bool,
    #[serde(default, rename = "window")]
    pub windows: Vec<WindowConfig>,
}

// Hand-written (not derived) so `sidebar_drag` defaults to `true`, matching serde's
// `default_true` parse default — a derived `bool` default would be `false` and silently
// disagree with parsing an empty config. The struct literal makes this drift-proof: adding
// a field fails to compile until it's set here too.
impl Default for Config {
    fn default() -> Self {
        Config {
            dark_mode: false,
            format_on_save: false,
            allow_insecure: Vec::new(),
            session: None,
            density: Density::Comfortable,
            sidebar_drag: true,
            auto_update: true,
            windows: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    pub title: String,
    #[serde(default = "config_core::default_window_width")]
    pub width: u32,
    #[serde(default = "config_core::default_window_height")]
    pub height: u32,
    /// Default login store for this window's tabs (the middle link of the session chain
    /// `tab.session → window.session → app-wide default`). Set it to make the whole window one
    /// profile. Omit → tabs fall back to the shared app-wide store unless they set their own.
    #[serde(default)]
    pub session: Option<String>,
    /// Which tab is active when this window launches. `false`/unset opens the first `load_on_open`
    /// (loaded) tab, else the blank background — the first tab isn't always loaded, so it isn't
    /// forced. `true` opens the first tab even if it isn't loaded. Titles are display labels, not
    /// addresses, so there's no "open the tab named X" form (that would tie launch to a duplicable
    /// title — the warden/family way is that title never selects a tab).
    #[serde(default)]
    pub open_on_launch: bool,
    /// Optional per-window accent colour (`#rgb` or `#rrggbb`). The chrome shows it as a
    /// name banner + a faint tint, giving each window an at-a-glance identity. Omit → neutral.
    #[serde(default)]
    pub colour: Option<String>,
    /// Loose (ungrouped) tabs. They render in a leading headerless section, before any group.
    /// Curator no longer requires groups — a window can mix loose tabs and groups, or use only
    /// loose tabs.
    #[serde(default, rename = "tab")]
    pub tabs: Vec<Tab>,
    #[serde(default, rename = "group")]
    pub groups: Vec<Group<Tab>>,
}

/// True for a `#rgb` or `#rrggbb` hex colour — the forms the chrome banner accepts. Delegates to
/// the shared `config_core` parser so curator and warden validate accent colours identically.
fn is_hex_colour(s: &str) -> bool {
    config_core::Colour::parse(s).is_ok()
}

/// The shared app-wide login store used by any tab that sets no `session` (and whose window
/// sets none either). One store → tabs share cookies, so SSO across related services works.
pub const DEFAULT_SESSION: &str = "default";

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tab {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub load_on_open: bool,
    #[serde(default)]
    pub reload_every: Option<u64>,
    /// This tab's login store (top link of the session chain). Tabs sharing a `session` string
    /// share a login (even across windows); a distinct string gives a separate account. Omit →
    /// inherit the window's `session`, else the app-wide default.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    EmptyField(&'static str),
    DuplicateWindowTitle(String),
    DuplicateGroupName { window: String, name: String },
    InvalidUrl { title: String, url: String },
    ZeroReload(String),
    InvalidWindowSize { width: u32, height: u32 },
    InvalidColour { title: String, colour: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "cannot read config: {e}"),
            ConfigError::Parse(e) => write!(f, "invalid TOML: {e}"),
            ConfigError::EmptyField(field) => write!(f, "empty {field}"),
            ConfigError::DuplicateWindowTitle(t) => write!(f, "duplicate window title: {t}"),
            ConfigError::DuplicateGroupName { window, name } => {
                write!(f, "window {window:?} has duplicate group name: {name:?}")
            }
            ConfigError::InvalidUrl { title, url } => {
                write!(f, "tab \"{title}\" has invalid url: {url}")
            }
            ConfigError::ZeroReload(title) => write!(f, "tab \"{title}\" reload_every must be > 0"),
            ConfigError::InvalidWindowSize { width, height } => {
                write!(f, "window size must be positive, got {width}×{height}")
            }
            ConfigError::InvalidColour { title, colour } => {
                write!(f, "window \"{title}\" has invalid colour: {colour}")
            }
        }
    }
}

/// Per-tab field validation shared by loose and grouped tabs: non-empty title + url, a parseable
/// url, and a positive `reload_every`.
fn validate_tab(tab: &Tab) -> Result<(), ConfigError> {
    if tab.title.trim().is_empty() {
        return Err(ConfigError::EmptyField("title"));
    }
    if tab.url.trim().is_empty() {
        return Err(ConfigError::EmptyField("url"));
    }
    if url::Url::parse(&tab.url).is_err() {
        return Err(ConfigError::InvalidUrl {
            title: tab.title.clone(),
            url: tab.url.clone(),
        });
    }
    if matches!(tab.reload_every, Some(0)) {
        return Err(ConfigError::ZeroReload(tab.title.clone()));
    }
    Ok(())
}

pub fn parse_and_validate(src: &str) -> Result<(Config, Vec<Warning>), ConfigError> {
    let cfg: Config = toml::from_str(src).map_err(ConfigError::Parse)?;
    let mut seen_titles = std::collections::HashSet::new();
    let mut warnings: Vec<Warning> = Vec::new();
    for w in &cfg.windows {
        if w.title.trim().is_empty() {
            return Err(ConfigError::EmptyField("title"));
        }
        if !seen_titles.insert(w.title.clone()) {
            return Err(ConfigError::DuplicateWindowTitle(w.title.clone()));
        }
        if w.width == 0 || w.height == 0 {
            return Err(ConfigError::InvalidWindowSize {
                width: w.width,
                height: w.height,
            });
        }
        if let Some(colour) = &w.colour {
            if !is_hex_colour(colour) {
                return Err(ConfigError::InvalidColour {
                    title: w.title.clone(),
                    colour: colour.clone(),
                });
            }
        }
        // Group names are unique per window. Tab *titles* are not checked — they're display
        // labels, never an address (identity is the URL-hash label, namespaced per window), so
        // duplicates are allowed (the warden/family way). A URL repeated within a window is
        // non-fatal (the labels still disambiguate via the `-1`/`-2` suffix in `tab_views`) but
        // warned once. The URL warning is intentionally per-window, not global: labels are
        // namespaced per window (`{window_id}:{tab_hash}`), so the same URL in two windows is no
        // collision — and it's a supported multi-account pattern (same service, two windows, two
        // sessions).
        let mut group_names = std::collections::HashSet::new();
        let mut seen_urls = std::collections::HashSet::new();
        let mut warned_urls = std::collections::HashSet::new();
        let window_title = w.title.clone();
        let mut check_tab = |tab: &Tab| -> Result<(), ConfigError> {
            validate_tab(tab)?;
            if !seen_urls.insert(tab.url.clone()) && warned_urls.insert(tab.url.clone()) {
                warnings.push(Warning {
                    window: window_title.clone(),
                    message: format!("duplicate url: {}", tab.url),
                });
            }
            Ok(())
        };
        for tab in &w.tabs {
            check_tab(tab)?;
        }
        for group in &w.groups {
            if group.name.trim().is_empty() {
                return Err(ConfigError::EmptyField("name"));
            }
            if !group_names.insert(group.name.trim().to_string()) {
                return Err(ConfigError::DuplicateGroupName {
                    window: w.title.clone(),
                    name: group.name.clone(),
                });
            }
            for tab in &group.tabs {
                check_tab(tab)?;
            }
        }
    }
    Ok((cfg, warnings))
}

pub fn load_config(path: &Path) -> Result<(Config, Vec<Warning>), ConfigError> {
    let src = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
    parse_and_validate(&src)
}

const CONFIG_ENV: &str = "CURATOR_CONFIG";
const CONFIG_DIR: &str = "curator";

/// Config path to load at launch: `$CURATOR_CONFIG` if set and non-empty, else
/// [`default_config_path`]. Shared with warden and lector via config-core — including the
/// set-but-empty fall-through, which this app previously got wrong.
pub fn resolve_config_path() -> std::path::PathBuf {
    config_core::resolve_config_path(CONFIG_ENV, CONFIG_DIR)
}

/// Default config location: `~/.config/curator/config.toml`.
pub fn default_config_path() -> std::path::PathBuf {
    config_core::default_config_path(CONFIG_DIR)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TabView {
    pub label: String,
    /// The group this tab renders under, or `None` for a loose (ungrouped) tab — the chrome
    /// shows a section header only for `Some(name)`. Serialized to the sidebar as `null` for loose.
    pub group: Option<String>,
    pub title: String,
    pub url: String,
    pub load_on_open: bool,
    pub reload_every: Option<u64>,
    /// Resolved login store: `tab.session → window.session → Config.session → DEFAULT_SESSION`.
    /// Tabs with the
    /// same value share a WebKit data store (one login); distinct values are isolated. Not
    /// serialized to the chrome sidebar — it's a backend concern, not UI.
    #[serde(skip)]
    pub session: String,
}

/// Stable within-window webview label derived from a tab's URL. Position-independent so
/// inserting/removing/reordering tabs doesn't remap an existing webview. (Task 3 namespaces
/// this with the window id.)
fn url_label(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("tab-{:016x}", h.finish())
}

/// A configured `session` value, treating blank/whitespace-only as unset so an empty
/// `session = ""` falls through the chain rather than keying a store on "". Takes `Option<&str>`
/// so all three cascade links (tab/window `Option<String>` via `as_deref`, and the already-`&str`
/// global) share the one blank-is-unset rule.
fn normalized_session(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

impl WindowConfig {
    /// Flatten this window's loose tabs + groups → ordered tabs with stable labels (file order:
    /// loose tabs first as a headerless section, then each group). `global_session` is the
    /// app-wide session base (the bottom of the cascade); pass `None` for no global default.
    pub fn tab_views(&self, global_session: Option<&str>) -> Vec<TabView> {
        let wid = crate::identity::window_id(&self.title);
        let mut views = Vec::new();
        let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        // Loose tabs (group `None`) first, then each group's tabs (group `Some(name)`), all in
        // file order, sharing one url-label dedup map so duplicate URLs across the window still
        // get distinct labels.
        let entries = self.tabs.iter().map(|t| (t, Option::<String>::None)).chain(
            self.groups
                .iter()
                .flat_map(|g| g.tabs.iter().map(move |t| (t, Some(g.name.clone())))),
        );
        for (tab, group) in entries {
            let base = url_label(&tab.url);
            let n = seen.entry(base.clone()).or_insert(0);
            let within = if *n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            *n += 1;
            // Session chain: the tab's own store, else the window's, else the app-wide global,
            // else the shared default (blank values are treated as unset and fall through).
            let session = normalized_session(tab.session.as_deref())
                .or_else(|| normalized_session(self.session.as_deref()))
                .or_else(|| normalized_session(global_session))
                .unwrap_or_else(|| DEFAULT_SESSION.to_string());
            views.push(TabView {
                label: crate::identity::namespaced(&wid, &within),
                group,
                title: tab.title.clone(),
                url: tab.url.clone(),
                load_on_open: tab.load_on_open,
                reload_every: tab.reload_every,
                session,
            });
        }
        views
    }

    /// Label of the tab to make active on launch. `false`/unset: the first `load_on_open` (loaded)
    /// tab, else `None` (blank) — the first tab isn't always loaded, so it isn't forced. `true`:
    /// the first tab even if cold.
    pub fn startup_label(&self, global_session: Option<&str>) -> Option<String> {
        let views = self.tab_views(global_session);
        if self.open_on_launch {
            views.first().map(|v| v.label.clone())
        } else {
            views
                .iter()
                .find(|v| v.load_on_open)
                .map(|v| v.label.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[[window]]
title = "Comms"

[[window.group]]
name = "Chat"
[[window.group.tab]]
title = "Gmail"
url = "https://mail.google.com/"
[[window.group.tab]]
title = "Calendar"
url = "https://calendar.google.com/"
load_on_open = true
reload_every = 15
"#;

    #[test]
    fn density_defaults_comfortable_and_parses_compact() {
        assert_eq!(
            parse_and_validate(VALID).unwrap().0.density,
            Density::Comfortable
        );
        let cfg = parse_and_validate(&format!("density = \"compact\"\n{VALID}"))
            .unwrap()
            .0;
        assert_eq!(cfg.density, Density::Compact);
        // An unrecognised value is a parse error (serde rejects the unknown variant).
        assert!(parse_and_validate(&format!("density = \"roomy\"\n{VALID}")).is_err());
    }

    #[test]
    fn sidebar_drag_defaults_true_and_parses_false() {
        assert!(parse_and_validate(VALID).unwrap().0.sidebar_drag);
        let cfg = parse_and_validate(&format!("sidebar_drag = false\n{VALID}"))
            .unwrap()
            .0;
        assert!(!cfg.sidebar_drag);
        // The derived-vs-serde default trap: an empty config must agree with Config::default().
        assert!(Config::default().sidebar_drag);
    }

    #[test]
    fn auto_update_defaults_true_and_parses_false() {
        assert!(parse_and_validate(VALID).unwrap().0.auto_update);
        let cfg = parse_and_validate(&format!("auto_update = false\n{VALID}"))
            .unwrap()
            .0;
        assert!(!cfg.auto_update);
        assert!(Config::default().auto_update);
    }

    #[test]
    fn parses_windows_groups_tabs_in_order() {
        let cfg = parse_and_validate(VALID).unwrap().0;
        assert_eq!(cfg.windows.len(), 1);
        let w = &cfg.windows[0];
        assert_eq!(w.title, "Comms");
        assert_eq!(w.groups.len(), 1);
        assert_eq!(w.groups[0].tabs.len(), 2);
        assert_eq!(w.groups[0].tabs[0].title, "Gmail");
    }

    #[test]
    fn per_window_size_defaults_and_overrides() {
        let cfg = parse_and_validate(VALID).unwrap().0;
        assert_eq!((cfg.windows[0].width, cfg.windows[0].height), (1500, 1000));
        let cfg = parse_and_validate(&with_window_keys("Comms", "width = 1680\nheight = 1120"))
            .unwrap()
            .0;
        assert_eq!((cfg.windows[0].width, cfg.windows[0].height), (1680, 1120));
    }

    #[test]
    fn app_global_dark_and_insecure() {
        let cfg = parse_and_validate(&format!(
            "dark_mode = true\nallow_insecure = [\"10.0.0.1\"]\n{VALID}"
        ))
        .unwrap()
        .0;
        assert!(cfg.dark_mode);
        assert_eq!(cfg.allow_insecure, vec!["10.0.0.1".to_string()]);
    }

    #[test]
    fn rejects_duplicate_window_titles() {
        let src = format!("{VALID}\n[[window]]\ntitle = \"Comms\"\n");
        let err = parse_and_validate(&src).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateWindowTitle(_)));
    }

    #[test]
    fn rejects_empty_window_title() {
        let src = "[[window]]\ntitle = \"\"\n";
        assert!(matches!(
            parse_and_validate(src).unwrap_err(),
            ConfigError::EmptyField("title")
        ));
    }

    #[test]
    fn rejects_zero_window_dimension() {
        let err = parse_and_validate(&with_window_keys("Comms", "width = 0")).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidWindowSize { .. }));
    }

    #[test]
    fn rejects_unknown_tab_key() {
        // The removed `always_load` (renamed to `load_on_open`) and any typo'd key must fail
        // loudly via `deny_unknown_fields` rather than being silently ignored — otherwise an
        // eager tab would be quietly demoted to lazy with no signal.
        let src = format!("{VALID}\n[[window.tab]]\ntitle = \"X\"\nurl = \"https://x.test/\"\nalways_load = true\n");
        assert!(matches!(
            parse_and_validate(&src).unwrap_err(),
            ConfigError::Parse(_)
        ));
    }

    #[test]
    fn accepts_valid_window_colour() {
        let cfg = parse_and_validate(&with_window_keys("Comms", "colour = \"#0f8a8a\""))
            .unwrap()
            .0;
        assert_eq!(cfg.windows[0].colour.as_deref(), Some("#0f8a8a"));
        // Short form is accepted too.
        let cfg = parse_and_validate(&with_window_keys("Comms", "colour = \"#abc\""))
            .unwrap()
            .0;
        assert_eq!(cfg.windows[0].colour.as_deref(), Some("#abc"));
    }

    #[test]
    fn rejects_invalid_window_colour() {
        let err = parse_and_validate(&with_window_keys("Comms", "colour = \"teal\"")).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidColour { .. }));
    }

    #[test]
    fn rejects_invalid_tab_url() {
        let src = "[[window]]\ntitle = \"W\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"Bad\"\nurl = \"not a url\"\n";
        assert!(matches!(
            parse_and_validate(src).unwrap_err(),
            ConfigError::InvalidUrl { .. }
        ));
    }

    #[test]
    fn rejects_zero_reload_every() {
        let src = "[[window]]\ntitle = \"W\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\nreload_every = 0\n";
        assert!(matches!(
            parse_and_validate(src).unwrap_err(),
            ConfigError::ZeroReload(_)
        ));
    }

    #[test]
    fn flattens_to_ordered_tabviews_per_window() {
        let cfg = parse_and_validate(VALID).unwrap().0;
        let views = cfg.windows[0].tab_views(None);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].title, "Gmail");
        assert_ne!(views[0].label, views[1].label);
        assert!(views[1].load_on_open);
        assert_eq!(views[1].reload_every, Some(15));
    }

    #[test]
    fn startup_label_resolves_per_window() {
        // Default (no `open_on_launch`) opens the first LOADED (load_on_open) tab — not necessarily
        // the first. Here Gmail is cold and Calendar is load_on_open, so Calendar wins.
        let loaded = "[[window]]\ntitle = \"Comms\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n[[window.group.tab]]\ntitle = \"Calendar\"\nurl = \"https://calendar.google.com/\"\nload_on_open = true\n";
        let cfg = parse_and_validate(loaded).unwrap().0;
        let cal = cfg.windows[0]
            .tab_views(None)
            .into_iter()
            .find(|v| v.title == "Calendar")
            .unwrap()
            .label;
        assert_eq!(cfg.windows[0].startup_label(None), Some(cal));

        // `open_on_launch = true` forces the first tab even though it's cold (Gmail).
        let forced = "[[window]]\ntitle = \"Comms\"\nopen_on_launch = true\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n[[window.group.tab]]\ntitle = \"Calendar\"\nurl = \"https://calendar.google.com/\"\nload_on_open = true\n";
        let cfg = parse_and_validate(forced).unwrap().0;
        assert_eq!(
            cfg.windows[0].startup_label(None),
            Some(cfg.windows[0].tab_views(None)[0].label.clone())
        );

        // No loaded tab anywhere → blank background, nothing forced.
        let cold = "[[window]]\ntitle = \"Comms\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n";
        let cfg = parse_and_validate(cold).unwrap().0;
        assert_eq!(cfg.windows[0].startup_label(None), None);
    }

    #[test]
    fn open_on_launch_string_is_rejected() {
        // `open_on_launch` is a plain bool now — titles are display labels, never an address, so
        // the old `open_on_launch = "<title>"` form no longer parses (breaking change).
        let named = "[[window]]\ntitle = \"Comms\"\nopen_on_launch = \"Calendar\"\n[[window.tab]]\ntitle = \"Calendar\"\nurl = \"https://calendar.google.com/\"\n";
        assert!(matches!(
            parse_and_validate(named),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn resolve_config_path_honours_env_override() {
        // Unset → the default ~/.config/curator/config.toml.
        std::env::remove_var("CURATOR_CONFIG");
        assert_eq!(resolve_config_path(), default_config_path());

        // Set → exactly that path.
        std::env::set_var("CURATOR_CONFIG", "/tmp/curator-dev.toml");
        assert_eq!(
            resolve_config_path(),
            std::path::PathBuf::from("/tmp/curator-dev.toml")
        );
        std::env::remove_var("CURATOR_CONFIG");
    }

    #[test]
    fn a_set_but_empty_env_var_falls_through_to_the_default() {
        // curator shipped this bug: `var_os(..).map(PathBuf::from)` turned an empty
        // CURATOR_CONFIG into PathBuf::from(""), whose only symptom was
        // "cannot read config: No such file or directory".
        std::env::set_var("CURATOR_CONFIG", "");
        assert_eq!(resolve_config_path(), default_config_path());
        std::env::remove_var("CURATOR_CONFIG");
    }

    #[test]
    fn load_missing_file_errors() {
        let err = load_config(std::path::Path::new("/no/such/curator.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io(_)));
    }

    #[test]
    fn malformed_toml_is_parse_error() {
        let err = parse_and_validate("this is not toml = =").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn label_is_stable_when_a_tab_is_inserted_before_it() {
        let base = parse_and_validate(VALID).unwrap().0;
        let gmail_label = base.windows[0]
            .tab_views(None)
            .into_iter()
            .find(|t| t.url == "https://mail.google.com/")
            .unwrap()
            .label;
        // Insert a new tab ahead of Gmail in the same window; Gmail's label must not move.
        let src = "[[window]]\ntitle = \"Comms\"\n[[window.group]]\nname = \"New\"\n[[window.group.tab]]\ntitle = \"X\"\nurl = \"https://x.test/\"\n[[window.group]]\nname = \"Chat\"\n[[window.group.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n";
        let inserted = parse_and_validate(src).unwrap().0;
        let gmail = inserted.windows[0]
            .tab_views(None)
            .into_iter()
            .find(|t| t.url == "https://mail.google.com/")
            .unwrap();
        assert_eq!(
            gmail.label, gmail_label,
            "Gmail's label must not change when a tab is inserted before it"
        );
    }

    #[test]
    fn duplicate_urls_get_distinct_labels() {
        let src = "[[window]]\ntitle = \"W\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"A\"\nurl = \"https://same.test/\"\n[[window.group.tab]]\ntitle = \"B\"\nurl = \"https://same.test/\"\n";
        let cfg = parse_and_validate(src).unwrap().0;
        let views = cfg.windows[0].tab_views(None);
        assert_ne!(views[0].label, views[1].label);
    }

    #[test]
    fn tab_labels_are_window_namespaced() {
        let cfg = parse_and_validate(VALID).unwrap().0;
        let wid = crate::identity::window_id("Comms");
        assert!(cfg.windows[0].tab_views(None)[0]
            .label
            .starts_with(&format!("{wid}:")));
    }

    #[test]
    fn bundled_example_config_parses() {
        let src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/config.toml"
        ));
        assert!(
            parse_and_validate(src).is_ok(),
            "examples/config.toml must parse: {:?}",
            parse_and_validate(src).unwrap_err()
        );
    }

    #[test]
    fn session_chain_resolves_tab_then_window_then_default() {
        // Window sets a session; first tab inherits it, second overrides with its own.
        let src = "[[window]]\ntitle = \"W\"\nsession = \"win\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"Inherits\"\nurl = \"https://a.test/\"\n[[window.group.tab]]\ntitle = \"Own\"\nurl = \"https://b.test/\"\nsession = \"tabown\"\n";
        let views = parse_and_validate(src).unwrap().0.windows[0].tab_views(None);
        assert_eq!(views[0].session, "win");
        assert_eq!(views[1].session, "tabown");

        // Neither tab nor window sets a session → the shared app-wide default.
        let bare = "[[window]]\ntitle = \"X\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n";
        assert_eq!(
            parse_and_validate(bare).unwrap().0.windows[0].tab_views(None)[0].session,
            DEFAULT_SESSION
        );

        // A blank session is treated as unset and falls through to the default.
        let blank = "[[window]]\ntitle = \"Y\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\nsession = \"  \"\n";
        assert_eq!(
            parse_and_validate(blank).unwrap().0.windows[0].tab_views(None)[0].session,
            DEFAULT_SESSION
        );
    }

    #[test]
    fn session_cascades_from_global() {
        let src = r#"
session = "shared"
[[window]]
title = "W"
  [[window.tab]]
  title = "T"
  url = "https://t.test/"
"#;
        let cfg = parse_and_validate(src).unwrap().0;
        let views = cfg.windows[0].tab_views(cfg.session.as_deref());
        assert_eq!(views[0].session, "shared");
    }

    #[test]
    fn empty_window_session_falls_through_to_global() {
        // Window session "" opts out of being the window default; the tab is unset → falls
        // through the chain to the global "shared" (empty = unset, not "force default").
        let src = r#"
session = "shared"
[[window]]
title = "W"
session = ""
  [[window.tab]]
  title = "T"
  url = "https://t.test/"
"#;
        let cfg = parse_and_validate(src).unwrap().0;
        let views = cfg.windows[0].tab_views(cfg.session.as_deref());
        assert_eq!(views[0].session, "shared");
    }

    #[test]
    fn loose_tabs_resolve_before_groups_with_none_group() {
        let src = r#"
[[window]]
title = "W"
  [[window.tab]]
  title = "Loose"
  url = "https://loose.test/"
  [[window.group]]
  name = "G"
    [[window.group.tab]]
    title = "Grouped"
    url = "https://grouped.test/"
"#;
        let cfg = parse_and_validate(src).unwrap().0;
        let views = cfg.windows[0].tab_views(None);
        assert_eq!(views[0].title, "Loose");
        assert_eq!(views[0].group, None);
        assert_eq!(views[1].title, "Grouped");
        assert_eq!(views[1].group.as_deref(), Some("G"));
    }

    #[test]
    fn loose_tab_with_empty_url_errors() {
        let src = r#"
[[window]]
title = "W"
  [[window.tab]]
  title = "Loose"
  url = "  "
"#;
        assert!(matches!(
            parse_and_validate(src),
            Err(ConfigError::EmptyField("url"))
        ));
    }

    #[test]
    fn duplicate_url_within_window_warns() {
        let src = r#"
[[window]]
title = "W"
  [[window.tab]]
  title = "A"
  url = "https://same.test/"
  [[window.tab]]
  title = "B"
  url = "https://same.test/"
"#;
        let (_cfg, warnings) = parse_and_validate(src).unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.window == "W" && w.message.contains("duplicate url")));
    }

    #[test]
    fn duplicate_tab_title_window_wide_is_allowed() {
        // Titles are display labels, not addresses (identity is the URL-hash label), so a title
        // repeated across loose + grouped tabs is fine — the warden/family way. The two distinct
        // URLs still yield distinct labels.
        let src = r#"
[[window]]
title = "W"
  [[window.tab]]
  title = "Dup"
  url = "https://a.test/"
  [[window.group]]
  name = "G"
    [[window.group.tab]]
    title = "Dup"
    url = "https://b.test/"
"#;
        let (cfg, warnings) = parse_and_validate(src).unwrap();
        let labels: Vec<_> = cfg.windows[0]
            .tab_views(None)
            .into_iter()
            .map(|v| v.label)
            .collect();
        assert_eq!(labels.len(), 2);
        assert_ne!(labels[0], labels[1], "distinct URLs → distinct labels");
        // Same title, different URLs → no duplicate-url warning either.
        assert!(warnings.is_empty());
    }

    #[test]
    fn duplicate_group_name_errors() {
        let src = r#"
[[window]]
title = "W"
  [[window.group]]
  name = "G"
    [[window.group.tab]]
    title = "A"
    url = "https://a.test/"
  [[window.group]]
  name = "G"
    [[window.group.tab]]
    title = "B"
    url = "https://b.test/"
"#;
        assert!(matches!(
            parse_and_validate(src),
            Err(ConfigError::DuplicateGroupName { .. })
        ));
    }

    // Test helpers: build a one-window config with the given extra window-level keys.
    fn with_window_keys(title: &str, keys: &str) -> String {
        format!(
            "[[window]]\ntitle = \"{title}\"\n{keys}\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n"
        )
    }
}
