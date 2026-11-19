use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::sync::Mutex;

use crate::config::{reload_config, set_enabled_and_persist, CONFIG};
use crate::ffi::*;
use crate::i18n::t;
use crate::log_info;
use crate::mouse;
use crate::mouse::buttons;
use crate::mouse::resolve;

pub(crate) struct MenuItemState {
    pub(crate) item: *mut AnyObject,
}
unsafe impl Send for MenuItemState {}
unsafe impl Sync for MenuItemState {}

pub(crate) static MOUSE_ENABLE_ITEM: Mutex<Option<MenuItemState>> = Mutex::new(None);

pub(crate) struct FixedMenuItems {
    pub(crate) reload: *mut AnyObject,
    pub(crate) quit: *mut AnyObject,
}
unsafe impl Send for FixedMenuItems {}
unsafe impl Sync for FixedMenuItems {}
pub(crate) static FIXED_MENU_ITEMS: Mutex<Option<FixedMenuItems>> = Mutex::new(None);

pub(crate) fn refresh_menu_titles() {
    unsafe {
        let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
        if let Some(ref s) = *MOUSE_ENABLE_ITEM.lock().unwrap() {
            let ns_title = make_nsstring(&t("menu.enable_mouse"));
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);

            let state: isize = if enabled { 1 } else { 0 };
            let _: () = msg_send![s.item, setState: state];
        }
        if let Some(ref items) = *FIXED_MENU_ITEMS.lock().unwrap() {
            for (item, key) in [
                (items.reload, "menu.reload_config"),
                (items.quit, "menu.quit"),
            ] {
                let ns_title = make_nsstring(&t(key));
                let _: () = msg_send![item, setTitle: ns_title];
                CFRelease(ns_title as *const c_void);
            }
        }
    }
}

pub(crate) extern "C" fn handle_mouse_enable_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    let enable = !CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    let errs = set_enabled_and_persist(enable);
    if !errs.is_empty() {
        log_info!("Config sync during toggle: {} error(s):", errs.len());
        for e in &errs {
            log_info!("  • {}", e);
        }
    }
    resolve::invalidate_cache();
    if enable {

        if !has_accessibility_permission() {
            request_accessibility_permission();
            log_info!(
                "Accessibility permission missing; system prompt shown. If a mouse-control entry \
                 from a previous build exists in System Settings > Privacy & Security > \
                 Accessibility, remove it first, then grant again."
            );
        }
        mouse::start();
        log_info!("Mouse control enabled via menu.");
    } else {
        buttons::release_all();
        mouse::stop();
        log_info!("Mouse control disabled via menu.");
    }
    refresh_menu_titles();
}

pub(crate) fn perform_reload_config() {
    let errs = reload_config();
    if errs.is_empty() {
        log_info!("Config reloaded successfully.");
    } else {
        log_info!("Config reload: {} error(s):", errs.len());
        for e in &errs {
            log_info!("  • {}", e);
        }
    }

    buttons::release_all();
    resolve::invalidate_cache();
    let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if enabled {
        mouse::start();
    } else {
        mouse::stop();
    }
    refresh_menu_titles();
}

pub(crate) fn perform_graceful_shutdown(reason: &str) {
    log_info!("{}", reason);

    buttons::release_all();
    buttons::flush();
    mouse::stop();
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, terminate: std::ptr::null::<AnyObject>()];
    }
}

pub(crate) extern "C" fn handle_reload_config(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    perform_reload_config();
}

pub(crate) extern "C" fn handle_quit(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    perform_graceful_shutdown("User quit via menu bar.");
}
