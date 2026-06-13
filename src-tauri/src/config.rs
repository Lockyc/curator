use serde::Deserialize;

/// What to open when the app launches. `false` (default) → blank; `true` → the first tab;
/// a string → the tab whose `title` matches (falling back to the first tab if none match).
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
    #[serde(default)]
    pub open_on_launch: OpenOnLaunch,
    #[serde(default, rename = "group")]
    pub groups: Vec<Group>,
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
}

use std::path::Path;

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    EmptyField(&'static str),
    InvalidUrl { title: String, url: String },
    ZeroReload(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "cannot read config: {e}"),
            ConfigError::Parse(e) => write!(f, "invalid TOML: {e}"),
            ConfigError::EmptyField(field) => write!(f, "empty {field}"),
            ConfigError::InvalidUrl { title, url } => {
                write!(f, "tab \"{title}\" has invalid url: {url}")
            }
            ConfigError::ZeroReload(title) => {
                write!(f, "tab \"{title}\" reload_every must be > 0")
            }
        }
    }
}

pub fn parse_and_validate(src: &str) -> Result<Config, ConfigError> {
    let cfg: Config = toml::from_str(src).map_err(ConfigError::Parse)?;
    for group in &cfg.groups {
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
    Ok(cfg)
}

pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let src = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
    parse_and_validate(&src)
}

/// Default config location: `~/.config/curator/tabs.toml`.
///
/// Deliberately `~/.config` (not `dirs::config_dir()`, which on macOS is
/// `~/Library/Application Support`) so the config slots into the dotfiles bare-repo workflow.
pub fn default_config_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".config")
        .join("curator")
        .join("tabs.toml")
}

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TabView {
    pub label: String,
    pub group: String,
    pub title: String,
    pub url: String,
    pub always_load: bool,
    pub reload_every: Option<u64>,
}

/// Stable webview label derived from a tab's URL. Position-independent so that inserting,
/// removing, or reordering tabs in the config does not remap an already-created content
/// webview onto a different tab (the bug index-based labels caused). Deterministic across
/// runs (`DefaultHasher` uses fixed keys).
fn url_label(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("tab-{:016x}", h.finish())
}

impl Config {
    /// Flatten groups → ordered tabs with stable, URL-derived labels. Render order is file
    /// order. Tabs sharing a URL get a deterministic `-N` suffix so their labels stay unique.
    pub fn tab_views(&self) -> Vec<TabView> {
        let mut views = Vec::new();
        let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for group in &self.groups {
            for tab in &group.tabs {
                let base = url_label(&tab.url);
                let n = seen.entry(base.clone()).or_insert(0);
                let label = if *n == 0 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                };
                *n += 1;
                views.push(TabView {
                    label,
                    group: group.name.clone(),
                    title: tab.title.clone(),
                    url: tab.url.clone(),
                    always_load: tab.always_load,
                    reload_every: tab.reload_every,
                });
            }
        }
        views
    }

    /// Label of the tab to open on launch (per `open_on_launch`). `None` = blank screen.
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
[[group]]
name = "Comms"
[[group.tab]]
title = "Gmail"
url = "https://mail.google.com/"
[[group.tab]]
title = "Calendar"
url = "https://calendar.google.com/"
always_load = true
reload_every = 15
"#;

    #[test]
    fn parses_groups_and_tabs_in_order() {
        let cfg: Config = toml::from_str(VALID).unwrap();
        assert_eq!(cfg.groups.len(), 1);
        assert_eq!(cfg.groups[0].name, "Comms");
        assert_eq!(cfg.groups[0].tabs.len(), 2);
        assert_eq!(cfg.groups[0].tabs[0].title, "Gmail");
        assert_eq!(cfg.groups[0].tabs[1].title, "Calendar");
    }

    #[test]
    fn optional_fields_default() {
        let cfg: Config = toml::from_str(VALID).unwrap();
        let gmail = &cfg.groups[0].tabs[0];
        assert!(!gmail.always_load);
        assert_eq!(gmail.reload_every, None);
        let cal = &cfg.groups[0].tabs[1];
        assert!(cal.always_load);
        assert_eq!(cal.reload_every, Some(15));
    }

    #[test]
    fn rejects_empty_title() {
        let src = r#"
[[group]]
name = "G"
[[group.tab]]
title = ""
url = "https://x.test/"
"#;
        let err = parse_and_validate(src).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyField("title")));
    }

    #[test]
    fn rejects_invalid_url() {
        let src = r#"
[[group]]
name = "G"
[[group.tab]]
title = "Bad"
url = "not a url"
"#;
        let err = parse_and_validate(src).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidUrl { .. }));
    }

    #[test]
    fn rejects_zero_reload_every() {
        let src = r#"
[[group]]
name = "G"
[[group.tab]]
title = "T"
url = "https://x.test/"
reload_every = 0
"#;
        let err = parse_and_validate(src).unwrap_err();
        assert!(matches!(err, ConfigError::ZeroReload(_)));
    }

    #[test]
    fn malformed_toml_is_parse_error() {
        let err = parse_and_validate("this is not toml = =").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn load_missing_file_errors() {
        let err = load_config(std::path::Path::new("/no/such/curator.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io(_)));
    }

    #[test]
    fn flattens_to_ordered_tabviews() {
        let cfg: Config = toml::from_str(VALID).unwrap();
        let views = cfg.tab_views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].group, "Comms");
        assert_eq!(views[0].title, "Gmail");
        assert_eq!(views[1].title, "Calendar");
        assert!(views[1].always_load);
        assert_eq!(views[1].reload_every, Some(15));
        // Distinct URLs → distinct labels; labels are deterministic across calls.
        assert_ne!(views[0].label, views[1].label);
        assert_eq!(
            cfg.tab_views()
                .iter()
                .map(|t| t.label.clone())
                .collect::<Vec<_>>(),
            views.iter().map(|t| t.label.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn label_is_stable_when_a_tab_is_inserted_before_it() {
        let base: Config = toml::from_str(VALID).unwrap();
        let gmail_label = base.tab_views()[0].label.clone();
        // Prepend a whole new group/tab ahead of everything.
        let inserted: Config = toml::from_str(&format!(
            "[[group]]\nname = \"New\"\n[[group.tab]]\ntitle = \"X\"\nurl = \"https://x.test/\"\n{VALID}"
        ))
        .unwrap();
        let gmail = inserted
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
        let src = r#"
[[group]]
name = "G"
[[group.tab]]
title = "A"
url = "https://same.test/"
[[group.tab]]
title = "B"
url = "https://same.test/"
"#;
        let cfg: Config = toml::from_str(src).unwrap();
        let views = cfg.tab_views();
        assert_ne!(views[0].label, views[1].label);
    }

    #[test]
    fn open_on_launch_resolves_startup_label() {
        // absent → blank
        let cfg: Config = toml::from_str(VALID).unwrap();
        assert_eq!(cfg.startup_label(), None);

        // true → first tab
        let cfg: Config = toml::from_str(&format!("open_on_launch = true\n{VALID}")).unwrap();
        assert_eq!(cfg.startup_label(), Some(cfg.tab_views()[0].label.clone()));

        // named tab → that tab
        let cfg: Config =
            toml::from_str(&format!("open_on_launch = \"Calendar\"\n{VALID}")).unwrap();
        let cal = cfg
            .tab_views()
            .into_iter()
            .find(|v| v.title == "Calendar")
            .unwrap();
        assert_eq!(cfg.startup_label(), Some(cal.label));

        // unknown name → falls back to first
        let cfg: Config = toml::from_str(&format!("open_on_launch = \"Nope\"\n{VALID}")).unwrap();
        assert_eq!(cfg.startup_label(), Some(cfg.tab_views()[0].label.clone()));
    }
}
