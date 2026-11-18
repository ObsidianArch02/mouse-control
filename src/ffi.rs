//! FFI 与 ObjC 桥接的基础工具:CF 函数声明、裸指针的 Send/Sync 包装、NSString 转换。
//! 被所有模块依赖,是叶子层(窗口切换器的颜色/图层 helper 已随切换器删除)。
//!
//! FFI and ObjC-bridging primitives: CF function declarations, Send/Sync wrappers for raw
//! pointers, and NSString conversion. A leaf module (the switcher's color/layer helpers were
//! removed with the switcher).

use crate::log_info;
use objc2::msg_send;
use objc2::runtime::{AnyObject, Sel};
use std::ffi::{c_char, c_void, CString};

// ========== FFI 外部函数声明 / FFI extern declarations ==========

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    pub(crate) fn CFRetain(cf: *const c_void) -> *const c_void;
    pub(crate) fn CFRelease(cf: *const c_void);
    pub(crate) fn CFEqual(cf1: *const c_void, cf2: *const c_void) -> bool;
    pub(crate) fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    pub(crate) static kCFBooleanTrue: *const c_void;
    pub(crate) fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: u8,
    ) -> i32;
    pub(crate) static kCFRunLoopDefaultMode: *mut c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub(crate) fn AXIsProcessTrusted() -> bool;
    pub(crate) fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    pub(crate) static kAXTrustedCheckOptionPrompt: *const c_void;
}

// AppKit 框架链接占位 / AppKit framework link placeholder
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc", kind = "dylib")]
extern "C" {
    pub(crate) fn objc_allocateClassPair(
        superclass: *mut AnyObject,
        name: *const c_char,
        extra_bytes: usize,
    ) -> *mut AnyObject;
    pub(crate) fn objc_registerClassPair(cls: *mut AnyObject);
    pub(crate) fn class_addMethod(
        cls: *mut AnyObject,
        name: Sel,
        imp: *mut c_void,
        types: *const c_char,
    ) -> bool;
}

// ========== NSString / 对象生命周期 helper ==========

/// 用 Rust &str 构造一个 NSString(CFStringCreateWithCString 返回 +1,调用方负责 release)。
/// Build an NSString from a Rust &str (CFStringCreateWithCString returns +1; caller must release).
pub(crate) fn make_nsstring(s: &str) -> *mut AnyObject {
    unsafe {
        let c_str = CString::new(s).unwrap();
        let cf = CFStringCreateWithCString(std::ptr::null(), c_str.as_ptr(), 0x08000100u32);
        if cf.is_null() {
            log_info!("CFStringCreateWithCString failed for '{}'", s);
        }
        cf as *mut AnyObject
    }
}

/// 把 NSString 转成 Rust String。
/// Convert an NSString to a Rust String.
pub(crate) unsafe fn nsstring_to_rust(ns: *mut AnyObject) -> String {
    if ns.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![ns, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(utf8)
        .to_string_lossy()
        .into_owned()
}

/// 当前进程是否拥有辅助功能(AX)权限。
/// Whether the current process has Accessibility permission.
pub(crate) fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// 显式请求辅助功能权限:未授权时弹系统授权框(kAXTrustedCheckOptionPrompt=true)。
/// 返回调用时刻是否已授权。ad-hoc 签名下重装新构建后旧授权条目会静默失效,弹窗会再次
/// 出现,但列表里的旧条目仍显示开启,用户必须先删掉旧条目再重新授权(调用方负责在
/// 日志里写明这一步)。
/// Explicitly request Accessibility permission: shows the system prompt when untrusted
/// (kAXTrustedCheckOptionPrompt=true). Returns whether the process is trusted at call time.
/// After reinstalling a new ad-hoc-signed build the old grant silently stops matching, so the
/// prompt reappears while the stale entry still looks enabled; the user must remove it and
/// grant again (callers log that step).
pub(crate) fn request_accessibility_permission() -> bool {
    unsafe {
        let keys: [*const c_void; 1] = [kAXTrustedCheckOptionPrompt];
        let values: [*const c_void; 1] = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            std::ptr::null(),
            std::ptr::null(),
        );
        let trusted = AXIsProcessTrustedWithOptions(options);
        if !options.is_null() {
            CFRelease(options);
        }
        trusted
    }
}
