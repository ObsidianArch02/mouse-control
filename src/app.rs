//! 前台 app 检测与缓存:per-app 配置覆盖的前置模块。
//! 主线程维护一份当前前台 app 的稳定快照(初始读取 + NSWorkspace 通知更新),
//! 鼠标事件线程只读取缓存,不在热路径上调用 AppKit。
//!
//! Frontmost-app detection and cache: the prerequisite for per-app config overrides.
//! The main thread keeps a stable snapshot of the frontmost app (initial read + NSWorkspace
//! notifications); the mouse event thread only reads the cache and never calls AppKit on the
//! hot path.

use crate::ffi::{
    class_addMethod, make_nsstring, nsstring_to_rust, objc_allocateClassPair,
    objc_registerClassPair, CFRelease,
};
use crate::mouse::ffi::{
    kCGWindowLayer, kCGWindowOwnerPID, objc_msgSend_window_number_at_point, CFArrayGetCount,
    CFArrayGetValueAtIndex, CFDictionaryGetValue, CFNumberGetValue, CGDisplayBounds,
    CGMainDisplayID, CGWindowListCopyWindowInfo, NSPointFFI, K_CF_NUMBER_INT_TYPE,
    K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
};
use crate::{log_debug, log_info};
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ACTIVATION_POLICY_PROHIBITED: isize = 2;
const WIN_META_CACHE_MAX: usize = 512;
const HIT_CACHE_TTL: Duration = Duration::from_millis(40);
const HIT_CACHE_RADIUS_PX: f64 = 4.0;
const MAX_HIT_WALK_DEPTH: usize = 6;

/// 当前前台 app 的稳定身份。字段来自 NSRunningApplication;无 bundle 的小工具/脚本只有
/// localized_name,两者都缺失时 per-app 档只会命中无 app 字段的全局/设备档。
/// Stable identity for the current frontmost app. Fields come from NSRunningApplication;
/// bundle-less utilities/scripts only have localized_name, and if both are missing per-app
/// profiles are ignored in favor of profiles without an app matcher.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AppIdentity {
    pub(crate) bundle_id: Option<String>,
    pub(crate) localized_name: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedApp {
    identity: AppIdentity,
    activation_policy: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HitDecision {
    Accept,
    SkipBelow,
    FallbackActive,
}

static ACTIVE_APP: Mutex<Option<AppIdentity>> = Mutex::new(None);
static PID_MAP: std::sync::LazyLock<Mutex<HashMap<i32, CachedApp>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static WIN_META_CACHE: std::sync::LazyLock<Mutex<HashMap<u32, (i32, i32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static APP_EPOCH: AtomicU64 = AtomicU64::new(1);

struct HitCache {
    x: f64,
    y: f64,
    epoch: u64,
    updated_at: Option<Instant>,
    result: Option<AppIdentity>,
}

static HIT_CACHE: Mutex<HitCache> = Mutex::new(HitCache {
    x: 0.0,
    y: 0.0,
    epoch: 0,
    updated_at: None,
    result: None,
});

/// 返回当前缓存的 app 身份。供鼠标线程 resolve 使用,只读 Mutex,不调用 AppKit。
/// Return the cached app identity. Used by the mouse thread for resolution; read-only Mutex,
/// no AppKit call.
pub(crate) fn active_identity() -> Option<AppIdentity> {
    ACTIVE_APP.lock().unwrap().clone()
}

fn set_active(identity: AppIdentity) -> bool {
    let mut guard = ACTIVE_APP.lock().unwrap();
    let changed = guard.as_ref() != Some(&identity);
    *guard = Some(identity);
    changed
}

unsafe fn optional_nsstring_to_rust(ns: *mut AnyObject) -> Option<String> {
    if ns.is_null() {
        return None;
    }
    let s = nsstring_to_rust(ns);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

unsafe fn frontmost_application() -> *mut AnyObject {
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    msg_send![workspace, frontmostApplication]
}

unsafe fn app_from_notification(notification: *mut c_void) -> *mut AnyObject {
    if notification.is_null() {
        return std::ptr::null_mut();
    }

    // NSWorkspaceDidActivateApplicationNotification 的 userInfo 会带上真正被激活的
    // NSRunningApplication。这里优先用通知里的 app,而不是重新查 frontmostApplication,
    // 避免快速切 app 时 frontmostApplication 仍是旧值,导致 per-app 覆盖应用到错误目标。
    // NSWorkspaceDidActivateApplicationNotification carries the app that actually activated.
    // Prefer that over re-querying frontmostApplication, which can still be stale during
    // rapid app switches and would apply per-app overrides to the wrong target.
    let user_info: *mut AnyObject = msg_send![notification as *mut AnyObject, userInfo];
    if user_info.is_null() {
        return std::ptr::null_mut();
    }

    let key = make_nsstring("NSWorkspaceApplicationKey");
    let app: *mut AnyObject = msg_send![user_info, objectForKey: key];
    CFRelease(key as *const c_void);
    app
}

unsafe fn identity_from_running_app(app: *mut AnyObject) -> AppIdentity {
    if app.is_null() {
        return AppIdentity {
            bundle_id: None,
            localized_name: None,
        };
    }
    let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
    let name: *mut AnyObject = msg_send![app, localizedName];
    AppIdentity {
        bundle_id: optional_nsstring_to_rust(bundle),
        localized_name: optional_nsstring_to_rust(name),
    }
}

unsafe fn cached_app_from_running_app(app: *mut AnyObject) -> CachedApp {
    let activation_policy: isize = if app.is_null() {
        ACTIVATION_POLICY_PROHIBITED
    } else {
        msg_send![app, activationPolicy]
    };
    CachedApp {
        identity: identity_from_running_app(app),
        activation_policy,
    }
}

fn classify_hit_candidate(layer: i32, app: Option<&CachedApp>) -> HitDecision {
    // 桌面层(layer < 0)不触发窗口归因,交由调用方按当前前台 app 兜底。
    // Desktop windows (layer < 0) do not override attribution; fall back to frontmost app.
    if layer < 0 {
        return HitDecision::FallbackActive;
    }
    let Some(app) = app else {
        return HitDecision::FallbackActive;
    };
    // 无界面后台/虚拟叠层(如 policy=2 的全屏透明驱动窗口)向下穿透继续查找真实宿主窗口。
    // Skip prohibited background processes (e.g., policy=2 fullscreen helper overlays) to inspect windows below.
    if app.activation_policy == ACTIVATION_POLICY_PROHIBITED {
        return HitDecision::SkipBelow;
    }
    if app.identity.bundle_id.as_deref() == Some("com.apple.dock") {
        return HitDecision::FallbackActive;
    }
    if app.identity.bundle_id.is_none() && app.identity.localized_name.is_none() {
        return HitDecision::FallbackActive;
    }
    HitDecision::Accept
}

unsafe fn lookup_or_query_pid(pid: i32) -> Option<CachedApp> {
    if pid <= 0 {
        return None;
    }
    if let Some(hit) = PID_MAP.lock().unwrap().get(&pid).cloned() {
        return Some(hit);
    }
    let app: *mut AnyObject = msg_send![
        class!(NSRunningApplication),
        runningApplicationWithProcessIdentifier: pid
    ];
    let entry = cached_app_from_running_app(app);
    let mut map = PID_MAP.lock().unwrap();
    if map.len() >= WIN_META_CACHE_MAX && !map.contains_key(&pid) {
        map.retain(|_, v| v.activation_policy != ACTIVATION_POLICY_PROHIBITED);
        if map.len() >= WIN_META_CACHE_MAX {
            map.clear();
        }
    }
    map.insert(pid, entry.clone());
    Some(entry)
}

unsafe fn window_owner_and_layer(wn: u32) -> Option<(i32, i32)> {
    if let Some(hit) = WIN_META_CACHE.lock().unwrap().get(&wn).copied() {
        return Some(hit);
    }
    let list = CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW, wn);
    if list.is_null() {
        return None;
    }
    let meta = if CFArrayGetCount(list) > 0 {
        let w = CFArrayGetValueAtIndex(list, 0);
        if w.is_null() {
            None
        } else {
            match (
                read_dict_int(w, kCGWindowOwnerPID),
                read_dict_int(w, kCGWindowLayer),
            ) {
                (Some(pid), Some(layer)) => Some((pid, layer)),
                _ => None,
            }
        }
    } else {
        None
    };
    CFRelease(list);
    if let Some(v) = meta {
        let mut cache = WIN_META_CACHE.lock().unwrap();
        if cache.len() >= WIN_META_CACHE_MAX && !cache.contains_key(&wn) {
            cache.clear();
        }
        cache.insert(wn, v);
    }
    meta
}

/// 光标坐标处的 app 身份:实时请求 WindowServer 做单点窗口命中测试,自动跳过透明穿透层
/// 及后台覆盖层(如 policy=2 的虚拟显示驱动),无 200ms 切窗滞后。
///
/// App identity at cursor coordinates: perform live WindowServer hit-testing, skipping
/// click-through and background overlay processes without stale snapshot lag.
pub(crate) fn identity_at_point(x: f64, y: f64) -> Option<AppIdentity> {
    let epoch = APP_EPOCH.load(Ordering::Acquire);
    {
        let c = HIT_CACHE.lock().unwrap();
        if c.epoch == epoch
            && c.updated_at.is_some_and(|t| t.elapsed() < HIT_CACHE_TTL)
            && (x - c.x).abs() <= HIT_CACHE_RADIUS_PX
            && (y - c.y).abs() <= HIT_CACHE_RADIUS_PX
        {
            return c.result.clone();
        }
    }

    let result = autoreleasepool(|_| unsafe {
        let main_h = CGDisplayBounds(CGMainDisplayID()).size.height;
        let ns_pt = NSPointFFI { x, y: main_h - y };
        let ns_win_cls = class!(NSWindow) as *const _ as *mut AnyObject;
        let mut below: isize = 0;
        let mut resolved = None;
        for _ in 0..MAX_HIT_WALK_DEPTH {
            let wn = objc_msgSend_window_number_at_point(
                ns_win_cls,
                sel!(windowNumberAtPoint:belowWindowWithWindowNumber:),
                ns_pt,
                below,
            );
            if wn <= 0 {
                break;
            }
            let Some((pid, layer)) = window_owner_and_layer(wn as u32) else {
                break;
            };
            let cached = lookup_or_query_pid(pid);
            match classify_hit_candidate(layer, cached.as_ref()) {
                HitDecision::Accept => {
                    resolved = cached.map(|c| c.identity);
                    break;
                }
                HitDecision::SkipBelow => {
                    below = wn;
                }
                HitDecision::FallbackActive => {
                    break;
                }
            }
        }
        resolved
    });

    let mut c = HIT_CACHE.lock().unwrap();
    c.x = x;
    c.y = y;
    c.epoch = epoch;
    c.updated_at = Some(Instant::now());
    c.result = result.clone();
    result
}

unsafe fn seed_pid_map() {
    autoreleasepool(|_| unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let apps: *mut AnyObject = msg_send![workspace, runningApplications];
        if apps.is_null() {
            return;
        }
        let count = CFArrayGetCount(apps as *const c_void);
        let mut entries = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count {
            let app = CFArrayGetValueAtIndex(apps as *const c_void, i) as *mut AnyObject;
            if app.is_null() {
                continue;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            if pid > 0 {
                entries.push((pid, cached_app_from_running_app(app)));
            }
        }
        let mut map = PID_MAP.lock().unwrap();
        for (pid, entry) in entries {
            map.insert(pid, entry);
        }
    });
}

/// 从窗口 CFDictionary 读 i32 值(kCGWindowLayer / kCGWindowOwnerPID 都是 CFNumber)。
/// Read an i32 value from a window CFDictionary (kCGWindowLayer / kCGWindowOwnerPID are
/// both CFNumbers).
unsafe fn read_dict_int(dict: *const c_void, key: *const c_void) -> Option<i32> {
    let value = CFDictionaryGetValue(dict, key);
    if value.is_null() {
        return None;
    }
    let mut out: i32 = 0;
    if CFNumberGetValue(value, K_CF_NUMBER_INT_TYPE, &mut out as *mut i32 as *mut c_void) {
        Some(out)
    } else {
        None
    }
}

unsafe fn refresh_from_frontmost() -> AppIdentity {
    identity_from_running_app(frontmost_application())
}

/// 观察者目标保活。NSNotificationCenter 的 selector 式 observer 不 retain target,
/// 与 menu target 同款问题:必须由我们显式 retain 并持有。
/// Keeps the observer target alive. The selector-based NSNotificationCenter observer does not
/// retain its target, mirroring the menu target: we must retain and own it explicitly.
struct ObserverTarget(Mutex<Option<*mut AnyObject>>);
unsafe impl Send for ObserverTarget {}
unsafe impl Sync for ObserverTarget {}

static OBSERVER_TARGET: OnceLock<ObserverTarget> = OnceLock::new();

fn observer_target() -> &'static Mutex<Option<*mut AnyObject>> {
    &OBSERVER_TARGET
        .get_or_init(|| ObserverTarget(Mutex::new(None)))
        .0
}

extern "C" fn handle_app_activation(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    // 通知携带的 app 是权威值;极少数缺失 userInfo 的情况回退到 frontmostApplication。
    // The app carried by the notification is authoritative; fall back only when its
    // userInfo is missing.
    let (cached, pid) = unsafe {
        let app = app_from_notification(notification);
        if app.is_null() {
            let f = frontmost_application();
            let pid: i32 = if f.is_null() {
                0
            } else {
                msg_send![f, processIdentifier]
            };
            (cached_app_from_running_app(f), pid)
        } else {
            let pid: i32 = msg_send![app, processIdentifier];
            (cached_app_from_running_app(app), pid)
        }
    };
    let identity = cached.identity.clone();
    if pid > 0 {
        PID_MAP.lock().unwrap().insert(pid, cached);
    }
    APP_EPOCH.fetch_add(1, Ordering::Release);
    if !set_active(identity) {
        return;
    }
    log_debug!("[app] frontmost app changed: {:?}", active_identity());
    // 切 app 前先释放已按住的侧键目标键,避免旧 app 的 cmd/ctrl/opt/shift 卡在新 app。
    // Release held side-button targets before the new app receives events, so modifiers from
    // the old app never stick in the new app.
    crate::mouse::buttons::release_all();
    // 指针加速可能按 app 覆盖,切 app 时重算并重应用。
    // Pointer acceleration may be overridden per app; re-evaluate and re-apply on switch.
    if crate::mouse::resolve::has_per_app_pointer_profiles() {
        crate::mouse::pointer::apply();
    }
}

extern "C" fn handle_app_launch(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    let entry = unsafe {
        let app = app_from_notification(notification);
        if app.is_null() {
            None
        } else {
            let pid: i32 = msg_send![app, processIdentifier];
            if pid > 0 {
                Some((pid, cached_app_from_running_app(app)))
            } else {
                None
            }
        }
    };
    if let Some((pid, cached)) = entry {
        PID_MAP.lock().unwrap().insert(pid, cached);
    }
    APP_EPOCH.fetch_add(1, Ordering::Release);
}

extern "C" fn handle_app_terminate(_self: *mut c_void, _cmd: Sel, notification: *mut c_void) {
    let term_pid = unsafe {
        let app = app_from_notification(notification);
        if app.is_null() {
            0
        } else {
            msg_send![app, processIdentifier]
        }
    };
    if term_pid > 0 {
        PID_MAP.lock().unwrap().remove(&term_pid);
        WIN_META_CACHE
            .lock()
            .unwrap()
            .retain(|_, (owner, _)| *owner != term_pid);
    }
    APP_EPOCH.fetch_add(1, Ordering::Release);
}

unsafe fn install_observer() {
    if observer_target().lock().unwrap().is_some() {
        return;
    }

    let class_name = CString::new("MouseControlAppObserver").unwrap();
    let superclass: *const objc2::runtime::AnyClass = class!(NSObject);
    let cls = objc_allocateClassPair(superclass as *mut AnyObject, class_name.as_ptr(), 0);
    if cls.is_null() {
        log_info!("[app] failed to allocate observer class");
        return;
    }
    let types = CString::new("v@:@").unwrap();
    class_addMethod(
        cls,
        sel!(handleWorkspaceDidActivateApplication:),
        handle_app_activation as *mut c_void,
        types.as_ptr(),
    );
    class_addMethod(
        cls,
        sel!(handleWorkspaceDidLaunchApplication:),
        handle_app_launch as *mut c_void,
        types.as_ptr(),
    );
    class_addMethod(
        cls,
        sel!(handleWorkspaceDidTerminateApplication:),
        handle_app_terminate as *mut c_void,
        types.as_ptr(),
    );
    objc_registerClassPair(cls);

    let target: *mut AnyObject = msg_send![cls as *const AnyObject, new];
    if target.is_null() {
        log_info!("[app] failed to create observer target");
        return;
    }
    let _: *mut AnyObject = msg_send![target, retain];
    *observer_target().lock().unwrap() = Some(target);

    // NSWorkspace 的通知必须订阅 NSWorkspace.sharedWorkspace.notificationCenter,
    // 不能订阅 NSNotificationCenter.defaultCenter;否则 activation 通知根本收不到。
    // NSWorkspace notifications must be observed on
    // NSWorkspace.sharedWorkspace.notificationCenter, not the default notification center;
    // otherwise the activation notifications never arrive.
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let center: *mut AnyObject = msg_send![workspace, notificationCenter];
    for (notif_name, selector) in [
        (
            "NSWorkspaceDidActivateApplicationNotification",
            sel!(handleWorkspaceDidActivateApplication:),
        ),
        (
            "NSWorkspaceDidLaunchApplicationNotification",
            sel!(handleWorkspaceDidLaunchApplication:),
        ),
        (
            "NSWorkspaceDidTerminateApplicationNotification",
            sel!(handleWorkspaceDidTerminateApplication:),
        ),
    ] {
        let notification_name = make_nsstring(notif_name);
        let _: () = msg_send![
            center,
            addObserver: target,
            selector: selector,
            name: notification_name,
            object: std::ptr::null::<AnyObject>()
        ];
        CFRelease(notification_name as *const c_void);
    }
    log_info!("[app] active-app notification observer installed.");
}

/// 主线程启动时调用一次:初始化当前前台 app,并注册后续激活通知。
/// Called once on the main thread at startup: initializes the current frontmost app and
/// registers the activation observer.
pub(crate) fn start_monitor() {
    unsafe {
        seed_pid_map();
        let initial = refresh_from_frontmost();
        set_active(initial);
        install_observer();
    }
    log_debug!("[app] active-app monitor started: {:?}", active_identity());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(bundle: Option<&str>, name: Option<&str>, policy: isize) -> CachedApp {
        CachedApp {
            identity: AppIdentity {
                bundle_id: bundle.map(str::to_string),
                localized_name: name.map(str::to_string),
            },
            activation_policy: policy,
        }
    }

    #[test]
    fn hit_classifier_skips_prohibited_overlays_and_accepts_gui_and_popovers() {
        let cua = cached(Some("com.trycua.driver"), Some("Cua Driver"), 2);
        assert_eq!(
            classify_hit_candidate(0, Some(&cua)),
            HitDecision::SkipBelow
        );

        let ghostty = cached(Some("com.mitchellh.ghostty"), Some("Ghostty"), 0);
        assert_eq!(
            classify_hit_candidate(0, Some(&ghostty)),
            HitDecision::Accept
        );

        let popover = cached(Some("com.mowglii.ItsycalApp"), Some("Itsycal"), 1);
        assert_eq!(
            classify_hit_candidate(3, Some(&popover)),
            HitDecision::Accept
        );

        let dock = cached(Some("com.apple.dock"), Some("Dock"), 1);
        assert_eq!(
            classify_hit_candidate(20, Some(&dock)),
            HitDecision::FallbackActive
        );

        assert_eq!(
            classify_hit_candidate(-2_147_483_603, Some(&ghostty)),
            HitDecision::FallbackActive
        );
        assert_eq!(classify_hit_candidate(0, None), HitDecision::FallbackActive);
    }

    #[test]
    fn synthetic_scroll_marker_matches_self_and_mos() {
        assert!(crate::mouse::ffi::is_synthetic_scroll_marker(
            crate::mouse::ffi::SYNTHETIC_MARKER
        ));
        assert!(crate::mouse::ffi::is_synthetic_scroll_marker(
            crate::mouse::ffi::MOS_SYNTHETIC_MARKER
        ));
        assert!(!crate::mouse::ffi::is_synthetic_scroll_marker(0));
    }
}
