use crate::i18n::{t, tf};
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "com.eacryo.mouse-control";

extern "C" {
    fn getuid() -> u32;
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

fn install_dir() -> PathBuf {
    home_dir().join(".local").join("bin")
}

fn installed_binary() -> PathBuf {
    install_dir().join("mouse-control")
}

fn plist_path() -> PathBuf {
    home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn log_dir() -> PathBuf {
    home_dir().join("Library").join("Logs").join("mouse-control")
}

fn gui_domain() -> String {
    format!("gui/{}", unsafe { getuid() })
}

fn service_target() -> String {
    format!("{}/{LABEL}", gui_domain())
}

pub(crate) fn dispatch(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        None | Some("run") => None,
        Some("install") => Some(cmd_install(&args[1..])),
        Some("uninstall") => Some(cmd_uninstall()),
        Some("status") => Some(cmd_status()),
        Some("--version") | Some("version") => {
            println!("mouse-control {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        Some("--help") | Some("-h") | Some("help") => {
            print_usage();
            Some(0)
        }
        Some(other) => {
            eprintln!("{}", tf("service.unknown_command", &[("cmd", other)]));
            print_usage();
            Some(2)
        }
    }
}

fn print_usage() {
    println!("{}", t("service.usage"));
}

fn cmd_install(args: &[String]) -> i32 {

    if let Some(other) = args.first() {
        eprintln!("{}", tf("service.unknown_command", &[("cmd", other)]));
        return 2;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail("current_exe", &e.to_string()),
    };
    let target = installed_binary();

    if same_path(&exe, &target) {
        let p = display(&target);
        println!("{}", tf("service.install_copy_skip", &[("path", &p)]));
    } else {
        if let Err(e) = std::fs::create_dir_all(install_dir()) {
            return fail("create install dir", &e.to_string());
        }
        let tmp = install_dir().join(".mouse-control.new");
        if let Err(e) = std::fs::copy(&exe, &tmp) {
            return fail("copy binary", &e.to_string());
        }
        if let Err(e) = std::fs::rename(&tmp, &target) {
            return fail("replace binary", &e.to_string());
        }
        let p = display(&target);
        println!("{}", tf("service.install_copy", &[("path", &p)]));
    }

    if !sign(&target) {
        return 1;
    }

    if let Err(e) = write_plist() {
        return fail("write plist", &e.to_string());
    }
    let p = display(&plist_path());
    println!("{}", tf("service.plist_written", &[("path", &p)]));

    if agent_loaded() {
        println!("{}", t("service.agent_replaced"));
        let _ = run_launchctl(&["bootout", &service_target()]);
    }
    let plist = display(&plist_path());
    let domain = gui_domain();
    match run_launchctl(&["bootstrap", &domain, &plist]) {
        Ok(_) => println!("{}", tf("service.agent_booted", &[("label", LABEL)])),
        Err(e) => return fail("launchctl bootstrap", &e),
    }

    let p = display(&target);
    println!("{}", tf("service.install_done", &[("path", &p)]));
    0
}

fn cmd_uninstall() -> i32 {
    if !plist_path().exists() && !installed_binary().exists() {
        let p = display(&plist_path());
        println!("{}", tf("service.not_installed", &[("path", &p)]));
        return 0;
    }
    if agent_loaded() {
        let _ = run_launchctl(&["bootout", &service_target()]);
    } else {
        println!("{}", t("service.agent_not_loaded"));
    }
    let plist = plist_path();
    if plist.exists() {
        if let Err(e) = std::fs::remove_file(&plist) {
            return fail("remove plist", &e.to_string());
        }
    }
    let bin = installed_binary();
    if bin.exists() {
        if let Err(e) = std::fs::remove_file(&bin) {
            return fail("remove binary", &e.to_string());
        }
    }
    println!("{}", tf("service.uninstall_done", &[("label", LABEL)]));
    let log = display(&log_dir());
    println!(
        "{}",
        tf(
            "service.uninstall_kept",
            &[("config_dir", "~/.config/mouse-control"), ("log_dir", &log)]
        )
    );
    0
}

fn cmd_status() -> i32 {
    let exe = std::env::current_exe().unwrap_or_default();
    let p = display(&exe);
    let v = env!("CARGO_PKG_VERSION");
    println!("{}", tf("service.status_this", &[("path", &p), ("version", v)]));

    let bin = installed_binary();
    if bin.exists() {

        let ver = Command::new(&bin)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "?".to_string());
        let p = display(&bin);
        println!(
            "{}",
            tf("service.status_installed", &[("path", &p), ("version", &ver)])
        );
    } else {
        let p = display(&bin);
        println!(
            "{}",
            tf("service.status_installed_missing", &[("path", &p)])
        );
    }

    let plist = plist_path();
    let state_key = if plist.exists() {
        "service.state_present"
    } else {
        "service.state_missing"
    };
    let p = display(&plist);
    let state = t(state_key);
    println!(
        "{}",
        tf("service.status_plist", &[("path", &p), ("state", &state)])
    );

    match launchctl_print() {
        Some(out) => {
            let pid = extract_field(&out, "pid").unwrap_or("-");
            let state = extract_field(&out, "state").unwrap_or("-");
            println!(
                "{}",
                tf("service.status_loaded", &[("pid", pid), ("state", state)])
            );
        }
        None => println!("{}", t("service.status_not_loaded")),
    }

    if bin.exists() {
        if let Some(req) = designated_requirement(&bin) {
            println!("{}", tf("service.status_requirement", &[("req", &req)]));
        }
    }
    0
}

fn run_launchctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn agent_loaded() -> bool {
    launchctl_print().is_some()
}

fn launchctl_print() -> Option<String> {
    run_launchctl(&["print", &service_target()]).ok()
}

fn extract_field<'a>(print_out: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key} = ");
    print_out
        .lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .map(str::trim)
}

fn sign(path: &Path) -> bool {
    let result = Command::new("codesign")
        .args(["--force", "--sign", "-", "--identifier", LABEL])
        .arg(path)
        .output();
    match result {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            eprintln!("{}", tf("service.sign_failed", &[("error", &err)]));
            false
        }
        Err(e) => {
            let err = e.to_string();
            eprintln!("{}", tf("service.sign_failed", &[("error", &err)]));
            false
        }
    }
}

fn designated_requirement(path: &Path) -> Option<String> {
    let out = Command::new("codesign")
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

fn render_plist() -> String {
    let bin = xml_escape(&display(&installed_binary()));
    let stdout_log = xml_escape(&display(&log_dir().join("launchd-stdout.log")));
    let stderr_log = xml_escape(&display(&log_dir().join("launchd-stderr.log")));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{bin}</string>
		<string>run</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>StandardOutPath</key>
	<string>{stdout_log}</string>
	<key>StandardErrorPath</key>
	<string>{stderr_log}</string>
</dict>
</plist>
"#
    )
}

fn write_plist() -> std::io::Result<()> {
    let path = plist_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(log_dir())?;
    std::fs::write(&path, render_plist())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn display(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn fail(action: &str, error: &str) -> i32 {
    eprintln!(
        "{}",
        tf("service.op_failed", &[("action", action), ("error", error)])
    );
    1
}
