//! 鼠标模块的 FFI 声明与事件 tap 基础设施:CoreGraphics/CoreFoundation/IOKit 的 extern、
//! 语义常量与通用 tap 创建流程。原共享 event_tap.rs 中鼠标需要的部分全部收进本模块
//! (窗口切换器及其键盘 event tap 已移除)。
//!
//! Mouse-module FFI declarations and event-tap infrastructure: CoreGraphics/CoreFoundation/
//! IOKit externs, semantic constants, and the generic tap-creation flow. Everything the mouse
//! module needs from the old shared event_tap.rs lives here now (the window switcher and its
//! keyboard tap are gone).

use crate::ffi::has_accessibility_permission;
use crate::log_info;
use objc2::runtime::{AnyObject, Sel};
use std::ffi::c_void;
use std::time::Duration;

// ========== 类型别名 / type aliases ==========

pub(crate) type CGEventRef = *mut c_void;
pub(crate) type CGEventTapProxy = *mut c_void;
pub(crate) type CFMachPortRef = *mut c_void;
pub(crate) type CFRunLoopSourceRef = *mut c_void;
pub(crate) type CFRunLoopRef = *mut c_void;
pub(crate) type CFStringRef = *mut c_void;
pub(crate) type CFAllocatorRef = *mut c_void;
pub(crate) type CGEventType = u32;
pub(crate) type CGEventFlags = u64;
pub(crate) type CGEventMask = u64;
#[cfg_attr(test, allow(dead_code))]
pub(crate) type CGEventSourceRef = *const c_void;
#[cfg_attr(test, allow(dead_code))]
pub(crate) type CGKeyCode = u16;

pub(crate) type CGEventTapCallBack = Option<
    unsafe extern "C" fn(
        proxy: CGEventTapProxy,
        event_type: CGEventType,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef,
>;

// ========== CGEventTap 语义常量 / semantic constants ==========
// 用语义化枚举替代裸数字,降低调用方硬编码出错概率。
// Semantic enums in place of raw magic numbers, reducing per-caller hardcoding errors.

/// CGEventTapCreate 的 tap location 参数。
/// Tap location for CGEventTapCreate.
#[allow(dead_code)]
pub(crate) mod tap_location {
    /// HID 层:最底层,能看到所有硬件事件。
    /// HID level: lowest, sees all hardware events.
    pub(crate) const HID_EVENT_TAP: i32 = 0;
    /// Session 层。
    /// Session level.
    pub(crate) const SESSION_EVENT_TAP: i32 = 1;
}

/// CGEventTapCreate 的 placement 参数。
/// Placement for CGEventTapCreate.
#[allow(dead_code)]
pub(crate) mod tap_placement {
    /// 队首插入:最先看到事件。
    /// Head insert: sees events first.
    pub(crate) const HEAD_INSERT: i32 = 0;
    /// 队尾插入:最后看到事件。
    /// Tail insert: sees events last.
    pub(crate) const TAIL_INSERT: i32 = 1;
}

/// CGEventTapCreate 的 options 参数。
/// 注意:枚举值与直觉相反(见 CGEventTypes.h),Default=0 可改事件,ListenOnly=1 只读。
/// Options for CGEventTapCreate. Note the counterintuitive values (see CGEventTypes.h):
/// Default=0 is mutable, ListenOnly=1 is read-only.
#[allow(dead_code)]
pub(crate) mod tap_options {
    /// 默认 tap:可修改/丢弃事件(需要 AX 权限)。
    /// Default tap: may modify/drop events (requires AX permission).
    pub(crate) const DEFAULT_TAP: u32 = 0;
    /// 只听不改:不能修改事件。
    /// Listen only: cannot modify events.
    pub(crate) const LISTEN_ONLY: u32 = 1;
}

// ========== CoreGraphics FFI ==========

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub(crate) fn CGEventTapCreate(
        tap: i32,
        place: i32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    pub(crate) fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub(crate) fn CGEventGetIntegerValueField(event: CGEventRef, field: i32) -> i64;
    pub(crate) fn CGEventSetIntegerValueField(event: CGEventRef, field: i32, value: i64);
    pub(crate) fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    pub(crate) fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    // 创建一个空 CGEvent。source 传 null 表示默认 source;后续用字段 55 设置事件类型。
    // Create an empty CGEvent. source=null uses the default source; set field 55 afterwards
    // for the event type.
    pub(crate) fn CGEventCreate(source: *const c_void) -> CGEventRef;
    // 合成键盘事件。source 传 null 表示默认 source;key_down 用 1/0 对应 Boolean。
    // Create a synthetic keyboard event. null source = default; key_down uses 1/0 (Boolean).
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: CGKeyCode,
        key_down: u8,
    ) -> CGEventRef;
    // 从 CGEvent 提取底层 IOHIDEvent(公开 API);用于事件归因(按设备匹配配置)。
    // Extract the underlying IOHIDEvent from a CGEvent (public API); used for event attribution
    // (matching events to the producing device for per-device config).
    pub(crate) fn CGEventCopyIOHIDEvent(event: CGEventRef) -> *mut c_void;

    // 读事件坐标(全局显示坐标:主屏左上原点,y 向下,与 kCGWindowBounds 同一坐标系)。
    // 用于滚动事件的"光标下窗口" app 归因。
    // Event location in global display coordinates (origin at the main display's top-left,
    // y down; the same space as kCGWindowBounds). Used for window-under-cursor app
    // attribution of scroll events.
    pub(crate) fn CGEventGetLocation(event: CGEventRef) -> CGPointFFI;
    // 写入合成事件的光标坐标,保证基于坐标分发的宿主应用能正确路由滚轮事件。
    // Set the cursor location on synthetic events so coordinate-routed apps dispatch accurately.
    pub(crate) fn CGEventSetLocation(event: CGEventRef, location: CGPointFFI);

    // 合成全新的滚轮事件。source 传 null 表示用默认 source;wheelCount 通常为 2
    // (wheel1=垂直,wheel2=水平)。
    // Create a brand-new scroll wheel event. source=null for the default source; wheelCount is
    // usually 2 (wheel1=vertical, wheel2=horizontal).
    pub(crate) fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;

    // 将事件投递到指定 tap 层级。kCGSessionEventTap=1 投递到 session 层,
    // 不经过 HID 层 tap,绕过系统自然滚动的 HID 层覆盖。
    // Post an event to a tap level. kCGSessionEventTap=1 posts to the session level,
    // bypassing HID-level taps and thus the system's natural-scroll override at the HID layer.
    pub(crate) fn CGEventPost(tap: i32, event: CGEventRef);
}

// ========== CoreFoundation FFI ==========

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;
    pub(crate) fn CFMachPortInvalidate(port: CFMachPortRef);

    pub(crate) fn CFRunLoopAddSource(
        rl: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: CFStringRef,
    );
    pub(crate) fn CFRunLoopRemoveSource(
        rl: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: CFStringRef,
    );
    pub(crate) fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub(crate) fn CFRunLoopRun();
    pub(crate) fn CFRunLoopStop(rl: CFRunLoopRef);

    pub(crate) static kCFRunLoopDefaultMode: CFStringRef;
}

pub(crate) const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: CGEventType = 0xFFFF_FFFE;
pub(crate) const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: CGEventType = 0xFFFF_FFFF;

// ========== 窗口归因 FFI / window attribution FFI ==========
// 滚动事件的 app 归因按"光标下的窗口"进行(macOS 把滚轮派发给光标下的窗口,不是
// key window)。CGWindowListCopyWindowInfo 枚举屏幕上的窗口(顺序为从前往后),
// 每个元素是 CFDictionary,键为 kCGWindow* 常量。
// Scroll events attribute the app by "the window under the cursor" (macOS dispatches
// wheel events to the window under the pointer, not the key window).
// CGWindowListCopyWindowInfo enumerates on-screen windows front-to-back; each element is
// a CFDictionary keyed by the kCGWindow* constants.

/// CGWindowListCopyWindowInfo 选项:只取屏幕上的窗口。
/// CGWindowListCopyWindowInfo option: on-screen windows only.
#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
/// CGWindowListCopyWindowInfo 选项:排除桌面元素(桌面图标窗口)。
/// CGWindowListCopyWindowInfo option: exclude desktop elements (desktop icon windows).
#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CF_NUMBER_SINT64_TYPE: isize = 4;

/// CFNumberType 的 kCFNumberIntType = 9(枚举常量不是导出符号,直接用值)。
/// CFNumberType kCFNumberIntType = 9 (an enum constant, not an exported symbol; raw value).
#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CF_NUMBER_INT_TYPE: isize = 9;
#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CF_NUMBER_FLOAT64_TYPE: isize = 6;

/// 按值传递的 CGPoint(f64 x/y)。
/// CGPoint passed by value (f64 x/y).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct CGPointFFI {
    pub x: f64,
    pub y: f64,
}

/// 按值传递的 CGSize(f64 width/height)。
/// CGSize passed by value (f64 width/height).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct CGSizeFFI {
    pub width: f64,
    pub height: f64,
}

/// 按值传递的 CGRect(origin + size)。
/// CGRect passed by value (origin + size).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct CGRectFFI {
    pub origin: CGPointFFI,
    pub size: CGSizeFFI,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    // 枚举屏幕上的窗口,返回 CFArrayRef(CFDictionary 数组,+1 由调用方 CFRelease)。
    // 参数 2(relativeToWindow)在不指定参照窗口时传 0。
    // On-screen window list as a CFArrayRef of CFDictionarys (+1 owned; caller CFReleases).
    // Pass 0 for relativeToWindow when no reference window is needed.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGWindowListCopyWindowInfo(
        option: u32,
        relative_to_window: u32,
    ) -> *const c_void;

    // 从 kCGWindowBounds 字典还原 CGRect(失败返回 false)。
    // Restore a CGRect from a kCGWindowBounds dictionary (false on failure).
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGRectMakeWithDictionaryRepresentation(
        dict: *const c_void,
        rect: *mut CGRectFFI,
    ) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    // CF 字典按键取值(借用,不持有)。
    // Borrowed value lookup in a CF dictionary (not owned).
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CFDictionaryGetValue(
        the_dict: *const c_void,
        key: *const c_void,
    ) -> *const c_void;

    // 读 CFNumber(成功返回 true 并写入 value)。
    // Read a CFNumber (returns true and writes value on success).
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CFNumberGetValue(
        number: *const c_void,
        the_type: isize,
        value: *mut c_void,
    ) -> bool;

    // 窗口字典键:属主 PID、frame(kCGWindowBounds 是嵌套字典)、窗口层、不透明度。
    // Window dictionary keys: owner PID, frame (kCGWindowBounds is a nested dictionary),
    // window layer, alpha.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowOwnerPID: CFStringRef;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowBounds: CFStringRef;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowLayer: CFStringRef;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowAlpha: CFStringRef;
}

// ========== 通用 tap 创建流程 / generic tap-creation flow ==========

// 缺 Accessibility 权限时,event tap 创建会失败。每隔 RETRY_INTERVAL 重试一次,最多 RETRY_MAX 次
// (约 2 分钟),期间用户可在系统设置里授权;超过上限就记日志放弃。设上限是为了避免无限轮询;
// 用户授权后下次重试即建成,无需重启。`cancel` 为可选取消标志:置位时重试循环提前退出
// (用于运行时停用鼠标 tap,避免重试期间 join 阻塞调用线程)。
//
// When Accessibility permission is missing, CGEventTapCreate fails. Retry every RETRY_INTERVAL up
// to RETRY_MAX times (~2 min), during which the user can grant permission in System Settings; once
// the limit is exhausted, log and give up. The cap avoids infinite polling; once granted, the next
// retry succeeds -- no restart needed. `cancel` is an optional flag: when set, the retry loop bails
// out early (used when stopping the mouse tap at runtime so join() doesn't block the caller during
// the retry window).
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const RETRY_MAX: u32 = 40;

/// 创建 event tap 并加入当前线程的 CFRunLoop。失败时按 RETRY_INTERVAL/RETRY_MAX 重试。
/// 返回创建好的 tap(或 None 表示重试耗尽)。
///
/// Create an event tap and add it to the current thread's CFRunLoop, retrying on failure.
/// Returns the created tap (or None if retries exhausted).
///
/// # Safety
/// 调用方必须在专用线程上调用(后续 CFRunLoopRun 会阻塞该线程)。
/// Caller must invoke on a dedicated thread (CFRunLoopRun will block it afterwards).
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn create_tap_with_retry(
    location: i32,
    placement: i32,
    options: u32,
    mask: CGEventMask,
    callback: CGEventTapCallBack,
    user_info: *mut c_void,
    log_name: &str,
    cancel: Option<&'static std::sync::atomic::AtomicBool>,
) -> Option<CFMachPortRef> {
    let mut tap = CGEventTapCreate(location, placement, options, mask, callback, user_info);

    // 首次创建失败(通常是缺 Accessibility 权限):有限次重试,给用户时间去系统设置授权。
    // First creation failed (usually missing Accessibility): retry a bounded number of times
    // to give the user time to grant permission in System Settings.
    if tap.is_null() {
        log_info!(
            "[{}] No Accessibility permission yet; event tap will retry every {:?} up to {} times (~{}s).",
            log_name,
            RETRY_INTERVAL,
            RETRY_MAX,
            RETRY_INTERVAL.as_secs() * RETRY_MAX as u64
        );
        let mut granted = false;
        for _ in 0..RETRY_MAX {
            std::thread::sleep(RETRY_INTERVAL);
            // 取消请求(运行时停用):立即放弃,线程正常结束。
            // Stop requested (runtime disable): bail out so the thread exits promptly.
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                log_info!("[{}] Event tap cancelled by stop request.", log_name);
                return None;
            }
            if has_accessibility_permission() {
                tap = CGEventTapCreate(location, placement, options, mask, callback, user_info);
                if !tap.is_null() {
                    granted = true;
                    break;
                }
            }
        }
        if granted {
            log_info!(
                "[{}] Accessibility permission granted; event tap created.",
                log_name
            );
        } else {
            log_info!(
                "[{}] Event tap retry exhausted ({}x). Disabled until restart. \
                 Grant Accessibility in System Settings and relaunch; if a mouse-control entry \
                 from a previous build is still listed, remove it first.",
                log_name,
                RETRY_MAX
            );
            return None;
        }
    }

    Some(tap)
}

// ========== IOKit 私有 SPI / IOKit private SPI ==========

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IOHIDEventSystemClient:设备枚举入口(LinearMouse PointerDeviceManager 同款)。
    // IOHIDEventSystemClient: device enumeration entry (same as LinearMouse's PointerDeviceManager).
    pub(crate) fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> *mut c_void;
    pub(crate) fn IOHIDEventSystemClientSetMatchingMultiple(
        client: *mut c_void,
        matchings: *const c_void,
    );
    // 返回 CFArrayRef(IOHIDServiceClient 列表),调用方持有 +1。
    // Returns a CFArrayRef of IOHIDServiceClient (+1 owned by caller).
    pub(crate) fn IOHIDEventSystemClientCopyServices(client: *mut c_void) -> *mut c_void;
    // 按 senderID 反查 IOHIDServiceClient(+1 owned,调用方 CFRelease)。
    // 这是归因链的关键:IOHIDEventGetSenderID 的返回值传给此函数,得到的 IOHIDServiceClient
    // 与枚举列表(CopyServices)中的对象是同一实例(指针可比对)。LinearMouse 同款链路。
    // Look up an IOHIDServiceClient by sender ID (+1 owned, caller CFReleases). This is the key
    // link of the attribution chain: the IOHIDEventGetSenderID result feeds this function, and
    // the returned IOHIDServiceClient is the same instance as in the enumerated list (pointer
    // comparison works). Same chain as LinearMouse.
    pub(crate) fn IOHIDEventSystemClientCopyServiceForRegistryID(
        client: *mut c_void,
        registry_id: u64,
    ) -> *mut c_void;
    // 把 client 调度到 runloop:未调度的 client 匹配/registry-ID 映射不完整,归因用的
    // CopyServiceForRegistryID 会持续失败(实测)。调度后匹配常驻,归因可靠。
    // Schedule the client on a runloop: an unscheduled client's matching / registry-ID map is
    // incomplete and CopyServiceForRegistryID keeps failing (measured). Scheduling keeps the
    // matching live and makes attribution reliable.
    pub(crate) fn IOHIDEventSystemClientScheduleWithRunLoop(
        client: *mut c_void,
        runloop: CFRunLoopRef,
        mode: *const c_void,
    );
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn IOHIDEventSystemClientUnscheduleWithRunLoop(
        client: *mut c_void,
        runloop: CFRunLoopRef,
        mode: *const c_void,
    );
}

// ========== 事件归因:IOHIDEvent sender ID(私有 SPI)/ Event attribution ==========
// CGEventCopyIOHIDEvent 是公开 CoreGraphics 函数(见上),取出 CGEvent 内层 IOHIDEvent。
// 以下两个是私有 SPI,用来从 IOHIDEvent 读 senderID 并映射回设备。
// CGEventCopyIOHIDEvent is a public CoreGraphics function (declared above) that extracts the
// IOHIDEvent inside a CGEvent. The two below are private SPI for reading the sender ID from the
// IOHIDEvent and mapping it back to a device.
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// 读 IOHIDEvent 的 sender ID(= 产生该事件的 IORegistry entry ID)。
    /// Read the sender ID of an IOHIDEvent (= the IORegistry entry ID of the producing device).
    pub(crate) fn IOHIDEventGetSenderID(event: *mut c_void) -> u64;

    /// 读 IOHIDEvent 的整数字段(公开 API):用于按钮事件诊断,读取真实 HID 按钮号/掩码。
    /// Read an integer field from an IOHIDEvent (public API); used for button-event
    /// diagnostics to get the real HID button number/mask.
    pub(crate) fn IOHIDEventGetIntegerValue(event: *mut c_void, field: u64) -> i64;
}

// IOHIDEventTypeButton=5 的字段(见 IOKit IOHIDEventTypes.h):按钮掩码与按钮号。
// Button-event fields (IOHIDEventTypes.h): button mask and button number.
pub(crate) const K_IOHID_EVENT_FIELD_BUTTON_MASK: u64 = 0x0005_0001;
pub(crate) const K_IOHID_EVENT_FIELD_BUTTON_NUMBER: u64 = 0x0005_0002;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// IOHIDManager:设备插拔通知(公开 API)。用 IOHIDManager 监听鼠标/触控板接入/移除,
    /// 触发注册表重建——蓝牙设备休眠断连重连等场景下,事件驱动地刷新归因链,避免
    /// 旧 IOHIDEventSystemClient 缓存过期导致归因持续失败。
    ///
    /// IOHIDManager: device plug/unplug notifications (public API). Used to rebuild the registry
    /// on mouse/trackpad attach/detach -- event-driven refresh of the attribution chain for cases
    /// like Bluetooth disconnect/reconnect, where the stale IOHIDEventSystemClient cache would
    /// otherwise keep attribution failing.
    pub(crate) fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> *mut c_void;
    pub(crate) fn IOHIDManagerSetDeviceMatchingMultiple(
        manager: *mut c_void,
        multiple: *const c_void,
    );
    // IOHIDDeviceCallback: void (*)(void *context, IOReturn result, void *sender, IOHIDDeviceCallbackRef callback)
    pub(crate) fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    );
    pub(crate) fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: *mut c_void,
        callback: Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void)>,
        context: *mut c_void,
    );
    pub(crate) fn IOHIDManagerScheduleWithRunLoop(
        manager: *mut c_void,
        runloop: *const c_void,
        mode: *const c_void,
    );
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn IOHIDServiceClientGetRegistryID(client: *mut c_void) -> *const c_void;
    // CopyProperty 返回 +1 CFTypeRef,调用方负责 CFRelease。
    // CopyProperty returns +1 CFTypeRef; caller must CFRelease.
    pub(crate) fn IOHIDServiceClientCopyProperty(
        client: *mut c_void,
        key: *const c_void,
    ) -> *mut c_void;
    pub(crate) fn IOHIDServiceClientSetProperty(
        client: *mut c_void,
        key: *const c_void,
        value: *mut c_void,
    ) -> bool;
    // 判断 HID service 是否符合某 (usage page, usage) 对(公开 API,系统 SDK 自带)。
    // 比读 PrimaryUsage 单值可靠:有些真实鼠标(如 ATK A9 SE 这类 Nearlink/星闪设备)
    // 的 PrimaryUsage 被系统报成 Keyboard(6),但它的 DeviceUsagePairs 声明了 Mouse(1,2),
    // ConformsTo 能识别真实用途;反之有些键盘也声明了多余的 Mouse 用途,会被一并纳入
    // (与 LinearMouse 行为一致,归因靠 senderID 精确匹配不受影响)。
    //
    // Check whether a HID service conforms to a (usage page, usage) pair (public API from the
    // system SDK). More reliable than reading PrimaryUsage alone: some real mice (e.g. ATK A9 SE
    // Nearlink devices) report PrimaryUsage = Keyboard(6), yet declare Mouse(1,2) in their
    // DeviceUsagePairs, which ConformsTo sees; conversely some keyboards declare extra Mouse
    // usages and get included too (same behavior as LinearMouse; attribution uses exact senderID
    // matching, so this doesn't affect correctness).
    pub(crate) fn IOHIDServiceClientConformsTo(
        client: *mut c_void,
        usage_page: u32,
        usage: u32,
    ) -> i32;
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) unsafe fn service_registry_id(service: *mut c_void) -> Option<u64> {
    if service.is_null() {
        return None;
    }
    let num = IOHIDServiceClientGetRegistryID(service);
    if num.is_null() {
        return None;
    }
    let mut id: u64 = 0;
    if CFNumberGetValue(
        num,
        K_CF_NUMBER_SINT64_TYPE,
        &mut id as *mut u64 as *mut c_void,
    ) && id != 0
    {
        Some(id)
    } else {
        None
    }
}

// ========== CFArray 遍历 / CFArray iteration ==========
// 遍历 CFArrayRef 用 C 函数而不是 msg_send!(objectAtIndex:):后者会被 objc2 的
// 类型编码校验拦截(返回编码 @ vs Rust 声明 ^v,运行时 panic)。
// Iterating a CFArrayRef uses C functions instead of msg_send!(objectAtIndex:): the latter
// trips objc2's runtime type-encoding validation (method returns '@', Rust declares '^v').
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    /// CFIndex(设备数)。
    /// CFIndex (device count).
    pub(crate) fn CFArrayGetCount(the_array: *const c_void) -> isize;
    /// 按索引取元素(借用,不持有)。
    /// Element by index (borrowed, not owned).
    pub(crate) fn CFArrayGetValueAtIndex(the_array: *const c_void, idx: isize) -> *const c_void;
}

// ========== 滚轮事件字段常量 / scroll wheel event field constants ==========
// 见 CGEventTypes.h。
// See CGEventTypes.h.

/// 垂直滚动量(整数,行级)。field 11。
/// Vertical scroll delta (integer, line-level). field 11.
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1: i32 = 11;
/// 水平滚动量(整数,行级)。field 12。
/// Horizontal scroll delta (integer, line-level). field 12.
#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2: i32 = 12;

/// 是否为连续(像素级)滚动事件。field 88。0=离散(行级),1=连续(触控板式)。
/// Whether the event is continuous (pixel-level) scroll. field 88. 0=discrete (line), 1=continuous (trackpad).
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS: i32 = 88;

/// CGEvent 类型字段。field 55。合成连续事件时用于显式设为 22(kCGEventScrollWheel)。
/// CGEvent type field. field 55. Used when synthesizing continuous events to explicitly
/// set 22 (kCGEventScrollWheel).
pub(crate) const K_CG_EVENT_TYPE: i32 = 55;

/// 像素级滚动量(垂直)。field 96。
/// Pixel-level vertical scroll delta. field 96.
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1: i32 = 96;
/// 像素级滚动量(水平)。field 97。
/// Pixel-level horizontal scroll delta. field 97.
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2: i32 = 97;
/// 固定点滚动量(垂直,16.16)。field 93。
/// Fixed-point vertical scroll delta (16.16). field 93.
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1: i32 = 93;
/// 固定点滚动量(水平,16.16)。field 94。
/// Fixed-point horizontal scroll delta (16.16). field 94.
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2: i32 = 94;

// ========== 事件合成相关常量 / synthetic event constants ==========

/// CGEventPost 的 tap location:kCGSessionEventTap=1。
/// 合成事件 post 到 session 层,不经过 HID 层 tap,绕过系统自然滚动覆盖。
/// CGEventPost tap location: kCGSessionEventTap=1.
/// Synthetic events posted at session level bypass HID-level taps, avoiding the system's
/// natural-scroll override at the HID layer.
pub(crate) const K_CG_SESSION_EVENT_TAP: i32 = 1;

/// CGEventPost 的 HID 层 tap location(kCGHIDEventTap=0)。媒体/系统键
/// (NSSystemDefined)按权威实现投递到这一层才生效。
/// CGEventPost HID-level tap location (kCGHIDEventTap=0). Media/system keys
/// (NSSystemDefined) only take effect when posted at this level, per the reference impl.
#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_HID_EVENT_TAP: i32 = 0;

/// CGEventCreateScrollWheelEvent2 的 units:kCGScrollEventUnitLine=1(行级,离散滚动)。
/// CGEventCreateScrollWheelEvent2 units: kCGScrollEventUnitLine=1 (line-level, discrete scroll).
pub(crate) const K_CG_SCROLL_EVENT_UNIT_LINE: u32 = 1;

/// kCGScrollEventUnitPixel=0(像素级,离散滚动;line_height 模式用)。
/// kCGScrollEventUnitPixel=0 (pixel-level, discrete scroll; used by the line_height mode).
#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_SCROLL_EVENT_UNIT_PIXEL: u32 = 0;

/// eventSourceUserData 字段(field 42)。用于在合成事件上打标记,防止自己的 tap 无限循环。
/// eventSourceUserData field (field 42). Used to tag synthetic events so our own tap can
/// recognize and skip them, preventing infinite loops.
pub(crate) const K_CG_EVENT_SOURCE_USER_DATA: i32 = 42;

/// 合成事件标记魔数(ASCII "OMTSCRL")。写入 eventSourceUserData,我们的 tap 据此跳过。
/// Synthetic-event marker magic (ASCII "OMTSCRL"). Written to eventSourceUserData so our tap
/// can recognize and skip our own synthetic events.
#[allow(dead_code, clippy::unusual_byte_groupings)]
pub(crate) const SYNTHETIC_MARKER: i64 = 0x4F4D_5453_4352_4C;

/// Mos.app 滚轮 tap 的放行标记(ASCII "MOSSMOOT")。写入后合成滚轮事件在下游
/// kCGAnnotatedSessionEventTap 会被 Mos 直接放行,避免二次平滑或二次反转。
/// Pass-through marker checked by Mos.app's scroll tap (ASCII "MOSSMOOT"). Writing this
/// value makes Mos skip our synthetic scroll events at kCGAnnotatedSessionEventTap without
/// double smoothing or reversal.
#[allow(clippy::unusual_byte_groupings)]
pub(crate) const MOS_SYNTHETIC_MARKER: i64 = 0x4D4F_5353_4D4F_4F54;

#[inline]
pub(crate) fn is_synthetic_scroll_marker(user_data: i64) -> bool {
    user_data == SYNTHETIC_MARKER || user_data == MOS_SYNTHETIC_MARKER
}

// ========== 属性键常量 / property key constants ==========

/// 私有键:线性缩放开关(macOS 14 Sonoma+)。1 = 线性跟踪(无加速)。
/// Private key: linear-scaling switch (macOS 14 Sonoma+). 1 = linear tracking (no acceleration).
pub(crate) const KEY_LINEAR_SCALING: &str = "HIDUseLinearScalingMouseAcceleration";
/// 每设备指针加速(现代 macOS)。
/// Per-device pointer acceleration (modern macOS).
pub(crate) const KEY_POINTER_ACCEL: &str = "HIDPointerAcceleration";
/// 鼠标加速类型键(旧系统回退)。
/// Mouse acceleration type key (legacy fallback).
pub(crate) const KEY_MOUSE_ACCEL: &str = "HIDMouseAcceleration";
/// 设备主用途页(matching 用:按 Generic Desktop 页过滤后,再用 ConformsTo 判定指针设备)。
/// Device primary usage page (for matching: filter to the Generic Desktop page, then decide
/// pointer devices via ConformsTo).
pub(crate) const KEY_PRIMARY_USAGE_PAGE: &str = "PrimaryUsagePage";
/// 设备产品名(日志用)。
/// Device product name (for logs).
pub(crate) const KEY_PRODUCT: &str = "Product";
/// 设备厂商 ID(USB VID)。
/// Device vendor ID (USB VID).
pub(crate) const KEY_VENDOR_ID: &str = "VendorID";
/// 设备产品 ID(USB PID)。
/// Device product ID (USB PID).
pub(crate) const KEY_PRODUCT_ID: &str = "ProductID";
/// 设备连接方式(USB/Bluetooth/BLE)。
/// Device transport (USB/Bluetooth/BLE).
pub(crate) const KEY_TRANSPORT: &str = "Transport";

// ========== 键盘事件修饰键标志 / keyboard modifier flag constants ==========
// 见 CGEventTypes.h 的 CGEventFlags。
// See CGEventTypes.h (CGEventFlags).

pub(crate) const K_CG_EVENT_FLAG_MASK_SHIFT: u64 = 1 << 17;
pub(crate) const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 1 << 18;
pub(crate) const K_CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 1 << 19;
pub(crate) const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;

// ========== 常用虚拟键码 / common virtual key codes ==========
// 值来自 Apple Events.h 的 kVK_* 常量(US 布局物理键位)。
// Values from Apple Events.h kVK_* constants (US-layout physical keys).

pub(crate) const K_VK_ANSI_A: u16 = 0x00;
pub(crate) const K_VK_ANSI_B: u16 = 0x0B;
pub(crate) const K_VK_ANSI_C: u16 = 0x08;
pub(crate) const K_VK_ANSI_D: u16 = 0x02;
pub(crate) const K_VK_ANSI_E: u16 = 0x0E;
pub(crate) const K_VK_ANSI_F: u16 = 0x03;
pub(crate) const K_VK_ANSI_G: u16 = 0x05;
pub(crate) const K_VK_ANSI_H: u16 = 0x04;
pub(crate) const K_VK_ANSI_I: u16 = 0x22;
pub(crate) const K_VK_ANSI_J: u16 = 0x26;
pub(crate) const K_VK_ANSI_K: u16 = 0x28;
pub(crate) const K_VK_ANSI_L: u16 = 0x25;
pub(crate) const K_VK_ANSI_M: u16 = 0x2E;
pub(crate) const K_VK_ANSI_N: u16 = 0x2D;
pub(crate) const K_VK_ANSI_O: u16 = 0x1F;
pub(crate) const K_VK_ANSI_P: u16 = 0x23;
pub(crate) const K_VK_ANSI_Q: u16 = 0x0C;
pub(crate) const K_VK_ANSI_R: u16 = 0x0F;
pub(crate) const K_VK_ANSI_S: u16 = 0x01;
pub(crate) const K_VK_ANSI_T: u16 = 0x11;
pub(crate) const K_VK_ANSI_U: u16 = 0x20;
pub(crate) const K_VK_ANSI_V: u16 = 0x09;
pub(crate) const K_VK_ANSI_W: u16 = 0x0D;
pub(crate) const K_VK_ANSI_X: u16 = 0x07;
pub(crate) const K_VK_ANSI_Y: u16 = 0x10;
pub(crate) const K_VK_ANSI_Z: u16 = 0x06;
pub(crate) const K_VK_ANSI_0: u16 = 0x1D;
pub(crate) const K_VK_ANSI_1: u16 = 0x12;
pub(crate) const K_VK_ANSI_2: u16 = 0x13;
pub(crate) const K_VK_ANSI_3: u16 = 0x14;
pub(crate) const K_VK_ANSI_4: u16 = 0x15;
pub(crate) const K_VK_ANSI_5: u16 = 0x17;
pub(crate) const K_VK_ANSI_6: u16 = 0x16;
pub(crate) const K_VK_ANSI_7: u16 = 0x1A;
pub(crate) const K_VK_ANSI_8: u16 = 0x1C;
pub(crate) const K_VK_ANSI_9: u16 = 0x19;
pub(crate) const K_VK_ANSI_LEFT_BRACKET: u16 = 0x21;
pub(crate) const K_VK_ANSI_RIGHT_BRACKET: u16 = 0x1E;
pub(crate) const K_VK_ANSI_SEMICOLON: u16 = 0x29;
pub(crate) const K_VK_ANSI_QUOTE: u16 = 0x27;
pub(crate) const K_VK_ANSI_COMMA: u16 = 0x2B;
pub(crate) const K_VK_ANSI_PERIOD: u16 = 0x2F;
pub(crate) const K_VK_ANSI_SLASH: u16 = 0x2C;
pub(crate) const K_VK_ANSI_MINUS: u16 = 0x1B;
pub(crate) const K_VK_ANSI_EQUAL: u16 = 0x18;
pub(crate) const K_VK_ANSI_GRAVE: u16 = 0x32;
pub(crate) const K_VK_ANSI_BACKSLASH: u16 = 0x2A;
pub(crate) const K_VK_RETURN: u16 = 0x24;
pub(crate) const K_VK_TAB: u16 = 0x30;
pub(crate) const K_VK_SPACE: u16 = 0x31;
pub(crate) const K_VK_DELETE: u16 = 0x33;
pub(crate) const K_VK_ESCAPE: u16 = 0x35;
pub(crate) const K_VK_FORWARD_DELETE: u16 = 0x75;
pub(crate) const K_VK_HOME: u16 = 0x73;
pub(crate) const K_VK_END: u16 = 0x77;
pub(crate) const K_VK_PAGE_UP: u16 = 0x74;
pub(crate) const K_VK_PAGE_DOWN: u16 = 0x79;
pub(crate) const K_VK_LEFT_ARROW: u16 = 0x7B;
pub(crate) const K_VK_RIGHT_ARROW: u16 = 0x7C;
pub(crate) const K_VK_DOWN_ARROW: u16 = 0x7D;
pub(crate) const K_VK_UP_ARROW: u16 = 0x7E;
pub(crate) const K_VK_COMMAND: u16 = 0x37;
pub(crate) const K_VK_SHIFT: u16 = 0x38;
pub(crate) const K_VK_OPTION: u16 = 0x3A;
pub(crate) const K_VK_CONTROL: u16 = 0x3B;
pub(crate) const K_VK_CAPS_LOCK: u16 = 0x39;
// 系统功能键(非标准 128 以内的键码,实测可用 CGEventCreateKeyboardEvent 模拟)。
// System function keys (outside the standard 0..0x7F range; CGEventCreateKeyboardEvent works).
pub(crate) const K_VK_MISSION_CONTROL: u16 = 0xA0;
pub(crate) const K_VK_LAUNCHPAD: u16 = 0x82;

// ========== 媒体/系统键(NSSystemDefined) / media & system keys ==========
// 值来自 IOKit/hidsystem/ev_keymap.h 的 NX_KEYTYPE_*。
// Values from IOKit/hidsystem/ev_keymap.h (NX_KEYTYPE_*).

pub(crate) const K_NX_KEYTYPE_SOUND_UP: i32 = 0;
pub(crate) const K_NX_KEYTYPE_SOUND_DOWN: i32 = 1;
pub(crate) const K_NX_KEYTYPE_BRIGHTNESS_UP: i32 = 2;
pub(crate) const K_NX_KEYTYPE_BRIGHTNESS_DOWN: i32 = 3;
pub(crate) const K_NX_KEYTYPE_MUTE: i32 = 7;
pub(crate) const K_NX_KEYTYPE_PLAY: i32 = 16;
pub(crate) const K_NX_KEYTYPE_NEXT: i32 = 17;
pub(crate) const K_NX_KEYTYPE_PREVIOUS: i32 = 18;

// ========== NSSystemDefined 事件合成 FFI / NSSystemDefined event FFI ==========
// 媒体键无法用 CGEventCreateKeyboardEvent 合成,要走 AppKit 的
// [NSEvent otherEventWithType:...] + [NSEvent CGEvent] + CGEventPost(kCGHIDEventTap)。
// 该调用带 NSPoint 结构体参数、返回 CF 指针,objc2 的 msg_send! 无法可靠编码,
// 与 AGENTS.md 约定一致,这里用裸 objc_msgSend FFI。
//
// Media keys cannot be synthesized with CGEventCreateKeyboardEvent; the working path is
// [NSEvent otherEventWithType:...] -> [NSEvent CGEvent] -> CGEventPost(kCGHIDEventTap).
// The call takes an NSPoint struct argument and returns a CF pointer, which objc2's
// msg_send! cannot reliably encode, so this uses raw objc_msgSend FFI per AGENTS.md.

/// 按值传递的 NSPoint(与 AppKit 的 {double, double} 结构体布局一致)。
/// NSPoint passed by value (layout identical to AppKit's {double, double} struct).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct NSPointFFI {
    pub x: f64,
    pub y: f64,
}

#[link(name = "AppKit", kind = "framework")]
#[allow(clashing_extern_declarations)] // 同一符号 objc_msgSend 按不同签名声明,属预期。
                                       // Same symbol objc_msgSend declared with different
                                       // signatures; intentional.
extern "C" {
    /// objc_msgSend 裸调用:构造 NSSystemDefined 事件
    /// (+[NSEvent otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:])。
    /// 返回 autorelease 的对象,调用方应立即使用并在使用后显式 release。
    /// Raw objc_msgSend: construct an NSSystemDefined event
    /// (+[NSEvent otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:]).
    /// Returns an autoreleased object; use it immediately and release explicitly afterwards.
    #[cfg_attr(test, allow(dead_code))]
    #[link_name = "objc_msgSend"]
    pub(crate) fn objc_msgSend_create_system_event(
        receiver: *mut AnyObject,
        selector: Sel,
        event_type: u64,
        location: NSPointFFI,
        modifier_flags: u64,
        timestamp: f64,
        window_number: isize,
        context: *mut AnyObject,
        subtype: i16,
        data1: isize,
        data2: isize,
    ) -> *mut AnyObject;

    /// objc_msgSend 裸调用:取 NSEvent 对应的 CGEvent(-[NSEvent CGEvent])。
    /// 返回 CGEventRef(不持有,仅借用 NSEvent 生命周期)。
    /// Raw objc_msgSend: fetch the CGEvent for an NSEvent (-[NSEvent CGEvent]).
    /// Returns a CGEventRef (borrowed; valid only while the NSEvent lives).
    #[cfg_attr(test, allow(dead_code))]
    #[link_name = "objc_msgSend"]
    pub(crate) fn objc_msgSend_cgevent(receiver: *mut AnyObject, selector: Sel) -> *mut c_void;

    /// objc_msgSend 裸调用:显式 retain,避免依赖线程是否存在 autorelease pool。
    /// Raw objc_msgSend: explicit retain, so we don't depend on an autorelease pool
    /// existing on this thread.
    #[cfg_attr(test, allow(dead_code))]
    #[link_name = "objc_msgSend"]
    pub(crate) fn objc_msgSend_retain(receiver: *mut AnyObject, selector: Sel) -> *mut AnyObject;

    /// objc_msgSend 裸调用:显式 release。
    /// Raw objc_msgSend: explicit release.
    #[cfg_attr(test, allow(dead_code))]
    #[link_name = "objc_msgSend"]
    pub(crate) fn objc_msgSend_release(receiver: *mut AnyObject, selector: Sel);
}

// HID 用途常量 / HID usage constants
/// kHIDPage_GenericDesktop = 0x01
pub(crate) const USAGE_PAGE_GENERIC_DESKTOP: i64 = 1;
/// kHIDUsage_GD_Pointer = 0x01
pub(crate) const USAGE_GD_POINTER: i64 = 1;
/// kHIDUsage_GD_Mouse = 0x02
pub(crate) const USAGE_GD_MOUSE: i64 = 2;
/// kHIDUsage_GD_Trackpad = 0x05
pub(crate) const USAGE_GD_TRACKPAD: i64 = 5;
