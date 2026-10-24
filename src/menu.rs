//! What you see when you double-click the app: first-time setup, then a
//! small menu. Syncing itself always happens in the background process.
//!
//! Every question inside a menu option can be left with Enter or 0 ("back").

use crate::config::{default_ignores, Config, DeviceId, FolderConfig, Home, PeerConfig};
use crate::service;
use crate::ui;
use crate::update::{self, Release};
use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const DEFAULT_FOLDER_ID: &str = "minisync";

/// Result of the update check started when the menu opens.
#[derive(Clone)]
enum UpdateInfo {
    Checking,
    Latest,
    Available(Release),
    Unknown,
}

pub fn run(home: &Home) -> Result<()> {
    ui::init();
    if !home.config_path().exists() {
        setup(home)?;
    } else if !service::running_from_install() {
        // Opened from a copy somewhere else (e.g. a newer download).
        ui::clear();
        title_box("Mini Sync is already set up on this computer");
        ui::info(&format!("This copy is not the installed one ({}).", service::installed_exe()?.display()));
        println!();
        if confirm("Install this copy (update) and restart syncing? Your settings are kept.") {
            let cfg = home.load_config()?;
            match service::install(home, cfg.listen_port) {
                Ok(exe) => ui::ok(&format!("Updated. Running from {}.", exe.display())),
                Err(e) => ui::err(&format!("Could not update: {e:#}")),
            }
            pause();
        }
    }
    menu(home)
}

// ---------------- input helpers ----------------

fn read_line() -> String {
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        std::process::exit(0); // window closed
    }
    line.trim().trim_matches('"').to_string()
}

/// Question with a default that Enter accepts.
fn ask(question: &str, default: &str) -> String {
    if default.is_empty() {
        ui::prompt(&format!("{question}: "));
    } else {
        ui::prompt(&format!("{question} {}: ", ui::dim(&format!("[{default}]"))));
    }
    let line = read_line();
    if line.is_empty() {
        default.to_string()
    } else {
        line
    }
}

/// Question inside a menu option. Returns None when the user goes back
/// (types 0/b/back, or presses Enter when there is no default).
fn ask_back(question: &str, default: &str) -> Option<String> {
    if default.is_empty() {
        ui::prompt(&format!("{question} {}: ", ui::dim("(Enter or 0 = back)")));
    } else {
        ui::prompt(&format!("{question} {} {}: ", ui::dim(&format!("[{default}]")), ui::dim("(0 = back)")));
    }
    let line = read_line();
    if matches!(line.to_lowercase().as_str(), "0" | "b" | "back") {
        return None;
    }
    if line.is_empty() {
        return if default.is_empty() { None } else { Some(default.to_string()) };
    }
    Some(line)
}

/// Yes/no; anything other than "y"/"yes" (including Enter) means no / back.
fn confirm(question: &str) -> bool {
    ui::prompt(&format!("{question} {}: ", ui::dim("(y = yes, Enter = back)")));
    matches!(read_line().to_lowercase().as_str(), "y" | "yes")
}

fn pause() {
    println!();
    ui::prompt(&ui::dim("Press Enter to go back to the menu"));
    read_line();
}

fn title_box(subtitle: &str) {
    let title = format!("Mini Sync  v{}", update::CURRENT_VERSION);
    ui::boxed(&[
        (title.clone(), format!("{}  {}", ui::accent("Mini Sync"), ui::dim(&format!("v{}", update::CURRENT_VERSION)))),
        (subtitle.to_string(), subtitle.to_string()),
    ]);
}

// ---------------- first-time setup ----------------

fn setup(home: &Home) -> Result<()> {
    ui::clear();
    title_box("First-time setup - 3 quick steps");

    ui::section("Step 1 of 3 - this device");
    let name = ask("Name for this device", &crate::default_device_name());
    let default_folder = dirs::home_dir().unwrap_or_default().join("MiniSync");
    let folder = PathBuf::from(ask("Folder to sync", &default_folder.display().to_string()));

    let key = home.load_or_create_key()?;
    let mut cfg = Config {
        device_name: name,
        listen_port: 22100,
        folders: vec![FolderConfig {
            id: DEFAULT_FOLDER_ID.into(),
            path: folder,
            ignore: default_ignores(),
        }],
        peers: vec![],
    };
    home.save_config(&cfg)?;

    ui::section("Step 2 of 3 - this device's ID");
    show_id(&key.public);
    ui::info("Run Mini Sync on your other device too, and give each device the other's ID.");

    ui::section("Step 3 of 3 - your other device");
    if let Some(peer) = ask_peer(&key.public) {
        cfg.peers.push(peer);
        home.save_config(&cfg)?;
    } else {
        ui::info("Skipped - you can add it later from the menu (option 2).");
    }

    println!();
    ui::info("Installing and starting in the background...");
    match service::install(home, cfg.listen_port) {
        Ok(exe) => {
            ui::ok("Done! Mini Sync is running in the background and starts at every login.");
            ui::info(&format!("Installed to {}", exe.display()));
            ui::info("To open this menu again: tray icon -> Open Mini Sync.");
        }
        Err(e) => ui::err(&format!("Could not install: {e:#}")),
    }
    pause();
    Ok(())
}

fn show_id(id: &DeviceId) {
    println!("\n      {}\n", ui::bold(&id.to_string()));
    if service::copy_to_clipboard(&id.to_string()) {
        ui::ok("Copied to the clipboard");
    }
}

fn ask_peer(me: &DeviceId) -> Option<PeerConfig> {
    loop {
        let id = ask_back("Paste the other device's ID", "")?;
        match DeviceId::parse(&id) {
            Ok(parsed) if parsed == *me => ui::warn("That is this device's own ID. Paste the OTHER device's ID."),
            Ok(parsed) => {
                let name = ask_back("Name for that device", "my-other-device")?;
                return Some(PeerConfig { name, id: parsed.to_string(), addresses: vec![] });
            }
            Err(_) => ui::warn("That doesn't look like a device ID, try again."),
        }
    }
}

// ---------------- main menu ----------------

fn draw(cfg: &Config, key: &DeviceId, running: bool, upd: &UpdateInfo) {
    ui::clear();
    let (status_plain, status) = if running {
        ("● Running in the background".to_string(), ui::green("● Running in the background"))
    } else {
        ("● Not running - choose 8 to start".to_string(), ui::red("● Not running - choose 8 to start"))
    };
    let title_plain = format!("Mini Sync  v{}", update::CURRENT_VERSION);
    let title = format!("{}  {}", ui::accent("Mini Sync"), ui::dim(&format!("v{}", update::CURRENT_VERSION)));
    let mut lines = vec![(title_plain, title), (status_plain, status)];
    match upd {
        UpdateInfo::Available(r) => {
            let t = format!("↑ Update v{} available - choose 12", r.version);
            lines.push((t.clone(), ui::yellow(&t)));
        }
        UpdateInfo::Checking => lines.push(("Checking for updates...".into(), ui::dim("Checking for updates..."))),
        _ => {}
    }
    ui::boxed(&lines);

    println!();
    ui::field("This device", &ui::bold(&cfg.device_name));
    ui::field("Device ID", &key.to_string());
    for f in &cfg.folders {
        ui::field("Folder", &format!("{}  {}", f.path.display(), ui::dim(&format!("(ID: {})", f.id))));
    }
    if cfg.peers.is_empty() {
        ui::field("Devices", &ui::yellow("none yet - choose 2 to add one"));
    } else {
        let names: Vec<&str> = cfg.peers.iter().map(|p| p.name.as_str()).collect();
        ui::field("Devices", &names.join(", "));
    }

    let item = |n: &str, text: &str| format!("{}  {:<26}", ui::cyan(&format!("{n:>2}")), text);
    let row = |a: (&str, &str), b: (&str, &str)| println!("   {}  {}", item(a.0, a.1), item(b.0, b.1));

    ui::section("Devices                          Folders");
    row(("1", "Copy this device's ID"), ("4", "Add a folder"));
    row(("2", "Add or update a device"), ("5", "Remove a folder"));
    row(("3", "Remove a device"), ("6", "Open synced folder"));

    ui::section("App                              Security & updates");
    row(("7", "View log"), ("10", "Change this device's ID"));
    row(("8", if running { "Restart" } else { "Start" }), ("11", "Uninstall"));
    row(("9", "Stop until next login"), ("12", "Check for updates"));

    println!("\n   {}  {}", ui::cyan(" 0"), ui::dim("Close this window (syncing keeps running)"));
    println!();
}

fn menu(home: &Home) -> Result<()> {
    // Check for updates in the background so the menu opens instantly.
    let upd = Arc::new(Mutex::new(UpdateInfo::Checking));
    {
        let upd = upd.clone();
        std::thread::spawn(move || {
            let r = match update::check() {
                Ok(Some(rel)) => UpdateInfo::Available(rel),
                Ok(None) => UpdateInfo::Latest,
                Err(_) => UpdateInfo::Unknown,
            };
            *upd.lock().unwrap() = r;
        });
    }

    loop {
        let cfg = home.load_config()?;
        let key = home.load_or_create_key()?;
        let running = service::is_running(cfg.listen_port);
        let upd_now = upd.lock().unwrap().clone();
        draw(&cfg, &key.public, running, &upd_now);

        ui::prompt("Choose a number: ");
        let choice = read_line();
        println!();
        match choice.as_str() {
            "1" => {
                ui::section("This device's ID");
                show_id(&key.public);
                ui::info("Paste it on your other device: menu -> 2.");
                pause();
            }
            "2" => {
                ui::section("Add or update a device");
                if !cfg.peers.is_empty() {
                    ui::info("To update a device that got a new ID, use the same name as before.");
                }
                if let Some(peer) = ask_peer(&key.public) {
                    let mut cfg = cfg.clone();
                    let new_id = DeviceId::parse(&peer.id)?;
                    let replacing = cfg.peers.iter().any(|p| p.name == peer.name);
                    cfg.peers.retain(|p| DeviceId::parse(&p.id).ok() != Some(new_id) && p.name != peer.name);
                    let name = peer.name.clone();
                    cfg.peers.push(peer);
                    home.save_config(&cfg)?;
                    if replacing {
                        ui::ok(&format!("Updated {name}'s ID - the old ID is no longer trusted."));
                    } else {
                        ui::ok(&format!("Added {name}."));
                        ui::info(&format!("Make sure this device's ID is added on {name} too."));
                    }
                    restart(home, &cfg);
                    pause();
                }
            }
            "3" => {
                ui::section("Remove a device");
                let names: Vec<String> = cfg.peers.iter().map(|p| p.name.clone()).collect();
                if let Some(i) = pick("device", &names) {
                    let name = names[i].clone();
                    if confirm(&format!("Stop syncing with {name}?")) {
                        let mut cfg = cfg.clone();
                        home.remove_peer(&mut cfg, &name)?;
                        ui::ok(&format!("Removed {name}. Nothing was deleted on either device."));
                        restart(home, &cfg);
                    }
                    pause();
                } else if names.is_empty() {
                    pause();
                }
            }
            "4" => {
                ui::section("Add a folder");
                let Some(path) = ask_back("Folder path", "") else { continue };
                let path = PathBuf::from(path);
                let suggested = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase().replace(' ', "-"))
                    .unwrap_or_else(|| "folder".into());
                ui::info("The folder ID must be the same on every device that syncs this folder.");
                let Some(id) = ask_back("Folder ID", &suggested) else { continue };
                let mut cfg = cfg.clone();
                if cfg.folders.iter().any(|f| f.id == id) {
                    ui::err(&format!("A folder with ID {id:?} already exists."));
                } else {
                    cfg.folders.push(FolderConfig { id: id.clone(), path, ignore: default_ignores() });
                    home.save_config(&cfg)?;
                    ui::ok(&format!("Added folder {id:?}."));
                    restart(home, &cfg);
                }
                pause();
            }
            "5" => {
                ui::section("Remove a folder");
                let labels: Vec<String> =
                    cfg.folders.iter().map(|f| format!("{} (ID: {})", f.path.display(), f.id)).collect();
                if let Some(i) = pick("folder", &labels) {
                    let folder = cfg.folders[i].clone();
                    ui::info(&format!("This stops syncing {}.", folder.path.display()));
                    ui::info("The files stay on this device, and nothing is deleted on your other devices.");
                    if confirm("Remove it?") {
                        let mut cfg = cfg.clone();
                        home.remove_folder(&mut cfg, &folder.id)?;
                        ui::ok("Removed.");
                        restart(home, &cfg);
                    }
                    pause();
                } else if labels.is_empty() {
                    pause();
                }
            }
            "6" => {
                for f in &cfg.folders {
                    service::open_path(&f.path);
                }
            }
            "7" => service::open_path(&service::log_path(home)),
            "8" => {
                restart(home, &cfg);
                pause();
            }
            "9" => {
                if confirm("Stop syncing until next login?") {
                    service::stop(cfg.listen_port);
                    ui::ok("Stopped. It starts again at next login (or choose 8).");
                    pause();
                }
            }
            "10" => {
                ui::section("Change this device's ID");
                ui::info("This gives this device a new ID. The old ID stops working right away,");
                ui::info("so your other devices stop syncing with it until you give them the new ID");
                ui::info("(on each: menu -> 2, using the same name as before).");
                println!();
                ui::warn("Lost or stolen device? Don't use this - remove it on your OTHER devices (3).");
                println!();
                if confirm("Change this device's ID?") {
                    service::stop(cfg.listen_port);
                    let key = home.rotate_key()?;
                    ui::ok("New ID created:");
                    show_id(&key.public);
                    ui::info(&format!("On each other device: menu -> 2 -> paste this ID -> name it {:?}.", cfg.device_name));
                    restart(home, &cfg);
                    pause();
                }
            }
            "11" => {
                ui::section("Uninstall");
                ui::info("Removes Mini Sync, its settings and this device's ID from this computer.");
                ui::info("Your synced files (and the .minisync-trash backup folder) are NOT deleted.");
                println!();
                if confirm("Uninstall?") {
                    service::uninstall(home, cfg.listen_port, false)?;
                    ui::ok("Uninstalled. You can close this window.");
                    pause();
                    return Ok(());
                }
            }
            "12" => {
                ui::section("Updates");
                ui::info(&format!("This is v{}. Checking GitHub...", update::CURRENT_VERSION));
                match update::check() {
                    Ok(None) => {
                        *upd.lock().unwrap() = UpdateInfo::Latest;
                        ui::ok("You have the latest version.");
                    }
                    Ok(Some(rel)) => {
                        *upd.lock().unwrap() = UpdateInfo::Available(rel.clone());
                        ui::ok(&format!("Version v{} is available.", rel.version));
                        if confirm(&format!("Install v{} now?", rel.version)) {
                            ui::info("Downloading and checking the signature...");
                            match update::install_from_menu(home, cfg.listen_port, &rel) {
                                Ok(()) => {
                                    ui::ok(&format!("Updated to v{} and running.", rel.version));
                                    ui::info("Close this window and reopen Mini Sync to use the new menu.");
                                }
                                Err(e) => ui::err(&format!("Update failed: {e:#}")),
                            }
                        }
                    }
                    Err(e) => ui::err(&format!("Could not check: {e:#}")),
                }
                pause();
            }
            "0" | "q" => return Ok(()),
            "" => {} // Enter: just show the menu again
            other => {
                ui::warn(&format!("{other:?} is not an option - type a number from the list."));
                pause();
            }
        }
    }
}

/// Shows a numbered list and returns the chosen index (None = back).
fn pick(what: &str, items: &[String]) -> Option<usize> {
    if items.is_empty() {
        ui::info(&format!("There is no {what} to remove."));
        return None;
    }
    for (i, item) in items.iter().enumerate() {
        println!("   {}  {item}", ui::cyan(&format!("{:>2}", i + 1)));
    }
    println!();
    loop {
        let answer = ask_back(&format!("Which {what}?"), "")?;
        match answer.parse::<usize>() {
            Ok(n) if n >= 1 && n <= items.len() => return Some(n - 1),
            _ => ui::warn(&format!("Type a number from 1 to {}.", items.len())),
        }
    }
}

fn restart(home: &Home, cfg: &Config) {
    ui::info("Restarting...");
    match service::restart(home, cfg.listen_port) {
        Ok(()) => ui::ok("Running."),
        Err(e) => ui::err(&format!("Restart failed: {e:#}")),
    }
}
