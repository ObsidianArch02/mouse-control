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
    kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowOwnerPID, CFArrayGetCount,
    CFArrayGetValueAtIndex, CFDictionaryGetValue, CFNumberGetValue, CGRectFFI,
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, K_CF_NUMBER_FLOAT64_TYPE,
    K_CF_NUMBER_INT_TYPE, K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
    K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY,
};
use crate::{log_debug, log_info};
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const ACTIVATION_POLICY_PROHIBITED: isize = 2;
const PID_MAP_MAX: usize = 512;
const MAX_WINDOW_LAYER: i32 = 20;
const WINDOW_SNAPSHOT_TTL: Duration = Duration::from_millis(3000);
const POST_SWITCH_SETTLE_FIRST: Duration = Duration::from_millis(150);
const POST_SWITCH_SETTLE_SECOND: Duration = Duration::from_millis(400);

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

#[derive(Debug, Clone)]
struct WindowRect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    identity: AppIdentity,
    scroll: crate::mouse::resolve::ResolvedScroll,
}

struct WindowSnapshot {
    windows: Vec<WindowRect>,
    epoch: u64,
    config_gen: u64,
    updated_at: Option<Instant>,
}

struct SnapSignalState {
    immediate: bool,
    next_due: Option<Instant>,
    follow_up_due: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapWaitAction {
    RunNow,
    WaitFor(Duration),
    WaitForever,
}

impl SnapSignalState {
    const fn new() -> Self {
        Self {
            immediate: false,
            next_due: None,
            follow_up_due: None,
        }
    }

    fn request_immediate(&mut self) -> bool {
        let notify = !self.immediate;
        self.immediate = true;
        notify
    }

    fn request_post_switch(&mut self, now: Instant) {
        self.immediate = true;
        self.next_due = Some(now + POST_SWITCH_SETTLE_FIRST);
        self.follow_up_due = Some(now + POST_SWITCH_SETTLE_SECOND);
    }

    fn poll_action(&mut self, now: Instant) -> SnapWaitAction {
        if self.immediate {
            self.immediate = false;
            return SnapWaitAction::RunNow;
        }
        if let Some(due) = self.next_due {
            if now >= due {
                self.next_due = self.follow_up_due.take();
                return SnapWaitAction::RunNow;
            }
            return SnapWaitAction::WaitFor(due.saturating_duration_since(now));
        }
        SnapWaitAction::WaitForever
    }
}

static ACTIVE_APP: Mutex<Option<AppIdentity>> = Mutex::new(None);
static ACTIVE_SCROLL: std::sync::LazyLock<Mutex<crate::mouse::resolve::ResolvedScroll>> =
    std::sync::LazyLock::new(|| {
        let app = ACTIVE_APP.lock().unwrap().clone();
        Mutex::new(crate::mouse::resolve::resolve_scroll_for_app(app.as_ref()))
    });
static PID_MAP: std::sync::LazyLock<Mutex<HashMap<i32, CachedApp>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static APP_EPOCH: AtomicU64 = AtomicU64::new(1);
static WINDOW_SNAPSHOT: std::sync::LazyLock<Mutex<WindowSnapshot>> =
    std::sync::LazyLock::new(|| {
        Mutex::new(WindowSnapshot {
            windows: Vec::new(),
            epoch: 0,
            config_gen: 0,
            updated_at: None,
        })
    });
static SNAP_SIGNAL: Mutex<SnapSignalState> = Mutex::new(SnapSignalState::new());
static SNAP_CV: Condvar = Condvar::new();
static SNAP_WORKER: OnceLock<()> = OnceLock::new();

/// 返回当前缓存的 app 身份。供鼠标线程 resolve 使用,只读 Mutex,不调用 AppKit。
/// Return the cached app identity. Used by the mouse thread for resolution; read-only Mutex,
/// no AppKit call.
pub(crate) fn active_identity() -> Option<AppIdentity> {
    ACTIVE_APP.lock().unwrap().clone()
}

/// 返回当前前台 app 预解析好的滚轮参数(Copy,零堆分配)。
/// Return the pre-resolved scroll config for the active app (Copy, zero heap allocation).
pub(crate) fn active_scroll() -> crate::mouse::resolve::ResolvedScroll {
    *ACTIVE_SCROLL.lock().unwrap()
}

/// 配置重载时同步刷新前台 app 与窗口快照的预解析滚轮参数。
/// Refresh pre-resolved scroll parameters after a config reload.
pub(crate) fn refresh_scroll_cache() {
    let active = ACTIVE_APP.lock().unwrap().clone();
    *ACTIVE_SCROLL.lock().unwrap() = crate::mouse::resolve::resolve_scroll_for_app(active.as_ref());
    if SNAP_WORKER.get().is_some() {
        request_snapshot_refresh();
    }
}

fn set_active(identity: AppIdentity) -> bool {
    let resolved = crate::mouse::resolve::resolve_scroll_for_app(Some(&identity));
    *ACTIVE_SCROLL.lock().unwrap() = resolved;
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

fn should_include_window(
    layer: i32,
    alpha: f64,
    width: f64,
    height: f64,
    app: Option<&CachedApp>,
) -> bool {
    // 排除桌面层(layer < 0)与 Dock/MenuBar 及以上系统层(layer >= 20),保留普通窗口与浮层。
    // Exclude desktop windows (layer < 0) and Dock/MenuBar system layers (layer >= 20), keeping
    // normal windows and popovers.
    if !(0..MAX_WINDOW_LAYER).contains(&layer) {
        return false;
    }
    if !alpha.is_finite() || alpha <= 0.0 {
        return false;
    }
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return false;
    }
    let Some(app) = app else {
        return false;
    };
    // 跳过无界面后台/虚拟叠层(如 policy=2 的全屏透明驱动窗口)与 Dock。
    // Skip prohibited background helpers (e.g., policy=2 fullscreen driver overlays) and Dock.
    if app.activation_policy == ACTIVATION_POLICY_PROHIBITED {
        return false;
    }
    if app.identity.bundle_id.as_deref() == Some("com.apple.dock") {
        return false;
    }
    if app.identity.bundle_id.is_none() && app.identity.localized_name.is_none() {
        return false;
    }
    true
}

fn lookup_window_identity(
    snap: &WindowSnapshot,
    current_epoch: u64,
    x: f64,
    y: f64,
    now: Instant,
) -> (Option<AppIdentity>, bool) {
    // 切窗后快照代际滞后时直接返回 None,让调用方回退到已同步更新的前台 app,并触发刷新。
    // When snapshot epoch lags behind the latest switch, return None so the caller falls back
    // immediately to the updated frontmost app while triggering an async refresh.
    if snap.epoch != current_epoch {
        return (None, true);
    }
    let hit = snap
        .windows
        .iter()
        .find(|r| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
        .map(|r| r.identity.clone());
    let needs_refresh = snap
        .updated_at
        .is_none_or(|t| now.saturating_duration_since(t) >= WINDOW_SNAPSHOT_TTL);
    (hit, needs_refresh)
}

fn lookup_window_scroll(
    snap: &WindowSnapshot,
    current_epoch: u64,
    current_cfg_gen: u64,
    x: f64,
    y: f64,
    now: Instant,
) -> (Option<crate::mouse::resolve::ResolvedScroll>, bool) {
    if snap.epoch != current_epoch || snap.config_gen != current_cfg_gen {
        return (None, true);
    }
    let hit = snap
        .windows
        .iter()
        .find(|r| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
        .map(|r| r.scroll);
    let needs_refresh = snap
        .updated_at
        .is_none_or(|t| now.saturating_duration_since(t) >= WINDOW_SNAPSHOT_TTL);
    (hit, needs_refresh)
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
    if map.len() >= PID_MAP_MAX && !map.contains_key(&pid) {
        map.retain(|_, v| v.activation_policy != ACTIVATION_POLICY_PROHIBITED);
        if map.len() >= PID_MAP_MAX {
            map.clear();
        }
    }
    map.insert(pid, entry.clone());
    Some(entry)
}

unsafe fn capture_window_rects() -> Vec<WindowRect> {
    let list = CGWindowListCopyWindowInfo(
        K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
        0,
    );
    if list.is_null() {
        return Vec::new();
    }
    let count = CFArrayGetCount(list);
    let mut windows = Vec::with_capacity(count.max(0) as usize);
    for i in 0..count {
        let w = CFArrayGetValueAtIndex(list, i);
        if w.is_null() {
            continue;
        }
        let Some(layer) = read_dict_int(w, kCGWindowLayer) else {
            continue;
        };
        if !(0..MAX_WINDOW_LAYER).contains(&layer) {
            continue;
        }
        let alpha = read_dict_f64(w, kCGWindowAlpha).unwrap_or(1.0);
        if !alpha.is_finite() || alpha <= 0.0 {
            continue;
        }
        let bounds_dict = CFDictionaryGetValue(w, kCGWindowBounds);
        if bounds_dict.is_null() {
            continue;
        }
        let mut rect = CGRectFFI::default();
        if !CGRectMakeWithDictionaryRepresentation(bounds_dict, &mut rect) {
            continue;
        }
        if !rect.origin.x.is_finite()
            || !rect.origin.y.is_finite()
            || !rect.size.width.is_finite()
            || !rect.size.height.is_finite()
            || rect.size.width <= 0.0
            || rect.size.height <= 0.0
        {
            continue;
        }
        let Some(pid) = read_dict_int(w, kCGWindowOwnerPID) else {
            continue;
        };
        let cached = lookup_or_query_pid(pid);
        if !should_include_window(
            layer,
            alpha,
            rect.size.width,
            rect.size.height,
            cached.as_ref(),
        ) {
            continue;
        }
        if let Some(cached) = cached {
            let scroll = crate::mouse::resolve::resolve_scroll_for_app(Some(&cached.identity));
            windows.push(WindowRect {
                x0: rect.origin.x,
                y0: rect.origin.y,
                x1: rect.origin.x + rect.size.width,
                y1: rect.origin.y + rect.size.height,
                identity: cached.identity,
                scroll,
            });
        }
    }
    CFRelease(list);
    windows
}

fn ensure_snap_worker() {
    SNAP_WORKER.get_or_init(|| {
        std::thread::Builder::new()
            .name("window-snap".into())
            .spawn(|| loop {
                let mut signal = SNAP_SIGNAL.lock().unwrap();
                loop {
                    match signal.poll_action(Instant::now()) {
                        SnapWaitAction::RunNow => break,
                        SnapWaitAction::WaitFor(dur) => {
                            let (s, _) = SNAP_CV.wait_timeout(signal, dur).unwrap();
                            signal = s;
                        }
                        SnapWaitAction::WaitForever => {
                            signal = SNAP_CV.wait(signal).unwrap();
                        }
                    }
                }
                drop(signal);

                loop {
                    let target_epoch = APP_EPOCH.load(Ordering::Acquire);
                    let target_cfg_gen = crate::mouse::resolve::config_generation();
                    let windows = autoreleasepool(|_| unsafe { capture_window_rects() });
                    {
                        let mut snap = WINDOW_SNAPSHOT.lock().unwrap();
                        snap.windows = windows;
                        snap.epoch = target_epoch;
                        snap.config_gen = target_cfg_gen;
                        snap.updated_at = Some(Instant::now());
                    }
                    if APP_EPOCH.load(Ordering::Acquire) == target_epoch
                        && crate::mouse::resolve::config_generation() == target_cfg_gen
                    {
                        break;
                    }
                }
            })
            .expect("failed to spawn window-snap worker thread");
    });
}

fn request_snapshot_refresh() {
    ensure_snap_worker();
    let mut signal = SNAP_SIGNAL.lock().unwrap();
    if signal.request_immediate() {
        SNAP_CV.notify_one();
    }
}

fn request_post_switch_snapshot() {
    ensure_snap_worker();
    let mut signal = SNAP_SIGNAL.lock().unwrap();
    signal.request_post_switch(Instant::now());
    SNAP_CV.notify_one();
}

/// 光标坐标处的 app 身份:只读内存窗口快照,不在 event tap 线程发起任何 AppKit/SkyLight IPC;
/// 快照代际滞后或过期时仅唤醒后台 window-snap 线程异步刷新。
///
/// App identity at cursor coordinates: reads only the in-memory window snapshot with zero
/// AppKit or SkyLight IPC on the event-tap thread, waking the background window-snap worker
/// asynchronously when the snapshot epoch lags or TTL expires.
pub(crate) fn identity_at_point(x: f64, y: f64) -> Option<AppIdentity> {
    let current_epoch = APP_EPOCH.load(Ordering::Acquire);
    let (hit, needs_refresh) = {
        let snap = WINDOW_SNAPSHOT.lock().unwrap();
        lookup_window_identity(&snap, current_epoch, x, y, Instant::now())
    };
    if needs_refresh {
        request_snapshot_refresh();
    }
    hit
}

/// 零堆分配滚轮热路径:直接从内存窗口快照复制预解析好的 ResolvedScroll,未命中或代际滞后时
/// 立即回退到前台 app 的预解析参数。
/// Zero-allocation scroll hot path: copies the pre-resolved ResolvedScroll directly from the
/// in-memory window snapshot, falling back to the active app's pre-resolved config on miss.
pub(crate) fn scroll_at_point_or_active(x: f64, y: f64) -> crate::mouse::resolve::ResolvedScroll {
    let current_epoch = APP_EPOCH.load(Ordering::Acquire);
    let current_cfg_gen = crate::mouse::resolve::config_generation();
    let (hit, needs_refresh) = {
        let snap = WINDOW_SNAPSHOT.lock().unwrap();
        lookup_window_scroll(&snap, current_epoch, current_cfg_gen, x, y, Instant::now())
    };
    if needs_refresh {
        request_snapshot_refresh();
    }
    hit.unwrap_or_else(active_scroll)
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

/// 从窗口 CFDictionary 读 f64 值(kCGWindowAlpha 是 CFNumber)。
/// Read an f64 value from a window CFDictionary (kCGWindowAlpha is a CFNumber).
unsafe fn read_dict_f64(dict: *const c_void, key: *const c_void) -> Option<f64> {
    let value = CFDictionaryGetValue(dict, key);
    if value.is_null() {
        return None;
    }
    let mut out: f64 = 0.0;
    if CFNumberGetValue(
        value,
        K_CF_NUMBER_FLOAT64_TYPE,
        &mut out as *mut f64 as *mut c_void,
    ) {
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
    request_post_switch_snapshot();
    if !set_active(identity) {
        return;
    }
    log_debug!("[app] frontmost app changed: {:?}", active_identity());
    // 切 app 前先释放已按住的侧键目标键,避免旧 app 的 cmd/ctrl/opt/shift 卡在新 app。
    // Release held side-button targets before the new app receives events, so modifiers from
    // the old app never stick in the new app.
    crate::mouse::buttons::release_all();
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
    request_post_switch_snapshot();
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
    }
    APP_EPOCH.fetch_add(1, Ordering::Release);
    request_post_switch_snapshot();
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
    request_snapshot_refresh();
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
    fn window_filter_excludes_overlays_dock_transparent_and_empty_bounds() {
        let cua = cached(Some("com.trycua.driver"), Some("Cua Driver"), 2);
        assert!(!should_include_window(0, 1.0, 2752.0, 1152.0, Some(&cua)));

        let ghostty = cached(Some("com.mitchellh.ghostty"), Some("Ghostty"), 0);
        assert!(should_include_window(0, 1.0, 1200.0, 800.0, Some(&ghostty)));
        assert!(!should_include_window(
            0,
            0.0,
            1200.0,
            800.0,
            Some(&ghostty)
        ));
        assert!(!should_include_window(0, 1.0, 0.0, 0.0, Some(&ghostty)));
        assert!(!should_include_window(
            -2_147_483_603,
            1.0,
            1200.0,
            800.0,
            Some(&ghostty)
        ));

        let popover = cached(Some("com.mowglii.ItsycalApp"), Some("Itsycal"), 1);
        assert!(should_include_window(3, 1.0, 320.0, 400.0, Some(&popover)));

        let dock = cached(Some("com.apple.dock"), Some("Dock"), 1);
        assert!(!should_include_window(20, 1.0, 2752.0, 1152.0, Some(&dock)));
        assert!(!should_include_window(0, 1.0, 2752.0, 1152.0, Some(&dock)));

        let unnamed = cached(None, None, 0);
        assert!(!should_include_window(0, 1.0, 400.0, 300.0, Some(&unnamed)));
        assert!(!should_include_window(0, 1.0, 400.0, 300.0, None));
    }

    #[test]
    fn lookup_window_identity_respects_epoch_z_order_and_ttl() {
        let now = Instant::now();
        let front = AppIdentity {
            bundle_id: Some("com.mitchellh.ghostty".to_string()),
            localized_name: Some("Ghostty".to_string()),
        };
        let back = AppIdentity {
            bundle_id: Some("net.imput.helium".to_string()),
            localized_name: Some("Helium".to_string()),
        };
        let front_scroll = crate::mouse::resolve::ResolvedScroll {
            scroll_mode: crate::mouse::scrolling::ScrollMode::Pixel,
            line_count: 1,
            ..Default::default()
        };
        let back_scroll = crate::mouse::resolve::ResolvedScroll {
            scroll_mode: crate::mouse::scrolling::ScrollMode::Pixel,
            line_count: 3,
            ..Default::default()
        };
        let snap = WindowSnapshot {
            windows: vec![
                WindowRect {
                    x0: 100.0,
                    y0: 100.0,
                    x1: 500.0,
                    y1: 500.0,
                    identity: front.clone(),
                    scroll: front_scroll,
                },
                WindowRect {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 1000.0,
                    y1: 1000.0,
                    identity: back.clone(),
                    scroll: back_scroll,
                },
            ],
            epoch: 3,
            config_gen: 7,
            updated_at: Some(now),
        };

        assert_eq!(
            lookup_window_identity(&snap, 4, 200.0, 200.0, now),
            (None, true)
        );
        assert_eq!(
            lookup_window_identity(&snap, 3, 200.0, 200.0, now),
            (Some(front), false)
        );
        assert_eq!(
            lookup_window_identity(&snap, 3, 50.0, 50.0, now),
            (Some(back.clone()), false)
        );
        assert_eq!(
            lookup_window_identity(&snap, 3, 1200.0, 1200.0, now),
            (None, false)
        );
        assert_eq!(
            lookup_window_identity(&snap, 3, 50.0, 50.0, now + WINDOW_SNAPSHOT_TTL),
            (Some(back), true)
        );

        assert_eq!(
            lookup_window_scroll(&snap, 3, 8, 200.0, 200.0, now),
            (None, true)
        );
        assert_eq!(
            lookup_window_scroll(&snap, 3, 7, 200.0, 200.0, now),
            (Some(front_scroll), false)
        );
        assert_eq!(
            lookup_window_scroll(&snap, 3, 7, 50.0, 50.0, now + WINDOW_SNAPSHOT_TTL),
            (Some(back_scroll), true)
        );
    }

    #[test]
    fn snap_signal_schedules_immediate_and_two_settle_passes() {
        let t0 = Instant::now();
        let mut s = SnapSignalState::new();
        assert_eq!(s.poll_action(t0), SnapWaitAction::WaitForever);

        s.request_post_switch(t0);
        assert_eq!(s.poll_action(t0), SnapWaitAction::RunNow);
        assert_eq!(
            s.poll_action(t0 + Duration::from_millis(50)),
            SnapWaitAction::WaitFor(Duration::from_millis(100))
        );
        assert_eq!(
            s.poll_action(t0 + POST_SWITCH_SETTLE_FIRST),
            SnapWaitAction::RunNow
        );
        assert_eq!(
            s.poll_action(t0 + Duration::from_millis(200)),
            SnapWaitAction::WaitFor(Duration::from_millis(200))
        );
        assert_eq!(
            s.poll_action(t0 + POST_SWITCH_SETTLE_SECOND),
            SnapWaitAction::RunNow
        );
        assert_eq!(
            s.poll_action(t0 + POST_SWITCH_SETTLE_SECOND),
            SnapWaitAction::WaitForever
        );
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
