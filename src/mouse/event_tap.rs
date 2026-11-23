use crate::app::{self, AppIdentity};
use crate::ffi::CFRelease;
use crate::mouse::buttons;
use crate::mouse::device;
use crate::mouse::ffi::{
    self, is_synthetic_scroll_marker, tap_location, tap_options, tap_placement,
    CFRunLoopGetCurrent, CGEventFlags, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventGetLocation, CGEventGetTimestamp, CGEventMask, CGEventRef, CGEventSetDoubleValueField,
    CGEventSetIntegerValueField, CGEventTapProxy, CGEventType, CLOCK_UPTIME_RAW,
    K_CG_EVENT_SOURCE_USER_DATA, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
    K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2, K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1,
    K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS,
    K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
    MOS_SYNTHETIC_MARKER, clock_gettime_nsec_np,
};
use crate::mouse::resolve;
use crate::mouse::scrolling::{compute_continuous_delta, compute_delta, ScrollMode, ScrollUnit};
use crate::{log_debug, log_info};
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::thread;

const K_CG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;
const K_CG_EVENT_SCROLL_WHEEL: CGEventType = 22;

const K_CG_MOUSE_EVENT_BUTTON_NUMBER: i32 = 3;

const MAX_SCROLL_EVENT_AGE_NS: u64 = 25_000_000;
const MAX_SANE_EVENT_AGE_NS: u64 = 10_000_000_000;
const MIN_SCROLL_INTERVAL_NS: u64 = 8_000_000;
const SOURCE_CADENCE_TOLERANCE_NS: u64 = 250_000;
const DISPATCH_JITTER_TOLERANCE_NS: u64 = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollDropDecision {
    Allow,
    DropStale(u64),
    DropRateLimit(u64),
}

#[derive(Debug, Clone, Copy, Default)]
struct ScrollDropGate {
    last_emitted_ns: u64,
    last_event_ns: u64,
}

impl ScrollDropGate {
    const fn new() -> Self {
        Self {
            last_emitted_ns: 0,
            last_event_ns: 0,
        }
    }

    fn evaluate(&mut self, now_ns: u64, ev_ts_ns: u64) -> ScrollDropDecision {
        if ev_ts_ns > 0 && now_ns > ev_ts_ns {
            let age_ns = now_ns - ev_ts_ns;
            if age_ns > MAX_SCROLL_EVENT_AGE_NS && age_ns < MAX_SANE_EVENT_AGE_NS {
                return ScrollDropDecision::DropStale(age_ns);
            }
        }
        if self.last_emitted_ns > 0 && now_ns >= self.last_emitted_ns {
            let dt_ns = now_ns - self.last_emitted_ns;
            let on_schedule = self.last_event_ns != 0
                && ev_ts_ns > 0
                && ev_ts_ns <= now_ns
                && ev_ts_ns.saturating_sub(self.last_event_ns)
                    >= MIN_SCROLL_INTERVAL_NS - SOURCE_CADENCE_TOLERANCE_NS;
            let min_dt_ns = if on_schedule {
                MIN_SCROLL_INTERVAL_NS - DISPATCH_JITTER_TOLERANCE_NS
            } else {
                MIN_SCROLL_INTERVAL_NS
            };
            if dt_ns < min_dt_ns {
                return ScrollDropDecision::DropRateLimit(dt_ns);
            }
        }
        self.last_emitted_ns = now_ns;
        self.last_event_ns =
            if ev_ts_ns > 0 && ev_ts_ns <= now_ns && now_ns - ev_ts_ns <= MAX_SCROLL_EVENT_AGE_NS {
                ev_ts_ns
            } else {
                0
            };
        ScrollDropDecision::Allow
    }
}

fn line_delta_from_point(point: i64) -> i64 {
    let line = point / 10;
    if point != 0 && line == 0 {
        point.signum()
    } else {
        line
    }
}

unsafe fn mutate_scroll_pixel_in_place(event: CGEventRef, point_dy: i64, point_dx: i64) {
    let line_dy = line_delta_from_point(point_dy);
    let line_dx = line_delta_from_point(point_dx);
    unsafe {
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
}

unsafe fn mutate_scroll_line_in_place(event: CGEventRef, dy: i32, dx: i32) {
    let dy64 = dy as i64;
    let dx64 = dx as i64;
    unsafe {
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
}

unsafe fn apply_scroll_in_place(event: CGEventRef, dy: i32, dx: i32, unit: ScrollUnit) {
    if dy == 0 && dx == 0 {
        return;
    }
    unsafe {
        match unit {
            ScrollUnit::Line => mutate_scroll_line_in_place(event, dy, dx),
            ScrollUnit::Pixel => mutate_scroll_pixel_in_place(event, dy as i64, dx as i64),
        }
    }
}

fn event_type_name(t: CGEventType) -> &'static str {
    match t {
        K_CG_EVENT_OTHER_MOUSE_DOWN => "other down",
        K_CG_EVENT_OTHER_MOUSE_UP => "other up",
        K_CG_EVENT_SCROLL_WHEEL => "scroll",
        _ => "other",
    }
}

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
    if !app::needs_cursor_window_lookup() {
        return None;
    }
    app::identity_at_point(x, y)
}

thread_local! {
    static CURRENT_TAP: Cell<ffi::CFMachPortRef> = const {
        Cell::new(std::ptr::null_mut())
    };
    static SCROLL_GATE: Cell<ScrollDropGate> = const {
        Cell::new(ScrollDropGate::new())
    };
}

unsafe extern "C" fn mouse_event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    unsafe {
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
            let continuous =
                CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS);
            if continuous != 0 {
                return event;
            }

            let user_data = CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA);
            if is_synthetic_scroll_marker(user_data) {
                return event;
            }

            let debug_enabled = crate::logger::enabled(crate::logger::LogLevel::Debug);
            let now_ns = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
            let ev_ts_ns = CGEventGetTimestamp(event);
            let decision = SCROLL_GATE.with(|cell| {
                let mut gate = cell.get();
                let d = gate.evaluate(now_ns, ev_ts_ns);
                cell.set(gate);
                d
            });
            match decision {
                ScrollDropDecision::Allow => {}
                ScrollDropDecision::DropStale(age_ns) => {
                    if debug_enabled {
                        log_debug!("[mouse] scroll dropped stale age={}us", age_ns / 1000);
                    }
                    return std::ptr::null_mut();
                }
                ScrollDropDecision::DropRateLimit(dt_ns) => {
                    if debug_enabled {
                        log_debug!("[mouse] scroll dropped rate_limit dt={}us", dt_ns / 1000);
                    }
                    return std::ptr::null_mut();
                }
            }

            let line_deltas = || {
                (
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1),
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2),
                )
            };

            let dev_key = if debug_enabled || resolve::has_per_device_scroll_profiles() {
                device::device_from_cgevent(event)
            } else {
                None
            };
            if !debug_enabled && !dev_key.is_some_and(resolve::device_matches_scroll_profile) {
                let scroll = if app::needs_cursor_window_lookup() {
                    let loc = CGEventGetLocation(event);
                    app::scroll_at_point_or_active(loc.x, loc.y)
                } else {
                    app::active_scroll()
                };
                if scroll.scroll_mode == ScrollMode::Continuous {
                    let raw_point_dy = CGEventGetIntegerValueField(
                        event,
                        K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
                    );
                    let raw_point_dx = CGEventGetIntegerValueField(
                        event,
                        K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2,
                    );
                    let (point_dy, point_dx) = if raw_point_dy == 0 && raw_point_dx == 0 {
                        let (dy, dx) = line_deltas();
                        (dy * 10, dx * 10)
                    } else {
                        (raw_point_dy, raw_point_dx)
                    };
                    let (ndy, ndx) = compute_continuous_delta(point_dy, point_dx, scroll);
                    if ndy != 0 || ndx != 0 {
                        mutate_scroll_pixel_in_place(event, ndy, ndx);
                    }
                } else {
                    let (dy, dx) = line_deltas();
                    let (ndy, ndx, unit) = compute_delta(dy, dx, scroll);
                    apply_scroll_in_place(event, ndy, ndx, unit);
                }
                return event;
            }

            let loc = CGEventGetLocation(event);
            let app_key = scroll_app_at_cursor(loc.x, loc.y).or_else(app::active_identity);
            let debug_app = if debug_enabled { app_key.clone() } else { None };
            let resolved = resolve::resolve(dev_key, app_key);
            let scroll = resolved.scroll();

            if scroll.scroll_mode == ScrollMode::Continuous {
                let raw_point_dy =
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
                let raw_point_dx =
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
                let cached_line = if (raw_point_dy == 0 && raw_point_dx == 0) || debug_enabled {
                    Some(line_deltas())
                } else {
                    None
                };
                let (point_dy, point_dx) = if raw_point_dy == 0 && raw_point_dx == 0 {
                    let (dy, dx) = cached_line.unwrap_or((0, 0));
                    (dy * 10, dx * 10)
                } else {
                    (raw_point_dy, raw_point_dx)
                };
                let (ndy, ndx) = compute_continuous_delta(point_dy, point_dx, scroll);
                if debug_enabled {
                    let (dy, dx) = cached_line.unwrap_or((0, 0));
                    let flags = CGEventGetFlags(event);
                    log_debug!(
                        "[mouse] scroll dev={:?} app={:?} cursor=({:.0},{:.0}) dy={} dx={} mode=continuous out=({},{},cont) flags=0x{:x} reverse={}",
                        dev_key,
                        debug_app,
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
                let (dy, dx) = line_deltas();
                let (ndy, ndx, unit) = compute_delta(dy, dx, scroll);
                if debug_enabled {
                    let flags = CGEventGetFlags(event);
                    log_debug!(
                        "[mouse] scroll dev={:?} app={:?} cursor=({:.0},{:.0}) dy={} dx={} mode={} lc={} lh={:?} out=({},{},{:?}) flags=0x{:x} reverse={}",
                        dev_key,
                        debug_app,
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
        } else if event_type == K_CG_EVENT_OTHER_MOUSE_DOWN
            || event_type == K_CG_EVENT_OTHER_MOUSE_UP
        {
            let button = CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_BUTTON_NUMBER);
            let debug_enabled = crate::logger::enabled(crate::logger::LogLevel::Debug);
            if debug_enabled {
                log_button_event_diag(event_type, event, button, CGEventGetFlags(event));
            }
            let Ok(button_u32) = u32::try_from(button) else {
                return event;
            };
            if !resolve::supports_button(button_u32) {
                return event;
            }

            let dev_key = if resolve::has_per_device_profiles() || debug_enabled {
                device::device_from_cgevent(event)
            } else {
                None
            };
            let app_key = app::active_identity();
            let debug_app = if debug_enabled { app_key.clone() } else { None };
            let resolved = resolve::resolve(dev_key, app_key);
            if let Some(action) = resolved.button_map.get(&button_u32) {
                log_debug!(
                    "[mouse] button {}({}) mapped to {:?} (dev={:?}, app={:?})",
                    button,
                    button_name(button),
                    action,
                    dev_key,
                    debug_app
                );
                if event_type == K_CG_EVENT_OTHER_MOUSE_DOWN {
                    buttons::handle_down(button_u32, action);
                } else {
                    buttons::handle_up(button_u32);
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
}

struct RunLoopMutex(Mutex<Option<ffi::CFRunLoopRef>>);
unsafe impl Send for RunLoopMutex {}
unsafe impl Sync for RunLoopMutex {}

static RUNLOOP: OnceLock<RunLoopMutex> = OnceLock::new();

fn runloop_static() -> &'static Mutex<Option<ffi::CFRunLoopRef>> {
    &RUNLOOP.get_or_init(|| RunLoopMutex(Mutex::new(None))).0
}

static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn stop() {

    buttons::release_all();

    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    let rl = runloop_static().lock().unwrap().take();
    if let Some(rl) = rl {
        unsafe {
            ffi::CFRunLoopStop(rl);
            CFRelease(rl as *const c_void);
        }
    }
}

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
            SCROLL_GATE.with(|c| c.set(ScrollDropGate::new()));
            ffi::CGEventTapEnable(tap, true);

            crate::ffi::CFRetain(rl as *const c_void);
            *runloop_static().lock().unwrap() = Some(rl);
            if !STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
                log_info!("Mouse event tap started.");

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

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn scroll_drop_gate_allows_fresh_events_at_normal_cadence() {
        let mut gate = ScrollDropGate::new();
        assert_eq!(gate.evaluate(100 * MS, 99 * MS), ScrollDropDecision::Allow);
        assert_eq!(gate.evaluate(108 * MS, 107 * MS), ScrollDropDecision::Allow);
        assert_eq!(gate.evaluate(120 * MS, 119 * MS), ScrollDropDecision::Allow);
    }

    #[test]
    fn scroll_drop_gate_drops_stale_events_without_blocking_next_fresh_event() {
        let mut gate = ScrollDropGate::new();
        assert_eq!(gate.evaluate(100 * MS, 99 * MS), ScrollDropDecision::Allow);
        assert_eq!(
            gate.evaluate(150 * MS, 120 * MS),
            ScrollDropDecision::DropStale(30 * MS)
        );
        assert_eq!(gate.evaluate(151 * MS, 150 * MS), ScrollDropDecision::Allow);
    }

    #[test]
    fn scroll_drop_gate_ignores_zero_future_or_out_of_range_timestamps() {
        let mut gate = ScrollDropGate::new();
        assert_eq!(gate.evaluate(100 * MS, 0), ScrollDropDecision::Allow);
        assert_eq!(
            gate.evaluate(120 * MS, 130 * MS),
            ScrollDropDecision::Allow
        );
        assert_eq!(
            gate.evaluate(20_000 * MS, 100 * MS),
            ScrollDropDecision::Allow
        );
    }

    #[test]
    fn scroll_drop_gate_drops_subframe_bursts_without_advancing_window() {
        let mut gate = ScrollDropGate::new();
        assert_eq!(gate.evaluate(100 * MS, 100 * MS), ScrollDropDecision::Allow);
        assert_eq!(
            gate.evaluate(103 * MS, 103 * MS),
            ScrollDropDecision::DropRateLimit(3 * MS)
        );
        assert_eq!(
            gate.evaluate(107 * MS, 107 * MS),
            ScrollDropDecision::DropRateLimit(7 * MS)
        );
        assert_eq!(gate.evaluate(108 * MS, 108 * MS), ScrollDropDecision::Allow);
        assert_eq!(
            gate.evaluate(112 * MS, 112 * MS),
            ScrollDropDecision::DropRateLimit(4 * MS)
        );
        assert_eq!(gate.evaluate(116 * MS, 116 * MS), ScrollDropDecision::Allow);
    }

    #[test]
    fn scroll_drop_gate_tolerates_125hz_dispatch_and_clock_jitter() {
        let mut gate = ScrollDropGate::new();
        assert_eq!(
            gate.evaluate(101_200_000, 100_000_000),
            ScrollDropDecision::Allow
        );
        assert_eq!(
            gate.evaluate(108_000_000, 107_850_000),
            ScrollDropDecision::Allow
        );
    }

    #[test]
    fn scroll_drop_gate_still_drops_queued_back_to_back_bursts() {
        let mut gate = ScrollDropGate::new();
        assert_eq!(
            gate.evaluate(105 * MS, 90 * MS),
            ScrollDropDecision::Allow
        );
        assert_eq!(
            gate.evaluate(106 * MS, 98 * MS),
            ScrollDropDecision::DropRateLimit(MS)
        );
    }
}
