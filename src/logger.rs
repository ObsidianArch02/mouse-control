use flume::{Receiver, Sender};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO ",
        }
    }
}

pub struct LogConfig {
    pub level: LogLevel,
    pub file_path: String,
    pub log_stdout: bool,
}

type LogEntry = (SystemTime, LogLevel, String);

static LOG_TX: OnceLock<Sender<LogEntry>> = OnceLock::new();
static LOG_LEVEL: AtomicUsize = AtomicUsize::new(LogLevel::Info as usize);

const LOG_CHANNEL_CAPACITY: usize = 512;
const DEBUG_FLUSH_DELAY: Duration = Duration::from_millis(100);
const DEBUG_BATCH_MAX: usize = 64;

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        if $crate::logger::enabled($crate::logger::LogLevel::Debug) {
            $crate::logger::_log($crate::logger::LogLevel::Debug, format_args!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        if $crate::logger::enabled($crate::logger::LogLevel::Info) {
            $crate::logger::_log($crate::logger::LogLevel::Info, format_args!($($arg)*));
        }
    };
}

#[inline]
pub fn enabled(level: LogLevel) -> bool {
    let threshold =
        unsafe { std::mem::transmute::<usize, LogLevel>(LOG_LEVEL.load(Ordering::Relaxed)) };
    level >= threshold
}

pub fn _log(level: LogLevel, args: fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    if let Some(tx) = LOG_TX.get() {
        let _ = tx.try_send((SystemTime::now(), level, args.to_string()));
    }
}

pub fn init(config: &LogConfig, is_dev: bool) {
    let (tx, rx) = flume::bounded::<LogEntry>(LOG_CHANNEL_CAPACITY);
    LOG_TX.set(tx).ok();
    LOG_LEVEL.store(config.level as usize, Ordering::Relaxed);

    let (write_stdout, file_path) = resolve_sink(config, is_dev);
    std::thread::spawn(move || writer_loop(rx, write_stdout, file_path));
}

pub fn reconfigure(level: LogLevel) {
    LOG_LEVEL.store(level as usize, Ordering::Relaxed);
}

fn write_log_entry(
    stdout: &mut Option<BufWriter<std::io::Stdout>>,
    file: &mut Option<BufWriter<std::fs::File>>,
    entry: LogEntry,
    flush_now: &mut bool,
    count: &mut usize,
) {
    let (time, level, message) = entry;
    let msg = format!("{} {} {}\n", timestamp(time), level.as_str(), message);
    if let Some(out) = stdout {
        let _ = out.write_all(msg.as_bytes());
    }
    if let Some(f) = file {
        let _ = f.write_all(msg.as_bytes());
    }
    *count += 1;
    if level >= LogLevel::Info {
        *flush_now = true;
    }
}

fn writer_loop(rx: Receiver<LogEntry>, write_stdout: bool, file_path: Option<String>) {
    let mut stdout: Option<BufWriter<std::io::Stdout>> =
        write_stdout.then(|| BufWriter::new(std::io::stdout()));
    let mut file: Option<BufWriter<std::fs::File>> = file_path
        .and_then(|path| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| {
                    eprintln!("[logger] cannot open log file {}: {}", path, e);
                })
                .ok()
        })
        .map(BufWriter::new);

    while let Ok(first) = rx.recv() {
        let mut flush_now = false;
        let mut count = 0usize;
        write_log_entry(
            &mut stdout,
            &mut file,
            first,
            &mut flush_now,
            &mut count,
        );

        while count < DEBUG_BATCH_MAX {
            match rx.try_recv() {
                Ok(entry) => write_log_entry(
                    &mut stdout,
                    &mut file,
                    entry,
                    &mut flush_now,
                    &mut count,
                ),
                Err(_) => break,
            }
        }

        while !flush_now && count < DEBUG_BATCH_MAX {
            match rx.recv_timeout(DEBUG_FLUSH_DELAY) {
                Ok(entry) => write_log_entry(
                    &mut stdout,
                    &mut file,
                    entry,
                    &mut flush_now,
                    &mut count,
                ),
                Err(_) => break,
            }
        }

        if let Some(ref mut out) = stdout {
            let _ = out.flush();
        }
        if let Some(ref mut f) = file {
            let _ = f.flush();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SinkTarget {
    StdoutOnly,
    ExplicitFile(String),
    DefaultFile,
}

fn classify_sink(file_path: &str, log_stdout: bool) -> SinkTarget {
    if !file_path.is_empty() {
        if file_path == "stdout" || file_path == "-" {
            return SinkTarget::StdoutOnly;
        }
        return SinkTarget::ExplicitFile(file_path.to_string());
    }
    if log_stdout {
        return SinkTarget::StdoutOnly;
    }
    SinkTarget::DefaultFile
}

fn resolve_sink(config: &LogConfig, is_dev: bool) -> (bool, Option<String>) {
    match classify_sink(&config.file_path, config.log_stdout) {
        SinkTarget::StdoutOnly => (true, None),
        SinkTarget::ExplicitFile(path) => (is_dev, Some(path)),
        SinkTarget::DefaultFile => (is_dev, default_log_file_path()),
    }
}

fn default_log_file_path() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!("{}/Library/Logs/mouse-control", home);
    let _ = std::fs::create_dir_all(&dir);

    cleanup_old_logs(&dir);
    let stamp = file_timestamp();
    Some(format!("{}/mouse-control-{}.log", dir, stamp))
}

fn cleanup_old_logs(dir: &str) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let cutoff = match SystemTime::now().checked_sub(Duration::from_secs(30 * 86_400)) {
        Some(c) => c,
        None => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !name.starts_with("mouse-control-") || !name.ends_with(".log") {
            continue;
        }
        if let Ok(meta) = entry.metadata()
            && let Ok(mtime) = meta.modified()
            && mtime < cutoff
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

type TimeT = i64;

#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const i8,
}

unsafe extern "C" {
    fn localtime_r(time: *const TimeT, result: *mut Tm) -> *mut Tm;
}

fn timestamp(time: SystemTime) -> String {
    let now = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = secs as TimeT;
        localtime_r(&s, &mut tm);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            ms,
        )
    }
}

fn file_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = secs as TimeT;
        localtime_r(&s, &mut tm);
        format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_sink_respects_file_path_then_log_stdout_then_default() {
        assert_eq!(classify_sink("stdout", false), SinkTarget::StdoutOnly);
        assert_eq!(classify_sink("-", false), SinkTarget::StdoutOnly);
        assert_eq!(
            classify_sink("/var/log/custom.log", true),
            SinkTarget::ExplicitFile("/var/log/custom.log".to_string())
        );
        assert_eq!(classify_sink("", true), SinkTarget::StdoutOnly);
        assert_eq!(classify_sink("", false), SinkTarget::DefaultFile);
    }
}
