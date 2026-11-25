mod app;
mod config;
mod ffi;
mod i18n;
mod logger;
mod menu;
mod mouse;
mod service;

use config::CONFIG;
use ffi::*;
use i18n::{t, tf};
use menu::*;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use objc2_foundation::NSSize;
use std::ffi::{c_void, CString};

fn init_app() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];

        let _: bool = msg_send![nsapp, setActivationPolicy: 1isize];
    }
}

fn setup_status_bar() {
    unsafe {
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];

        let status_item: *mut AnyObject = msg_send![status_bar, statusItemWithLength: -1.0f64];
        let _: *mut AnyObject = msg_send![status_item, retain];

        let button: *mut AnyObject = msg_send![status_item, button];

        let png_bytes: &[u8] = include_bytes!("../assets/statusbar-icon.png");
        let nsdata: *mut AnyObject = msg_send![
            class!(NSData),
            dataWithBytes: png_bytes.as_ptr() as *const c_void,
            length: png_bytes.len()
        ];
        let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let image: *mut AnyObject = msg_send![image, initWithData: nsdata];
        if !image.is_null() {

            let icon_size: f64 = msg_send![status_bar, thickness];
            let aspect: f64 = 162.0 / 128.0;
            let _: () = msg_send![image, setSize: NSSize::new(icon_size * aspect, icon_size)];
            let is_template: bool = true;
            let _: () = msg_send![image, setTemplate: is_template];
            let _: () = msg_send![button, setImage: image];

            let _: () = msg_send![button, setImagePosition: 1usize];
            let _: () = msg_send![image, release];
        } else {
            let ns_title = make_nsstring("Mouse");
            let _: () = msg_send![button, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }

        let _: () = msg_send![button, sizeToFit];
        let _: () = msg_send![button, setNeedsDisplay: true];

        let menu_title = make_nsstring("");
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: menu_title];
        CFRelease(menu_title as *const c_void);

        let action_cls = {
            let name = CString::new("MouseControlMenuTarget").unwrap();
            let superclass: *const objc2::runtime::AnyClass = class!(NSObject);
            let cls = objc_allocateClassPair(superclass as *mut AnyObject, name.as_ptr(), 0);
            if cls.is_null() {
                log_info!("Failed to allocate ObjC class for menu target.");
                return;
            }
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(handleEnableMouseToggle:),
                handle_mouse_enable_toggle as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleReloadConfig:),
                handle_reload_config as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(handleQuit:),
                handle_quit as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            cls
        };
        let menu_target: *mut AnyObject = msg_send![action_cls as *const AnyObject, new];

        let _: *mut AnyObject = msg_send![menu_target, retain];

        let enable_title = make_nsstring(&t("menu.enable_mouse"));
        let enable_key = make_nsstring("");
        let enable_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let enable_item: *mut AnyObject = msg_send![
            enable_item,
            initWithTitle: enable_title,
            action: sel!(handleEnableMouseToggle:),
            keyEquivalent: enable_key
        ];
        CFRelease(enable_title as *const c_void);
        CFRelease(enable_key as *const c_void);
        let _: () = msg_send![enable_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: enable_item];
        *MOUSE_ENABLE_ITEM.lock().unwrap() = Some(MenuItemState { item: enable_item });

        let sep1: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep1];

        let reload_title = make_nsstring(&t("menu.reload_config"));
        let reload_key = make_nsstring("");
        let reload_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let reload_item: *mut AnyObject = msg_send![
            reload_item,
            initWithTitle: reload_title,
            action: sel!(handleReloadConfig:),
            keyEquivalent: reload_key
        ];
        CFRelease(reload_title as *const c_void);
        CFRelease(reload_key as *const c_void);
        let _: () = msg_send![reload_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: reload_item];

        let sep2: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep2];

        let quit_title = make_nsstring(&t("menu.quit"));
        let quit_key = make_nsstring("q");
        let quit_item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let quit_item: *mut AnyObject = msg_send![
            quit_item,
            initWithTitle: quit_title,
            action: sel!(handleQuit:),
            keyEquivalent: quit_key
        ];
        CFRelease(quit_title as *const c_void);
        CFRelease(quit_key as *const c_void);
        let _: () = msg_send![quit_item, setTarget: menu_target];
        let _: () = msg_send![menu, addItem: quit_item];

        *FIXED_MENU_ITEMS.lock().unwrap() = Some(FixedMenuItems {
            reload: reload_item,
            quit: quit_item,
        });

        let _: () = msg_send![status_item, setMenu: menu];

        for _ in 0..10 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1u8);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match service::dispatch(&args) {
        service::DispatchResult::Run(opts) => opts,
        service::DispatchResult::Exit(code) => std::process::exit(code),
    };

    if let Some(path) = opts.config_path {
        config::set_config_path_override(path);
    }

    let _instance_lock = match service::acquire_instance_lock() {
        Ok(lock) => lock,
        Err(pid) => {
            let pid_str = pid.map(|p| p.to_string()).unwrap_or_else(|| "?".to_string());
            eprintln!("{}", tf("service.already_running", &[("pid", &pid_str)]));
            std::process::exit(0);
        }
    };

    init_app();

    {
        let cfg = CONFIG.read().unwrap();
        let level = match cfg.logging.level.as_str() {
            "debug" => logger::LogLevel::Debug,
            _ => logger::LogLevel::Info,
        };
        let is_dev = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains("target/"))
            .unwrap_or(false);
        logger::init(
            &logger::LogConfig {
                level,
                file_path: cfg.logging.file_path.clone(),
                log_stdout: opts.log_stdout,
            },
            is_dev,
        );
    }

    drop(CONFIG.read().unwrap());

    setup_status_bar();
    service::install_signal_handlers();

    app::start_monitor();

    refresh_menu_titles();

    {
        let cfg = CONFIG.read().unwrap();
        let errs = cfg.validate();
        if !errs.is_empty() {
            let cfg_path = config::resolved_config_path();
            log_info!(
                "Config errors in {} ({} issue(s)):",
                cfg_path.display(),
                errs.len()
            );
            for e in &errs {
                log_info!("  • {}", e);
            }
            log_info!("Using defaults for invalid fields.");
        }
    }

    let mouse_enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if mouse_enabled {

        if !has_accessibility_permission() {
            request_accessibility_permission();
            log_info!(
                "Accessibility permission missing; system prompt shown. If a mouse-control entry \
                 from a previous build exists in System Settings > Privacy & Security > \
                 Accessibility, remove it first, then grant again."
            );
        }
        mouse::start();
    }

    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, finishLaunching];
        let _: () = msg_send![nsapp, run];
    }
}
