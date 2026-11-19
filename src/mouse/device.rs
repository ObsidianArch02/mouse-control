//! 实时设备注册表 + CGEvent -> 产生事件的设备的归因链(对应 Phase 0+1)。
//!
//! 归因链(私有 SPI,沿用 LinearMouse 的做法):
//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!   by_registry_id 查表 -> Device
//! 失败时惰性重枚举一次;仍失败则回退到 last_active(匹配"所有鼠标"档)。
//!
//! 插拔/重枚举线程模型:IOHIDManager 回调只做信号转发,重枚举在专用 worker 线程
//! 执行。重建 client 的 30ms settle 与设备列表构建全程不持 REGISTRY 锁,只在整体
//! 交换新状态时短时持锁,鼠标线程的归因路径持锁时间从 30ms+ 降到微秒级。
//!
//! Live device registry + CGEvent -> producing-device attribution chain (Phase 0+1).
//!
//! Attribution chain (private SPI, mirroring LinearMouse):
//!   CGEventCopyIOHIDEvent(cgEvent) -> IOHIDEventRef
//!   IOHIDEventGetSenderID(ioHIDEvent) -> registry_id (uint64)
//!   by_registry_id lookup -> Device
//! On failure, lazily re-enumerate once; if still failing, fall back to last_active
//! (which matches the "All Mice" profile).
//!
//! Plug/re-enumeration threading model: the IOHIDManager callback only forwards a signal, and
//! re-enumeration runs on a dedicated worker thread. The 30ms settle of a client rebuild and
//! the device-list build happen entirely outside the REGISTRY lock; only the final swap of the
//! whole state takes the lock briefly, so the mouse thread's attribution path holds the lock
//! for microseconds instead of 30ms+.

use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
use crate::mouse::ffi::*;
use crate::{log_debug, log_info};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

// ========== 设备身份与运行时表示 / device identity & runtime handle ==========

/// 设备硬件身份(配置按 VID+PID 匹配;name/transport 仅展示用)。
/// Device hardware identity (config matches on VID+PID; name/transport for display only).
#[derive(Debug, Clone)]
pub(crate) struct DeviceIdentity {
    pub(crate) vendor_id: u32,
    pub(crate) product_id: u32,
    // 名称/传输方式仅展示用(设置 UI 已移除,故标 allow(dead_code),保留数据结构)。
    // name/transport are display-only (the settings UI is gone; kept in the data model,
    // hence #[allow(dead_code)]).
    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) transport: String,
}

/// 运行时设备句柄。硬件身份只在 DeviceIdentity 里;枚举时的 service_client 指针由
/// services 数组保活(归因时做 CFEqual 比对)。
///
/// Runtime device handle. Hardware identity lives only in DeviceIdentity; the service_client
/// pointer from enumeration is kept alive by the services array (used for CFEqual during
/// attribution).
pub(crate) struct Device {
    pub(crate) identity: DeviceIdentity,
    /// 枚举时的 IOHIDServiceClient 指针(由 services 数组保活;用于归因时的 CFEqual 比对)。
    /// The IOHIDServiceClient pointer from enumeration (kept alive by the services array;
    /// used for CFEqual comparison during attribution).
    service_client: *mut c_void,
}

unsafe impl Send for Device {}
unsafe impl Sync for Device {}

/// (VID, PID) 组合,用作解析缓存与 per-device 状态的键。
/// (VID, PID) pair, used as the key for the resolve cache and per-device state.
pub(crate) type DeviceKey = (u32, u32);

impl Device {
    pub(crate) fn key(&self) -> DeviceKey {
        (self.identity.vendor_id, self.identity.product_id)
    }
}

// ========== 全局注册表 / global registry ==========

struct DeviceRegistry {
    /// 持有 event system client(保活 service client)。
    /// Holds the event system client (keeps service clients alive).
    client: *mut c_void,
    /// 持有 services CFArray(保活其中的 IOHIDServiceClient)。
    /// Holds the services CFArray (keeping its IOHIDServiceClients alive).
    services: *mut c_void,
    devices: Vec<Device>,
    by_registry_id: HashMap<u64, DeviceKey>,
    missed_senders: HashSet<u64>,
    scheduled_rl: Option<crate::mouse::ffi::CFRunLoopRef>,
}

unsafe impl Send for DeviceRegistry {}
unsafe impl Sync for DeviceRegistry {}

static REGISTRY: OnceLock<Mutex<DeviceRegistry>> = OnceLock::new();

/// 专用 hid-monitor 线程的 CFRunLoop(承接 IOHIDManager 与 IOHIDEventSystemClient,
/// 与高实时性的 mouse-tap 线程彻底隔离)。启动握手通过 Mutex + Condvar 完成,即使
/// IOHIDManagerCreate 失败也不会死锁。
/// Dedicated hid-monitor thread's CFRunLoop (hosts IOHIDManager and IOHIDEventSystemClient,
/// isolated from the real-time mouse-tap thread). Startup handshake uses Mutex + Condvar so
/// callers never deadlock even if IOHIDManagerCreate returns null.
struct HidRunloopState {
    ready: bool,
    runloop: Option<crate::mouse::ffi::CFRunLoopRef>,
}
unsafe impl Send for HidRunloopState {}
unsafe impl Sync for HidRunloopState {}

static HID_RUNLOOP: OnceLock<(Mutex<HidRunloopState>, Condvar)> = OnceLock::new();
static HID_MONITOR_THREAD: OnceLock<()> = OnceLock::new();
static PLUG_MONITOR_ACTIVE: AtomicBool = AtomicBool::new(false);

fn hid_runloop_sync() -> &'static (Mutex<HidRunloopState>, Condvar) {
    HID_RUNLOOP.get_or_init(|| {
        (
            Mutex::new(HidRunloopState {
                ready: false,
                runloop: None,
            }),
            Condvar::new(),
        )
    })
}

fn ensure_hid_runloop() -> Option<crate::mouse::ffi::CFRunLoopRef> {
    let pair = hid_runloop_sync();
    HID_MONITOR_THREAD.get_or_init(|| {
        std::thread::Builder::new()
            .name("hid-monitor".into())
            .spawn(|| {
                let pair = hid_runloop_sync();
                let started = objc2::rc::autoreleasepool(|_| unsafe {
                    let rl = CFRunLoopGetCurrent();
                    let manager_obj = IOHIDManagerCreate(std::ptr::null(), 0);
                    if manager_obj.is_null() {
                        log_info!("[device] failed to create IOHIDManager");
                        let mut state = pair.0.lock().unwrap();
                        state.ready = true;
                        state.runloop = None;
                        pair.1.notify_all();
                        return false;
                    }
                    // matching:PrimaryUsagePage=Generic Desktop + PrimaryUsage in {Pointer, Mouse, Trackpad}。
                    // 与 build_state 的过滤一致。
                    // Matching: PrimaryUsagePage = Generic Desktop + PrimaryUsage in {Pointer, Mouse, Trackpad},
                    // mirroring build_state's filter.
                    let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
                    let page_val: *mut AnyObject = msg_send![
                        class!(NSNumber),
                        numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32
                    ];
                    let dict: *mut AnyObject = msg_send![
                        class!(NSDictionary),
                        dictionaryWithObject: page_val,
                        forKey: page_key
                    ];
                    let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
                    IOHIDManagerSetDeviceMatchingMultiple(manager_obj, arr as *const c_void);
                    CFRelease(page_key as *const c_void);

                    let cb: Option<
                        unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void),
                    > = Some(
                        device_change_callback
                            as unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void),
                    );
                    IOHIDManagerRegisterDeviceMatchingCallback(
                        manager_obj,
                        cb,
                        std::ptr::null_mut(),
                    );
                    IOHIDManagerRegisterDeviceRemovalCallback(
                        manager_obj,
                        cb,
                        std::ptr::null_mut(),
                    );
                    IOHIDManagerScheduleWithRunLoop(
                        manager_obj,
                        rl,
                        crate::mouse::ffi::kCFRunLoopDefaultMode,
                    );
                    crate::ffi::CFRetain(rl as *const c_void);
                    *manager_static().lock().unwrap() = manager_obj;
                    log_info!("[device] plug/unplug monitor started.");

                    let mut state = pair.0.lock().unwrap();
                    state.ready = true;
                    state.runloop = Some(rl);
                    pair.1.notify_all();
                    true
                });
                if started {
                    unsafe { CFRunLoopRun() };
                }
            })
            .expect("failed to spawn hid-monitor thread");
    });

    let mut state = pair.0.lock().unwrap();
    while !state.ready {
        state = pair.1.wait(state).unwrap();
    }
    state.runloop
}

/// last_active 的设备 (VID, PID)(归因失败时的回退)。由 event_tap 回调在每次成功归因后更新。
/// 存硬件身份而非进程内 id:设备重枚举后 id 会漂移(NEXT_ID 单调递增),而 VID/PID 稳定
/// (蓝牙断连重连后不变),按硬件身份兜底才可靠。
///
/// Last-active device's (VID, PID) (fallback when attribution fails). Updated by the event_tap
/// callback after each successful attribution. Stored by hardware identity, not the process-local
/// id: the id drifts on re-enumeration (NEXT_ID is monotonic), while VID/PID are stable across a
/// Bluetooth disconnect/reconnect, so the hardware identity is the reliable fallback.
static LAST_ACTIVE_KEY: Mutex<Option<DeviceKey>> = Mutex::new(None);

fn registry() -> &'static Mutex<DeviceRegistry> {
    REGISTRY.get_or_init(|| {
        Mutex::new(DeviceRegistry {
            client: std::ptr::null_mut(),
            services: std::ptr::null_mut(),
            devices: Vec::new(),
            by_registry_id: HashMap::new(),
            missed_senders: HashSet::new(),
            scheduled_rl: None,
        })
    })
}

// ========== 属性读取 helper / property-read helpers ==========
// 复用 pointer.rs 的模式;这里只读不写,故更简单。

/// 读取 IOHIDServiceClient 的整数属性(CFNumber)。
/// Read an integer property from an IOHIDServiceClient (CFNumber).
unsafe fn prop_int(service: *mut c_void, key: &str) -> Option<i64> {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        return None;
    }
    let i: i64 = msg_send![v as *mut AnyObject, longLongValue];
    CFRelease(v as *const c_void);
    Some(i)
}

/// 读取 IOHIDServiceClient 的字符串属性(NSString)。
/// Read a string property from an IOHIDServiceClient (NSString).
unsafe fn prop_string(service: *mut c_void, key: &str) -> String {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        return String::new();
    }
    let s = nsstring_to_rust(v as *mut AnyObject);
    CFRelease(v as *const c_void);
    s
}

// ========== 枚举 / enumeration ==========

/// 一次枚举产出的完整注册表状态(锁外构建,随后短时持锁整体交换)。
/// A complete registry state produced by one enumeration pass (built outside the lock, then
/// swapped in as a whole under a brief lock).
struct RegistryState {
    client: *mut c_void,
    services: *mut c_void,
    devices: Vec<Device>,
    by_registry_id: HashMap<u64, DeviceKey>,
    scheduled_rl: Option<crate::mouse::ffi::CFRunLoopRef>,
}

/// 构建一份全新的注册表状态:新建 IOHIDEventSystemClient、等匹配 settle、调度到鼠标
/// 线程 runloop、拷贝 services 并过滤指针设备。全程不持 REGISTRY 锁(含 30ms sleep);
/// 调用方随后用 swap_state 短时持锁交换。构建失败时返回 client 为空的状态,旧注册表
/// 原样保留(重建期间旧状态继续服务归因,交换后旧 client/services 才释放)。
///
/// Build a brand-new registry state: create a fresh IOHIDEventSystemClient, wait for matching
/// to settle, schedule it on the mouse thread's runloop, copy services and filter pointer
/// devices. The REGISTRY lock is never held (including the 30ms sleep); the caller swaps the
/// result in with swap_state under a brief lock. On failure the returned state has a null
/// client and the old registry is left untouched (the old state keeps serving attribution
/// during the rebuild; the old client/services are only released after the swap).
unsafe fn build_state() -> RegistryState {
    objc2::rc::autoreleasepool(|_| unsafe {
        let mut state = RegistryState {
            client: std::ptr::null_mut(),
            services: std::ptr::null_mut(),
            devices: Vec::new(),
            by_registry_id: HashMap::new(),
            scheduled_rl: None,
        };
        // 一律新建 client:断连重连后旧 client 的注册表缓存过期,只有整体重建才可靠。
        // Always create a fresh client: after a disconnect/reconnect the old client's registry
        // cache is stale, and only a full rebuild is reliable.
        let client = IOHIDEventSystemClientCreate(std::ptr::null());
        if client.is_null() {
            log_info!("[device] failed to create IOHIDEventSystemClient");
            return state;
        }
        // 匹配 Generic Desktop 页(后续按 usage 过滤)。
        // Match the Generic Desktop page (filtered by usage below).
        let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
        let page_val: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
        let dict: *mut AnyObject =
            msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
        let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
        IOHIDEventSystemClientSetMatchingMultiple(client, arr as *const c_void);
        CFRelease(page_key as *const c_void);
        // IOHIDEventSystemClient 的匹配是异步的:刚创建后立刻 CopyServices 可能拿到
        // 空列表(实测枚举结果在 0/1 间摇摆)。等 ~30ms 让匹配完成,枚举才可靠。
        // 该竞态只在新建 client 的路径出现;此 sleep 在 build_state 里,不持任何锁。
        // Matching on IOHIDEventSystemClient is asynchronous: CopyServices right after
        // creation can return an empty list (measured 0/1 flapping). Waiting ~30ms
        // lets matching settle; only the fresh-client path has this race, and the sleep
        // lives in build_state, holding no lock.
        std::thread::sleep(std::time::Duration::from_millis(30));
        // 调度到鼠标线程 runloop:未调度的 client 的 registry-ID 映射不完整,归因用的
        // CopyServiceForRegistryID 会持续返回 nil(实测),滚动事件归因失败后回退到
        // "所有鼠标"档 —— per-device 设置(如反转滚动)在启动后不生效,直到重新枚举
        // 出可用的 client。调度后匹配常驻,归因可靠(LinearMouse 同款做法)。
        // Schedule the client on the mouse thread's runloop: an unscheduled client's
        // registry-ID map is incomplete and CopyServiceForRegistryID keeps returning nil
        // (measured), so scroll attribution fails and falls back to the "All Mice" profile --
        // per-device settings (e.g. reverse scrolling) don't apply after startup until a
        // working client is re-enumerated. Scheduling keeps the matching live and makes
        // attribution reliable (same approach as LinearMouse).
        if let Some(rl) = ensure_hid_runloop() {
            crate::ffi::CFRetain(rl as *const c_void);
            IOHIDEventSystemClientScheduleWithRunLoop(
                client,
                rl,
                crate::mouse::ffi::kCFRunLoopDefaultMode,
            );
            state.scheduled_rl = Some(rl);
        }

        let services = IOHIDEventSystemClientCopyServices(client);
        if services.is_null() {
            // 拷贝失败:丢弃半成品(新 client 在这里释放),旧注册表保持可用。
            // Copy failed: discard the half-built state (the fresh client is released here) and
            // keep the old registry usable.
            log_info!("[device] no services returned");
            if let Some(rl) = state.scheduled_rl.take() {
                IOHIDEventSystemClientUnscheduleWithRunLoop(
                    client,
                    rl,
                    crate::mouse::ffi::kCFRunLoopDefaultMode,
                );
                CFRelease(rl as *const c_void);
            }
            CFRelease(client as *const c_void);
            return state;
        }
        state.client = client;
        state.services = services;

        let count = CFArrayGetCount(services);
        for i in 0..count {
            let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
            // 用 ConformsTo 判断是否指针/鼠标/触控板,不读 PrimaryUsage 单值。
            // 原因:有些真实鼠标(如 ATK A9 SE 这类 Nearlink/星闪设备)的 PrimaryUsage 被
            // 系统报成 Keyboard(6),白名单 {1,2,5} 会把它剔除;ConformsTo 检查整个
            // DeviceUsagePairs,能识别它声明过的 Mouse(1,2)/Pointer(1,1)/Trackpad(1,5)。
            // 副作用:少数键盘也声明了多余的 Mouse 用途(与 LinearMouse 行为一致),会一并
            // 纳入;归因靠 senderID 精确匹配,不影响功能正确性。
            //
            // Determine pointer/mouse/trackpad via ConformsTo instead of the PrimaryUsage scalar.
            // Some real mice (e.g. ATK A9 SE Nearlink devices) report PrimaryUsage = Keyboard(6),
            // and a {1,2,5} whitelist would drop them; ConformsTo inspects the full DeviceUsagePairs
            // and sees the Mouse(1,2)/Pointer(1,1)/Trackpad(1,5) usages they declare. Side effect:
            // a few keyboards also declare extra Mouse usages (same as LinearMouse) and get included;
            // attribution uses exact senderID matching, so this never affects correctness.
            let is_pointer = IOHIDServiceClientConformsTo(service, 1, USAGE_GD_POINTER as u32) != 0
                || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_MOUSE as u32) != 0
                || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_TRACKPAD as u32) != 0;
            if !is_pointer {
                continue;
            }
            let vid = prop_int(service, KEY_VENDOR_ID).unwrap_or(0) as u32;
            let pid = prop_int(service, KEY_PRODUCT_ID).unwrap_or(0) as u32;
            let name = prop_string(service, KEY_PRODUCT);
            let transport = prop_string(service, KEY_TRANSPORT);
            if let Some(reg_id) = service_registry_id(service) {
                state.by_registry_id.insert(reg_id, (vid, pid));
            }

            let dev = Device {
                identity: DeviceIdentity {
                    vendor_id: vid,
                    product_id: pid,
                    name,
                    transport,
                },
                service_client: service,
            };
            state.devices.push(dev);
        }

        log_debug!(
            "[device] enumerated {} pointer device(s).",
            state.devices.len()
        );
        state
    })
}

/// 短时持锁把新状态整体换入注册表,再释放被替换的旧 client/services。
/// 交换期间归因路径拿不到锁,看到的要么是完整旧状态、要么是完整新状态;旧 devices
/// 的 service 指针由旧 services 数组保活,随之一起释放,无悬垂。
///
/// Swap the new state into the registry under a brief lock, then release the replaced old
/// client/services. Attribution can't take the lock mid-swap, so it sees either the complete
/// old state or the complete new state. The old devices' service pointers are kept alive by the
/// old services array, which is released together with them, so nothing dangles.
unsafe fn swap_state(new: RegistryState) {
    if new.client.is_null() {
        // 构建失败:不交换,保留旧注册表。
        // Build failed: keep the old registry instead of swapping.
        return;
    }
    let (old_client, old_services, old_rl) = {
        let mut reg = registry().lock().unwrap();
        let old_client = reg.client;
        let old_services = reg.services;
        let old_rl = reg.scheduled_rl;
        reg.client = new.client;
        reg.services = new.services;
        reg.devices = new.devices;
        reg.by_registry_id = new.by_registry_id;
        reg.missed_senders.clear();
        reg.scheduled_rl = new.scheduled_rl;
        (old_client, old_services, old_rl)
    };
    if !old_services.is_null() {
        CFRelease(old_services as *const c_void);
    }
    if !old_client.is_null() {
        if let Some(rl) = old_rl {
            IOHIDEventSystemClientUnscheduleWithRunLoop(
                old_client,
                rl,
                crate::mouse::ffi::kCFRunLoopDefaultMode,
            );
            CFRelease(rl as *const c_void);
        }
        CFRelease(old_client as *const c_void);
    } else if let Some(rl) = old_rl {
        CFRelease(rl as *const c_void);
    }
}

/// 锁外重建 + 短时交换:强制重建 IOHIDEventSystemClient 并重枚举。蓝牙断连重连、
/// dock 掉线重连后旧 client 的缓存过期,只有整体重建才可靠。返回新枚举的设备数。
///
/// Rebuild outside the lock + brief swap: forcibly recreate the IOHIDEventSystemClient and
/// re-enumerate. After a Bluetooth or dock disconnect/reconnect the old client's cache is
/// stale, and only a full rebuild is reliable. Returns the freshly enumerated device count.
unsafe fn rebuild_and_swap() -> usize {
    let state = build_state();
    let count = state.devices.len();
    swap_state(state);
    count
}

/// 启动时枚举一次(惰性:首次归因失败时也会触发)。
/// Enumerate once at startup (also lazily triggered on first attribution failure).
pub(crate) fn ensure_enumerated() {
    {
        let reg = registry().lock().unwrap();
        if !reg.client.is_null() && !reg.devices.is_empty() {
            return;
        }
    }
    unsafe { rebuild_and_swap() };
}

/// 当前已连接设备列表的快照(VID/PID/名称)。无设置 UI 时仅作调试/日志与后续扩展用,
/// 故标 allow(dead_code);若 registry 为空,先触发一次枚举。
///
/// Snapshot of currently-connected devices (VID/PID/name). With no settings UI this is only
/// for debugging/logging and future extension, hence #[allow(dead_code)]. Triggers enumeration
/// if the registry is empty.
#[allow(dead_code)]
pub(crate) fn connected_devices() -> Vec<DeviceIdentity> {
    {
        let empty = {
            let reg = registry().lock().unwrap();
            reg.client.is_null() || reg.devices.is_empty()
        };
        if empty {
            unsafe { rebuild_and_swap() };
        }
    }
    {
        let empty = registry().lock().unwrap().devices.is_empty();
        if empty {
            // 枚举返回 0 但设备可能在场(陈旧 client / 异步匹配竞态):重建 client 重试,
            // 重建路径自带 30ms settle(在锁外的 build_state 里)。重试仍为空才认作真无设备。
            // Enumeration came back empty while devices may be present (stale client or the
            // async-matching race): retry with a rebuilt client (whose 30ms settle lives in the
            // lock-free build_state). Only give up after the retries still return empty.
            for _ in 0..2 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                unsafe { rebuild_and_swap() };
                if !registry().lock().unwrap().devices.is_empty() {
                    break;
                }
            }
        }
    }
    let reg = registry().lock().unwrap();
    reg.devices.iter().map(|d| d.identity.clone()).collect()
}

// ========== 设备插拔监听 / device plug/unplug monitoring ==========

/// IOHIDManager 实例(保活:释放后回调即失效)。由 start_plug_monitor 创建,仅一次。
/// 用 Send+Sync 包装的 Mutex(与 DeviceRegistry 同模式,static 需要 Send+Sync)。
/// IOHIDManager instance (kept alive: releasing it would invalidate the callbacks). Created by
/// start_plug_monitor, once. Wrapped Mutex with Send+Sync (same pattern as DeviceRegistry;
/// statics need Send+Sync).
struct ManagerMutex(Mutex<*mut c_void>);
unsafe impl Send for ManagerMutex {}
unsafe impl Sync for ManagerMutex {}

static MANAGER: OnceLock<ManagerMutex> = OnceLock::new();

fn manager_static() -> &'static Mutex<*mut c_void> {
    &MANAGER
        .get_or_init(|| ManagerMutex(Mutex::new(std::ptr::null_mut())))
        .0
}

/// 插拔信号:回调只置位,worker 线程消费。处理期间的信号不丢(合并为一次),醒来后
/// 再处理一遍,保证最终状态收敛到最新设备列表。
/// Plug signal: the callback only sets the flag; the worker thread consumes it. Signals
/// arriving during handling are coalesced into one more pass after wakeup, so the registry
/// always converges to the latest device list.
struct PlugSignal {
    pending: bool,
    last_run: Option<Instant>,
}

static PLUG_SIGNAL: Mutex<PlugSignal> = Mutex::new(PlugSignal {
    pending: false,
    last_run: None,
});
static PLUG_CV: Condvar = Condvar::new();

/// plug worker 只需启动一次(由 start_plug_monitor 惰性创建),线程与进程同寿命。
/// The plug worker starts once (lazily from start_plug_monitor) and lives as long as the
/// process.
static PLUG_WORKER: OnceLock<()> = OnceLock::new();

const PLUG_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

fn notify_plug_signal() {
    let mut signal = PLUG_SIGNAL.lock().unwrap();
    signal.pending = true;
    PLUG_CV.notify_all();
}

unsafe extern "C" fn device_change_callback(
    _context: *mut c_void,
    _result: i32,
    _sender: *mut c_void,
    _callback: *mut c_void,
) {
    if PLUG_MONITOR_ACTIVE.load(Ordering::Acquire) {
        notify_plug_signal();
    }
}

/// 惰性启动 plug worker 线程(仅一次;在注册回调前调用,保证信号有消费者)。
/// Lazily start the plug worker thread (once; called before the callbacks are registered so
/// the signal always has a consumer).
fn ensure_plug_worker() {
    PLUG_WORKER.get_or_init(|| {
        std::thread::Builder::new()
            .name("device-plug".into())
            .spawn(|| loop {
                let mut signal = PLUG_SIGNAL.lock().unwrap();
                loop {
                    if !signal.pending {
                        signal = PLUG_CV.wait(signal).unwrap();
                        continue;
                    }
                    if let Some(last) = signal.last_run {
                        let elapsed = last.elapsed();
                        if elapsed < PLUG_DEBOUNCE {
                            let remain = PLUG_DEBOUNCE - elapsed;
                            let (s, _) = PLUG_CV.wait_timeout(signal, remain).unwrap();
                            signal = s;
                            continue;
                        }
                    }
                    signal.pending = false;
                    signal.last_run = Some(Instant::now());
                    break;
                }
                drop(signal);
                objc2::rc::autoreleasepool(|_| unsafe { handle_plug_event() });
            })
            .expect("failed to spawn device plug worker thread");
    });
}

/// 处理一次插拔信号:防抖 -> 锁外重建并交换注册表 -> 释放侧键 -> 触发指针重应用。
/// Handle one plug signal: debounce -> rebuild + swap the registry outside the lock ->
/// release side buttons -> trigger the pointer re-apply.
unsafe fn handle_plug_event() {
    let count = rebuild_and_swap();
    log_debug!(
        "[device] plug/unplug event: re-enumerated {} device(s).",
        count
    );
    // 断连/重连可能让用户松开侧键的事件丢失(设备移除时 up 事件收不到),
    // 释放所有按住的侧键目标键,避免 cmd/ctrl/opt/shift 卡住。
    // A disconnect/reconnect can lose the side-button up event (the device is gone before
    // the up arrives), so release every held target to keep modifiers like cmd/ctrl/opt/
    // shift from sticking.
    crate::mouse::buttons::release_all();
    // apply 已异步化:这里只是入队,立即返回。apply 内部检查 mouse.enabled,未启用时
    // 自动跳过;幂等可重复调用,待处理请求在 pointer 侧合并为一次。
    // apply is async now: this only enqueues and returns immediately. apply checks
    // mouse.enabled internally and skips when disabled; idempotent, safe to call repeatedly,
    // and pending requests coalesce into one on the pointer side.
    crate::mouse::pointer::apply();
}

/// 启动设备插拔监听:激活回调转发并确保专用 hid-monitor 线程已启动。
/// Start device plug/unplug monitoring: activate callback forwarding and ensure the
/// dedicated hid-monitor thread is running.
pub(crate) unsafe fn start_plug_monitor() {
    ensure_plug_worker();
    let was_active = PLUG_MONITOR_ACTIVE.swap(true, Ordering::AcqRel);
    let _ = ensure_hid_runloop();
    if !was_active && !registry().lock().unwrap().client.is_null() {
        notify_plug_signal();
    }
}

pub(crate) unsafe fn detach_plug_monitor() {
    PLUG_MONITOR_ACTIVE.store(false, Ordering::Release);
}

// ========== 归因 / attribution ==========

/// 用 senderID 反查设备索引。
/// 链路:IOHIDEventSystemClientCopyServiceForRegistryID(client, senderID) 得到 IOHIDServiceClient,
/// 再用 CFEqual 与枚举列表逐项比对(Swift 字典对 CF key 也用 CFEqual,而非裸指针地址——
/// Copy 返回的对象与枚举出的可能不是同一实例地址)。
///
/// Look up the device index by sender ID. The chain:
/// IOHIDEventSystemClientCopyServiceForRegistryID(client, senderID) yields an IOHIDServiceClient,
/// then CFEqual matches it against the enumerated list (Swift dictionaries also compare CF keys
/// with CFEqual, not raw pointer addresses -- the Copy-returned object may not be the same
/// instance address as the enumerated one).
unsafe fn lookup_service_index(reg: &DeviceRegistry, sender: u64) -> Option<usize> {
    if reg.client.is_null() {
        return None;
    }
    let svc = IOHIDEventSystemClientCopyServiceForRegistryID(reg.client, sender);
    if svc.is_null() {
        return None;
    }
    // 设备数极少(鼠标/触控板几个),线性 CFEqual 遍历足够快。
    // Device count is tiny (a few mice/trackpads); a linear CFEqual scan is fast enough.
    let idx = reg
        .devices
        .iter()
        .position(|d| crate::ffi::CFEqual(d.service_client, svc));
    // Copy 返回 +1,立即释放(索引已取出,设备由 services 数组保活)。
    // Copy returns +1; release immediately (the index is already taken; devices are kept
    // alive by the services array).
    CFRelease(svc as *const c_void);
    idx
}

/// 从 CGEvent 找到产生它的设备。
/// 归因链:CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID ->
/// IOHIDEventSystemClientCopyServiceForRegistryID -> CFEqual 匹配枚举列表。
/// 失败时惰性重枚举一次再查;仍失败返回 last_active(若无则 None,调用方用"所有鼠标"档)。
///
/// Find the device that produced a CGEvent.
/// Chain: CGEventCopyIOHIDEvent -> IOHIDEventGetSenderID ->
/// IOHIDEventSystemClientCopyServiceForRegistryID -> CFEqual against the enumerated list.
/// On failure, lazily re-enumerate once and retry; if still failing, return last_active
/// (or None if there is none, in which case the caller uses the "All Mice" profile).
pub(crate) fn device_from_cgevent(cg_event: crate::mouse::ffi::CGEventRef) -> Option<DeviceKey> {
    unsafe {
        let io = crate::mouse::ffi::CGEventCopyIOHIDEvent(cg_event);
        if io.is_null() {
            return last_active_key();
        }
        let sender = IOHIDEventGetSenderID(io);
        CFRelease(io as *const c_void);
        if sender == 0 {
            return last_active_key();
        }
        let mut trigger_rebuild = false;
        {
            let mut reg = registry().lock().unwrap();
            if let Some(&key) = reg.by_registry_id.get(&sender) {
                drop(reg);
                *LAST_ACTIVE_KEY.lock().unwrap() = Some(key);
                return Some(key);
            }
            if !reg.missed_senders.contains(&sender) {
                if let Some(idx) = lookup_service_index(&reg, sender) {
                    let key = reg.devices[idx].key();
                    reg.by_registry_id.insert(sender, key);
                    drop(reg);
                    *LAST_ACTIVE_KEY.lock().unwrap() = Some(key);
                    return Some(key);
                }
                reg.missed_senders.insert(sender);
                trigger_rebuild = true;
            }
        }
        if trigger_rebuild {
            ensure_plug_worker();
            notify_plug_signal();
        }
        last_active_key()
    }
}

/// 回退:取 last_active 设备的 (VID,PID)。按硬件身份匹配,不受重枚举 id 漂移影响。
/// Fallback: return the last-active device's (VID, PID), matched by hardware identity so
/// re-enumeration id drift doesn't break it.
fn last_active_key() -> Option<DeviceKey> {
    let key = *LAST_ACTIVE_KEY.lock().unwrap();
    key.and_then(|k| {
        let reg = registry().lock().unwrap();
        reg.devices.iter().find(|d| d.key() == k).map(|d| d.key())
    })
}
