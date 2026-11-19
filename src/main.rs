//! mouse-control:最小菜单栏 accessory 进程,只做鼠标控制。
//! 能力:per-device VID+PID 配置档、指针加速禁用/恢复、滚轮 default/line 两模式 + 反转、
//! 运行时热开关、配置热重载、设备热插拔重建。纯 Rust + objc2 调 AppKit/CoreGraphics/IOKit,
//! 无 Swift bridge、无 UI 框架。
//! 形态:单二进制、无 .app 封装;install/uninstall/status 子命令把它注册成 launchd
//! LaunchAgent(见 service.rs),菜单栏图标照常显示。
//!
//! mouse-control: a minimal menu-bar accessory process for mouse control only.
//! Capabilities: per-device VID+PID profiles, pointer-acceleration disable/restore, scroll
//! modes default/line with reverse, runtime hot toggle, config hot reload, and device
//! plug/unplug rebuilds. Pure Rust + objc2 calling AppKit/CoreGraphics/IOKit -- no Swift
//! bridge, no UI framework.
//! Form factor: a single binary with no .app bundle; the install/uninstall/status subcommands
//! register it as a launchd LaunchAgent (see service.rs) while the menu-bar icon still shows.

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
use i18n::t;
use menu::*;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use objc2_foundation::NSSize;
use std::ffi::{c_void, CString};

fn init_app() {
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyAccessory = 1:无 Dock 图标,菜单栏常驻。
        // NSApplicationActivationPolicyAccessory = 1: no Dock icon, menu-bar resident.
        let _: bool = msg_send![nsapp, setActivationPolicy: 1isize];
    }
}

fn setup_status_bar() {
    unsafe {
        let status_bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
        // NSVariableStatusItemLength = -1.0:槽位按 button 内容自适应,sizeToFit 后贴合图标。
        // NSVariableStatusItemLength = -1.0: the slot auto-sizes to the button, hugging the icon.
        let status_item: *mut AnyObject = msg_send![status_bar, statusItemWithLength: -1.0f64];
        let _: *mut AnyObject = msg_send![status_item, retain];

        let button: *mut AnyObject = msg_send![status_item, button];

        // 单色 template PNG(assets/statusbar-icon.png 嵌入二进制)。setTemplate:YES 让系统
        // 按菜单栏前景色渲染(深浅色菜单栏都清晰);sizeToFit 让 button 贴合 image。
        // Monochrome template PNG (assets/statusbar-icon.png embedded in the binary).
        // setTemplate:YES renders it in the menu-bar foreground color; sizeToFit hugs the image.
        let png_bytes: &[u8] = include_bytes!("../assets/statusbar-icon.png");
        let nsdata: *mut AnyObject = msg_send![
            class!(NSData),
            dataWithBytes: png_bytes.as_ptr() as *const c_void,
            length: png_bytes.len()
        ];
        let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let image: *mut AnyObject = msg_send![image, initWithData: nsdata];
        if !image.is_null() {
            // statusbar-icon.png 是 162x128(横向留白更宽)。高度取状态栏厚度、宽度按纵横比
            // 等比缩放,保持图形像素大小不变、只放大左右空隙。
            // statusbar-icon.png is 162x128 (wider horizontal margins). Set the height to the
            // status-bar thickness and scale the width by the aspect ratio.
            let icon_size: f64 = msg_send![status_bar, thickness];
            let aspect: f64 = 162.0 / 128.0;
            let _: () = msg_send![image, setSize: NSSize::new(icon_size * aspect, icon_size)];
            let is_template: bool = true;
            let _: () = msg_send![image, setTemplate: is_template];
            let _: () = msg_send![button, setImage: image];
            // NSImageOnly = 1
            let _: () = msg_send![button, setImagePosition: 1usize];
            let _: () = msg_send![image, release]; // button retained it; drop our alloc +1
        } else {
            let ns_title = make_nsstring("Mouse");
            let _: () = msg_send![button, setTitle: ns_title];
            CFRelease(ns_title as *const c_void);
        }

        let _: () = msg_send![button, sizeToFit];
        let _: () = msg_send![button, setNeedsDisplay: true];

        // Build menu
        let menu_title = make_nsstring("");
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: menu_title];
        CFRelease(menu_title as *const c_void);

        // Menu action target class
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
        // NSMenuItem 不 retain 其 target(弱引用),显式 retain 保活,否则菜单点击会野指针。
        // NSMenuItem does not retain its target (weak); retain explicitly so the target stays
        // alive, otherwise menu clicks would hit a dangling pointer.
        let _: *mut AnyObject = msg_send![menu_target, retain];

        // Enable Mouse Control item(勾选态)
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

        // Separator
        let sep1: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep1];

        // Reload Config item
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

        // Separator
        let sep2: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep2];

        // Quit item(Cmd+Q:仅本 app 为前台时生效,后台菜单栏 app 的 Cmd+Q 归当前前台 app,
        // 这是 macOS 语义;设 mainMenu 让 AppKit 的 keyEquivalent 路由能找到它,同 LinearMouse)。
        // Quit item. Cmd+Q only works while this app is frontmost (a background menu-bar app's
        // Cmd+Q goes to the frontmost app per macOS semantics); setting mainMenu lets AppKit's
        // keyEquivalent routing find it, same mechanism as LinearMouse.
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

        // 登记固定标题项,供热重载 locale 时批量重设标题。
        // Register fixed-title items for locale hot-reload re-titling.
        *FIXED_MENU_ITEMS.lock().unwrap() = Some(FixedMenuItems {
            reload: reload_item,
            quit: quit_item,
        });

        let _: () = msg_send![status_item, setMenu: menu];

        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, setMainMenu: menu];

        // Pump run loop to let SystemUIServer connect
        for _ in 0..10 {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1u8);
        }
    }
}

// ========== Main ==========

fn main() {
    // 0. 子命令分发(install/uninstall/status 等)。必须在任何 AppKit/CONFIG 初始化之前:
    //    服务管理不需要 GUI,也不应触发配置文件创建。
    // 0. Subcommand dispatch (install/uninstall/status/...). Must run before any AppKit/CONFIG
    //    init: service management needs no GUI and must not create the config file.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = service::dispatch(&args) {
        std::process::exit(code);
    }

    // 1. Init NSApplication as accessory (no dock icon)
    init_app();

    // 1b. 初始化 logger:早于一切,从 CONFIG 读日志级别;二进制在 target/ 下运行时视为
    //     dev(同时输出 stdout),安装形态只写文件。
    //     Init logger: before everything else, read log level from CONFIG; a binary running
    //     under target/ is dev (mirrors to stdout), the installed form writes file only.
    {
        let cfg = CONFIG.read().unwrap(); // 触发 LazyLock 初始化和 config 加载
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
            },
            is_dev,
        );
    }

    // 2. 强制 CONFIG 初始化(顺带应用 i18n locale),保证菜单按配置 locale 构建。
    //     CONFIG 的 LazyLock 初始化会调用 i18n::apply_config_locale,无循环依赖。
    //     Force CONFIG init (also applies i18n locale) so the menu is built with the configured
    //     locale. CONFIG's LazyLock init calls i18n::apply_config_locale; no cycle (see i18n.rs).
    drop(CONFIG.read().unwrap());

    // 3. Setup status bar menu
    setup_status_bar();

    // 3a. 启动前台 app 监听。放在鼠标事件 tap 之前,保证第一批事件就能按当前前台 app
    //     解析;监听器常驻,不随“启用鼠标控制”开关启停。
    //     Start the frontmost-app monitor before the mouse tap so the first events resolve
    //     against the current app. It stays resident regardless of the mouse-control toggle.
    app::start_monitor();

    // 3b. 按实际 locale/启用状态修正初始菜单标题与勾选。
    //     Fix initial menu labels/checkmark against the real config.
    refresh_menu_titles();

    // 4. 报告配置校验错误(resilient 加载已保留合法字段,非法字段用默认值)。
    //     Report config validation errors (resilient load kept valid fields, defaults for bad ones).
    {
        let cfg = CONFIG.read().unwrap();
        let errs = cfg.validate();
        if !errs.is_empty() {
            log_info!(
                "Config errors in ~/.config/mouse-control/config.toml ({} issue(s)):",
                errs.len()
            );
            for e in &errs {
                log_info!("  • {}", e);
            }
            log_info!("Using defaults for invalid fields.");
        }
    }

    // 5. 配置启用时启动鼠标事件 tap(缺 AX 权限时 tap 在线程内有限重试,不影响应用启动;
    //    指针加速由现代 macOS 系统原生管理,本进程不再干预指针属性)。
    //    Start the mouse event tap when enabled in config (missing Accessibility only makes the
    //    tap retry in-thread; pointer acceleration is left to macOS native settings).
    let mouse_enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if mouse_enabled {
        // 缺权限时显式弹系统授权框(每次启动都会检查一次);tap 线程的有限重试会在用户
        // 授权后自动建成,无需重启。重装新构建后旧授权条目静默失效:弹窗会复现,但列表里
        // 旧条目仍显示开启,必须删掉重新授权,日志里写明这一点。
        // Explicitly raise the system prompt when untrusted (checked on every launch); the tap
        // thread's bounded retry picks up the grant without a restart. After a reinstall the
        // stale grant silently stops matching: the prompt reappears while the old entry still
        // looks enabled, so it must be removed and re-granted, as the log line states.
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

    // 6. Run the main event loop (blocks until [NSApp terminate:])
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![nsapp, finishLaunching];
        let _: () = msg_send![nsapp, run];
    }
}
