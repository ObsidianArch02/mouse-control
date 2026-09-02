//! TOML 配置:只保留鼠标控制(mouse)、日志(logging)与语言(i18n)三段。
//! 沿用 per-field resilient 加载:validate() 收集错误,merge_valid() 保留合法字段、
//! 非法字段重置为默认值。加载/重载/保存均写 ~/.config/mouse-control/config.toml。
//!
//! TOML config: only the mouse control (mouse), logging, and i18n sections remain.
//! Keeps the per-field resilient load: validate() collects errors, merge_valid() keeps valid
//! fields and resets invalid ones to defaults. Load/reload/save target
//! ~/.config/mouse-control/config.toml.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::RwLock;

use crate::i18n::{self, tf};

// ========== Structs ==========

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub logging: LoggingSection,
    pub i18n: I18nSection,
    pub mouse: MouseSection,
}

/// Line 模式每格行数的上限。初始为 10,现按实际需要放宽到 20(例如 iPhone Mirroring 用 15)。
/// Maximum lines per notch in Line mode. Originally 10, now relaxed to 20 (e.g. 15 for
/// iPhone Mirroring).
pub(crate) const MAX_LINE_COUNT: u32 = 20;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingSection {
    // 日志级别:"debug" | "info";默认 "info"(常规档,不刷屏)。
    // Log level: "debug" | "info"; default "info".
    pub level: String,
    // 日志文件路径;空 = 默认路径 ~/Library/Logs/mouse-control/mouse-control-<时间戳>.log。
    // Log file path; empty = default (timestamped file under ~/Library/Logs/mouse-control/).
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
    // 语言:"auto" | "en" | "zh-Hans" | "zh-Hant";默认 auto(跟随系统)。
    // Locale: "auto" | "en" | "zh-Hans" | "zh-Hant"; default auto (follow system).
    pub locale: String,
}

impl Default for I18nSection {
    fn default() -> Self {
        Self {
            locale: "auto".to_string(),
        }
    }
}

/// 设备匹配器(None = 通配,即"所有鼠标")。配置按 VID+PID 匹配设备。
/// Device matcher (None = wildcard, i.e. "All Mice"). Config matches devices by VID+PID.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DeviceMatcher {
    // 扁平序列化为 device_vendor_id / device_product_id(顶层标量,便于手写 TOML)。
    // Flattened as device_vendor_id / device_product_id (top-level scalars for hand-written TOML).
    #[serde(rename = "device_vendor_id", skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u32>,
    #[serde(rename = "device_product_id", skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u32>,
}

/// 指针覆盖(部分字段,None = 继承下层档)。
/// Pointer override (partial; None = inherit from the lower layer).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PartialPointerSection {
    pub disable_acceleration: Option<bool>,
}

/// 单个配置档。device 全空 + app 全空 = 全局"所有鼠标"档(默认层);其后可按设备、
/// app 或“设备 + app”加覆盖档,同一 profile 必须同时满足设备与 app 两个匹配条件。
/// A single profile. Empty device + empty app = the global "All Mice" profile (the default
/// layer); later profiles may narrow by device, app, or both. A profile matches only when both
/// its device and app matchers match.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MouseProfile {
    #[serde(flatten)]
    pub device: DeviceMatcher,
    // 前台 app 匹配器:两个字段都为空 = 匹配所有 app;只写 app_bundle_id 时精确匹配
    // NSRunningApplication.bundleIdentifier,只写 app_name 时精确匹配 localizedName,
    // 两个都写时任一个匹配即可。
    // Frontmost-app matcher: both empty = match every app; app_bundle_id alone matches
    // NSRunningApplication.bundleIdentifier exactly, app_name alone matches localizedName
    // exactly, and if both are present either match is sufficient.
    #[serde(
        rename = "app_bundle_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub app_bundle_id: Option<String>,
    #[serde(rename = "app_name", default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    // 反转滚动方向。true = 相对系统当前方向取反(HID tap 事件已含系统自然滚动翻转,
    // 合成事件不再被翻转,见 scrolling.rs 的 should_flip)。
    // Reverse scroll direction. true = flip relative to the system's current direction
    // (HID-tap events already carry the system natural-scroll flip; synthetic events are not
    // flipped again, see should_flip in scrolling.rs).
    pub reverse_scroll: Option<bool>,
    pub scroll_mode: Option<String>,
    // Line 模式每格行数(1..=MAX_LINE_COUNT)。
    // Line mode lines per notch (1..=MAX_LINE_COUNT).
    pub line_count: Option<u32>,
    pub pointer: Option<PartialPointerSection>,
    // 侧键映射:按钮号字符串 "3"/"4" -> 键盘动作串(如 "ctrl+left")。
    // 按住侧键 = 按下目标键,松开 = 释放;不自动重复。未写本字段时继承下层档,
    // 具体度更高的档对同一按钮覆盖前者,同具体度内后写者赢。
    // Side-button map: button-number string "3"/"4" -> keyboard action string
    // (e.g. "ctrl+left"). Holding the button presses the target, releasing releases it;
    // no auto-repeat. An absent field inherits the lower layer; more specific profiles
    // override the same button, and within the same specificity later profiles win.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_map: Option<BTreeMap<String, String>>,
}

impl MouseProfile {
    /// 默认"所有鼠标"档:Windows 风格反转、Line 模式每格 3 行、1:1 线性追踪,
    /// 侧键 3/4 映射为桌面左右切换(ctrl+left / ctrl+right)。
    /// The default "All Mice" profile: Windows-style reverse scrolling, Line mode at
    /// 3 lines per notch, 1:1 linear tracking, side buttons 3/4 mapped to desktop
    /// left/right (ctrl+left / ctrl+right).
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
    // 启用鼠标控制功能(总开关)。默认 true:首次运行即接管鼠标(需辅助功能权限)。
    // Enable mouse control (master switch). Default true: takes over the mouse on
    // first run (requires Accessibility permission).
    pub enabled: bool,
    // 配置档列表:第一个无 device/app 字段的档是全局"所有鼠标"默认层;其后可按设备、
    // app 或“设备 + app”加覆盖档。合并语义:先按具体度覆盖(全局 < 只限定设备或只
    // 限定 app < 同时限定设备 + app),同具体度内按文件顺序后者优先。
    // Profile list: the first profile without device/app fields is the global "All Mice" default
    // layer; later profiles may narrow by device, app, or both. Merge semantics: specificity
    // first (global < device-only or app-only < device + app), then file order within a tier.
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

// ========== Validation ==========

impl Config {
    /// 收集配置错误(非法字段不致命,merge_valid 会重置为默认)。
    /// Collect config errors (invalid fields are not fatal; merge_valid resets them).
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();

        // --- i18n ---
        if !["auto", "en", "zh-Hans", "zh-Hant"].contains(&self.i18n.locale.as_str()) {
            errs.push(tf(
                "errors.i18n_locale_invalid",
                &[("value", &self.i18n.locale)],
            ));
        }

        // --- logging ---
        if !["debug", "info"].contains(&self.logging.level.as_str()) {
            errs.push(tf(
                "errors.logging_level_invalid",
                &[("value", &self.logging.level)],
            ));
        }

        // --- mouse profiles ---
        for (i, p) in self.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            if let Some(ref mode) = p.scroll_mode {
                if !["default", "line", "continuous"].contains(&mode.as_str()) {
                    errs.push(tf("errors.mouse_scroll_mode_invalid", &[("value", mode)]));
                    // 用 prefix 区分哪个档出错,便于定位。
                    // Use the prefix to indicate which profile failed.
                    if let Some(last) = errs.last_mut() {
                        *last = format!("{prefix}.scroll_mode: {last}");
                    }
                }
            }
            if let Some(lc) = p.line_count {
                if !(1..=MAX_LINE_COUNT).contains(&lc) {
                    let msg = tf(
                        "errors.mouse_line_count_invalid",
                        &[("value", &lc.to_string())],
                    );
                    errs.push(format!("{prefix}.line_count: {msg}"));
                }
            }
            // button_map 逐条校验:非法 action 只报该条错误(不会因此丢弃整个档,
            // 避免连带丢失滚动配置);非 3/4 按钮号静默忽略。
            // Validate button_map per entry: an invalid action only reports that entry
            // (never drops the whole profile, so scroll settings survive); non-3/4 button
            // numbers are silently ignored.
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

    /// 把 `other` 中的合法字段并入 `self`(self 已含默认值),非法字段保持默认。
    /// Merge valid fields from `other` into `self`, keeping defaults for invalid fields.
    pub fn merge_valid(&mut self, other: Config, errs: &[String]) {
        let has_error = |prefix: &str| errs.iter().any(|e| e.starts_with(prefix));

        // logging:level 有校验;file_path 无校验,恒有效。
        // logging.level is validated; file_path has no validation, always valid.
        if !has_error("logging.level") {
            self.logging.level = other.logging.level;
        }
        self.logging.file_path = other.logging.file_path;

        // i18n:locale 有校验。
        // i18n.locale is validated.
        if !has_error("i18n.locale") {
            self.i18n.locale = other.i18n.locale;
        }

        // mouse:enabled 是 bool 无需校验;逐档校验,有错的档丢弃。
        // 若默认档也被丢弃,补回一个默认"所有鼠标"档。
        // mouse.enabled is an unchecked bool; profiles with errors are dropped.
        // If the default profile is dropped too, a fresh default "All Mice" profile is added.
        self.mouse.enabled = other.mouse.enabled;
        let mut profiles = Vec::new();
        for (i, p) in other.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            // 只有非 button_map 字段的错误才丢弃整个档;button_map 内部错误在
            // sanitize 时剔除对应条目即可。
            // Only errors on non-button_map fields drop the whole profile; button_map
            // errors just remove the offending entries during sanitize.
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

/// 剔除 button_map 中非 3/4 或解析失败的条目(保留合法条目,空结果置 None)。
/// Drop non-3/4 and unparseable button_map entries (keeping valid ones; None when empty).
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

// ========== Load / Save ==========

fn config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = std::path::PathBuf::from(home).join(".config/mouse-control");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("config.toml")
}

impl Config {
    /// 把当前配置序列化为 TOML 写回 `~/.config/mouse-control/config.toml`。
    /// Serialize this config to TOML and write it back to the config file.
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
            Ok(content) => {
                let loaded: Config = toml::from_str(&content).unwrap_or_default();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    // Start from defaults, merge only valid fields
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    let _ = merged.save();
                    (merged, errs)
                } else {
                    (loaded, Vec::new())
                }
            }
            Err(_) => {
                // File doesn't exist — write defaults
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
            Ok(content) => {
                let loaded: Config = toml::from_str(&content).unwrap_or_default();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    let _ = merged.save();
                    (merged, errs)
                } else {
                    (loaded, Vec::new())
                }
            }
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

// ========== Global singleton ==========

pub static CONFIG: std::sync::LazyLock<RwLock<Config>> = std::sync::LazyLock::new(|| {
    let (cfg, _errs) = Config::load_or_default();
    // 应用 config 里的 locale 覆盖(I18N 初始化时只用了系统语言)。
    // 无循环:I18N 不读 CONFIG,见 i18n.rs 文件头说明。
    // Apply the locale from config (I18N init only used the system locale).
    // No cycle: I18N does not read CONFIG; see the note at the top of i18n.rs.
    i18n::apply_config_locale(&cfg.i18n.locale);
    RwLock::new(cfg)
});

/// Reload config from disk and apply. Returns validation errors (empty = success).
pub fn reload_config() -> Vec<String> {
    let (new_cfg, errs) = Config::reload();
    let locale = new_cfg.i18n.locale.clone();
    let log_level = new_cfg.logging.level.clone();
    if let Ok(mut cfg) = CONFIG.write() {
        *cfg = new_cfg;
    }
    // locale 可能随 reload 改变,重新应用 / locale may change on reload, re-apply
    i18n::apply_config_locale(&locale);
    // 热更新日志级别 / hot-reload log level
    let lvl = match log_level.as_str() {
        "debug" => crate::logger::LogLevel::Debug,
        _ => crate::logger::LogLevel::Info,
    };
    crate::logger::reconfigure(lvl);
    errs
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
        // 只有一条错误(按钮 4);非 3/4 的 9 静默忽略。
        // Exactly one error (button 4); non-3/4 button 9 is silently ignored.
        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("mouse.profiles[0].button_map.4"));

        let mut merged = Config::default();
        merged.merge_valid(cfg, &errs);
        // 档整体保留,滚动字段不丢;非法/非 3/4 条目被剔除。
        // The profile survives with scroll settings intact; invalid/non-3/4 entries are
        // dropped.
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
}
