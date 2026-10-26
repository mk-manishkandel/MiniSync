//! Self-update from GitHub Releases.
//!
//! The release workflow (.github/workflows/release.yml) builds the app for
//! each platform and signs every file with the project's Ed25519 release key.
//! The app only installs a download whose signature matches the public key
//! below, so a tampered file - or a release made without the private key -
//! is refused.
//!
//! Downloads use the system's `curl` (built into Windows 10+ and macOS), so
//! the app needs no TLS library of its own.

use crate::config::Home;
use crate::service;
use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const REPO: &str = "mk-manishkandel/MiniSync";
/// Project page: downloads, help and source code.
pub const WEBSITE: &str = "https://github.com/mk-manishkandel/MiniSync";

/// Public half of the release signing key (tools/release-sign keygen).
const RELEASE_PUBLIC_KEY: &str = include_str!("../release-public-key.txt");

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    url: String,
    sig_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Name of the release file for this platform, e.g. "minisync-windows-x64.exe".
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("minisync-windows-x64.exe"),
        ("macos", "aarch64") => Some("minisync-macos-arm64"),
        ("macos", "x86_64") => Some("minisync-macos-x64"),
        _ => None,
    }
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn curl() -> Command {
    let mut c = Command::new("curl");
    service::quiet(&mut c);
    c.args(["-fsSL", "--retry", "2", "-H", "User-Agent: minisync-updater"]);
    c.stdin(Stdio::null());
    c
}

/// Asks GitHub for the latest release. Returns it only if it is newer than
/// this build and has a file for this platform.
pub fn check() -> Result<Option<Release>> {
    let Some(asset) = asset_name() else { return Ok(None) };
    let out = curl()
        .args(["--max-time", "30", "-H", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!("could not reach GitHub ({})", String::from_utf8_lossy(&out.stderr).trim());
    }
    let rel: GhRelease = serde_json::from_slice(&out.stdout).context("unexpected reply from GitHub")?;
    if rel.draft || rel.prerelease || !is_newer(&rel.tag_name, CURRENT_VERSION) {
        return Ok(None);
    }
    let find = |name: &str| rel.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let (Some(url), Some(sig_url)) = (find(asset), find(&format!("{asset}.sig"))) else {
        log::warn!("release {} has no signed file for this platform", rel.tag_name);
        return Ok(None);
    };
    Ok(Some(Release { version: rel.tag_name.trim_start_matches('v').to_string(), url, sig_url }))
}

fn download(url: &str, to: &Path) -> Result<()> {
    let out = curl().args(["--max-time", "900", "-o"]).arg(to).arg(url).output().context("running curl")?;
    if !out.status.success() {
        bail!("download failed ({})", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn decode_hex<const N: usize>(s: &str) -> Result<[u8; N]> {
    let s = s.trim();
    if s.len() != N * 2 {
        bail!("wrong length");
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

pub fn verify(data: &[u8], signature_hex: &str) -> Result<()> {
    let key = VerifyingKey::from_bytes(&decode_hex::<32>(RELEASE_PUBLIC_KEY).context("bad built-in release key")?)?;
    let sig = Signature::from_bytes(&decode_hex::<64>(signature_hex).context("malformed signature file")?);
    key.verify_strict(data, &sig).context("signature does not match - refusing to install this update")?;
    Ok(())
}

/// Downloads the new version next to the installed one and checks its signature.
fn download_verified(rel: &Release) -> Result<PathBuf> {
    let dir = service::install_dir()?;
    std::fs::create_dir_all(&dir)?;
    let new = dir.join(if cfg!(windows) { "minisync.new.exe" } else { "minisync.new" });
    let sig = dir.join("minisync.new.sig");
    download(&rel.url, &new)?;
    download(&rel.sig_url, &sig)?;
    let data = std::fs::read(&new)?;
    let sig_text = std::fs::read_to_string(&sig)?;
    let _ = std::fs::remove_file(&sig);
    if let Err(e) = verify(&data, &sig_text) {
        let _ = std::fs::remove_file(&new);
        return Err(e);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(new)
}

/// Puts the downloaded file in place of the installed program. Windows can't
/// overwrite a running .exe but can rename it, so the old one is moved aside.
fn swap_in(new: &Path) -> Result<PathBuf> {
    let installed = service::installed_exe()?;
    if cfg!(windows) && installed.exists() {
        // An older copy may still be running (e.g. a menu window that was left
        // open), which locks minisync.old.exe. Then use a fresh name instead.
        let mut old = installed.with_file_name("minisync.old.exe");
        if old.exists() && std::fs::remove_file(&old).is_err() {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
            old = installed.with_file_name(format!("minisync.old-{stamp}.exe"));
        }
        std::fs::rename(&installed, &old).context("moving the old version aside")?;
    }
    std::fs::rename(new, &installed).context("putting the new version in place")?;
    Ok(installed)
}

/// Removes the previous version left behind by an update (best effort).
pub fn cleanup_old() {
    if let Ok(dir) = service::install_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("minisync.old") && name.ends_with(".exe") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let _ = std::fs::remove_file(dir.join("minisync.new.exe"));
        let _ = std::fs::remove_file(dir.join("minisync.new"));
    }
}

/// Update from the menu window: the sync runs in another process, so stop
/// it, swap the file and start the new version.
pub fn install_from_menu(home: &Home, port: u16, rel: &Release) -> Result<()> {
    if !service::installed_exe()?.exists() {
        bail!("Mini Sync isn't installed on this computer yet");
    }
    let me = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
    if me.is_some_and(|n| n.starts_with("minisync.old")) {
        bail!("this window is an older version that was already replaced - close it and reopen Mini Sync");
    }
    let new = download_verified(rel)?;
    service::stop(port);
    swap_in(&new)?;
    service::start(home, port)
}

/// Update from the background process itself (tray). Swaps the file, starts
/// the new version and tells the caller to exit.
pub fn install_from_background(home: &Home, rel: &Release) -> Result<()> {
    if !service::running_from_install() {
        bail!("this copy is not the installed one; update from the menu instead");
    }
    let new = download_verified(rel)?;
    let installed = swap_in(&new)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        Command::new(&installed)
            .arg("--home")
            .arg(&home.dir)
            .args(["run", "--background", "--after-update"])
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting the new version")?;
    }
    // macOS: launchd (KeepAlive) restarts the program from the new file once we exit.
    #[cfg(not(windows))]
    let _ = (home, installed);
    Ok(())
}

/// What the tray shows about updates.
#[derive(Debug, Clone)]
pub enum UpdateState {
    UpToDate,
    Available(Release),
    Working(String),
    Failed(String),
}

pub type SharedUpdate = std::sync::Arc<std::sync::Mutex<UpdateState>>;

/// Runs one check and records the result.
pub fn check_into(shared: &SharedUpdate) {
    let result = match check() {
        Ok(Some(rel)) => {
            log::info!("update available: v{} (running v{CURRENT_VERSION})", rel.version);
            UpdateState::Available(rel)
        }
        Ok(None) => {
            log::info!("update check: v{CURRENT_VERSION} is the latest");
            UpdateState::UpToDate
        }
        Err(e) => {
            log::warn!("update check failed: {e:#}");
            UpdateState::Failed(format!("{e:#}"))
        }
    };
    *shared.lock().unwrap() = result;
}

/// Checks a minute after start, then every 6 hours.
pub fn spawn_checker(shared: SharedUpdate) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        loop {
            let busy = matches!(*shared.lock().unwrap(), UpdateState::Working(_));
            if !busy {
                check_into(&shared);
            }
            std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("v0.3.10", "0.3.9"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
    }

    #[test]
    fn rejects_bad_signature() {
        assert!(verify(b"hello", &"00".repeat(64)).is_err());
    }
}
