use crate::log_info;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::HashMap;
use std::ffi::c_char;
use std::sync::{LazyLock, RwLock};

const EN_TOML: &str = include_str!("../locales/en.toml");
const ZH_TOML: &str = include_str!("../locales/zh-Hans.toml");
const ZH_HANT_TOML: &str = include_str!("../locales/zh-Hant.toml");
const DEFAULT_LOCALE: &str = "en";

fn locale_raw(locale: &str) -> Option<&'static str> {
    match locale {
        "en" => Some(EN_TOML),
        "zh-Hans" => Some(ZH_TOML),
        "zh-Hant" => Some(ZH_HANT_TOML),
        _ => None,
    }
}

fn is_supported(locale: &str) -> bool {
    locale_raw(locale).is_some()
}

fn flatten(value: &toml::Value, prefix: &str, out: &mut HashMap<String, String>) {
    if let toml::Value::Table(t) = value {
        for (k, v) in t {
            let key = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                toml::Value::String(s) => {
                    out.insert(key, s.clone());
                }
                toml::Value::Table(_) => flatten(v, &key, out),
                _ => {}
            }
        }
    }
}

fn load_messages(locale: &str) -> HashMap<String, String> {
    let raw = match locale_raw(locale) {
        Some(r) => r,
        None => return HashMap::new(),
    };
    match toml::from_str::<toml::Value>(raw) {
        Ok(parsed) => {
            let mut map = HashMap::new();
            flatten(&parsed, "", &mut map);
            map
        }
        Err(e) => {
            log_info!("i18n: failed to parse locale '{}': {}", locale, e);
            HashMap::new()
        }
    }
}

static EN_MESSAGES: LazyLock<HashMap<String, String>> = LazyLock::new(|| load_messages("en"));

struct I18nState {
    locale: String,
    messages: HashMap<String, String>,
}

static I18N: LazyLock<RwLock<I18nState>> = LazyLock::new(|| {
    let locale = resolve_locale(None);
    RwLock::new(I18nState {
        messages: load_messages(&locale),
        locale,
    })
});

pub fn t(key: &str) -> String {
    let g = I18N.read().unwrap();
    if let Some(v) = g.messages.get(key) {
        return v.clone();
    }
    drop(g);
    if let Some(v) = EN_MESSAGES.get(key) {
        return v.clone();
    }
    key.to_string()
}

pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

pub fn apply_config_locale(locale_cfg: &str) {
    let resolved = resolve_locale(Some(locale_cfg));
    let mut g = I18N.write().unwrap();
    if g.locale == resolved {
        return;
    }
    g.locale = resolved.clone();
    g.messages = load_messages(&resolved);
}

fn resolve_locale(locale_cfg: Option<&str>) -> String {
    if let Some(cfg) = locale_cfg
        && cfg != "auto"
        && is_supported(cfg)
    {
        return cfg.to_string();
    }

    for tag in system_locales() {
        if let Some(loc) = map_tag_to_supported(&tag) {
            return loc.to_string();
        }
    }
    DEFAULT_LOCALE.to_string()
}

fn map_tag_to_supported(tag: &str) -> Option<&'static str> {
    let lower = tag.to_lowercase();
    if lower.starts_with("zh") {
        if lower.contains("hant")
            || lower.contains("tw")
            || lower.contains("hk")
            || lower.contains("mo")
        {
            Some("zh-Hant")
        } else {
            Some("zh-Hans")
        }
    } else if lower.starts_with("en") {
        Some("en")
    } else {
        None
    }
}

fn system_locales() -> Vec<String> {
    unsafe {
        let arr: *mut AnyObject = msg_send![class!(NSLocale), preferredLanguages];
        if arr.is_null() {
            return Vec::new();
        }
        let count: usize = msg_send![arr, count];
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let s: *mut AnyObject = msg_send![arr, objectAtIndex: i];
            out.push(nsstring_to_rust(s));
        }
        out
    }
}

unsafe fn nsstring_to_rust(ns: *mut AnyObject) -> String {
    if ns.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![ns, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    unsafe {
        std::ffi::CStr::from_ptr(utf8)
            .to_string_lossy()
            .into_owned()
    }
}
