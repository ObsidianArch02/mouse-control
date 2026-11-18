//! 状态栏菜单:启用/停用鼠标控制、重载配置、退出。
//! 菜单项状态(MOUSE_ENABLE_ITEM / FIXED_MENU_ITEMS)、动作回调与标题刷新;
//! setup_status_bar 留在 main.rs(装配代码)。
//!
//! Status bar menu: enable/disable mouse control, reload config, quit.
//! Menu-item state (MOUSE_ENABLE_ITEM / FIXED_MENU_ITEMS), action callbacks, and title
//! refresh; setup_status_bar stays in main.rs (wiring).

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

// ========== 菜单项状态 / menu-item state ==========

pub(crate) struct MenuItemState {
    pub(crate) item: *mut AnyObject,
}
unsafe impl Send for MenuItemState {}
unsafe impl Sync for MenuItemState {}

/// "启用鼠标控制"开关项(勾选态)。
/// The "Enable Mouse Control" toggle item (checkmark state).
pub(crate) static MOUSE_ENABLE_ITEM: Mutex<Option<MenuItemState>> = Mutex::new(None);

// 固定标题的菜单项(reload / quit)。locale 变更时由 refresh_menu_titles 批量重设标题。
// Fixed-title menu items (reload / quit); re-titled in bulk by refresh_menu_titles on locale change.
pub(crate) struct FixedMenuItems {
    pub(crate) reload: *mut AnyObject,
    pub(crate) quit: *mut AnyObject,
}
unsafe impl Send for FixedMenuItems {}
unsafe impl Sync for FixedMenuItems {}
pub(crate) static FIXED_MENU_ITEMS: Mutex<Option<FixedMenuItems>> = Mutex::new(None);

// ========== 菜单动作 / menu actions ==========

/// 用当前 locale 与启用状态重设全部菜单项标题/勾选。用于 locale 变更(reload)与启动时修正。
/// Re-title all menu items and sync the enable checkmark from the current locale and state.
/// Used on locale change (reload) and at startup to fix initial labels.
pub(crate) fn refresh_menu_titles() {
    unsafe {
        let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
        if let Some(ref s) = *MOUSE_ENABLE_ITEM.lock().unwrap() {
            let ns_title = make_nsstring(&t("menu.enable_mouse"));
            let _: () = msg_send![s.item, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
            // NSControlStateValueOn = 1, Off = 0
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

/// 启用/停用鼠标控制:翻转配置并写盘,热启停事件 tap 与指针设置。
/// Toggle mouse control: flip the config and persist it, then hot start/stop the event tap
/// and pointer settings.
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
        // 启用时若缺辅助功能权限,显式弹系统授权框提醒用户;重装后的旧授权条目需先删除。
        // When enabling without Accessibility, explicitly raise the system prompt to remind the
        // user; a stale entry from a previous build must be removed first.
        if !has_accessibility_permission() {
            request_accessibility_permission();
            log_info!(
                "Accessibility permission missing; system prompt shown. If a mouse-control entry \
                 from a previous build exists in System Settings > Privacy & Security > \
                 Accessibility, remove it first, then grant again."
            );
        }
        mouse::start();
        mouse::pointer::apply();
        log_info!("Mouse control enabled via menu.");
    } else {
        buttons::release_all();
        mouse::stop();
        mouse::pointer::restore();
        log_info!("Mouse control disabled via menu.");
    }
    refresh_menu_titles();
}

/// 重载配置文件:应用新配置(locale/日志级别),失效设备解析缓存,重应用指针设置,
/// 并按 enabled 热启停事件 tap。
///
/// Reload the config file: apply it (locale/log level), invalidate the device-resolution
/// cache, re-apply pointer settings, and hot start/stop the event tap per `enabled`.
pub(crate) extern "C" fn handle_reload_config(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let errs = reload_config();
    if errs.is_empty() {
        log_info!("Config reloaded successfully.");
    } else {
        log_info!("Config reload: {} error(s):", errs.len());
        for e in &errs {
            log_info!("  • {}", e);
        }
    }
    // 配置可能已变化:失效 per-device 解析缓存 + 重应用指针设置。
    // Config may have changed: release any held side-button actions (so cmd/ctrl/opt/shift
    // never stick after a reload), invalidate the per-device resolve cache + re-apply
    // pointer settings.
    buttons::release_all();
    resolve::invalidate_cache();
    let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if enabled {
        mouse::start();
        mouse::pointer::apply();
    } else {
        mouse::stop();
        mouse::pointer::restore();
    }
    refresh_menu_titles();
}

pub(crate) extern "C" fn handle_quit(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    log_info!("User quit via menu bar.");
    // 退出前释放按住的侧键目标键,避免 cmd/ctrl/opt/shift 在退出后卡住。
    // Release held side-button targets before quitting so modifiers like cmd/ctrl/opt/shift
    // never stay stuck after the app exits.
    buttons::release_all();
    buttons::flush();
    // 退出前恢复指针加速设置(否则系统鼠标保持线性,直到用户手动重置)。
    // Restore pointer acceleration settings before quitting (otherwise the mouse stays
    // linear until the user resets it manually).
    mouse::pointer::restore();
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, terminate: std::ptr::null::<AnyObject>()];
    }
}
