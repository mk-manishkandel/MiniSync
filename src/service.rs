//! Installing minisync as a background program that starts at login:
//! Windows: copy to %LOCALAPPDATA%\MiniSync, "Run" registry entry, Start Menu shortcut.
//! macOS:   copy to ~/Library/Application Support/MiniSync, LaunchAgent.

use crate::config::Home;
use anyhow::{bail, Context, Result};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "com.minisync";

pub fn install_dir() -> Result<PathBuf> {
    Ok(dirs::data_local_dir().context("cannot find the local data directory")?.join("MiniSync"))
}

pub fn installed_exe() -> Result<PathBuf> {
    let name = if cfg!(windows) { "minisync.exe" } else { "minisync" };
    Ok(install_dir()?.join(name))
}

pub fn is_running(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// Copies this program to its install location, registers it to start at
/// login and starts it in the background.
pub fn install(home: &Home, port: u16) -> Result<PathBuf> {
    let target = installed_exe()?;
    let current = std::env::current_exe()?;
    stop(port);
    if !same_file(&current, &target) {
        std::fs::create_dir_all(install_dir()?)?;
        let mut tries = 0;
        loop {
            match std::fs::copy(&current, &target) {
                Ok(_) => break,
                Err(e) if tries < 10 => {
                    tries += 1;
                    log::debug!("copy failed ({e}), retrying");
                    std::thread::sleep(Duration::from_millis(300));
                }
                Err(e) => return Err(e).with_context(|| format!("copying to {}", target.display())),
            }
        }
    }
    set_autostart(&target, home)?;
    #[cfg(windows)]
    if let Err(e) = start_menu_shortcut(&target, true) {
        log::warn!("could not create Start Menu shortcut: {e:#}");
    }
    start(home, port)?;
    Ok(target)
}

/// Stops the background copy and removes autostart, the installed program
/// and (unless `keep_settings`) the settings, key, index and log.
/// Synced files are never touched, nor is `.minisync-trash`.
pub fn uninstall(home: &Home, port: u16, keep_settings: bool) -> Result<()> {
    let folders = home.load_config().map(|c| c.folders).unwrap_or_default();
    stop(port);
    remove_autostart(home)?;
    #[cfg(windows)]
    let _ = start_menu_shortcut(&installed_exe()?, false);

    if !keep_settings {
        for f in &folders {
            let _ = std::fs::remove_file(f.path.join(crate::util::MARKER));
            let _ = std::fs::remove_dir_all(f.path.join(crate::util::TMP_DIR));
        }
        if home.dir.exists() {
            std::fs::remove_dir_all(&home.dir).with_context(|| format!("removing {}", home.dir.display()))?;
        }
    }

    let dir = install_dir()?;
    if running_from_install() && cfg!(windows) {
        // Windows can't delete a running .exe: remove it a moment after we exit.
        let script = format!("ping 127.0.0.1 -n 3 > nul & rmdir /s /q \"{}\"", dir.display());
        let mut c = Command::new("cmd");
        quiet(&mut c);
        let _ = c.args(["/c", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    } else if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

/// True if this process is the installed copy (not e.g. a download).
pub fn running_from_install() -> bool {
    let Ok(dir) = install_dir() else { return false };
    std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok())
        .zip(dir.canonicalize().ok())
        .map_or(false, |(exe, d)| exe.starts_with(d))
}

/// Opens the setup/menu window (used by the tray icon).
pub fn open_menu(home: &Home) {
    let Ok(exe) = std::env::current_exe() else { return };
    #[cfg(target_os = "macos")]
    {
        let _ = home;
        let _ = Command::new("open").args(["-a", "Terminal"]).arg(&exe).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = Command::new(exe).arg("--home").arg(&home.dir).spawn();
    }
}

fn background_args(home: &Home) -> Vec<String> {
    vec!["--home".into(), home.dir.display().to_string(), "run".into(), "--background".into()]
}

fn exe_to_start() -> Result<PathBuf> {
    let installed = installed_exe()?;
    if installed.exists() {
        Ok(installed)
    } else {
        Ok(std::env::current_exe()?)
    }
}

pub fn restart(home: &Home, port: u16) -> Result<()> {
    stop(port);
    start(home, port)
}

pub fn start(home: &Home, port: u16) -> Result<()> {
    if is_running(port) {
        return Ok(());
    }
    start_platform(home)?;
    for _ in 0..20 {
        if is_running(port) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("started, but it is not answering yet; check the log ({})", log_path(home).display())
}

pub fn stop(port: u16) {
    stop_platform();
    for _ in 0..20 {
        if !is_running(port) {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

pub fn log_path(home: &Home) -> PathBuf {
    home.dir.join("minisync.log")
}

/// Opens a file or folder with the system's default app.
pub fn open_path(path: &Path) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// Keeps a Mac from going to sleep while files are being transferred (a
/// laptop otherwise sleeps mid-transfer and the connection drops; the
/// download resumes after, but much later). The screen may still turn off.
/// Called about once a second.
///
/// Not on Windows: v0.3.8 did it there with SetThreadExecutionState, and
/// Microsoft Defender's ML model then flagged the app as a trojan
/// (Bearfoos.A!ml) and quarantined installed copies.
pub fn keep_awake(busy: bool) {
    #[cfg(target_os = "macos")]
    {
        static CAFFEINATE: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);
        let mut child = CAFFEINATE.lock().unwrap();
        if busy && child.is_none() {
            // -w: caffeinate quits by itself if Mini Sync exits.
            *child = Command::new("caffeinate")
                .args(["-i", "-w", &std::process::id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok();
        } else if !busy {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = busy;
}

pub fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    let cmd = if cfg!(windows) { "clip" } else if cfg!(target_os = "macos") { "pbcopy" } else { "xclip" };
    let mut c = Command::new(cmd);
    quiet(&mut c);
    let Ok(mut child) = c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Keeps helper commands (reg, taskkill, ...) from flashing a console window.
#[allow(unused_variables)]
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run_quiet(program: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut c = Command::new(program);
    quiet(&mut c);
    c.args(args).stdin(Stdio::null());
    c.output().with_context(|| format!("running {program}"))
}

// ---------------- Windows ----------------

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
fn set_autostart(exe: &Path, home: &Home) -> Result<()> {
    let mut command = format!("\"{}\"", exe.display());
    for a in background_args(home) {
        if a.contains(' ') {
            command.push_str(&format!(" \"{a}\""));
        } else {
            command.push_str(&format!(" {a}"));
        }
    }
    let out = run_quiet("reg", &["add", RUN_KEY, "/v", "MiniSync", "/t", "REG_SZ", "/d", &command, "/f"])?;
    if !out.status.success() {
        bail!("reg add failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

#[cfg(windows)]
fn remove_autostart(_home: &Home) -> Result<()> {
    let _ = run_quiet("reg", &["delete", RUN_KEY, "/v", "MiniSync", "/f"]);
    Ok(())
}

#[cfg(windows)]
fn start_platform(home: &Home) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    Command::new(exe_to_start()?)
        .args(background_args(home))
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting background process")?;
    Ok(())
}

#[cfg(windows)]
fn stop_platform() {
    let me = std::process::id().to_string();
    let _ = run_quiet(
        "taskkill",
        &["/F", "/FI", "IMAGENAME eq minisync.exe", "/FI", &format!("PID ne {me}")],
    );
}

/// Lets your other devices reach this one through Windows Firewall. Replaces
/// every rule for the installed program, including the "Block" rules Windows
/// saves when its own "Allow access?" prompt is cancelled, with one Allow rule
/// for all network types (connections are still only accepted from device IDs
/// you added). Needs admin rights, so Windows asks first.
#[cfg(windows)]
pub fn allow_through_firewall() -> Result<()> {
    let exe = installed_exe()?.display().to_string().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop'; $exe='{exe}'; \
         Get-NetFirewallApplicationFilter | Where-Object {{ $_.Program -eq $exe }} | Get-NetFirewallRule | Remove-NetFirewallRule; \
         New-NetFirewallRule -DisplayName 'Mini Sync' -Direction Inbound -Action Allow -Program $exe -Profile Any | Out-Null"
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = data_encoding::BASE64.encode(&utf16);
    let elevate = format!(
        "$p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden \
         -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'; exit $p.ExitCode"
    );
    let out = run_quiet("powershell", &["-NoProfile", "-NonInteractive", "-Command", &elevate])?;
    if !out.status.success() {
        bail!("Windows did not allow it (was the permission prompt declined?)");
    }
    Ok(())
}

#[cfg(windows)]
fn start_menu_shortcut(exe: &Path, create: bool) -> Result<()> {
    let programs = dirs::data_dir()
        .context("no AppData")?
        .join(r"Microsoft\Windows\Start Menu\Programs");
    let lnk = programs.join("Mini Sync.lnk");
    if !create {
        let _ = std::fs::remove_file(&lnk);
        return Ok(());
    }
    let esc = |p: &Path| p.display().to_string().replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.Description='Mini Sync';$s.Save()",
        esc(&lnk),
        esc(exe)
    );
    let out = run_quiet("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script])?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

// ---------------- macOS ----------------

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home directory")?
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn gui_domain() -> String {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "501".into());
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
fn set_autostart(exe: &Path, home: &Home) -> Result<()> {
    let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut args = format!("    <string>{}</string>\n", xml(&exe.display().to_string()));
    for a in background_args(home) {
        args.push_str(&format!("    <string>{}</string>\n", xml(&a)));
    }
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#
    );
    let path = plist_path()?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, plist)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_autostart(_home: &Home) -> Result<()> {
    let _ = std::fs::remove_file(plist_path()?);
    Ok(())
}

#[cfg(target_os = "macos")]
fn start_platform(home: &Home) -> Result<()> {
    let plist = plist_path()?;
    if !plist.exists() {
        set_autostart(&exe_to_start()?, home)?;
    }
    let domain = gui_domain();
    let _ = run_quiet("launchctl", &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")]);
    let out = run_quiet("launchctl", &["bootstrap", &domain, &plist.display().to_string()])?;
    if !out.status.success() {
        bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn stop_platform() {
    let _ = run_quiet("launchctl", &["bootout", &format!("{}/{LAUNCHD_LABEL}", gui_domain())]);
}

// ---------------- other Unix (best effort, no autostart) ----------------

#[cfg(all(unix, not(target_os = "macos")))]
fn set_autostart(_exe: &Path, _home: &Home) -> Result<()> {
    log::warn!("start-at-login is not set up on this OS; add `minisync run --background` to your session startup");
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn remove_autostart(_home: &Home) -> Result<()> {
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn start_platform(home: &Home) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Command::new(exe_to_start()?)
        .args(background_args(home))
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn stop_platform() {
    let _ = run_quiet("pkill", &["-f", "minisync.*run --background"]);
}
