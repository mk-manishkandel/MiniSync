//! What you see when you double-click the app: first-time setup, then a
//! small menu. Syncing itself always happens in the background process.
//!
//! Every question inside a menu option can be left with Enter or 0 ("back").

use crate::config::{default_ignores, Config, DeviceId, FolderConfig, Home, PeerConfig};
use crate::service;
use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::PathBuf;

const DEFAULT_FOLDER_ID: &str = "minisync";

pub fn run(home: &Home) -> Result<()> {
    if !home.config_path().exists() {
        setup(home)?;
    } else if !service::running_from_install() {
        // Opened from a copy somewhere else (e.g. a newer download).
        println!("Mini Sync is already set up on this computer.");
        println!("This copy is not the installed one ({}).", service::installed_exe()?.display());
        if confirm("Install this copy (update) and restart syncing? Your settings are kept.") {
            let cfg = home.load_config()?;
            match service::install(home, cfg.listen_port) {
                Ok(exe) => println!("Updated. Running from {}.", exe.display()),
                Err(e) => println!("Could not update: {e:#}"),
            }
        }
    }
    menu(home)
}

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
        print!("{question}: ");
    } else {
        print!("{question} [{default}]: ");
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
        print!("{question} (Enter or 0 = back): ");
    } else {
        print!("{question} [{default}] (0 = back): ");
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
    print!("{question} (y = yes, Enter = back): ");
    matches!(read_line().to_lowercase().as_str(), "y" | "yes")
}

fn pause() {
    print!("\nPress Enter to continue");
    read_line();
}

fn setup(home: &Home) -> Result<()> {
    println!("==============================");
    println!("  Mini Sync - first-time setup");
    println!("==============================\n");
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

    show_id(&key.public);
    println!("Run Mini Sync on your other device too, and paste each device's ID into the other.\n");
    if let Some(peer) = ask_peer(&key.public) {
        cfg.peers.push(peer);
        home.save_config(&cfg)?;
    }

    println!("\nInstalling and starting in the background...");
    match service::install(home, cfg.listen_port) {
        Ok(exe) => {
            println!("Done! Mini Sync is now running in the background and will start");
            println!("automatically when you log in. You can close this window.");
            println!("(Installed to {})", exe.display());
            println!("To open this menu again: tray icon -> Open Mini Sync.");
        }
        Err(e) => println!("Could not install: {e:#}"),
    }
    pause();
    Ok(())
}

fn show_id(id: &DeviceId) {
    println!("\nThis device's ID:\n\n    {id}\n");
    if service::copy_to_clipboard(&id.to_string()) {
        println!("(copied to the clipboard)\n");
    }
}

fn ask_peer(me: &DeviceId) -> Option<PeerConfig> {
    loop {
        let id = ask_back("Paste the other device's ID", "")?;
        match DeviceId::parse(&id) {
            Ok(parsed) if parsed == *me => println!("That is this device's own ID. Paste the OTHER device's ID."),
            Ok(parsed) => {
                let name = ask_back("Name for that device", "my-other-device")?;
                return Some(PeerConfig { name, id: parsed.to_string(), addresses: vec![] });
            }
            Err(_) => println!("That doesn't look like a device ID, try again."),
        }
    }
}

fn menu(home: &Home) -> Result<()> {
    loop {
        let cfg = home.load_config()?;
        let key = home.load_or_create_key()?;
        let running = service::is_running(cfg.listen_port);

        println!("\n==============================");
        println!("  Mini Sync - {}", if running { "running in the background" } else { "NOT running" });
        println!("==============================");
        println!("This device: {}", cfg.device_name);
        println!("Device ID:   {}", key.public);
        for f in &cfg.folders {
            println!("Folder:      {} (ID: {})", f.path.display(), f.id);
        }
        if cfg.peers.is_empty() {
            println!("Devices:     none yet - choose 2 to add one");
        }
        for p in &cfg.peers {
            println!("Device:      {}", p.name);
        }
        println!();
        println!("  1) Copy this device's ID");
        println!("  2) Add or update a device");
        println!("  3) Remove a device (lost or stolen device? do this on your other devices)");
        println!("  4) Add another folder");
        println!("  5) Remove a folder (stops syncing it; files stay)");
        println!("  6) Open synced folder");
        println!("  7) View log");
        println!("  8) {}", if running { "Restart" } else { "Start" });
        println!("  9) Stop (until next login)");
        println!(" 10) Change this device's ID");
        println!(" 11) Uninstall (removes the app and its settings; your files stay)");
        println!(" 12) Check for updates (this is v{})", crate::update::CURRENT_VERSION);
        println!("  0) Close this window (syncing keeps running)");

        print!("\nChoose a number: ");
        let choice = read_line();
        println!();
        match choice.as_str() {
            "1" => show_id(&key.public),
            "2" => {
                if !cfg.peers.is_empty() {
                    println!("(To update a device that got a new ID, use the same name as before.)");
                }
                if let Some(peer) = ask_peer(&key.public) {
                    let mut cfg = cfg.clone();
                    let new_id = DeviceId::parse(&peer.id)?;
                    let replacing = cfg.peers.iter().any(|p| p.name == peer.name);
                    cfg.peers.retain(|p| DeviceId::parse(&p.id).ok() != Some(new_id) && p.name != peer.name);
                    if replacing {
                        println!("Updated {}'s ID; the old ID is no longer trusted.", peer.name);
                    } else {
                        println!("Added {}. Make sure this device's ID is added on {} too.", peer.name, peer.name);
                    }
                    cfg.peers.push(peer);
                    home.save_config(&cfg)?;
                    restart(home, &cfg);
                }
            }
            "3" => {
                let names: Vec<String> = cfg.peers.iter().map(|p| p.name.clone()).collect();
                if let Some(i) = pick("device", &names) {
                    let name = names[i].clone();
                    if confirm(&format!("Stop syncing with {name}?")) {
                        let mut cfg = cfg.clone();
                        home.remove_peer(&mut cfg, &name)?;
                        println!("Removed {name}. Nothing was deleted on either device.");
                        restart(home, &cfg);
                    }
                }
            }
            "4" => {
                let Some(path) = ask_back("Folder path", "") else { continue };
                let path = PathBuf::from(path);
                let suggested = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase().replace(' ', "-"))
                    .unwrap_or_else(|| "folder".into());
                println!("The folder ID must be the same on every device that syncs this folder.");
                let Some(id) = ask_back("Folder ID", &suggested) else { continue };
                let mut cfg = cfg.clone();
                if cfg.folders.iter().any(|f| f.id == id) {
                    println!("A folder with ID {id:?} already exists.");
                } else {
                    cfg.folders.push(FolderConfig { id, path, ignore: default_ignores() });
                    home.save_config(&cfg)?;
                    restart(home, &cfg);
                }
            }
            "5" => {
                let labels: Vec<String> =
                    cfg.folders.iter().map(|f| format!("{} (ID: {})", f.path.display(), f.id)).collect();
                if let Some(i) = pick("folder", &labels) {
                    let folder = cfg.folders[i].clone();
                    println!("\nThis stops syncing {}.", folder.path.display());
                    println!("The files stay on this device, and nothing is deleted on your other devices.");
                    if confirm("Remove it?") {
                        let mut cfg = cfg.clone();
                        home.remove_folder(&mut cfg, &folder.id)?;
                        println!("Removed.");
                        restart(home, &cfg);
                    }
                }
            }
            "6" => {
                for f in &cfg.folders {
                    service::open_path(&f.path);
                }
            }
            "7" => service::open_path(&service::log_path(home)),
            "8" => restart(home, &cfg),
            "9" => {
                if confirm("Stop syncing until next login?") {
                    service::stop(cfg.listen_port);
                    println!("Stopped. It will start again at next login (or choose 8).");
                }
            }
            "10" => {
                println!("This gives this device a new ID. The old ID stops working right away,");
                println!("so your other devices will stop syncing with this one until you give");
                println!("them the new ID (menu -> 2, using the same name as before).");
                println!("Use it if you think this device's key was copied.");
                println!("If a device was LOST or STOLEN, instead remove it on your other devices (3).");
                if confirm("Change this device's ID?") {
                    service::stop(cfg.listen_port);
                    let key = home.rotate_key()?;
                    println!("\nNew ID created.");
                    show_id(&key.public);
                    println!("On each other device: menu -> 2 -> paste this ID -> name it {:?}.", cfg.device_name);
                    restart(home, &cfg);
                }
            }
            "11" => {
                println!("This removes Mini Sync, its settings and this device's ID from this computer.");
                println!("Your synced files (and the .minisync-trash backup folder) are NOT deleted.");
                if confirm("Uninstall?") {
                    service::uninstall(home, cfg.listen_port, false)?;
                    println!("Uninstalled. You can close this window.");
                    pause();
                    return Ok(());
                }
            }
            "12" => {
                println!("Checking GitHub for a newer version...");
                match crate::update::check() {
                    Ok(None) => println!("You have the latest version (v{}).", crate::update::CURRENT_VERSION),
                    Ok(Some(rel)) => {
                        println!("Version v{} is available.", rel.version);
                        if confirm(&format!("Install v{} now?", rel.version)) {
                            println!("Downloading and checking the signature...");
                            match crate::update::install_from_menu(home, cfg.listen_port, &rel) {
                                Ok(()) => {
                                    println!("Updated to v{} and running.", rel.version);
                                    println!("Close this window and reopen Mini Sync to use the new menu.");
                                }
                                Err(e) => println!("Update failed: {e:#}"),
                            }
                        }
                    }
                    Err(e) => println!("Could not check: {e:#}"),
                }
            }
            "0" | "q" => return Ok(()),
            "" => {} // Enter: just show the menu again
            other => println!("{other:?} is not an option - type a number from the list."),
        }
    }
}

/// Shows a numbered list and returns the chosen index (None = back).
fn pick(what: &str, items: &[String]) -> Option<usize> {
    if items.is_empty() {
        println!("There is no {what} to remove.");
        return None;
    }
    for (i, item) in items.iter().enumerate() {
        println!("  {}) {item}", i + 1);
    }
    loop {
        let answer = ask_back(&format!("Which {what}?"), "")?;
        match answer.parse::<usize>() {
            Ok(n) if n >= 1 && n <= items.len() => return Some(n - 1),
            _ => println!("Type a number from 1 to {}.", items.len()),
        }
    }
}

fn restart(home: &Home, cfg: &Config) {
    print!("Restarting... ");
    let _ = std::io::stdout().flush();
    match service::restart(home, cfg.listen_port) {
        Ok(()) => println!("running."),
        Err(e) => println!("failed: {e:#}"),
    }
}
