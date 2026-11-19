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

#[derive(Debug, Clone)]
pub(crate) struct DeviceIdentity {
    pub(crate) vendor_id: u32,
    pub(crate) product_id: u32,

    #[allow(dead_code)]
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) transport: String,
}

pub(crate) struct Device {
    pub(crate) identity: DeviceIdentity,

    service_client: *mut c_void,
}

unsafe impl Send for Device {}
unsafe impl Sync for Device {}

pub(crate) type DeviceKey = (u32, u32);

impl Device {
    pub(crate) fn key(&self) -> DeviceKey {
        (self.identity.vendor_id, self.identity.product_id)
    }
}

struct DeviceRegistry {

    client: *mut c_void,

    services: *mut c_void,
    devices: Vec<Device>,
    by_registry_id: HashMap<u64, DeviceKey>,
    missed_senders: HashSet<u64>,
    scheduled_rl: Option<crate::mouse::ffi::CFRunLoopRef>,
}

unsafe impl Send for DeviceRegistry {}
unsafe impl Sync for DeviceRegistry {}

static REGISTRY: OnceLock<Mutex<DeviceRegistry>> = OnceLock::new();

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

unsafe fn prop_int(service: *mut c_void, key: &str) -> Option<i64> {
    unsafe {
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
}

unsafe fn prop_string(service: *mut c_void, key: &str) -> String {
    unsafe {
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
}

struct RegistryState {
    client: *mut c_void,
    services: *mut c_void,
    devices: Vec<Device>,
    by_registry_id: HashMap<u64, DeviceKey>,
    scheduled_rl: Option<crate::mouse::ffi::CFRunLoopRef>,
}

unsafe fn build_state() -> RegistryState {
    objc2::rc::autoreleasepool(|_| unsafe {
        let mut state = RegistryState {
            client: std::ptr::null_mut(),
            services: std::ptr::null_mut(),
            devices: Vec::new(),
            by_registry_id: HashMap::new(),
            scheduled_rl: None,
        };

        let client = IOHIDEventSystemClientCreate(std::ptr::null());
        if client.is_null() {
            log_info!("[device] failed to create IOHIDEventSystemClient");
            return state;
        }

        let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
        let page_val: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
        let dict: *mut AnyObject =
            msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
        let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
        IOHIDEventSystemClientSetMatchingMultiple(client, arr as *const c_void);
        CFRelease(page_key as *const c_void);

        std::thread::sleep(std::time::Duration::from_millis(30));

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

unsafe fn swap_state(new: RegistryState) {
    if new.client.is_null() {
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
    unsafe {
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
}

unsafe fn rebuild_and_swap() -> usize {
    unsafe {
        let state = build_state();
        let count = state.devices.len();
        swap_state(state);
        count
    }
}

pub(crate) fn ensure_enumerated() {
    {
        let reg = registry().lock().unwrap();
        if !reg.client.is_null() && !reg.devices.is_empty() {
            return;
        }
    }
    unsafe { rebuild_and_swap() };
}

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

struct ManagerMutex(Mutex<*mut c_void>);
unsafe impl Send for ManagerMutex {}
unsafe impl Sync for ManagerMutex {}

static MANAGER: OnceLock<ManagerMutex> = OnceLock::new();

fn manager_static() -> &'static Mutex<*mut c_void> {
    &MANAGER
        .get_or_init(|| ManagerMutex(Mutex::new(std::ptr::null_mut())))
        .0
}

struct PlugSignal {
    pending: bool,
    last_run: Option<Instant>,
}

static PLUG_SIGNAL: Mutex<PlugSignal> = Mutex::new(PlugSignal {
    pending: false,
    last_run: None,
});
static PLUG_CV: Condvar = Condvar::new();

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

unsafe fn handle_plug_event() {
    let count = unsafe { rebuild_and_swap() };
    log_debug!(
        "[device] plug/unplug event: re-enumerated {} device(s).",
        count
    );

    crate::mouse::buttons::release_all();
}

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

unsafe fn lookup_service_index(reg: &DeviceRegistry, sender: u64) -> Option<usize> {
    if reg.client.is_null() {
        return None;
    }
    unsafe {
        let svc = IOHIDEventSystemClientCopyServiceForRegistryID(reg.client, sender);
        if svc.is_null() {
            return None;
        }

        let idx = reg
            .devices
            .iter()
            .position(|d| crate::ffi::CFEqual(d.service_client, svc));

        CFRelease(svc as *const c_void);
        idx
    }
}

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

fn last_active_key() -> Option<DeviceKey> {
    let key = *LAST_ACTIVE_KEY.lock().unwrap();
    key.and_then(|k| {
        let reg = registry().lock().unwrap();
        reg.devices.iter().find(|d| d.key() == k).map(|d| d.key())
    })
}
