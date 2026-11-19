pub(crate) mod buttons;
pub(crate) mod device;
pub(crate) mod event_tap;
pub(crate) mod ffi;
pub(crate) mod resolve;
pub(crate) mod scrolling;

use crate::log_info;

static MOUSE_THREAD: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(None);

pub(crate) fn start() {
    let mut guard = MOUSE_THREAD.lock().unwrap();

    if guard.as_ref().is_some_and(|h| !h.is_finished()) {
        return;
    }
    *guard = Some(event_tap::start());
    log_info!("Mouse control enabled.");
}

pub(crate) fn stop() {
    event_tap::stop();
    let handle = MOUSE_THREAD.lock().unwrap().take();
    if let Some(h) = handle {
        let _ = h.join();
    }
    log_info!("Mouse control disabled.");
}
