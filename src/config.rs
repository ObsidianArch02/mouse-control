use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use crate::i18n::{self, tf};

static CONFIG_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub logging: LoggingSection,
    pub i18n: I18nSection,
    pub mouse: MouseSection,
}

pub(crate) const MAX_LINE_COUNT: u32 = 20;

pub(crate) const MAX_LINE_HEIGHT: u32 = 100;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingSection {

    pub level: String,

    pub file_path: String,
}

impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            file_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct I18nSection {

    pub locale: String,
}

impl Default for I18nSection {
    fn default() -> Self {
        Self {
            locale: "auto".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DeviceMatcher {

    #[serde(rename = "device_vendor_id", skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u32>,
    #[serde(rename = "device_product_id", skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PartialPointerSection {
    pub disable_acceleration: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MouseProfile {
    #[serde(flatten)]
    pub device: DeviceMatcher,

    #[serde(
        rename = "app_bundle_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub app_bundle_id: Option<String>,
    #[serde(rename = "app_name", default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,

    pub reverse_scroll: Option<bool>,
    pub scroll_mode: Option<String>,

    pub line_count: Option<u32>,

    pub line_height: Option<u32>,
    pub pointer: Option<PartialPointerSection>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_map: Option<BTreeMap<String, String>>,
}

impl MouseProfile {

    fn default_profile() -> Self {
        Self {
            reverse_scroll: Some(true),
            scroll_mode: Some("line".into()),
            line_count: Some(3),
            button_map: Some(BTreeMap::from([
                ("3".to_string(), "ctrl+left".to_string()),
                ("4".to_string(), "ctrl+right".to_string()),
            ])),
            pointer: Some(PartialPointerSection {
                disable_acceleration: Some(true),
            }),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MouseSection {

    pub enabled: bool,

    pub profiles: Vec<MouseProfile>,
}

impl Default for MouseSection {
    fn default() -> Self {
        Self {
            enabled: true,
            profiles: vec![MouseProfile::default_profile()],
        }
    }
}

impl Config {

    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();

        if !["auto", "en", "zh-Hans", "zh-Hant"].contains(&self.i18n.locale.as_str()) {
            errs.push(tf(
                "errors.i18n_locale_invalid",
                &[("value", &self.i18n.locale)],
            ));
        }

        if !["debug", "info"].contains(&self.logging.level.as_str()) {
            errs.push(tf(
                "errors.logging_level_invalid",
                &[("value", &self.logging.level)],
            ));
        }

        for (i, p) in self.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            if let Some(ref mode) = p.scroll_mode
                && !["default", "line", "pixel", "discrete", "continuous"].contains(&mode.as_str())
            {
                errs.push(tf("errors.mouse_scroll_mode_invalid", &[("value", mode)]));

                if let Some(last) = errs.last_mut() {
                    *last = format!("{prefix}.scroll_mode: {last}");
                }
            }
            if let Some(lc) = p.line_count
                && !(1..=MAX_LINE_COUNT).contains(&lc)
            {
                let msg = tf(
                    "errors.mouse_line_count_invalid",
                    &[("value", &lc.to_string())],
                );
                errs.push(format!("{prefix}.line_count: {msg}"));
            }

            if let Some(lh) = p.line_height
                && !(1..=MAX_LINE_HEIGHT).contains(&lh)
            {
                errs.push(format!(
                    "{prefix}.line_height: {} must be 1..={MAX_LINE_HEIGHT}",
                    lh
                ));
            }

            if let Some(ref map) = p.button_map {
                for (btn, action) in map {
                    if btn != "3" && btn != "4" {
                        continue;
                    }
                    if crate::mouse::buttons::parse_action(action).is_none() {
                        let msg = tf("errors.mouse_button_map_invalid", &[("value", action)]);
                        errs.push(format!("{prefix}.button_map.{btn}: {msg}"));
                    }
                }
            }
        }

        errs
    }

    pub fn merge_valid(&mut self, other: Config, errs: &[String]) {
        let has_error = |prefix: &str| errs.iter().any(|e| e.starts_with(prefix));

        if !has_error("logging.level") {
            self.logging.level = other.logging.level;
        }
        self.logging.file_path = other.logging.file_path;

        if !has_error("i18n.locale") {
            self.i18n.locale = other.i18n.locale;
        }

        self.mouse.enabled = other.mouse.enabled;
        let mut profiles = Vec::new();
        for (i, p) in other.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");

            let button_map_prefix = format!("{prefix}.button_map");
            let has_profile_error = errs
                .iter()
                .any(|e| e.starts_with(&prefix) && !e.starts_with(&button_map_prefix));
            if has_profile_error {
                continue;
            }
            profiles.push(sanitize_button_map(p));
        }
        if profiles.is_empty() {
            profiles.push(MouseProfile::default_profile());
        }
        self.mouse.profiles = profiles;
    }
}

fn sanitize_button_map(p: &MouseProfile) -> MouseProfile {
    let mut p = p.clone();
    let Some(ref map) = p.button_map else {
        return p;
    };
    let mut clean = BTreeMap::new();
    for (btn, action) in map {
        let valid =
            (btn == "3" || btn == "4") && crate::mouse::buttons::parse_action(action).is_some();
        if valid {
            clean.insert(btn.clone(), action.clone());
        }
    }
    p.button_map = if clean.is_empty() { None } else { Some(clean) };
    p
}

pub fn set_config_path_override(path: PathBuf) {
    let _ = CONFIG_PATH_OVERRIDE.set(path);
}

fn resolve_config_path_from(
    override_path: Option<&PathBuf>,
    xdg_config_home: Option<&str>,
    home: &str,
) -> PathBuf {
    if let Some(path) = override_path {
        return path.clone();
    }
    if let Some(xdg) = xdg_config_home.filter(|s| !s.is_empty()) {
        return PathBuf::from(xdg).join("mouse-control").join("config.toml");
    }
    PathBuf::from(home)
        .join(".config")
        .join("mouse-control")
        .join("config.toml")
}

pub fn resolved_config_path() -> PathBuf {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    resolve_config_path_from(CONFIG_PATH_OVERRIDE.get(), xdg.as_deref(), &home)
}

fn config_path() -> PathBuf {
    let path = resolved_config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    path
}

fn parse_and_sanitize_toml(content: &str) -> Result<(Config, Vec<String>, bool), String> {
    let loaded: Config = toml::from_str(content).map_err(|e| e.to_string())?;
    let errs = loaded.validate();
    if errs.is_empty() {
        Ok((loaded, Vec::new(), false))
    } else {
        let mut merged = Config::default();
        merged.merge_valid(loaded, &errs);
        Ok((merged, errs, true))
    }
}

fn apply_runtime_settings(cfg: &Config) {

    i18n::apply_config_locale(&cfg.i18n.locale);

    let lvl = match cfg.logging.level.as_str() {
        "debug" => crate::logger::LogLevel::Debug,
        _ => crate::logger::LogLevel::Info,
    };
    crate::logger::reconfigure(lvl);
}

impl Config {

    pub fn save(&self) -> Result<(), String> {
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| format!("serialize config: {}", e))?;
        let path = config_path();
        std::fs::write(&path, toml_str).map_err(|e| format!("write {}: {}", path.display(), e))?;
        Ok(())
    }

    pub fn load_or_default() -> (Self, Vec<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => match parse_and_sanitize_toml(&content) {
                Ok((cfg, errs, needs_save)) => {
                    if needs_save {
                        let _ = cfg.save();
                    }
                    (cfg, errs)
                }
                Err(parse_err) => (Config::default(), vec![format!("parse config: {parse_err}")]),
            },
            Err(_) => {

                let defaults = Config::default();
                if let Ok(toml_str) = toml::to_string_pretty(&defaults) {
                    let _ = std::fs::write(&path, toml_str);
                }
                (defaults, Vec::new())
            }
        }
    }

    pub fn reload() -> (Self, Vec<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => match parse_and_sanitize_toml(&content) {
                Ok((cfg, errs, needs_save)) => {
                    if needs_save {
                        let _ = cfg.save();
                    }
                    (cfg, errs)
                }
                Err(parse_err) => (Config::default(), vec![format!("parse config: {parse_err}")]),
            },
            Err(e) => {
                let defaults = Config::default();
                (
                    defaults,
                    vec![tf(
                        "errors.config_read_failed",
                        &[("error", &e.to_string())],
                    )],
                )
            }
        }
    }
}

pub static CONFIG: std::sync::LazyLock<RwLock<Config>> = std::sync::LazyLock::new(|| {
    let (cfg, _errs) = Config::load_or_default();

    i18n::apply_config_locale(&cfg.i18n.locale);
    RwLock::new(cfg)
});

pub fn reload_config() -> Vec<String> {
    let (new_cfg, errs) = Config::reload();
    apply_runtime_settings(&new_cfg);
    if let Ok(mut cfg) = CONFIG.write() {
        *cfg = new_cfg;
    }
    errs
}

pub fn set_enabled_and_persist(enabled: bool) -> Vec<String> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => match parse_and_sanitize_toml(&content) {
            Ok((mut new_cfg, errs, _)) => {
                new_cfg.mouse.enabled = enabled;
                apply_runtime_settings(&new_cfg);
                if let Ok(mut cfg) = CONFIG.write() {
                    *cfg = new_cfg.clone();
                }
                let _ = new_cfg.save();
                errs
            }
            Err(parse_err) => {
                if let Ok(mut cfg) = CONFIG.write() {
                    cfg.mouse.enabled = enabled;
                }
                vec![format!("parse config: {parse_err}")]
            }
        },
        Err(_) => {
            let to_save = if let Ok(mut cfg) = CONFIG.write() {
                cfg.mouse.enabled = enabled;
                Some(cfg.clone())
            } else {
                None
            };
            if let Some(cfg) = to_save {
                let _ = cfg.save();
            }
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip_preserves_button_map() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            button_map: Some(BTreeMap::from([
                ("3".to_string(), "ctrl+left".to_string()),
                ("4".to_string(), "cmd+[".to_string()),
            ])),
            ..Default::default()
        });
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        let map = parsed.mouse.profiles[0].button_map.as_ref().unwrap();
        assert_eq!(map.get("3").map(String::as_str), Some("ctrl+left"));
        assert_eq!(map.get("4").map(String::as_str), Some("cmd+["));
    }

    #[test]
    fn toml_parse_hand_written_button_map() {
        let raw = r#"
[mouse]
enabled = false

[[mouse.profiles]]
reverse_scroll = false
button_map = { "3" = "ctrl+left", "4" = "cmd+[" }

[[mouse.profiles]]
device_vendor_id = 1133
device_product_id = 50475
button_map = { "4" = "cmd+]" }
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.mouse.profiles.len(), 2);
        let map0 = cfg.mouse.profiles[0].button_map.as_ref().unwrap();
        assert_eq!(map0.get("3").map(String::as_str), Some("ctrl+left"));
        let map1 = cfg.mouse.profiles[1].button_map.as_ref().unwrap();
        assert_eq!(map1.get("4").map(String::as_str), Some("cmd+]"));
    }

    #[test]
    fn toml_parse_and_roundtrip_preserves_app_fields() {
        let raw = r#"
[mouse]
enabled = false

[[mouse.profiles]]
reverse_scroll = true

[[mouse.profiles]]
app_bundle_id = "com.apple.Safari"
reverse_scroll = false

[[mouse.profiles]]
app_name = "Google Chrome"
device_vendor_id = 1133
device_product_id = 50475
line_count = 5
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.mouse.profiles.len(), 3);
        assert_eq!(
            cfg.mouse.profiles[1].app_bundle_id.as_deref(),
            Some("com.apple.Safari")
        );
        assert_eq!(
            cfg.mouse.profiles[2].app_name.as_deref(),
            Some("Google Chrome")
        );

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mouse.profiles.len(), 3);
        assert_eq!(
            parsed.mouse.profiles[1].app_bundle_id.as_deref(),
            Some("com.apple.Safari")
        );
        assert_eq!(
            parsed.mouse.profiles[2].app_name.as_deref(),
            Some("Google Chrome")
        );
    }

    #[test]
    fn line_count_15_is_valid() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.ScreenContinuity".into()),
            line_count: Some(15),
            ..Default::default()
        });

        let errs = cfg.validate();
        assert!(errs.is_empty());

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mouse.profiles[0].line_count, Some(15));
    }

    #[test]
    fn line_height_valid_range_and_toml_roundtrip() {
        let raw = r#"
[mouse]
enabled = false

[[mouse.profiles]]
line_count = 2
line_height = 24

[[mouse.profiles]]
app_bundle_id = "com.apple.Notes"
line_count = 2
line_height = 24
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_empty());
        assert_eq!(cfg.mouse.profiles[0].line_height, Some(24));

        let mut cfg_bounds = Config::default();
        cfg_bounds.mouse.profiles.clear();
        cfg_bounds.mouse.profiles.push(MouseProfile {
            line_height: Some(1),
            ..Default::default()
        });
        cfg_bounds.mouse.profiles.push(MouseProfile {
            line_height: Some(crate::config::MAX_LINE_HEIGHT),
            ..Default::default()
        });
        assert!(cfg_bounds.validate().is_empty());

        let mut cfg_absent = Config::default();
        cfg_absent.mouse.profiles.clear();
        cfg_absent.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });
        let toml_str = toml::to_string_pretty(&cfg_absent).unwrap();
        assert!(!toml_str.contains("line_height"));
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mouse.profiles[0].line_height, None);
    }

    #[test]
    fn line_height_out_of_range_drops_profile() {
        let raw = r#"
[mouse]
enabled = false

[[mouse.profiles]]
reverse_scroll = true
line_count = 2
line_height = 101
"#;
        let loaded: Config = toml::from_str(raw).unwrap();
        let errs = loaded.validate();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("mouse.profiles[0].line_height"));

        let mut merged = Config::default();
        merged.merge_valid(loaded, &errs);
        assert_eq!(merged.mouse.profiles.len(), 1);
        assert_eq!(merged.mouse.profiles[0].line_height, None);
        assert_eq!(merged.mouse.profiles[0].line_count, Some(3));
    }

    #[test]
    fn continuous_scroll_mode_is_valid_and_roundtrips() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.ScreenContinuity".into()),
            scroll_mode: Some("continuous".into()),
            ..Default::default()
        });

        let errs = cfg.validate();
        assert!(errs.is_empty());

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            parsed.mouse.profiles[0].scroll_mode.as_deref(),
            Some("continuous")
        );
    }

    #[test]
    fn invalid_scroll_mode_still_reports_error() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            scroll_mode: Some("smooth".into()),
            ..Default::default()
        });

        let errs = cfg.validate();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("mouse.profiles[0].scroll_mode"));
    }

    #[test]
    fn invalid_button_map_keeps_scroll_settings() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            line_count: Some(7),
            button_map: Some(BTreeMap::from([
                ("3".to_string(), "ctrl+left".to_string()),
                ("4".to_string(), "not a key".to_string()),
                ("9".to_string(), "whatever".to_string()),
            ])),
            ..Default::default()
        });
        let errs = cfg.validate();

        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("mouse.profiles[0].button_map.4"));

        let mut merged = Config::default();
        merged.merge_valid(cfg, &errs);

        assert_eq!(merged.mouse.profiles.len(), 1);
        assert_eq!(merged.mouse.profiles[0].reverse_scroll, Some(true));
        assert_eq!(merged.mouse.profiles[0].line_count, Some(7));
        let map = merged.mouse.profiles[0].button_map.as_ref().unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("3").map(String::as_str), Some("ctrl+left"));
    }

    #[test]
    fn default_profile_has_win_style_settings() {
        let cfg = Config::default();
        assert!(cfg.mouse.enabled);
        assert_eq!(cfg.mouse.profiles.len(), 1);
        let p = &cfg.mouse.profiles[0];
        assert_eq!(p.reverse_scroll, Some(true));
        assert_eq!(p.scroll_mode.as_deref(), Some("line"));
        assert_eq!(p.line_count, Some(3));
        assert_eq!(
            p.pointer.as_ref().and_then(|pt| pt.disable_acceleration),
            Some(true)
        );
        let map = p.button_map.as_ref().unwrap();
        assert_eq!(map.get("3").map(String::as_str), Some("ctrl+left"));
        assert_eq!(map.get("4").map(String::as_str), Some("ctrl+right"));
    }

    #[test]
    fn parse_and_sanitize_rejects_malformed_toml_without_silent_default() {
        let broken = "[mouse]\nenabled = true\n[[mouse.profiles\nline_count = ???\n";
        assert!(parse_and_sanitize_toml(broken).is_err());
    }

    #[test]
    fn parse_and_sanitize_preserves_per_app_line_count_profiles() {
        let raw = r#"
[logging]
level = "debug"
file_path = ""

[i18n]
locale = "auto"

[mouse]
enabled = true

[[mouse.profiles]]
reverse_scroll = true
scroll_mode = "line"
line_count = 3

[mouse.profiles.pointer]
disable_acceleration = true

[mouse.profiles.button_map]
3 = "ctrl+right"
4 = "ctrl+left"

[[mouse.profiles]]
app_bundle_id = "com.apple.ScreenContinuity"
scroll_mode = "continuous"

[[mouse.profiles]]
app_bundle_id = "net.kovidgoyal.kitty"
scroll_mode = "line"
line_count = 1

[[mouse.profiles]]
app_bundle_id = "com.mitchellh.ghostty"
scroll_mode = "line"
line_count = 1

[[mouse.profiles]]
app_bundle_id = "com.apple.Notes"
line_count = 2
line_height = 24
"#;
        let (cfg, errs, needs_save) = parse_and_sanitize_toml(raw).unwrap();
        assert!(errs.is_empty());
        assert!(!needs_save);
        assert_eq!(cfg.mouse.profiles.len(), 5);
        assert_eq!(cfg.mouse.profiles[2].line_count, Some(1));
        assert_eq!(cfg.mouse.profiles[3].line_count, Some(1));
        assert_eq!(cfg.mouse.profiles[4].line_count, Some(2));
        assert_eq!(cfg.mouse.profiles[4].line_height, Some(24));
    }

    #[test]
    fn pixel_and_discrete_scroll_modes_are_valid_and_roundtrip() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            scroll_mode: Some("pixel".into()),
            line_count: Some(3),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.figma.Desktop".into()),
            scroll_mode: Some("discrete".into()),
            line_count: Some(1),
            ..Default::default()
        });

        let errs = cfg.validate();
        assert!(errs.is_empty());

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            parsed.mouse.profiles[0].scroll_mode.as_deref(),
            Some("pixel")
        );
        assert_eq!(
            parsed.mouse.profiles[1].scroll_mode.as_deref(),
            Some("discrete")
        );
    }

    #[test]
    fn config_path_prefers_cli_override_then_xdg_then_home() {
        let cli = PathBuf::from("/custom/mouse.toml");
        assert_eq!(
            resolve_config_path_from(Some(&cli), Some("/xdg"), "/Users/alice"),
            PathBuf::from("/custom/mouse.toml")
        );
        assert_eq!(
            resolve_config_path_from(None, Some("/Users/alice/.xdg"), "/Users/alice"),
            PathBuf::from("/Users/alice/.xdg/mouse-control/config.toml")
        );
        assert_eq!(
            resolve_config_path_from(None, Some(""), "/Users/alice"),
            PathBuf::from("/Users/alice/.config/mouse-control/config.toml")
        );
        assert_eq!(
            resolve_config_path_from(None, None, "/Users/alice"),
            PathBuf::from("/Users/alice/.config/mouse-control/config.toml")
        );
    }
}
