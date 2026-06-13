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
}
