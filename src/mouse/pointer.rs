//! 指针设置:禁用系统鼠标加速,让光标 1:1 线性跟踪(对应 docs/mouse-architecture.md §4.6)。
//! 参考 LinearMouse DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma):设 HIDUseLinearScalingMouseAcceleration = 1
//! - 旧系统回退:设 HIDPointerAcceleration = -1(IOFixed 编码,值 × 65536;"acceleration and sensitivity are disabled")
//!
//! 应用前保存每设备的原值,禁用配置或退出时恢复。
//!
//! apply/restore 线程模型:apply() 只把请求入队(待处理请求合并为一次),由专用
//! worker 线程串行执行;四个调用点(启动/菜单开关/菜单 reload/切 app)因此不再被
//! 30ms settle 与跨进程 IPC 阻塞。restore() 保持同步,但执行前先取消/排空队列中的
//! 待处理 apply(同一把队列锁 + 代数计数),保证退出/停用后 worker 不会再改写加速度
//! 属性。
//!
//! 可靠重放与掉线状态机:每次写入后读回校验,失败按 250ms 退避重试(上限 5 次),
//! 设备在场时打满上限有告警日志,不静默放弃;设备缺席(无指针设备)时不打失败日志,
//! 打一条 INFO 进入掉线态并挂起,等下一次 matching 回调触发再试,期间重复落空走
//! DEBUG,恢复成功打一条 INFO 附掉线时长。
//!
//! Pointer settings: disable macOS pointer acceleration for 1:1 linear cursor tracking.
//! Mirrors LinearMouse's DeviceManager.updatePointerSpeed:
//!
//! - macOS 14+ (Sonoma): set HIDUseLinearScalingMouseAcceleration = 1
//! - Legacy fallback: set HIDPointerAcceleration = -1 (IOFixed encoding, value × 65536;
//!   "-1 means acceleration and sensitivity are disabled")
//!
//! Original property values are saved before applying and restored when the config is
//! disabled or the app quits.
//!
//! apply/restore threading model: apply() only enqueues a request (pending requests coalesce
//! into one) and a dedicated worker thread runs it serially, so the four call sites (startup /
//! menu toggle / menu reload / app switch) are never blocked by the 30ms settle or the
//! cross-process IPC. restore() stays synchronous but first cancels/drains queued applies
//! (same queue lock + generation counter), guaranteeing the worker never touches acceleration
//! properties again after a disable/quit.
//!
//! Reliable replay and the dropout state machine: every write is verified by reading the
//! property back; on failure retry with a 250ms backoff (at most 5 attempts) and log one
//! warning when the cap is hit while devices are present -- never give up silently. When the
//! device is absent (no pointer devices) there is no failure log: one INFO line enters the
//! dropout state and the cycle suspends until the next matching callback; repeated misses log
//! at DEBUG, and recovery logs one INFO line with the dropout duration.

use crate::app;
use crate::config::CONFIG;
use crate::ffi::{make_nsstring, nsstring_to_rust, CFRelease};
use crate::mouse::ffi::*;
use crate::mouse::resolve;
use crate::{log_debug, log_info};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::ffi::c_void;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

/// 一个设备上被改动的属性(用于恢复)。
/// A property modified on one device (for restore).
struct SavedProp {
    /// IOHIDServiceClientRef(借用自 services 数组,由 PointerState 保活)。
    /// IOHIDServiceClientRef (borrowed from the services array; kept alive by PointerState).
    service: *mut c_void,
    /// NSString 属性键(+1,restore 时 release)。
    /// NSString property key (+1, released on restore).
    key: *mut AnyObject,
    /// 原值(+1,restore 时 set 回再 release);None = 属性原本不存在。
    /// Original value (+1, set back then released on restore); None = property didn't exist.
    original: Option<*mut c_void>,
}

/// 已应用的指针状态:持有 event system client 与 services 数组(保活 service client),外加 saved 列表。
/// Applied pointer state: holds the event system client + services array (keeping service
/// clients alive), plus the saved-properties list.
struct PointerState {
    /// IOHIDEventSystemClientRef (+1)
    client: *mut c_void,
    /// CFArrayRef of IOHIDServiceClient (+1)
    services: *mut c_void,
    saved: Vec<SavedProp>,
}

// 裸指针的 Send/Sync(与 SettingsUi/ObjPtr 同一模式:Mutex 守卫所有访问)。
// Raw pointers' Send/Sync (same pattern as SettingsUi/ObjPtr: Mutex guards all access).
unsafe impl Send for PointerState {}
unsafe impl Sync for PointerState {}

static POINTER_STATE: Mutex<Option<PointerState>> = Mutex::new(None);

/// 读取 IOHIDServiceClient 的整数属性(CFNumber, toll-free NSNumber)。
/// Read an integer property from an IOHIDServiceClient (CFNumber, toll-free NSNumber).
unsafe fn prop_int(service: *mut c_void, key: &str) -> i64 {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        return 0;
    }
    let i: i64 = msg_send![v as *mut AnyObject, longLongValue];
    CFRelease(v as *const c_void);
    i
}

/// 拷贝属性(+1),不存在返回 None。
/// Copy a property (+1); None if absent.
unsafe fn copy_prop(service: *mut c_void, key: &str) -> Option<*mut c_void> {
    let k = make_nsstring(key);
    let v = IOHIDServiceClientCopyProperty(service, k as *const c_void);
    CFRelease(k as *const c_void);
    if v.is_null() {
        None
    } else {
        Some(v)
    }
}

/// 属性是否存在。
/// Whether the property exists.
unsafe fn prop_exists(service: *mut c_void, key: &str) -> bool {
    match copy_prop(service, key) {
        Some(v) => {
            CFRelease(v as *const c_void);
            true
        }
        None => false,
    }
}

/// 设置整数属性。
/// Set an integer property.
unsafe fn set_prop_int(service: *mut c_void, key: &str, value: i64) -> bool {
    let k = make_nsstring(key);
    let n: *mut AnyObject = msg_send![class!(NSNumber), numberWithLongLong: value];
    let ok = IOHIDServiceClientSetProperty(service, k as *const c_void, n as *mut c_void);
    CFRelease(k as *const c_void);
    ok
}

/// 读回属性并按数值比对(写入校验)。
/// Read the property back and compare values (write verification).
unsafe fn verify_prop(service: *mut c_void, key: &str, expected: i64) -> bool {
    match copy_prop(service, key) {
        Some(v) => {
            let actual: i64 = msg_send![v as *mut AnyObject, longLongValue];
            CFRelease(v as *const c_void);
            actual == expected
        }
        None => false,
    }
}

/// 读取设备产品名(日志用)。
/// Read the device product name (for logs).
unsafe fn device_name(service: *mut c_void) -> String {
    match copy_prop(service, KEY_PRODUCT) {
        Some(v) => {
            let name = nsstring_to_rust(v as *mut AnyObject);
            CFRelease(v as *const c_void);
            name
        }
        None => "unknown".into(),
    }
}

// ========== 异步 apply 队列 / async apply queue ==========

/// 队列状态:pending = 有待处理的 apply(多次入队合并为一次);in_flight = worker 正在
/// 执行一个周期;generation = 代数计数,restore() 递增以取消在途的重试睡眠。
/// Queue state: pending = an apply is queued (repeated enqueues coalesce into one);
/// in_flight = the worker is running a cycle; generation = bumped by restore() so in-flight
/// retry sleeps bail out quickly.
struct ApplyQueue {
    pending: bool,
    in_flight: bool,
    generation: u64,
}

static APPLY_QUEUE: Mutex<ApplyQueue> = Mutex::new(ApplyQueue {
    pending: false,
    in_flight: false,
    generation: 0,
});
static APPLY_CV: Condvar = Condvar::new();

/// apply worker 只需启动一次(首次入队时惰性创建),线程与进程同寿命。
/// The apply worker starts once (lazily on first enqueue) and lives as long as the process.
static APPLY_WORKER: OnceLock<()> = OnceLock::new();

/// 写入校验失败后的重试参数:250ms 退避,最多 5 次尝试。
/// Retry parameters after a failed write verification: 250ms backoff, at most 5 attempts.
const APPLY_MAX_ATTEMPTS: u32 = 5;
const APPLY_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);

/// 当前掉线的起点;None = 不在掉线态。只在 apply worker 与 restore 路径上访问。
/// When the current dropout started; None = not in a dropout. Only touched by the apply
/// worker and the restore path.
static DROP_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

/// 惰性启动 apply worker(仅一次)。
/// Lazily start the apply worker (once).
fn ensure_apply_worker() {
    APPLY_WORKER.get_or_init(|| {
        std::thread::Builder::new()
            .name("pointer-apply".into())
            .spawn(|| loop {
                let mut q = APPLY_QUEUE.lock().unwrap();
                while !q.pending {
                    q = APPLY_CV.wait(q).unwrap();
                }
                q.pending = false;
                q.in_flight = true;
                let gen = q.generation;
                drop(q);
                run_apply_cycle(gen);
                let mut q = APPLY_QUEUE.lock().unwrap();
                q.in_flight = false;
                APPLY_CV.notify_all();
            })
            .expect("failed to spawn pointer apply worker thread");
    });
}

/// 取消检查:generation 已变化说明有 restore 在等(退出/停用路径),本周期应尽快放弃。
/// Cancellation check: a changed generation means a restore is waiting (a disable/quit path),
/// so this cycle should bail out soon.
fn apply_cancelled(gen: u64) -> bool {
    APPLY_QUEUE.lock().unwrap().generation != gen
}

// ========== 掉线状态机 / dropout state machine ==========

/// 进入掉线态:首次 apply 落空(无指针设备)打一条 INFO,之后的重复落空只走 DEBUG。
/// Enter the dropout state: the first missed apply (no pointer devices) logs one INFO line;
/// repeated misses only log at DEBUG.
fn enter_dropout_once() {
    let mut since = DROP_SINCE.lock().unwrap();
    if since.is_none() {
        log_info!("[pointer] drop detected; acceleration left at system defaults until the device returns");
        *since = Some(Instant::now());
    } else {
        log_debug!("[pointer] drop persists; waiting for the next matching callback to retry");
    }
}

/// 恢复:apply 成功后打一条 INFO 附掉线时长。
/// Recover: after a successful apply, log one INFO line with the dropout duration.
fn exit_dropout_logged() {
    let mut since = DROP_SINCE.lock().unwrap();
    if let Some(t) = since.take() {
        log_info!(
            "[pointer] re-applied after dropout ({}s)",
            t.elapsed().as_secs()
        );
    }
}

// ========== apply worker 周期 / apply worker cycle ==========

/// worker 的一个 apply 周期:先恢复原值,再按当前配置与在场设备重新应用。
/// One apply cycle on the worker: restore originals first, then re-apply based on the
/// current config and the devices actually present.
fn run_apply_cycle(cancel_gen: u64) {
    let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if !enabled {
        restore_state();
        return;
    }
    restore_state();
    let outcome = unsafe { disable_once(cancel_gen) };
    match outcome {
        ApplyOutcome::Applied => exit_dropout_logged(),
        ApplyOutcome::NoPointerDevices => enter_dropout_once(),
        ApplyOutcome::NoTargetDevices => {}
    }
}

/// 单次 disable 的结果分类。
/// Outcome classification of one disable pass.
enum ApplyOutcome {
    /// 至少一台目标设备写入并通过读回校验(个别设备的失败已各自告警)。
    /// At least one target device written and verified by read-back (individual failures
    /// already logged per device).
    Applied,
    /// 没有任何指针设备在场:进入掉线态,挂起等下一次 matching 回调。
    /// No pointer devices at all: enter the dropout state and wait for the next matching
    /// callback.
    NoPointerDevices,
    /// 有指针设备但没有设备要求禁用加速:正常的空操作,不算掉线。
    /// Pointer devices exist but none asks to disable acceleration: a normal no-op, not a
    /// dropout.
    NoTargetDevices,
}

/// 写入整数属性并读回校验;失败按 250ms 退避重试,上限 5 次。设备中途消失时校验自然
/// 一直失败,由 cancel_gen 或次数上限终止;这里不打设备缺席的失败日志(该路径由
/// disable_once 的掉线状态机处理)。打满上限仍失败则告警一次,不静默放弃。
///
/// Write an integer property and verify by reading it back; on failure retry with a 250ms
/// backoff, at most 5 attempts. If the device disappears mid-way the verification simply keeps
/// failing until cancelled via cancel_gen or the attempt cap. No device-absent failure log
/// here -- that path is handled by the dropout state machine in disable_once. When the cap is
/// exhausted, log one warning instead of giving up silently.
unsafe fn apply_prop_verified(
    service: *mut c_void,
    key: &str,
    value: i64,
    name: &str,
    cancel_gen: u64,
) -> bool {
    for attempt in 0..APPLY_MAX_ATTEMPTS {
        let ok = set_prop_int(service, key, value);
        if ok && verify_prop(service, key, value) {
            return true;
        }
        if attempt + 1 < APPLY_MAX_ATTEMPTS {
            if apply_cancelled(cancel_gen) {
                return false;
            }
            log_debug!(
                "[pointer] {}: set {}={} not verified (ok={}), retry {}/{} in {}ms",
                name,
                key,
                value,
                ok,
                attempt + 2,
                APPLY_MAX_ATTEMPTS,
                APPLY_RETRY_BACKOFF.as_millis()
            );
            std::thread::sleep(APPLY_RETRY_BACKOFF);
        }
    }
    // 设备在场但写入始终未通过校验:真实故障,给一条告警。
    // Device present but the write never verifies: a real failure, log one warning.
    log_info!(
        "[pointer] {}: property {} not verified after {} attempts",
        name,
        key,
        APPLY_MAX_ATTEMPTS
    );
    false
}

/// 禁用鼠标加速(单次):枚举鼠标/触控板设备,保存原值并设线性开关,写入后读回校验。
/// 在 apply worker 线程上执行,调用方(四个 apply 调用点)不再被阻塞。
///
/// One disable pass: enumerate mouse/trackpad devices, save originals and set the
/// linear-scaling switch, verifying every write by read-back. Runs on the apply worker
/// thread, so the four apply call sites are never blocked.
unsafe fn disable_once(cancel_gen: u64) -> ApplyOutcome {
    // 幂等护栏:已应用时不重复写(正常周期会先 restore_state 清空,这里只防路径外调用)。
    // Idempotence guard: never double-write over an applied state (a normal cycle clears it
    // via restore_state first; this only guards out-of-band calls).
    if POINTER_STATE.lock().unwrap().is_some() {
        return ApplyOutcome::Applied;
    }

    // 创建 event system client,匹配 Generic Desktop 页(鼠标/键盘/触控板等,后续按 usage 过滤)。
    // Create the event system client, matching the Generic Desktop page (mice/keyboards/trackpads;
    // filtered by usage below).
    let client = IOHIDEventSystemClientCreate(std::ptr::null());
    if client.is_null() {
        log_info!("[pointer] failed to create IOHIDEventSystemClient");
        return ApplyOutcome::NoPointerDevices;
    }
    let page_key = make_nsstring(KEY_PRIMARY_USAGE_PAGE);
    let page_val: *mut AnyObject =
        msg_send![class!(NSNumber), numberWithInt: USAGE_PAGE_GENERIC_DESKTOP as i32];
    // dictionaryWithObject:/arrayWithObject: 返回 autoreleased(+0),不能手动 release,交给自动释放池。
    // dictionaryWithObject:/arrayWithObject: return autoreleased (+0); must not be released
    // manually - the autorelease pool handles them.
    let dict: *mut AnyObject =
        msg_send![class!(NSDictionary), dictionaryWithObject: page_val, forKey: page_key];
    let arr: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: dict];
    IOHIDEventSystemClientSetMatchingMultiple(client, arr as *const c_void);
    CFRelease(page_key as *const c_void);
    // 与 device.rs 同款竞态:新建 client 后立即 CopyServices 可能拿到空列表
    // (日志出现过"no devices found"但设备在场)。等 ~30ms 让异步匹配完成。
    // 此 sleep 在 apply worker 线程上,不阻塞主线程与鼠标线程。
    // Same race as device.rs: CopyServices right after creating the client can return an
    // empty list (the log shows "no devices found" while the device was present). Wait
    // ~30ms for the asynchronous matching to settle. The sleep runs on the apply worker
    // thread and blocks neither the main thread nor the mouse thread.
    std::thread::sleep(std::time::Duration::from_millis(30));

    let services = IOHIDEventSystemClientCopyServices(client);
    if services.is_null() {
        log_info!("[pointer] no services returned by IOHIDEventSystemClient");
        CFRelease(client as *const c_void);
        // 拿不到服务列表与设备缺席同路径:挂起等下一次 matching 回调,由掉线状态机记录。
        // A null services list takes the device-absent path: suspend until the next matching
        // callback, recorded by the dropout state machine.
        return ApplyOutcome::NoPointerDevices;
    }

    // 遍历 services(CFArrayRef)。用 C 函数,不用 msg_send!(objectAtIndex:),
    // 避免 objc2 的类型编码校验 panic(方法返回 '@',Rust 声明 '^v')。
    // Iterate services (CFArrayRef) with C functions, not msg_send!(objectAtIndex:),
    // to avoid objc2's type-encoding panic (method returns '@', Rust declares '^v').
    let count = CFArrayGetCount(services);
    let mut saved: Vec<SavedProp> = Vec::new();
    let mut pointer_count: usize = 0;

    for i in 0..count {
        let service = CFArrayGetValueAtIndex(services, i) as *mut c_void;
        // 与 device.rs 的枚举判定保持一致:用 ConformsTo 而不是 PrimaryUsage 单值。
        // 有些真实鼠标(如 ATK A9 SE 这类 Nearlink/星闪设备)PrimaryUsage 被报成 Keyboard,
        // 白名单 {1,2,5} 会漏掉它们;ConformsTo 检查整个 DeviceUsagePairs。
        // Same pointer-mouse-trackpad test as device.rs enumeration: use ConformsTo, not the
        // PrimaryUsage scalar -- some real mice (e.g. ATK A9 SE Nearlink) report PrimaryUsage =
        // Keyboard, which a {1,2,5} whitelist would drop; ConformsTo inspects DeviceUsagePairs.
        let is_pointer = IOHIDServiceClientConformsTo(service, 1, USAGE_GD_POINTER as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_MOUSE as u32) != 0
            || IOHIDServiceClientConformsTo(service, 1, USAGE_GD_TRACKPAD as u32) != 0;
        if !is_pointer {
            continue;
        }
        pointer_count += 1;
        let name = device_name(service);

        // 读取 VID/PID,解析该设备是否需要禁用加速(per-device 配置)。
        // Read VID/PID and resolve whether this device should have acceleration disabled.
        let vid = prop_int(service, KEY_VENDOR_ID) as u32;
        let pid = prop_int(service, KEY_PRODUCT_ID) as u32;
        let resolved = resolve::resolve(Some((vid, pid)), app::active_identity());
        if !resolved.disable_acceleration {
            // 该设备的配置未要求禁用加速,跳过(保留系统默认)。
            // This device's config doesn't ask to disable acceleration; skip (keep system default).
            log_debug!(
                "[pointer] {}: keeping acceleration (vid={:#x} pid={:#x})",
                name,
                vid,
                pid
            );
            continue;
        }

        // macOS 14+ (Sonoma):线性缩放开关。
        // macOS 14+ (Sonoma): the linear-scaling switch.
        if prop_exists(service, KEY_LINEAR_SCALING) {
            let original = copy_prop(service, KEY_LINEAR_SCALING).unwrap();
            let ok = apply_prop_verified(service, KEY_LINEAR_SCALING, 1, &name, cancel_gen);
            log_debug!(
                "[pointer] {}: linear scaling ON (original saved, ok={})",
                name,
                ok
            );
            saved.push(SavedProp {
                service,
                key: make_nsstring(KEY_LINEAR_SCALING),
                original: Some(original),
            });
            continue;
        }

        // 旧系统回退:加速属性 = -1 (IOFixed:值 × 65536)。优先 HIDPointerAcceleration,
        // 不存在则用 HIDMouseAcceleration。
        // Legacy fallback: acceleration = -1 (IOFixed: value × 65536). Prefer
        // HIDPointerAcceleration, else HIDMouseAcceleration.
        let accel_key = if prop_exists(service, KEY_POINTER_ACCEL) {
            KEY_POINTER_ACCEL
        } else {
            KEY_MOUSE_ACCEL
        };
        let original = copy_prop(service, accel_key);
        let ok = apply_prop_verified(service, accel_key, -65536, &name, cancel_gen);
        log_debug!(
            "[pointer] {}: acceleration -> -1 via {} (ok={})",
            name,
            accel_key,
            ok
        );
        saved.push(SavedProp {
            service,
            key: make_nsstring(accel_key),
            original,
        });
    }

    if saved.is_empty() {
        CFRelease(services as *const c_void);
        CFRelease(client as *const c_void);
        if pointer_count == 0 {
            // 没有任何指针设备:issue #4 的掉线窗口,交由掉线状态机记录,不打失败日志。
            // No pointer devices at all: the dropout window of issue #4, recorded by the
            // dropout state machine instead of a failure log.
            return ApplyOutcome::NoPointerDevices;
        }
        return ApplyOutcome::NoTargetDevices;
    }

    log_debug!(
        "[pointer] disabled acceleration on {} device(s).",
        saved.len()
    );
    *POINTER_STATE.lock().unwrap() = Some(PointerState {
        client,
        services,
        saved,
    });
    ApplyOutcome::Applied
}

/// 恢复原始加速设置(内部实现,持有调用约定:与在途 apply 互斥由调用方保证)。
/// Restore the original acceleration settings (implementation; the caller guarantees mutual
/// exclusion with any in-flight apply).
fn restore_state() {
    let mut guard = POINTER_STATE.lock().unwrap();
    let Some(state) = guard.take() else {
        return;
    };
    unsafe {
        for sp in state.saved {
            let key_cf = sp.key as *const c_void;
            match sp.original {
                Some(orig) => {
                    let ok = IOHIDServiceClientSetProperty(sp.service, key_cf, orig);
                    log_debug!("[pointer] restored original property (ok={})", ok);
                    CFRelease(orig as *const c_void);
                }
                None => {
                    // 原值不存在:设 0(等效关闭我们设置的开关);极少见的旧系统边界情况。
                    // Original didn't exist: set 0 (equivalent to disabling what we set);
                    // a rare legacy edge case.
                    let zero: *mut AnyObject = msg_send![class!(NSNumber), numberWithInt: 0];
                    IOHIDServiceClientSetProperty(sp.service, key_cf, zero as *mut c_void);
                }
            }
            CFRelease(key_cf);
        }
        CFRelease(state.services as *const c_void);
        CFRelease(state.client as *const c_void);
    }
    log_debug!("[pointer] restored original acceleration settings.");
}

/// 取消待处理的 apply 并等待在途周期结束。pending 直接清零(合并窗口内的请求作废);
/// generation 递增让在途的重试睡眠尽快放弃;in_flight 等 worker 自己清。
/// Cancel queued applies and await any in-flight cycle. pending is cleared outright
/// (requests in the coalescing window are discarded); generation is bumped so in-flight
/// retry sleeps bail out quickly; in_flight is cleared by the worker itself.
fn drain_pending_applies() {
    if APPLY_WORKER.get().is_none() {
        // worker 从未启动:不可能有在途或待处理的 apply。
        // The worker never started: nothing can be in flight or pending.
        return;
    }
    let mut q = APPLY_QUEUE.lock().unwrap();
    q.pending = false;
    q.generation += 1;
    while q.in_flight {
        q = APPLY_CV.wait(q).unwrap();
    }
}

/// 恢复原始加速设置(禁用配置、Reload 或退出时调用)。
/// 同步语义保持不变;执行前先取消/排空队列中的待处理 apply 并等待在途周期结束,
/// 保证 restore 之后 worker 不会再写加速度属性(退出路径的关键约束)。
///
/// Restore the original acceleration settings (called on config disable, reload, or quit).
/// The synchronous semantics are unchanged; queued applies are cancelled and any in-flight
/// cycle awaited before running, so the worker never touches acceleration properties again
/// after restore (the key constraint for the quit path).
pub(crate) fn restore() {
    drain_pending_applies();
    restore_state();
    // 停用/退出时清掉掉线态,下次启用从干净状态开始(恢复日志只在真实恢复后出现)。
    // Clear the dropout state on disable/quit so the next enable starts clean (the recovery
    // log only ever follows a real recovery).
    *DROP_SINCE.lock().unwrap() = None;
}

/// 根据当前配置应用或恢复指针设置。启用时只把请求入队(多次调用合并为一次,由
/// apply worker 串行执行),调用线程不再被 30ms settle 与跨进程 IPC 阻塞;未启用时
/// 同步 restore(与旧行为一致,队列中的待处理 apply 一并取消)。
/// 启用周期内,disable_once 按 per-device 解析结果决定哪些设备禁用加速;
/// 若所有在场设备的解析结果都不要求禁用,则等同 restore(不改动任何设备)。
///
/// Apply or restore pointer settings based on the current config. When enabled, this only
/// enqueues a request (repeated calls coalesce into one and the apply worker runs it
/// serially), so the calling thread is never blocked by the 30ms settle or the cross-process
/// IPC. When disabled, restore runs synchronously (as before), cancelling any queued apply.
/// Inside an enabled cycle, disable_once resolves per-device whether to disable acceleration;
/// if no connected device asks for it, this is equivalent to restore (no device is touched).
pub(crate) fn apply() {
    let enabled = CONFIG.read().map(|c| c.mouse.enabled).unwrap_or(false);
    if !enabled {
        restore();
        return;
    }
    ensure_apply_worker();
    let mut q = APPLY_QUEUE.lock().unwrap();
    q.pending = true;
    APPLY_CV.notify_all();
}
