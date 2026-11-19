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
    self, is_synthetic_scroll_marker, tap_location, tap_options, tap_placement,
    CFRunLoopGetCurrent, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventGetLocation, CGEventMask, CGEventRef, CGEventSetDoubleValueField,
    CGEventSetIntegerValueField, CGEventTapProxy, CGEventType, K_CG_EVENT_SOURCE_USER_DATA,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
    K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
    K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2, MOS_SYNTHETIC_MARKER,
};
use crate::mouse::resolve;
use crate::mouse::scrolling::{compute_continuous_delta, compute_delta, ScrollMode, ScrollUnit};
use crate::{log_debug, log_info};
use std::cell::Cell;
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

/// 把像素级滚动量转换成连续/像素滚动事件所需的整数 line 分量。
/// 1 line = 10 pixels;不足 1 line 的非零滚动至少向上取整到 +/-1,与 CGEventCreateScrollWheelEvent2 一致。
///
/// Convert a pixel delta into the integer line component required by pixel/continuous scroll
/// events. 1 line = 10 pixels; a non-zero delta smaller than one line rounds away from zero
/// to +/-1, matching CGEventCreateScrollWheelEvent2.
fn line_delta_from_point(point: i64) -> i64 {
    let line = point / 10;
    if point != 0 && line == 0 {
        point.signum()
    } else {
        line
    }
}

/// 原地改写滚轮 CGEvent 为精准像素事件(isContinuous=1, hasPreciseScrollingDeltas=true),
/// 不创建新事件、不调用 CGEventPost,零 Mach IPC 往返。
/// 注意字段写入顺序:先写 DeltaAxis 再写 PointDelta 与 FixedPtDelta(写 DeltaAxis 会重置后两者)。
///
/// Mutate a scroll CGEvent in place into a precise pixel event (isContinuous=1,
/// hasPreciseScrollingDeltas=true) with zero CGEventPost Mach IPC.
/// Field order matters: set DeltaAxis before PointDelta and FixedPtDelta.
unsafe fn mutate_scroll_pixel_in_place(event: CGEventRef, point_dy: i64, point_dx: i64) {
    let line_dy = line_delta_from_point(point_dy);
    let line_dx = line_delta_from_point(point_dx);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, line_dy);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2, line_dx);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1, point_dy);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2, point_dx);
    CGEventSetDoubleValueField(
        event,
        K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1,
        point_dy as f64 / 10.0,
    );
    CGEventSetDoubleValueField(
        event,
        K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
        point_dx as f64 / 10.0,
    );
    CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, MOS_SYNTHETIC_MARKER);
}

/// 原地改写滚轮 CGEvent 为离散行事件(isContinuous=0, hasPreciseScrollingDeltas=false),
/// 零 CGEventPost Mach IPC。
/// Mutate a scroll CGEvent in place into a discrete line-unit event (isContinuous=0,
/// hasPreciseScrollingDeltas=false) with zero CGEventPost Mach IPC.
unsafe fn mutate_scroll_line_in_place(event: CGEventRef, dy: i32, dx: i32) {
    let dy64 = dy as i64;
    let dx64 = dx as i64;
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 0);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1, dy64);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2, dx64);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1, dy64 * 10);
    CGEventSetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2, dx64 * 10);
    CGEventSetDoubleValueField(
        event,
        K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1,
        dy as f64,
    );
    CGEventSetDoubleValueField(
        event,
        K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2,
        dx as f64,
    );
    CGEventSetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA, MOS_SYNTHETIC_MARKER);
}

unsafe fn apply_scroll_in_place(event: CGEventRef, dy: i32, dx: i32, unit: ScrollUnit) {
    if dy == 0 && dx == 0 {
        return;
    }
    match unit {
        ScrollUnit::Line => mutate_scroll_line_in_place(event, dy, dx),
        ScrollUnit::Pixel => mutate_scroll_pixel_in_place(event, dy as i64, dx as i64),
    }
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

fn scroll_app_at_cursor(x: f64, y: f64) -> Option<AppIdentity> {
    if !resolve::has_per_app_scroll_profiles() {
        return None;
    }
    app::identity_at_point(x, y)
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

    if event_type == K_CG_EVENT_SCROLL_WHEEL {
        // 跳过自己或协调器标记的合成滚轮事件,避免重复处理。
        // Skip synthetic scroll events carrying our marker or the Mos compatibility marker.
        let user_data = CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA);
        if is_synthetic_scroll_marker(user_data) {
            return event;
        }

        // continuous=1 表示连续(像素级)滚动事件,来自触摸板/Magic Mouse,直接放行。
        // continuous=1 means a continuous (pixel-level) scroll event from a trackpad / Magic Mouse.
        let continuous = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
        if continuous != 0 {
            return event;
        }

        // deltaAxis1 = 垂直滚动量,deltaAxis2 = 水平滚动量。
        // deltaAxis1 = vertical delta, deltaAxis2 = horizontal delta.
        let dy = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
        let dx = CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2);

        let debug_enabled = crate::logger::enabled(crate::logger::LogLevel::Debug);
        if !debug_enabled && !resolve::has_per_device_profiles() {
            // 零分配、零 HashMap、零 CGEventPost 的快速热路径:直接复制预解析的 12B 滚动参数
            // 并原地修改 CGEvent 返回,使回调稳定远低于 8000 Hz 轮询周期(125 us)。
            // Zero-allocation, zero-HashMap, zero-CGEventPost fast path: copy pre-resolved scroll
            // config and mutate the CGEvent in place to stay well below the 8000 Hz (125 us) budget.
            let scroll = if resolve::has_per_app_scroll_profiles() {
                let loc = CGEventGetLocation(event);
                app::scroll_at_point_or_active(loc.x, loc.y)
            } else {
                app::active_scroll()
            };
            if scroll.scroll_mode == ScrollMode::Continuous {
                let raw_point_dy =
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
                let raw_point_dx =
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
                let (point_dy, point_dx) = if raw_point_dy == 0 && raw_point_dx == 0 {
                    (dy * 10, dx * 10)
                } else {
                    (raw_point_dy, raw_point_dx)
                };
                let (ndy, ndx) = compute_continuous_delta(point_dy, point_dx, scroll);
                if ndy != 0 || ndx != 0 {
                    mutate_scroll_pixel_in_place(event, ndy, ndx);
                }
            } else {
                let (ndy, ndx, unit) = compute_delta(dy, dx, scroll);
                apply_scroll_in_place(event, ndy, ndx, unit);
            }
            return event;
        }

        let flags: CGEventFlags = CGEventGetFlags(event);
        let loc = CGEventGetLocation(event);
        let app_key = scroll_app_at_cursor(loc.x, loc.y).or_else(app::active_identity);
        let dev_key = if resolve::has_per_device_profiles() || debug_enabled {
            device::device_from_cgevent(event)
        } else {
            None
        };
        let resolved = resolve::resolve(dev_key, app_key.clone());
        let scroll = resolved.scroll();

        if scroll.scroll_mode == ScrollMode::Continuous {
            let raw_point_dy =
                CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
            let raw_point_dx =
                CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
            let (point_dy, point_dx) = if raw_point_dy == 0 && raw_point_dx == 0 {
                (dy * 10, dx * 10)
            } else {
                (raw_point_dy, raw_point_dx)
            };
            let (ndy, ndx) = compute_continuous_delta(point_dy, point_dx, scroll);
            if debug_enabled {
                log_debug!(
                    "[mouse] scroll dev={:?} app={:?} cursor=({:.0},{:.0}) dy={} dx={} mode=continuous out=({},{},cont) flags=0x{:x} reverse={}",
                    dev_key,
                    app_key,
                    loc.x,
                    loc.y,
                    dy,
                    dx,
                    ndy,
                    ndx,
                    flags,
                    scroll.reverse_scroll
                );
            }
            if ndy != 0 || ndx != 0 {
                mutate_scroll_pixel_in_place(event, ndy, ndx);
            }
        } else {
            let (ndy, ndx, unit) = compute_delta(dy, dx, scroll);
            if debug_enabled {
                log_debug!(
                    "[mouse] scroll dev={:?} app={:?} cursor=({:.0},{:.0}) dy={} dx={} mode={} lc={} lh={:?} out=({},{},{:?}) flags=0x{:x} reverse={}",
                    dev_key,
                    app_key,
                    loc.x,
                    loc.y,
                    dy,
                    dx,
                    scroll.scroll_mode.as_str(),
                    scroll.line_count,
                    scroll.line_height,
                    ndy,
                    ndx,
                    unit,
                    flags,
                    scroll.reverse_scroll
                );
            }
            apply_scroll_in_place(event, ndy, ndx, unit);
        }
        event
    } else if event_type == K_CG_EVENT_OTHER_MOUSE_DOWN || event_type == K_CG_EVENT_OTHER_MOUSE_UP {
        let flags: CGEventFlags = CGEventGetFlags(event);
        let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
        let debug_enabled = crate::logger::enabled(crate::logger::LogLevel::Debug);
        log_button_event_diag(event_type, event, button, flags);

        // 侧键映射:仅在配置了 per-device 档或开启 debug 日志时查询设备 sender,减少热路径开销;
        // 命中 3/4 映射时合成键盘动作并丢弃原鼠标事件,未命中时原样透传(保持系统默认行为)。
        // Side-button map: query the HID sender only when per-device profiles or debug logging need
        // it; on a 3/4 mapping hit, synthesize the keyboard action and drop the original event;
        // otherwise pass the event through unchanged.
        let dev_key = if resolve::has_per_device_profiles() || debug_enabled {
            device::device_from_cgevent(event)
        } else {
            None
        };
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
        let flags: CGEventFlags = CGEventGetFlags(event);
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

    thread::Builder::new()
        .name("mouse-tap".into())
        .spawn(move || unsafe {
            let _ = ffi::pthread_set_qos_class_self_np(ffi::QOS_CLASS_USER_INTERACTIVE, 0);
            let rl = CFRunLoopGetCurrent();
            device::start_plug_monitor();
            device::ensure_enumerated();

            // 挂在 Session 层(kCGSessionEventTap)并原地改写 CGEvent 返回:
            // 1) 不阻塞 kCGHIDEventTap,避免高速滚轮后抢占或卡滞 8000 Hz 指针移动事件;
            // 2) 位于系统自然滚动与 HID 加速之后、下游 Mos(AnnotatedSession) 之前,原地改写即可生效且零 CGEventPost IPC。
            // Hook kCGSessionEventTap and mutate scroll CGEvents in place:
            // 1) leaves kCGHIDEventTap unblocked so 8000 Hz mouseMoved never stalls behind scroll ticks;
            // 2) runs after HID natural-scroll/acceleration and before Mos (AnnotatedSession) with zero CGEventPost IPC.
            let tap = ffi::create_tap_with_retry(
                tap_location::SESSION_EVENT_TAP,
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
                None => {
                    device::detach_plug_monitor();
                    return;
                }
            };

            let source = ffi::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
            if !source.is_null() {
                ffi::CFRunLoopAddSource(rl, source, ffi::kCFRunLoopDefaultMode);
            }
            CURRENT_TAP.with(|c| c.set(tap));
            ffi::CGEventTapEnable(tap, true);

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
            device::detach_plug_monitor();
            CURRENT_TAP.with(|c| c.set(std::ptr::null_mut()));
            ffi::CGEventTapEnable(tap, false);
            if !source.is_null() {
                ffi::CFRunLoopRemoveSource(rl, source, ffi::kCFRunLoopDefaultMode);
                CFRelease(source as *const c_void);
            }
            ffi::CFMachPortInvalidate(tap);
            CFRelease(tap as *const c_void);
        })
        .expect("failed to spawn mouse-tap thread")
}
