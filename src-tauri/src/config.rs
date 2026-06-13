use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Config {
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

impl Config {
    /// Flatten groups → ordered tabs with stable `tab-<index>` labels.
    pub fn tab_views(&self) -> Vec<TabView> {
        let mut views = Vec::new();
        let mut idx = 0usize;
        for group in &self.groups {
            for tab in &group.tabs {
                views.push(TabView {
                    label: format!("tab-{idx}"),
                    group: group.name.clone(),
                    title: tab.title.clone(),
                    url: tab.url.clone(),
                    always_load: tab.always_load,
                    reload_every: tab.reload_every,
                });
                idx += 1;
            }
        }
        views
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
        assert_eq!(gmail.always_load, false);
        assert_eq!(gmail.reload_every, None);
        let cal = &cfg.groups[0].tabs[1];
        assert_eq!(cal.always_load, true);
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
    fn flattens_to_ordered_tabviews_with_stable_labels() {
        let cfg: Config = toml::from_str(VALID).unwrap();
        let views = cfg.tab_views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].label, "tab-0");
        assert_eq!(views[0].group, "Comms");
        assert_eq!(views[0].title, "Gmail");
        assert_eq!(views[1].label, "tab-1");
        assert_eq!(views[1].title, "Calendar");
        assert_eq!(views[1].always_load, true);
        assert_eq!(views[1].reload_every, Some(15));
    }
}
