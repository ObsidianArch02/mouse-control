//! 配置解析引擎:把全局"所有鼠标"档 + per-device 档 + per-app 档合并成具体生效配置。
//!
//! 合并语义:只合并设备匹配且前台 app 匹配的档;设备匹配沿用 VID+PID 通配,app 匹配用
//! app_bundle_id / app_name(全空 = 通配)。具体度高的档覆盖具体度低的档:全局(0) <
//! 只限定设备或只限定 app(1) < 同时限定设备 + app(2);同具体度内按文件顺序后者优先。
//!
//! Config resolution: merge the global "All Mice" profile + per-device + per-app profiles into
//! the effective config. Only profiles matching both device and frontmost-app participate.
//! Only profiles matching both device and app participate. Device matching keeps VID+PID
//! wildcard semantics; app matching uses app_bundle_id/app_name (both empty = wildcard).
//! More specific profiles override less specific ones: global (0) < device-only or app-only
//! (1) < device + app (2); within the same level, later profiles win.

use crate::app::AppIdentity;
use crate::config::{Config, MouseProfile, CONFIG};
use crate::mouse::buttons::{self, PressedKey};
use crate::mouse::device::DeviceKey;
use crate::mouse::scrolling::ScrollMode;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

/// 解析后的具体生效配置(非 Option,所有字段已定)。
/// Resolved effective config (non-Option; all fields are concrete).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedMouse {
    pub reverse_scroll: bool,
    pub scroll_mode: ScrollMode,
    pub line_count: u32,
    /// Line 模式一行对应的像素量;None = 系统行→像素换算(发 line 单位事件)。Some 时
    /// Line 模式改发像素单位事件,delta = line_count x line_height。
    /// Pixel height of one line in Line mode; None = the system's line-to-pixel conversion
    /// (line-unit events). When set, Line mode posts pixel-unit events with
    /// delta = line_count x line_height.
    pub line_height: Option<u32>,
    pub disable_acceleration: bool,
    /// 侧键按钮号(3/4)-> 已解析动作。命中即替换原侧键事件,未命中透传。
    /// Side-button number (3/4) -> parsed action. A hit replaces the original side-button
    /// event; a miss passes the event through untouched.
    pub button_map: BTreeMap<u32, PressedKey>,
}

impl Default for ResolvedMouse {
    fn default() -> Self {
        Self {
            reverse_scroll: true,
            scroll_mode: ScrollMode::Line,
            line_count: 3,
            line_height: None,
            disable_acceleration: true,
            // button_map 由配置档合并而来,兜底为空;默认的 3/4 映射由"所有鼠标"档提供。
            // button_map comes from merging config profiles; the fallback is empty (the
            // default 3/4 mappings live in the "All Mice" profile).
            button_map: BTreeMap::new(),
        }
    }
}

/// 解析缓存:key = (设备, 前台 app)。设备 None = "无设备/所有鼠标",app None = "无前台 app";
/// 只有两者完全相同时才复用缓存。在 reload_config / app 切换时失效。
/// Resolve cache: key = (device, frontmost app). Device None = "no device / All Mice",
/// app None = "no frontmost app"; only identical pairs share a cache entry. Invalidated on
/// reload_config and app switches.
type ResolveCacheKey = (Option<DeviceKey>, Option<AppIdentity>);

static CACHE: std::sync::LazyLock<
    Mutex<std::collections::HashMap<ResolveCacheKey, ResolvedMouse>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

const FLAG_COMPUTED: u8 = 1 << 0;
const FLAG_APP_SCROLL: u8 = 1 << 1;
const FLAG_APP_POINTER: u8 = 1 << 2;

static APP_PROFILE_FLAGS: AtomicU8 = AtomicU8::new(0);

/// 使缓存失效(配置重载或设备变更时调用)。
/// Invalidate the cache (called on config reload or device changes).
pub(crate) fn invalidate_cache() {
    APP_PROFILE_FLAGS.store(0, Ordering::Release);
    if let Ok(mut c) = CACHE.lock() {
        c.clear();
    }
}

fn app_profile_flags() -> u8 {
    let flags = APP_PROFILE_FLAGS.load(Ordering::Acquire);
    if flags & FLAG_COMPUTED != 0 {
        return flags;
    }
    let cfg = CONFIG.read().unwrap();
    let mut computed = FLAG_COMPUTED;
    for p in &cfg.mouse.profiles {
        if p.app_bundle_id.is_none() && p.app_name.is_none() {
            continue;
        }
        if p.reverse_scroll.is_some()
            || p.scroll_mode.is_some()
            || p.line_count.is_some()
            || p.line_height.is_some()
        {
            computed |= FLAG_APP_SCROLL;
        }
        if p.pointer
            .as_ref()
            .and_then(|ptr| ptr.disable_acceleration)
            .is_some()
        {
            computed |= FLAG_APP_POINTER;
        }
    }
    APP_PROFILE_FLAGS.store(computed, Ordering::Release);
    computed
}

pub(crate) fn has_per_app_scroll_profiles() -> bool {
    app_profile_flags() & FLAG_APP_SCROLL != 0
}

pub(crate) fn has_per_app_pointer_profiles() -> bool {
    app_profile_flags() & FLAG_APP_POINTER != 0
}

/// 匹配器是否匹配给定设备。None 键 = 通配(匹配所有设备,即"所有鼠标"档)。
/// Whether a matcher matches the given device. A None key = wildcard (matches all devices,
/// i.e. the "All Mice" profile).
fn device_matches(profile: &MouseProfile, device: Option<DeviceKey>) -> bool {
    let Some((vid, pid)) = device else {
        // 无设备(归因失败回退):只匹配通配档。
        // No device (attribution-failure fallback): match only wildcard profiles.
        return profile.device.vendor_id.is_none() && profile.device.product_id.is_none();
    };
    let vid_ok = profile.device.vendor_id.map(|v| v == vid).unwrap_or(true);
    let pid_ok = profile.device.product_id.map(|p| p == pid).unwrap_or(true);
    vid_ok && pid_ok
}

fn app_matches(profile: &MouseProfile, app: Option<&AppIdentity>) -> bool {
    let has_app_filter = profile.app_bundle_id.is_some() || profile.app_name.is_some();
    if !has_app_filter {
        return true;
    }

    let Some(active) = app else {
        // 无前台 app 信息时只命中无 app 字段的档。
        // Without a frontmost-app identity, only profiles without an app filter match.
        return false;
    };

    let bundle_matches = profile
        .app_bundle_id
        .as_ref()
        .map(|expected| active.bundle_id.as_ref() == Some(expected))
        .unwrap_or(false);
    let name_matches = profile
        .app_name
        .as_ref()
        .map(|expected| active.localized_name.as_ref() == Some(expected))
        .unwrap_or(false);

    // 两个字段都写时任一个匹配即可;只写一个时按该字段判断。
    // If both fields are present, either may match; if only one is present, use that one.
    match (profile.app_bundle_id.is_some(), profile.app_name.is_some()) {
        (true, true) => bundle_matches || name_matches,
        (true, false) => bundle_matches,
        (false, true) => name_matches,
        (false, false) => true,
    }
}

fn matches(profile: &MouseProfile, device: Option<DeviceKey>, app: Option<&AppIdentity>) -> bool {
    device_matches(profile, device) && app_matches(profile, app)
}

/// 配置档具体度:0 = 全局,1 = 只限定设备或只限定 app,2 = 同时限定设备 + app。
/// 设备维度和 app 维度分别只算一次,不把 VID/PID/bundle_id/app_name 拆开计分。
/// Profile specificity: 0 = global, 1 = device-only or app-only, 2 = device + app.
/// The device and app dimensions each count once; VID/PID/bundle_id/app_name are not
/// scored individually.
fn profile_specificity(profile: &MouseProfile) -> u8 {
    let has_device = profile.device.vendor_id.is_some() || profile.device.product_id.is_some();
    let has_app = profile.app_bundle_id.is_some() || profile.app_name.is_some();
    (has_device as u8) + (has_app as u8)
}

/// 解析某设备 + 当前前台 app 的生效配置。device = None 表示归因失败;app = None 表示
/// 前台 app 未知。
/// Resolve the effective config for a device plus the current frontmost app. device = None
/// means attribution failed; app = None means the frontmost app is unknown.
pub(crate) fn resolve(device: Option<DeviceKey>, app: Option<AppIdentity>) -> ResolvedMouse {
    let key = (device, app);

    // 查缓存。
    // Check the cache.
    if let Ok(c) = CACHE.lock() {
        if let Some(r) = c.get(&key) {
            return r.clone();
        }
    }

    let cfg = CONFIG.read().unwrap().clone();
    let r = resolve_from(&cfg, key.0, key.1.as_ref());

    if let Ok(mut c) = CACHE.lock() {
        c.insert(key, r.clone());
    }
    r
}

/// 从给定 Config 解析(供测试与无 CONFIG 的场景)。
/// Resolve from a given Config (for tests and CONFIG-free scenarios).
fn resolve_from(
    cfg: &Config,
    device: Option<DeviceKey>,
    app: Option<&AppIdentity>,
) -> ResolvedMouse {
    let mut r = ResolvedMouse::default();

    // 先用代码默认值兜底(确保所有字段有值)。
    // Start from code defaults so every field is concrete.
    let defaults = ResolvedMouse::default();
    r.reverse_scroll = defaults.reverse_scroll;
    r.scroll_mode = defaults.scroll_mode;
    r.line_count = defaults.line_count;
    r.line_height = defaults.line_height;
    r.disable_acceleration = defaults.disable_acceleration;
    r.button_map = defaults.button_map;

    // 先按具体度从低到高,再按文件顺序合并。高具体度的档后写入结果,因此会覆盖低层。
    // Merge by specificity (low to high), then file order. More specific profiles are written
    // later into the result, so they override lower layers.
    for specificity in 0..=2u8 {
        for p in &cfg.mouse.profiles {
            if profile_specificity(p) != specificity || !matches(p, device, app) {
                continue;
            }
            if let Some(rs) = p.reverse_scroll {
                r.reverse_scroll = rs;
            }
            if let Some(ref mode) = p.scroll_mode {
                r.scroll_mode = ScrollMode::from_str(mode);
            }
            if let Some(lc) = p.line_count {
                r.line_count = lc.clamp(1, crate::config::MAX_LINE_COUNT);
            }
            if let Some(lh) = p.line_height {
                r.line_height = Some(lh.clamp(1, crate::config::MAX_LINE_HEIGHT));
            }
            if let Some(ref ptr) = p.pointer {
                if let Some(da) = ptr.disable_acceleration {
                    r.disable_acceleration = da;
                }
            }
            // button_map 合并:后出现的档对同一按钮覆盖前者;某档未写 button_map 时
            // 不删除底层映射。解析失败的条目在 config 校验时已被剔除,这里防御性跳过。
            // Merge button_map: later profiles override the same button; a profile without
            // button_map never clears the lower layer. Unparseable entries are already dropped
            // by config validation; this is a defensive skip.
            if let Some(ref map) = p.button_map {
                for (btn, action) in map {
                    let Ok(button) = btn.parse::<u32>() else {
                        continue;
                    };
                    if button != 3 && button != 4 {
                        continue;
                    }
                    if let Some(key) = buttons::parse_action(action) {
                        r.button_map.insert(button, key);
                    }
                }
            }
        }
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PartialPointerSection;

    #[test]
    fn wildcard_only_falls_back_to_defaults() {
        let mut cfg = Config::default();
        // 默认配置含一个"所有鼠标"档,值为 Windows 风格默认。
        let r = resolve_from(&cfg, None, None);
        assert!(r.reverse_scroll);
        assert_eq!(r.scroll_mode, ScrollMode::Line);
        assert_eq!(r.line_count, 3);
        assert!(r.disable_acceleration);
        assert_eq!(r.button_map.len(), 2);
        let r2 = resolve_from(&cfg, Some((1133, 17492)), None);
        assert!(r2.reverse_scroll);
        let _ = &mut cfg;
    }

    #[test]
    fn per_device_overrides_wildcard() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // "所有鼠标"档:反转滚动开。
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });
        // 某设备档:反转滚动关。
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(17492),
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });

        // 匹配的设备:后者优先 -> 关。
        let r = resolve_from(&cfg, Some((1133, 17492)), None);
        assert!(!r.reverse_scroll);
        // 其他设备:只用通配档 -> 开。
        let r2 = resolve_from(&cfg, Some((1, 2)), None);
        assert!(r2.reverse_scroll);
        // 无设备(回退):只用通配档 -> 开。
        let r3 = resolve_from(&cfg, None, None);
        assert!(r3.reverse_scroll);
    }

    #[test]
    fn later_match_wins_on_merge() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // 两条通配档:后者覆盖前者。
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            line_count: Some(5),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(false),
            ..Default::default()
        });
        let r = resolve_from(&cfg, None, None);
        assert!(!r.reverse_scroll); // 后者胜
        assert_eq!(r.line_count, 5); // 前者字段保留(后者未设)
        let _ = &mut cfg;
    }

    #[test]
    fn toml_roundtrip_preserves_profiles() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            scroll_mode: Some("line".into()),
            line_count: Some(7),
            pointer: Some(PartialPointerSection {
                disable_acceleration: Some(true),
            }),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(0xC548),
                product_id: Some(0x4444),
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        // 反序列化后应保留两条档。
        // After deserialization, both profiles should survive.
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mouse.profiles.len(), 2);
        assert!(parsed.mouse.profiles[0].device.vendor_id.is_none());
        assert_eq!(parsed.mouse.profiles[1].device.vendor_id, Some(0xC548));
        assert_eq!(parsed.mouse.profiles[1].reverse_scroll, Some(false));
        // 解析结果应与原配置一致。
        // Resolution should match the original.
        let r = resolve_from(&parsed, Some((0xC548, 0x4444)), None);
        assert!(!r.reverse_scroll); // 设备档覆盖通配档
        assert_eq!(r.line_count, 7); // 来自通配档(line_count)
        assert!(r.disable_acceleration); // 来自通配档
    }

    #[test]
    fn button_map_merges_and_later_wins() {
        use crate::mouse::buttons::KeyAction;
        use crate::mouse::ffi::{K_CG_EVENT_FLAG_MASK_COMMAND, K_VK_ANSI_LEFT_BRACKET};

        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // 通配档:3 -> ctrl+left。
        // Wildcard profile: 3 -> ctrl+left.
        cfg.mouse.profiles.push(MouseProfile {
            button_map: Some(BTreeMap::from([("3".to_string(), "ctrl+left".to_string())])),
            ..Default::default()
        });
        // 设备档:3 覆盖为 cmd+left,新增 4 -> cmd+[。
        // Device profile: 3 overridden to cmd+left, plus 4 -> cmd+[.
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(50475),
            },
            button_map: Some(BTreeMap::from([
                ("3".to_string(), "cmd+left".to_string()),
                ("4".to_string(), "cmd+[".to_string()),
            ])),
            ..Default::default()
        });

        // 匹配设备:两条档合并,设备档胜出。
        // Matching device: both profiles merge, device wins.
        let r = resolve_from(&cfg, Some((1133, 50475)), None);
        assert_eq!(r.button_map.len(), 2);
        assert_eq!(r.button_map[&3].modifiers, K_CG_EVENT_FLAG_MASK_COMMAND);
        assert_eq!(
            r.button_map[&4].action,
            KeyAction::Keyboard {
                keycode: K_VK_ANSI_LEFT_BRACKET
            }
        );
        // 其他设备:只用通配档,只有 3。
        // Other devices: only the wildcard profile, only button 3.
        let r2 = resolve_from(&cfg, Some((1, 2)), None);
        assert_eq!(r2.button_map.len(), 1);
        assert!(r2.button_map.contains_key(&3));
        assert!(!r2.button_map.contains_key(&4));
    }

    #[test]
    fn line_height_merges_by_specificity_and_order() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // 缺省:None(沿用系统行→像素换算)。
        // Absent by default (the system's line-to-pixel conversion applies).
        let r0 = resolve_from(&cfg, None, None);
        assert_eq!(r0.line_height, None);

        cfg.mouse.profiles.push(MouseProfile {
            line_count: Some(2),
            line_height: Some(24),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Notes".into()),
            line_count: Some(3),
            line_height: Some(16),
            ..Default::default()
        });

        let notes = AppIdentity {
            bundle_id: Some("com.apple.Notes".into()),
            localized_name: Some("Notes".into()),
        };
        // app 档具体度更高,两个像素量字段都由它覆盖。
        // The app profile is more specific and overrides both fields.
        let r = resolve_from(&cfg, Some((1, 2)), Some(&notes));
        assert_eq!(r.line_height, Some(16));
        assert_eq!(r.line_count, 3);

        let other = AppIdentity {
            bundle_id: Some("com.google.Chrome".into()),
            localized_name: Some("Google Chrome".into()),
        };
        // 其他 app 只命中全局档。
        // Other apps only match the global profile.
        let r2 = resolve_from(&cfg, Some((1, 2)), Some(&other));
        assert_eq!(r2.line_height, Some(24));
        assert_eq!(r2.line_count, 2);
    }

    #[test]
    fn line_height_is_clamped_at_resolve() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            line_height: Some(500),
            ..Default::default()
        });
        let r = resolve_from(&cfg, None, None);
        assert_eq!(r.line_height, Some(crate::config::MAX_LINE_HEIGHT));
    }

    #[test]
    fn button_map_skips_invalid_and_non_3_4() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            button_map: Some(BTreeMap::from([
                ("3".to_string(), "ctrl+left".to_string()),
                ("5".to_string(), "cmd+x".to_string()),
                ("4".to_string(), "not-a-key".to_string()),
            ])),
            ..Default::default()
        });
        let r = resolve_from(&cfg, None, None);
        // 只有合法且属于 3/4 的条目生效。
        // Only valid 3/4 entries take effect.
        assert_eq!(r.button_map.len(), 1);
        assert!(r.button_map.contains_key(&3));
    }

    fn safari_identity() -> AppIdentity {
        AppIdentity {
            bundle_id: Some("com.apple.Safari".into()),
            localized_name: Some("Safari".into()),
        }
    }

    #[test]
    fn app_profile_overrides_wildcard() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            line_count: Some(3),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(false),
            line_count: Some(7),
            ..Default::default()
        });

        let safari = safari_identity();
        let r = resolve_from(&cfg, Some((1, 2)), Some(&safari));
        assert!(!r.reverse_scroll);
        assert_eq!(r.line_count, 7);

        let chrome = AppIdentity {
            bundle_id: Some("com.google.Chrome".into()),
            localized_name: Some("Google Chrome".into()),
        };
        let r2 = resolve_from(&cfg, Some((1, 2)), Some(&chrome));
        assert!(r2.reverse_scroll);
        assert_eq!(r2.line_count, 3);
    }

    #[test]
    fn app_profile_can_select_continuous_mode() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            scroll_mode: Some("line".into()),
            line_count: Some(3),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.ScreenContinuity".into()),
            scroll_mode: Some("continuous".into()),
            ..Default::default()
        });

        let mirroring = AppIdentity {
            bundle_id: Some("com.apple.ScreenContinuity".into()),
            localized_name: Some("iPhone Mirroring".into()),
        };
        let r = resolve_from(&cfg, Some((1, 2)), Some(&mirroring));
        assert_eq!(r.scroll_mode, ScrollMode::Continuous);
        // line_count 仍继承下层,但连续模式不会使用它。
        // line_count is still inherited, but continuous mode does not consume it.
        assert_eq!(r.line_count, 3);

        let safari = safari_identity();
        let r2 = resolve_from(&cfg, Some((1, 2)), Some(&safari));
        assert_eq!(r2.scroll_mode, ScrollMode::Line);
    }

    #[test]
    fn device_app_and_combined_profiles_merge_in_order() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            line_count: Some(3),
            pointer: Some(PartialPointerSection {
                disable_acceleration: Some(true),
            }),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(50475),
            },
            scroll_mode: Some("default".into()),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            line_count: Some(6),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(50475),
            },
            app_name: Some("Safari".into()),
            reverse_scroll: Some(false),
            line_count: Some(9),
            ..Default::default()
        });

        let safari = safari_identity();
        let r = resolve_from(&cfg, Some((1133, 50475)), Some(&safari));
        assert!(!r.reverse_scroll);
        assert_eq!(r.scroll_mode, ScrollMode::Default);
        assert_eq!(r.line_count, 9);
        assert!(r.disable_acceleration);

        let chrome = AppIdentity {
            bundle_id: Some("com.google.Chrome".into()),
            localized_name: Some("Google Chrome".into()),
        };
        let r2 = resolve_from(&cfg, Some((1133, 50475)), Some(&chrome));
        assert!(r2.reverse_scroll);
        assert_eq!(r2.scroll_mode, ScrollMode::Default);
        assert_eq!(r2.line_count, 3);

        let r3 = resolve_from(&cfg, Some((1, 2)), Some(&safari));
        assert!(r3.reverse_scroll);
        assert_eq!(r3.line_count, 6);
    }

    #[test]
    fn app_bundle_id_and_name_either_match_when_both_present() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.other.App".into()),
            app_name: Some("Safari".into()),
            reverse_scroll: Some(false),
            ..Default::default()
        });

        let safari = safari_identity();
        let r = resolve_from(&cfg, Some((1, 2)), Some(&safari));
        assert!(!r.reverse_scroll);

        let no_app = resolve_from(&cfg, Some((1, 2)), None);
        assert!(no_app.reverse_scroll);
    }

    #[test]
    fn unknown_app_only_matches_profiles_without_app_filter() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(false),
            ..Default::default()
        });

        let r = resolve_from(&cfg, Some((1, 2)), None);
        assert!(r.reverse_scroll);
    }

    #[test]
    fn combined_profile_wins_regardless_of_file_order() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            line_count: Some(3),
            ..Default::default()
        });
        // 组合档先写,但具体度更高,必须覆盖后面的 app-only 档。
        // The combined profile is written first, but has higher specificity and must win.
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(50475),
            },
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(false),
            line_count: Some(9),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(true),
            line_count: Some(6),
            ..Default::default()
        });

        let safari = safari_identity();
        let r = resolve_from(&cfg, Some((1133, 50475)), Some(&safari));
        assert!(!r.reverse_scroll);
        assert_eq!(r.line_count, 9);
    }

    #[test]
    fn same_specificity_keeps_file_order() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(false),
            line_count: Some(6),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(true),
            ..Default::default()
        });

        let safari = safari_identity();
        let r = resolve_from(&cfg, Some((1, 2)), Some(&safari));
        // 同具体度内后写者赢;未写的 line_count 继续继承前一条。
        // Within the same specificity, later wins; the omitted line_count is inherited.
        assert!(r.reverse_scroll);
        assert_eq!(r.line_count, 6);
    }

    #[test]
    fn device_only_and_app_only_share_same_tier() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(50475),
            },
            reverse_scroll: Some(false),
            line_count: Some(7),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(true),
            ..Default::default()
        });

        let safari = safari_identity();
        let r = resolve_from(&cfg, Some((1133, 50475)), Some(&safari));
        // device-only 与 app-only 同级,按文件顺序后写的 app-only 赢。
        // Device-only and app-only share a tier; the later app-only profile wins.
        assert!(r.reverse_scroll);
        assert_eq!(r.line_count, 7);
    }

    #[test]
    fn specificity_levels_merge_all_layers() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            scroll_mode: Some("line".into()),
            line_count: Some(3),
            pointer: Some(PartialPointerSection {
                disable_acceleration: Some(true),
            }),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(50475),
            },
            scroll_mode: Some("default".into()),
            line_count: Some(4),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.Safari".into()),
            line_count: Some(6),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(50475),
            },
            app_bundle_id: Some("com.apple.Safari".into()),
            reverse_scroll: Some(false),
            ..Default::default()
        });

        let safari = safari_identity();
        let r = resolve_from(&cfg, Some((1133, 50475)), Some(&safari));
        // 全局层提供 disable_acceleration,设备层提供 scroll_mode,app 层提供 line_count,
        // 组合层最终覆盖 reverse_scroll。
        // Global supplies disable_acceleration, device supplies scroll_mode, app supplies
        // line_count, and the combined profile finally overrides reverse_scroll.
        assert!(r.disable_acceleration);
        assert_eq!(r.scroll_mode, ScrollMode::Default);
        assert_eq!(r.line_count, 6);
        assert!(!r.reverse_scroll);
    }
}
