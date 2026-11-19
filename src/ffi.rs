use crate::log_info;
use objc2::msg_send;
use objc2::runtime::{AnyObject, Sel};
use std::ffi::{c_char, c_void, CString};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
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
unsafe extern "C" {
    pub(crate) fn AXIsProcessTrusted() -> bool;
    pub(crate) fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    pub(crate) static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
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

pub(crate) unsafe fn nsstring_to_rust(ns: *mut AnyObject) -> String {
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

pub(crate) fn has_accessibility_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

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
