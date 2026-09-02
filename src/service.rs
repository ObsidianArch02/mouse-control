//! 服务注册与生命周期管理:把单二进制安装成 launchd LaunchAgent(登录自启、崩溃自动重启),
//! 菜单栏图标照常显示。install/uninstall/status 子命令都在这里实现,不初始化 AppKit,
//! 因此在无 GUI 的会话(如 SSH)里也能执行。
//!
//! Service registration and lifecycle: install the single binary as a launchd LaunchAgent
//! (start at login, restart on crash) while the menu-bar icon still shows. The
//! install/uninstall/status subcommands live here, never touch AppKit, and therefore work
//! in non-GUI sessions (e.g. SSH).

use crate::i18n::{t, tf};
use std::path::{Path, PathBuf};
use std::process::Command;

/// launchd 任务的 Label,也是 codesign 的 identifier。沿用 app 形态时的 bundle id。
/// launchd job label; also the codesign identifier. Keeps the bundle id from the app form.
const LABEL: &str = "com.eacryo.mouse-control";

/// 自签名代码签名证书的推荐名字。钥匙串里存在该证书时 install 自动用它签名:
/// TCC 的 designated requirement 锚定在证书 leaf 哈希上,重装不再重置辅助功能授权;
/// ad-hoc 签名则锚定 cdhash,每次重装都失效。
/// Recommended name for a self-signed code-signing certificate. When present in the keychain,
/// install signs with it automatically: TCC's designated requirement then anchors to the
/// cert's leaf hash and Accessibility grants survive reinstalls, whereas ad-hoc signing pins
/// to the cdhash and breaks on every reinstall.
const CERT_NAME: &str = "mouse-control Code Signing";

extern "C" {
    fn getuid() -> u32;
}

// ========== 路径 / paths ==========

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

/// 安装位置:~/.local/bin。用户可写、免 sudo,且对 launchd 是稳定路径。
/// Install location: ~/.local/bin. User-writable, no sudo, and a stable path for launchd.
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

// ========== 子命令分发 / dispatch ==========

/// 返回 Some(exit_code) 表示子命令已处理完毕、进程应退出;None 表示进入正常 app 运行流程。
/// Some(exit_code) means the subcommand is handled and the process should exit;
/// None means fall through to the normal app run.
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

// ========== install ==========

fn cmd_install(args: &[String]) -> i32 {
    // --sign <identity>:显式指定签名身份;缺省时自动探测 CERT_NAME,探测不到退化为 ad-hoc。
    // --sign <identity>: explicit signing identity; otherwise auto-detect CERT_NAME and fall
    // back to ad-hoc when absent.
    let mut sign_identity: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sign" => match args.get(i + 1) {
                Some(v) => {
                    sign_identity = Some(v.clone());
                    i += 2;
                }
                None => {
                    eprintln!("{}", t("service.sign_needs_value"));
                    return 2;
                }
            },
            other => {
                eprintln!("{}", tf("service.unknown_command", &[("cmd", other)]));
                return 2;
            }
        }
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail("current_exe", &e.to_string()),
    };
    let target = installed_binary();

    // 1. 复制二进制到安装位置。先写临时文件再 rename:目标可能正被运行中的 agent 占用,
    //    原子替换让旧进程继续持有旧 inode,避免 ETXTBSY,也不让运行中进程的签名失效。
    // 1. Copy the binary into place via temp file + rename: the target may be in use by the
    //    running agent; atomic replacement keeps the old inode alive, avoiding ETXTBSY and
    //    keeping the running process's signature valid.
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

    // 2. 签名。每次必签:刚复制的文件没有有效签名。
    // 2. Sign. Mandatory on every install: a freshly copied file has no valid signature.
    let identity = sign_identity.or_else(detect_cert);
    if !sign(&target, identity.as_deref()) {
        return 1;
    }
    match identity {
        Some(id) => println!("{}", tf("service.sign_identity", &[("identity", &id)])),
        None => {
            println!("{}", t("service.sign_adhoc"));
            let cert = CERT_NAME;
            println!("{}", tf("service.sign_cert_hint", &[("cert", cert)]));
        }
    }

    // 3. 写 plist 并加载(已加载则先 bootout 再 bootstrap,等价于重启)。
    // 3. Write the plist and load it (bootout first when already loaded, i.e. a restart).
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

// ========== uninstall ==========

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

// ========== status ==========

fn cmd_status() -> i32 {
    let exe = std::env::current_exe().unwrap_or_default();
    let p = display(&exe);
    let v = env!("CARGO_PKG_VERSION");
    println!("{}", tf("service.status_this", &[("path", &p), ("version", v)]));

    let bin = installed_binary();
    if bin.exists() {
        // 装的二进制是什么版本:问它自己(--version 不启动 AppKit)。
        // Which version is installed: ask the binary itself (--version never starts AppKit).
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

// ========== launchctl / codesign helpers ==========

/// 跑一个 launchctl 子命令;成功返回 stdout,失败返回带 stderr 的 Err。
/// Run a launchctl subcommand; Ok(stdout) on success, Err(stderr) on failure.
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

/// agent 是否已加载:`launchctl print <target>` 退出码为 0 即已加载。
/// Whether the agent is loaded: `launchctl print <target>` exits 0 when it is.
fn agent_loaded() -> bool {
    launchctl_print().is_some()
}

fn launchctl_print() -> Option<String> {
    run_launchctl(&["print", &service_target()]).ok()
}

/// 从 `launchctl print` 输出里取 "key = value" 行的 value。
/// Extract the value of a "key = value" line from `launchctl print` output.
fn extract_field<'a>(print_out: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key} = ");
    print_out
        .lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .map(str::trim)
}

/// 探测钥匙串里的自签名证书。刻意不用 `security find-identity -v`:自签名根证书不受系统
/// 信任,-v 会把它过滤掉,而 codesign 签名只需要私钥、不要求受信。
/// Probe the keychain for the self-signed cert. Deliberately avoids
/// `security find-identity -v`: a self-signed root is untrusted and -v would filter it out,
/// while codesign only needs the private key, not trust.
fn detect_cert() -> Option<String> {
    let out = Command::new("security")
        .args(["find-identity", "-p", "codesigning"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains(CERT_NAME) {
        Some(CERT_NAME.to_string())
    } else {
        None
    }
}

/// 给已安装的二进制签名。identity 为 None 时 ad-hoc(--sign -)。--identifier 让 TCC 客户端名
/// 稳定;ad-hoc 下 designated requirement 仍锚定 cdhash(每次重装变化),提示文案由调用方输出。
/// Sign the installed binary. identity None means ad-hoc (--sign -). --identifier keeps the TCC
/// client name stable; under ad-hoc the designated requirement still pins to the cdhash (changes
/// on every reinstall), and the caller is responsible for telling the user.
fn sign(path: &Path, identity: Option<&str>) -> bool {
    let id_arg = identity.unwrap_or("-");
    let result = Command::new("codesign")
        .args(["--force", "--sign", id_arg, "--identifier", LABEL])
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

/// 读取二进制的 designated requirement(codesign -d -r-),即 TCC 授权真正比对的对象。
/// 输出可能折行,合并成一行返回。
/// Read the binary's designated requirement (codesign -d -r-), the thing TCC actually matches
/// grants against. The output may wrap across lines; collapse it into one.
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

// ========== plist 生成 / plist rendering ==========

/// 渲染 LaunchAgent plist。几个关键键值:
/// - RunAtLoad + KeepAlive.SuccessfulExit=false:登录即启动;崩溃重启,但从菜单正常退出
///   (exit 0)不会复活,留给用户真正的退出权。
/// - LimitLoadToSessionType=Aqua + ProcessType=Interactive:菜单栏图标需要 GUI 会话,
///   限定只在 Aqua 会话加载,并声明为交互型进程避免被后台调度降级。
/// - Standard{Out,Err}Path:launchd 下 stdout/stderr 没有终端,落盘到日志目录。
///
/// Render the LaunchAgent plist. Key entries:
/// - RunAtLoad + KeepAlive.SuccessfulExit=false: start at login; restart on crash, but a
///   deliberate menu Quit (exit 0) stays off.
/// - LimitLoadToSessionType=Aqua + ProcessType=Interactive: the menu-bar icon needs a GUI
///   session, so load only in Aqua sessions and declare the process interactive.
/// - Standard{Out,Err}Path: stdout/stderr have no terminal under launchd; land them in the log dir.
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

// ========== 小工具 / misc ==========

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn display(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 两个路径是否指向同一文件(canonicalize 失败时退化为字面比较)。
/// Whether two paths point at the same file (falls back to literal comparison when
/// canonicalize fails).
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
