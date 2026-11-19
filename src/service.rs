use crate::config;
use crate::ffi::has_accessibility_permission;
use crate::i18n::{t, tf};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

const BUNDLE_ID: &str = "com.eacryo.mouse-control";
const HOMEBREW_LABEL: &str = "homebrew.mxcl.mouse-control";
const LEGACY_LABEL: &str = "com.eacryo.mouse-control";
const DEFAULT_SIGN_IDENTITY: &str = "mouse-control-codesign";
const SIGN_IDENTITY_ENV: &str = "MOUSE_CONTROL_CODESIGN_IDENTITY";

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const SIG_IGN: usize = 1;

static SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn getuid() -> u32;
    fn flock(fd: i32, operation: i32) -> i32;
    fn signal(sig: i32, handler: usize) -> usize;
    static _dispatch_main_q: u8;
    static _dispatch_source_type_signal: u8;
    fn dispatch_source_create(
        source_type: *const c_void,
        handle: usize,
        mask: usize,
        queue: *mut c_void,
    ) -> *mut c_void;
    fn dispatch_source_set_event_handler_f(
        source: *mut c_void,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_resume(object: *mut c_void);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunOptions {
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) log_stdout: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ParsedCommand {
    Run(RunOptions),
    Status { config_path: Option<PathBuf> },
    Sign { identity: Option<String> },
    Version,
    Help,
    RemovedInstall(String),
}

pub(crate) enum DispatchResult {
    Run(RunOptions),
    Exit(i32),
}

pub(crate) struct InstanceLock {
    _file: File,
}

enum LockState {
    NotRunning,
    Running(Option<u32>),
}

pub(crate) fn dispatch(args: &[String]) -> DispatchResult {
    match parse_args(args) {
        Ok(ParsedCommand::Run(opts)) => DispatchResult::Run(opts),
        Ok(ParsedCommand::Status { config_path }) => {
            if let Some(path) = config_path {
                config::set_config_path_override(path);
            }
            DispatchResult::Exit(cmd_status())
        }
        Ok(ParsedCommand::Sign { identity }) => {
            DispatchResult::Exit(cmd_sign(identity.as_deref()))
        }
        Ok(ParsedCommand::Version) => {
            println!("mouse-control {}", env!("CARGO_PKG_VERSION"));
            DispatchResult::Exit(0)
        }
        Ok(ParsedCommand::Help) => {
            print_usage();
            DispatchResult::Exit(0)
        }
        Ok(ParsedCommand::RemovedInstall(cmd)) => {
            eprintln!("{}", tf("service.removed_install_cmd", &[("cmd", &cmd)]));
            DispatchResult::Exit(2)
        }
        Err(msg) => {
            eprintln!("{msg}");
            print_usage();
            DispatchResult::Exit(2)
        }
    }
}

fn parse_args(args: &[String]) -> Result<ParsedCommand, String> {
    let Some(first) = args.first().map(String::as_str) else {
        return Ok(ParsedCommand::Run(RunOptions {
            config_path: None,
            log_stdout: false,
        }));
    };

    match first {
        "run" => parse_run_flags(&args[1..]).map(ParsedCommand::Run),
        "status" => parse_status_flags(&args[1..]).map(|config_path| ParsedCommand::Status {
            config_path,
        }),
        "sign" => parse_sign_flags(&args[1..]).map(|identity| ParsedCommand::Sign { identity }),
        "install" | "uninstall" => Ok(ParsedCommand::RemovedInstall(first.to_string())),
        "--version" | "version" => {
            if args.len() > 1 {
                return Err(tf("service.unknown_command", &[("cmd", &args[1])]));
            }
            Ok(ParsedCommand::Version)
        }
        "--help" | "-h" | "help" => Ok(ParsedCommand::Help),
        flag if flag.starts_with('-') => parse_run_flags(args).map(ParsedCommand::Run),
        other => Err(tf("service.unknown_command", &[("cmd", other)])),
    }
}

fn parse_run_flags(args: &[String]) -> Result<RunOptions, String> {
    let mut config_path = None;
    let mut log_stdout = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--log-stdout" {
            log_stdout = true;
            i += 1;
        } else if arg == "--config" {
            let Some(val) = args.get(i + 1) else {
                return Err(tf("service.missing_flag_value", &[("flag", "--config")]));
            };
            config_path = Some(PathBuf::from(val));
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--config=") {
            if val.is_empty() {
                return Err(tf("service.missing_flag_value", &[("flag", "--config")]));
            }
            config_path = Some(PathBuf::from(val));
            i += 1;
        } else {
            return Err(tf("service.unknown_command", &[("cmd", arg)]));
        }
    }
    Ok(RunOptions {
        config_path,
        log_stdout,
    })
}

fn parse_status_flags(args: &[String]) -> Result<Option<PathBuf>, String> {
    let mut config_path = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--config" {
            let Some(val) = args.get(i + 1) else {
                return Err(tf("service.missing_flag_value", &[("flag", "--config")]));
            };
            config_path = Some(PathBuf::from(val));
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--config=") {
            if val.is_empty() {
                return Err(tf("service.missing_flag_value", &[("flag", "--config")]));
            }
            config_path = Some(PathBuf::from(val));
            i += 1;
        } else {
            return Err(tf("service.unknown_command", &[("cmd", arg)]));
        }
    }
    Ok(config_path)
}

fn parse_sign_flags(args: &[String]) -> Result<Option<String>, String> {
    let mut identity = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--identity" {
            let Some(val) = args.get(i + 1) else {
                return Err(tf("service.missing_flag_value", &[("flag", "--identity")]));
            };
            identity = Some(val.clone());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--identity=") {
            if val.is_empty() {
                return Err(tf("service.missing_flag_value", &[("flag", "--identity")]));
            }
            identity = Some(val.to_string());
            i += 1;
        } else {
            return Err(tf("service.unknown_command", &[("cmd", arg)]));
        }
    }
    Ok(identity)
}

pub(crate) fn acquire_instance_lock() -> Result<Option<InstanceLock>, Option<u32>> {
    let path = lock_file_path();
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("warning: cannot open lock file {}: {e}", path.display());
            return Ok(None);
        }
    };

    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::WouldBlock {
            return Err(read_lock_pid(&mut file));
        }
        eprintln!("warning: flock failed on {}: {err}", path.display());
        return Ok(None);
    }

    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = writeln!(file, "{}", std::process::id());
    let _ = file.flush();
    Ok(Some(InstanceLock { _file: file }))
}

pub(crate) fn install_signal_handlers() {
    install_signal_source(SIGTERM, handle_terminate_signal);
    install_signal_source(SIGINT, handle_terminate_signal);
    install_signal_source(SIGHUP, handle_reload_signal);
}

fn install_signal_source(sig: i32, handler: extern "C" fn(*mut c_void)) {
    unsafe {
        signal(sig, SIG_IGN);
        let main_queue = &raw const _dispatch_main_q as *mut c_void;
        let source_type = &raw const _dispatch_source_type_signal as *const c_void;
        let source = dispatch_source_create(source_type, sig as usize, 0, main_queue);
        if !source.is_null() {
            dispatch_source_set_event_handler_f(source, handler);
            dispatch_resume(source);
        }
    }
}

extern "C" fn handle_terminate_signal(_ctx: *mut c_void) {
    if !SHUTDOWN_STARTED.swap(true, Ordering::SeqCst) {
        crate::menu::perform_graceful_shutdown("Received termination signal; shutting down.");
    }
}

extern "C" fn handle_reload_signal(_ctx: *mut c_void) {
    crate::menu::perform_reload_config();
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn lock_file_path() -> PathBuf {
    if let Ok(tmpdir) = std::env::var("TMPDIR")
        && !tmpdir.is_empty()
    {
        return PathBuf::from(tmpdir).join("mouse-control.lock");
    }
    PathBuf::from(format!("/tmp/mouse-control-{}.lock", unsafe { getuid() }))
}

fn read_lock_pid(file: &mut File) -> Option<u32> {
    let mut buf = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    file.read_to_string(&mut buf).ok()?;
    buf.trim().parse::<u32>().ok()
}

fn probe_instance_lock() -> LockState {
    let path = lock_file_path();
    let Ok(mut file) = OpenOptions::new().read(true).write(true).open(&path) else {
        return LockState::NotRunning;
    };
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if rc != 0 && std::io::Error::last_os_error().kind() == ErrorKind::WouldBlock {
        return LockState::Running(read_lock_pid(&mut file));
    }
    LockState::NotRunning
}

fn plist_path(label: &str) -> PathBuf {
    home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"))
}

fn legacy_binary() -> PathBuf {
    home_dir().join(".local").join("bin").join("mouse-control")
}

fn gui_domain() -> String {
    format!("gui/{}", unsafe { getuid() })
}

fn service_target(label: &str) -> String {
    format!("{}/{label}", gui_domain())
}

fn print_usage() {
    println!("{}", t("service.usage"));
}

fn resolve_sign_identity(cli_identity: Option<&str>) -> String {
    if let Some(id) = cli_identity.filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    if let Ok(id) = std::env::var(SIGN_IDENTITY_ENV)
        && !id.is_empty()
    {
        return id;
    }
    DEFAULT_SIGN_IDENTITY.to_string()
}

fn has_signing_identity(identity: &str) -> Result<bool, String> {
    let out = Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let quoted = format!("\"{identity}\"");
    Ok(text.contains(&quoted))
}

fn cmd_sign(cli_identity: Option<&str>) -> i32 {
    let identity = resolve_sign_identity(cli_identity);
    match has_signing_identity(&identity) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "{}",
                tf("service.sign_identity_missing", &[("identity", &identity)])
            );
            return 1;
        }
        Err(e) => return fail("security find-identity", &e),
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p.canonicalize().unwrap_or(p),
        Err(e) => return fail("current_exe", &e.to_string()),
    };

    let result = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", &identity, "--identifier", BUNDLE_ID])
        .arg(&exe)
        .output();
    match result {
        Ok(o) if o.status.success() => {
            let path = display(&exe);
            println!(
                "{}",
                tf(
                    "service.sign_done",
                    &[("path", &path), ("identity", &identity)]
                )
            );
            if let Some(req) = designated_requirement(&exe) {
                println!("{}", tf("service.status_requirement", &[("req", &req)]));
            }
            println!("{}", t("service.sign_restart_hint"));
            0
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            eprintln!("{}", tf("service.sign_failed", &[("error", &err)]));
            1
        }
        Err(e) => {
            let err = e.to_string();
            eprintln!("{}", tf("service.sign_failed", &[("error", &err)]));
            1
        }
    }
}

fn cmd_status() -> i32 {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_canon = exe.canonicalize().unwrap_or_else(|_| exe.clone());
    let p = display(&exe_canon);
    let v = env!("CARGO_PKG_VERSION");
    println!("{}", tf("service.status_this", &[("path", &p), ("version", v)]));

    let cfg_path = config::resolved_config_path();
    let cfg_disp = display(&cfg_path);
    let cfg_state = if cfg_path.exists() {
        t("service.state_present")
    } else {
        t("service.state_missing")
    };
    println!(
        "{}",
        tf(
            "service.status_config",
            &[("path", &cfg_disp), ("state", &cfg_state)]
        )
    );

    match probe_instance_lock() {
        LockState::Running(Some(pid)) => {
            let pid_str = pid.to_string();
            println!("{}", tf("service.status_instance_running", &[("pid", &pid_str)]));
        }
        LockState::Running(None) => {
            println!("{}", tf("service.status_instance_running", &[("pid", "?")]));
        }
        LockState::NotRunning => {
            println!("{}", t("service.status_instance_stopped"));
        }
    }

    let ax_state = if has_accessibility_permission() {
        t("service.state_granted")
    } else {
        t("service.state_missing")
    };
    println!(
        "{}",
        tf("service.status_accessibility", &[("state", &ax_state)])
    );

    let brew_plist = plist_path(HOMEBREW_LABEL);
    let brew_plist_state = if brew_plist.exists() {
        t("service.state_present")
    } else {
        t("service.state_missing")
    };
    let brew_plist_disp = display(&brew_plist);
    println!(
        "{}",
        tf(
            "service.status_plist",
            &[("path", &brew_plist_disp), ("state", &brew_plist_state)]
        )
    );

    match launchctl_print(HOMEBREW_LABEL) {
        Some(out) => {
            let pid = extract_field(&out, "pid").unwrap_or("-");
            let state = extract_field(&out, "state").unwrap_or("-");
            println!(
                "{}",
                tf(
                    "service.status_loaded",
                    &[("label", HOMEBREW_LABEL), ("pid", pid), ("state", state)]
                )
            );
        }
        None => println!(
            "{}",
            tf("service.status_not_loaded", &[("label", HOMEBREW_LABEL)])
        ),
    }

    let legacy_plist = plist_path(LEGACY_LABEL);
    let legacy_bin = legacy_binary();
    let legacy_loaded = launchctl_print(LEGACY_LABEL).is_some();
    if legacy_plist.exists() || legacy_bin.exists() || legacy_loaded {
        let target = service_target(LEGACY_LABEL);
        let lp = display(&legacy_plist);
        let lb = display(&legacy_bin);
        println!(
            "{}",
            tf(
                "service.status_legacy_warning",
                &[("target", &target), ("plist", &lp), ("bin", &lb)]
            )
        );
    }

    if exe_canon.exists()
        && let Some(req) = designated_requirement(&exe_canon)
    {
        println!("{}", tf("service.status_requirement", &[("req", &req)]));
        let mode_key = if is_persistent_requirement(&req) {
            "service.sign_mode_persistent"
        } else {
            "service.sign_mode_adhoc"
        };
        println!("{}", t(mode_key));
    }
    0
}

fn run_launchctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn launchctl_print(label: &str) -> Option<String> {
    run_launchctl(&["print", &service_target(label)]).ok()
}

fn extract_field<'a>(print_out: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key} = ");
    print_out
        .lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .map(str::trim)
}

fn designated_requirement(path: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().skip_while(|l| !l.contains("designated")).peekable();
    lines.peek()?;
    let joined = lines.map(str::trim).collect::<Vec<_>>().join(" ");
    Some(joined)
}

fn is_persistent_requirement(req: &str) -> bool {
    req.contains("certificate") && !req.contains("cdhash")
}

fn display(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn fail(action: &str, error: &str) -> i32 {
    eprintln!(
        "{}",
        tf("service.op_failed", &[("action", action), ("error", error)])
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn parse_run_and_flags() {
        assert_eq!(
            parse_args(&s(&[])),
            Ok(ParsedCommand::Run(RunOptions {
                config_path: None,
                log_stdout: false,
            }))
        );
        assert_eq!(
            parse_args(&s(&["run", "--log-stdout", "--config", "/tmp/c.toml"])),
            Ok(ParsedCommand::Run(RunOptions {
                config_path: Some(PathBuf::from("/tmp/c.toml")),
                log_stdout: true,
            }))
        );
        assert_eq!(
            parse_args(&s(&["--config=/tmp/c.toml", "--log-stdout"])),
            Ok(ParsedCommand::Run(RunOptions {
                config_path: Some(PathBuf::from("/tmp/c.toml")),
                log_stdout: true,
            }))
        );
    }

    #[test]
    fn parse_subcommands_and_removed_install() {
        assert_eq!(
            parse_args(&s(&["status", "--config=/tmp/a.toml"])),
            Ok(ParsedCommand::Status {
                config_path: Some(PathBuf::from("/tmp/a.toml")),
            })
        );
        assert_eq!(
            parse_args(&s(&["sign", "--identity", "my-cert"])),
            Ok(ParsedCommand::Sign {
                identity: Some("my-cert".to_string()),
            })
        );
        assert_eq!(
            parse_args(&s(&["install"])),
            Ok(ParsedCommand::RemovedInstall("install".to_string()))
        );
        assert_eq!(
            parse_args(&s(&["uninstall"])),
            Ok(ParsedCommand::RemovedInstall("uninstall".to_string()))
        );
        assert!(parse_args(&s(&["run", "--config"])).is_err());
    }

    #[test]
    fn classifies_persistent_vs_adhoc_designated_requirement() {
        assert!(is_persistent_requirement(
            r#"designated => identifier "com.eacryo.mouse-control" and certificate root = H"0123456789""#
        ));
        assert!(!is_persistent_requirement(
            r#"designated => cdhash H"abcdef0123456789""#
        ));
    }
}
