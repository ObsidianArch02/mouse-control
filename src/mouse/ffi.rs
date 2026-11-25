use crate::ffi::has_accessibility_permission;
use crate::log_info;
use objc2::runtime::{AnyObject, Sel};
use std::ffi::c_void;
use std::time::Duration;

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

#[allow(dead_code)]
pub(crate) mod tap_location {

    pub(crate) const HID_EVENT_TAP: i32 = 0;

    pub(crate) const SESSION_EVENT_TAP: i32 = 1;
}

#[allow(dead_code)]
pub(crate) mod tap_placement {

    pub(crate) const HEAD_INSERT: i32 = 0;

    pub(crate) const TAIL_INSERT: i32 = 1;
}

#[allow(dead_code)]
pub(crate) mod tap_options {

    pub(crate) const DEFAULT_TAP: u32 = 0;

    pub(crate) const LISTEN_ONLY: u32 = 1;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
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
    pub(crate) fn CGEventSetDoubleValueField(event: CGEventRef, field: i32, value: f64);
    pub(crate) fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: CGKeyCode,
        key_down: u8,
    ) -> CGEventRef;

    pub(crate) fn CGEventCopyIOHIDEvent(event: CGEventRef) -> *mut c_void;

    pub(crate) fn CGEventGetLocation(event: CGEventRef) -> CGPointFFI;
    pub(crate) fn CGEventGetTimestamp(event: CGEventRef) -> u64;

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGEventPost(tap: i32, event: CGEventRef);
}

pub(crate) const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
pub(crate) const CLOCK_UPTIME_RAW: u32 = 8;

unsafe extern "C" {
    pub(crate) fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    pub(crate) fn clock_gettime_nsec_np(clock_id: u32) -> u64;
}

#[repr(C)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct CFRunLoopSourceContext {
    pub version: isize,
    pub info: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<unsafe extern "C" fn(*const c_void)>,
    pub copy_description: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
    pub equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> u8>,
    pub hash: Option<unsafe extern "C" fn(*const c_void) -> usize>,
    pub schedule: Option<unsafe extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    pub cancel: Option<unsafe extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    pub perform: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub(crate) fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;
    pub(crate) fn CFMachPortInvalidate(port: CFMachPortRef);

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CFRunLoopSourceCreate(
        allocator: CFAllocatorRef,
        order: isize,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);

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
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CFRunLoopWakeUp(rl: CFRunLoopRef);

    pub(crate) static kCFRunLoopDefaultMode: CFStringRef;
}

pub(crate) const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: CGEventType = 0xFFFF_FFFE;
pub(crate) const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: CGEventType = 0xFFFF_FFFF;

#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;

#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CF_NUMBER_SINT64_TYPE: isize = 4;

#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CF_NUMBER_INT_TYPE: isize = 9;
#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CF_NUMBER_FLOAT64_TYPE: isize = 6;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct CGPointFFI {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct CGSizeFFI {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct CGRectFFI {
    pub origin: CGPointFFI,
    pub size: CGSizeFFI,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGWindowListCopyWindowInfo(
        option: u32,
        relative_to_window: u32,
    ) -> *const c_void;

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CGRectMakeWithDictionaryRepresentation(
        dict: *const c_void,
        rect: *mut CGRectFFI,
    ) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CFDictionaryGetValue(
        the_dict: *const c_void,
        key: *const c_void,
    ) -> *const c_void;

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn CFNumberGetValue(
        number: *const c_void,
        the_type: isize,
        value: *mut c_void,
    ) -> bool;

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowOwnerPID: CFStringRef;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowBounds: CFStringRef;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowLayer: CFStringRef;
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) static kCGWindowAlpha: CFStringRef;
}

const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const RETRY_MAX: u32 = 40;

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
    let mut tap = unsafe { CGEventTapCreate(location, placement, options, mask, callback, user_info) };

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

            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                log_info!("[{}] Event tap cancelled by stop request.", log_name);
                return None;
            }
            if has_accessibility_permission() {
                tap = unsafe {
                    CGEventTapCreate(location, placement, options, mask, callback, user_info)
                };
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

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {

    pub(crate) fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> *mut c_void;
    pub(crate) fn IOHIDEventSystemClientSetMatchingMultiple(
        client: *mut c_void,
        matchings: *const c_void,
    );

    pub(crate) fn IOHIDEventSystemClientCopyServices(client: *mut c_void) -> *mut c_void;

    pub(crate) fn IOHIDEventSystemClientCopyServiceForRegistryID(
        client: *mut c_void,
        registry_id: u64,
    ) -> *mut c_void;

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

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {

    pub(crate) fn IOHIDEventGetSenderID(event: *mut c_void) -> u64;

    pub(crate) fn IOHIDEventGetIntegerValue(event: *mut c_void, field: u64) -> i64;
}

pub(crate) const K_IOHID_EVENT_FIELD_BUTTON_MASK: u64 = 0x0005_0001;
pub(crate) const K_IOHID_EVENT_FIELD_BUTTON_NUMBER: u64 = 0x0005_0002;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {

    pub(crate) fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> *mut c_void;
    pub(crate) fn IOHIDManagerSetDeviceMatchingMultiple(
        manager: *mut c_void,
        multiple: *const c_void,
    );

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
unsafe extern "C" {
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn IOHIDServiceClientGetRegistryID(client: *mut c_void) -> *const c_void;

    pub(crate) fn IOHIDServiceClientCopyProperty(
        client: *mut c_void,
        key: *const c_void,
    ) -> *mut c_void;

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
    unsafe {
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
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {

    pub(crate) fn CFArrayGetCount(the_array: *const c_void) -> isize;

    pub(crate) fn CFArrayGetValueAtIndex(the_array: *const c_void, idx: isize) -> *const c_void;
}

pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1: i32 = 11;

#[allow(dead_code)]
pub(crate) const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2: i32 = 12;

pub(crate) const K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS: i32 = 88;

pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1: i32 = 96;

pub(crate) const K_CG_SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2: i32 = 97;

pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_1: i32 = 93;

pub(crate) const K_CG_SCROLL_WHEEL_EVENT_FIXED_PT_DELTA_AXIS_2: i32 = 94;

#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_HID_EVENT_TAP: i32 = 0;

#[cfg_attr(test, allow(dead_code))]
pub(crate) const K_CG_EVENT_SOURCE_STATE_PRIVATE: i32 = -1;

pub(crate) const K_CG_EVENT_SOURCE_USER_DATA: i32 = 42;

#[allow(dead_code, clippy::unusual_byte_groupings)]
pub(crate) const SYNTHETIC_MARKER: i64 = 0x4F4D_5453_4352_4C;

#[allow(clippy::unusual_byte_groupings)]
pub(crate) const MOS_SYNTHETIC_MARKER: i64 = 0x4D4F_5353_4D4F_4F54;

#[inline]
pub(crate) fn is_synthetic_scroll_marker(user_data: i64) -> bool {
    user_data == SYNTHETIC_MARKER || user_data == MOS_SYNTHETIC_MARKER
}

pub(crate) const KEY_PRIMARY_USAGE_PAGE: &str = "PrimaryUsagePage";

pub(crate) const KEY_PRODUCT: &str = "Product";

pub(crate) const KEY_VENDOR_ID: &str = "VendorID";

pub(crate) const KEY_PRODUCT_ID: &str = "ProductID";

pub(crate) const KEY_TRANSPORT: &str = "Transport";

pub(crate) const K_CG_EVENT_FLAG_MASK_SHIFT: u64 = 1 << 17;
pub(crate) const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 1 << 18;
pub(crate) const K_CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 1 << 19;
pub(crate) const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;

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

pub(crate) const K_VK_MISSION_CONTROL: u16 = 0xA0;
pub(crate) const K_VK_LAUNCHPAD: u16 = 0x82;

pub(crate) const K_NX_KEYTYPE_SOUND_UP: i32 = 0;
pub(crate) const K_NX_KEYTYPE_SOUND_DOWN: i32 = 1;
pub(crate) const K_NX_KEYTYPE_BRIGHTNESS_UP: i32 = 2;
pub(crate) const K_NX_KEYTYPE_BRIGHTNESS_DOWN: i32 = 3;
pub(crate) const K_NX_KEYTYPE_MUTE: i32 = 7;
pub(crate) const K_NX_KEYTYPE_PLAY: i32 = 16;
pub(crate) const K_NX_KEYTYPE_NEXT: i32 = 17;
pub(crate) const K_NX_KEYTYPE_PREVIOUS: i32 = 18;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct NSPointFFI {
    pub x: f64,
    pub y: f64,
}

#[link(name = "AppKit", kind = "framework")]
#[allow(clashing_extern_declarations)]
unsafe extern "C" {

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

    #[cfg_attr(test, allow(dead_code))]
    #[link_name = "objc_msgSend"]
    pub(crate) fn objc_msgSend_cgevent(receiver: *mut AnyObject, selector: Sel) -> *mut c_void;

    #[cfg_attr(test, allow(dead_code))]
    #[link_name = "objc_msgSend"]
    pub(crate) fn objc_msgSend_retain(receiver: *mut AnyObject, selector: Sel) -> *mut AnyObject;

    #[cfg_attr(test, allow(dead_code))]
    #[link_name = "objc_msgSend"]
    pub(crate) fn objc_msgSend_release(receiver: *mut AnyObject, selector: Sel);
}

pub(crate) const USAGE_PAGE_GENERIC_DESKTOP: i64 = 1;

pub(crate) const USAGE_GD_POINTER: i64 = 1;

pub(crate) const USAGE_GD_MOUSE: i64 = 2;

pub(crate) const USAGE_GD_TRACKPAD: i64 = 5;
