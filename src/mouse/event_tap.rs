//! 鼠标事件 event tap。
//! 在独立线程上建一个 default-tap 的 CGEventTap(HID 层),监听鼠标按键与滚轮事件。
//! 滚轮事件根据配置分两种模式处理:默认(透传+反转)、按行(固定行数)。
//! 两种模式统一使用合成事件方案:丢弃原始事件,合成新事件 post 到 session 层,
//! 绕过系统自然滚动在 HID 层的覆盖。
//!
//! Mouse event tap.
//! Spawns a default-tap CGEventTap (HID level) on a dedicated thread that listens for mouse
//! button and scroll events. Scroll events are processed in one of two modes: Default
//! (passthrough + optional reverse) or Line (fixed line count). Both use the synthetic-event
//! approach: drop the original, post a new event to the session level, bypassing the system
//! natural-scroll override at the HID layer.

use crate::app::{self, AppIdentity};
use crate::ffi::CFRelease;
use crate::mouse::buttons;
use crate::mouse::device;
use crate::mouse::ffi::{
    self, tap_location, tap_options, tap_placement, CFRunLoopGetCurrent, CGEventCreate,
    CGEventCreateScrollWheelEvent2, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventGetLocation, CGEventMask, CGEventPost, CGEventRef, CGEventSetFlags,
    CGEventSetIntegerValueField, CGEventTapProxy, CGEventType, K_CG_EVENT_SOURCE_USER_DATA,
    K_CG_EVENT_TYPE, K_CG_SCROLL_EVENT_UNIT_LINE, K_CG_SCROLL_EVENT_UNIT_PIXEL,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
    K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2, K_CG_SESSION_EVENT_TAP, SYNTHETIC_MARKER,
};
use crate::mouse::resolve;
use crate::mouse::scrolling::{compute_continuous_delta, compute_delta, ScrollMode, ScrollUnit};
use crate::{log_debug, log_info};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::thread;

// ========== 鼠标事件类型常量 / mouse event type constants ==========
// 见 CGEventType.h。
// See CGEventType.h.
const K_CG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;
const K_CG_EVENT_SCROLL_WHEEL: CGEventType = 22;

// kCGMouseEventButtonNumber 字段(field 3)。注意不是 field 0(kCGMouseEventNumber,
// 那是事件序号计数器,不是按钮号)。
// kCGMouseEventButtonNumber field (field 3). Not field 0 (kCGMouseEventNumber is a
// per-event-type sequence counter, not the button number).
const K_CG_MOUSE_EVENT_BUTTON_NUMBER: i32 = 3;

/// 合成一个滚轮事件并 post 到 session 层。
/// 单位由 ScrollUnit 决定:行单位(缺省/默认模式)或像素单位(line_height 模式);
/// 默认模式透传原 delta。
///
/// Synthesize a scroll event and post it to the session level.
/// The unit comes from ScrollUnit: lines (absent line_height / default mode) or pixels
/// (the line_height mode); Default mode passes the raw delta through.
unsafe fn post_scroll_event(dy: i32, dx: i32, units: ScrollUnit, flags: CGEventFlags) {
    let unit = match units {
        ScrollUnit::Line => K_CG_SCROLL_EVENT_UNIT_LINE,
        ScrollUnit::Pixel => K_CG_SCROLL_EVENT_UNIT_PIXEL,
    };
    let synthetic = CGEventCreateScrollWheelEvent2(std::ptr::null(), unit, 2, dy, dx, 0);

    if synthetic.is_null() {
        log_info!("[mouse] failed to synthesize scroll event");
        return;
    }

    CGEventSetFlags(synthetic, flags);
    CGEventSetIntegerValueField(synthetic, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);

    CGEventPost(K_CG_SESSION_EVENT_TAP, synthetic);
    CFRelease(synthetic as *const c_void);
}

/// 把像素级滚动量转换成连续滚动事件所需的 line/fixed-point 分量。
/// 1 line = 10 pixels;不足 1 line 的非零滚动至少向上取整到 +/-1,模仿 trackpad 事件。
///
/// Convert a pixel delta into the line/fixed-point components required by a continuous scroll
/// event. 1 line = 10 pixels; a non-zero delta smaller than one line is rounded away from zero
/// to at least +/-1, mimicking real trackpad events.
fn line_delta_from_point(point: i64) -> i64 {
    let line = point / 10;
    if point != 0 && line == 0 {
        point.signum()
    } else {
        line
    }
}

/// 合成一个连续(触控板式)滚轮事件并 post 到 session 层。
/// 与 Mac Mouse Fix 的 ContinuousScroll 输出一致:显式设置 type=22、continuous=1,
/// 同时写入 line / point / fixed-point delta。
///
/// Synthesize a continuous (trackpad-style) scroll event and post it to the session level.
/// Matches Mac Mouse Fix's ContinuousScroll output: explicitly set type=22, continuous=1,
/// and write line / point / fixed-point deltas together.
unsafe fn post_continuous_scroll_event(point_dy: i64, point_dx: i64, flags: CGEventFlags) {
    let synthetic = CGEventCreate(std::ptr::null());
    if synthetic.is_null() {
        log_info!("[mouse] failed to synthesize continuous scroll event");
        return;
    }

    let line_dy = line_delta_from_point(point_dy);
    let line_dx = line_delta_from_point(point_dx);

    CGEventSetIntegerValueField(synthetic, K_CG_EVENT_TYPE, K_CG_EVENT_SCROLL_WHEEL as i64);
    CGEventSetIntegerValueField(synthetic, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);

    CGEventSetIntegerValueField(synthetic, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, line_dy);
    CGEventSetIntegerValueField(
        synthetic,
        K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
        point_dy,
    );
    CGEventSetIntegerValueField(
        synthetic,
        K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1,
        line_dy * 65_536,
    );

    CGEventSetIntegerValueField(synthetic, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2, line_dx);
    CGEventSetIntegerValueField(
        synthetic,
        K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
        point_dx,
    );
    CGEventSetIntegerValueField(
        synthetic,
        K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
        line_dx * 65_536,
    );

    CGEventSetFlags(synthetic, flags);
    CGEventSetIntegerValueField(synthetic, K_CG_EVENT_SOURCE_USER_DATA, SYNTHETIC_MARKER);

    CGEventPost(K_CG_SESSION_EVENT_TAP, synthetic);
    CFRelease(synthetic as *const c_void);
}

/// 把事件类型翻译成可读名称,便于日志阅读。
/// Translate an event type into a readable name for log readability.
fn event_type_name(t: CGEventType) -> &'static str {
    match t {
        K_CG_EVENT_OTHER_MOUSE_DOWN => "other down",
        K_CG_EVENT_OTHER_MOUSE_UP => "other up",
        K_CG_EVENT_SCROLL_WHEEL => "scroll",
        _ => "other",
    }
}

/// 把 buttonNumber(0-based)翻译成可读名称,便于日志阅读。
/// 0=左,1=右,2=中;3/4=标准 HID 后退/前进侧键;其余用裸数字。
/// Translate buttonNumber (0-based) to a readable name for log readability.
fn button_name(button: i64) -> String {
    match button {
        0 => "left".to_string(),
        1 => "right".to_string(),
        2 => "middle".to_string(),
        3 => "back".to_string(),
        4 => "forward".to_string(),
        n => format!("btn{}", n),
    }
}

/// 诊断日志:打印某个鼠标按键事件的 CG 按钮号 + 底层 IOHID 按钮字段/设备 sender。
/// HID 层 tap 上 CG 按钮号可能不可靠,附带 IOHID 字段用于定位真实按钮。
///
/// Diagnostic log: print the CG button number of a mouse button event plus the underlying
/// IOHID button fields/device sender. The CG button number can be unreliable on the HID-level
/// tap, so the IOHID fields help locate the real button.
fn log_button_event_diag(
    event_type: CGEventType,
    event: CGEventRef,
    button: i64,
    flags: CGEventFlags,
) {
    if !crate::logger::enabled(crate::logger::LogLevel::Debug) {
        return;
    }
    unsafe {
        let io = ffi::CGEventCopyIOHIDEvent(event);
        if io.is_null() {
            log_debug!(
                "[mouse] {} cg_btn={}({}) flags=0x{:x} (no iohid)",
                event_type_name(event_type),
                button,
                button_name(button),
                flags
            );
            return;
        }
        let hid_btn = ffi::IOHIDEventGetIntegerValue(io, ffi::K_IOHID_EVENT_FIELD_BUTTON_NUMBER);
        let hid_mask = ffi::IOHIDEventGetIntegerValue(io, ffi::K_IOHID_EVENT_FIELD_BUTTON_MASK);
        let sender = ffi::IOHIDEventGetSenderID(io);
        CFRelease(io as *const c_void);
        log_debug!(
            "[mouse] {} cg_btn={}({}) hid_btn={} hid_mask=0x{:x} sender={} flags=0x{:x}",
            event_type_name(event_type),
            button,
            button_name(button),
            hid_btn,
            hid_mask,
            sender,
            flags
        );
    }
}

thread_local! {
    /// 滚动归因缓存:取整光标坐标 -> 光标下窗口的 app 身份(含 None)。滚动爆发期间
    /// 光标几乎静止,坐标不变就复用上次查询,把窗口枚举从"每个滚轮事件一次"降到
    /// "每次坐标变化一次"。只在鼠标线程使用。
    /// Scroll attribution cache: rounded cursor position -> app identity of the window
    /// under it (None included). The cursor barely moves during a scroll burst; an
    /// unchanged position reuses the last lookup, dropping window enumeration from once
    /// per wheel event to once per position change. Mouse thread only.
    static CURSOR_APP: RefCell<((i32, i32), Option<AppIdentity>)> = const {
        RefCell::new(((i32::MIN, i32::MIN), None))
    };
}

/// 光标下窗口的 app 身份(带坐标缓存)。hit-test 在 app.rs,这里只做缓存。
/// App identity of the window under the cursor (position-cached). The hit-test lives in
/// app.rs; this wrapper only caches.
fn scroll_app_at_cursor(x: f64, y: f64) -> Option<AppIdentity> {
    let key = (x.round() as i32, y.round() as i32);
    CURSOR_APP.with(|cache| {
        let mut slot = cache.borrow_mut();
        if slot.0 != key {
            slot.0 = key;
            slot.1 = app::identity_at_point(x, y);
        }
        slot.1.clone()
    })
}

thread_local! {
    static CURRENT_TAP: Cell<ffi::CFMachPortRef> = const {
        Cell::new(std::ptr::null_mut())
    };
}

unsafe extern "C" fn mouse_event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if event_type == ffi::K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == ffi::K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        if !STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            let tap = CURRENT_TAP.with(|c| c.get());
            if !tap.is_null() {
                ffi::CGEventTapEnable(tap, true);
                log_info!(
                    "[mouse] event tap disabled by system (type=0x{:x}); re-enabled.",
                    event_type
                );
            }
        }
        return event;
    }
    if event.is_null() {
        return event;
    }

    let flags: CGEventFlags = CGEventGetFlags(event);

    if event_type == K_CG_EVENT_SCROLL_WHEEL {
        // 跳过自己合成的滚轮事件(防御性,post 到 session 层的事件理论上不经过 HID tap)。
        // Skip our own synthetic scroll events (defensive; session-posted events shouldn't reach HID tap).
        let user_data = CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA);
        if user_data == SYNTHETIC_MARKER {
            return event;
        }

        // continuous=1 表示连续(像素级)滚动事件,来自触摸板/Magic Mouse。
        // 我们的两种模式只处理鼠标滚轮(离散事件),触摸板跳过。
        // continuous=1 means a continuous (pixel-level) scroll event from a trackpad / Magic Mouse.
        // Both modes handle only discrete mouse wheel events; trackpad is skipped.
        let continuous = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
        if continuous != 0 {
            return event;
        }

        // deltaAxis1 = 垂直滚动量,deltaAxis2 = 水平滚动量。
        // deltaAxis1 = vertical delta, deltaAxis2 = horizontal delta.
        let dy = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
        let dx = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
        // 连续模式需要像素级 delta;若硬件事件未提供,按 1 line = 10 px 兜底。
        // Continuous mode needs pixel-level deltas; fall back to 1 line = 10 px when the
        // hardware event does not provide them.
        let raw_point_dy =
            CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
        let raw_point_dx =
            CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
        let (point_dy, point_dx) = if raw_point_dy == 0 && raw_point_dx == 0 {
            (dy * 10, dx * 10)
        } else {
            (raw_point_dy, raw_point_dx)
        };

        // app 归因:按"光标下的窗口"找 app(macOS 把滚轮派发给光标下的窗口,不是
        // key window;只看前台 app 在不激活直接滚失焦窗口时永不命中),hit-test 失败
        // 回退前台 app,保持原有行为。
        // App attribution: resolve the app from the window under the cursor (macOS
        // dispatches wheel events to the window under the pointer, not the key window;
        // the frontmost app alone never hits when scrolling an unfocused window without
        // activating it). Fall back to the frontmost app on a hit-test miss, preserving
        // the previous behavior.
        let loc = CGEventGetLocation(event);
        let app_key = scroll_app_at_cursor(loc.x, loc.y).or_else(app::active_identity);
        // 设备归因:CGEvent -> 产生事件的设备 -> (VID,PID)。失败返回 None(回退"所有鼠标"档)。
        // Device attribution: CGEvent -> producing device -> (VID, PID). None on failure
        // (falls back to the "All Mice" profile).
        let dev_key = device::device_from_cgevent(event);
        // 解析该设备 + 光标下窗口 app 的生效配置(合并全局档 + 设备档 + app 档)。
        // Resolve the effective config for this device and the window-under-cursor app
        // (merging global + per-device + per-app profiles).
        let resolved = resolve::resolve(dev_key, app_key.clone());

        // 归因诊断放最前,与普通滚动日志区分开,便于排查 per-device 设置不生效。
        // Attribution diagnostics lead the line, distinguishing it from plain scroll logs so
        // per-device settings not applying is easy to spot.
        log_debug!(
            "[mouse] scroll dev={:?} app={:?} cursor=({:.0},{:.0}) dy={} dx={} flags=0x{:x} reverse={}",
            dev_key,
            app_key,
            loc.x,
            loc.y,
            dy,
            dx,
            flags,
            resolved.reverse_scroll
        );

        // Continuous:像素级透传 + 反转,合成连续事件。Default / Line:计算 delta(透传或
        // 行数归一化 + 反转)-> post 离散事件。无论哪种都丢弃原 event。
        // Continuous: pixel passthrough + reverse, then post a continuous event. Default / Line:
        // compute delta (passthrough or line-count normalization + reverse) -> post a discrete
        // event. Either way the original event is dropped.
        if resolved.scroll_mode == ScrollMode::Continuous {
            let (ndy, ndx) = compute_continuous_delta(point_dy, point_dx, &resolved);
            post_continuous_scroll_event(ndy, ndx, flags);
        } else {
            let (ndy, ndx, unit) = compute_delta(dy, dx, &resolved);
            post_scroll_event(ndy, ndx, unit, flags);
        }
        std::ptr::null_mut()
    } else if event_type == K_CG_EVENT_OTHER_MOUSE_DOWN || event_type == K_CG_EVENT_OTHER_MOUSE_UP {
        let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
        log_button_event_diag(event_type, event, button, flags);

        // 侧键映射:先归因设备再解析生效配置;命中 3/4 映射时合成键盘动作并丢弃
        // 原鼠标事件,未命中时原样透传(保持系统默认行为)。
        // Side-button map: attribute the device and frontmost app, then resolve; on a 3/4 mapping
        // hit, synthesize the keyboard action and drop the original mouse event; otherwise pass
        // the event through unchanged.
        let dev_key = device::device_from_cgevent(event);
        let app_key = app::active_identity();
        let resolved = resolve::resolve(dev_key, app_key.clone());
        if let Some(action) = resolved.button_map.get(&(button as u32)) {
            log_debug!(
                "[mouse] button {}({}) mapped to {:?} (dev={:?}, app={:?})",
                button,
                button_name(button),
                action,
                dev_key,
                app_key
            );
            if event_type == K_CG_EVENT_OTHER_MOUSE_DOWN {
                buttons::handle_down(button as u32, action);
            } else {
                buttons::handle_up(button as u32);
            }
            return std::ptr::null_mut();
        }
        event
    } else {
        let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
        log_button_event_diag(event_type, event, button, flags);
        event
    }
}

/// 鼠标事件线程的 RunLoop 引用,供 stop() 调 CFRunLoopStop。
/// 由 start() 的线程在进入 CFRunLoopRun 前存入、结束后清空。
/// 用 Send+Sync 包装的 Mutex(static 需要 Send+Sync,与 device.rs 的 ManagerMutex 同模式)。
///
/// The mouse thread's RunLoop reference, for stop() to call CFRunLoopStop on.
/// Stored by the start() thread before CFRunLoopRun and cleared when it exits.
/// Wrapped Mutex with Send+Sync (same pattern as device.rs's ManagerMutex; statics need it).
struct RunLoopMutex(Mutex<Option<ffi::CFRunLoopRef>>);
unsafe impl Send for RunLoopMutex {}
unsafe impl Sync for RunLoopMutex {}

static RUNLOOP: OnceLock<RunLoopMutex> = OnceLock::new();

fn runloop_static() -> &'static Mutex<Option<ffi::CFRunLoopRef>> {
    &RUNLOOP.get_or_init(|| RunLoopMutex(Mutex::new(None))).0
}

/// 运行时停止请求标志:stop() 置位后,重试循环(thread::sleep 阻塞期)醒来即放弃,
/// 避免 stop() 的 join() 在缺权限重试窗口期间阻塞调用线程。
///
/// Runtime stop request flag: once set by stop(), the retry loop (blocked in thread::sleep)
/// bails out on wake-up, so stop()'s join() never blocks the caller during the missing-
/// permission retry window.
static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 运行时停止鼠标事件线程:置位取消标志 + CFRunLoopStop,线程自然结束。
/// 幂等:未运行时无操作。由菜单/主流程在停用鼠标控制时调用。
///
/// Stop the mouse event thread at runtime: set the cancel flag + CFRunLoopStop, the thread
/// exits naturally. Idempotent: no-op when not running. Called by the menu / main flow when
/// mouse control is turned off.
pub(crate) fn stop() {
    // 释放所有按住的侧键目标键,防止 cmd/ctrl/opt/shift 卡住(停用/退出路径)。
    // Release every held side-button target so modifiers like cmd/ctrl/opt/shift never
    // get stuck on the disable/quit path.
    buttons::release_all();
    // 先置位标志再停 RunLoop:重试窗口内线程醒来即可见(不依赖 RunLoop)。
    // Set the flag first, then stop the RunLoop: during the retry window the thread sees the
    // flag on wake-up (no RunLoop involved yet).
    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    let rl = runloop_static().lock().unwrap().take();
    if let Some(rl) = rl {
        unsafe {
            ffi::CFRunLoopStop(rl);
            CFRelease(rl as *const c_void);
        }
    }
}

/// 启动鼠标事件监听线程。由 main.rs / 菜单在启用鼠标时调用。
/// Start the mouse event listener thread. Called by main.rs / the menu when mouse control
/// is enabled.
pub(crate) fn start() -> thread::JoinHandle<()> {
    let mask: CGEventMask = (1u64 << K_CG_EVENT_OTHER_MOUSE_DOWN)
        | (1u64 << K_CG_EVENT_OTHER_MOUSE_UP)
        | (1u64 << K_CG_EVENT_SCROLL_WHEEL);

    STOP_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);

    thread::spawn(move || unsafe {
        // 启动时枚举一次已连接设备(惰性:归因失败时也会重枚举)。
        // Enumerate connected devices once at startup (also lazily re-done on attribution failure).
        device::ensure_enumerated();
        // 创建 event tap(HID 层,可修改/丢弃事件)。传取消标志:停止请求(缺权限重试期)提前退出。
        // Create event tap (HID level, mutable). Pass the cancel flag: bails out early on a
        // stop request (even during the missing-permission retry window).
        let tap = ffi::create_tap_with_retry(
            tap_location::HID_EVENT_TAP,
            tap_placement::HEAD_INSERT,
            tap_options::DEFAULT_TAP,
            mask,
            Some(mouse_event_tap_callback),
            std::ptr::null_mut(),
            "mouse",
            Some(&STOP_REQUESTED),
        );

        let tap = match tap {
            Some(t) => t,
            None => return,
        };

        let rl = CFRunLoopGetCurrent();
        let source = ffi::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        if !source.is_null() {
            ffi::CFRunLoopAddSource(rl, source, ffi::kCFRunLoopDefaultMode);
        }
        CURRENT_TAP.with(|c| c.set(tap));
        ffi::CGEventTapEnable(tap, true);

        // 设备插拔监听:蓝牙断连重连时事件驱动地重建注册表(避免旧 client 缓存过期
        // 导致归因链失效,滚动方向/档位错乱)。回调与 event tap 同线程,安全。
        // Device plug/unplug monitor: event-driven registry rebuild on Bluetooth disconnect/
        // reconnect (avoids the stale-client attribution failure that breaks scroll direction
        // / profile matching). Callbacks run on this same thread, safe.
        device::start_plug_monitor(rl);

        // 存入 RunLoop 供 stop() 使用;存入后再查一次停止标志,消除"存入后、run 前置位
        // 标志"的竞态窗口(stop() 读 RUNLOOP 时要么读到 Some 而 CFRunLoopStop 有效,
        // 要么读到 None 而线程的检查会拦截)。
        // Store the RunLoop for stop(); re-check the stop flag after storing to close the race
        // where the flag is set between the store and the run (stop() either reads Some and
        // CFRunLoopStop works, or reads None and the thread's check catches it).
        crate::ffi::CFRetain(rl as *const c_void);
        *runloop_static().lock().unwrap() = Some(rl);
        if !STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            log_info!("Mouse event tap started.");
            // 阻塞运行 RunLoop,直到 stop() 触发 CFRunLoopStop 或线程被终止。
            // Block on the RunLoop until stop() fires CFRunLoopStop or the thread is killed.
            ffi::CFRunLoopRun();
        }
        if let Some(r) = runloop_static().lock().unwrap().take() {
            CFRelease(r as *const c_void);
        }
        CURRENT_TAP.with(|c| c.set(std::ptr::null_mut()));
        ffi::CGEventTapEnable(tap, false);
        if !source.is_null() {
            ffi::CFRunLoopRemoveSource(rl, source, ffi::kCFRunLoopDefaultMode);
            CFRelease(source as *const c_void);
        }
        ffi::CFMachPortInvalidate(tap);
        CFRelease(tap as *const c_void);
    })
}
