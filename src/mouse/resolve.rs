use crate::app::AppIdentity;
use crate::config::{Config, MouseProfile, CONFIG};
use crate::mouse::buttons::{self, PressedKey};
use crate::mouse::device::DeviceKey;
use crate::mouse::scrolling::ScrollMode;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedScroll {
    pub reverse_scroll: bool,
    pub scroll_mode: ScrollMode,
    pub line_count: u32,
    pub line_height: Option<u32>,
}

impl Default for ResolvedScroll {
    fn default() -> Self {
        Self {
            reverse_scroll: true,
            scroll_mode: ScrollMode::Line,
            line_count: 3,
            line_height: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedMouse {
    pub reverse_scroll: bool,
    pub scroll_mode: ScrollMode,
    pub line_count: u32,

    pub line_height: Option<u32>,
    #[allow(dead_code)]
    pub disable_acceleration: bool,

    pub button_map: BTreeMap<u32, PressedKey>,
}

impl ResolvedMouse {
    pub(crate) fn scroll(&self) -> ResolvedScroll {
        ResolvedScroll {
            reverse_scroll: self.reverse_scroll,
            scroll_mode: self.scroll_mode,
            line_count: self.line_count,
            line_height: self.line_height,
        }
    }
}

impl Default for ResolvedMouse {
    fn default() -> Self {
        Self {
            reverse_scroll: true,
            scroll_mode: ScrollMode::Line,
            line_count: 3,
            line_height: None,
            disable_acceleration: true,

            button_map: BTreeMap::new(),
        }
    }
}

type ResolveCacheKey = (Option<DeviceKey>, Option<AppIdentity>);

static CACHE: std::sync::LazyLock<
    Mutex<std::collections::HashMap<ResolveCacheKey, Arc<ResolvedMouse>>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

const FLAG_COMPUTED: u8 = 1 << 0;
const FLAG_APP_SCROLL: u8 = 1 << 1;
const FLAG_HAS_DEVICE: u8 = 1 << 2;
const FLAG_DEVICE_SCROLL: u8 = 1 << 3;

static APP_PROFILE_FLAGS: AtomicU8 = AtomicU8::new(0);
static CONFIG_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn config_generation() -> u64 {
    CONFIG_GENERATION.load(Ordering::Acquire)
}

pub(crate) fn resolve_scroll_for_app(app: Option<&AppIdentity>) -> ResolvedScroll {
    let cfg = CONFIG.read().unwrap();
    resolve_scroll_from(&cfg, None, app)
}

pub(crate) fn invalidate_cache() {
    APP_PROFILE_FLAGS.store(0, Ordering::Release);
    CONFIG_GENERATION.fetch_add(1, Ordering::Release);
    if let Ok(mut c) = CACHE.lock() {
        c.clear();
    }
    crate::app::refresh_scroll_cache();
}

fn has_device_filter(profile: &MouseProfile) -> bool {
    profile.device.vendor_id.is_some() || profile.device.product_id.is_some()
}

fn has_app_filter(profile: &MouseProfile) -> bool {
    profile.app_bundle_id.is_some() || profile.app_name.is_some()
}

fn affects_scroll(profile: &MouseProfile) -> bool {
    profile.reverse_scroll.is_some()
        || profile.scroll_mode.is_some()
        || profile.line_count.is_some()
        || profile.line_height.is_some()
}

fn app_profile_flags() -> u8 {
    let flags = APP_PROFILE_FLAGS.load(Ordering::Acquire);
    if flags & FLAG_COMPUTED != 0 {
        return flags;
    }
    let cfg = CONFIG.read().unwrap();
    let mut computed = FLAG_COMPUTED;
    for p in &cfg.mouse.profiles {
        let scroll = affects_scroll(p);
        if has_device_filter(p) {
            computed |= FLAG_HAS_DEVICE;
            if scroll {
                computed |= FLAG_DEVICE_SCROLL;
            }
        }
        if scroll && has_app_filter(p) {
            computed |= FLAG_APP_SCROLL;
        }
    }
    APP_PROFILE_FLAGS.store(computed, Ordering::Release);
    computed
}

pub(crate) fn has_per_app_scroll_profiles() -> bool {
    app_profile_flags() & FLAG_APP_SCROLL != 0
}

fn any_matches_per_app_scroll_profile_in<'a>(
    cfg: &Config,
    mut apps: impl Iterator<Item = &'a AppIdentity>,
) -> bool {
    apps.any(|app| {
        cfg.mouse
            .profiles
            .iter()
            .any(|p| has_app_filter(p) && affects_scroll(p) && app_matches(p, Some(app)))
    })
}

pub(crate) fn any_matches_per_app_scroll_profile<'a>(
    apps: impl Iterator<Item = &'a AppIdentity>,
) -> bool {
    if !has_per_app_scroll_profiles() {
        return false;
    }
    let cfg = CONFIG.read().unwrap();
    any_matches_per_app_scroll_profile_in(&cfg, apps)
}

pub(crate) fn has_per_device_scroll_profiles() -> bool {
    app_profile_flags() & FLAG_DEVICE_SCROLL != 0
}

fn device_matches_scroll_profile_in(cfg: &Config, device: DeviceKey) -> bool {
    cfg.mouse
        .profiles
        .iter()
        .any(|p| has_device_filter(p) && affects_scroll(p) && device_matches(p, Some(device)))
}

pub(crate) fn device_matches_scroll_profile(device: DeviceKey) -> bool {
    if !has_per_device_scroll_profiles() {
        return false;
    }
    let cfg = CONFIG.read().unwrap();
    device_matches_scroll_profile_in(&cfg, device)
}

pub(crate) fn has_per_device_profiles() -> bool {
    app_profile_flags() & FLAG_HAS_DEVICE != 0
}

fn device_matches(profile: &MouseProfile, device: Option<DeviceKey>) -> bool {
    let Some((vid, pid)) = device else {

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

fn profile_specificity(profile: &MouseProfile) -> u8 {
    let has_device = profile.device.vendor_id.is_some() || profile.device.product_id.is_some();
    let has_app = profile.app_bundle_id.is_some() || profile.app_name.is_some();
    (has_device as u8) + (has_app as u8)
}

pub(crate) fn resolve(device: Option<DeviceKey>, app: Option<AppIdentity>) -> Arc<ResolvedMouse> {
    let key = (device, app);

    if let Ok(c) = CACHE.lock()
        && let Some(r) = c.get(&key)
    {
        return Arc::clone(r);
    }

    let r = {
        let cfg = CONFIG.read().unwrap();
        Arc::new(resolve_from(&cfg, key.0, key.1.as_ref()))
    };

    if let Ok(mut c) = CACHE.lock() {
        c.insert(key, Arc::clone(&r));
    }
    r
}

pub(crate) fn supports_button(button: u32) -> bool {
    matches!(button, 3 | 4)
}

fn apply_scroll_fields(scroll: &mut ResolvedScroll, p: &MouseProfile) {
    if let Some(rs) = p.reverse_scroll {
        scroll.reverse_scroll = rs;
    }
    if let Some(ref mode) = p.scroll_mode {
        scroll.scroll_mode = ScrollMode::from_str(mode);
    }
    if let Some(lc) = p.line_count {
        scroll.line_count = lc.clamp(1, crate::config::MAX_LINE_COUNT);
    }
    if let Some(lh) = p.line_height {
        scroll.line_height = Some(lh.clamp(1, crate::config::MAX_LINE_HEIGHT));
    }
}

fn resolve_scroll_from(
    cfg: &Config,
    device: Option<DeviceKey>,
    app: Option<&AppIdentity>,
) -> ResolvedScroll {
    let mut scroll = ResolvedScroll::default();
    for specificity in 0..=2u8 {
        for p in &cfg.mouse.profiles {
            if profile_specificity(p) != specificity || !matches(p, device, app) {
                continue;
            }
            apply_scroll_fields(&mut scroll, p);
        }
    }
    scroll
}

fn resolve_from(
    cfg: &Config,
    device: Option<DeviceKey>,
    app: Option<&AppIdentity>,
) -> ResolvedMouse {
    let mut r = ResolvedMouse::default();
    let mut scroll = r.scroll();

    for specificity in 0..=2u8 {
        for p in &cfg.mouse.profiles {
            if profile_specificity(p) != specificity || !matches(p, device, app) {
                continue;
            }
            apply_scroll_fields(&mut scroll, p);
            if let Some(ref ptr) = p.pointer
                && let Some(da) = ptr.disable_acceleration
            {
                r.disable_acceleration = da;
            }

            if let Some(ref map) = p.button_map {
                for (btn, action) in map {
                    let Ok(button) = btn.parse::<u32>() else {
                        continue;
                    };
                    if !supports_button(button) {
                        continue;
                    }
                    if let Some(key) = buttons::parse_action(action) {
                        r.button_map.insert(button, key);
                    }
                }
            }
        }
    }

    r.reverse_scroll = scroll.reverse_scroll;
    r.scroll_mode = scroll.scroll_mode;
    r.line_count = scroll.line_count;
    r.line_height = scroll.line_height;
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PartialPointerSection;

    #[test]
    fn wildcard_only_falls_back_to_defaults() {
        let mut cfg = Config::default();

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

        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });

        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(17492),
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });

        let r = resolve_from(&cfg, Some((1133, 17492)), None);
        assert!(!r.reverse_scroll);

        let r2 = resolve_from(&cfg, Some((1, 2)), None);
        assert!(r2.reverse_scroll);

        let r3 = resolve_from(&cfg, None, None);
        assert!(r3.reverse_scroll);
    }

    #[test]
    fn later_match_wins_on_merge() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();

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
        assert!(!r.reverse_scroll);
        assert_eq!(r.line_count, 5);
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

        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mouse.profiles.len(), 2);
        assert!(parsed.mouse.profiles[0].device.vendor_id.is_none());
        assert_eq!(parsed.mouse.profiles[1].device.vendor_id, Some(0xC548));
        assert_eq!(parsed.mouse.profiles[1].reverse_scroll, Some(false));

        let r = resolve_from(&parsed, Some((0xC548, 0x4444)), None);
        assert!(!r.reverse_scroll);
        assert_eq!(r.line_count, 7);
        assert!(r.disable_acceleration);
    }

    #[test]
    fn button_map_merges_and_later_wins() {
        use crate::mouse::buttons::KeyAction;
        use crate::mouse::ffi::{K_CG_EVENT_FLAG_MASK_COMMAND, K_VK_ANSI_LEFT_BRACKET};

        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();

        cfg.mouse.profiles.push(MouseProfile {
            button_map: Some(BTreeMap::from([("3".to_string(), "ctrl+left".to_string())])),
            ..Default::default()
        });

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

        let r = resolve_from(&cfg, Some((1133, 50475)), None);
        assert_eq!(r.button_map.len(), 2);
        assert_eq!(r.button_map[&3].modifiers, K_CG_EVENT_FLAG_MASK_COMMAND);
        assert_eq!(
            r.button_map[&4].action,
            KeyAction::Keyboard {
                keycode: K_VK_ANSI_LEFT_BRACKET
            }
        );

        let r2 = resolve_from(&cfg, Some((1, 2)), None);
        assert_eq!(r2.button_map.len(), 1);
        assert!(r2.button_map.contains_key(&3));
        assert!(!r2.button_map.contains_key(&4));
    }

    #[test]
    fn line_height_merges_by_specificity_and_order() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();

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

        let r = resolve_from(&cfg, Some((1, 2)), Some(&notes));
        assert_eq!(r.line_height, Some(16));
        assert_eq!(r.line_count, 3);

        let other = AppIdentity {
            bundle_id: Some("com.google.Chrome".into()),
            localized_name: Some("Google Chrome".into()),
        };

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

        assert!(r.disable_acceleration);
        assert_eq!(r.scroll_mode, ScrollMode::Default);
        assert_eq!(r.line_count, 6);
        assert!(!r.reverse_scroll);
    }

    #[test]
    fn any_matches_per_app_scroll_profile_tracks_running_apps() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            scroll_mode: Some("pixel".into()),
            line_count: Some(4),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            app_bundle_id: Some("com.apple.ScreenContinuity".into()),
            scroll_mode: Some("continuous".into()),
            ..Default::default()
        });

        let safari = safari_identity();
        let continuity = AppIdentity {
            bundle_id: Some("com.apple.ScreenContinuity".into()),
            localized_name: Some("iPhone Mirroring".into()),
        };

        assert!(!any_matches_per_app_scroll_profile_in(
            &cfg,
            std::iter::once(&safari)
        ));
        assert!(any_matches_per_app_scroll_profile_in(
            &cfg,
            [&safari, &continuity].into_iter()
        ));
    }

    #[test]
    fn device_matches_scroll_profile_ignores_button_only_and_unmatched_devices() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            scroll_mode: Some("pixel".into()),
            line_count: Some(4),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(0x046d),
                product_id: Some(0xc548),
            },
            button_map: Some(BTreeMap::from([("3".to_string(), "cmd+[".to_string())])),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(0x05ac),
                product_id: None,
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });

        assert!(!device_matches_scroll_profile_in(&cfg, (0x046d, 0xc548)));
        assert!(!device_matches_scroll_profile_in(&cfg, (0x1532, 0x0084)));
        assert!(device_matches_scroll_profile_in(&cfg, (0x05ac, 0x0304)));

        let safari = safari_identity();
        assert_eq!(
            resolve_scroll_from(&cfg, None, Some(&safari)),
            resolve_from(&cfg, Some((0x046d, 0xc548)), Some(&safari)).scroll()
        );
        assert_eq!(
            resolve_scroll_from(&cfg, None, Some(&safari)),
            resolve_from(&cfg, Some((0x1532, 0x0084)), Some(&safari)).scroll()
        );
    }
}
