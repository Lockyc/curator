use serde::{Deserialize, Serialize};
use std::path::Path;

/// What to open when a window launches. `false` (default) → blank; `true` → its first tab;
/// a string → the tab whose `title` matches (falling back to the first).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OpenOnLaunch {
    Toggle(bool),
    Tab(String),
}
impl Default for OpenOnLaunch {
    fn default() -> Self {
        OpenOnLaunch::Toggle(false)
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Config {
    /// Force dark appearance app-wide (applied per window at setup). Omit/false = follow system.
    #[serde(default)]
    pub dark_mode: bool,
    /// Hosts whose self-signed/invalid TLS curator accepts. Process-wide (WebKit-global).
    #[serde(default)]
    pub allow_insecure: Vec<String>,
    #[serde(default, rename = "window")]
    pub windows: Vec<WindowConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WindowConfig {
    pub title: String,
    #[serde(default = "default_window_width")]
    pub width: u32,
    #[serde(default = "default_window_height")]
    pub height: u32,
    /// Opt into native banners from web `Notification` calls.
    #[serde(default)]
    pub notifications: bool,
    /// Opt into unread pills + dock-badge contribution.
    #[serde(default)]
    pub unread: bool,
    /// Default login store for this window's tabs (the middle link of the session chain
    /// `tab.session → window.session → app-wide default`). Set it to make the whole window one
    /// profile. Omit → tabs fall back to the shared app-wide store unless they set their own.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub open_on_launch: OpenOnLaunch,
    #[serde(default, rename = "group")]
    pub groups: Vec<Group>,
}

/// The shared app-wide login store used by any tab that sets no `session` (and whose window
/// sets none either). One store → tabs share cookies, so SSO across related services works.
pub const DEFAULT_SESSION: &str = "default";

/// A window is "live" (eager-load, never hide, inject sync/notify/badge shims) iff it opts
/// into either noisy feature. Plain windows keep curator's lazy/hide model.
impl WindowConfig {
    pub fn is_live(&self) -> bool {
        self.notifications || self.unread
    }
}

fn default_window_width() -> u32 {
    1500
}
fn default_window_height() -> u32 {
    1000
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Group {
    pub name: String,
    #[serde(default, rename = "tab")]
    pub tabs: Vec<Tab>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Tab {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub always_load: bool,
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
    InvalidUrl { title: String, url: String },
    ZeroReload(String),
    InvalidWindowSize { width: u32, height: u32 },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "cannot read config: {e}"),
            ConfigError::Parse(e) => write!(f, "invalid TOML: {e}"),
            ConfigError::EmptyField(field) => write!(f, "empty {field}"),
            ConfigError::DuplicateWindowTitle(t) => write!(f, "duplicate window title: {t}"),
            ConfigError::InvalidUrl { title, url } => {
                write!(f, "tab \"{title}\" has invalid url: {url}")
            }
            ConfigError::ZeroReload(title) => write!(f, "tab \"{title}\" reload_every must be > 0"),
            ConfigError::InvalidWindowSize { width, height } => {
                write!(f, "window size must be positive, got {width}×{height}")
            }
        }
    }
}

pub fn parse_and_validate(src: &str) -> Result<Config, ConfigError> {
    let cfg: Config = toml::from_str(src).map_err(ConfigError::Parse)?;
    let mut seen_titles = std::collections::HashSet::new();
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
        for group in &w.groups {
            if group.name.trim().is_empty() {
                return Err(ConfigError::EmptyField("name"));
            }
            for tab in &group.tabs {
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
            }
        }
    }
    Ok(cfg)
}

pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let src = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
    parse_and_validate(&src)
}

/// Config path to load at launch: `$CURATOR_CONFIG` if set, else [`default_config_path`].
///
/// The env override lets `just dev` point at the repo's `examples/config.toml` so iterating
/// on curator never touches the developer's real `~/.config/curator/config.toml`.
pub fn resolve_config_path() -> std::path::PathBuf {
    std::env::var_os("CURATOR_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_config_path)
}

/// Default config location: `~/.config/curator/config.toml`.
///
/// Deliberately `~/.config` (not `dirs::config_dir()`, which on macOS is
/// `~/Library/Application Support`) so the config slots into the dotfiles bare-repo workflow.
pub fn default_config_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".config")
        .join("curator")
        .join("config.toml")
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TabView {
    pub label: String,
    pub group: String,
    pub title: String,
    pub url: String,
    pub always_load: bool,
    pub reload_every: Option<u64>,
    /// Resolved login store: `tab.session → window.session → DEFAULT_SESSION`. Tabs with the
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
/// `session = ""` falls through the chain rather than keying a store on "".
fn normalized_session(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

impl WindowConfig {
    /// Flatten this window's groups → ordered tabs with stable labels (file order).
    pub fn tab_views(&self) -> Vec<TabView> {
        let wid = crate::identity::window_id(&self.title);
        let mut views = Vec::new();
        let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for group in &self.groups {
            for tab in &group.tabs {
                let base = url_label(&tab.url);
                let n = seen.entry(base.clone()).or_insert(0);
                let within = if *n == 0 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                };
                *n += 1;
                // Session chain: the tab's own store, else the window's, else the shared default
                // (blank values are treated as unset and fall through).
                let session = normalized_session(&tab.session)
                    .or_else(|| normalized_session(&self.session))
                    .unwrap_or_else(|| DEFAULT_SESSION.to_string());
                views.push(TabView {
                    label: crate::identity::namespaced(&wid, &within),
                    group: group.name.clone(),
                    title: tab.title.clone(),
                    url: tab.url.clone(),
                    always_load: tab.always_load,
                    reload_every: tab.reload_every,
                    session,
                });
            }
        }
        views
    }

    /// Label of the tab to open on launch (per `open_on_launch`). `None` = blank.
    pub fn startup_label(&self) -> Option<String> {
        let views = self.tab_views();
        match &self.open_on_launch {
            OpenOnLaunch::Toggle(false) => None,
            OpenOnLaunch::Toggle(true) => views.first().map(|v| v.label.clone()),
            OpenOnLaunch::Tab(title) => views
                .iter()
                .find(|v| v.title == *title)
                .or_else(|| views.first())
                .map(|v| v.label.clone()),
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
always_load = true
reload_every = 15
"#;

    #[test]
    fn parses_windows_groups_tabs_in_order() {
        let cfg = parse_and_validate(VALID).unwrap();
        assert_eq!(cfg.windows.len(), 1);
        let w = &cfg.windows[0];
        assert_eq!(w.title, "Comms");
        assert_eq!(w.groups.len(), 1);
        assert_eq!(w.groups[0].tabs.len(), 2);
        assert_eq!(w.groups[0].tabs[0].title, "Gmail");
    }

    #[test]
    fn per_window_size_defaults_and_overrides() {
        let cfg = parse_and_validate(VALID).unwrap();
        assert_eq!((cfg.windows[0].width, cfg.windows[0].height), (1500, 1000));
        let cfg =
            parse_and_validate(&with_window_keys("Comms", "width = 1680\nheight = 1120")).unwrap();
        assert_eq!((cfg.windows[0].width, cfg.windows[0].height), (1680, 1120));
    }

    #[test]
    fn flags_default_off_and_parse() {
        let cfg = parse_and_validate(VALID).unwrap();
        assert!(!cfg.windows[0].notifications);
        assert!(!cfg.windows[0].unread);
        assert!(!cfg.windows[0].is_live());
        let cfg = parse_and_validate(&with_window_keys(
            "Comms",
            "notifications = true\nunread = true",
        ))
        .unwrap();
        assert!(cfg.windows[0].notifications);
        assert!(cfg.windows[0].unread);
        assert!(cfg.windows[0].is_live());
    }

    #[test]
    fn app_global_dark_and_insecure() {
        let cfg = parse_and_validate(&format!(
            "dark_mode = true\nallow_insecure = [\"10.0.0.1\"]\n{VALID}"
        ))
        .unwrap();
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
        let cfg = parse_and_validate(VALID).unwrap();
        let views = cfg.windows[0].tab_views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].title, "Gmail");
        assert_ne!(views[0].label, views[1].label);
        assert!(views[1].always_load);
        assert_eq!(views[1].reload_every, Some(15));
    }

    #[test]
    fn startup_label_resolves_per_window() {
        let cfg = parse_and_validate(VALID).unwrap();
        assert_eq!(cfg.windows[0].startup_label(), None);
        let cfg = parse_and_validate(&with_window_keys("Comms", "open_on_launch = true")).unwrap();
        assert_eq!(
            cfg.windows[0].startup_label(),
            Some(cfg.windows[0].tab_views()[0].label.clone())
        );

        // named tab → that tab
        let named = "[[window]]\ntitle = \"Comms\"\nopen_on_launch = \"Calendar\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n[[window.group.tab]]\ntitle = \"Calendar\"\nurl = \"https://calendar.google.com/\"\n";
        let cfg = parse_and_validate(named).unwrap();
        let cal = cfg.windows[0]
            .tab_views()
            .into_iter()
            .find(|v| v.title == "Calendar")
            .unwrap();
        assert_eq!(cfg.windows[0].startup_label(), Some(cal.label));

        // unknown name → falls back to first
        let unknown = "[[window]]\ntitle = \"Comms\"\nopen_on_launch = \"Nope\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n[[window.group.tab]]\ntitle = \"Calendar\"\nurl = \"https://calendar.google.com/\"\n";
        let cfg = parse_and_validate(unknown).unwrap();
        assert_eq!(
            cfg.windows[0].startup_label(),
            Some(cfg.windows[0].tab_views()[0].label.clone())
        );
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
        let base = parse_and_validate(VALID).unwrap();
        let gmail_label = base.windows[0]
            .tab_views()
            .into_iter()
            .find(|t| t.url == "https://mail.google.com/")
            .unwrap()
            .label;
        // Insert a new tab ahead of Gmail in the same window; Gmail's label must not move.
        let src = "[[window]]\ntitle = \"Comms\"\n[[window.group]]\nname = \"New\"\n[[window.group.tab]]\ntitle = \"X\"\nurl = \"https://x.test/\"\n[[window.group]]\nname = \"Chat\"\n[[window.group.tab]]\ntitle = \"Gmail\"\nurl = \"https://mail.google.com/\"\n";
        let inserted = parse_and_validate(src).unwrap();
        let gmail = inserted.windows[0]
            .tab_views()
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
        let cfg = parse_and_validate(src).unwrap();
        let views = cfg.windows[0].tab_views();
        assert_ne!(views[0].label, views[1].label);
    }

    #[test]
    fn tab_labels_are_window_namespaced() {
        let cfg = parse_and_validate(VALID).unwrap();
        let wid = crate::identity::window_id("Comms");
        assert!(cfg.windows[0].tab_views()[0]
            .label
            .starts_with(&format!("{wid}:")));
    }

    #[test]
    fn bundled_example_config_parses() {
        let src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/config.toml"
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
        let views = parse_and_validate(src).unwrap().windows[0].tab_views();
        assert_eq!(views[0].session, "win");
        assert_eq!(views[1].session, "tabown");

        // Neither tab nor window sets a session → the shared app-wide default.
        let bare = "[[window]]\ntitle = \"X\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n";
        assert_eq!(
            parse_and_validate(bare).unwrap().windows[0].tab_views()[0].session,
            DEFAULT_SESSION
        );

        // A blank session is treated as unset and falls through to the default.
        let blank = "[[window]]\ntitle = \"Y\"\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\nsession = \"  \"\n";
        assert_eq!(
            parse_and_validate(blank).unwrap().windows[0].tab_views()[0].session,
            DEFAULT_SESSION
        );
    }

    // Test helpers: build a one-window config with the given extra window-level keys.
    fn with_window_keys(title: &str, keys: &str) -> String {
        format!(
            "[[window]]\ntitle = \"{title}\"\n{keys}\n[[window.group]]\nname = \"G\"\n[[window.group.tab]]\ntitle = \"T\"\nurl = \"https://x.test/\"\n"
        )
    }
}
