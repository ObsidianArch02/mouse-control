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
use crate::{log_debug, log_info};
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::ffi::{c_void, CString};
use std::sync::{Mutex, OnceLock};

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

static ACTIVE_APP: Mutex<Option<AppIdentity>> = Mutex::new(None);

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

extern "C" fn handle_app_activation(_self: *mut c_void, _cmd: Sel, _notification: *mut c_void) {
    // 通知携带的 app 是权威值;极少数缺失 userInfo 的情况回退到 frontmostApplication。
    // The app carried by the notification is authoritative; fall back only when its
    // userInfo is missing.
    let identity = unsafe {
        let app = app_from_notification(_notification);
        if app.is_null() {
            refresh_from_frontmost()
        } else {
            identity_from_running_app(app)
        }
    };
    if !set_active(identity) {
        return;
    }
    log_debug!("[app] frontmost app changed: {:?}", active_identity());
    // 切 app 前先释放已按住的侧键目标键,避免旧 app 的 cmd/ctrl/opt/shift 卡在新 app。
    // Release held side-button targets before the new app receives events, so modifiers from
    // the old app never stick in the new app.
    crate::mouse::buttons::release_all();
    // 解析结果现在包含 app,旧缓存不再可靠;失效后按需重建。
    // Resolved state now includes the app, so stale cache entries are invalid; rebuild on demand.
    crate::mouse::resolve::invalidate_cache();
    // 指针加速可能按 app 覆盖,切 app 时重算并重应用。
    // Pointer acceleration may be overridden per app; re-evaluate and re-apply on switch.
    crate::mouse::pointer::apply();
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
    let notification_name = make_nsstring("NSWorkspaceDidActivateApplicationNotification");
    let _: () = msg_send![
        center,
        addObserver: target,
        selector: sel!(handleWorkspaceDidActivateApplication:),
        name: notification_name,
        object: std::ptr::null::<AnyObject>()
    ];
    CFRelease(notification_name as *const c_void);
    log_info!("[app] active-app notification observer installed.");
}

/// 主线程启动时调用一次:初始化当前前台 app,并注册后续激活通知。
/// Called once on the main thread at startup: initializes the current frontmost app and
/// registers the activation observer.
pub(crate) fn start_monitor() {
    unsafe {
        let initial = refresh_from_frontmost();
        set_active(initial);
        install_observer();
    }
    log_debug!("[app] active-app monitor started: {:?}", active_identity());
}
